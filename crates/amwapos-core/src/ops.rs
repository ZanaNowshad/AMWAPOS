//! Operational control (Wave 7): the Alert Centre's engine.
//!
//! Conditions are measured by deterministic checks (`ops_checks`) and turned
//! into ordinary cases, the same ones a person handles for a cash difference.
//! There is no second alert list: the checks only remember, per incident key,
//! whether the condition holds (`alert_conditions`).
//!
//! Rules (docs/OPERATIONAL_CONTROL.md):
//! * **One incident, one case.** A condition has a stable key (for example
//!   `terminal_not_seen:<device>`). While a case for the key is open, later
//!   checks update that case (last seen, occurrences, current facts); a
//!   unique index makes a second open case impossible.
//! * **Debounce.** Some kinds open a case only after the condition held for a
//!   while (`raise_after`); all kinds close only after it stayed clear for a
//!   while (`clear_after`), so a flapping condition keeps one open case and
//!   counts its returns in `occurrences`.
//! * **Auto-resolution** is only for kinds whose recovery the system can
//!   prove (the till was seen again, the records were saved, a backup was
//!   made). The case is resolved with a system event; nothing is deleted.
//! * **A person's decision sticks.** A case dismissed (or resolved) while the
//!   condition still holds is not opened again until the condition has
//!   cleared once; a real recurrence then opens a new case (a new episode),
//!   and every earlier case stays in the history.
//! * **Severity is deterministic**: low / medium / high from the measured
//!   facts, never from an AI.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};

use crate::audit::{self, Actor};
use crate::error::AppResult;
use crate::ids::{new_id, next_seq};
use crate::time;

/// One system-alert kind, defined in code (no plug-ins, no merchant scripting).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct AlertKind {
    pub kind: &'static str,
    /// Minutes the condition must hold before a case opens.
    pub raise_after_min: i64,
    /// Minutes the condition must stay clear before the case closes (and
    /// before a dismissed condition may open a new case).
    pub clear_after_min: i64,
    /// The system resolves the case when it can prove recovery.
    pub auto_resolve: bool,
    /// Where the merchant fixes it.
    pub workflow: &'static str,
}

pub const KINDS: &[AlertKind] = &[
    AlertKind { kind: "sync_failures", raise_after_min: 0, clear_after_min: 0, auto_resolve: true, workflow: "/admin/sync-reconciliation" },
    AlertKind { kind: "terminal_not_seen", raise_after_min: 0, clear_after_min: 5, auto_resolve: true, workflow: "/admin/terminals" },
    AlertKind { kind: "terminal_backlog", raise_after_min: 10, clear_after_min: 10, auto_resolve: true, workflow: "/admin/terminals" },
    AlertKind { kind: "terminal_incompatible", raise_after_min: 0, clear_after_min: 0, auto_resolve: true, workflow: "/admin/terminals" },
    AlertKind {
        kind: "credential_rotation_stale",
        raise_after_min: 0,
        clear_after_min: 0,
        auto_resolve: true,
        workflow: "/admin/terminals",
    },
    AlertKind { kind: "backup_overdue", raise_after_min: 0, clear_after_min: 0, auto_resolve: true, workflow: "/admin/backups" },
    AlertKind { kind: "print_failures", raise_after_min: 0, clear_after_min: 0, auto_resolve: true, workflow: "/admin/diagnostics" },
    AlertKind {
        kind: "payment_review_backlog",
        raise_after_min: 0,
        clear_after_min: 0,
        auto_resolve: true,
        workflow: "/admin/payment-reviews",
    },
    AlertKind { kind: "rider_cash_held", raise_after_min: 0, clear_after_min: 0, auto_resolve: true, workflow: "/admin/deliveries" },
];

pub fn kind(k: &str) -> Option<&'static AlertKind> {
    KINDS.iter().find(|x| x.kind == k)
}

/// A condition measured now. `key` is the incident's stable identity.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub key: String,
    pub kind: &'static str,
    /// low | medium | high
    pub severity: &'static str,
    pub title: String,
    pub branch_id: String,
    pub entity_type: &'static str,
    pub entity_id: String,
    pub device_id: Option<String>,
    pub facts: Value,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Reconciled {
    pub opened: usize,
    pub updated: usize,
    pub resolved: usize,
    pub suppressed: usize,
}

