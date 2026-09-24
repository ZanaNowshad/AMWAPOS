//! Multi-terminal synchronization (hub ⇄ terminals over the store LAN).
//!
//! Model
//! -----
//! * Change capture: triggers append `(table, pk, op)` to `sync_outbox`
//!   (migration 0002). Payloads are read when shipped.
//! * Authority:
//!   - `Hub` tables (catalogue, staff, settings, devices) are authoritative on
//!     the hub; terminals only receive them.
//!   - `Append` tables (sales, payments, refunds, cash events, stock
//!     movements) are immutable records created on any device, identified by
//!     ULIDs and inserted idempotently (`INSERT OR IGNORE`) everywhere. A
//!     financial record can therefore never be duplicated or lost by retries.
//!   - `Shared` tables (customers, shifts, deliveries, unknown barcodes) are
//!     mutable; the hub serializes updates in arrival order and rebroadcasts,
//!     so every node converges on the hub's order. Wall clocks are never used
//!     to decide conflicts.
//! * Stock levels are derived: applying a new stock movement adjusts the
//!   receiving node's cached level, so levels converge because addition
//!   commutes.
//! * Terminals push first, then pull (excluding their own changes).
//! * A change that cannot be applied goes to `sync_dead_letters` for review
//!   and retry; it is never dropped and never blocks other changes.
//! * Every request is HMAC-signed with a per-device key derived from the hub's
//!   master secret (kept in OS secure storage); revoked devices are refused.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use hmac::{Hmac, Mac};
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::Sha256;

use crate::audit;
use crate::auth;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ids::new_id;
use crate::service::{AppCore, DeviceIdentity};
use crate::settings;
use crate::system::DiagnosticItem;
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Policy {
    Hub,
    Append,
    Shared,
}

/// Replicated tables in dependency order (parents first).
pub const TABLES: &[(&str, &[&str], Policy)] = &[
    ("business", &["business_id"], Policy::Hub),
    ("branches", &["branch_id"], Policy::Hub),
    ("devices", &["device_id"], Policy::Hub),
    ("settings", &["key"], Policy::Hub),
    ("roles", &["role_id"], Policy::Hub),
    ("permissions", &["code"], Policy::Hub),
    ("role_permissions", &["role_id", "permission_code"], Policy::Hub),
    ("users", &["user_id"], Policy::Hub),
    ("categories", &["category_id"], Policy::Hub),
    ("tax_rules", &["tax_rule_id"], Policy::Hub),
    ("products", &["product_id"], Policy::Hub),
    ("product_barcodes", &["barcode_id"], Policy::Hub),
    ("product_prices", &["price_id"], Policy::Hub),
    ("product_cost_history", &["cost_id"], Policy::Hub),
    ("product_costs", &["product_id", "branch_id"], Policy::Hub),
    ("suppliers", &["supplier_id"], Policy::Hub),
    ("customers", &["customer_id"], Policy::Shared),
    ("customer_notes", &["note_id"], Policy::Shared),
    ("shifts", &["shift_id"], Policy::Shared),
    ("unknown_barcodes", &["barcode"], Policy::Shared),
    ("sales", &["sale_id"], Policy::Append),
    ("sale_items", &["sale_item_id"], Policy::Append),
    ("payments", &["payment_id"], Policy::Append),
    ("refunds", &["refund_id"], Policy::Append),
    ("refund_items", &["refund_item_id"], Policy::Append),
    ("refund_tenders", &["refund_tender_id"], Policy::Append),
    ("cash_events", &["cash_event_id"], Policy::Append),
    ("stock_movements", &["movement_id"], Policy::Append),
    ("delivery_orders", &["delivery_id"], Policy::Shared),
    ("delivery_events", &["event_id"], Policy::Shared),
];

/// Columns never shipped to other devices.
const PRIVATE_COLUMNS: &[(&str, &str)] = &[("devices", "credential_hash")];

pub const PROTOCOL_VERSION: i64 = 1;
pub const KEY_SYNC: &str = "local.sync";
pub const SECRET_HUB_MASTER: &str = "amwapos.hub.master_secret";
pub const SECRET_DEVICE_KEY: &str = "amwapos.sync.device_key";
const PAIRING_TTL_MINUTES: i64 = 15;
pub const DEFAULT_PORT: u16 = 47800;

