//! Backup, verification and restore.
//!
//! * Backups use SQLite's online backup API from a consistent read snapshot,
//!   so they can run while the POS is selling.
//! * Every backup is verified after creation: integrity check, schema version,
//!   record counts and SHA-256 are stored in a manifest next to the file.
//! * Restore never starts without a passing inspection and a safety backup of
//!   the current database. The restore itself is one backup-API copy into the
//!   live database, followed by migrations if the backup is older.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::audit;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ids::new_id;
use crate::service::AppCore;
use crate::settings::{self, BackupSettings};
use crate::system::DiagnosticItem;
use crate::time;

pub const EXT: &str = "amwbak";

pub fn free_space(p: &Path) -> Option<u64> {
    let mut probe = p.to_path_buf();
    while !probe.exists() {
        probe = probe.parent()?.to_path_buf();
    }
    fs4::available_space(&probe).ok()
}

pub fn file_sha256(p: &Path) -> AppResult<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(hex::encode(h.finalize()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    pub format: String,
    pub app_version: String,
    pub schema_version: i64,
    pub business_name: Option<String>,
    pub business_id: Option<String>,
    pub device_code: Option<String>,
    pub created_at: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub record_counts: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupRow {
    pub backup_id: Option<String>,
    pub path: String,
    pub file_name: String,
    pub kind: String,
    pub size_bytes: Option<i64>,
    pub status: String,
    pub error: Option<String>,
    pub created_at: String,
    pub duration_ms: Option<i64>,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Inspection {
    pub path: String,
    pub ok: bool,
    pub integrity: String,
    pub schema_version: i64,
    pub current_schema_version: i64,
    pub compatible: bool,
    pub business_name: Option<String>,
    pub same_business: bool,
    pub record_counts: serde_json::Map<String, serde_json::Value>,
    pub sha256: String,
    pub manifest_sha256: Option<String>,
    pub checksum_matches: Option<bool>,
    pub size_bytes: u64,
    pub created_at: Option<String>,
    pub problems: Vec<String>,
}

const COUNTED: &[&str] = &[
    "products", "product_barcodes", "sales", "sale_items", "payments", "refunds", "stock_movements", "cash_events", "shifts", "customers",
    "suppliers", "purchase_orders", "users", "audit_logs",
];

fn record_counts(c: &Connection) -> AppResult<serde_json::Map<String, serde_json::Value>> {
    let mut m = serde_json::Map::new();
    for t in COUNTED {
        let n: i64 = c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap_or(-1);
        m.insert((*t).into(), n.into());
    }
    Ok(m)
}

fn manifest_path(p: &Path) -> PathBuf {
    p.with_extension(format!("{EXT}.json"))
}

/// Inspect a backup file without modifying it.
pub fn inspect_file(path: &Path, current_business: Option<&str>) -> AppResult<Inspection> {
    if !path.is_file() {
        return Err(AppError::not_found(format!("Backup file {}", path.display())));
    }
    let size = std::fs::metadata(path)?.len();
    let sha = file_sha256(path)?;
    let manifest: Option<BackupManifest> = std::fs::read_to_string(manifest_path(path)).ok().and_then(|s| serde_json::from_str(&s).ok());
    let mut problems = vec![];
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| AppError::validation(format!("This file is not a readable AMWAPOS backup: {e}")))?;
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap_or_else(|e| format!("unreadable: {e}"));
    if integrity != "ok" {
        problems.push(format!("Integrity check failed: {integrity}"));
    }
    let schema: i64 = conn
        .query_row("SELECT COALESCE(MAX(version),0) FROM schema_migrations", [], |r| r.get(0))
        .unwrap_or(0);
    if schema == 0 {
        problems.push("This file does not contain an AMWAPOS database.".into());
    }
    let latest = crate::db::latest_schema_version();
    if schema > latest {
        problems.push(format!("The backup was made by a newer AMWAPOS (schema {schema}); this version supports up to {latest}."));
    }
    let (bname, bid): (Option<String>, Option<String>) = conn
        .query_row("SELECT name, business_id FROM business LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .unwrap_or(None)
        .map(|(a, b)| (Some(a), Some(b)))
        .unwrap_or((None, None));
    let counts = if schema > 0 { record_counts(&conn)? } else { Default::default() };
    let checksum_matches = manifest.as_ref().map(|m| m.sha256 == sha);
    if checksum_matches == Some(false) {
        problems.push("The file does not match the checksum recorded when the backup was made.".into());
    }
    Ok(Inspection {
        path: path.to_string_lossy().to_string(),
        ok: problems.is_empty(),
        integrity,
        schema_version: schema,
        current_schema_version: latest,
        compatible: schema > 0 && schema <= latest,
        same_business: match (current_business, &bid) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        },
        business_name: bname,
        record_counts: counts,
        sha256: sha,
        manifest_sha256: manifest.as_ref().map(|m| m.sha256.clone()),
        checksum_matches,
        size_bytes: size,
        created_at: manifest.map(|m| m.created_at),
        problems,
    })
}

impl AppCore {
    fn backup_dir(&self, c: &Connection) -> AppResult<PathBuf> {
        let cfg: BackupSettings = settings::get(c, settings::KEY_BACKUP)?;
        Ok(if cfg.directory.trim().is_empty() { self.data_dir.join("backups") } else { PathBuf::from(cfg.directory) })
    }

    /// Create and verify a backup. `kind` is manual | automatic | safety.
    pub(crate) fn backup_create_internal(&self, dir: Option<PathBuf>, kind: &str, user_id: Option<&str>) -> AppResult<BackupRow> {
        let started = Instant::now();
        let dir = match dir {
            Some(d) => d,
            None => self.db.read(|c| self.backup_dir(c))?,
        };
        std::fs::create_dir_all(&dir).map_err(|e| AppError::new(ErrorCode::Io, format!("Cannot create backup folder {}: {e}", dir.display())))?;
        let db_size = std::fs::metadata(self.db.path()).map(|m| m.len()).unwrap_or(0)
            + std::fs::metadata(self.db.path().with_extension("db-wal")).map(|m| m.len()).unwrap_or(0);
        if let Some(free) = free_space(&dir) {
            let need = db_size + db_size / 5 + 10 * 1024 * 1024;
            if free < need {
                return Err(AppError::new(
                    ErrorCode::InsufficientDisk,
                    format!(
                        "Not enough free space for a backup: {:.1} MB needed, {:.1} MB free in {}.",
                        need as f64 / 1_048_576.0,
                        free as f64 / 1_048_576.0,
                        dir.display()
                    ),
                ));
            }
        }
        let (code, bname, bid) = self.db.read(|c| {
            let code: Option<String> = c.query_row("SELECT code FROM branches LIMIT 1", [], |r| r.get(0)).optional()?;
            let b: Option<(String, String)> = c.query_row("SELECT name, business_id FROM business LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            Ok((code, b.as_ref().map(|x| x.0.clone()), b.map(|x| x.1)))
        })?;
        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let file = dir.join(format!("AMWAPOS-{}-{}-{}.{EXT}", code.unwrap_or_else(|| "STORE".into()), kind, ts));
        let tmp = file.with_extension("partial");
        let res: AppResult<BackupManifest> = (|| {
            {
                let mut dst = Connection::open(&tmp)?;
                self.db.read(|src| {
                    let b = rusqlite::backup::Backup::new(src, &mut dst)?;
                    // One step copies every page from a single consistent read snapshot.
                    match b.step(-1)? {
                        rusqlite::backup::StepResult::Done => Ok(()),
                        other => Err(AppError::new(ErrorCode::DatabaseBusy, format!("Backup did not complete ({other:?}). Try again."))),
                    }
                })?;
                dst.execute_batch("PRAGMA journal_mode=DELETE;")?;
            }
            std::fs::rename(&tmp, &file)?;
            let insp = inspect_file(&file, None)?;
            if !insp.ok {
                return Err(AppError::new(ErrorCode::Database, format!("The new backup failed verification: {}", insp.problems.join("; "))));
            }
            let m = BackupManifest {
                format: "amwapos-sqlite-v1".into(),
                app_version: audit::APP_VERSION.into(),
                schema_version: insp.schema_version,
                business_name: bname.clone(),
                business_id: bid.clone(),
                device_code: self.device().map(|d| d.device_code),
                created_at: time::now_str(),
                sha256: insp.sha256.clone(),
                size_bytes: insp.size_bytes,
                record_counts: insp.record_counts.clone(),
            };
            std::fs::write(manifest_path(&file), serde_json::to_vec_pretty(&m)?)?;
            Ok(m)
        })();
        let _ = std::fs::remove_file(&tmp);
        let dur = started.elapsed().as_millis() as i64;
        let id = new_id();
        let now = time::now_str();
        match res {
            Ok(m) => {
                self.db.write(|tx| {
                    tx.execute(
                        "INSERT INTO backups(backup_id, path, kind, size_bytes, sha256, status, record_counts_json, duration_ms, created_by, created_at)
                         VALUES (?1,?2,?3,?4,?5,'completed',?6,?7,?8,?9)",
                        params![id, file.to_string_lossy(), kind, m.size_bytes as i64, m.sha256, serde_json::to_string(&m.record_counts)?, dur, user_id, now],
                    )?;
                    Ok(())
                })?;
                Ok(BackupRow {
                    backup_id: Some(id),
                    file_name: file.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                    path: file.to_string_lossy().to_string(),
                    kind: kind.into(),
                    size_bytes: Some(m.size_bytes as i64),
                    status: "completed".into(),
                    error: None,
                    created_at: now,
                    duration_ms: Some(dur),
                    exists: true,
                })
            }
            Err(e) => {
                let _ = std::fs::remove_file(&file);
                let _ = self.db.write(|tx| {
                    tx.execute(
                        "INSERT INTO backups(backup_id, path, kind, status, error, duration_ms, created_by, created_at) VALUES (?1,?2,?3,'failed',?4,?5,?6,?7)",
                        params![id, file.to_string_lossy(), kind, e.message, dur, user_id, now],
                    )?;
                    Ok(())
                });
                Err(e)
            }
        }
    }

    pub fn backup_create(&self, token: &str, directory: Option<String>) -> AppResult<BackupRow> {
        let s = self.session(token)?;
        s.require("backup.manage")?;
        let dir = directory.filter(|d| !d.trim().is_empty()).map(PathBuf::from);
        let row = self.backup_create_internal(dir, "manual", Some(&s.user_id))?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            audit::record(tx, &actor, "backup.created", "backup", row.backup_id.as_deref(), None, Some(&json!({ "path": row.path, "size_bytes": row.size_bytes })))?;
            Ok(())
        })?;
        Ok(row)
    }

    pub fn backups_list(&self, token: &str) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        s.require("backup.manage")?;
        let (dir, cfg) = self.db.read(|c| Ok((self.backup_dir(c)?, settings::get::<BackupSettings>(c, settings::KEY_BACKUP)?)))?;
        let mut rows: Vec<BackupRow> = self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT backup_id, path, kind, size_bytes, status, error, created_at, duration_ms FROM backups ORDER BY created_at DESC LIMIT 200",
            )?;
            let rows = st
                .query_map([], |r| {
                    let path: String = r.get(1)?;
                    Ok(BackupRow {
                        backup_id: r.get(0)?,
                        exists: Path::new(&path).exists(),
                        file_name: Path::new(&path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                        path,
                        kind: r.get(2)?,
                        size_bytes: r.get(3)?,
                        status: r.get(4)?,
                        error: r.get(5)?,
                        created_at: r.get(6)?,
                        duration_ms: r.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        // Files present in the folder but not recorded here (e.g. copied from another PC).
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == EXT).unwrap_or(false) && !rows.iter().any(|r| Path::new(&r.path) == p) {
                    let meta = e.metadata().ok();
                    rows.push(BackupRow {
                        backup_id: None,
                        path: p.to_string_lossy().to_string(),
                        file_name: p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                        kind: "external".into(),
                        size_bytes: meta.as_ref().map(|m| m.len() as i64),
                        status: "completed".into(),
                        error: None,
                        created_at: meta
                            .and_then(|m| m.modified().ok())
                            .map(|t| time::fmt(chrono::DateTime::<chrono::Utc>::from(t)))
                            .unwrap_or_default(),
                        duration_ms: None,
                        exists: true,
                    });
                }
            }
        }
        let last_ok = rows.iter().filter(|r| r.status == "completed" && r.kind != "external").map(|r| r.created_at.clone()).max();
        let next = if cfg.automatic {
            last_ok
                .as_ref()
                .and_then(|l| time::parse(l).ok())
                .map(|t| time::fmt(t + chrono::Duration::hours(cfg.interval_hours)))
        } else {
            None
        };
        Ok(json!({
            "directory": dir.to_string_lossy(), "settings": cfg, "last_success_at": last_ok, "next_due_at": next,
            "free_bytes": free_space(&dir), "backups": rows,
        }))
    }

    pub fn backup_inspect(&self, token: &str, path: &str) -> AppResult<Inspection> {
        let s = self.session(token)?;
        s.require("backup.manage")?;
        let bid: Option<String> = self.db.read(|c| Ok(c.query_row("SELECT business_id FROM business LIMIT 1", [], |r| r.get(0)).optional()?))?;
        inspect_file(Path::new(path), bid.as_deref())
    }

    /// Restore a verified backup over the live database. Requires the owner-level
    /// `backup.restore` permission. A safety backup is taken first.
    pub fn backup_restore(&self, token: &str, path: &str, acknowledge_different_business: bool) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        s.require("backup.restore")?;
        let started = Instant::now();
        let bid: Option<String> = self.db.read(|c| Ok(c.query_row("SELECT business_id FROM business LIMIT 1", [], |r| r.get(0)).optional()?))?;
        let src_path = PathBuf::from(path);
        let insp = inspect_file(&src_path, bid.as_deref())?;
        if !insp.ok || !insp.compatible {
            return Err(AppError::validation(format!("This backup cannot be restored: {}", insp.problems.join("; "))).with_details(serde_json::to_value(&insp)?));
        }
        if !insp.same_business && !acknowledge_different_business {
            return Err(AppError::conflict(format!(
                "This backup belongs to a different business ({}). Confirm explicitly to replace this store's data.",
                insp.business_name.clone().unwrap_or_default()
            )));
        }
        if let Some(free) = free_space(&self.data_dir) {
            let need = insp.size_bytes * 2 + 10 * 1024 * 1024;
            if free < need {
                return Err(AppError::new(ErrorCode::InsufficientDisk, format!("Not enough free space to restore safely: {:.1} MB needed.", need as f64 / 1_048_576.0)));
            }
        }
        let safety = self.backup_create_internal(Some(self.data_dir.join("backups").join("safety")), "safety", Some(&s.user_id))?;
        // Copy the backup into the live database under the writer lock.
        self.db.close_readers();
        let report = self.db.with_writer(|w| {
            let src = Connection::open_with_flags(&src_path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
            {
                let b = rusqlite::backup::Backup::new(&src, w)?;
                match b.step(-1)? {
                    rusqlite::backup::StepResult::Done => {}
                    other => return Err(AppError::new(ErrorCode::DatabaseBusy, format!("Restore did not complete ({other:?}). No changes were made."))),
                }
            }
            w.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
            let rep = crate::db::migrate(w, self.db.path())?;
            crate::auth::seed_roles(w)?;
            Ok(rep)
        })?;
        self.db.close_readers();
        let device = self.db.read(crate::service::load_device)?;
        self.set_device(device);
        let counts_after = self.db.read(record_counts)?;
        let dur = started.elapsed().as_millis() as i64;
        let ok = counts_after == insp.record_counts || report.from_version != report.to_version;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            tx.execute(
                "INSERT INTO backups(backup_id, path, kind, size_bytes, sha256, status, record_counts_json, duration_ms, created_by, created_at)
                 VALUES (?1,?2,'safety',?3,NULL,'completed',NULL,?4,?5,?6)",
                params![new_id(), safety.path, safety.size_bytes, safety.duration_ms, s.user_id, safety.created_at],
            )?;
            audit::record(tx, &actor, "backup.restored", "backup", None, None, Some(&json!({
                "path": path, "sha256": insp.sha256, "safety_backup": safety.path, "duration_ms": dur, "counts_verified": ok,
            })))?;
            Ok(())
        })?;
        // Everyone signs in again against the restored staff list.
        self.sessions.remove_user(&s.user_id);
        for (u, _) in self.sessions.active_users() {
            self.sessions.remove_user(&u);
        }
        Ok(json!({
            "restored_from": path, "safety_backup": safety.path, "duration_ms": dur, "record_counts": counts_after,
            "counts_verified": ok, "migrated_from_schema": report.from_version, "schema_version": report.to_version,
        }))
    }

    /// Run the automatic backup if due and prune old automatic backups.
    /// Returns Some(row) when a backup was made.
    pub fn backup_run_scheduled(&self) -> AppResult<Option<BackupRow>> {
        if self.device().is_none() {
            return Ok(None);
        }
        let (cfg, last): (BackupSettings, Option<String>) = self.db.read(|c| {
            Ok((
                settings::get(c, settings::KEY_BACKUP)?,
                c.query_row("SELECT MAX(created_at) FROM backups WHERE kind='automatic' AND status='completed'", [], |r| r.get(0))?,
            ))
        })?;
        if !cfg.automatic {
            return Ok(None);
        }
        let due = match last.and_then(|l| time::parse(&l).ok()) {
            Some(t) => time::now() - t >= chrono::Duration::hours(cfg.interval_hours),
            None => true,
        };
        if !due {
            return Ok(None);
        }
        let row = self.backup_create_internal(None, "automatic", None)?;
        // Prune older automatic backups beyond `keep`.
        let old: Vec<(String, String)> = self.db.read(|c| {
            let mut st = c.prepare("SELECT backup_id, path FROM backups WHERE kind='automatic' AND status='completed' ORDER BY created_at DESC LIMIT -1 OFFSET ?1")?;
            let rows = st.query_map([cfg.keep], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        for (_id, p) in old {
            let _ = std::fs::remove_file(&p);
            let _ = std::fs::remove_file(manifest_path(Path::new(&p)));
        }
        Ok(Some(row))
    }

    pub(crate) fn backup_diagnostic(&self) -> AppResult<DiagnosticItem> {
        let (last_ok, last_fail, cfg): (Option<String>, Option<(String, String)>, BackupSettings) = self.db.read(|c| {
            Ok((
                c.query_row("SELECT MAX(created_at) FROM backups WHERE status='completed' AND kind<>'safety'", [], |r| r.get(0))?,
                c.query_row("SELECT created_at, error FROM backups WHERE status='failed' ORDER BY created_at DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())))
                    .optional()?,
                settings::get(c, settings::KEY_BACKUP)?,
            ))
        })?;
        let stale = match &last_ok {
            Some(t) => time::parse(t).map(|t| time::now() - t > chrono::Duration::hours(cfg.interval_hours * 2)).unwrap_or(true),
            None => true,
        };
        Ok(DiagnosticItem {
            component: "Backup".into(),
            state: if last_ok.is_none() || stale { "warning".into() } else { "ok".into() },
            summary: match &last_ok {
                Some(t) => format!("Last successful backup {t}"),
                None => "No successful backup yet".into(),
            },
            details: json!({ "last_success_at": last_ok, "last_failure": last_fail, "automatic": cfg.automatic, "interval_hours": cfg.interval_hours, "directory": cfg.directory }),
        })
    }
}