fn minutes_between(a: &str, b: DateTime<Utc>) -> i64 {
    time::parse(a).map(|t| (b - t).num_minutes()).unwrap_or(i64::MAX)
}

#[allow(clippy::too_many_arguments)]
fn system_event(
    c: &Connection,
    case_id: &str,
    kind: &str,
    from: Option<&str>,
    to: Option<&str>,
    note: &str,
    evidence: Option<&Value>,
    at: &str,
) -> AppResult<()> {
    let seq: i64 = c.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM case_events WHERE case_id=?1", [case_id], |r| r.get(0))?;
    let ev = evidence.cloned().unwrap_or_else(|| json!({})).as_object().cloned().map(|mut m| {
        m.insert("actor".into(), json!("system"));
        Value::Object(m)
    });
    c.execute(
        "INSERT INTO case_events(event_id, case_id, seq, kind, from_status, to_status, note, evidence_json, user_id, operation_id, at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,NULL,NULL,?9)",
        params![new_id(), case_id, seq, kind, from, to, note, ev.map(|v| v.to_string()), at],
    )?;
    Ok(())
}

/// Reconcile the measured conditions with the open cases. `evaluated` lists
/// the kinds that were checked in this run: only their keys can clear (a kind
/// that was not checked keeps its state). Safe to run any number of times
/// and concurrently (callers run it in a write transaction; the unique index
/// on open keys backs it up).
pub fn reconcile(c: &Connection, now: DateTime<Utc>, evaluated: &[&str], conditions: &[Condition]) -> AppResult<Reconciled> {
    let mut out = Reconciled::default();
    let now_s = time::fmt(now);
    let system = Actor::default();
    let present: std::collections::HashSet<&str> = conditions.iter().map(|x| x.key.as_str()).collect();
    for cond in conditions {
        let spec = match kind(cond.kind) {
            Some(s) => s,
            None => continue,
        };
        type State = (i64, Option<String>, i64);
        let state: Option<State> = c
            .query_row("SELECT active, active_since, suppressed FROM alert_conditions WHERE dedupe_key=?1", [&cond.key], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        let open: Option<(String, i64)> = c
            .query_row(
                "SELECT case_id, COALESCE(condition_active,1) FROM cases WHERE dedupe_key=?1 AND status NOT IN ('resolved','dismissed')",
                [&cond.key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (was_active, since, suppressed) = match &state {
            Some((a, s, sup)) => (*a == 1, s.clone(), *sup == 1),
            None => (false, None, false),
        };
        let since = if was_active { since.unwrap_or_else(|| now_s.clone()) } else { now_s.clone() };
        c.execute(
            "INSERT INTO alert_conditions(dedupe_key, kind, active, active_since, cleared_since, last_evaluated_at, suppressed)
             VALUES (?1,?2,1,?3,NULL,?4,0)
             ON CONFLICT(dedupe_key) DO UPDATE SET active=1, active_since=?3, cleared_since=NULL, last_evaluated_at=?4",
            params![cond.key, cond.kind, since, now_s],
        )?;
        match open {
            Some((case_id, cond_active)) => {
                // Same incident: refresh its current facts. A return after a
                // brief recovery counts as another occurrence of this case.
                let returned = cond_active == 0 || !was_active;
                c.execute(
                    "UPDATE cases SET last_seen_at=?2, latest_json=?3, severity=?4, title=?5, condition_active=1,
                        occurrences=occurrences + ?6, updated_at=CASE WHEN ?6=1 OR severity<>?4 OR title<>?5 THEN ?2 ELSE updated_at END
                     WHERE case_id=?1",
                    params![case_id, now_s, cond.facts.to_string(), cond.severity, cond.title, returned as i64],
                )?;
                if returned {
                    system_event(c, &case_id, "evidence", None, None, "The condition returned.", Some(&cond.facts), &now_s)?;
                }
                out.updated += 1;
            }
            None if suppressed => out.suppressed += 1,
            None => {
                if minutes_between(&since, now) < spec.raise_after_min {
                    continue;
                }
                let episode: i64 = c.query_row("SELECT COUNT(*)+1 FROM cases WHERE dedupe_key=?1", [&cond.key], |r| r.get(0))?;
                let id = new_id();
                let number = format!("C-{:05}", next_seq(c, "case")?);
                let mut facts = cond.facts.clone();
                if let Some(m) = facts.as_object_mut() {
                    m.insert("episode".into(), json!(episode));
                }
                c.execute(
                    "INSERT INTO cases(case_id, case_number, kind, severity, status, branch_id, entity_type, entity_id, title, facts_json,
                        created_by, created_at, updated_at, source, dedupe_key, device_id, link, latest_json, condition_active,
                        first_seen_at, last_seen_at, occurrences)
                     VALUES (?1,?2,?3,?4,'new',?5,?6,?7,?8,?9,NULL,?10,?10,'system',?11,?12,?13,?14,1,?15,?10,1)",
                    params![
                        id,
                        number,
                        cond.kind,
                        cond.severity,
                        cond.branch_id,
                        cond.entity_type,
                        cond.entity_id,
                        cond.title,
                        facts.to_string(),
                        now_s,
                        cond.key,
                        cond.device_id,
                        spec.workflow,
                        cond.facts.to_string(),
                        since
                    ],
                )?;
                system_event(c, &id, "created", None, Some("new"), &format!("Opened by the system: {}", cond.title), Some(&facts), &now_s)?;
                c.execute("UPDATE alert_conditions SET last_case_id=?2 WHERE dedupe_key=?1", params![cond.key, id])?;
                audit::record(
                    c,
                    &system,
                    "case.opened",
                    "case",
                    Some(&id),
                    None,
                    Some(&json!({ "case_number": number, "kind": cond.kind, "source": "system", "key": cond.key, "episode": episode })),
                )?;
                out.opened += 1;
            }
        }
    }
    // Conditions of the checked kinds that no longer hold.
    let mut cleared: Vec<(String, String, Option<String>, i64)> = vec![];
    {
        let mut st = c.prepare("SELECT dedupe_key, kind, cleared_since, active FROM alert_conditions")?;
        for row in
            st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, i64>(3)?)))?
        {
            let (key, k, cs, active) = row?;
            if evaluated.contains(&k.as_str()) && !present.contains(key.as_str()) {
                cleared.push((key, k, cs, active));
            }
        }
    }
    for (key, k, cleared_since, active) in cleared {
        let spec = match kind(&k) {
            Some(s) => s,
            None => continue,
        };
        let since = match (active, cleared_since) {
            (1, _) | (_, None) => {
                c.execute(
                    "UPDATE alert_conditions SET active=0, cleared_since=?2, last_evaluated_at=?2 WHERE dedupe_key=?1",
                    params![key, now_s],
                )?;
                c.execute(
                    "UPDATE cases SET condition_active=0, updated_at=?2 WHERE dedupe_key=?1 AND status NOT IN ('resolved','dismissed') AND COALESCE(condition_active,1)=1",
                    params![key, now_s],
                )?;
                now_s.clone()
            }
            (_, Some(s)) => s,
        };
        if minutes_between(&since, now) < spec.clear_after_min {
            continue;
        }
        // Clear for long enough: a dismissed condition may open a new case
        // next time; an auto-resolving case closes now.
        c.execute("UPDATE alert_conditions SET suppressed=0, last_evaluated_at=?2 WHERE dedupe_key=?1", params![key, now_s])?;
        if !spec.auto_resolve {
            continue;
        }
        let open: Option<(String, String, String)> = c
            .query_row(
                "SELECT case_id, status, case_number FROM cases WHERE dedupe_key=?1 AND status NOT IN ('resolved','dismissed')",
                [&key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((id, status, number)) = open {
            let note = "Recovered: the system no longer sees this condition.";
            c.execute(
                "UPDATE cases SET status='resolved', resolution_code='recovered', resolution_note=?2, resolved_by=NULL, resolved_at=?3,
                    updated_at=?3, condition_active=0 WHERE case_id=?1",
                params![id, note, now_s],
            )?;
            system_event(c, &id, "status", Some(&status), Some("resolved"), note, None, &now_s)?;
            audit::record(
                c,
                &system,
                "case.auto_resolved",
                "case",
                Some(&id),
                Some(&json!({ "status": status })),
                Some(&json!({ "case_number": number, "key": key })),
            )?;
            out.resolved += 1;
        }
    }
    Ok(out)
}

/// A person closed a system case (resolved or dismissed). While its
/// condition still holds, no new case is opened for the same key.
pub fn note_closed_by_person(c: &Connection, case_id: &str) -> AppResult<()> {
    let key: Option<String> = c.query_row("SELECT dedupe_key FROM cases WHERE case_id=?1", [case_id], |r| r.get(0)).optional()?.flatten();
    if let Some(k) = key {
        c.execute("UPDATE alert_conditions SET suppressed=1 WHERE dedupe_key=?1 AND active=1", [&k])?;
    }
    Ok(())
}

/// Add what a person did about an incident to its open case (if one is
/// open), as evidence in the case history under that person's name.
pub(crate) fn note_on_open_case(c: &Connection, key: &str, user_id: &str, note: &str, evidence: &Value) -> AppResult<bool> {
    let case: Option<String> = c
        .query_row("SELECT case_id FROM cases WHERE dedupe_key=?1 AND status NOT IN ('resolved','dismissed')", [key], |r| r.get(0))
        .optional()?;
    let Some(case_id) = case else { return Ok(false) };
    let seq: i64 = c.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM case_events WHERE case_id=?1", [&case_id], |r| r.get(0))?;
    c.execute(
        "INSERT INTO case_events(event_id, case_id, seq, kind, from_status, to_status, note, evidence_json, user_id, operation_id, at)
         VALUES (?1,?2,?3,'evidence',NULL,NULL,?4,?5,?6,NULL,?7)",
        params![new_id(), case_id, seq, note, evidence.to_string(), user_id, time::now_str()],
    )?;
    Ok(true)
}

/// A terminal is behind with sending when what it reported says so: its
/// oldest waiting change is older than [`BACKLOG_STALE_MIN`] minutes, or (for
/// terminals that do not report that) at least [`BACKLOG_COUNT`] changes wait
/// and it has sent nothing for [`BACKLOG_STALE_MIN`] minutes. Nothing
/// reported, nothing concluded.
pub fn is_backlog(pending: i64, oldest_pending: Option<&str>, last_push: Option<&str>, now: DateTime<Utc>) -> bool {
    if pending <= 0 {
        return false;
    }
    let stale = |t: Option<&str>| t.map(|t| minutes_between(t, now) >= BACKLOG_STALE_MIN);
    match stale(oldest_pending) {
        Some(old) => old,
        None => pending >= BACKLOG_COUNT && stale(last_push).unwrap_or(true),
    }
}

/// A sync problem's reason in plain words (the screens translate it).
pub fn reason_phrase(code: &str) -> &'static str {
    match code {
        "version_mismatch" => "the app versions do not match",
        "not_permitted" => "the till is not allowed to change these records",
        "invalid_record" => "the records are incomplete",
        "missing_dependency" => "records they depend on have not arrived",
        "conflict" => "they conflict with records already saved",
        "superseded" => "newer versions were saved since",
        "storage_error" => "the database was busy or full",
        "legacy_unclassified" => "the reason was not recorded before this update",
        _ => "an unexpected error",
    }
}

