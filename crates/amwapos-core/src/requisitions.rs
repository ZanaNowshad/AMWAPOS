//! Purchase requisitions: a request to buy, before any order exists
//! (docs/PROCUREMENT.md).
//!
//! draft → submitted → approved → converted, or rejected / cancelled.
//! Each line remembers where it came from: typed by a person (`manual`) or
//! suggested by the replenishment engine (`replenishment`, with the figures
//! the engine used). Conversion turns the whole requisition into draft
//! purchase orders, one per supplier, exactly once. Converting part of a
//! requisition is refused: split it first (cancel and create a new one).

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ids::{new_id, next_seq};
use crate::purchasing::{insert_draft_po, PoLineInput};
use crate::service::AppCore;
use crate::setup::clean_opt;
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReqLineInput {
    /// Keep an existing line (and its provenance) when editing.
    #[serde(default)]
    pub line_id: Option<String>,
    pub product_id: String,
    #[serde(default)]
    pub supplier_id: Option<String>,
    pub qty_milli: i64,
    #[serde(default)]
    pub unit_cost_minor: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReqInput {
    #[serde(default)]
    pub note: Option<String>,
    pub lines: Vec<ReqLineInput>,
    /// Creating: makes a retry safe. Editing: not used.
    #[serde(default)]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub expected_version: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FromSuggestions {
    pub product_ids: Vec<String>,
    #[serde(default)]
    pub branch_id: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    pub operation_id: String,
}

fn load(c: &Connection, id: &str, show_cost: bool) -> AppResult<Value> {
    type H = (
        String,
        String,
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        Option<String>,
    );
    let h: H = c
        .query_row(
            "SELECT r.requisition_id, r.number, r.status, r.note, r.created_at, r.branch_id, u.display_name, r.submitted_at, r.decided_at,
                    r.decision_note, r.converted_at, r.version, d.display_name
             FROM requisitions r LEFT JOIN users u ON u.user_id=r.created_by LEFT JOIN users d ON d.user_id=r.decided_by WHERE r.requisition_id=?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("Requisition"))?;
    let mut st = c.prepare(
        "SELECT l.line_id, l.line_no, l.product_id, p.name, p.name_ar, p.sku, p.allow_decimal_quantity, l.supplier_id, s.name, l.qty_milli, l.packs,
                l.units_per_case, l.unit_cost_minor, l.source, l.evidence_json, l.po_id, o.po_number
         FROM requisition_lines l JOIN products p ON p.product_id=l.product_id LEFT JOIN suppliers s ON s.supplier_id=l.supplier_id
         LEFT JOIN purchase_orders o ON o.po_id=l.po_id WHERE l.requisition_id=?1 ORDER BY l.line_no",
    )?;
    let lines: Vec<Value> = st
        .query_map([id], |r| {
            let cost: Option<i64> = r.get(12)?;
            let qty: i64 = r.get(9)?;
            Ok(json!({ "line_id": r.get::<_, String>(0)?, "line_no": r.get::<_, i64>(1)?, "product_id": r.get::<_, String>(2)?,
                "product_name": r.get::<_, String>(3)?, "product_name_ar": r.get::<_, Option<String>>(4)?, "sku": r.get::<_, String>(5)?,
                "allow_decimal_quantity": r.get::<_, i64>(6)? == 1, "supplier_id": r.get::<_, Option<String>>(7)?, "supplier_name": r.get::<_, Option<String>>(8)?,
                "qty_milli": qty, "packs": r.get::<_, Option<i64>>(10)?, "units_per_case": r.get::<_, Option<i64>>(11)?,
                "unit_cost_minor": if show_cost { json!(cost) } else { Value::Null },
                "line_total_minor": if show_cost { json!(cost.and_then(|c| crate::money::extend(c, qty).ok())) } else { Value::Null },
                "source": r.get::<_, String>(13)?,
                "evidence": r.get::<_, Option<String>>(14)?.and_then(|e| serde_json::from_str::<Value>(&e).ok()),
                "po_id": r.get::<_, Option<String>>(15)?, "po_number": r.get::<_, Option<String>>(16)? }))
        })?
        .collect::<Result<_, _>>()?;
    let total: i64 = lines.iter().filter_map(|l| l["line_total_minor"].as_i64()).sum();
    let mut st =
        c.prepare("SELECT po_id, po_number, status, total_minor FROM purchase_orders WHERE requisition_id=?1 ORDER BY po_number")?;
    let pos: Vec<Value> = st
        .query_map([id], |r| {
            Ok(json!({ "po_id": r.get::<_, String>(0)?, "po_number": r.get::<_, String>(1)?, "status": r.get::<_, String>(2)?,
                "total_minor": if show_cost { json!(r.get::<_, i64>(3)?) } else { Value::Null } }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(
        json!({ "requisition_id": h.0, "number": h.1, "status": h.2, "note": h.3, "created_at": h.4, "branch_id": h.5, "created_by_name": h.6,
        "submitted_at": h.7, "decided_at": h.8, "decision_note": h.9, "converted_at": h.10, "version": h.11, "decided_by_name": h.12,
        "lines": lines, "estimated_total_minor": if show_cost { json!(total) } else { Value::Null }, "purchase_orders": pos }),
    )
}

struct CleanLine {
    line_id: Option<String>,
    product_id: String,
    supplier_id: Option<String>,
    qty_milli: i64,
    unit_cost_minor: Option<i64>,
}

fn clean_lines(c: &Connection, lines: &[ReqLineInput]) -> AppResult<Vec<CleanLine>> {
    if lines.is_empty() {
        return Err(AppError::validation("Add at least one product to the requisition."));
    }
    if lines.len() > 1000 {
        return Err(AppError::validation("A requisition can have at most 1,000 lines."));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = vec![];
    for l in lines {
        let pid = validate::id(&l.product_id, "Product")?;
        if !seen.insert(pid.clone()) {
            return Err(AppError::validation("Each product can appear only once on a requisition."));
        }
        let (dec, name): (i64, String) = c
            .query_row("SELECT allow_decimal_quantity, name FROM products WHERE product_id=?1", [&pid], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
            .ok_or_else(|| AppError::not_found("Product"))?;
        validate::qty_positive(l.qty_milli, dec == 1, &format!("Quantity for {name}"))?;
        if let Some(cst) = l.unit_cost_minor {
            validate::money_non_negative(cst, &format!("Cost for {name}"))?;
        }
        let sid = l.supplier_id.as_deref().filter(|x| !x.is_empty()).map(|x| validate::id(x, "Supplier")).transpose()?;
        if let Some(sid) = &sid {
            c.query_row("SELECT 1 FROM suppliers WHERE supplier_id=?1", [sid], |_| Ok(()))
                .optional()?
                .ok_or_else(|| AppError::not_found("Supplier"))?;
        }
        out.push(CleanLine {
            line_id: l.line_id.clone().filter(|x| !x.is_empty()),
            product_id: pid,
            supplier_id: sid,
            qty_milli: l.qty_milli,
            unit_cost_minor: l.unit_cost_minor,
        });
    }
    Ok(out)
}

fn status_of(c: &Connection, id: &str) -> AppResult<(String, i64)> {
    c.query_row("SELECT status, version FROM requisitions WHERE requisition_id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?
        .ok_or_else(|| AppError::not_found("Requisition"))
}

impl AppCore {
    fn req_view(&self, token: &str, id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        let show_cost = s.has("products.view_cost");
        self.db.read(|c| load(c, id, show_cost))
    }

    pub fn requisitions_list(&self, token: &str, status: Option<String>) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("requisitions.create") && !s.has("purchasing.approve") && !s.has("purchasing.manage") {
            return Err(AppError::forbidden("requisitions.create"));
        }
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT r.requisition_id, r.number, r.status, r.note, r.created_at, u.display_name,
                        (SELECT COUNT(*) FROM requisition_lines l WHERE l.requisition_id=r.requisition_id),
                        (SELECT COUNT(DISTINCT l.supplier_id) FROM requisition_lines l WHERE l.requisition_id=r.requisition_id)
                 FROM requisitions r LEFT JOIN users u ON u.user_id=r.created_by
                 WHERE r.branch_id=?1 AND (?2 IS NULL OR r.status=?2) ORDER BY r.created_at DESC LIMIT 500",
            )?;
            let rows: Vec<Value> = st
                .query_map(params![s.branch_id, status.filter(|x| !x.is_empty())], |r| {
                    Ok(json!({ "requisition_id": r.get::<_, String>(0)?, "number": r.get::<_, String>(1)?, "status": r.get::<_, String>(2)?,
                        "note": r.get::<_, Option<String>>(3)?, "created_at": r.get::<_, String>(4)?, "created_by_name": r.get::<_, Option<String>>(5)?,
                        "line_count": r.get::<_, i64>(6)?, "supplier_count": r.get::<_, i64>(7)? }))
                })?
                .collect::<Result<_, _>>()?;
            Ok(json!({ "rows": rows }))
        })
    }

    pub fn requisition_get(&self, token: &str, requisition_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("requisitions.create") && !s.has("purchasing.approve") && !s.has("purchasing.manage") {
            return Err(AppError::forbidden("requisitions.create"));
        }
        let id = validate::id(requisition_id, "Requisition")?;
        self.req_view(token, &id)
    }

    /// A requisition typed by a person (lines are `manual`).
    pub fn requisition_create(&self, token: &str, input: ReqInput) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("requisitions.create")?;
        self.require_back_office_writable()?;
        let note = clean_opt(&input.note, "Note", 1000)?;
        let op = input.operation_id.clone().ok_or_else(|| AppError::validation("Missing operation id."))?;
        crate::idempotency::validate_operation_id(&op)?;
        let actor = self.actor(&s, None);
        let id = self.db.write(|tx| {
            if let Some(done) = tx.query_row("SELECT requisition_id FROM requisitions WHERE create_operation_id=?1", [&op], |r| r.get::<_, String>(0)).optional()? {
                return Ok(done);
            }
            let lines = clean_lines(tx, &input.lines)?;
            let id = new_id();
            let number = format!("REQ-{:05}", next_seq(tx, "requisition")?);
            let now = time::now_str();
            tx.execute(
                "INSERT INTO requisitions(requisition_id, number, branch_id, status, note, created_by, created_at, updated_at, create_operation_id)
                 VALUES (?1,?2,?3,'draft',?4,?5,?6,?6,?7)",
                params![id, number, s.branch_id, note, s.user_id, now, op],
            )?;
            for (i, l) in lines.iter().enumerate() {
                tx.execute(
                    "INSERT INTO requisition_lines(line_id, requisition_id, line_no, product_id, supplier_id, qty_milli, unit_cost_minor, source)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,'manual')",
                    params![new_id(), id, i as i64 + 1, l.product_id, l.supplier_id, l.qty_milli, l.unit_cost_minor],
                )?;
            }
            audit::record(tx, &actor, "requisition.created", "requisition", Some(&id), None, Some(&json!({ "number": number, "lines": lines.len(), "source": "manual" })))?;
            Ok(id)
        })?;
        self.req_view(token, &id)
    }

    /// A requisition from Suggested orders. The suggestions are recomputed
    /// here, on the server, at this moment: the request names products only,
    /// never quantities. Products the engine does not suggest ordering are
    /// listed as skipped with the reason.
    pub fn requisition_from_suggestions(&self, token: &str, input: FromSuggestions) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("requisitions.create")?;
        self.require_back_office_writable()?;
        crate::idempotency::validate_operation_id(&input.operation_id)?;
        let note = clean_opt(&input.note, "Note", 1000)?;
        if input.product_ids.is_empty() || input.product_ids.len() > 1000 {
            return Err(AppError::validation("Choose 1–1,000 suggested products."));
        }
        let ids: Vec<String> = input.product_ids.iter().map(|p| validate::id(p, "Product")).collect::<AppResult<_>>()?;
        let actor = self.actor(&s, None);
        let (id, skipped) = self.db.write(|tx| {
            if let Some(done) = tx
                .query_row("SELECT requisition_id FROM requisitions WHERE create_operation_id=?1", [&input.operation_id], |r| r.get::<_, String>(0))
                .optional()?
            {
                return Ok((done, vec![]));
            }
            let today = time::business_date(time::now(), &time::day(tx)?)?;
            let branch = crate::branches::report_scope(tx, &s, input.branch_id.as_deref())?.unwrap_or_else(|| s.branch_id.clone());
            if branch != s.branch_id {
                return Err(AppError::validation("Create requisitions in your own branch."));
            }
            let rows = crate::replenish::run(tx, &branch, &today, Some(&ids))?;
            let mut skipped = vec![];
            let mut take = vec![];
            for r in rows {
                if r.assessment.state == "order" && r.assessment.suggested_milli > 0 {
                    take.push(r);
                } else {
                    skipped.push(json!({ "product_id": r.product_id, "name": r.name, "state": r.assessment.state }));
                }
            }
            if take.is_empty() {
                return Err(AppError::validation("None of the chosen products needs ordering now.").with_details(json!({ "skipped": skipped })));
            }
            let id = new_id();
            let number = format!("REQ-{:05}", next_seq(tx, "requisition")?);
            let now = time::now_str();
            tx.execute(
                "INSERT INTO requisitions(requisition_id, number, branch_id, status, note, created_by, created_at, updated_at, create_operation_id)
                 VALUES (?1,?2,?3,'draft',?4,?5,?6,?6,?7)",
                params![id, number, branch, note, s.user_id, now, input.operation_id],
            )?;
            for (i, r) in take.iter().enumerate() {
                let a = &r.assessment;
                let sup = a.supplier.as_ref();
                let evidence = json!({ "today": today, "state": a.state, "reasons": a.reasons, "warnings": a.warnings, "facts": r.facts,
                    "usable_milli": a.usable_milli, "inbound_milli": a.inbound_milli, "position_milli": a.position_milli,
                    "per_day_milli": a.per_day_milli, "days_used": a.days_used, "reorder_point_milli": a.reorder_point_milli,
                    "reorder_point_source": a.reorder_point_source, "order_up_to_milli": a.order_up_to_milli, "order_up_to_source": a.order_up_to_source,
                    "need_milli": a.need_milli, "suggested_milli": a.suggested_milli, "supplier_reason": a.supplier_reason,
                    "alternatives": a.alternatives.iter().map(|x| json!({ "supplier_id": x.supplier_id, "supplier_name": x.supplier_name })).collect::<Vec<_>>() });
                tx.execute(
                    "INSERT INTO requisition_lines(line_id, requisition_id, line_no, product_id, supplier_id, qty_milli, packs, units_per_case, unit_cost_minor, source, evidence_json)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'replenishment',?10)",
                    params![
                        new_id(),
                        id,
                        i as i64 + 1,
                        r.product_id,
                        sup.map(|x| x.supplier_id.clone()),
                        a.suggested_milli,
                        a.packs,
                        sup.and_then(|x| x.units_per_case),
                        sup.and_then(|x| x.last_cost_minor),
                        evidence.to_string()
                    ],
                )?;
            }
            audit::record(tx, &actor, "requisition.created", "requisition", Some(&id), None,
                Some(&json!({ "number": number, "lines": take.len(), "source": "replenishment", "skipped": skipped.len() })))?;
            Ok((id, skipped))
        })?;
        let mut v = self.req_view(token, &id)?;
        v["skipped"] = json!(skipped);
        Ok(v)
    }

    /// Edit a draft requisition. A kept line keeps its provenance; a
    /// suggested line whose quantity or supplier a person changed is marked
    /// as edited in its evidence.
    pub fn requisition_save(&self, token: &str, requisition_id: &str, input: ReqInput) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("requisitions.create")?;
        self.require_back_office_writable()?;
        let id = validate::id(requisition_id, "Requisition")?;
        let note = clean_opt(&input.note, "Note", 1000)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let (status, version) = status_of(tx, &id)?;
            if status != "draft" {
                return Err(AppError::conflict("Only a draft requisition can be edited."));
            }
            if input.expected_version.is_some_and(|v| v != version) {
                return Err(AppError::conflict("Someone changed this requisition meanwhile. Reload and try again."));
            }
            let lines = clean_lines(tx, &input.lines)?;
            type Old = (String, String, Option<String>, i64, Option<i64>, Option<i64>, Option<i64>, String, Option<String>);
            let mut st = tx.prepare(
                "SELECT line_id, product_id, supplier_id, qty_milli, unit_cost_minor, packs, units_per_case, source, evidence_json FROM requisition_lines WHERE requisition_id=?1",
            )?;
            let old: BTreeMap<String, Old> = st
                .query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)))?
                .collect::<Result<Vec<Old>, _>>()?
                .into_iter()
                .map(|o| (o.0.clone(), o))
                .collect();
            drop(st);
            tx.execute("DELETE FROM requisition_lines WHERE requisition_id=?1", [&id])?;
            for (i, l) in lines.iter().enumerate() {
                let prev = l.line_id.as_ref().and_then(|lid| old.get(lid)).filter(|o| o.1 == l.product_id);
                let (source, evidence, packs, upc) = match prev {
                    Some(o) => {
                        let changed = o.2 != l.supplier_id || o.3 != l.qty_milli;
                        let ev = match (&o.8, changed) {
                            (Some(e), true) => serde_json::from_str::<Value>(e).ok().map(|mut v| {
                                v["edited"] = json!(true);
                                v.to_string()
                            }),
                            (e, _) => e.clone(),
                        };
                        (o.7.clone(), ev, if changed { None } else { o.5 }, if o.2 == l.supplier_id { o.6 } else { None })
                    }
                    None => ("manual".to_string(), None, None, None),
                };
                tx.execute(
                    "INSERT INTO requisition_lines(line_id, requisition_id, line_no, product_id, supplier_id, qty_milli, packs, units_per_case, unit_cost_minor, source, evidence_json)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        prev.map(|o| o.0.clone()).unwrap_or_else(new_id),
                        id,
                        i as i64 + 1,
                        l.product_id,
                        l.supplier_id,
                        l.qty_milli,
                        packs,
                        upc,
                        l.unit_cost_minor,
                        source,
                        evidence
                    ],
                )?;
            }
            tx.execute(
                "UPDATE requisitions SET note=?2, updated_at=?3, version=version+1 WHERE requisition_id=?1",
                params![id, note, time::now_str()],
            )?;
            audit::record(tx, &actor, "requisition.updated", "requisition", Some(&id), None, Some(&json!({ "lines": lines.len() })))?;
            Ok(())
        })?;
        self.req_view(token, &id)
    }

    /// submit (draft → submitted), approve / reject (submitted → …, needs
    /// `purchasing.approve`), cancel (draft, submitted or approved).
    pub fn requisition_set_status(&self, token: &str, requisition_id: &str, action: &str, note: Option<String>) -> AppResult<Value> {
        let s = self.session(token)?;
        let perm = match action {
            "submit" | "cancel" => "requisitions.create",
            "approve" | "reject" => "purchasing.approve",
            _ => return Err(AppError::validation("Unknown requisition action.")),
        };
        if action == "cancel" && s.has("purchasing.approve") {
            // Approvers may cancel too.
        } else {
            s.require(perm)?;
        }
        self.require_back_office_writable()?;
        let id = validate::id(requisition_id, "Requisition")?;
        let note = clean_opt(&note, "Note", 500)?;
        if action == "reject" && note.is_none() {
            return Err(AppError::validation("Say why the requisition is rejected."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let (cur, _) = status_of(tx, &id)?;
            let to = match (cur.as_str(), action) {
                ("draft", "submit") => "submitted",
                ("submitted", "approve") => "approved",
                ("submitted", "reject") => "rejected",
                ("draft" | "submitted" | "approved", "cancel") => "cancelled",
                (c, a) if c == action_target(a) => return Ok(()), // repeated click
                _ => return Err(AppError::conflict(format!("A requisition that is {cur} cannot be {}.", action_words(action)))),
            };
            if action == "submit" {
                let missing: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM requisition_lines WHERE requisition_id=?1 AND supplier_id IS NULL",
                    [&id],
                    |r| r.get(0),
                )?;
                if missing > 0 {
                    return Err(AppError::validation("Choose a supplier for every line before submitting."));
                }
            }
            let now = time::now_str();
            match to {
                "submitted" => tx.execute(
                    "UPDATE requisitions SET status='submitted', submitted_by=?2, submitted_at=?3, updated_at=?3, version=version+1 WHERE requisition_id=?1",
                    params![id, s.user_id, now],
                )?,
                "approved" | "rejected" => tx.execute(
                    "UPDATE requisitions SET status=?2, decided_by=?3, decided_at=?4, decision_note=?5, updated_at=?4, version=version+1 WHERE requisition_id=?1",
                    params![id, to, s.user_id, now, note],
                )?,
                _ => tx.execute(
                    "UPDATE requisitions SET status='cancelled', decision_note=COALESCE(?2, decision_note), updated_at=?3, version=version+1 WHERE requisition_id=?1",
                    params![id, note, now],
                )?,
            };
            audit::record(tx, &actor, &format!("requisition.{to}"), "requisition", Some(&id), Some(&json!({ "status": cur })),
                Some(&json!({ "status": to, "note": note })))?;
            Ok(())
        })?;
        self.req_view(token, &id)
    }

    /// Turn an approved requisition into draft purchase orders, one per
    /// supplier. Exactly once: repeating the same operation id returns the
    /// same orders; a second conversion is refused. The whole requisition is
    /// converted, or nothing.
    pub fn requisition_convert(&self, token: &str, requisition_id: &str, operation_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("purchasing.manage")?;
        self.require_back_office_writable()?;
        crate::idempotency::validate_operation_id(operation_id)?;
        let id = validate::id(requisition_id, "Requisition")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let (status, op, branch): (String, Option<String>, String) = tx
                .query_row("SELECT status, convert_operation_id, branch_id FROM requisitions WHERE requisition_id=?1", [&id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Requisition"))?;
            if status == "converted" {
                if op.as_deref() == Some(operation_id) {
                    return Ok(());
                }
                return Err(AppError::conflict("This requisition was already turned into purchase orders.").with_details(json!({ "kind": "already_converted" })));
            }
            let other: Option<String> =
                tx.query_row("SELECT requisition_id FROM requisitions WHERE convert_operation_id=?1", [operation_id], |r| r.get(0)).optional()?;
            if other.is_some() {
                return Err(AppError::new(ErrorCode::IdempotencyMismatch, "This operation id was used for another requisition."));
            }
            if status != "approved" {
                return Err(AppError::conflict("Only an approved requisition can be turned into purchase orders."));
            }
            if branch != s.branch_id {
                return Err(AppError::validation("Convert the requisition in its own branch."));
            }
            type L = (String, String, Option<String>, i64, Option<i64>, Option<String>, String, i64);
            let mut st = tx.prepare(
                "SELECT l.line_id, l.product_id, l.supplier_id, l.qty_milli, l.unit_cost_minor, l.po_item_id, p.name,
                        COALESCE((SELECT t.rate_bp FROM tax_rules t WHERE t.tax_rule_id=p.tax_rule_id), 0)
                 FROM requisition_lines l JOIN products p ON p.product_id=l.product_id WHERE l.requisition_id=?1 ORDER BY l.line_no",
            )?;
            let lines: Vec<L> = st
                .query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))?
                .collect::<Result<_, _>>()?;
            drop(st);
            if lines.iter().any(|l| l.5.is_some()) {
                return Err(AppError::conflict("Part of this requisition is already on a purchase order."));
            }
            let mut by_supplier: BTreeMap<String, Vec<(String, PoLineInput)>> = BTreeMap::new();
            for (lid, pid, sid, qty, cost, _, name, tax) in lines {
                let sid = sid.ok_or_else(|| AppError::validation(format!("{name}: choose a supplier.")))?;
                let active: i64 = tx.query_row("SELECT active FROM suppliers WHERE supplier_id=?1", [&sid], |r| r.get(0))?;
                if active != 1 {
                    return Err(AppError::validation(format!("{name}: the supplier is inactive. Choose another.")));
                }
                let cost = match cost {
                    Some(c) => c,
                    None => crate::catalogue::cost_baselines(tx, &sid, &pid)?
                        .last_confirmed_minor
                        .ok_or_else(|| AppError::validation(format!("{name}: enter the expected cost (no confirmed cost from this supplier yet).")))?,
                };
                by_supplier.entry(sid).or_default().push((lid, PoLineInput { product_id: pid, qty_milli: qty, unit_cost_minor: cost, tax_rate_bp: tax }));
            }
            let number: String = tx.query_row("SELECT number FROM requisitions WHERE requisition_id=?1", [&id], |r| r.get(0))?;
            let mut pos = vec![];
            for (sid, lines) in by_supplier {
                let ids: Vec<String> = lines.iter().map(|l| l.0.clone()).collect();
                let inputs: Vec<PoLineInput> = lines.into_iter().map(|l| l.1).collect();
                let note = format!("From requisition {number}");
                let po = insert_draft_po(tx, &s, &sid, None, Some(&note), None, &inputs, Some((&id, &ids)))?;
                pos.push(po);
            }
            let now = time::now_str();
            tx.execute(
                "UPDATE requisitions SET status='converted', converted_by=?2, converted_at=?3, convert_operation_id=?4, updated_at=?3, version=version+1 WHERE requisition_id=?1",
                params![id, s.user_id, now, operation_id],
            )?;
            audit::record(tx, &actor, "requisition.converted", "requisition", Some(&id), Some(&json!({ "status": "approved" })),
                Some(&json!({ "status": "converted", "purchase_orders": pos })))?;
            Ok(())
        })?;
        self.req_view(token, &id)
    }
}

fn action_target(a: &str) -> &'static str {
    match a {
        "submit" => "submitted",
        "approve" => "approved",
        "reject" => "rejected",
        _ => "cancelled",
    }
}

fn action_words(a: &str) -> &'static str {
    match a {
        "submit" => "submitted",
        "approve" => "approved",
        "reject" => "rejected",
        _ => "cancelled",
    }
}
