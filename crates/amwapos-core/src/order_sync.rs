//! Order event feed → other systems (docs/WHATSAPP_AUTOPILOT.md, "Sync").
//!
//! `order_events` (written by triggers, migration 0039) is the single record
//! of every order and delivery state change. A *sync target* reads it in
//! order with its own cursor and hands each event to an adapter in the hub
//! (`amwapos-hub/src/order_sync.rs`): a signed webhook or a CSV file. New
//! kinds of system are new adapters; nothing here changes.
//!
//! Delivery rules, the same for every adapter:
//! * in order, one event at a time per target; at-least-once (a receiver
//!   de-duplicates on `event_id`), each event carries a per-order `version`
//!   so an older state never overwrites a newer one at the receiver;
//! * a temporary failure retries the same event with growing waits; after
//!   `MAX_ATTEMPTS` (or at once for a refusal) the event is parked and the
//!   target moves on, so one bad event never stops the rest;
//! * parked events stay visible and are sent again on request;
//! * AMWAPOS is the source of truth: nothing a target says changes an order.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::Session;
use crate::error::{AppError, AppResult};
use crate::ids::new_id;
use crate::service::AppCore;
use crate::time;
use crate::{audit, validate};

/// Attempts on one event before it is parked.
pub const MAX_ATTEMPTS: i64 = 8;
/// Waits between attempts (seconds), the last one repeated.
const BACKOFF_S: &[i64] = &[10, 30, 120, 300, 900, 1800, 3600];
/// Delivered events older than this are removed once every consumer is past them.
const KEEP_DAYS: i64 = 30;

