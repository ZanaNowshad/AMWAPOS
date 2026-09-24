//! Refunds: validated against remaining refundable quantity, prorated
//! exactly from the original sale lines, restocked through the ledger.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::{new_id, next_seq};
use crate::inventory::{self, Movement};
use crate::money;
use crate::pricing;
use crate::printing::PrintOutcome;
use crate::sales::{load_sale_detail, open_shift_for, shift_required};
use crate::service::AppCore;
use crate::setup::clean;
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RefundLineInput {
    pub sale_item_id: String,
    pub qty_milli: i64,
    #[serde(default = "yes")]
    pub restock: bool,
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RefundTenderInput {
    pub method: String,
    pub amount_minor: i64,
    #[serde(default)]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RefundRequest {
    pub sale_id: String,
    pub lines: Vec<RefundLineInput>,
    pub reason: String,
    /// If empty, the refund goes back to cash when the sale had cash,
    /// otherwise to the sale's first payment method.
    #[serde(default)]
    pub tenders: Vec<RefundTenderInput>,
    pub operation_id: String,
    #[serde(default, skip_serializing)]
    pub approval_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefundPreviewLine {
    pub sale_item_id: String,
    pub name: String,
    pub qty_milli: i64,
    pub amount_minor: i64,
    pub tax_minor: i64,
    pub restock: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefundPreview {
    pub lines: Vec<RefundPreviewLine>,
    pub subtotal_minor: i64,
    pub tax_minor: i64,
    pub total_minor: i64,
    pub tenders: Vec<RefundTenderInput>,
    pub requires_approval: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefundResult {
    pub refund_id: String,
    pub refund_receipt_number: String,
    pub total_minor: i64,
    pub tax_minor: i64,
    pub tenders: Vec<RefundTenderInput>,
    pub created_at: String,
    #[serde(default)]
    pub replayed: bool,
    #[serde(default)]
    pub print: Option<PrintOutcome>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefundRow {
    pub refund_id: String,
    pub refund_receipt_number: String,
    pub original_receipt_number: String,
    pub original_sale_id: String,
    pub created_at: String,
    pub user_name: Option<String>,
    pub approver_name: Option<String>,
    pub reason: String,
    pub total_minor: i64,
    pub methods: String,
}

struct Computed {
    lines: Vec<(RefundPreviewLine, Option<String>, i64, bool)>, // preview, product_id, cost, track
    subtotal: i64,
    tax: i64,
    total: i64,
    cost: i64,
}

fn compute(c: &rusqlite::Connection, sale_id: &str, lines: &[RefundLineInput]) -> AppResult<Computed> {
    if lines.is_empty() {
        return Err(AppError::validation("Select at least one item to refund."));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Computed { lines: vec![], subtotal: 0, tax: 0, total: 0, cost: 0 };
    for l in lines {
        let iid = validate::id(&l.sale_item_id, "Sale line")?;
        if !seen.insert(iid.clone()) {
            return Err(AppError::validation("Each sale line can appear only once in a refund."));
        }
        let (sid, pid, name, qty, total, tax, cost, is_custom, dec, track): (String, Option<String>, String, i64, i64, i64, i64, i64, i64, i64) = c
            .query_row(
                "SELECT i.sale_id, i.product_id, i.product_name_snapshot, i.qty_milli, i.line_total_minor, i.tax_minor, i.cost_snapshot_minor,
                        i.is_custom, COALESCE(p.allow_decimal_quantity,1), COALESCE(p.track_inventory,0)
                 FROM sale_items i LEFT JOIN products p ON p.product_id=i.product_id WHERE i.sale_item_id=?1",
                [&iid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?)),
            )
            .optional()?
            .ok_or_else(|| AppError::not_found("Sale line"))?;
        if sid != sale_id {
            return Err(AppError::validation("A refund line does not belong to this sale."));
        }
        validate::qty_positive(l.qty_milli, dec == 1 || is_custom == 1, &format!("Refund quantity for {name}"))?;
        let (rq, ramt, rtax, rcost): (i64, i64, i64, i64) = c.query_row(
            "SELECT COALESCE(SUM(qty_milli),0), COALESCE(SUM(amount_minor),0), COALESCE(SUM(tax_minor),0), COALESCE(SUM(cost_minor),0)
             FROM refund_items WHERE original_sale_item_id=?1",
            [&iid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        if l.qty_milli > qty - rq {
            return Err(AppError::validation(format!(
                "{name}: only {} can still be refunded (sold {}, already refunded {}).",
                money::format_qty(qty - rq),
                money::format_qty(qty),
                money::format_qty(rq)
            )));
        }
        let amount = pricing::prorate(total, qty, ramt, rq, l.qty_milli)?;
        let tax_amt = pricing::prorate(tax, qty, rtax, rq, l.qty_milli)?;
        let cost_amt = pricing::prorate(cost, qty, rcost, rq, l.qty_milli)?;
        out.total += amount;
        out.tax += tax_amt;
        out.cost += cost_amt;
        out.lines.push((
            RefundPreviewLine {
                sale_item_id: iid,
                name,
                qty_milli: l.qty_milli,
                amount_minor: amount,
                tax_minor: tax_amt,
                restock: l.restock && pid.is_some(),
            },
            pid,
            cost_amt,
            track == 1,
        ));
    }
    out.subtotal = out.total - out.tax;
    Ok(out)
}

impl AppCore {
    /// Compute a refund without writing anything.
    pub fn refund_preview(&self, token: &str, req: RefundRequest) -> AppResult<RefundPreview> {
        let s = self.session(token)?;
        let sid = validate::id(&req.sale_id, "Sale")?;
        self.db.read(|c| {
            let comp = compute(c, &sid, &req.lines)?;
            let tenders = default_tenders(c, &sid, comp.total, &req.tenders)?;
            Ok(RefundPreview {
                lines: comp.lines.into_iter().map(|x| x.0).collect(),
                subtotal_minor: comp.subtotal,
                tax_minor: comp.tax,
                total_minor: comp.total,
                tenders,
                requires_approval: !s.has("refund.create"),
            })
        })
    }

    pub fn refund_create(&self, token: &str, req: RefundRequest) -> AppResult<RefundResult> {
        let s = self.session(token)?;
        let sid = validate::id(&req.sale_id, "Sale")?;
        let reason = clean(&req.reason, "Reason", 200, true)?;
        idempotency::validate_operation_id(&req.operation_id)?;
        if let Check::Replay { result } = self.db.read(|c| idempotency::check(c, &req.operation_id, "refund.create", &req))? {
            let mut r: RefundResult = serde_json::from_value(result)?;
            r.replayed = true;
            r.print = self.db.read(|c| crate::printing::latest_job_outcome(c, "refund", &r.refund_id))?;
            return Ok(r);
        }
        let rn_for_summary: String = self.db.read(|c| {
            c.query_row("SELECT receipt_number FROM sales WHERE sale_id=?1", [&sid], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Sale"))
        })?;
        let approved =
            self.authorize(&s, "refund.create", req.approval_token.as_deref(), &format!("Refund on receipt {rn_for_summary}"))?;
        let device = self.require_device()?;
        let actor = self.actor(&s, approved.clone());
        let result = self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "refund.create", &req)? {
                Check::Replay { result } => {
                    let mut r: RefundResult = serde_json::from_value(result)?;
                    r.replayed = true;
                    return Ok(r);
                }
                Check::New { payload_hash } => payload_hash,
            };
            let shift_id = open_shift_for(tx, &s)?.ok_or_else(shift_required)?;
            let comp = compute(tx, &sid, &req.lines)?;
            let tenders = default_tenders(tx, &sid, comp.total, &req.tenders)?;
            let tz = self.store_timezone(tx)?;
            let now = time::now();
            let now_s = time::fmt(now);
            let rid = new_id();
            let rnum = format!("{}-R{:06}", device.device_code, next_seq(tx, &format!("refund:{}", device.device_id))?);
            tx.execute(
                "INSERT INTO refunds(refund_id, refund_receipt_number, original_sale_id, branch_id, device_id, shift_id, user_id, approved_by, reason,
                    subtotal_minor, tax_minor, total_minor, cost_total_minor, operation_id, business_date, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![
                    rid, rnum, sid, s.branch_id, s.device_id, shift_id, s.user_id, approved, reason, comp.subtotal, comp.tax, comp.total,
                    comp.cost, req.operation_id, time::business_date(now, &tz)?, now_s
                ],
            )?;
            for (pl, pid, cost, track) in &comp.lines {
                tx.execute(
                    "INSERT INTO refund_items(refund_item_id, refund_id, original_sale_item_id, product_id, qty_milli, amount_minor, tax_minor, cost_minor, restock)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![new_id(), rid, pl.sale_item_id, pid, pl.qty_milli, pl.amount_minor, pl.tax_minor, cost, pl.restock as i64],
                )?;
                if pl.restock && *track {
                    if let Some(pid) = pid {
                        let unit_cost = money::div_round(*cost as i128 * 1000, pl.qty_milli as i128) as i64;
                        inventory::apply_movement(
                            tx,
                            &Movement {
                                product_id: pid,
                                branch_id: &s.branch_id,
                                kind: "refund",
                                qty_delta_milli: pl.qty_milli,
                                unit_cost_minor: Some(unit_cost),
                                source_type: "refund",
                                source_id: Some(&rid),
                                reason: Some(&reason),
                                user_id: Some(&s.user_id),
                                device_id: Some(&s.device_id),
                            },
                        )?;
                    }
                }
            }
            for t in &tenders {
                tx.execute(
                    "INSERT INTO refund_tenders(refund_tender_id, refund_id, method, amount_minor, reference) VALUES (?1,?2,?3,?4,?5)",
                    params![new_id(), rid, t.method, t.amount_minor, t.reference],
                )?;
            }
            crate::printing::enqueue(tx, "refund", &rid, None, Some(&s.user_id))?;
            if tenders.iter().any(|t| t.method == "cash") {
                crate::printing::enqueue_drawer_pulse(tx, Some(&s.user_id), &rid)?;
            }
            let result = RefundResult {
                refund_id: rid.clone(),
                refund_receipt_number: rnum.clone(),
                total_minor: comp.total,
                tax_minor: comp.tax,
                tenders: tenders.clone(),
                created_at: now_s,
                replayed: false,
                print: None,
            };
            audit::record(tx, &actor, "refund.created", "refund", Some(&rid), None, Some(&json!({
                "refund_receipt_number": rnum, "original_sale_id": sid, "total_minor": comp.total, "reason": reason,
                "lines": comp.lines.iter().map(|l| json!({"sale_item_id": l.0.sale_item_id, "qty_milli": l.0.qty_milli, "restock": l.0.restock})).collect::<Vec<_>>()
            })))?;
            idempotency::complete(tx, &req.operation_id, "refund.create", Some(&s.user_id), Some(&s.device_id), &hash, Some(&rid), &serde_json::to_value(&result)?)?;
            Ok(result)
        })?;
        let mut result = result;
        if !result.replayed {
            self.print_pending_for(&result.refund_id);
        }
        result.print = self.db.read(|c| crate::printing::latest_job_outcome(c, "refund", &result.refund_id)).unwrap_or(None);
        Ok(result)
    }

    pub fn refunds_list(&self, token: &str, from: Option<String>, to: Option<String>, limit: Option<i64>) -> AppResult<Vec<RefundRow>> {
        let s = self.session(token)?;
        s.require("sales.view")?;
        let limit = validate::limit(limit, 200, 1000);
        self.db.read(|c| {
            let tz = self.store_timezone(c)?;
            let (a, b) = match (&from, &to) {
                (None, None) => ("0".to_string(), "9".to_string()),
                _ => time::local_date_range_utc(from.as_deref().unwrap_or("2000-01-01"), to.as_deref().unwrap_or("2999-12-31"), &tz)?,
            };
            let mut st = c.prepare(&format!(
                "SELECT r.refund_id, r.refund_receipt_number, s.receipt_number, r.original_sale_id, r.created_at, u.display_name, a.display_name,
                        r.reason, r.total_minor, COALESCE((SELECT group_concat(method) FROM refund_tenders t WHERE t.refund_id=r.refund_id),'')
                 FROM refunds r JOIN sales s ON s.sale_id=r.original_sale_id LEFT JOIN users u ON u.user_id=r.user_id
                 LEFT JOIN users a ON a.user_id=r.approved_by WHERE r.created_at>=?1 AND r.created_at<?2
                 ORDER BY r.created_at DESC LIMIT {limit}"
            ))?;
            let rows = st
                .query_map(params![a, b], |r| {
                    Ok(RefundRow {
                        refund_id: r.get(0)?,
                        refund_receipt_number: r.get(1)?,
                        original_receipt_number: r.get(2)?,
                        original_sale_id: r.get(3)?,
                        created_at: r.get(4)?,
                        user_name: r.get(5)?,
                        approver_name: r.get(6)?,
                        reason: r.get(7)?,
                        total_minor: r.get(8)?,
                        methods: r.get(9)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Sale details for the refund screen (includes refunded quantities).
    pub fn refund_lookup(&self, token: &str, receipt_number: &str) -> AppResult<crate::sales::SaleDetail> {
        let s = self.session(token)?;
        let rn = clean(receipt_number, "Receipt number", 40, true)?;
        self.db.read(|c| {
            let sid: String = c
                .query_row("SELECT sale_id FROM sales WHERE receipt_number=?1 COLLATE NOCASE", [&rn], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::new(crate::ErrorCode::NotFound, format!("No sale with receipt number {rn} was found.")))?;
            load_sale_detail(c, &sid, s.has("products.view_cost"))
        })
    }
}

fn default_tenders(c: &rusqlite::Connection, sale_id: &str, total: i64, given: &[RefundTenderInput]) -> AppResult<Vec<RefundTenderInput>> {
    let methods: Vec<(String, i64)> = {
        let mut st = c.prepare("SELECT method, SUM(amount_minor) FROM payments WHERE sale_id=?1 GROUP BY method")?;
        let rows = st.query_map([sale_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    if given.is_empty() {
        if total == 0 {
            return Ok(vec![]);
        }
        let method = if methods.iter().any(|m| m.0 == "cash") || methods.is_empty() { "cash".to_string() } else { methods[0].0.clone() };
        return Ok(vec![RefundTenderInput { method, amount_minor: total, reference: None }]);
    }
    let mut sum = 0;
    for t in given {
        if t.amount_minor <= 0 {
            return Err(AppError::validation("Each refund amount must be greater than zero."));
        }
        if t.method != "cash" && !methods.iter().any(|m| m.0 == t.method) {
            return Err(AppError::validation(format!(
                "The original sale was not paid by {}. Refund to cash or an original payment method.",
                crate::receipt::method_label(&t.method)
            )));
        }
        if let Some(r) = &t.reference {
            clean(r, "Reference", 60, false)?;
        }
        sum += t.amount_minor;
    }
    if sum != total {
        return Err(AppError::validation("Refund payments must add up exactly to the refund total."));
    }
    Ok(given.to_vec())
}