/// The incident key for records from one computer refused for one reason.
pub fn sync_failure_key(origin: Option<&str>, reason: &str) -> String {
    format!("sync_failures:{}:{reason}", origin.filter(|o| !o.is_empty()).unwrap_or("local"))
}

// ------------------------------------------------------------------ the checks

/// Thresholds (documented in OPERATIONAL_CONTROL.md). Terminals sync every
/// few seconds and send a heartbeat each cycle; a till is "not seen
/// recently" after 5 minutes on the health view, and becomes a case only
/// after 15 minutes while it has an open shift (a till switched off at
/// closing time is not an incident).
pub const NOT_SEEN_HEALTH_MIN: i64 = 5;
pub const NOT_SEEN_CASE_MIN: i64 = 15;
pub const NOT_SEEN_HIGH_MIN: i64 = 120;
/// A backlog is many records waiting while the till is alive but has not
/// pushed for a while.
pub const BACKLOG_COUNT: i64 = 50;
pub const BACKLOG_STALE_MIN: i64 = 15;
pub const PAYMENT_REVIEW_WAIT_MIN: i64 = 60;
pub const RIDER_CASH_HOURS: i64 = 24;

/// Tables whose failure to save touches money, stock or sale evidence.
pub const FINANCIAL_TABLES: &[&str] = &[
    "sales",
    "sale_items",
    "payments",
    "refunds",
    "refund_items",
    "refund_tenders",
    "cash_events",
    "stock_movements",
    "shifts",
    "customer_ledger",
    "loyalty_ledger",
    "sale_collections",
    "rider_handovers",
    "rider_handover_items",
    "sale_voids",
    "sale_item_promotions",
    "coupon_redemptions",
    "receipt_snapshots",
];

