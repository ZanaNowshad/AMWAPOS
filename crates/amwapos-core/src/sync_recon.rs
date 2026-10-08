//! The Sync Reconciliation Centre (Wave 7, docs/OPERATIONAL_CONTROL.md).
//!
//! `sync_dead_letters` is the one record of changes that could not be
//! saved. A person with `sync.manage` on the hub can:
//! * see them, paged and grouped by computer and reason, in plain words;
//! * try one again, or every eligible one in a selection, through the normal
//!   apply path (the same validation as a live send; idempotent: a finished
//!   record is never applied twice);
//! * close one without applying it, with a reason. It is kept, never
//!   deleted, and the record itself is never edited by hand.
//!
//! Terminals show their own list read-only: they never act on it (their
//! records settle when the hub saves or closes its copy, or on a later
//! successful send). The assistant can read the list; it cannot act.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::idempotency::{self, Check};
use crate::ops;
use crate::service::AppCore;
use crate::sync::{self, ApplySide, Change};
use crate::time;
use crate::validate;

/// At most this many records are tried again in one bulk request.
pub const BULK_LIMIT: i64 = 500;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeadQuery {
    /// open (default) | closed (resolved or closed) | all
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub reason_code: Option<String>,
    #[serde(default)]
    pub table: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BulkRetry {
    /// Specific records; otherwise every open record matching the filter.
    #[serde(default)]
    pub dead_ids: Vec<String>,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub reason_code: Option<String>,
    #[serde(default)]
    pub table: Option<String>,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloseRequest {
    pub dead_id: String,
    pub note: String,
    /// Required for a sale, refund, payment, cash or stock record: closing
    /// it means the hub's totals will not include it.
    #[serde(default)]
    pub confirm_financial: bool,
    pub operation_id: String,
}

/// Safe identifying fields shown for a record (never the record itself).
const REF_FIELDS: &[&str] = &["receipt_number", "shift_number", "handover_number", "order_number", "transfer_number", "sku"];

fn filters(q_origin: &Option<String>, q_reason: &Option<String>, q_table: &Option<String>) -> (String, Vec<String>) {
    let mut w = String::new();
    let mut args = vec![];
    for (col, v) in [("l.origin", q_origin), ("l.reason_code", q_reason), ("l.table_name", q_table)] {
        if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
            args.push(v.to_string());
            w.push_str(&format!(" AND {col}=?{}", args.len()));
        }
    }
    (w, args)
}

fn record_ref(table: &str, payload: Option<&str>) -> Option<String> {
    if table == "devices" || table == "users" {
        return None;
    }
    let v: Value = serde_json::from_str(payload?).ok()?;
    let row = v.get("row")?.as_object()?;
    REF_FIELDS.iter().find_map(|f| row.get(*f).and_then(|x| x.as_str().map(String::from).or_else(|| x.as_i64().map(|n| n.to_string()))))
}

/// direction, origin, table, pk, status, payload, retryable, reason.
type LetterRow = (String, Option<String>, String, String, String, Option<String>, i64, String);

/// The outcome of trying one record again.
fn retry_one(tx: &Connection, id: &str, user_id: &str) -> AppResult<(&'static str, Value)> {
    let row: Option<LetterRow> = tx
        .query_row(
            "SELECT direction, origin, table_name, row_pk, status, payload_json, retryable, reason_code FROM sync_dead_letters WHERE dead_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
        )
        .optional()?;
    let Some((direction, origin, table, _pk, status, payload, retryable, reason)) = row else {
        return Err(AppError::not_found("Sync problem"));
    };
    if status != "open" {
        return Ok(("already_finished", json!({ "status": status })));
    }
    if retryable == 0 {
        return Ok(("not_retryable", json!({ "reason_code": reason })));
    }
    if direction != "apply" {
        return Ok(("not_here", json!({ "direction": direction })));
    }
    let Some(ch) = payload.as_deref().and_then(|p| serde_json::from_str::<Change>(p).ok()) else {
        tx.execute("UPDATE sync_dead_letters SET reason_code='invalid_record', retryable=0 WHERE dead_id=?1", [id])?;
        return Ok(("not_retryable", json!({ "reason_code": "invalid_record" })));
    };
    let now = time::now_str();
    if superseded(tx, &ch)? {
        tx.execute(
            "UPDATE sync_dead_letters SET reason_code='superseded', retryable=0, attempts=attempts+1, last_attempt_at=?2 WHERE dead_id=?1",
            params![id, now],
        )?;
        return Ok(("superseded", json!({ "table": table })));
    }
    let origin = origin.unwrap_or_default();
    tx.execute_batch("SAVEPOINT retry")?;
    sync::set_control(tx, false, Some(&origin))?;
    let r = sync::apply_change(tx, &ch, &ApplySide::Hub(&origin));
    sync::set_control(tx, false, None)?;
    match r {
        Ok(_) => {
            tx.execute_batch("RELEASE retry")?;
            tx.execute(
                "UPDATE sync_dead_letters SET status='resolved', resolution='applied', resolved_by=?2, resolved_at=?3,
                    attempts=attempts+1, last_attempt_at=?3 WHERE dead_id=?1",
                params![id, user_id, now],
            )?;
            Ok(("applied", json!({ "table": table })))
        }
        Err(e) => {
            tx.execute_batch("ROLLBACK TO retry; RELEASE retry")?;
            let (reason, retryable) = sync::classify(&e);
            tx.execute(
                "UPDATE sync_dead_letters SET attempts=attempts+1, last_attempt_at=?2, error=?3, reason_code=?4, retryable=?5 WHERE dead_id=?1",
                params![id, now, e.message, reason, retryable],
            )?;
            Ok(("failed", json!({ "reason_code": reason, "can_retry": retryable })))
        }
    }
}

/// A newer version of a shared record was saved since this one was refused:
/// applying the old one would overwrite it.
fn superseded(c: &Connection, ch: &Change) -> AppResult<bool> {
    let Some((pks, sync::Policy::Shared)) = sync::policy(&ch.table) else { return Ok(false) };
    let Some(theirs) = ch.row.as_ref().and_then(|r| r.get("updated_at")).and_then(|v| v.as_str()) else { return Ok(false) };
    if !sync::columns(c, &ch.table)?.iter().any(|k| k == "updated_at") {
        return Ok(false);
    }
    let mut w = vec![];
    let mut args = vec![];
    for (i, k) in pks.iter().enumerate() {
        w.push(format!("\"{k}\" = ?{}", i + 1));
        args.push(ch.pk.get(*k).and_then(|v| v.as_str()).unwrap_or("").to_string());
    }
    let ours: Option<String> = c
        .query_row(
            &format!("SELECT updated_at FROM {} WHERE {}", ch.table, w.join(" AND ")),
            rusqlite::params_from_iter(args.iter()),
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(ours.map(|o| o.as_str() > theirs).unwrap_or(false))
}

impl AppCore {
    /// Problems are handled on the hub (or a standalone computer): a terminal
    /// shows its own list but never acts on it.
    fn require_reconciliation_here(&self) -> AppResult<()> {
        if self.device().map(|d| d.mode == "terminal").unwrap_or(false) {
            return Err(AppError::new(ErrorCode::Forbidden, "Sync problems are handled on the hub."));
        }
        Ok(())
    }

    /// The records that could not be saved, paged, with counts per computer
    /// and reason. The record itself is never returned.
    pub fn sync_dead_letters(&self, token: &str, q: DeadQuery) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("sync.manage")?;
        let limit = q.limit.unwrap_or(50).clamp(1, 200);
        let offset = q.offset.unwrap_or(0).max(0);
        let status_w = match q.status.as_deref().unwrap_or("open") {
            "open" => "l.status='open'",
            "closed" => "l.status<>'open'",
            "all" => "1=1",
            _ => return Err(AppError::validation("Unknown status filter.")),
        };
        let (w, args) = filters(&q.origin, &q.reason_code, &q.table);
        let is_terminal = self.device().map(|d| d.mode == "terminal").unwrap_or(false);
        self.db.read(|c| {
            let total: i64 = c.query_row(
                &format!("SELECT COUNT(*) FROM sync_dead_letters l WHERE {status_w}{w}"),
                rusqlite::params_from_iter(args.iter()),
                |r| r.get(0),
            )?;
            let mut st = c.prepare(&format!(
                "SELECT l.dead_id, l.direction, l.origin, d.name, l.table_name, l.op, l.reason_code, l.retryable, l.error, l.attempts,
                        l.status, l.resolution, l.resolution_note, u.display_name, l.resolved_at, l.created_at, l.last_attempt_at, l.payload_json
                 FROM sync_dead_letters l LEFT JOIN devices d ON d.device_id=l.origin LEFT JOIN users u ON u.user_id=l.resolved_by
                 WHERE {status_w}{w} ORDER BY l.created_at DESC, l.dead_id LIMIT {limit} OFFSET {offset}"
            ))?;
            let rows = st
                .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                    let table: String = r.get(4)?;
                    let payload: Option<String> = r.get(17)?;
                    Ok(json!({
                        "dead_id": r.get::<_, String>(0)?, "direction": r.get::<_, String>(1)?,
                        "origin_id": r.get::<_, Option<String>>(2)?, "origin": r.get::<_, Option<String>>(3)?,
                        "table": table, "op": r.get::<_, String>(5)?, "record_ref": record_ref(&table, payload.as_deref()),
                        "reason_code": r.get::<_, String>(6)?, "can_retry": r.get::<_, i64>(7)? == 1,
                        "error": r.get::<_, String>(8)?, "attempts": r.get::<_, i64>(9)?,
                        "status": r.get::<_, String>(10)?, "resolution": r.get::<_, Option<String>>(11)?,
                        "resolution_note": r.get::<_, Option<String>>(12)?, "resolved_by": r.get::<_, Option<String>>(13)?,
                        "resolved_at": r.get::<_, Option<String>>(14)?, "created_at": r.get::<_, String>(15)?,
                        "last_attempt_at": r.get::<_, String>(16)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut gs = c.prepare(
                "SELECT l.origin, d.name, l.reason_code, COUNT(*), SUM(l.retryable), MIN(l.created_at)
                 FROM sync_dead_letters l LEFT JOIN devices d ON d.device_id=l.origin
                 WHERE l.status='open' GROUP BY l.origin, l.reason_code ORDER BY COUNT(*) DESC LIMIT 100",
            )?;
            let groups = gs
                .query_map([], |r| {
                    Ok(json!({ "origin_id": r.get::<_, Option<String>>(0)?, "origin": r.get::<_, Option<String>>(1)?,
                        "reason_code": r.get::<_, String>(2)?, "count": r.get::<_, i64>(3)?, "retryable": r.get::<_, i64>(4)?,
                        "oldest": r.get::<_, String>(5)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({ "rows": rows, "total": total, "groups": groups, "can_act": !is_terminal && s.has("sync.manage") }))
        })
    }

    /// Try one record again through the normal apply path. A finished record
    /// is reported, not applied again.
    pub fn sync_retry_dead_letter(&self, token: &str, dead_id: &str, operation_id: Option<&str>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("sync.manage")?;
        self.require_reconciliation_here()?;
        let id = validate::id(dead_id, "Sync problem")?;
        let actor = self.actor(&s, None);
        let req = json!({ "dead_id": id });
        if let Some(op) = operation_id {
            idempotency::validate_operation_id(op)?;
            if let Check::Replay { result } = self.db.read(|c| idempotency::check(c, op, "sync.retry", &req))? {
                return Ok(result);
            }
        }
        self.db.write(|tx| {
            let hash = match operation_id {
                Some(op) => match idempotency::check(tx, op, "sync.retry", &req)? {
                    Check::Replay { result } => return Ok(result),
                    Check::New { payload_hash } => Some(payload_hash),
                },
                None => None,
            };
            let (origin, reason): (Option<String>, String) = tx
                .query_row("SELECT origin, reason_code FROM sync_dead_letters WHERE dead_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| AppError::not_found("Sync problem"))?;
            let (outcome, detail) = retry_one(tx, &id, &s.user_id)?;
            if outcome != "already_finished" {
                audit::record(
                    tx,
                    &actor,
                    "sync.dead_letter_retried",
                    "sync",
                    Some(&id),
                    None,
                    Some(&json!({ "outcome": outcome, "detail": detail })),
                )?;
            }
            if outcome == "applied" {
                ops::note_on_open_case(
                    tx,
                    &ops::sync_failure_key(origin.as_deref(), &reason),
                    &s.user_id,
                    "A record was tried again and saved.",
                    &json!({ "dead_id": id, "outcome": outcome }),
                )?;
            }
            let out = json!({ "dead_id": id, "outcome": outcome, "detail": detail });
            if let (Some(op), Some(h)) = (operation_id, hash) {
                idempotency::complete(tx, op, "sync.retry", Some(&s.user_id), Some(&s.device_id), &h, Some(&id), &out)?;
            }
            Ok(out)
        })
    }

    /// Try again every eligible record in a selection (open and retryable),
    /// at most [`BULK_LIMIT`] per request. Ineligible records are counted,
    /// never applied.
    pub fn sync_retry_dead_letters(&self, token: &str, req: BulkRetry) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("sync.manage")?;
        self.require_reconciliation_here()?;
        idempotency::validate_operation_id(&req.operation_id)?;
        if req.dead_ids.len() as i64 > BULK_LIMIT {
            return Err(AppError::validation("Select at most 500 records at a time."));
        }
        if let Check::Replay { result } = self.db.read(|c| idempotency::check(c, &req.operation_id, "sync.retry_bulk", &req))? {
            return Ok(result);
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "sync.retry_bulk", &req)? {
                Check::Replay { result } => return Ok(result),
                Check::New { payload_hash } => payload_hash,
            };
            // The selection, then who in it is eligible.
            let (w, args) = filters(&req.origin, &req.reason_code, &req.table);
            let selected: Vec<(String, Option<String>, String, String, i64)> = if req.dead_ids.is_empty() {
                let mut st = tx.prepare(&format!(
                    "SELECT l.dead_id, l.origin, l.reason_code, l.direction, l.retryable FROM sync_dead_letters l
                     WHERE l.status='open'{w} ORDER BY l.created_at"
                ))?;
                let r = st
                    .query_map(rusqlite::params_from_iter(args.iter()), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                r
            } else {
                let mut st = tx.prepare(
                    "SELECT dead_id, origin, reason_code, direction, CASE WHEN status='open' THEN retryable ELSE 0 END FROM sync_dead_letters WHERE dead_id=?1",
                )?;
                let mut out = vec![];
                for id in &req.dead_ids {
                    if let Some(r) = st.query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).optional()? {
                        out.push(r);
                    }
                }
                out
            };
            let (eligible, ineligible): (Vec<_>, Vec<_>) = selected.into_iter().partition(|r| r.4 == 1 && r.3 == "apply");
            let take = eligible.len().min(BULK_LIMIT as usize);
            let mut applied = 0;
            let mut failed = 0;
            let mut superseded = 0;
            let mut ids = vec![];
            let mut saved_per_key: std::collections::BTreeMap<String, i64> = Default::default();
            for (id, origin, reason, _, _) in eligible.iter().take(take) {
                let (outcome, _) = retry_one(tx, id, &s.user_id)?;
                match outcome {
                    "applied" => {
                        applied += 1;
                        *saved_per_key.entry(ops::sync_failure_key(origin.as_deref(), reason)).or_default() += 1;
                    }
                    "superseded" => superseded += 1,
                    _ => failed += 1,
                }
                ids.push(id.clone());
            }
            for (key, n) in &saved_per_key {
                ops::note_on_open_case(
                    tx,
                    key,
                    &s.user_id,
                    &format!("{n} records were tried again and saved."),
                    &json!({ "count": n, "operation_id": req.operation_id }),
                )?;
            }
            let out = json!({
                "eligible": eligible.len(), "ineligible": ineligible.len(), "tried": take,
                "remaining": eligible.len() - take, "applied": applied, "failed": failed, "superseded": superseded,
            });
            audit::record(
                tx,
                &actor,
                "sync.dead_letters_bulk_retried",
                "sync",
                None,
                None,
                Some(&json!({ "result": out, "filter": { "origin": req.origin, "reason_code": req.reason_code, "table": req.table }, "dead_ids": ids })),
            )?;
            idempotency::complete(tx, &req.operation_id, "sync.retry_bulk", Some(&s.user_id), Some(&s.device_id), &hash, None, &out)?;
            Ok(out)
        })
    }

    /// Close a record without applying it: kept with who, when and why. The
    /// hub's records will not include it; the till that sent it is told.
    pub fn sync_close_dead_letter(&self, token: &str, req: CloseRequest) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("sync.manage")?;
        self.require_reconciliation_here()?;
        idempotency::validate_operation_id(&req.operation_id)?;
        let id = validate::id(&req.dead_id, "Sync problem")?;
        let note = req.note.trim().to_string();
        if note.chars().count() < 5 {
            return Err(AppError::validation("Say why this record is closed without applying it (at least 5 characters)."));
        }
        if note.chars().count() > 500 {
            return Err(AppError::validation("Keep the reason under 500 characters."));
        }
        if let Check::Replay { result } = self.db.read(|c| idempotency::check(c, &req.operation_id, "sync.close", &req))? {
            return Ok(result);
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "sync.close", &req)? {
                Check::Replay { result } => return Ok(result),
                Check::New { payload_hash } => payload_hash,
            };
            let (status, table, origin, reason): (String, String, Option<String>, String) = tx
                .query_row("SELECT status, table_name, origin, reason_code FROM sync_dead_letters WHERE dead_id=?1", [&id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Sync problem"))?;
            if status != "open" {
                return Err(AppError::conflict("This sync problem is already finished."));
            }
            if ops::FINANCIAL_TABLES.contains(&table.as_str()) && !req.confirm_financial {
                return Err(AppError::new(
                    ErrorCode::ApprovalRequired,
                    "This is a money or stock record. Closing it means the hub's totals will not include it. Confirm to continue.",
                ));
            }
            let now = time::now_str();
            tx.execute(
                "UPDATE sync_dead_letters SET status='closed', resolution='closed_without_applying', resolution_note=?2, resolved_by=?3, resolved_at=?4
                 WHERE dead_id=?1",
                params![id, note, s.user_id, now],
            )?;
            audit::record(
                tx,
                &actor,
                "sync.dead_letter_closed",
                "sync",
                Some(&id),
                None,
                Some(&json!({ "table": table, "origin": origin, "reason_code": reason, "note": note })),
            )?;
            ops::note_on_open_case(
                tx,
                &ops::sync_failure_key(origin.as_deref(), &reason),
                &s.user_id,
                &format!("A record was closed without applying it: {note}"),
                &json!({ "dead_id": id, "table": table }),
            )?;
            let out = json!({ "dead_id": id, "status": "closed" });
            idempotency::complete(tx, &req.operation_id, "sync.close", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &out)?;
            Ok(out)
        })
    }
}
