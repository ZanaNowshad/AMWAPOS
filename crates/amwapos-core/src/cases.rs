//! Operational cases: something a person should look into, with the facts,
//! a status and a history that is never changed.
//!
//! Lifecycle: new → acknowledged → in progress → resolved, or dismissed.
//! Finished cases stay finished (a database trigger refuses changes). Every
//! step is a `case_events` row with who, when and why.
//!
//! First use: cash differences. A closed shift whose drawer differs from the
//! expected amount by more than Settings → Shift → "Open a case above" gets a
//! case, made on the hub (or a single computer) once the shift is known
//! there. The case states facts ("Drawer is BHD 6.250 short"); it never
//! says who is to blame.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit::{self, Actor};
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::{new_id, next_seq};
use crate::service::AppCore;
use crate::settings;
use crate::shifts::shift_summary;
use crate::time;
use crate::validate;

pub const STATUSES: [&str; 5] = ["new", "acknowledged", "in_progress", "resolved", "dismissed"];
pub const CASH_RESOLUTIONS: [&str; 5] = ["counting_error", "cash_found", "change_error", "unexplained", "other"];

#[derive(Debug, Clone, Serialize)]
pub struct CaseRow {
    pub case_id: String,
    pub case_number: String,
    pub kind: String,
    pub severity: String,
    pub status: String,
    pub branch_id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub title: String,
    pub facts: Value,
    pub assignee_user_id: Option<String>,
    pub assignee_name: Option<String>,
    pub created_by_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub resolution_code: Option<String>,
    pub resolution_note: Option<String>,
    pub resolved_by_name: Option<String>,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseEvent {
    pub seq: i64,
    pub kind: String,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub note: Option<String>,
    pub evidence: Option<Value>,
    pub user_name: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseDetail {
    #[serde(flatten)]
    pub case: CaseRow,
    pub events: Vec<CaseEvent>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CaseAction {
    pub case_id: String,
    /// acknowledge | start | resolve | dismiss | note | assign
    pub action: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub assignee_user_id: Option<String>,
    #[serde(default)]
    pub resolution_code: Option<String>,
    pub operation_id: String,
}

const ROW_SQL: &str = "SELECT k.case_id, k.case_number, k.kind, k.severity, k.status, k.branch_id, k.entity_type, k.entity_id, k.title, k.facts_json,
        k.assignee_user_id, a.display_name, c.display_name, k.created_at, k.updated_at, k.resolution_code, k.resolution_note, r.display_name, k.resolved_at
     FROM cases k LEFT JOIN users a ON a.user_id=k.assignee_user_id LEFT JOIN users c ON c.user_id=k.created_by
     LEFT JOIN users r ON r.user_id=k.resolved_by";

fn row(r: &rusqlite::Row) -> rusqlite::Result<CaseRow> {
    let facts: String = r.get(9)?;
    Ok(CaseRow {
        case_id: r.get(0)?,
        case_number: r.get(1)?,
        kind: r.get(2)?,
        severity: r.get(3)?,
        status: r.get(4)?,
        branch_id: r.get(5)?,
        entity_type: r.get(6)?,
        entity_id: r.get(7)?,
        title: r.get(8)?,
        facts: serde_json::from_str(&facts).unwrap_or(Value::Null),
        assignee_user_id: r.get(10)?,
        assignee_name: r.get(11)?,
        created_by_name: r.get(12)?,
        created_at: r.get(13)?,
        updated_at: r.get(14)?,
        resolution_code: r.get(15)?,
        resolution_note: r.get(16)?,
        resolved_by_name: r.get(17)?,
        resolved_at: r.get(18)?,
    })
}

fn get(c: &Connection, id: &str) -> AppResult<CaseRow> {
    c.query_row(&format!("{ROW_SQL} WHERE k.case_id=?1"), [id], row).optional()?.ok_or_else(|| AppError::not_found("Case"))
}

#[allow(clippy::too_many_arguments)]
fn add_event(
    c: &Connection,
    case_id: &str,
    kind: &str,
    from: Option<&str>,
    to: Option<&str>,
    note: Option<&str>,
    evidence: Option<&Value>,
    user: Option<&str>,
    operation_id: Option<&str>,
) -> AppResult<()> {
    let seq: i64 = c.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM case_events WHERE case_id=?1", [case_id], |r| r.get(0))?;
    c.execute(
        "INSERT INTO case_events(event_id, case_id, seq, kind, from_status, to_status, note, evidence_json, user_id, operation_id, at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![new_id(), case_id, seq, kind, from, to, note, evidence.map(|e| e.to_string()), user, operation_id, time::now_str()],
    )?;
    Ok(())
}

/// The facts of a drawer difference, fixed when the case opens.
fn cash_facts(c: &Connection, shift_id: &str) -> AppResult<(Value, i64, String)> {
    let s = shift_summary(c, shift_id)?;
    let variance = s.variance_minor.unwrap_or(0);
    let mut st = c.prepare(
        "SELECT e.type, e.amount_minor, e.reason, e.created_at, u.display_name, a.display_name FROM cash_events e
         LEFT JOIN users u ON u.user_id=e.actor_user_id LEFT JOIN users a ON a.user_id=e.approved_by
         WHERE e.shift_id=?1 ORDER BY e.created_at",
    )?;
    let movements = st
        .query_map([shift_id], |r| {
            Ok(json!({ "kind": r.get::<_, String>(0)?, "amount_minor": r.get::<_, i64>(1)?, "reason": r.get::<_, String>(2)?,
                       "at": r.get::<_, String>(3)?, "by": r.get::<_, Option<String>>(4)?, "approved_by": r.get::<_, Option<String>>(5)? }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let facts = json!({
        "shift_id": s.shift_id, "shift_number": s.shift_number, "business_date": s.business_date,
        "register_id": s.register_id, "register_name": s.register_name, "drawer_id": s.drawer_id, "drawer_name": s.drawer_name,
        "device_name": s.device_name, "cashier_user_id": s.user_id, "cashier_name": s.cashier_name,
        "opened_at": s.opened_at, "closed_at": s.closed_at,
        "opening_float_minor": s.opening_float_minor, "cash_sales_minor": s.cash_sales_minor, "cash_refunds_minor": s.cash_refunds_minor,
        "paid_in_minor": s.paid_in_minor, "paid_out_minor": s.paid_out_minor, "safe_drop_minor": s.safe_drop_minor,
        "delivery_collections_minor": s.cash_collections_minor + s.rider_handover_minor,
        "expected_cash_minor": s.expected_cash_minor, "counted_cash_minor": s.counted_cash_minor, "variance_minor": variance,
        "approved_by_name": s.variance_approved_by_name, "close_note": s.close_note, "cash_movements": movements,
    });
    let branch: String = c.query_row("SELECT branch_id FROM shifts WHERE shift_id=?1", [shift_id], |r| r.get(0))?;
    Ok((facts, variance, branch))
}

fn severity(variance: i64, threshold: i64) -> &'static str {
    let t = threshold.max(1);
    if variance.abs() >= t * 5 {
        "high"
    } else if variance.abs() >= t * 2 {
        "medium"
    } else {
        "low"
    }
}

/// Open the cash-difference case for a shift (once; an existing case is
/// returned as it is).
fn open_cash_case(c: &Connection, shift_id: &str, created_by: Option<&str>, note: Option<&str>) -> AppResult<(String, bool)> {
    if let Some(id) = c
        .query_row("SELECT case_id FROM cases WHERE kind='cash_variance' AND entity_type='shift' AND entity_id=?1", [shift_id], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
    {
        return Ok((id, false));
    }
    let status: String = c
        .query_row("SELECT status FROM shifts WHERE shift_id=?1", [shift_id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| AppError::not_found("Shift"))?;
    if status != "closed" {
        return Err(AppError::conflict("Count and close the shift first."));
    }
    let (facts, variance, branch) = cash_facts(c, shift_id)?;
    let cfg: settings::ShiftSettings = settings::get(c, settings::KEY_SHIFT)?;
    let title =
        if variance == 0 { "Drawer matched the expected amount".to_string() } else { crate::dayclose::variance_words(c, variance)? };
    let id = new_id();
    let number = format!("C-{:05}", next_seq(c, "case")?);
    let now = time::now_str();
    c.execute(
        "INSERT INTO cases(case_id, case_number, kind, severity, status, branch_id, entity_type, entity_id, title, facts_json, created_by, created_at, updated_at)
         VALUES (?1,?2,'cash_variance',?3,'new',?4,'shift',?5,?6,?7,?8,?9,?9)",
        params![id, number, severity(variance, cfg.variance_case_minor), branch, shift_id, title, facts.to_string(), created_by, now],
    )?;
    add_event(c, &id, "created", None, Some("new"), note, None, created_by, None)?;
    let actor = Actor { user_id: created_by.map(String::from), device_id: None, branch_id: Some(branch), approved_by: None };
    audit::record(
        c,
        &actor,
        "case.opened",
        "case",
        Some(&id),
        None,
        Some(&json!({ "case_number": number, "kind": "cash_variance", "shift_id": shift_id, "variance_minor": variance })),
    )?;
    Ok((id, true))
}

/// Make a case for every counted drawer whose difference is above the
/// setting and that has none yet. Only shifts closed after this feature
/// arrived (migration 28) are looked at, so an upgrade does not reopen old
/// history. Safe to run any number of times.
pub fn sweep_cash_variances(c: &rusqlite::Transaction) -> AppResult<usize> {
    let cfg: settings::ShiftSettings = settings::get(c, settings::KEY_SHIFT)?;
    let since: Option<String> = c.query_row("SELECT applied_at FROM schema_migrations WHERE version=28", [], |r| r.get(0)).optional()?;
    let Some(since) = since else { return Ok(0) };
    let mut st = c.prepare(
        "SELECT s.shift_id FROM shifts s WHERE s.status='closed' AND s.closed_at>=?1 AND ABS(COALESCE(s.variance_minor,0))>?2 AND COALESCE(s.variance_minor,0)<>0
           AND NOT EXISTS (SELECT 1 FROM cases k WHERE k.kind='cash_variance' AND k.entity_type='shift' AND k.entity_id=s.shift_id)",
    )?;
    let ids = st.query_map(params![since, cfg.variance_case_minor], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    for id in &ids {
        open_cash_case(c, id, None, None)?;
    }
    Ok(ids.len())
}

fn allowed(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("new", "acknowledged" | "in_progress" | "resolved" | "dismissed")
            | ("acknowledged", "in_progress" | "resolved" | "dismissed")
            | ("in_progress", "resolved" | "dismissed")
    )
}

impl AppCore {
    fn cases_writable(&self) -> AppResult<()> {
        if self.device().map(|d| d.mode == "terminal").unwrap_or(false) {
            return Err(AppError::conflict("Cases are handled on the hub computer."));
        }
        Ok(())
    }

    /// Run the cash-difference sweep where cases live (hub or single computer).
    pub(crate) fn sweep_cases(&self) {
        if self.cases_writable().is_ok() {
            let _ = self.db.write(sweep_cash_variances);
        }
    }

    pub fn cases_list(&self, token: &str, status: Option<String>) -> AppResult<Vec<CaseRow>> {
        let s = self.session(token)?;
        s.require("cases.view")?;
        self.db.read(|c| {
            let scope = crate::branches::list_scope(c, &s)?;
            let filter = match status.as_deref().unwrap_or("open") {
                "open" => "k.status NOT IN ('resolved','dismissed')",
                "closed" => "k.status IN ('resolved','dismissed')",
                "all" => "1=1",
                other if STATUSES.contains(&other) => "k.status=?2",
                _ => return Err(AppError::validation("Unknown case status.")),
            };
            let mut st = c.prepare(&format!(
                "{ROW_SQL} WHERE (?1 IS NULL OR k.branch_id=?1) AND {filter} AND (?2 IS NULL OR ?2=?2)
                 ORDER BY CASE k.status WHEN 'new' THEN 0 WHEN 'acknowledged' THEN 1 WHEN 'in_progress' THEN 2 ELSE 3 END,
                          CASE k.severity WHEN 'high' THEN 0 WHEN 'medium' THEN 1 ELSE 2 END, k.created_at DESC LIMIT 500"
            ))?;
            let rows = st.query_map(params![scope, status], row)?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn case_get(&self, token: &str, case_id: &str) -> AppResult<CaseDetail> {
        let s = self.session(token)?;
        s.require("cases.view")?;
        let id = validate::id(case_id, "Case")?;
        self.db.read(|c| {
            let case = get(c, &id)?;
            if let Some(b) = crate::branches::list_scope(c, &s)? {
                if b != case.branch_id {
                    return Err(AppError::not_found("Case"));
                }
            }
            let mut st = c.prepare(
                "SELECT e.seq, e.kind, e.from_status, e.to_status, e.note, e.evidence_json, u.display_name, e.at FROM case_events e
                 LEFT JOIN users u ON u.user_id=e.user_id WHERE e.case_id=?1 ORDER BY e.seq",
            )?;
            let events = st
                .query_map([&id], |r| {
                    Ok(CaseEvent {
                        seq: r.get(0)?,
                        kind: r.get(1)?,
                        from_status: r.get(2)?,
                        to_status: r.get(3)?,
                        note: r.get(4)?,
                        evidence: r.get::<_, Option<String>>(5)?.and_then(|e| serde_json::from_str(&e).ok()),
                        user_name: r.get(6)?,
                        at: r.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CaseDetail { case, events })
        })
    }

    /// Open a cash-difference case for a shift by hand (any difference).
    pub fn case_open_for_shift(&self, token: &str, shift_id: &str, note: Option<String>) -> AppResult<CaseDetail> {
        let s = self.session(token)?;
        s.require("cases.manage")?;
        self.cases_writable()?;
        let sid = validate::id(shift_id, "Shift")?;
        let note = crate::setup::clean_opt(&note, "Note", 500)?;
        let id = self.db.write(|tx| Ok(open_cash_case(tx, &sid, Some(&s.user_id), note.as_deref())?.0))?;
        self.case_get(token, &id)
    }

    /// One step on a case. Acknowledge, start, note and assign need
    /// `cases.manage`; resolve and dismiss need `cases.resolve` and a note.
    pub fn case_act(&self, token: &str, req: CaseAction) -> AppResult<CaseDetail> {
        let s = self.session(token)?;
        let perm = match req.action.as_str() {
            "acknowledge" | "start" | "note" | "assign" => "cases.manage",
            "resolve" | "dismiss" => "cases.resolve",
            _ => return Err(AppError::validation("Unknown case action.")),
        };
        s.require(perm)?;
        self.cases_writable()?;
        let id = validate::id(&req.case_id, "Case")?;
        let note = crate::setup::clean_opt(&req.note, "Note", 1000)?;
        idempotency::validate_operation_id(&req.operation_id)?;
        if let Check::Replay { .. } = self.db.read(|c| idempotency::check(c, &req.operation_id, "case.act", &req))? {
            return self.case_get(token, &id);
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "case.act", &req)? {
                Check::Replay { .. } => return Ok(()),
                Check::New { payload_hash } => payload_hash,
            };
            let case = get(tx, &id)?;
            if let Some(b) = crate::branches::list_scope(tx, &s)? {
                if b != case.branch_id {
                    return Err(AppError::not_found("Case"));
                }
            }
            if matches!(case.status.as_str(), "resolved" | "dismissed") {
                return Err(AppError::conflict(if case.status == "resolved" { "This case is already resolved." } else { "This case was dismissed." }));
            }
            let now = time::now_str();
            match req.action.as_str() {
                "note" => {
                    let n = note.clone().ok_or_else(|| AppError::validation("Write the note."))?;
                    add_event(tx, &id, "note", None, None, Some(&n), None, Some(&s.user_id), Some(&req.operation_id))?;
                    tx.execute("UPDATE cases SET updated_at=?2 WHERE case_id=?1", params![id, now])?;
                }
                "assign" => {
                    let who = match req.assignee_user_id.as_deref().filter(|u| !u.is_empty()) {
                        Some(u) => {
                            let u = validate::id(u, "Person")?;
                            let ok: Option<i64> = tx.query_row("SELECT active FROM users WHERE user_id=?1", [&u], |r| r.get(0)).optional()?;
                            if ok != Some(1) {
                                return Err(AppError::validation("Choose an active person."));
                            }
                            Some(u)
                        }
                        None => None,
                    };
                    tx.execute("UPDATE cases SET assignee_user_id=?2, updated_at=?3 WHERE case_id=?1", params![id, who, now])?;
                    add_event(tx, &id, "assigned", None, None, note.as_deref(), Some(&json!({ "assignee_user_id": who })), Some(&s.user_id), Some(&req.operation_id))?;
                }
                act => {
                    let to = match act {
                        "acknowledge" => "acknowledged",
                        "start" => "in_progress",
                        "resolve" => "resolved",
                        _ => "dismissed",
                    };
                    if !allowed(&case.status, to) {
                        return Err(AppError::conflict("That step does not apply to this case now."));
                    }
                    let code = if to == "resolved" || to == "dismissed" {
                        if note.is_none() {
                            return Err(AppError::validation("Say what was found."));
                        }
                        match req.resolution_code.as_deref().filter(|c| !c.is_empty()) {
                            Some(c) if case.kind == "cash_variance" && !CASH_RESOLUTIONS.contains(&c) => {
                                return Err(AppError::validation("Unknown outcome."));
                            }
                            other => other.map(String::from),
                        }
                    } else {
                        None
                    };
                    if to == "resolved" || to == "dismissed" {
                        tx.execute(
                            "UPDATE cases SET status=?2, resolution_code=?3, resolution_note=?4, resolved_by=?5, resolved_at=?6, updated_at=?6 WHERE case_id=?1",
                            params![id, to, code, note, s.user_id, now],
                        )?;
                    } else {
                        tx.execute("UPDATE cases SET status=?2, updated_at=?3 WHERE case_id=?1", params![id, to, now])?;
                    }
                    add_event(tx, &id, "status", Some(&case.status), Some(to), note.as_deref(), code.as_ref().map(|c| json!({ "resolution_code": c })).as_ref(), Some(&s.user_id), Some(&req.operation_id))?;
                }
            }
            audit::record(tx, &actor, &format!("case.{}", req.action), "case", Some(&id), Some(&json!({ "status": case.status })), Some(&json!({ "action": req.action, "note": note, "resolution_code": req.resolution_code })))?;
            idempotency::complete(tx, &req.operation_id, "case.act", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &json!({ "case_id": id }))?;
            Ok(())
        })?;
        self.case_get(token, &id)
    }

    /// Attach a photo or document to a case as evidence.
    pub fn case_attach(&self, token: &str, case_id: &str, file_name: &str, data_b64: &str) -> AppResult<CaseDetail> {
        let s = self.session(token)?;
        s.require("cases.manage")?;
        self.cases_writable()?;
        let id = validate::id(case_id, "Case")?;
        let name = crate::setup::clean(file_name, "File name", 120, true)?;
        let bytes = crate::ids::b64_decode(data_b64).ok_or_else(|| AppError::validation("The file could not be read."))?;
        let fid = new_id();
        let (path, sha, mime) = crate::docintel::service::store_original(&self.data_dir.join("cases"), &name, &bytes, &fid)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let case = get(tx, &id)?;
            if matches!(case.status.as_str(), "resolved" | "dismissed") {
                return Err(AppError::conflict("This case is finished."));
            }
            let ev = json!({ "file_id": fid, "file_name": name, "path": path.to_string_lossy(), "mime": mime, "sha256": sha });
            add_event(tx, &id, "evidence", None, None, None, Some(&ev), Some(&s.user_id), None)?;
            tx.execute("UPDATE cases SET updated_at=?2 WHERE case_id=?1", params![id, time::now_str()])?;
            audit::record(tx, &actor, "case.evidence_added", "case", Some(&id), None, Some(&json!({ "file": name, "sha256": sha })))?;
            Ok(())
        })?;
        self.case_get(token, &id)
    }

    pub fn case_evidence(&self, token: &str, case_id: &str, file_id: &str) -> AppResult<Value> {
        let detail = self.case_get(token, case_id)?;
        let ev = detail
            .events
            .iter()
            .filter_map(|e| e.evidence.as_ref())
            .find(|e| e["file_id"].as_str() == Some(file_id))
            .ok_or_else(|| AppError::not_found("File"))?;
        let bytes = std::fs::read(ev["path"].as_str().unwrap_or_default()).map_err(|_| AppError::not_found("File"))?;
        Ok(json!({ "file_name": ev["file_name"], "mime": ev["mime"], "base64": crate::ids::b64(&bytes) }))
    }
}
