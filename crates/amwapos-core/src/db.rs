//! SQLite connection management, migrations and integrity checks.
//!
//! * WAL journal, `synchronous=FULL` for financial durability, foreign keys on.
//! * One writer connection guarded by a mutex; every mutation runs inside a
//!   `BEGIN IMMEDIATE` transaction so write locks are taken up front and
//!   busy conditions surface before any work is done.
//! * A small pool of reader connections for queries (WAL allows concurrent
//!   readers while a write is in progress).
//! * Migrations are embedded, checksummed and immutable: an applied
//!   migration whose checksum changed aborts startup.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult, ErrorCode};

pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration { version: 1, name: "init", sql: include_str!("migrations/0001_init.sql") },
    Migration { version: 2, name: "sync", sql: include_str!("migrations/0002_sync.sql") },
];

pub fn latest_schema_version() -> i64 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

fn checksum(sql: &str) -> String {
    hex::encode(Sha256::digest(sql.as_bytes()))
}

const READ_POOL: usize = 4;
const BUSY_TIMEOUT_MS: u64 = 5_000;

pub struct Db {
    path: PathBuf,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MigrationReport {
    pub from_version: i64,
    pub to_version: i64,
    pub applied: Vec<i64>,
    pub safety_backup: Option<String>,
}

fn configure(conn: &Connection) -> AppResult<()> {
    conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = FULL;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -32000;",
    )?;
    Ok(())
}

impl Db {
    /// Open (creating if `create` is true) the database at `path`, apply
    /// pending migrations, and return the handle plus a migration report.
    ///
    /// When `create` is false and the file does not exist this fails with
    /// `NotFound` rather than silently creating an empty store.
    pub fn open(path: &Path, create: bool) -> AppResult<(Db, MigrationReport)> {
        let exists = path.exists();
        if !exists && !create {
            return Err(AppError::new(
                ErrorCode::NotFound,
                format!(
                    "The store database was not found at {}. It has not been recreated. Restore a backup or check the data folder.",
                    path.display()
                ),
            ));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        if create {
            flags |= OpenFlags::SQLITE_OPEN_CREATE;
        }
        let writer = Connection::open_with_flags(path, flags)?;
        configure(&writer)?;
        let mode: String = writer.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(AppError::new(ErrorCode::Database, format!("Could not enable WAL journal mode (got {mode}).")));
        }
        if exists {
            let qc: String = writer.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
            if qc != "ok" {
                return Err(AppError::new(
                    ErrorCode::DatabaseCorrupt,
                    "The database failed its integrity check. No data has been modified. Restore a backup or export diagnostics.",
                )
                .with_details(serde_json::json!({ "quick_check": qc })));
            }
        }
        let report = migrate(&writer, path)?;
        let db = Db { path: path.to_path_buf(), writer: Mutex::new(writer), readers: Mutex::new(Vec::new()) };
        Ok((db, report))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn new_reader(&self) -> AppResult<Connection> {
        let c = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        c.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
        c.execute_batch("PRAGMA foreign_keys = ON; PRAGMA temp_store = MEMORY; PRAGMA cache_size = -16000;")?;
        Ok(c)
    }

    /// Run a read-only closure on a pooled reader connection.
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> AppResult<T>) -> AppResult<T> {
        let conn = {
            let mut pool = self.readers.lock().map_err(|_| AppError::internal("reader pool poisoned"))?;
            pool.pop()
        };
        let conn = match conn {
            Some(c) => c,
            None => self.new_reader()?,
        };
        let out = f(&conn);
        if let Ok(mut pool) = self.readers.lock() {
            if pool.len() < READ_POOL {
                pool.push(conn);
            }
        }
        out
    }

