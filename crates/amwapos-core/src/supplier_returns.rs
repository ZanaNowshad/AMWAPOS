//! Returning goods to a supplier (docs/PROCUREMENT.md).
//!
//! draft → confirmed → credited; a draft can be cancelled; a confirmed
//! return is undone only by a reversal (compensating movements).
//! - Confirming writes exactly one `supplier_return` stock movement per line,
//!   out of the batch when the line names one (Wave 3: evidence).
//! - The expected credit (at the cost the goods came in) is kept apart from
//!   the supplier's actual credit note, which is posted in Payables. Linking
//!   the credit note moves no stock: the return already did.
//! - Goods refused at delivery are not a return: they never became stock
//!   (see procurement.rs).

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::idempotency::{self, Check};
use crate::ids::{new_id, next_seq};
use crate::inventory::{apply_movement_lot, avg_cost, current_qty, Movement};
use crate::money;
use crate::service::AppCore;
use crate::setup::clean_opt;
use crate::time;
use crate::validate;

pub const REASONS: [&str; 9] =
    ["damaged", "incorrect_item", "over_delivery", "short_dated", "expired", "quality", "recalled", "commercial", "other"];

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReturnLineInput {
    pub product_id: String,
    #[serde(default)]
    pub lot_id: Option<String>,
    pub qty_milli: i64,
    /// Defaults to the batch's cost, else the receipt's, else the average cost.
    #[serde(default)]
    pub unit_cost_minor: Option<i64>,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReturnInput {
    pub supplier_id: String,
    #[serde(default)]
    pub receipt_id: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    pub lines: Vec<ReturnLineInput>,
    #[serde(default)]
    pub operation_id: Option<String>,
}

fn load(c: &Connection, id: &str, show_cost: bool) -> AppResult<Value> {
    type H = (
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        i64,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
    );
    let h: H = c
        .query_row(
            "SELECT r.return_id, r.number, r.supplier_id, s.name, r.receipt_id, r.po_id, r.status, r.note, r.expected_credit_minor, r.credit_invoice_id,
                    r.created_at, r.confirmed_at, r.reversed_at, r.reversal_reason, r.version
             FROM supplier_returns r JOIN suppliers s ON s.supplier_id=r.supplier_id WHERE r.return_id=?1",
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
                    r.get(13)?,
                    r.get(14)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("Supplier return"))?;
    let mut st = c.prepare(
        "SELECT l.line_id, l.line_no, l.product_id, p.name, p.name_ar, p.sku, l.lot_id, k.lot_number, k.supplier_lot_code, l.qty_milli, l.unit_cost_minor,
                l.reason, l.movement_id, l.reversal_movement_id
         FROM supplier_return_lines l JOIN products p ON p.product_id=l.product_id LEFT JOIN stock_lots k ON k.lot_id=l.lot_id
         WHERE l.return_id=?1 ORDER BY l.line_no",
    )?;
    let lines: Vec<Value> = st
        .query_map([id], |r| {
            let qty: i64 = r.get(9)?;
            let cost: i64 = r.get(10)?;
            Ok(json!({ "line_id": r.get::<_, String>(0)?, "line_no": r.get::<_, i64>(1)?, "product_id": r.get::<_, String>(2)?,
                "product_name": r.get::<_, String>(3)?, "product_name_ar": r.get::<_, Option<String>>(4)?, "sku": r.get::<_, String>(5)?,
                "lot_id": r.get::<_, Option<String>>(6)?, "lot_number": r.get::<_, Option<String>>(7)?, "supplier_lot_code": r.get::<_, Option<String>>(8)?,
                "qty_milli": qty, "unit_cost_minor": if show_cost { json!(cost) } else { Value::Null },
                "value_minor": if show_cost { json!(money::extend(cost, qty).ok()) } else { Value::Null },
                "reason": r.get::<_, String>(11)?, "movement_id": r.get::<_, Option<String>>(12)?, "reversal_movement_id": r.get::<_, Option<String>>(13)? }))
        })?
        .collect::<Result<_, _>>()?;
    let credit: Option<(String, i64, String, Option<String>)> = match &h.9 {
        Some(inv) => c
            .query_row("SELECT number, total_minor, posting, invoice_number FROM supplier_invoices WHERE invoice_id=?1", [inv], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .optional()?,
        None => None,
    };
    let actual = credit.as_ref().filter(|x| x.2 == "posted").map(|x| x.1);
    Ok(
        json!({ "return_id": h.0, "number": h.1, "supplier_id": h.2, "supplier_name": h.3, "receipt_id": h.4, "po_id": h.5, "status": h.6, "note": h.7,
        "expected_credit_minor": if show_cost { json!(h.8) } else { Value::Null },
        "credit_invoice_id": h.9,
        "credit_note": credit.as_ref().map(|x| json!({ "number": x.0, "total_minor": if show_cost { json!(x.1) } else { Value::Null }, "posting": x.2, "supplier_number": x.3 })),
        "actual_credit_minor": if show_cost { json!(actual) } else { Value::Null },
        "credit_difference_minor": if show_cost { json!(actual.map(|a| a - h.8)) } else { Value::Null },
        "created_at": h.10, "confirmed_at": h.11, "reversed_at": h.12, "reversal_reason": h.13, "version": h.14, "lines": lines }),
    )
}

struct Clean {
    product_id: String,
    lot_id: Option<String>,
    qty: i64,
    cost: Option<i64>,
    reason: String,
}

fn clean_lines(lines: &[ReturnLineInput]) -> AppResult<Vec<Clean>> {
    if lines.is_empty() {
        return Err(AppError::validation("Add at least one product to return."));
    }
    if lines.len() > 500 {
        return Err(AppError::validation("A return can have at most 500 lines."));
    }
    lines
        .iter()
        .map(|l| {
            if !REASONS.contains(&l.reason.as_str()) {
                return Err(AppError::validation("Choose why the goods are returned."));
            }
            if l.qty_milli <= 0 {
                return Err(AppError::validation("A returned quantity must be more than zero."));
            }
            if l.unit_cost_minor.is_some_and(|c| c < 0) {
                return Err(AppError::validation("A cost cannot be negative."));
            }
            Ok(Clean {
                product_id: validate::id(&l.product_id, "Product")?,
                lot_id: l.lot_id.as_deref().filter(|x| !x.is_empty()).map(|x| validate::id(x, "Batch")).transpose()?,
                qty: l.qty_milli,
                cost: l.unit_cost_minor,
                reason: l.reason.clone(),
            })
        })
        .collect()
}

/// The unit cost for a returned line: the batch's, else what the receipt
/// paid, else the average cost.
fn line_cost(c: &Connection, l: &Clean, receipt: Option<&str>, branch: &str) -> AppResult<i64> {
    if let Some(x) = l.cost {
        return Ok(x);
    }
    if let Some(lot) = &l.lot_id {
        if let Some(x) = c.query_row("SELECT unit_cost_minor FROM stock_lots WHERE lot_id=?1", [lot], |r| r.get(0)).optional()? {
            return Ok(x);
        }
    }
    if let Some(rid) = receipt {
        if let Some(x) = c
            .query_row(
                "SELECT unit_cost_minor FROM goods_receipt_items WHERE receipt_id=?1 AND product_id=?2 LIMIT 1",
                params![rid, l.product_id],
                |r| r.get(0),
            )
            .optional()?
        {
            return Ok(x);
        }
    }
    avg_cost(c, &l.product_id, branch)
}

fn write_lines(c: &Connection, id: &str, lines: &[Clean], receipt: Option<&str>, branch: &str, dec_check: bool) -> AppResult<i64> {
    c.execute("DELETE FROM supplier_return_lines WHERE return_id=?1", [id])?;
    let mut total = 0i64;
    let mut seen = std::collections::HashSet::new();
    for (i, l) in lines.iter().enumerate() {
        if !seen.insert((l.product_id.clone(), l.lot_id.clone())) {
            return Err(AppError::validation("Each product (and batch) can appear only once on a return."));
        }
        let (dec, name, track): (i64, String, i64) = c
            .query_row("SELECT allow_decimal_quantity, name, track_inventory FROM products WHERE product_id=?1", [&l.product_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?
            .ok_or_else(|| AppError::not_found("Product"))?;
        if track != 1 {
            return Err(AppError::validation(format!("{name} does not track stock.")));
        }
        if dec_check {
            validate::qty_positive(l.qty, dec == 1, &format!("Quantity for {name}"))?;
        }
        if let Some(lot) = &l.lot_id {
            let ok: Option<(String, String)> = c
                .query_row("SELECT product_id, branch_id FROM stock_lots WHERE lot_id=?1", [lot], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            if ok.as_ref().map(|x| (x.0.as_str(), x.1.as_str())) != Some((l.product_id.as_str(), branch)) {
                return Err(AppError::validation(format!("{name}: the batch is not one of this product in this branch.")));
            }
        }
        let cost = line_cost(c, l, receipt, branch)?;
        total += money::extend(cost, l.qty)?;
        c.execute(
            "INSERT INTO supplier_return_lines(line_id, return_id, line_no, product_id, lot_id, qty_milli, unit_cost_minor, reason)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![new_id(), id, i as i64 + 1, l.product_id, l.lot_id, l.qty, cost, l.reason],
        )?;
    }
    Ok(total)
}

impl AppCore {
    fn ret_view(&self, token: &str, id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        let show = s.has("products.view_cost");
        self.db.read(|c| load(c, id, show))
    }

    pub fn supplier_returns_list(&self, token: &str, status: Option<String>, supplier_id: Option<String>) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("supplier_returns.manage") && !s.has("purchasing.manage") && !s.has("payables.view") {
            return Err(AppError::forbidden("supplier_returns.manage"));
        }
        let show = s.has("products.view_cost");
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT r.return_id, r.number, s.name, r.status, r.expected_credit_minor, r.created_at, r.confirmed_at, r.credit_invoice_id,
                        (SELECT COUNT(*) FROM supplier_return_lines l WHERE l.return_id=r.return_id)
                 FROM supplier_returns r JOIN suppliers s ON s.supplier_id=r.supplier_id
                 WHERE r.branch_id=?1 AND (?2 IS NULL OR r.status=?2) AND (?3 IS NULL OR r.supplier_id=?3) ORDER BY r.created_at DESC LIMIT 500",
            )?;
            let rows: Vec<Value> = st
                .query_map(params![s.branch_id, status.filter(|x| !x.is_empty()), supplier_id.filter(|x| !x.is_empty())], |r| {
                    Ok(json!({ "return_id": r.get::<_, String>(0)?, "number": r.get::<_, String>(1)?, "supplier_name": r.get::<_, String>(2)?,
                        "status": r.get::<_, String>(3)?, "expected_credit_minor": if show { json!(r.get::<_, i64>(4)?) } else { Value::Null },
                        "created_at": r.get::<_, String>(5)?, "confirmed_at": r.get::<_, Option<String>>(6)?,
                        "credit_invoice_id": r.get::<_, Option<String>>(7)?, "line_count": r.get::<_, i64>(8)? }))
                })?
                .collect::<Result<_, _>>()?;
            Ok(json!({ "rows": rows }))
        })
    }

    pub fn supplier_return_get(&self, token: &str, return_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("supplier_returns.manage") && !s.has("purchasing.manage") && !s.has("payables.view") {
            return Err(AppError::forbidden("supplier_returns.manage"));
        }
        let id = validate::id(return_id, "Supplier return")?;
        self.ret_view(token, &id)
    }

    /// Create (return_id None) or edit a draft return.
    pub fn supplier_return_save(&self, token: &str, return_id: Option<String>, input: ReturnInput) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("supplier_returns.manage")?;
        self.require_back_office_writable()?;
        let sid = validate::id(&input.supplier_id, "Supplier")?;
        let note = clean_opt(&input.note, "Note", 1000)?;
        let receipt = input.receipt_id.as_deref().filter(|x| !x.is_empty()).map(|x| validate::id(x, "Receipt")).transpose()?;
        let lines = clean_lines(&input.lines)?;
        let actor = self.actor(&s, None);
        let id = self.db.write(|tx| {
            tx.query_row("SELECT 1 FROM suppliers WHERE supplier_id=?1", [&sid], |_| Ok(())).optional()?.ok_or_else(|| AppError::not_found("Supplier"))?;
            let po: Option<String> = match &receipt {
                Some(r) => {
                    let (sup, po): (Option<String>, Option<String>) = tx
                        .query_row("SELECT supplier_id, po_id FROM goods_receipts WHERE receipt_id=?1", [r], |x| Ok((x.get(0)?, x.get(1)?)))
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Receipt"))?;
                    if sup.as_deref() != Some(sid.as_str()) {
                        return Err(AppError::validation("The receipt is from another supplier."));
                    }
                    po
                }
                None => None,
            };
            let now = time::now_str();
            let id = match &return_id {
                Some(id) => {
                    let id = validate::id(id, "Supplier return")?;
                    let st: String = tx
                        .query_row("SELECT status FROM supplier_returns WHERE return_id=?1", [&id], |r| r.get(0))
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Supplier return"))?;
                    if st != "draft" {
                        return Err(AppError::conflict("Only a draft return can be edited."));
                    }
                    tx.execute(
                        "UPDATE supplier_returns SET supplier_id=?2, receipt_id=?3, po_id=?4, note=?5, updated_at=?6, version=version+1 WHERE return_id=?1",
                        params![id, sid, receipt, po, note, now],
                    )?;
                    id
                }
                None => {
                    if let Some(op) = &input.operation_id {
                        idempotency::validate_operation_id(op)?;
                        if let Check::Replay { result } = idempotency::check(tx, op, "supplier_return.create", &input)? {
                            return Ok(result["return_id"].as_str().unwrap_or_default().to_string());
                        }
                    }
                    let id = new_id();
                    let number = format!("SR-{:05}", next_seq(tx, "supplier_return")?);
                    tx.execute(
                        "INSERT INTO supplier_returns(return_id, number, supplier_id, branch_id, receipt_id, po_id, status, note, created_by, created_at, updated_at)
                         VALUES (?1,?2,?3,?4,?5,?6,'draft',?7,?8,?9,?9)",
                        params![id, number, sid, s.branch_id, receipt, po, note, s.user_id, now],
                    )?;
                    if let Some(op) = &input.operation_id {
                        let hash = idempotency::payload_hash("supplier_return.create", &input)?;
                        idempotency::complete(tx, op, "supplier_return.create", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &json!({ "return_id": id }))?;
                    }
                    id
                }
            };
            let total = write_lines(tx, &id, &lines, receipt.as_deref(), &s.branch_id, true)?;
            tx.execute("UPDATE supplier_returns SET expected_credit_minor=?2 WHERE return_id=?1", params![id, total])?;
            audit::record(tx, &actor, if return_id.is_some() { "supplier_return.updated" } else { "supplier_return.created" }, "supplier_return", Some(&id), None,
                Some(&json!({ "supplier_id": sid, "lines": lines.len(), "expected_credit_minor": total })))?;
            Ok(id)
        })?;
        self.ret_view(token, &id)
    }

    pub fn supplier_return_cancel(&self, token: &str, return_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("supplier_returns.manage")?;
        self.require_back_office_writable()?;
        let id = validate::id(return_id, "Supplier return")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let st: String = tx
                .query_row("SELECT status FROM supplier_returns WHERE return_id=?1", [&id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Supplier return"))?;
            match st.as_str() {
                "cancelled" => return Ok(()),
                "draft" => {}
                _ => return Err(AppError::conflict("Only a draft return can be cancelled. A confirmed return is reversed instead.")),
            }
            tx.execute(
                "UPDATE supplier_returns SET status='cancelled', updated_at=?2, version=version+1 WHERE return_id=?1",
                params![id, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "supplier_return.cancelled",
                "supplier_return",
                Some(&id),
                Some(&json!({ "status": "draft" })),
                Some(&json!({ "status": "cancelled" })),
            )?;
            Ok(())
        })?;
        self.ret_view(token, &id)
    }

    /// Confirm a draft return: the goods leave stock (one `supplier_return`
    /// movement per line, from the batch when named). Checked inside the
    /// same transaction as the movements, so a sale or another stock change
    /// at the same moment cannot make it return stock that is not there.
    pub fn supplier_return_confirm(&self, token: &str, return_id: &str, operation_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("supplier_returns.manage")?;
        self.require_back_office_writable()?;
        idempotency::validate_operation_id(operation_id)?;
        let id = validate::id(return_id, "Supplier return")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let (st, op, receipt, branch): (String, Option<String>, Option<String>, String) = tx
                .query_row("SELECT status, confirm_operation_id, receipt_id, branch_id FROM supplier_returns WHERE return_id=?1", [&id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Supplier return"))?;
            if st != "draft" {
                if op.as_deref() == Some(operation_id) {
                    return Ok(());
                }
                return Err(AppError::conflict(format!("This return is already {st}.")));
            }
            let other: Option<String> =
                tx.query_row("SELECT return_id FROM supplier_returns WHERE confirm_operation_id=?1", [operation_id], |r| r.get(0)).optional()?;
            if other.is_some() {
                return Err(AppError::new(ErrorCode::IdempotencyMismatch, "This operation id was used for another return."));
            }
            if branch != s.branch_id {
                return Err(AppError::validation("Confirm the return in its own branch."));
            }
            type L = (String, String, Option<String>, i64, i64, String);
            let mut q = tx.prepare(
                "SELECT l.line_id, l.product_id, l.lot_id, l.qty_milli, l.unit_cost_minor, p.name FROM supplier_return_lines l
                 JOIN products p ON p.product_id=l.product_id WHERE l.return_id=?1 ORDER BY l.line_no",
            )?;
            let lines: Vec<L> = q.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<Result<_, _>>()?;
            drop(q);
            if lines.is_empty() {
                return Err(AppError::validation("Add at least one product to return."));
            }
            for (lid, pid, lot, qty, cost, name) in &lines {
                let on_hand = current_qty(tx, pid, &branch)?;
                if *qty > on_hand {
                    return Err(AppError::validation(format!("{name}: only {} in stock; cannot return {}.", money::format_qty(on_hand), money::format_qty(*qty))));
                }
                if let Some(lot) = lot {
                    let pl = crate::lots::replay(tx, pid, &branch)?;
                    let bal = pl.lots.iter().find(|x| &x.facts.lot_id == lot).map(|x| x.balance_milli).unwrap_or(0);
                    if *qty > bal {
                        return Err(AppError::validation(format!("{name}: the batch has {} left; cannot return {}.", money::format_qty(bal), money::format_qty(*qty))));
                    }
                }
                if let Some(rid) = &receipt {
                    let got: i64 = tx.query_row(
                        "SELECT COALESCE(SUM(qty_milli),0) FROM goods_receipt_items WHERE receipt_id=?1 AND product_id=?2",
                        params![rid, pid],
                        |r| r.get(0),
                    )?;
                    let returned: i64 = tx.query_row(
                        "SELECT COALESCE(SUM(l.qty_milli),0) FROM supplier_return_lines l JOIN supplier_returns r ON r.return_id=l.return_id
                         WHERE r.receipt_id=?1 AND l.product_id=?2 AND r.status IN ('confirmed','credited') AND r.return_id<>?3",
                        params![rid, pid, id],
                        |r| r.get(0),
                    )?;
                    if *qty > got - returned {
                        return Err(AppError::validation(format!(
                            "{name}: the receipt accepted {}, and {} was already returned.",
                            money::format_qty(got),
                            money::format_qty(returned)
                        )));
                    }
                }
                let (_, mid) = apply_movement_lot(
                    tx,
                    &Movement {
                        product_id: pid,
                        branch_id: &branch,
                        kind: "supplier_return",
                        qty_delta_milli: -qty,
                        unit_cost_minor: Some(*cost),
                        source_type: "supplier_return",
                        source_id: Some(&id),
                        reason: None,
                        user_id: Some(&s.user_id),
                        device_id: Some(&s.device_id),
                    },
                    None,
                    lot.as_deref(),
                )?;
                tx.execute("UPDATE supplier_return_lines SET movement_id=?2 WHERE line_id=?1", params![lid, mid])?;
            }
            let now = time::now_str();
            tx.execute(
                "UPDATE supplier_returns SET status='confirmed', confirmed_by=?2, confirmed_at=?3, confirm_operation_id=?4, updated_at=?3, version=version+1 WHERE return_id=?1",
                params![id, s.user_id, now, operation_id],
            )?;
            audit::record(tx, &actor, "supplier_return.confirmed", "supplier_return", Some(&id), Some(&json!({ "status": "draft" })),
                Some(&json!({ "status": "confirmed", "lines": lines.len() })))?;
            Ok(())
        })?;
        self.ret_view(token, &id)
    }

    /// Undo a confirmed return that was not credited: compensating movements
    /// bring the goods back (into the same batch). The return stays, marked
    /// reversed.
    pub fn supplier_return_reverse(&self, token: &str, return_id: &str, reason: &str, operation_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("supplier_returns.manage")?;
        self.require_back_office_writable()?;
        idempotency::validate_operation_id(operation_id)?;
        let id = validate::id(return_id, "Supplier return")?;
        let reason = reason.trim().chars().take(300).collect::<String>();
        if reason.is_empty() {
            return Err(AppError::validation("Say why the return is reversed."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let (st, op, branch): (String, Option<String>, String) = tx
                .query_row("SELECT status, reverse_operation_id, branch_id FROM supplier_returns WHERE return_id=?1", [&id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Supplier return"))?;
            match st.as_str() {
                "reversed" if op.as_deref() == Some(operation_id) => return Ok(()),
                "confirmed" => {}
                "credited" => return Err(AppError::conflict("The supplier already credited this return. Reverse the credit note in Payables first.")),
                _ => return Err(AppError::conflict(format!("A return that is {st} cannot be reversed."))),
            }
            if branch != s.branch_id {
                return Err(AppError::validation("Reverse the return in its own branch."));
            }
            let mut q = tx.prepare("SELECT line_id, product_id, lot_id, qty_milli, unit_cost_minor FROM supplier_return_lines WHERE return_id=?1 ORDER BY line_no")?;
            let lines: Vec<(String, String, Option<String>, i64, i64)> =
                q.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<_, _>>()?;
            drop(q);
            for (lid, pid, lot, qty, cost) in lines {
                let (_, mid) = apply_movement_lot(
                    tx,
                    &Movement {
                        product_id: &pid,
                        branch_id: &branch,
                        kind: "supplier_return",
                        qty_delta_milli: qty,
                        unit_cost_minor: Some(cost),
                        source_type: "supplier_return_reversal",
                        source_id: Some(&id),
                        reason: Some(&reason),
                        user_id: Some(&s.user_id),
                        device_id: Some(&s.device_id),
                    },
                    None,
                    lot.as_deref(),
                )?;
                tx.execute("UPDATE supplier_return_lines SET reversal_movement_id=?2 WHERE line_id=?1", params![lid, mid])?;
            }
            let now = time::now_str();
            tx.execute(
                "UPDATE supplier_returns SET status='reversed', reversed_by=?2, reversed_at=?3, reversal_reason=?4, reverse_operation_id=?5, updated_at=?3, version=version+1
                 WHERE return_id=?1",
                params![id, s.user_id, now, reason, operation_id],
            )?;
            audit::record(tx, &actor, "supplier_return.reversed", "supplier_return", Some(&id), Some(&json!({ "status": "confirmed" })),
                Some(&json!({ "status": "reversed", "reason": reason })))?;
            Ok(())
        })?;
        self.ret_view(token, &id)
    }

    /// Draft the supplier's credit note in Payables from a confirmed return
    /// (at the expected amounts), linked to it. Posting it there credits the
    /// return. It moves no stock.
    pub fn supplier_return_draft_credit(&self, token: &str, return_id: &str, supplier_number: &str, date: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("supplier_returns.manage")?;
        if !s.has("payables.review") {
            s.require("purchasing.manage")?;
        }
        self.require_back_office_writable()?;
        let id = validate::id(return_id, "Supplier return")?;
        let number = supplier_number.trim().chars().take(60).collect::<String>();
        if number.is_empty() {
            return Err(AppError::validation("Enter the supplier's credit note number."));
        }
        time::validate_date(date)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let (st, sid, link, expected, num): (String, String, Option<String>, i64, String) = tx
                .query_row(
                    "SELECT status, supplier_id, credit_invoice_id, expected_credit_minor, number FROM supplier_returns WHERE return_id=?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Supplier return"))?;
            if link.is_some() {
                return Err(AppError::conflict("A credit note is already linked to this return."));
            }
            if st != "confirmed" {
                return Err(AppError::conflict("Confirm the return before its credit note."));
            }
            if expected <= 0 {
                return Err(AppError::validation("The return has no value to credit."));
            }
            let inv = new_id();
            let seq = format!("SI-{:05}", next_seq(tx, "supplier_invoice")?);
            let now = time::now_str();
            tx.execute(
                "INSERT INTO supplier_invoices(invoice_id, number, doc_type, supplier_id, scan_id, invoice_number, invoice_date, subtotal_minor, vat_minor,
                    total_minor, status, posting, notes, created_by, created_at, updated_at, source)
                 VALUES (?1,?2,'credit_note',?3,NULL,?4,?5,?6,0,?6,'draft','not_posted',?7,?8,?9,?9,'manual')",
                params![inv, seq, sid, number, date, expected, format!("Credit for supplier return {num}"), s.user_id, now],
            )?;
            let mut q = tx.prepare("SELECT l.product_id, p.name, l.qty_milli, l.unit_cost_minor FROM supplier_return_lines l JOIN products p ON p.product_id=l.product_id WHERE l.return_id=?1 ORDER BY l.line_no")?;
            let lines: Vec<(String, String, i64, i64)> = q.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
            drop(q);
            for (i, (pid, name, qty, cost)) in lines.into_iter().enumerate() {
                tx.execute(
                    "INSERT INTO supplier_invoice_lines(invoice_id, line_no, product_id, description, qty_milli, unit_cost_minor, line_total_minor) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![inv, i as i64 + 1, pid, name, qty, cost, money::extend(cost, qty)?],
                )?;
            }
            tx.execute("UPDATE supplier_returns SET credit_invoice_id=?2, updated_at=?3, version=version+1 WHERE return_id=?1", params![id, inv, now])?;
            audit::record(tx, &actor, "supplier_return.credit_drafted", "supplier_return", Some(&id), None, Some(&json!({ "credit_invoice_id": inv, "amount_minor": expected })))?;
            Ok(())
        })?;
        self.ret_view(token, &id)
    }

    /// Link a credit note already entered in Payables to a confirmed return.
    /// One credit note per return and one return per credit note.
    pub fn supplier_return_link_credit(&self, token: &str, return_id: &str, invoice_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("supplier_returns.manage")?;
        self.require_back_office_writable()?;
        let id = validate::id(return_id, "Supplier return")?;
        let inv = validate::id(invoice_id, "Credit note")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let (st, sid, link): (String, String, Option<String>) = tx
                .query_row("SELECT status, supplier_id, credit_invoice_id FROM supplier_returns WHERE return_id=?1", [&id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Supplier return"))?;
            if link.as_deref() == Some(inv.as_str()) {
                return Ok(());
            }
            if link.is_some() {
                return Err(AppError::conflict("A credit note is already linked to this return."));
            }
            if st != "confirmed" {
                return Err(AppError::conflict("Only a confirmed return can be linked to a credit note."));
            }
            let (kind, isup, status, posting): (String, String, String, String) = tx
                .query_row("SELECT doc_type, supplier_id, status, posting FROM supplier_invoices WHERE invoice_id=?1", [&inv], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Credit note"))?;
            if kind != "credit_note" || isup != sid || status == "void" || posting == "reversed" {
                return Err(AppError::validation("Choose a credit note of the same supplier that is not void or reversed."));
            }
            let taken: Option<String> =
                tx.query_row("SELECT number FROM supplier_returns WHERE credit_invoice_id=?1", [&inv], |r| r.get(0)).optional()?;
            if let Some(t) = taken {
                return Err(AppError::conflict(format!("This credit note is already linked to return {t}.")));
            }
            let to = if posting == "posted" { "credited" } else { "confirmed" };
            tx.execute(
                "UPDATE supplier_returns SET credit_invoice_id=?2, status=?3, updated_at=?4, version=version+1 WHERE return_id=?1",
                params![id, inv, to, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "supplier_return.credit_linked",
                "supplier_return",
                Some(&id),
                None,
                Some(&json!({ "credit_invoice_id": inv, "status": to })),
            )?;
            Ok(())
        })?;
        self.ret_view(token, &id)
    }
}