pub fn policy(table: &str) -> Option<(&'static [&'static str], Policy)> {
    TABLES.iter().find(|t| t.0 == table).map(|t| (t.1, t.2))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SyncSettings {
    /// Terminal: base URL of the hub, e.g. http://192.168.1.10:47800
    pub hub_url: String,
    pub hub_instance_id: String,
    pub business_id: String,
    pub push_cursor: i64,
    pub pull_cursor: i64,
    pub last_push_at: Option<String>,
    pub last_pull_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub last_error_at: Option<String>,
    /// Hub: listening port.
    pub port: u16,
    /// Terminal: set when the hub identity no longer matches (rebuilt hub).
    pub blocked_reason: Option<String>,
    pub hub_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Change {
    pub seq: i64,
    pub table: String,
    pub pk: Map<String, Value>,
    pub op: String,
    #[serde(default)]
    pub row: Option<Map<String, Value>>,
    #[serde(default)]
    pub origin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushRequest {
    pub device_id: String,
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rejected {
    pub seq: i64,
    pub table: String,
    pub pk: Map<String, Value>,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResponse {
    pub accepted: usize,
    pub rejected: Vec<Rejected>,
    /// Highest terminal seq processed (accepted or rejected).
    pub up_to_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub device_id: String,
    pub since_seq: i64,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullResponse {
    pub changes: Vec<Change>,
    pub next_seq: i64,
    pub has_more: bool,
    pub hub_max_seq: i64,
    pub hub_instance_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubInfo {
    pub product: String,
    pub protocol: i64,
    pub app_version: String,
    pub schema_version: i64,
    pub hub_instance_id: String,
    pub business_id: String,
    pub business_name: String,
    pub hub_name: String,
    pub max_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairRequest {
    pub code: String,
    pub device_name: String,
    pub device_code: String,
    pub app_version: String,
    pub schema_version: i64,
    #[serde(default)]
    pub os_info: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub hub_seq: i64,
    pub tables: Vec<(String, Vec<Map<String, Value>>)>,
    pub stock_levels: Vec<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairResponse {
    pub device: DeviceIdentity,
    pub device_key: String,
    pub hub_instance_id: String,
    pub business_id: String,
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub device_id: String,
    pub app_version: String,
    pub schema_version: i64,
    pub pending_count: i64,
    #[serde(default)]
    pub current_user_id: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
}

// ---------- signing ----------

type HmacSha256 = Hmac<Sha256>;

pub fn hmac_hex(key: &str, msg: &[u8]) -> String {
    let mut m = HmacSha256::new_from_slice(key.as_bytes()).expect("hmac key");
    m.update(msg);
    hex::encode(m.finalize().into_bytes())
}

pub fn body_hash(body: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(Sha256::digest(body))
}

/// Canonical string signed for every request / response.
pub fn signing_string(method: &str, path: &str, ts: i64, nonce: &str, body: &[u8]) -> String {
    format!("{method}\n{path}\n{ts}\n{nonce}\n{}", body_hash(body))
}

pub fn verify_hex_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Replay protection for signed requests (hub side).
#[derive(Default)]
pub struct NonceCache(Mutex<HashMap<String, i64>>);

impl NonceCache {
    pub fn check_and_insert(&self, device: &str, nonce: &str, ts: i64) -> bool {
        let mut m = match self.0.lock() {
            Ok(m) => m,
            Err(_) => return false,
        };
        let now = time::now().timestamp_millis();
        m.retain(|_, t| now - *t < 10 * 60 * 1000);
        let key = format!("{device}:{nonce}");
        if m.contains_key(&key) {
            return false;
        }
        m.insert(key, ts);
        true
    }
}

// ---------- row (de)serialization ----------

static COLUMNS: Mutex<Option<HashMap<String, Vec<String>>>> = Mutex::new(None);

fn columns(c: &Connection, table: &str) -> AppResult<Vec<String>> {
    if policy(table).is_none() && table != "stock_levels" {
        return Err(AppError::validation(format!("Table {table} is not replicated.")));
    }
    if let Ok(g) = COLUMNS.lock() {
        if let Some(m) = g.as_ref() {
            if let Some(v) = m.get(table) {
                return Ok(v.clone());
            }
        }
    }
    let mut st = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols: Vec<String> = st.query_map([], |r| r.get::<_, String>(1))?.collect::<Result<Vec<_>, _>>()?;
    if let Ok(mut g) = COLUMNS.lock() {
        g.get_or_insert_with(HashMap::new).insert(table.to_string(), cols.clone());
    }
    Ok(cols)
}

fn to_json(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).to_string()),
        ValueRef::Blob(b) => Value::String(hex::encode(b)),
    }
}

fn to_sql(v: &Value) -> AppResult<SqlValue> {
    Ok(match v {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(*b as i64),
        Value::Number(n) => match n.as_i64() {
            Some(i) => SqlValue::Integer(i),
            None => SqlValue::Real(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => SqlValue::Text(s.clone()),
        _ => return Err(AppError::validation("Nested values are not allowed in replicated rows.")),
    })
}

fn read_rows(c: &Connection, table: &str, where_sql: &str, args: &[SqlValue]) -> AppResult<Vec<Map<String, Value>>> {
    let cols = columns(c, table)?;
    let sql = format!("SELECT {} FROM {table} {where_sql}", cols.iter().map(|x| format!("\"{x}\"")).collect::<Vec<_>>().join(","));
    let mut st = c.prepare(&sql)?;
    let mut rows = st.query(params_from_iter(args.iter()))?;
    let mut out = vec![];
    while let Some(r) = rows.next()? {
        let mut m = Map::new();
        for (i, col) in cols.iter().enumerate() {
            if PRIVATE_COLUMNS.iter().any(|(t, cc)| *t == table && cc == col) {
                continue;
            }
            m.insert(col.clone(), to_json(r.get_ref(i)?));
        }
        out.push(m);
    }
    Ok(out)
}

fn read_row(c: &Connection, table: &str, pk: &Map<String, Value>) -> AppResult<Option<Map<String, Value>>> {
    let (pks, _) = policy(table).ok_or_else(|| AppError::validation(format!("Table {table} is not replicated.")))?;
    let mut args = vec![];
    let mut w = vec![];
    for (i, k) in pks.iter().enumerate() {
        w.push(format!("\"{k}\" = ?{}", i + 1));
        args.push(to_sql(pk.get(*k).unwrap_or(&Value::Null))?);
    }
    Ok(read_rows(c, table, &format!("WHERE {}", w.join(" AND ")), &args)?.into_iter().next())
}

fn outbox_changes(c: &Connection, since: i64, limit: i64, exclude_origin: Option<&str>) -> AppResult<(Vec<Change>, i64, bool)> {
    let mut st = c.prepare(
        "SELECT seq, table_name, row_pk, op, origin FROM sync_outbox WHERE seq > ?1 AND (?2 IS NULL OR origin IS NULL OR origin <> ?2) ORDER BY seq LIMIT ?3",
    )?;
    let raw: Vec<(i64, String, String, String, Option<String>)> = st
        .query_map(params![since, exclude_origin, limit + 1], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let has_more = raw.len() as i64 > limit;
    let raw: Vec<_> = raw.into_iter().take(limit as usize).collect();
    // Advance past entries excluded by origin as well.
    let scanned_to: i64 = if has_more {
        raw.last().map(|r| r.0).unwrap_or(since)
    } else {
        c.query_row("SELECT COALESCE(MAX(seq), ?1) FROM sync_outbox", [since], |r| r.get(0))?
    };
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    let mut out = vec![];
    for (seq, table, pk_s, op, origin) in raw {
        if !seen.insert((table.clone(), pk_s.clone(), op.clone())) {
            continue;
        }
        if policy(&table).is_none() {
            continue;
        }
        let pk: Map<String, Value> = serde_json::from_str(&pk_s)?;
        let row = if op == "upsert" {
            match read_row(c, &table, &pk)? {
                Some(r) => Some(r),
                None => continue, // deleted since; a later delete entry follows
            }
        } else {
            None
        };
        out.push(Change { seq, table, pk, op, row, origin });
    }
    Ok((out, scanned_to, has_more))
}

pub enum ApplySide<'a> {
    /// Hub applying a terminal push. The string is the pushing device id.
    Hub(&'a str),
    /// Terminal applying hub changes.
    Terminal,
}

fn set_control(c: &Connection, suppress: bool, origin: Option<&str>) -> AppResult<()> {
    c.execute("UPDATE sync_control SET v=?1 WHERE k='suppress'", [if suppress { "1" } else { "0" }])?;
    c.execute("UPDATE sync_control SET v=?1 WHERE k='origin'", [origin])?;
    Ok(())
}

fn validate_hub_push(c: &Connection, ch: &Change, device: &str) -> AppResult<()> {
    let (_, pol) = policy(&ch.table).ok_or_else(|| AppError::validation("Table is not replicated."))?;
    if pol == Policy::Hub {
        return Err(AppError::new(ErrorCode::Forbidden, format!("Terminals cannot change {} (hub-managed).", ch.table)));
    }
    if ch.op == "delete" {
        return Err(AppError::new(ErrorCode::Forbidden, "Terminals cannot delete replicated records."));
    }
    let row = ch.row.as_ref().ok_or_else(|| AppError::validation("Missing row data."))?;
    let owns = |col: &str| row.get(col).and_then(|v| v.as_str()) == Some(device);
    let parent_owned = |table: &str, id_col: &str, parent_col: &str| -> AppResult<bool> {
        let pid = row.get(parent_col).and_then(|v| v.as_str()).unwrap_or("");
        Ok(c.query_row(&format!("SELECT device_id FROM {table} WHERE {id_col}=?1"), [pid], |r| r.get::<_, String>(0))
            .optional()?
            .map(|d| d == device)
            .unwrap_or(false))
    };
    let ok = match ch.table.as_str() {
        "sales" | "refunds" | "cash_events" | "shifts" | "stock_movements" => owns("device_id"),
        "sale_items" | "payments" => parent_owned("sales", "sale_id", "sale_id")?,
        "refund_items" | "refund_tenders" => parent_owned("refunds", "refund_id", "refund_id")?,
        _ => true,
    };
    if !ok {
        return Err(AppError::new(ErrorCode::Forbidden, format!("{} record is not owned by the pushing terminal.", ch.table)));
    }
    Ok(())
}

/// Apply one change. Returns true when something was written.
pub fn apply_change(c: &Connection, ch: &Change, side: &ApplySide) -> AppResult<bool> {
    let (pks, pol) = policy(&ch.table).ok_or_else(|| AppError::validation(format!("Table {} is not replicated.", ch.table)))?;
    if let ApplySide::Hub(dev) = side {
        validate_hub_push(c, ch, dev)?;
    }
    let cols = columns(c, &ch.table)?;
    if ch.op == "delete" {
        if pol == Policy::Append {
            return Err(AppError::validation("Append-only records cannot be deleted."));
        }
        let mut w = vec![];
        let mut args = vec![];
        for (i, k) in pks.iter().enumerate() {
            w.push(format!("\"{k}\" = ?{}", i + 1));
            args.push(to_sql(ch.pk.get(*k).unwrap_or(&Value::Null))?);
        }
        let n = c.execute(&format!("DELETE FROM {} WHERE {}", ch.table, w.join(" AND ")), params_from_iter(args.iter()))?;
        return Ok(n > 0);
    }
    if ch.op != "upsert" {
        return Err(AppError::validation("Unknown change operation."));
    }
    let mut row = ch.row.clone().ok_or_else(|| AppError::validation("Missing row data."))?;
    for k in row.keys() {
        if !cols.contains(k) {
            return Err(AppError::validation(format!("Unknown column {}.{k} (version mismatch?).", ch.table)));
        }
    }
    for k in pks.iter() {
        if row.get(*k).map(|v| v.is_null()).unwrap_or(true) {
            return Err(AppError::validation("Missing primary key."));
        }
    }
    // Never overwrite a local-only secret column.
    if ch.table == "devices" {
        row.remove("credential_hash");
    }
    // Stock movements: insert once, recompute the running balance locally,
    // and adjust the cached stock level.
    if ch.table == "stock_movements" {
        let id = row.get("movement_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let exists: bool = c.query_row("SELECT 1 FROM stock_movements WHERE movement_id=?1", [&id], |_| Ok(true)).optional()?.unwrap_or(false);
        if exists {
            return Ok(false);
        }
        let pid = row.get("product_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let br = row.get("branch_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let delta = row.get("qty_delta_milli").and_then(|v| v.as_i64()).unwrap_or(0);
        let now = time::now_str();
        c.execute(
            "INSERT INTO stock_levels(product_id, branch_id, qty_milli, last_movement_at, updated_at) VALUES (?1,?2,?3,?4,?4)
             ON CONFLICT(product_id, branch_id) DO UPDATE SET qty_milli = qty_milli + ?3, last_movement_at=?4, updated_at=?4",
            params![pid, br, delta, now],
        )?;
        let bal: i64 = c.query_row("SELECT qty_milli FROM stock_levels WHERE product_id=?1 AND branch_id=?2", params![pid, br], |r| r.get(0))?;
        row.insert("balance_after_milli".into(), json!(bal));
    }
    let keys: Vec<String> = row.keys().cloned().collect();
    let placeholders: Vec<String> = (1..=keys.len()).map(|i| format!("?{i}")).collect();
    let args: Vec<SqlValue> = keys.iter().map(|k| to_sql(&row[k])).collect::<AppResult<_>>()?;
    let quoted: Vec<String> = keys.iter().map(|k| format!("\"{k}\"")).collect();
    let sql = match pol {
        Policy::Append => format!("INSERT OR IGNORE INTO {} ({}) VALUES ({})", ch.table, quoted.join(","), placeholders.join(",")),
        _ => {
            let updates: Vec<String> = keys
                .iter()
                .filter(|k| !pks.contains(&k.as_str()))
                .map(|k| format!("\"{k}\"=excluded.\"{k}\""))
                .collect();
            let conflict = pks.iter().map(|k| format!("\"{k}\"")).collect::<Vec<_>>().join(",");
            if updates.is_empty() {
                format!("INSERT OR IGNORE INTO {} ({}) VALUES ({})", ch.table, quoted.join(","), placeholders.join(","))
            } else {
                format!(
                    "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT({conflict}) DO UPDATE SET {}",
                    ch.table,
                    quoted.join(","),
                    placeholders.join(","),
                    updates.join(",")
                )
            }
        }
    };
    let n = c.execute(&sql, params_from_iter(args.iter()))?;
    Ok(n > 0)
}

fn record_dead_letter(c: &Connection, direction: &str, origin: Option<&str>, ch: &Change, error: &str) -> AppResult<()> {
    let now = time::now_str();
    let pk = serde_json::to_string(&ch.pk)?;
    let existing: Option<String> = c
        .query_row(
            "SELECT dead_id FROM sync_dead_letters WHERE direction=?1 AND table_name=?2 AND row_pk=?3 AND status='open'",
            params![direction, ch.table, pk],
            |r| r.get(0),
        )
        .optional()?;
    match existing {
        Some(id) => {
            c.execute(
                "UPDATE sync_dead_letters SET attempts=attempts+1, error=?2, payload_json=?3, last_attempt_at=?4 WHERE dead_id=?1",
                params![id, error, serde_json::to_string(ch)?, now],
            )?;
        }
        None => {
            c.execute(
                "INSERT INTO sync_dead_letters(dead_id, direction, origin, table_name, row_pk, op, payload_json, error, attempts, status, created_at, last_attempt_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,1,'open',?9,?9)",
                params![new_id(), direction, origin, ch.table, pk, ch.op, serde_json::to_string(ch)?, error, now],
            )?;
        }
    }
    Ok(())
}

fn sync_settings(c: &Connection) -> AppResult<SyncSettings> {
    settings::get(c, KEY_SYNC)
}

fn hub_instance_id(c: &Connection) -> AppResult<String> {
    // The business id plus the first migration time identifies this database instance.
    let bid: String = c.query_row("SELECT business_id FROM business LIMIT 1", [], |r| r.get(0))?;
    let at: String = c.query_row("SELECT applied_at FROM schema_migrations WHERE version=1", [], |r| r.get(0))?;
    Ok(auth::sha256_hex(&format!("{bid}|{at}"))[..24].to_string())
}

impl AppCore {
    // ---------------- hub side ----------------

    fn require_hub(&self) -> AppResult<DeviceIdentity> {
        let d = self.require_device()?;
        if d.mode != "hub" {
            return Err(AppError::conflict("This computer is not running as a hub."));
        }
        Ok(d)
    }

    pub fn hub_master_secret(&self) -> AppResult<String> {
        match self.secrets.get(SECRET_HUB_MASTER)? {
            Some(s) => Ok(s),
            None => {
                let s = auth::random_token();
                self.secrets.set(SECRET_HUB_MASTER, &s)?;
                Ok(s)
            }
        }
    }

    pub fn hub_device_key(&self, device_id: &str) -> AppResult<String> {
        Ok(hmac_hex(&self.hub_master_secret()?, format!("device:{device_id}").as_bytes()))
    }

    /// Authenticate a signed request. Returns the active device row id.
    pub fn hub_authenticate(&self, nonces: &NonceCache, device_id: &str, ts: i64, nonce: &str, signature: &str, method: &str, path: &str, body: &[u8]) -> AppResult<String> {
        self.require_hub()?;
        let id = validate::id(device_id, "Device")?;
        let now = time::now().timestamp_millis();
        if (now - ts).abs() > 5 * 60 * 1000 {
            return Err(AppError::new(ErrorCode::Unauthenticated, "Request timestamp outside the allowed window. Check the terminal clock."));
        }
        if nonce.len() < 16 || nonce.len() > 64 {
            return Err(AppError::new(ErrorCode::Unauthenticated, "Invalid nonce."));
        }
        let active: Option<i64> = self.db.read(|c| Ok(c.query_row("SELECT active FROM devices WHERE device_id=?1", [&id], |r| r.get(0)).optional()?))?;
        match active {
            Some(1) => {}
            Some(_) => return Err(AppError::new(ErrorCode::Forbidden, "This terminal has been revoked. Ask the owner to re-activate or re-pair it.")),
            None => return Err(AppError::new(ErrorCode::Unauthenticated, "Unknown terminal. Pair it with the hub again.")),
        }
        let key = self.hub_device_key(&id)?;
        let expect = hmac_hex(&key, signing_string(method, path, ts, nonce, body).as_bytes());
        if !verify_hex_eq(&expect, signature) {
            return Err(AppError::new(ErrorCode::Unauthenticated, "Invalid request signature."));
        }
        if !nonces.check_and_insert(&id, nonce, ts) {
            return Err(AppError::new(ErrorCode::Unauthenticated, "Replayed request rejected."));
        }
        Ok(id)
    }

    pub fn hub_info(&self) -> AppResult<HubInfo> {
        let d = self.require_hub()?;
        self.db.read(|c| {
            let (bid, bname): (String, String) = c.query_row("SELECT business_id, name FROM business LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(HubInfo {
                product: "AMWAPOS".into(),
                protocol: PROTOCOL_VERSION,
                app_version: audit::APP_VERSION.into(),
                schema_version: crate::db::latest_schema_version(),
                hub_instance_id: hub_instance_id(c)?,
                business_id: bid,
                business_name: bname,
                hub_name: d.name.clone(),
                max_seq: c.query_row("SELECT COALESCE(MAX(seq),0) FROM sync_outbox", [], |r| r.get(0))?,
            })
        })
    }

    /// Convert a standalone store into a hub (idempotent).
    pub fn sync_enable_hub(&self, token: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("sync.manage")?;
        let mut d = self.require_device()?;
        if d.mode == "terminal" {
            return Err(AppError::conflict("A terminal cannot become a hub."));
        }
        let actor = self.actor(&s, None);
        if d.mode != "hub" {
            d.mode = "hub".into();
            self.db.write(|tx| {
                tx.execute("UPDATE devices SET operating_mode='hub' WHERE device_id=?1", [&d.device_id])?;
                settings::put(tx, settings::KEY_DEVICE, &d, Some(&s.user_id))?;
                let mut ss = sync_settings(tx)?;
                if ss.port == 0 {
                    ss.port = DEFAULT_PORT;
                }
                settings::put(tx, KEY_SYNC, &ss, Some(&s.user_id))?;
                audit::record(tx, &actor, "sync.hub_enabled", "device", Some(&d.device_id), None, None)?;
                Ok(())
            })?;
            self.set_device(Some(d));
        }
        self.hub_master_secret()?;
        self.sync_status(token)
    }

    /// Issue a one-time pairing code (shown on the hub, typed on the terminal).
    pub fn sync_issue_pairing_code(&self, token: &str, device_name: Option<String>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("devices.manage")?;
        let d = self.require_hub()?;
        use rand::Rng;
        let code: String = format!("{:08}", rand::rngs::OsRng.gen_range(0..100_000_000u32));
        let now = time::now();
        let expires = time::fmt(now + chrono::Duration::minutes(PAIRING_TTL_MINUTES));
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            tx.execute(
                "INSERT INTO pairing_codes(code_hash, branch_id, device_name, created_by, created_at, expires_at) VALUES (?1,?2,?3,?4,?5,?6)",
                params![auth::sha256_hex(&code), d.branch_id, device_name, s.user_id, time::fmt(now), expires],
            )?;
            audit::record(tx, &actor, "sync.pairing_code_issued", "device", None, None, Some(&json!({ "expires_at": expires })))?;
            Ok(())
        })?;
        Ok(json!({ "code": code, "expires_at": expires }))
    }

    /// Hub: validate a pairing code, register the terminal and return its key
    /// and a bootstrap snapshot.
    pub fn hub_pair(&self, req: PairRequest) -> AppResult<PairResponse> {
        let hub = self.require_hub()?;
        if req.schema_version != crate::db::latest_schema_version() {
            return Err(AppError::conflict(format!(
                "Version mismatch: the hub uses schema {}, this terminal uses {}. Install the same AMWAPOS version on both.",
                crate::db::latest_schema_version(),
                req.schema_version
            )));
        }
        let code = req.code.trim().to_string();
        if code.len() != 8 || !code.chars().all(|c| c.is_ascii_digit()) {
            return Err(AppError::new(ErrorCode::InvalidCredentials, "Invalid pairing code."));
        }
        let name = crate::setup::clean(&req.device_name, "Terminal name", 60, true)?;
        let dcode = crate::setup::validate_code(&req.device_code, "Terminal code")?;
        let device_id = new_id();
        let now = time::now_str();
        let identity = DeviceIdentity { device_id: device_id.clone(), device_code: dcode.clone(), name: name.clone(), branch_id: hub.branch_id.clone(), mode: "terminal".into() };
        let hash = auth::sha256_hex(&code);
        self.db.write(|tx| {
            let row: Option<(String, Option<String>)> = tx
                .query_row("SELECT expires_at, used_at FROM pairing_codes WHERE code_hash=?1", [&hash], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            match row {
                Some((exp, None)) if exp > now => {}
                Some((_, Some(_))) => return Err(AppError::new(ErrorCode::InvalidCredentials, "This pairing code was already used. Issue a new one on the hub.")),
                Some(_) => return Err(AppError::new(ErrorCode::InvalidCredentials, "This pairing code has expired. Issue a new one on the hub.")),
                None => return Err(AppError::new(ErrorCode::InvalidCredentials, "Invalid pairing code.")),
            }
            let taken: bool = tx.query_row("SELECT 1 FROM devices WHERE device_code=?1", [&dcode], |_| Ok(true)).optional()?.unwrap_or(false);
            if taken {
                return Err(AppError::duplicate(format!("Terminal code {dcode} is already used. Choose another code.")));
            }
            tx.execute("UPDATE pairing_codes SET used_at=?2, used_by_device=?3 WHERE code_hash=?1", params![hash, now, device_id])?;
            tx.execute(
                "INSERT INTO devices(device_id, branch_id, name, device_code, operating_mode, active, activated_at, app_version, os_info) VALUES (?1,?2,?3,?4,'terminal',1,?5,?6,?7)",
                params![device_id, hub.branch_id, name, dcode, now, req.app_version, req.os_info],
            )?;
            audit::record(
                tx,
                &audit::Actor { device_id: Some(hub.device_id.clone()), branch_id: Some(hub.branch_id.clone()), ..Default::default() },
                "sync.terminal_paired",
                "device",
                Some(&device_id),
                None,
                Some(&json!({ "name": name, "code": dcode })),
            )?;
            Ok(())
        })?;
        let snapshot = self.hub_snapshot()?;
        let (hid, bid) = self.db.read(|c| Ok((hub_instance_id(c)?, c.query_row("SELECT business_id FROM business LIMIT 1", [], |r| r.get::<_, String>(0))?)))?;
        Ok(PairResponse { device: identity, device_key: self.hub_device_key(&device_id)?, hub_instance_id: hid, business_id: bid, snapshot })
    }

    /// Consistent snapshot of everything a new terminal needs.
    pub fn hub_snapshot(&self) -> AppResult<Snapshot> {
        self.db.read(|c| {
            c.execute_batch("BEGIN")?;
            let res = (|| -> AppResult<Snapshot> {
                let hub_seq: i64 = c.query_row("SELECT COALESCE(MAX(seq),0) FROM sync_outbox", [], |r| r.get(0))?;
                let since = time::fmt(time::now() - chrono::Duration::days(45));
                let mut tables = vec![];
                for (t, _, pol) in TABLES {
                    let rows = match (*pol, *t) {
                        (_, "settings") => read_rows(c, t, "WHERE key NOT LIKE 'local.%'", &[])?,
                        (_, "categories") => read_rows(c, t, "ORDER BY parent_id IS NOT NULL, created_at", &[])?,
                        (Policy::Hub, _) | (Policy::Shared, _) if *t != "shifts" && *t != "delivery_events" && *t != "delivery_orders" => read_rows(c, t, "", &[])?,
                        (_, "shifts") => read_rows(c, t, "WHERE opened_at >= ?1 OR status='open'", &[SqlValue::Text(since.clone())])?,
                        (_, "delivery_orders") => read_rows(c, t, "WHERE created_at >= ?1 OR status IN ('pending','preparing','dispatched')", &[SqlValue::Text(since.clone())])?,
                        (_, "delivery_events") => read_rows(c, t, "WHERE delivery_id IN (SELECT delivery_id FROM delivery_orders WHERE created_at >= ?1 OR status IN ('pending','preparing','dispatched'))", &[SqlValue::Text(since.clone())])?,
                        (_, "sales") => read_rows(c, t, "WHERE completed_at >= ?1", &[SqlValue::Text(since.clone())])?,
                        (_, "sale_items") | (_, "payments") => read_rows(c, t, "WHERE sale_id IN (SELECT sale_id FROM sales WHERE completed_at >= ?1)", &[SqlValue::Text(since.clone())])?,
                        (_, "refunds") => read_rows(c, t, "WHERE original_sale_id IN (SELECT sale_id FROM sales WHERE completed_at >= ?1)", &[SqlValue::Text(since.clone())])?,
                        (_, "refund_items") | (_, "refund_tenders") => read_rows(c, t, "WHERE refund_id IN (SELECT refund_id FROM refunds WHERE original_sale_id IN (SELECT sale_id FROM sales WHERE completed_at >= ?1))", &[SqlValue::Text(since.clone())])?,
                        // Historical cash events and movements are summarized by stock_levels / shifts.
                        (_, "cash_events") | (_, "stock_movements") => vec![],
                        _ => read_rows(c, t, "", &[])?,
                    };
                    tables.push((t.to_string(), rows));
                }
                let stock_levels = read_rows(c, "stock_levels", "", &[])?;
                Ok(Snapshot { hub_seq, tables, stock_levels })
            })();
            let _ = c.execute_batch("COMMIT");
            res
        })
    }

    /// Hub: apply a terminal's pushed changes; each change in its own savepoint.
    pub fn hub_apply_push(&self, device_id: &str, req: PushRequest) -> AppResult<PushResponse> {
        self.require_hub()?;
        if req.device_id != device_id {
            return Err(AppError::new(ErrorCode::Forbidden, "Device mismatch."));
        }
        if req.changes.len() > 2000 {
            return Err(AppError::validation("Push at most 2,000 changes per request."));
        }
        self.db.write(|tx| {
            set_control(tx, false, Some(device_id))?;
            let mut accepted = 0;
            let mut rejected = vec![];
            let mut up_to = 0;
            for ch in &req.changes {
                up_to = up_to.max(ch.seq);
                tx.execute_batch("SAVEPOINT change")?;
                match apply_change(tx, ch, &ApplySide::Hub(device_id)) {
                    Ok(_) => {
                        tx.execute_batch("RELEASE change")?;
                        accepted += 1;
                    }
                    Err(e) => {
                        tx.execute_batch("ROLLBACK TO change; RELEASE change")?;
                        record_dead_letter(tx, "apply", Some(device_id), ch, &e.message)?;
                        rejected.push(Rejected { seq: ch.seq, table: ch.table.clone(), pk: ch.pk.clone(), error: e.message });
                    }
                }
            }
            set_control(tx, false, None)?;
            let now = time::now_str();
            tx.execute(
                "INSERT INTO device_heartbeats(device_id, last_seen_at, last_push_at) VALUES (?1,?2,?2)
                 ON CONFLICT(device_id) DO UPDATE SET last_seen_at=?2, last_push_at=?2",
                params![device_id, now],
            )?;
            Ok(PushResponse { accepted, rejected, up_to_seq: up_to })
        })
    }

    pub fn hub_pull(&self, device_id: &str, req: PullRequest) -> AppResult<PullResponse> {
        self.require_hub()?;
        let limit = req.limit.unwrap_or(500).clamp(1, 2000);
        let (changes, next, has_more, max, hid) = self.db.read(|c| {
            let max: i64 = c.query_row("SELECT COALESCE(MAX(seq),0) FROM sync_outbox", [], |r| r.get(0))?;
            let (changes, next, has_more) = outbox_changes(c, req.since_seq, limit, Some(device_id))?;
            Ok((changes, next, has_more, max, hub_instance_id(c)?))
        })?;
        self.db.write(|tx| {
            tx.execute(
                "INSERT INTO device_heartbeats(device_id, last_seen_at, last_pull_at) VALUES (?1,?2,?2)
                 ON CONFLICT(device_id) DO UPDATE SET last_seen_at=?2, last_pull_at=?2",
                params![device_id, time::now_str()],
            )?;
            Ok(())
        })?;
        Ok(PullResponse { changes, next_seq: next, has_more, hub_max_seq: max, hub_instance_id: hid })
    }

    pub fn hub_heartbeat(&self, device_id: &str, hb: Heartbeat) -> AppResult<Value> {
        self.require_hub()?;
        self.db.write(|tx| {
            tx.execute(
                "INSERT INTO device_heartbeats(device_id, last_seen_at, app_version, schema_version, pending_count, last_error, current_user_id)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(device_id) DO UPDATE SET last_seen_at=?2, app_version=?3, schema_version=?4, pending_count=?5, last_error=?6, current_user_id=?7",
                params![device_id, time::now_str(), hb.app_version, hb.schema_version, hb.pending_count, hb.last_error, hb.current_user_id],
            )?;
            Ok(())
        })?;
        Ok(json!({ "ok": true, "server_time": time::now_str(), "schema_version": crate::db::latest_schema_version(), "app_version": audit::APP_VERSION }))
    }

    // ---------------- terminal side ----------------

    /// Fresh install: store pairing result and load the hub snapshot.
    pub fn terminal_bootstrap(&self, hub_url: &str, pr: PairResponse) -> AppResult<Value> {
        if self.device().is_some() {
            return Err(AppError::conflict("This installation is already set up."));
        }
        let hub_url = hub_url.trim_end_matches('/').to_string();
        self.secrets.set(SECRET_DEVICE_KEY, &pr.device_key)?;
        let identity = pr.device.clone();
        self.db.write(|tx| {
            let existing: i64 = tx.query_row("SELECT COUNT(*) FROM business", [], |r| r.get(0))?;
            if existing > 0 {
                return Err(AppError::conflict("This database already contains a store. Terminals must start from an empty installation."));
            }
            set_control(tx, true, None)?;
            tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
            for (table, rows) in &pr.snapshot.tables {
                if policy(table).is_none() {
                    return Err(AppError::validation(format!("Unexpected table {table} in snapshot.")));
                }
                for (i, row) in rows.iter().enumerate() {
                    let (pks, _) = policy(table).unwrap();
                    let pk: Map<String, Value> = pks.iter().map(|k| (k.to_string(), row.get(*k).cloned().unwrap_or(Value::Null))).collect();
                    // Movements are not part of snapshots; stock levels are loaded below.
                    let ch = Change { seq: i as i64, table: table.clone(), pk, op: "upsert".into(), row: Some(row.clone()), origin: None };
                    apply_change(tx, &ch, &ApplySide::Terminal).map_err(|e| AppError::new(e.code, format!("Snapshot {table}: {}", e.message)))?;
                }
            }
            let cols = columns(tx, "stock_levels")?;
            for row in &pr.snapshot.stock_levels {
                let keys: Vec<&String> = row.keys().filter(|k| cols.contains(k)).collect();
                let sql = format!(
                    "INSERT OR REPLACE INTO stock_levels ({}) VALUES ({})",
                    keys.iter().map(|k| format!("\"{k}\"")).collect::<Vec<_>>().join(","),
                    (1..=keys.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",")
                );
                let args: Vec<SqlValue> = keys.iter().map(|k| to_sql(&row[*k])).collect::<AppResult<_>>()?;
                tx.execute(&sql, params_from_iter(args.iter()))?;
            }
            set_control(tx, false, None)?;
            // Our own snapshot application must not be pushed back.
            tx.execute("DELETE FROM sync_outbox", [])?;
            let ss = SyncSettings {
                hub_url: hub_url.clone(),
                hub_instance_id: pr.hub_instance_id.clone(),
                business_id: pr.business_id.clone(),
                pull_cursor: pr.snapshot.hub_seq,
                push_cursor: 0,
                last_success_at: Some(time::now_str()),
                ..Default::default()
            };
            settings::put(tx, KEY_SYNC, &ss, None)?;
            settings::put(tx, settings::KEY_DEVICE, &identity, None)?;
            settings::put(tx, settings::KEY_PRINTER, &settings::PrinterSettings::default(), None)?;
            settings::put(
                tx,
                settings::KEY_BACKUP,
                &settings::BackupSettings { directory: self.data_dir.join("backups").to_string_lossy().to_string(), ..Default::default() },
                None,
            )?;
            audit::record(
                tx,
                &audit::Actor { device_id: Some(identity.device_id.clone()), branch_id: Some(identity.branch_id.clone()), ..Default::default() },
                "sync.terminal_bootstrapped",
                "device",
                Some(&identity.device_id),
                None,
                Some(&json!({ "hub_url": hub_url, "hub_seq": pr.snapshot.hub_seq })),
            )?;
            settings::put(tx, settings::KEY_SETUP_COMPLETE, &json!({ "at": time::now_str(), "via": "pairing" }), None)?;
            Ok(())
        })?;
        self.write_marker()?;
        self.set_device(Some(identity));
        Ok(json!({ "ok": true }))
    }

    pub fn terminal_sync_settings(&self) -> AppResult<SyncSettings> {
        self.db.read(sync_settings)
    }

    pub fn terminal_device_key(&self) -> AppResult<String> {
        self.secrets
            .get(SECRET_DEVICE_KEY)?
            .ok_or_else(|| AppError::new(ErrorCode::Sync, "This terminal's hub credential is missing from secure storage. Pair it with the hub again."))
    }

    /// Terminal: local changes not yet accepted by the hub.
    pub fn terminal_collect_push(&self, limit: i64) -> AppResult<(Vec<Change>, i64)> {
        let d = self.require_device()?;
        if d.mode != "terminal" {
            return Err(AppError::conflict("Not a terminal."));
        }
        self.db.read(|c| {
            let ss = sync_settings(c)?;
            let (mut changes, scanned, _) = outbox_changes(c, ss.push_cursor, limit, None)?;
            // Hub-managed tables are never pushed (e.g. local role seeding).
            changes.retain(|ch| policy(&ch.table).map(|p| p.1 != Policy::Hub).unwrap_or(false));
            Ok((changes, scanned))
        })
    }

    pub fn terminal_record_push(&self, scanned_to: i64, resp: &PushResponse, pushed: &[Change]) -> AppResult<()> {
        self.db.write(|tx| {
            for r in &resp.rejected {
                if let Some(ch) = pushed.iter().find(|c| c.seq == r.seq) {
                    record_dead_letter(tx, "push", None, ch, &r.error)?;
                }
            }
            let mut ss = sync_settings(tx)?;
            ss.push_cursor = ss.push_cursor.max(scanned_to);
            ss.last_push_at = Some(time::now_str());
            settings::put(tx, KEY_SYNC, &ss, None)?;
            Ok(())
        })
    }

    /// Terminal: apply a pull batch. Rows with unpushed local edits are skipped
    /// (the local version reaches the hub first and is rebroadcast).
    pub fn terminal_apply_pull(&self, resp: &PullResponse) -> AppResult<(usize, usize)> {
        // Identity checks first: a rebuilt or rolled-back hub must never become
        // the authority over this terminal's records.
        let ss = self.terminal_sync_settings()?;
        let blocked = if !ss.hub_instance_id.is_empty() && ss.hub_instance_id != resp.hub_instance_id {
            Some("The hub's database has changed (rebuilt or restored). Synchronization is paused to protect this terminal's records. Check the hub, then confirm in Sync settings.")
        } else if resp.hub_max_seq < ss.pull_cursor {
            Some("The hub reports fewer changes than this terminal has already received. It may have been restored from an older backup. Synchronization is paused.")
        } else {
            None
        };
        if let Some(reason) = blocked {
            self.db.write(|tx| {
                let mut ss = sync_settings(tx)?;
                ss.blocked_reason = Some(reason.to_string());
                settings::put(tx, KEY_SYNC, &ss, None)
            })?;
            return Err(AppError::new(ErrorCode::Sync, reason));
        }
        let res = self.db.write(|tx| {
            let mut ss = sync_settings(tx)?;
            if ss.hub_instance_id.is_empty() {
                ss.hub_instance_id = resp.hub_instance_id.clone();
            }
            let pending: HashSet<(String, String)> = {
                let mut st = tx.prepare("SELECT table_name, row_pk FROM sync_outbox WHERE seq > ?1")?;
                let rows = st.query_map([ss.push_cursor], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<HashSet<_>, _>>()?;
                rows
            };
            set_control(tx, true, None)?;
            let mut applied = 0;
            let mut failed = 0;
            for ch in &resp.changes {
                let key = (ch.table.clone(), serde_json::to_string(&ch.pk)?);
                if pending.contains(&key) && policy(&ch.table).map(|p| p.1 == Policy::Shared).unwrap_or(false) {
                    continue;
                }
                tx.execute_batch("SAVEPOINT change")?;
                match apply_change(tx, ch, &ApplySide::Terminal) {
                    Ok(_) => {
                        tx.execute_batch("RELEASE change")?;
                        applied += 1;
                    }
                    Err(e) => {
                        tx.execute_batch("ROLLBACK TO change; RELEASE change")?;
                        record_dead_letter(tx, "pull", ch.origin.as_deref(), ch, &e.message)?;
                        failed += 1;
                    }
                }
            }
            set_control(tx, false, None)?;
            ss.pull_cursor = ss.pull_cursor.max(resp.next_seq);
            ss.last_pull_at = Some(time::now_str());
            settings::put(tx, KEY_SYNC, &ss, None)?;
            Ok((applied, failed))
        })?;
        self.reload_device_identity()?;
        Ok(res)
    }

    /// Refresh the in-memory device identity (the hub may rename a terminal).
    fn reload_device_identity(&self) -> AppResult<()> {
        if let Some(mut d) = self.device() {
            let name: Option<String> = self
                .db
                .read(|c| Ok(c.query_row("SELECT name FROM devices WHERE device_id=?1", [&d.device_id], |r| r.get(0)).optional()?))?;
            if let Some(name) = name {
                if name != d.name {
                    d.name = name;
                    self.db.write(|tx| settings::put(tx, settings::KEY_DEVICE, &d, None))?;
                    self.set_device(Some(d));
                }
            }
        }
        Ok(())
    }

    pub fn terminal_note_result(&self, error: Option<String>, hub_version: Option<String>) -> AppResult<()> {
        self.db.write(|tx| {
            let mut ss = sync_settings(tx)?;
            let now = time::now_str();
            match error {
                Some(e) => {
                    ss.last_error = Some(e);
                    ss.last_error_at = Some(now);
                }
                None => {
                    ss.last_error = None;
                    ss.last_success_at = Some(now);
                }
            }
            if hub_version.is_some() {
                ss.hub_version = hub_version;
            }
            settings::put(tx, KEY_SYNC, &ss, None)?;
            Ok(())
        })
    }

    pub fn terminal_pending_count(&self) -> AppResult<i64> {
        self.db.read(|c| {
            let ss = sync_settings(c)?;
            let tables: Vec<&str> = TABLES.iter().filter(|t| t.2 != Policy::Hub).map(|t| t.0).collect();
            let ph = tables.iter().map(|t| format!("'{t}'")).collect::<Vec<_>>().join(",");
            Ok(c.query_row(&format!("SELECT COUNT(*) FROM sync_outbox WHERE seq > ?1 AND table_name IN ({ph})"), [ss.push_cursor], |r| r.get(0))?)
        })
    }

    pub fn terminal_heartbeat(&self) -> AppResult<Heartbeat> {
        let d = self.require_device()?;
        let ss = self.terminal_sync_settings()?;
        Ok(Heartbeat {
            device_id: d.device_id,
            app_version: audit::APP_VERSION.into(),
            schema_version: crate::db::latest_schema_version(),
            pending_count: self.terminal_pending_count()?,
            current_user_id: self.sessions.active_users().first().map(|u| u.0.clone()),
            last_error: ss.last_error,
        })
    }

    /// Clear a terminal's blocked state after an operator has checked the hub.
    pub fn sync_unblock(&self, token: &str, accept_new_hub: bool) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("sync.manage")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let mut ss = sync_settings(tx)?;
            ss.blocked_reason = None;
            if accept_new_hub {
                // Keep all local records; re-read everything from the hub.
                ss.hub_instance_id = String::new();
                ss.pull_cursor = 0;
            }
            settings::put(tx, KEY_SYNC, &ss, Some(&s.user_id))?;
            audit::record(tx, &actor, "sync.unblocked", "device", None, None, Some(&json!({ "accept_new_hub": accept_new_hub })))?;
            Ok(())
        })?;
        self.sync_status(token)
    }

    // ---------------- status & dead letters ----------------

    pub fn sync_status(&self, token: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        let d = self.device();
        let mode = d.as_ref().map(|d| d.mode.clone()).unwrap_or_else(|| "standalone".into());
        let ss = self.terminal_sync_settings()?;
        let pending = if mode == "terminal" { self.terminal_pending_count()? } else { 0 };
        let (dead, devices): (i64, Vec<Value>) = self.db.read(|c| {
            let dead: i64 = c.query_row("SELECT COUNT(*) FROM sync_dead_letters WHERE status='open'", [], |r| r.get(0))?;
            let max_seq: i64 = c.query_row("SELECT COALESCE(MAX(seq),0) FROM sync_outbox", [], |r| r.get(0))?;
            let mut st = c.prepare(
                "SELECT d.device_id, d.name, d.device_code, d.operating_mode, d.active, h.last_seen_at, h.pending_count, h.last_push_at, h.last_pull_at,
                        h.app_version, h.schema_version, h.last_error
                 FROM devices d LEFT JOIN device_heartbeats h ON h.device_id=d.device_id ORDER BY d.device_code",
            )?;
            let now = time::now();
            let rows = st
                .query_map([], |r| {
                    let seen: Option<String> = r.get(5)?;
                    let schema: Option<i64> = r.get(10)?;
                    let active = r.get::<_, i64>(4)? == 1;
                    let mode: String = r.get(3)?;
                    let age = seen.as_ref().and_then(|s| time::parse(s).ok()).map(|t| (now - t).num_seconds());
                    let status = if !active {
                        "revoked"
                    } else if mode != "terminal" {
                        "hub"
                    } else if schema.map(|s| s != crate::db::latest_schema_version()).unwrap_or(false) {
                        "version_mismatch"
                    } else if r.get::<_, Option<String>>(11)?.is_some() {
                        "error"
                    } else {
                        match age {
                            None => "never_seen",
                            Some(a) if a > 300 => "offline",
                            _ if r.get::<_, Option<i64>>(6)?.unwrap_or(0) > 0 => "behind",
                            _ => "healthy",
                        }
                    };
                    Ok(json!({ "device_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "code": r.get::<_, String>(2)?, "mode": mode,
                        "active": active, "last_seen_at": seen, "pending": r.get::<_, Option<i64>>(6)?, "last_push_at": r.get::<_, Option<String>>(7)?,
                        "last_pull_at": r.get::<_, Option<String>>(8)?, "app_version": r.get::<_, Option<String>>(9)?, "schema_version": schema,
                        "last_error": r.get::<_, Option<String>>(11)?, "status": status }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let _ = max_seq;
            Ok((dead, rows))
        })?;
        let _ = s;
        Ok(json!({
            "mode": mode, "device": d, "hub_url": ss.hub_url, "port": if ss.port == 0 { DEFAULT_PORT } else { ss.port },
            "pending": pending, "dead_letters": dead, "last_push_at": ss.last_push_at, "last_pull_at": ss.last_pull_at,
            "last_success_at": ss.last_success_at, "last_error": ss.last_error, "last_error_at": ss.last_error_at,
            "blocked_reason": ss.blocked_reason, "hub_version": ss.hub_version, "app_version": audit::APP_VERSION,
            "schema_version": crate::db::latest_schema_version(),
            "devices": if mode == "hub" { devices } else { vec![] },
        }))
    }

    pub fn sync_dead_letters(&self, token: &str) -> AppResult<Vec<Value>> {
        let s = self.session(token)?;
        s.require("sync.manage")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT l.dead_id, l.direction, d.name, l.table_name, l.row_pk, l.op, l.error, l.attempts, l.created_at, l.last_attempt_at
                 FROM sync_dead_letters l LEFT JOIN devices d ON d.device_id=l.origin WHERE l.status='open' ORDER BY l.created_at DESC LIMIT 500",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(json!({ "dead_id": r.get::<_, String>(0)?, "direction": r.get::<_, String>(1)?, "origin": r.get::<_, Option<String>>(2)?,
                        "table": r.get::<_, String>(3)?, "pk": r.get::<_, String>(4)?, "op": r.get::<_, String>(5)?, "error": r.get::<_, String>(6)?,
                        "attempts": r.get::<_, i64>(7)?, "created_at": r.get::<_, String>(8)?, "last_attempt_at": r.get::<_, String>(9)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Retry a dead letter locally. For 'push' letters on a terminal the row is
    /// re-queued to the outbox so the next sync sends it again.
    pub fn sync_retry_dead_letter(&self, token: &str, dead_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("sync.manage")?;
        let id = validate::id(dead_id, "Dead letter")?;
        let actor = self.actor(&s, None);
        let is_hub = self.device().map(|d| d.mode == "hub").unwrap_or(false);
        self.db.write(|tx| {
            let (direction, origin, payload): (String, Option<String>, String) = tx
                .query_row("SELECT direction, origin, payload_json FROM sync_dead_letters WHERE dead_id=?1 AND status='open'", [&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?
                .ok_or_else(|| AppError::not_found("Dead letter"))?;
            let ch: Change = serde_json::from_str(&payload)?;
            let outcome = match direction.as_str() {
                "push" => {
                    tx.execute(
                        "INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES (?1,?2,?3,NULL)",
                        params![ch.table, serde_json::to_string(&ch.pk)?, ch.op],
                    )?;
                    "requeued"
                }
                _ => {
                    let side = if is_hub { ApplySide::Hub(origin.as_deref().unwrap_or("")) } else { ApplySide::Terminal };
                    set_control(tx, !is_hub, origin.as_deref())?;
                    let r = apply_change(tx, &ch, &side);
                    set_control(tx, false, None)?;
                    r?;
                    "applied"
                }
            };
            tx.execute("UPDATE sync_dead_letters SET status='resolved', last_attempt_at=?2 WHERE dead_id=?1", params![id, time::now_str()])?;
            audit::record(tx, &actor, "sync.dead_letter_retried", "sync", Some(&id), None, Some(&json!({ "outcome": outcome })))?;
            Ok(json!({ "outcome": outcome }))
        })
    }

    pub(crate) fn sync_diagnostic(&self) -> AppResult<DiagnosticItem> {
        let mode = self.device().map(|d| d.mode).unwrap_or_else(|| "standalone".into());
        let ss = self.terminal_sync_settings()?;
        let dead: i64 = self.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM sync_dead_letters WHERE status='open'", [], |r| r.get(0))?))?;
        let pending = if mode == "terminal" { self.terminal_pending_count()? } else { 0 };
        let state = if ss.blocked_reason.is_some() || dead > 0 {
            "error"
        } else if mode == "terminal" && ss.last_error.is_some() {
            "warning"
        } else if mode == "standalone" {
            "info"
        } else {
            "ok"
        };
        Ok(DiagnosticItem {
            component: "Sync".into(),
            state: state.into(),
            summary: match mode.as_str() {
                "standalone" => "Standalone (no hub)".into(),
                "hub" => format!("Hub · {dead} unresolved change(s)"),
                _ => format!("Terminal · {pending} pending · last sync {}", ss.last_success_at.clone().unwrap_or_else(|| "never".into())),
            },
            details: json!({ "mode": mode, "hub_url": ss.hub_url, "pending": pending, "dead_letters": dead, "last_error": ss.last_error,
                "blocked_reason": ss.blocked_reason, "push_cursor": ss.push_cursor, "pull_cursor": ss.pull_cursor }),
        })
    }
}