    /// Run `f` inside a `BEGIN IMMEDIATE` transaction on the writer. Commits
    /// on `Ok`, rolls back on `Err`. Busy failures at BEGIN are retried with
    /// bounded backoff; failures after work has started are not retried here
    /// (callers use idempotency keys for safe retries).
    pub fn write<T>(&self, f: impl FnOnce(&Transaction) -> AppResult<T>) -> AppResult<T> {
        let mut guard = self.writer.lock().map_err(|_| AppError::internal("writer connection poisoned"))?;
        let mut attempt = 0u32;
        let mut f = Some(f);
        loop {
            match guard.transaction_with_behavior(TransactionBehavior::Immediate) {
                Ok(tx) => {
                    let f = f.take().expect("closure used once");
                    return match f(&tx) {
                        Ok(v) => {
                            tx.commit().map_err(AppError::from)?;
                            Ok(v)
                        }
                        Err(e) => {
                            // Dropping the transaction rolls back.
                            drop(tx);
                            Err(e)
                        }
                    };
                }
                Err(e) => {
                    let err: AppError = e.into();
                    if err.code == ErrorCode::DatabaseBusy && attempt < 5 {
                        attempt += 1;
                        std::thread::sleep(Duration::from_millis(50 * (1 << attempt)));
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    /// Direct access to the writer for maintenance tasks (backup, checkpoint).
    pub fn with_writer<T>(&self, f: impl FnOnce(&mut Connection) -> AppResult<T>) -> AppResult<T> {
        let mut guard = self.writer.lock().map_err(|_| AppError::internal("writer connection poisoned"))?;
        f(&mut guard)
    }

    pub fn integrity_check(&self) -> AppResult<Vec<String>> {
        self.read(|c| {
            let mut stmt = c.prepare("PRAGMA integrity_check")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn schema_version(&self) -> AppResult<i64> {
        self.read(current_version)
    }

    /// Drop pooled readers (needed before restoring over the file).
    pub fn close_readers(&self) {
        if let Ok(mut pool) = self.readers.lock() {
            pool.clear();
        }
    }
}

fn current_version(c: &Connection) -> AppResult<i64> {
    let has: bool = c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_migrations'", [], |r| {
        r.get::<_, i64>(0).map(|n| n > 0)
    })?;
    if !has {
        return Ok(0);
    }
    Ok(c.query_row("SELECT COALESCE(MAX(version),0) FROM schema_migrations", [], |r| r.get(0))?)
}

pub(crate) fn migrate(conn: &Connection, path: &Path) -> AppResult<MigrationReport> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            checksum TEXT NOT NULL,
            applied_at TEXT NOT NULL
         );",
    )?;
    // Verify applied migrations are unchanged.
    let applied: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT version, checksum FROM schema_migrations ORDER BY version")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (v, sum) in &applied {
        match MIGRATIONS.iter().find(|m| m.version == *v) {
            Some(m) if checksum(m.sql) == *sum => {}
            Some(_) => {
                return Err(AppError::new(
                    ErrorCode::Database,
                    format!("Migration {v} has changed since it was applied. Startup stopped to protect data."),
                ))
            }
            None => {
                return Err(AppError::new(
                    ErrorCode::Database,
                    format!(
                    "This database was created by a newer AMWAPOS (schema {v}). Install the newer version or restore a compatible backup."
                ),
                ))
            }
        }
    }
    let from = applied.last().map(|a| a.0).unwrap_or(0);
    let pending: Vec<&Migration> = MIGRATIONS.iter().filter(|m| m.version > from).collect();
    let mut report = MigrationReport { from_version: from, to_version: from, applied: vec![], safety_backup: None };
    if pending.is_empty() {
        return Ok(report);
    }
    // Safety backup before upgrading a database that already has data.
    if from > 0 {
        let dir = path.parent().unwrap_or(Path::new(".")).join("backups");
        std::fs::create_dir_all(&dir)?;
        let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let dest = dir.join(format!("pre-migration-v{from}-{ts}.db"));
        conn.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])?;
        report.safety_backup = Some(dest.to_string_lossy().to_string());
    }
    for m in pending {
        let res = (|| -> AppResult<()> {
            conn.execute_batch("BEGIN IMMEDIATE")?;
            conn.execute_batch(m.sql)?;
            conn.execute(
                "INSERT INTO schema_migrations(version,name,checksum,applied_at) VALUES (?1,?2,?3,?4)",
                rusqlite::params![m.version, m.name, checksum(m.sql), crate::time::now_str()],
            )?;
            conn.execute_batch("COMMIT")?;
            Ok(())
        })();
        if let Err(e) = res {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(AppError::new(
                ErrorCode::Database,
                format!(
                    "Database upgrade to schema {} ({}) failed: {}. The database was left at schema {}.",
                    m.version, m.name, e.message, report.to_version
                ),
            ));
        }
        report.applied.push(m.version);
        report.to_version = m.version;
    }
    Ok(report)
}

/// Run a closure; if SQLite reports BUSY, retry a bounded number of times.
/// Only use for operations that are safe to repeat (reads or idempotent writes).
pub fn with_busy_retry<T>(mut f: impl FnMut() -> AppResult<T>) -> AppResult<T> {
    let mut attempt = 0;
    loop {
        match f() {
            Err(e) if e.code == ErrorCode::DatabaseBusy && attempt < 4 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(100 * attempt));
            }
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_migrate_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.db");
        assert!(Db::open(&p, false).is_err(), "must not create without permission");
        let (db, rep) = Db::open(&p, true).unwrap();
        assert_eq!(rep.from_version, 0);
        assert_eq!(rep.to_version, latest_schema_version());
        assert_eq!(db.schema_version().unwrap(), latest_schema_version());
        drop(db);
        let (db, rep) = Db::open(&p, false).unwrap();
        assert!(rep.applied.is_empty());
        assert_eq!(db.integrity_check().unwrap(), vec!["ok".to_string()]);
    }

    #[test]
    fn tampered_migration_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.db");
        let (db, _) = Db::open(&p, true).unwrap();
        db.with_writer(|c| {
            c.execute("UPDATE schema_migrations SET checksum='x' WHERE version=1", [])?;
            Ok(())
        })
        .unwrap();
        drop(db);
        let err = Db::open(&p, false).err().unwrap();
        assert!(err.message.contains("changed"));
    }

    #[test]
    fn newer_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.db");
        let (db, _) = Db::open(&p, true).unwrap();
        db.with_writer(|c| {
            c.execute("INSERT INTO schema_migrations VALUES (9999,'future','x','2026-01-01T00:00:00.000Z')", [])?;
            Ok(())
        })
        .unwrap();
        drop(db);
        let err = Db::open(&p, false).err().unwrap();
        assert!(err.message.contains("newer"));
    }

    #[test]
    fn write_rolls_back_on_error() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _) = Db::open(&dir.path().join("t.db"), true).unwrap();
        let r: AppResult<()> = db.write(|tx| {
            tx.execute("INSERT INTO sequences(name,value) VALUES ('x',1)", [])?;
            Err(AppError::validation("boom"))
        });
        assert!(r.is_err());
        let n: i64 = db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM sequences", [], |r| r.get(0))?)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn append_only_tables_reject_update() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _) = Db::open(&dir.path().join("t.db"), true).unwrap();
        let r = db.write(|tx| {
            tx.execute_batch(
                "INSERT INTO audit_logs(audit_id,event_type,entity_type,previous_hash,audit_hash,app_version,schema_version,created_at)
                 VALUES ('a','x','y','0','1','v',1,'t');",
            )?;
            tx.execute("UPDATE audit_logs SET event_type='z'", [])?;
            Ok(())
        });
        assert!(r.is_err());
    }
}
