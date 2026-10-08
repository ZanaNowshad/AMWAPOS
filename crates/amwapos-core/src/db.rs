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
    Migration { version: 3, name: "receipt_arabic", sql: include_str!("migrations/0003_receipt_arabic.sql") },
    Migration { version: 4, name: "messaging_ocr", sql: include_str!("migrations/0004_messaging_ocr.sql") },
    Migration { version: 5, name: "ai", sql: include_str!("migrations/0005_ai.sql") },
    Migration { version: 6, name: "customer_accounts", sql: include_str!("migrations/0006_customer_accounts.sql") },
    Migration { version: 7, name: "whatsapp_inprocess", sql: include_str!("migrations/0007_whatsapp_inprocess.sql") },
    Migration { version: 8, name: "parse_and_pdf_retry", sql: include_str!("migrations/0008_parse_and_pdf_retry.sql") },
    Migration { version: 9, name: "org_inventory_loyalty_orders", sql: include_str!("migrations/0009_org_inventory_loyalty_orders.sql") },
    Migration { version: 10, name: "digital_order_cart", sql: include_str!("migrations/0010_digital_order_cart.sql") },
    Migration { version: 11, name: "ai_admin_proposals", sql: include_str!("migrations/0011_ai_admin_proposals.sql") },
    Migration { version: 12, name: "ai_workspace", sql: include_str!("migrations/0012_ai_workspace.sql") },
    Migration { version: 13, name: "perm_seeds_dual_alerts", sql: include_str!("migrations/0013_perm_seeds_dual_alerts.sql") },
    Migration { version: 14, name: "wa_phone_contacts", sql: include_str!("migrations/0014_wa_phone_contacts.sql") },
    Migration { version: 15, name: "send_loop", sql: include_str!("migrations/0015_send_loop.sql") },
    Migration { version: 16, name: "rider_cash", sql: include_str!("migrations/0016_rider_cash.sql") },
    Migration { version: 17, name: "not_delivered", sql: include_str!("migrations/0017_not_delivered.sql") },
    Migration { version: 18, name: "bahrain_address", sql: include_str!("migrations/0018_bahrain_address.sql") },
    Migration { version: 19, name: "wa_received", sql: include_str!("migrations/0019_wa_received.sql") },
    Migration { version: 20, name: "product_images", sql: include_str!("migrations/0020_product_images.sql") },
    Migration { version: 21, name: "wa_catalog", sql: include_str!("migrations/0021_wa_catalog.sql") },
    Migration { version: 22, name: "document_intelligence", sql: include_str!("migrations/0022_document_intelligence.sql") },
    Migration { version: 23, name: "whatsapp_ai_orders", sql: include_str!("migrations/0023_whatsapp_ai_orders.sql") },
    Migration { version: 24, name: "wa_catalog_hardening", sql: include_str!("migrations/0024_wa_catalog_hardening.sql") },
    Migration { version: 25, name: "order_reservations", sql: include_str!("migrations/0025_order_reservations.sql") },
    Migration { version: 26, name: "accounts_payable", sql: include_str!("migrations/0026_accounts_payable.sql") },
    Migration { version: 27, name: "wave1_finance", sql: include_str!("migrations/0027_wave1_finance.sql") },
    Migration { version: 28, name: "wave2_trading_day", sql: include_str!("migrations/0028_wave2_trading_day.sql") },
    Migration { version: 29, name: "wave3_inventory_truth", sql: include_str!("migrations/0029_wave3_inventory_truth.sql") },
    Migration { version: 30, name: "wave4_procurement", sql: include_str!("migrations/0030_wave4_procurement.sql") },
    Migration { version: 31, name: "wave5_commercial", sql: include_str!("migrations/0031_wave5_commercial.sql") },
    Migration { version: 32, name: "wave6_promotions", sql: include_str!("migrations/0032_wave6_promotions.sql") },
    Migration { version: 33, name: "wave7_alert_centre", sql: include_str!("migrations/0033_wave7_alert_centre.sql") },
    Migration { version: 34, name: "wave7_sync_reconciliation", sql: include_str!("migrations/0034_wave7_sync_reconciliation.sql") },
    Migration { version: 35, name: "wave7_terminal_health", sql: include_str!("migrations/0035_wave7_terminal_health.sql") },
    Migration { version: 36, name: "wave7_device_credentials", sql: include_str!("migrations/0036_wave7_device_credentials.sql") },
];

