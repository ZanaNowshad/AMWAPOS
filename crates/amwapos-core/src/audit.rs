//! Tamper-evident audit trail.
//!
//! Each entry stores `previous_hash` and `audit_hash = SHA-256(previous_hash ||
//! canonical entry fields)`. Editing or deleting any historical entry breaks the
//! chain from that point, which `verify_chain` detects. Triggers additionally
//! reject UPDATE/DELETE on the table.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::AppResult;
use crate::ids::new_id;
use crate::time;

pub const GENESIS: &str = "GENESIS";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub user_id: Option<String>,
    pub device_id: Option<String>,
    pub branch_id: Option<String>,
    pub approved_by: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn compute_hash(
    previous: &str,
    audit_id: &str,
    event_type: &str,
    entity_type: &str,
    entity_id: Option<&str>,
    user_id: Option<&str>,
    device_id: Option<&str>,
    approved_by: Option<&str>,
    before: Option<&str>,
    after: Option<&str>,
    created_at: &str,
) -> String {
    let mut h = Sha256::new();
    for part in [
        Some(previous),
        Some(audit_id),
        Some(event_type),
        Some(entity_type),
        entity_id,
        user_id,
        device_id,
        approved_by,
        before,
        after,
        Some(created_at),
    ] {
        let p = part.unwrap_or("\u{0}");
        h.update((p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
    }
    hex::encode(h.finalize())
}

/// Append an audit entry inside the caller's transaction.
pub fn record(
    conn: &Connection,
    actor: &Actor,
    event_type: &str,
    entity_type: &str,
    entity_id: Option<&str>,
    before: Option<&Value>,
    after: Option<&Value>,
) -> AppResult<String> {
    let previous: String = conn
        .query_row("SELECT audit_hash FROM audit_logs ORDER BY seq DESC LIMIT 1", [], |r| r.get(0))
        .optional()?
        .unwrap_or_else(|| GENESIS.to_string());
    let audit_id = new_id();
    let created_at = time::now_str();
    let before_s = before.map(|v| v.to_string());
    let after_s = after.map(|v| v.to_string());
    let hash = compute_hash(
        &previous,
        &audit_id,
        event_type,
        entity_type,
        entity_id,
        actor.user_id.as_deref(),
        actor.device_id.as_deref(),
        actor.approved_by.as_deref(),
        before_s.as_deref(),
        after_s.as_deref(),
        &created_at,
    );
    let schema: i64 = crate::db::latest_schema_version();
    conn.execute(
        "INSERT INTO audit_logs(audit_id, branch_id, device_id, user_id, approved_by, event_type, entity_type, entity_id,
            before_json, after_json, previous_hash, audit_hash, app_version, schema_version, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![
            audit_id,
            actor.branch_id,
            actor.device_id,
            actor.user_id,
            actor.approved_by,
            event_type,
            entity_type,
            entity_id,
            before_s,
            after_s,
            previous,
            hash,
            APP_VERSION,
            schema,
            created_at
        ],
    )?;
    Ok(audit_id)
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainReport {
    pub entries: i64,
    pub valid: bool,
    pub first_broken_seq: Option<i64>,
    pub message: String,
}

pub fn verify_chain(conn: &Connection) -> AppResult<ChainReport> {
    let mut stmt = conn.prepare(
        "SELECT seq, audit_id, event_type, entity_type, entity_id, user_id, device_id, approved_by,
                before_json, after_json, previous_hash, audit_hash, created_at
         FROM audit_logs ORDER BY seq",
    )?;
    let mut rows = stmt.query([])?;
    let mut prev = GENESIS.to_string();
    let mut n = 0i64;
    while let Some(r) = rows.next()? {
        n += 1;
        let seq: i64 = r.get(0)?;
        let previous_hash: String = r.get(10)?;
        let stored: String = r.get(11)?;
        let computed = compute_hash(
            &previous_hash,
            &r.get::<_, String>(1)?,
            &r.get::<_, String>(2)?,
            &r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?.as_deref(),
            r.get::<_, Option<String>>(5)?.as_deref(),
            r.get::<_, Option<String>>(6)?.as_deref(),
            r.get::<_, Option<String>>(7)?.as_deref(),
            r.get::<_, Option<String>>(8)?.as_deref(),
            r.get::<_, Option<String>>(9)?.as_deref(),
            &r.get::<_, String>(12)?,
        );
        if previous_hash != prev || computed != stored {
            return Ok(ChainReport {
                entries: n,
                valid: false,
                first_broken_seq: Some(seq),
                message: format!("The audit chain is broken at entry {seq}."),
            });
        }
        prev = stored;
    }
    Ok(ChainReport { entries: n, valid: true, first_broken_seq: None, message: format!("All {n} audit entries verified.") })
}
