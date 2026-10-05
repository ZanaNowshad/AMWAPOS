//! Completed-sale void: the whole sale cancelled the same trading day, before
//! its shift closes — the "wrong customer / rang it twice / walked away"
//! correction made at the till. It is not a refund:
//!
//! * only the whole sale, only on the trading day it was made, only while the
//!   shift that took the money is still open (so the drawer still holds it);
//! * never after a refund, a delivery or a cash collection against the sale;
//! * a cashier needs a manager approval bound to this exact sale;
//! * the original sale is never changed; `sale_voids` records who, why, when,
//!   who approved and which reversal it produced.
//!
//! The reversal itself (VAT, tenders back exactly as paid, restocking with
//! `void` movements, customer account, loyalty, frozen receipt) is written by
//! the same code as a refund, so the arithmetic exists once. Anything outside
//! these rules is a refund.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::new_id;
use crate::refunds::{self, RefundLineInput, RefundResult, RefundTenderInput, Reversal};
use crate::sales::{open_shift_for, shift_required};
use crate::service::AppCore;
use crate::setup::clean;
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VoidRequest {
    pub sale_id: String,
    pub reason: String,
    pub operation_id: String,
    #[serde(default, skip_serializing)]
    pub approval_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoidResult {
    pub void_id: String,
    pub sale_id: String,
    #[serde(flatten)]
    pub reversal: RefundResult,
}

/// Why this sale can or cannot be voided now (shown before asking).
#[derive(Debug, Clone, Serialize)]
pub struct VoidCheck {
    pub allowed: bool,
    /// Plain reason when not allowed; the till suggests a refund instead.
    pub reason: Option<String>,
    pub requires_approval: bool,
    pub total_minor: i64,
}

/// The rule, checked inside the writing transaction and for the preview.
fn eligibility(c: &rusqlite::Connection, s: &crate::auth::Session, sale_id: &str) -> AppResult<Result<i64, String>> {
    let row: Option<(String, String, String, i64)> = c
        .query_row("SELECT branch_id, business_date, shift_id, total_minor FROM sales WHERE sale_id=?1", [sale_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    let (branch, date, shift, total) = row.ok_or_else(|| AppError::not_found("Sale"))?;
    let today = time::business_date(time::now(), &time::day(c)?)?;
    let exists = |sql: &str| -> AppResult<bool> { Ok(c.query_row(sql, [sale_id], |_| Ok(())).optional()?.is_some()) };
    let why = if branch != s.branch_id {
        Some("This sale was made at another branch.")
    } else if exists("SELECT 1 FROM sale_voids WHERE sale_id=?1")? {
        Some("This sale has already been voided.")
    } else if date != today {
        Some("Only today's sales can be voided. Use a refund for earlier sales.")
    } else if open_shift_for(c, s)?.as_deref() != Some(shift.as_str()) {
        Some("The shift that took this sale is closed. Use a refund instead.")
    } else if exists("SELECT 1 FROM refunds WHERE original_sale_id=?1")? {
        Some("Part of this sale has been refunded. Use a refund for the rest.")
    } else if exists("SELECT 1 FROM sale_collections WHERE sale_id=?1")? || exists("SELECT 1 FROM delivery_orders WHERE sale_id=?1")? {
        Some("This sale was sent for delivery. Use the delivery's not-delivered step or a refund.")
    } else {
        None
    };
    Ok(match why {
        Some(w) => Err(w.to_string()),
        None => Ok(total),
    })
}

/// The whole sale, every line, restocked; tenders returned exactly as paid.
fn whole_sale(c: &rusqlite::Connection, sale_id: &str) -> AppResult<(Vec<RefundLineInput>, Vec<RefundTenderInput>)> {
    let mut st = c.prepare("SELECT sale_item_id, qty_milli FROM sale_items WHERE sale_id=?1 ORDER BY line_no")?;
    let lines = st
        .query_map([sale_id], |r| Ok(RefundLineInput { sale_item_id: r.get(0)?, qty_milli: r.get(1)?, restock: true }))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut st = c.prepare("SELECT method, SUM(amount_minor) FROM payments WHERE sale_id=?1 GROUP BY method ORDER BY MIN(created_at)")?;
    let tenders = st
        .query_map([sale_id], |r| Ok(RefundTenderInput { method: r.get(0)?, amount_minor: r.get(1)?, reference: None }))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|t| t.amount_minor > 0)
        .collect();
    Ok((lines, tenders))
}

impl AppCore {
    pub fn sale_void_check(&self, token: &str, sale_id: &str) -> AppResult<VoidCheck> {
        let s = self.session(token)?;
        let sid = validate::id(sale_id, "Sale")?;
        self.db.read(|c| {
            let total: i64 = c.query_row("SELECT total_minor FROM sales WHERE sale_id=?1", [&sid], |r| r.get(0)).optional()?.unwrap_or(0);
            Ok(match eligibility(c, &s, &sid)? {
                Ok(_) => VoidCheck { allowed: true, reason: None, requires_approval: !s.has("pos.void_sale"), total_minor: total },
                Err(why) => VoidCheck { allowed: false, reason: Some(why), requires_approval: !s.has("pos.void_sale"), total_minor: total },
            })
        })
    }

    pub fn sale_void(&self, token: &str, req: VoidRequest) -> AppResult<VoidResult> {
        let s = self.session(token)?;
        let sid = validate::id(&req.sale_id, "Sale")?;
        let reason = clean(&req.reason, "Reason", 200, true)?;
        idempotency::validate_operation_id(&req.operation_id)?;
        if let Check::Replay { result } = self.db.read(|c| idempotency::check(c, &req.operation_id, "sale.void", &req))? {
            let mut r: VoidResult = serde_json::from_value(result)?;
            r.reversal.replayed = true;
            return Ok(r);
        }
        let (rn, total) = self.db.read(|c| {
            c.query_row("SELECT receipt_number, total_minor FROM sales WHERE sale_id=?1", [&sid], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .optional()?
            .ok_or_else(|| AppError::not_found("Sale"))
        })?;
        // Refuse early (before asking a manager) when the rule says no.
        if let Err(why) = self.db.read(|c| eligibility(c, &s, &sid))? {
            return Err(AppError::conflict(why));
        }
        let (cur, digits) = self.db.read(|c| self.currency(c))?;
        let approved = self.authorize_bound(
            &s,
            "pos.void_sale",
            req.approval_token.as_deref(),
            &format!("Void receipt {rn} ({cur} {})", crate::money::format_decimal(total, digits)),
            "sale.void",
            &sid,
            &json!({ "reason": reason }),
        )?;
        let device = self.require_device()?;
        let actor = self.actor(&s, approved.clone());
        let result = self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "sale.void", &req)? {
                Check::Replay { result } => {
                    let mut r: VoidResult = serde_json::from_value(result)?;
                    r.reversal.replayed = true;
                    return Ok(r);
                }
                Check::New { payload_hash } => payload_hash,
            };
            // Re-check inside the transaction: nothing may have changed since.
            if let Err(why) = eligibility(tx, &s, &sid)? {
                return Err(AppError::conflict(why));
            }
            open_shift_for(tx, &s)?.ok_or_else(shift_required)?;
            let (lines, tenders) = whole_sale(tx, &sid)?;
            let comp = refunds::compute_for(tx, &sid, &lines)?;
            if comp.total_minor() != total || tenders.iter().map(|t| t.amount_minor).sum::<i64>() != total {
                return Err(AppError::conflict("This sale's payments do not add up to its total; use a refund."));
            }
            let reversal = refunds::write_reversal(tx, &s, &actor, &device, Reversal::Void, &sid, &comp, &tenders, &reason, &req.operation_id, approved.as_deref())?;
            let void_id = new_id();
            tx.execute(
                "INSERT INTO sale_voids(void_id, sale_id, refund_id, reason, user_id, approved_by, branch_id, device_id, business_date, operation_id, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    void_id, sid, reversal.refund_id, reason, s.user_id, approved, s.branch_id, s.device_id,
                    time::business_date(time::now(), &time::day(tx)?)?, req.operation_id, reversal.created_at
                ],
            )?;
            audit::record(tx, &actor, "sale.void_recorded", "sale", Some(&sid), None, Some(&json!({
                "void_id": void_id, "receipt_number": rn, "reversal": reversal.refund_receipt_number, "reason": reason
            })))?;
            let result = VoidResult { void_id, sale_id: sid.clone(), reversal };
            idempotency::complete(tx, &req.operation_id, "sale.void", Some(&s.user_id), Some(&s.device_id), &hash, Some(&result.void_id), &serde_json::to_value(&result)?)?;
            Ok(result)
        })?;
        let mut result = result;
        if !result.reversal.replayed {
            self.print_pending_for(&result.reversal.refund_id);
        }
        result.reversal.print =
            self.db.read(|c| crate::printing::latest_job_outcome(c, "refund", &result.reversal.refund_id)).unwrap_or(None);
        Ok(result)
    }
}