pub fn latest_schema_version() -> i64 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

/// Checksum of a migration with line endings normalised to LF, so a build
/// from a Windows checkout (CRLF) and one from Linux (LF) agree.
fn checksum(sql: &str) -> String {
    hex::encode(Sha256::digest(sql.replace("\r\n", "\n").as_bytes()))
}

/// A stored checksum matches if it equals the normalised one, or the raw hash
/// of the LF or CRLF text (databases created before normalisation).
fn checksum_matches(sql: &str, stored: &str) -> bool {
    let lf = sql.replace("\r\n", "\n");
    let crlf = lf.replace('\n', "\r\n");
    stored == checksum(sql)
        || stored == hex::encode(Sha256::digest(crlf.as_bytes()))
        || stored == hex::encode(Sha256::digest(sql.as_bytes()))
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

/// SQL functions available on every connection.
///
/// `amw_now()` returns the application clock in the same format the Rust code
/// writes timestamps with. Queries must compare stored timestamps against this,
/// never SQLite's `'now'`: the two clocks can differ by milliseconds (they do on
/// Windows), which made a price saved "now" look like a future price.
fn register_functions(conn: &Connection) -> AppResult<()> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function("amw_now", 0, FunctionFlags::SQLITE_UTF8, |_| Ok(crate::time::now_str()))?;
    conn.create_scalar_function("amw_rbranch", 0, FunctionFlags::SQLITE_UTF8, |_| Ok(REPORT_BRANCH.with(|b| b.borrow().clone())))?;
    Ok(())
}