/// The kinds the checks below measure.
pub const CHECKED: &[&str] = &[
    "sync_failures",
    "terminal_not_seen",
    "terminal_backlog",
    "terminal_incompatible",
    "backup_overdue",
    "print_failures",
    "payment_review_backlog",
    "rider_cash_held",
    "credential_rotation_stale",
];

/// Measure every condition now. Reads only; bounded queries (grouped, never
/// one query per row).
pub fn measure(c: &Connection, now: DateTime<Utc>, this_device: &str, backup_state: Option<(&str, &str)>) -> AppResult<Vec<Condition>> {
    let mut out = vec![];
    let branch: String =
        c.query_row("SELECT branch_id FROM devices WHERE device_id=?1", [this_device], |r| r.get(0)).optional()?.unwrap_or_default();
    let ago = |min: i64| time::fmt(now - chrono::Duration::minutes(min));
    // Records that could not be saved: one incident per sending computer
    // and reason (400 sales refused for one reason are one case).
    {
        let fin = FINANCIAL_TABLES.iter().map(|t| format!("'{t}'")).collect::<Vec<_>>().join(",");
        let mut st = c.prepare(&format!(
            "SELECT COALESCE(l.origin,''), COALESCE(d.name,''), COALESCE(d.branch_id,''), COUNT(*), SUM(l.table_name IN ({fin})),
                    MIN(l.created_at), GROUP_CONCAT(DISTINCT l.table_name), l.reason_code, SUM(l.retryable)
             FROM sync_dead_letters l LEFT JOIN devices d ON d.device_id=l.origin
             WHERE l.status='open' GROUP BY COALESCE(l.origin,''), l.reason_code"
        ))?;
        for row in st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })? {
            let (origin, name, br, n, fin_n, oldest, tables, reason, retryable) = row?;
            let who = if origin.is_empty() {
                "this computer".to_string()
            } else if name.is_empty() {
                "another computer".into()
            } else {
                name
            };
            out.push(Condition {
                key: sync_failure_key(Some(&origin), &reason),
                kind: "sync_failures",
                severity: if fin_n > 0 { "high" } else { "medium" },
                title: format!("{n} records from {who} could not be saved: {}", reason_phrase(&reason)),
                branch_id: if br.is_empty() { branch.clone() } else { br },
                entity_type: "device",
                entity_id: if origin.is_empty() { this_device.to_string() } else { origin.clone() },
                device_id: (!origin.is_empty()).then_some(origin),
                facts: json!({ "count": n, "money_or_stock": fin_n, "oldest": oldest, "reason": reason, "can_retry": retryable,
                    "tables": tables.unwrap_or_default().split(',').collect::<Vec<_>>() }),
            });
        }
    }
    // Terminals, from what the hub observed (heartbeats); never guessed.
    {
        let hub_schema = crate::db::latest_schema_version();
        let mut st = c.prepare(
            "SELECT d.device_id, d.name, d.branch_id, h.last_seen_at, h.pending_count, h.last_push_at, h.schema_version, h.app_version,
                    (SELECT x.shift_number FROM shifts x WHERE x.device_id=d.device_id AND x.status='open' ORDER BY x.opened_at DESC LIMIT 1),
                    h.protocol_version, h.oldest_pending_at
             FROM devices d JOIN device_heartbeats h ON h.device_id=d.device_id
             WHERE d.active=1 AND d.operating_mode='terminal'",
        )?;
        for row in st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, Option<String>>(10)?,
            ))
        })? {
            let (id, name, br, seen, pending, last_push, schema, app, shift, protocol, oldest_pending) = row?;
            let since = minutes_between(&seen, now);
            if since >= NOT_SEEN_CASE_MIN {
                if let Some(shift) = shift {
                    out.push(Condition {
                        key: format!("terminal_not_seen:{id}"),
                        kind: "terminal_not_seen",
                        severity: if since >= NOT_SEEN_HIGH_MIN { "high" } else { "medium" },
                        title: format!("{name} has not been seen for {since} minutes during an open shift"),
                        branch_id: br.clone(),
                        entity_type: "device",
                        entity_id: id.clone(),
                        device_id: Some(id.clone()),
                        facts: json!({ "last_seen_at": seen, "minutes": since, "open_shift": shift }),
                    });
                }
            }
            let pending = pending.unwrap_or(0);
            if since < NOT_SEEN_CASE_MIN && is_backlog(pending, oldest_pending.as_deref(), last_push.as_deref(), now) {
                out.push(Condition {
                    key: format!("terminal_backlog:{id}"),
                    kind: "terminal_backlog",
                    severity: "medium",
                    title: format!("{name} has {pending} records waiting to send"),
                    branch_id: br.clone(),
                    entity_type: "device",
                    entity_id: id.clone(),
                    device_id: Some(id.clone()),
                    facts: json!({ "pending": pending, "oldest_pending_at": oldest_pending, "last_push_at": last_push, "last_seen_at": seen }),
                });
            }
            if let Some(p) = protocol.filter(|p| *p != crate::sync::PROTOCOL_VERSION) {
                out.push(Condition {
                    key: format!("terminal_incompatible:{id}"),
                    kind: "terminal_incompatible",
                    severity: "high",
                    title: format!("{name} speaks a different sync protocol ({p}; the hub speaks {})", crate::sync::PROTOCOL_VERSION),
                    branch_id: br.clone(),
                    entity_type: "device",
                    entity_id: id.clone(),
                    device_id: Some(id.clone()),
                    facts: json!({ "protocol_version": p, "hub_protocol_version": crate::sync::PROTOCOL_VERSION, "app_version": app }),
                });
            } else if let Some(v) = schema.filter(|v| *v != hub_schema) {
                out.push(Condition {
                    key: format!("terminal_incompatible:{id}"),
                    kind: "terminal_incompatible",
                    severity: "high",
                    title: format!("{name} runs a different database version ({v}; the hub has {hub_schema})"),
                    branch_id: br.clone(),
                    entity_type: "device",
                    entity_id: id.clone(),
                    device_id: Some(id.clone()),
                    facts: json!({ "schema_version": v, "hub_schema_version": hub_schema, "app_version": app }),
                });
            }
        }
    }
    // Credential rotations a terminal has not picked up (versions and times
    // only).
    {
        let mut st = c.prepare(
            "SELECT device_id, name, branch_id, credential_staged_at, credential_version, credential_next_version FROM devices
             WHERE operating_mode='terminal' AND active=1 AND credential_next_version IS NOT NULL AND credential_staged_at < ?1",
        )?;
        let cutoff = time::fmt(now - chrono::Duration::hours(crate::device_credentials::STALE_HOURS));
        for row in st.query_map([&cutoff], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })? {
            let (id, name, br, staged, v, n) = row?;
            let hours = minutes_between(&staged, now) / 60;
            out.push(Condition {
                key: format!("credential_rotation_stale:{id}"),
                kind: "credential_rotation_stale",
                severity: "medium",
                title: format!("{name} has not picked up its new credential for {hours} hours"),
                branch_id: br,
                entity_type: "device",
                entity_id: id.clone(),
                device_id: Some(id),
                facts: json!({ "staged_at": staged, "version": v, "next_version": n }),
            });
        }
    }
    // Backups (a store-wide fact, kept by the backup subsystem).
    if let Some((state, summary)) = backup_state.filter(|b| b.0 == "warning" || b.0 == "error") {
        out.push(Condition {
            key: "backup_overdue".into(),
            kind: "backup_overdue",
            severity: if state == "error" { "high" } else { "medium" },
            title: summary.to_string(),
            branch_id: branch.clone(),
            entity_type: "store",
            entity_id: "backup".into(),
            device_id: None,
            facts: json!({ "state": state, "summary": summary }),
        });
    }
    // Print jobs that failed on this computer (observed, not a printer state).
    {
        let (n, last_fail, last_ok): (i64, Option<String>, Option<String>) = c.query_row(
            "SELECT COUNT(*) FILTER (WHERE status='failed'), MAX(CASE WHEN status='failed' THEN updated_at END),
                    MAX(CASE WHEN status='printed' THEN updated_at END)
             FROM print_jobs WHERE kind<>'drawer' AND updated_at>=?1",
            [ago(24 * 60)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let still = match (&last_fail, &last_ok) {
            (Some(f), Some(o)) => f > o,
            (Some(_), None) => true,
            _ => false,
        };
        if n > 0 && still {
            out.push(Condition {
                key: format!("print_failures:{this_device}"),
                kind: "print_failures",
                severity: "low",
                title: format!("{n} print jobs failed in the last 24 hours"),
                branch_id: branch.clone(),
                entity_type: "device",
                entity_id: this_device.to_string(),
                device_id: Some(this_device.to_string()),
                facts: json!({ "failed_24h": n, "last_failed_at": last_fail }),
            });
        }
    }
    // Payment screenshots waiting for a person.
    {
        let (n, oldest): (i64, Option<String>) = c.query_row(
            "SELECT COUNT(*), MIN(created_at) FROM payment_reviews
             WHERE status IN ('pending','ocr_match','likely_match','mismatch','needs_review') AND created_at < ?1",
            [ago(PAYMENT_REVIEW_WAIT_MIN)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if n > 0 {
            out.push(Condition {
                key: "payment_review_backlog".into(),
                kind: "payment_review_backlog",
                severity: "medium",
                title: format!("{n} payment screenshots have waited over an hour to be checked"),
                branch_id: branch.clone(),
                entity_type: "store",
                entity_id: "payment_reviews".into(),
                device_id: None,
                facts: json!({ "waiting": n, "oldest": oldest }),
            });
        }
    }
    // Delivery cash a rider still holds after a day.
    {
        let mut st = c.prepare(
            "SELECT k.held_by, COALESCE(u.display_name,''), COUNT(*), SUM(k.amount_minor), MIN(k.created_at)
             FROM sale_collections k LEFT JOIN users u ON u.user_id=k.held_by
             WHERE k.held_by IS NOT NULL AND k.created_at < ?1
               AND NOT EXISTS (SELECT 1 FROM rider_handover_items i WHERE i.collection_id=k.collection_id)
             GROUP BY k.held_by",
        )?;
        for row in st.query_map([ago(RIDER_CASH_HOURS * 60)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, String>(4)?))
        })? {
            let (rider, name, n, amount, oldest) = row?;
            out.push(Condition {
                key: format!("rider_cash_held:{rider}"),
                kind: "rider_cash_held",
                severity: "medium",
                title: format!("{name} has held delivery cash from {n} orders for over a day"),
                branch_id: branch.clone(),
                entity_type: "user",
                entity_id: rider,
                device_id: None,
                facts: json!({ "collections": n, "amount_minor": amount, "oldest": oldest }),
            });
        }
    }
    Ok(out)
}

impl crate::service::AppCore {
    /// Run the checks and update the Alert Centre (hub or single computer;
    /// a terminal never writes cases). Idempotent; never on the sale path.
    /// A person asks for the checks to run now (they also run every minute
    /// on the hub). Same checks, same rules; nothing else.
    pub fn ops_check_now(&self, token: &str) -> AppResult<Reconciled> {
        let s = self.session(token)?;
        s.require("cases.manage")?;
        self.ops_evaluate()
    }

    pub fn ops_evaluate(&self) -> AppResult<Reconciled> {
        let d = match self.device() {
            Some(d) if d.mode != "terminal" => d,
            _ => return Ok(Reconciled::default()),
        };
        let backup = self.backup_diagnostic().ok();
        let backup_state = backup.as_ref().map(|b| (b.state.as_str(), b.summary.as_str()));
        let now = time::now();
        self.db.write(|tx| {
            let conds = measure(tx, now, &d.device_id, backup_state)?;
            reconcile(tx, now, CHECKED, &conds)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate_until(&c, std::path::Path::new(":memory:"), i64::MAX).unwrap();
        c
    }

    fn cond(key: &str, k: &'static str) -> Condition {
        Condition {
            key: key.into(),
            kind: k,
            severity: "medium",
            title: "Till 2 not seen".into(),
            branch_id: "b".into(),
            entity_type: "device",
            entity_id: "d2".into(),
            device_id: Some("d2".into()),
            facts: json!({ "minutes": 20 }),
        }
    }

    fn at(min: i64) -> DateTime<Utc> {
        time::parse("2026-10-08T08:00:00.000Z").unwrap() + chrono::Duration::minutes(min)
    }

    fn count(c: &Connection, sql: &str) -> i64 {
        c.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn one_incident_is_one_case_however_often_checked() {
        let c = db();
        let k = ["terminal_not_seen"];
        for m in 0..60 {
            reconcile(&c, at(m), &k, &[cond("terminal_not_seen:d2", "terminal_not_seen")]).unwrap();
        }
        assert_eq!(count(&c, "SELECT COUNT(*) FROM cases"), 1);
        assert_eq!(count(&c, "SELECT occurrences FROM cases"), 1);
        // Concurrent second run at the same instant: still one.
        reconcile(&c, at(60), &k, &[cond("terminal_not_seen:d2", "terminal_not_seen")]).unwrap();
        assert_eq!(count(&c, "SELECT COUNT(*) FROM cases"), 1);
    }

    #[test]
    fn flapping_keeps_one_case_and_counts_returns() {
        let c = db();
        let k = ["terminal_not_seen"];
        let on = [cond("terminal_not_seen:d2", "terminal_not_seen")];
        // Disappears, returns for 2 minutes (less than clear_after = 5), disappears again: five times.
        let mut m = 0;
        for _ in 0..5 {
            reconcile(&c, at(m), &k, &on).unwrap();
            reconcile(&c, at(m + 1), &k, &[]).unwrap();
            reconcile(&c, at(m + 3), &k, &[]).unwrap();
            m += 4;
        }
        reconcile(&c, at(m), &k, &on).unwrap();
        assert_eq!(count(&c, "SELECT COUNT(*) FROM cases"), 1);
        assert_eq!(count(&c, "SELECT status='new' FROM cases"), 1);
        assert_eq!(count(&c, "SELECT occurrences FROM cases"), 6);
        // Then it stays clear for 5 minutes: resolved by the system, kept.
        reconcile(&c, at(m + 1), &k, &[]).unwrap();
        reconcile(&c, at(m + 7), &k, &[]).unwrap();
        assert_eq!(
            count(&c, "SELECT COUNT(*) FROM cases WHERE status='resolved' AND resolution_code='recovered' AND resolved_by IS NULL"),
            1
        );
        // A real recurrence later is a new episode; the first case is untouched.
        reconcile(&c, at(m + 60), &k, &on).unwrap();
        assert_eq!(count(&c, "SELECT COUNT(*) FROM cases"), 2);
        assert_eq!(count(&c, "SELECT COUNT(*) FROM cases WHERE json_extract(facts_json,'$.episode')=2 AND status='new'"), 1);
    }

    #[test]
    fn a_dismissed_condition_stays_quiet_until_it_clears() {
        let c = db();
        let k = ["backup_overdue"];
        let on = [cond("backup_overdue", "backup_overdue")];
        reconcile(&c, at(0), &k, &on).unwrap();
        let id: String = c.query_row("SELECT case_id FROM cases", [], |r| r.get(0)).unwrap();
        c.execute("UPDATE cases SET status='dismissed' WHERE case_id=?1", [&id]).unwrap();
        note_closed_by_person(&c, &id).unwrap();
        for m in 1..30 {
            let r = reconcile(&c, at(m), &k, &on).unwrap();
            assert_eq!(r.opened, 0);
        }
        assert_eq!(count(&c, "SELECT COUNT(*) FROM cases"), 1);
        // Clears, then returns: a new case.
        reconcile(&c, at(31), &k, &[]).unwrap();
        reconcile(&c, at(32), &k, &on).unwrap();
        assert_eq!(count(&c, "SELECT COUNT(*) FROM cases"), 2);
    }

    #[test]
    fn debounce_waits_before_opening_and_unchecked_kinds_never_clear() {
        let c = db();
        let k = ["terminal_backlog"];
        let on = [cond("terminal_backlog:d2", "terminal_backlog")];
        reconcile(&c, at(0), &k, &on).unwrap();
        reconcile(&c, at(9), &k, &on).unwrap();
        assert_eq!(count(&c, "SELECT COUNT(*) FROM cases"), 0, "raise_after = 10 minutes");
        reconcile(&c, at(10), &k, &on).unwrap();
        assert_eq!(count(&c, "SELECT COUNT(*) FROM cases"), 1);
        // A run that did not check this kind leaves it alone.
        reconcile(&c, at(40), &["backup_overdue"], &[]).unwrap();
        assert_eq!(count(&c, "SELECT COUNT(*) FROM cases WHERE status='new' AND condition_active=1"), 1);
    }
}