pub fn secret_slot(sink_id: &str) -> String {
    format!("order_sync.{sink_id}.secret")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct SinkInput {
    pub name: String,
    /// webhook | csv
    pub kind: String,
    /// webhook: the receiving address (http or https).
    pub url: String,
    /// csv: the file to append to.
    pub path: String,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SinkRow {
    pub sink_id: String,
    pub name: String,
    pub kind: String,
    pub url: Option<String>,
    pub path: Option<String>,
    pub enabled: bool,
    pub cursor_seq: i64,
    /// Events not yet handed to this target.
    pub behind: i64,
    pub attempts: i64,
    pub next_at: Option<String>,
    pub last_error: Option<String>,
    pub last_ok_at: Option<String>,
    pub delivered: i64,
    pub parked: i64,
    pub created_at: String,
}

/// One event for one target, as the hub worker sends it.
#[derive(Debug, Clone)]
pub struct SyncJob {
    pub sink_id: String,
    pub kind: String,
    pub config: Value,
    pub event_seq: i64,
    pub event_id: String,
    /// Sent again from the parked list (not from the cursor).
    pub from_parked: bool,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SyncOutcome {
    Delivered,
    /// Try the same event again later (network, 5xx, 429, a locked file).
    Retry {
        error: String,
        after_s: Option<i64>,
    },
    /// The receiver refused this event (4xx other than 408/409/425/429):
    /// it is parked at once.
    Refused {
        error: String,
    },
}

fn clean_config(kind: &str, i: &SinkInput) -> AppResult<Value> {
    match kind {
        "webhook" => {
            let u = i.url.trim();
            if !(u.starts_with("http://") || u.starts_with("https://")) || u.len() > 500 || u.contains(char::is_whitespace) {
                return Err(AppError::validation("Enter the receiving address, starting with https:// (or http:// on the store network)."));
            }
            Ok(json!({ "url": u }))
        }
        "csv" => {
            let p = i.path.trim();
            if p.is_empty() || p.len() > 400 || !p.to_ascii_lowercase().ends_with(".csv") {
                return Err(AppError::validation("Enter the full path of a .csv file, for example on a shared folder."));
            }
            Ok(json!({ "path": p }))
        }
        _ => Err(AppError::validation("A sync target is a webhook or a CSV file.")),
    }
}

fn sink_row(c: &Connection, id: &str, head: i64) -> AppResult<SinkRow> {
    c.query_row(
        "SELECT sink_id, name, kind, config_json, enabled, cursor_seq, attempts, next_at, last_error, last_ok_at, delivered, created_at,
                (SELECT COUNT(*) FROM sync_sink_failures f WHERE f.sink_id=s.sink_id AND f.status IN ('parked','retry'))
           FROM sync_sinks s WHERE sink_id=?1",
        [id],
        |r| {
            let cfg: Value = serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(Value::Null);
            let cursor: i64 = r.get(5)?;
            Ok(SinkRow {
                sink_id: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                url: cfg["url"].as_str().map(String::from),
                path: cfg["path"].as_str().map(String::from),
                enabled: r.get::<_, i64>(4)? == 1,
                cursor_seq: cursor,
                behind: (head - cursor).max(0),
                attempts: r.get(6)?,
                next_at: r.get(7)?,
                last_error: r.get(8)?,
                last_ok_at: r.get(9)?,
                delivered: r.get(10)?,
                created_at: r.get(11)?,
                parked: r.get(12)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("Sync target"))
}

fn head(c: &Connection) -> AppResult<i64> {
    Ok(c.query_row("SELECT COALESCE(MAX(seq),0) FROM order_events", [], |r| r.get(0))?)
}

/// The event as receivers get it: what changed (`kind`, `state`, `version`)
/// and the order as it is now, with POS prices and the sale and delivery.
pub fn event_payload(c: &Connection, seq: i64) -> AppResult<Value> {
    let (event_id, source, order_id, delivery_id, kind, state_key, version, at): (
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        i64,
        String,
    ) = c
        .query_row(
            "SELECT event_id, source, order_id, delivery_id, kind, state_key, version, created_at FROM order_events WHERE seq=?1",
            [seq],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("Order event"))?;
    let mut parts = state_key.splitn(2, '|');
    let (s1, s2) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
    let state = if source == "order" {
        json!({ "status": s1, "payment_state": s2 })
    } else {
        json!({ "status": s1, "outcome": (!s2.is_empty()).then_some(s2) })
    };
    let mut out = json!({
        "event_id": event_id, "seq": seq, "kind": kind, "source": source, "version": version, "occurred_at": at, "state": state,
        "order": Value::Null, "delivery": Value::Null,
    });
    if let Some(o) = &order_id {
        let row: Option<Value> = c
            .query_row(
                "SELECT order_id, order_number, branch_id, channel, status, payment_state, customer_id, phone, delivery_wanted, address, area,
                        flat, building, road, block, delivery_fee_minor, sale_id, delivery_id, external_ref, created_at, updated_at
                   FROM digital_orders WHERE order_id=?1",
                [o],
                |r| {
                    Ok(json!({
                        "order_id": r.get::<_, String>(0)?, "order_number": r.get::<_, String>(1)?, "branch_id": r.get::<_, String>(2)?,
                        "channel": r.get::<_, String>(3)?, "status": r.get::<_, String>(4)?, "payment_state": r.get::<_, String>(5)?,
                        "customer_id": r.get::<_, Option<String>>(6)?, "phone": r.get::<_, Option<String>>(7)?,
                        "delivery_wanted": r.get::<_, i64>(8)? == 1, "address": r.get::<_, Option<String>>(9)?, "area": r.get::<_, Option<String>>(10)?,
                        "flat": r.get::<_, Option<String>>(11)?, "building": r.get::<_, Option<String>>(12)?, "road": r.get::<_, Option<String>>(13)?,
                        "block": r.get::<_, Option<String>>(14)?, "delivery_fee_minor": r.get::<_, Option<i64>>(15)?,
                        "sale_id": r.get::<_, Option<String>>(16)?, "delivery_id": r.get::<_, Option<String>>(17)?,
                        "external_ref": r.get::<_, Option<String>>(18)?, "created_at": r.get::<_, String>(19)?, "updated_at": r.get::<_, String>(20)?,
                    }))
                },
            )
            .optional()?;
        if let Some(mut row) = row {
            let branch_id = row["branch_id"].as_str().unwrap_or_default().to_string();
            let (sub, lines, _) = crate::waorders::service::totals(c, o, &branch_id)?;
            let lines: Vec<Value> = lines
                .into_iter()
                .map(|l| {
                    let sku: Option<String> = l["product_id"]
                        .as_str()
                        .and_then(|p| c.query_row("SELECT sku FROM products WHERE product_id=?1", [p], |r| r.get(0)).ok().flatten());
                    json!({ "line_no": l["line_no"], "product_id": l["product_id"], "sku": sku, "name": l["name"], "qty_milli": l["qty_milli"],
                            "unit_price_minor": l["unit_price_minor"], "line_total_minor": l["line_total_minor"] })
                })
                .collect();
            let fee = row["delivery_fee_minor"].as_i64().unwrap_or(0);
            row["lines"] = json!(lines);
            row["subtotal_minor"] = json!(sub);
            row["total_minor"] = json!(sub + fee);
            // Once rung up, the sale is the authoritative amount.
            if let Some(sale) = row["sale_id"].as_str() {
                row["sale"] = c
                    .query_row("SELECT sale_id, receipt_number, total_minor, completed_at FROM sales WHERE sale_id=?1", [sale], |r| {
                        Ok(json!({ "sale_id": r.get::<_, String>(0)?, "receipt_number": r.get::<_, String>(1)?, "total_minor": r.get::<_, i64>(2)?,
                                   "completed_at": r.get::<_, Option<String>>(3)? }))
                    })
                    .optional()?
                    .unwrap_or(Value::Null);
            }
            out["order"] = row;
        }
    }
    let did = delivery_id.or_else(|| out["order"]["delivery_id"].as_str().map(String::from));
    if let Some(d) = did {
        out["delivery"] = c
            .query_row(
                "SELECT delivery_id, delivery_number, status, outcome, COALESCE(pay_state, payment_status), amount_minor, dispatched_at, delivered_at, sale_id
                   FROM delivery_orders WHERE delivery_id=?1",
                [&d],
                |r| {
                    Ok(json!({ "delivery_id": r.get::<_, String>(0)?, "delivery_number": r.get::<_, String>(1)?, "status": r.get::<_, String>(2)?,
                               "outcome": r.get::<_, Option<String>>(3)?, "payment": r.get::<_, Option<String>>(4)?, "amount_minor": r.get::<_, i64>(5)?,
                               "dispatched_at": r.get::<_, Option<String>>(6)?, "delivered_at": r.get::<_, Option<String>>(7)?,
                               "sale_id": r.get::<_, Option<String>>(8)? }))
                },
            )
            .optional()?
            .unwrap_or(Value::Null);
    }
    Ok(out)
}

impl AppCore {
    fn order_sync_admin(&self, token: &str) -> AppResult<Session> {
        let s = self.session(token)?;
        s.require("settings.manage")?;
        self.require_back_office_writable()?;
        Ok(s)
    }

    pub fn order_sync_list(&self, token: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("settings.manage")?;
        self.db.read(|c| {
            let h = head(c)?;
            let ids: Vec<String> = {
                let mut st = c.prepare("SELECT sink_id FROM sync_sinks ORDER BY created_at")?;
                let r = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
                r
            };
            let rows = ids.iter().map(|i| sink_row(c, i, h)).collect::<AppResult<Vec<_>>>()?;
            let last: Option<(i64, String, String)> = c
                .query_row("SELECT seq, kind, created_at FROM order_events ORDER BY seq DESC LIMIT 1", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?;
            Ok(json!({ "targets": rows, "head_seq": h, "last_event": last.map(|(s, k, a)| json!({ "seq": s, "kind": k, "at": a })) }))
        })
    }

    /// A new target starts at the end of the feed (no history). A webhook
    /// gets a signing secret, returned once here and kept in the credential
    /// store.
    pub fn order_sync_create(&self, token: &str, input: SinkInput) -> AppResult<Value> {
        let s = self.order_sync_admin(token)?;
        let name = crate::setup::clean(&input.name, "Name", 80, true)?;
        let config = clean_config(&input.kind, &input)?;
        let id = new_id();
        let secret = (input.kind == "webhook").then(|| {
            let mut b = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
            hex::encode(b)
        });
        if let Some(sec) = &secret {
            self.secrets.set(&secret_slot(&id), sec)?;
        }
        let actor = self.actor(&s, None);
        let t = time::now_str();
        self.db.write(|tx| {
            let h = head(tx)?;
            tx.execute(
                "INSERT INTO sync_sinks(sink_id, name, kind, config_json, enabled, cursor_seq, created_by, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
                params![id, name, input.kind, config.to_string(), input.enabled.unwrap_or(true) as i64, h, s.user_id, t],
            )?;
            audit::record(tx, &actor, "order_sync.created", "sync_sink", Some(&id), None, Some(&json!({ "name": name, "kind": input.kind, "config": config })))?;
            Ok(())
        })?;
        let row = self.db.read(|c| sink_row(c, &id, head(c)?))?;
        Ok(json!({ "target": row, "secret": secret }))
    }

    pub fn order_sync_update(&self, token: &str, sink_id: &str, input: SinkInput) -> AppResult<SinkRow> {
        let s = self.order_sync_admin(token)?;
        let id = validate::id(sink_id, "Sync target")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let kind: String = tx
                .query_row("SELECT kind FROM sync_sinks WHERE sink_id=?1", [&id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Sync target"))?;
            let name = crate::setup::clean(&input.name, "Name", 80, true)?;
            let config = clean_config(&kind, &input)?;
            let enabled = input.enabled.unwrap_or(true);
            // Turning a target back on, or changing where it points, retries at once.
            tx.execute(
                "UPDATE sync_sinks SET name=?2, config_json=?3, enabled=?4, next_at=NULL, attempts=0, updated_at=?5 WHERE sink_id=?1",
                params![id, name, config.to_string(), enabled as i64, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "order_sync.updated",
                "sync_sink",
                Some(&id),
                None,
                Some(&json!({ "name": name, "enabled": enabled, "config": config })),
            )?;
            Ok(())
        })?;
        self.db.read(|c| sink_row(c, &id, head(c)?))
    }

    pub fn order_sync_delete(&self, token: &str, sink_id: &str) -> AppResult<()> {
        let s = self.order_sync_admin(token)?;
        let id = validate::id(sink_id, "Sync target")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            tx.execute("DELETE FROM sync_sink_failures WHERE sink_id=?1", [&id])?;
            if tx.execute("DELETE FROM sync_sinks WHERE sink_id=?1", [&id])? == 0 {
                return Err(AppError::not_found("Sync target"));
            }
            audit::record(tx, &actor, "order_sync.deleted", "sync_sink", Some(&id), None, None)?;
            Ok(())
        })?;
        let _ = self.secrets.delete(&secret_slot(&id));
        Ok(())
    }

    /// Events this target could not take (parked) or that are queued to be sent again.
    pub fn order_sync_failures(&self, token: &str, sink_id: &str) -> AppResult<Vec<Value>> {
        let s = self.session(token)?;
        s.require("settings.manage")?;
        let id = validate::id(sink_id, "Sync target")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT f.event_seq, f.attempts, f.error, f.status, f.updated_at, e.kind, e.created_at,
                        (SELECT order_number FROM digital_orders o WHERE o.order_id=e.order_id)
                   FROM sync_sink_failures f LEFT JOIN order_events e ON e.seq=f.event_seq
                  WHERE f.sink_id=?1 AND f.status IN ('parked','retry') ORDER BY f.event_seq",
            )?;
            let r = st
                .query_map([&id], |r| {
                    Ok(json!({ "event_seq": r.get::<_, i64>(0)?, "attempts": r.get::<_, i64>(1)?, "error": r.get::<_, String>(2)?,
                               "status": r.get::<_, String>(3)?, "updated_at": r.get::<_, String>(4)?, "kind": r.get::<_, Option<String>>(5)?,
                               "occurred_at": r.get::<_, Option<String>>(6)?, "order_number": r.get::<_, Option<String>>(7)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(r)
        })
    }

    /// Send parked events again (one, or all of a target's), or dismiss one.
    pub fn order_sync_failure_action(&self, token: &str, sink_id: &str, event_seq: Option<i64>, action: &str) -> AppResult<usize> {
        let s = self.order_sync_admin(token)?;
        let id = validate::id(sink_id, "Sync target")?;
        let status = match action {
            "retry" => "retry",
            "dismiss" => "dismissed",
            _ => return Err(AppError::validation("Choose retry or dismiss.")),
        };
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let n = tx.execute(
                "UPDATE sync_sink_failures SET status=?3, attempts=CASE WHEN ?3='retry' THEN 0 ELSE attempts END, updated_at=?4
                  WHERE sink_id=?1 AND (?2 IS NULL OR event_seq=?2) AND status IN ('parked','retry')",
                params![id, event_seq, status, time::now_str()],
            )?;
            if status == "retry" {
                tx.execute("UPDATE sync_sinks SET next_at=NULL WHERE sink_id=?1", [&id])?;
            }
            audit::record(
                tx,
                &actor,
                &format!("order_sync.failure_{action}"),
                "sync_sink",
                Some(&id),
                None,
                Some(&json!({ "event_seq": event_seq, "count": n })),
            )?;
            Ok(n)
        })
    }

    // ------------------------------------------------------------ worker side

    /// Enabled targets that have something to send now.
    pub fn order_sync_due(&self) -> AppResult<Vec<String>> {
        let now = time::now_str();
        self.db.read(|c| {
            let h = head(c)?;
            let mut st = c.prepare(
                "SELECT sink_id FROM sync_sinks s WHERE enabled=1 AND (next_at IS NULL OR next_at <= ?1)
                   AND (cursor_seq < ?2 OR EXISTS (SELECT 1 FROM sync_sink_failures f WHERE f.sink_id=s.sink_id AND f.status='retry'))
                 ORDER BY created_at",
            )?;
            let r = st.query_map(params![now, h], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            Ok(r)
        })
    }

    /// The next event for a target: an event queued to be sent again first,
    /// then the one after its cursor. `None` when there is nothing, it is
    /// off, or it is waiting before a retry.
    pub fn order_sync_next(&self, sink_id: &str) -> AppResult<Option<SyncJob>> {
        let now = time::now_str();
        self.db.read(|c| {
            let s: Option<(String, String, i64, i64, Option<String>)> = c
                .query_row("SELECT kind, config_json, enabled, cursor_seq, next_at FROM sync_sinks WHERE sink_id=?1", [sink_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .optional()?;
            let Some((kind, cfg, enabled, cursor, next_at)) = s else { return Ok(None) };
            if enabled == 0 || next_at.as_deref().is_some_and(|n| n > now.as_str()) {
                return Ok(None);
            }
            let parked: Option<i64> = c
                .query_row(
                    "SELECT event_seq FROM sync_sink_failures WHERE sink_id=?1 AND status='retry' ORDER BY event_seq LIMIT 1",
                    [sink_id],
                    |r| r.get(0),
                )
                .optional()?;
            let (seq, from_parked) = match parked {
                Some(p) => (p, true),
                None => match c.query_row("SELECT MIN(seq) FROM order_events WHERE seq > ?1", [cursor], |r| r.get::<_, Option<i64>>(0))? {
                    Some(n) => (n, false),
                    None => return Ok(None),
                },
            };
            let payload = match event_payload(c, seq) {
                Ok(p) => p,
                // The event was pruned: nothing left to send for it.
                Err(e) if e.code == crate::ErrorCode::NotFound => json!({ "missing": true, "seq": seq }),
                Err(e) => return Err(e),
            };
            Ok(Some(SyncJob {
                sink_id: sink_id.to_string(),
                kind,
                config: serde_json::from_str(&cfg).unwrap_or(Value::Null),
                event_seq: seq,
                event_id: payload["event_id"].as_str().unwrap_or_default().to_string(),
                from_parked,
                payload,
            }))
        })
    }

    /// Record what happened to a job. A retried temporary failure waits
    /// longer each time; the `MAX_ATTEMPTS`th (or a refusal) parks the event
    /// and the target moves on to the next one.
    pub fn order_sync_complete(&self, job: &SyncJob, outcome: SyncOutcome) -> AppResult<()> {
        let now = time::now();
        let t = time::fmt(now);
        self.db.write(|tx| {
            let attempts: i64 = if job.from_parked {
                tx.query_row(
                    "SELECT attempts FROM sync_sink_failures WHERE sink_id=?1 AND event_seq=?2",
                    params![job.sink_id, job.event_seq],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0)
            } else {
                tx.query_row("SELECT attempts FROM sync_sinks WHERE sink_id=?1", [&job.sink_id], |r| r.get(0)).optional()?.unwrap_or(0)
            } + 1;
            let park = |tx: &Connection, error: &str| -> AppResult<()> {
                tx.execute(
                    "INSERT INTO sync_sink_failures(sink_id, event_seq, attempts, error, status, created_at, updated_at) VALUES (?1,?2,?3,?4,'parked',?5,?5)
                     ON CONFLICT(sink_id, event_seq) DO UPDATE SET attempts=?3, error=?4, status='parked', updated_at=?5",
                    params![job.sink_id, job.event_seq, attempts, error.chars().take(500).collect::<String>(), t],
                )?;
                Ok(())
            };
            match (&outcome, job.from_parked) {
                (SyncOutcome::Delivered, false) => {
                    tx.execute(
                        "UPDATE sync_sinks SET cursor_seq=MAX(cursor_seq, ?2), attempts=0, next_at=NULL, last_error=NULL, last_ok_at=?3,
                            delivered=delivered+1, updated_at=?3 WHERE sink_id=?1",
                        params![job.sink_id, job.event_seq, t],
                    )?;
                }
                (SyncOutcome::Delivered, true) => {
                    tx.execute(
                        "UPDATE sync_sink_failures SET status='resolved', updated_at=?3 WHERE sink_id=?1 AND event_seq=?2",
                        params![job.sink_id, job.event_seq, t],
                    )?;
                    tx.execute(
                        "UPDATE sync_sinks SET last_ok_at=?2, last_error=NULL, delivered=delivered+1, updated_at=?2 WHERE sink_id=?1",
                        params![job.sink_id, t],
                    )?;
                }
                (SyncOutcome::Retry { error, after_s }, from_parked) if attempts < MAX_ATTEMPTS => {
                    let wait = after_s.unwrap_or(BACKOFF_S[((attempts - 1) as usize).min(BACKOFF_S.len() - 1)]).clamp(1, 24 * 3600);
                    let next = time::fmt(now + chrono::Duration::seconds(wait));
                    if from_parked {
                        tx.execute(
                            "UPDATE sync_sink_failures SET attempts=?3, error=?4, updated_at=?5 WHERE sink_id=?1 AND event_seq=?2",
                            params![job.sink_id, job.event_seq, attempts, error.chars().take(500).collect::<String>(), t],
                        )?;
                    } else {
                        tx.execute("UPDATE sync_sinks SET attempts=?2 WHERE sink_id=?1", params![job.sink_id, attempts])?;
                    }
                    tx.execute(
                        "UPDATE sync_sinks SET next_at=?2, last_error=?3, updated_at=?4 WHERE sink_id=?1",
                        params![job.sink_id, next, error.chars().take(500).collect::<String>(), t],
                    )?;
                }
                (SyncOutcome::Retry { error, .. } | SyncOutcome::Refused { error }, from_parked) => {
                    park(tx, error)?;
                    if !from_parked {
                        tx.execute(
                            "UPDATE sync_sinks SET cursor_seq=MAX(cursor_seq, ?2), attempts=0, next_at=NULL, last_error=?3, updated_at=?4 WHERE sink_id=?1",
                            params![job.sink_id, job.event_seq, error.chars().take(500).collect::<String>(), t],
                        )?;
                    } else {
                        tx.execute(
                            "UPDATE sync_sinks SET last_error=?2, updated_at=?3 WHERE sink_id=?1",
                            params![job.sink_id, error.chars().take(500).collect::<String>(), t],
                        )?;
                    }
                }
            }
            Ok(())
        })
    }

    /// A sample event for "Send a test" (the latest real one, or a made-up
    /// order when the feed is still empty). Never recorded anywhere.
    pub fn order_sync_test_job(&self, token: &str, sink_id: &str) -> AppResult<SyncJob> {
        let _ = self.order_sync_admin(token)?;
        let id = validate::id(sink_id, "Sync target")?;
        self.db.read(|c| {
            let (kind, cfg): (String, String) = c
                .query_row("SELECT kind, config_json FROM sync_sinks WHERE sink_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| AppError::not_found("Sync target"))?;
            let mut payload = match c.query_row("SELECT MAX(seq) FROM order_events", [], |r| r.get::<_, Option<i64>>(0))? {
                Some(seq) => event_payload(c, seq)?,
                None => json!({ "kind": "order.draft", "state": { "status": "draft" }, "order": Value::Null, "delivery": Value::Null }),
            };
            payload["test"] = json!(true);
            payload["event_id"] = json!(format!("test-{}", new_id()));
            Ok(SyncJob {
                sink_id: id.clone(),
                kind,
                config: serde_json::from_str(&cfg).unwrap_or(Value::Null),
                event_seq: 0,
                event_id: payload["event_id"].as_str().unwrap_or_default().to_string(),
                from_parked: false,
                payload,
            })
        })
    }

    /// Remove old events every consumer has passed (and no target still needs).
    pub fn order_events_prune(&self) -> AppResult<usize> {
        let cutoff = time::fmt(time::now() - chrono::Duration::days(KEEP_DAYS));
        self.db.write(|tx| {
            let min_cursor: i64 = tx.query_row(
                "SELECT MIN(c) FROM (SELECT cursor_seq AS c FROM sync_sinks UNION ALL SELECT cursor_seq FROM order_event_cursors
                                     UNION ALL SELECT (SELECT COALESCE(MAX(seq),0) FROM order_events))",
                [],
                |r| r.get(0),
            )?;
            Ok(tx.execute(
                "DELETE FROM order_events WHERE seq <= ?1 AND created_at < ?2
                   AND seq NOT IN (SELECT event_seq FROM sync_sink_failures WHERE status IN ('parked','retry'))",
                params![min_cursor, cutoff],
            )?)
        })
    }
}

/// HMAC-SHA256 (RFC 2104), for webhook signatures: `X-Amwapos-Signature:
/// sha256=<hex of HMAC(secret, body)>`.
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(msg).finalize();
    Sha256::new().chain_update(opad).chain_update(inner).finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hex::encode(hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6 (key longer than the block).
        assert_eq!(
            hex::encode(hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }
}