thread_local! {
    static REPORT_BRANCH: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` with `amw_rbranch()` returning `branch` (multi-branch report
/// filter; NULL means every branch). Queries run synchronously on this
/// thread, so the value is visible to them and cleared afterwards.
pub(crate) fn with_report_branch<T>(branch: Option<String>, f: impl FnOnce() -> T) -> T {
    REPORT_BRANCH.with(|b| *b.borrow_mut() = branch);
    let out = f();
    REPORT_BRANCH.with(|b| *b.borrow_mut() = None);
    out
}

fn configure(conn: &Connection) -> AppResult<()> {
    register_functions(conn)?;
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
        register_functions(&c)?;
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
    migrate_until(conn, path, i64::MAX)
}

/// Apply migrations up to `max_version` only. The app always migrates to the
/// latest; upgrade tests use this to build a database as an older release
/// left it.
#[doc(hidden)]
pub fn migrate_until(conn: &Connection, path: &Path, max_version: i64) -> AppResult<MigrationReport> {
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
            Some(m) if checksum_matches(m.sql, sum) => {}
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
    let pending: Vec<&Migration> = MIGRATIONS.iter().filter(|m| m.version > from && m.version <= max_version).collect();
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

    /// Installations on an earlier schema (13: the last `main` release line;
    /// 19: the last soak build before product images and the WhatsApp
    /// catalogue) upgrade through the normal runner: data kept, a safety
    /// backup taken, no automatic picture search queued and nothing
    /// published to WhatsApp by the upgrade itself.
    #[test]
    fn upgrade_from_earlier_schemas_keeps_data_and_starts_nothing() {
        for from in [13, 19, 24] {
            upgrade_from(from);
        }
    }

    /// Schema 24 already holds reviewed supplier invoices from Document
    /// Intelligence. The Accounts Payable upgrade keeps them as unposted
    /// records: no liability, credit, payment, reservation or stock change.
    fn seed_supplier_invoices(c: &Connection) {
        c.execute_batch(
            "INSERT INTO suppliers(supplier_id,name,created_at,updated_at) VALUES ('S1','Gulf Foods','x','x');
             INSERT INTO supplier_invoices(invoice_id,number,doc_type,supplier_id,invoice_number,invoice_date,subtotal_minor,vat_minor,total_minor,status,posting,created_by,created_at,updated_at)
               VALUES ('I1','SI-00001','invoice','S1','GF-9','2026-08-01',1000,100,1100,'approved','not_supported','u','x','x'),
                      ('I2','SI-00002','credit_note','S1','GF-CN','2026-08-02',100,10,110,'draft','not_supported','u','x','x');
             INSERT INTO supplier_invoice_lines(invoice_id,line_no,product_id,description,qty_milli,unit_cost_minor,line_total_minor)
               VALUES ('I1',1,'P1','Laban',1000,1000,1000);",
        )
        .unwrap();
    }

    fn upgrade_from(from: i64) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, checksum TEXT NOT NULL, applied_at TEXT NOT NULL);",
            )
            .unwrap();
            for m in MIGRATIONS.iter().filter(|m| m.version <= from) {
                c.execute_batch(m.sql).unwrap();
                c.execute(
                    "INSERT INTO schema_migrations(version,name,checksum,applied_at) VALUES (?1,?2,?3,'2026-09-01T00:00:00.000Z')",
                    rusqlite::params![m.version, m.name, checksum(m.sql)],
                )
                .unwrap();
            }
            c.execute_batch(
                "INSERT INTO tax_rules(tax_rule_id,name,rate_bp,inclusive,active,effective_from,created_at)
                   VALUES ('T1','VAT',1000,1,1,'2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z');
                 INSERT INTO categories(category_id,name,created_at,updated_at) VALUES ('C1','Dairy','x','x');
                 INSERT INTO products(product_id,sku,name,category_id,tax_rule_id,created_at,updated_at)
                   VALUES ('P1','100001','Laban','C1','T1','x','x');",
            )
            .unwrap();
            if from >= 22 {
                seed_supplier_invoices(&c);
            }
        }
        let (db, rep) = Db::open(&p, false).unwrap();
        if from >= 22 {
            let (posting, status, money, lines): (String, String, i64, i64) = db
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT (SELECT group_concat(posting) FROM supplier_invoices), (SELECT group_concat(status) FROM supplier_invoices),
                                (SELECT COUNT(*) FROM ap_liabilities) + (SELECT COUNT(*) FROM ap_credits) + (SELECT COUNT(*) FROM ap_payments)
                                  + (SELECT COUNT(*) FROM ap_allocations) + (SELECT COUNT(*) FROM stock_reservations) + (SELECT COUNT(*) FROM stock_movements),
                                (SELECT COUNT(*) FROM supplier_invoice_lines)",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )?)
                })
                .unwrap();
            assert_eq!(posting, "not_posted,not_posted", "existing records are kept, unposted");
            assert_eq!(status, "approved,draft", "review states kept");
            assert_eq!(money, 0, "the upgrade posts, pays, reserves and moves nothing");
            assert_eq!(lines, 1);
        }
        assert_eq!((rep.from_version, rep.to_version), (from, latest_schema_version()));
        assert_eq!(rep.applied, (from + 1..=latest_schema_version()).collect::<Vec<_>>());
        assert!(rep.safety_backup.as_deref().is_some_and(|b| Path::new(b).exists()), "backup before upgrading");
        let (name, hash, status): (String, Option<String>, String) = db
            .read(|c| {
                Ok(c.query_row("SELECT name, image_hash, auto_image_status FROM products WHERE product_id='P1'", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?)
            })
            .unwrap();
        assert_eq!((name.as_str(), hash, status.as_str()), ("Laban", None, "not_attempted"), "no search queued by the upgrade");
        let published: i64 = db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT (SELECT COUNT(*) FROM wa_catalog_products) + (SELECT COUNT(*) FROM wa_catalog_collections)",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(published, 0, "nothing published by the upgrade");
        assert_eq!(db.integrity_check().unwrap(), vec!["ok".to_string()]);
        drop(db);
        let (_db, rep) = Db::open(&p, false).unwrap();
        assert!(rep.applied.is_empty(), "idempotent: nothing re-applied");
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
    fn crlf_and_lf_checksums_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.db");
        let (db, _) = Db::open(&p, true).unwrap();
        let lf = MIGRATIONS[0].sql.replace("\r\n", "\n");
        let crlf = lf.replace('\n', "\r\n");
        for text in [crlf, lf] {
            let sum = hex::encode(Sha256::digest(text.as_bytes()));
            db.with_writer(|c| {
                c.execute("UPDATE schema_migrations SET checksum=?1 WHERE version=1", [&sum])?;
                Ok(())
            })
            .unwrap();
            drop(Db::open(&p, false).expect("line endings alone must not block startup"));
        }
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
