//! Shifts and cash drawer events.
//!
//! Expected drawer cash = opening float + cash sales (net of change)
//!   − cash refunds + paid in − paid out − safe drops.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::idempotency::{self, Check};
use crate::ids::{new_id, next_seq};
use crate::printing::PrintOutcome;
use crate::sales::{open_shift_for, shift_required};
use crate::service::AppCore;
use crate::settings;
use crate::setup::clean;
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodTotal {
    pub method: String,
    pub amount_minor: i64,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShiftSummary {
    pub shift_id: String,
    pub shift_number: String,
    pub user_id: String,
    pub cashier_name: String,
    pub device_id: String,
    pub device_name: Option<String>,
    pub status: String,
    pub business_date: String,
    pub opened_at: String,
    pub closed_at: Option<String>,
    pub opening_float_minor: i64,
    pub sale_count: i64,
    pub sales_total_minor: i64,
    pub discount_total_minor: i64,
    pub tax_total_minor: i64,
    pub by_method: Vec<MethodTotal>,
    pub refund_count: i64,
    pub refunds_total_minor: i64,
    pub cash_sales_minor: i64,
    pub cash_refunds_minor: i64,
    pub paid_in_minor: i64,
    pub paid_out_minor: i64,
    pub safe_drop_minor: i64,
    pub no_sale_count: i64,
    pub expected_cash_minor: i64,
    pub counted_cash_minor: Option<i64>,
    pub variance_minor: Option<i64>,
    /// False when the expected amount is hidden for blind counting.
    pub expected_visible: bool,
    pub close_note: Option<String>,
    pub variance_approved_by_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CashEventRequest {
    /// paid_in | paid_out | safe_drop | no_sale
    pub kind: String,
    #[serde(default)]
    pub amount_minor: i64,
    pub reason: String,
    pub operation_id: String,
    #[serde(default, skip_serializing)]
    pub approval_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ShiftCloseRequest {
    pub counted_cash_minor: i64,
    #[serde(default)]
    pub note: Option<String>,
    pub operation_id: String,
    #[serde(default, skip_serializing)]
    pub approval_token: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CashEventRow {
    pub cash_event_id: String,
    pub created_at: String,
    pub kind: String,
    pub amount_minor: i64,
    pub reason: String,
    pub user_name: Option<String>,
    pub approver_name: Option<String>,
    pub shift_number: String,
}

pub fn shift_summary(c: &Connection, shift_id: &str) -> AppResult<ShiftSummary> {
    let mut s = c
        .query_row(
            "SELECT s.shift_id, s.shift_number, s.user_id, COALESCE(u.display_name,''), s.device_id, d.name, s.status, s.business_date,
                    s.opened_at, s.closed_at, s.opening_float_minor, s.counted_cash_minor, s.variance_minor, s.close_note, a.display_name
             FROM shifts s LEFT JOIN users u ON u.user_id=s.user_id LEFT JOIN devices d ON d.device_id=s.device_id
             LEFT JOIN users a ON a.user_id=s.variance_approved_by WHERE s.shift_id=?1",
            [shift_id],
            |r| {
                Ok(ShiftSummary {
                    shift_id: r.get(0)?,
                    shift_number: r.get(1)?,
                    user_id: r.get(2)?,
                    cashier_name: r.get(3)?,
                    device_id: r.get(4)?,
                    device_name: r.get(5)?,
                    status: r.get(6)?,
                    business_date: r.get(7)?,
                    opened_at: r.get(8)?,
                    closed_at: r.get(9)?,
                    opening_float_minor: r.get(10)?,
                    counted_cash_minor: r.get(11)?,
                    variance_minor: r.get(12)?,
                    close_note: r.get(13)?,
                    variance_approved_by_name: r.get(14)?,
                    sale_count: 0,
                    sales_total_minor: 0,
                    discount_total_minor: 0,
                    tax_total_minor: 0,
                    by_method: vec![],
                    refund_count: 0,
                    refunds_total_minor: 0,
                    cash_sales_minor: 0,
                    cash_refunds_minor: 0,
                    paid_in_minor: 0,
                    paid_out_minor: 0,
                    safe_drop_minor: 0,
                    no_sale_count: 0,
                    expected_cash_minor: 0,
                    expected_visible: true,
                })
            },
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("Shift"))?;
    let (n, tot, disc, tax): (i64, i64, i64, i64) = c.query_row(
        "SELECT COUNT(*), COALESCE(SUM(total_minor),0), COALESCE(SUM(discount_minor),0), COALESCE(SUM(tax_minor),0) FROM sales WHERE shift_id=?1",
        [shift_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    s.sale_count = n;
    s.sales_total_minor = tot;
    s.discount_total_minor = disc;
    s.tax_total_minor = tax;
    let mut st = c.prepare(
        "SELECT p.method, SUM(p.amount_minor), COUNT(DISTINCT p.sale_id) FROM payments p JOIN sales x ON x.sale_id=p.sale_id
         WHERE x.shift_id=?1 GROUP BY p.method ORDER BY p.method",
    )?;
    s.by_method = st
        .query_map([shift_id], |r| Ok(MethodTotal { method: r.get(0)?, amount_minor: r.get(1)?, count: r.get(2)? }))?
        .collect::<Result<Vec<_>, _>>()?;
    s.cash_sales_minor = s.by_method.iter().filter(|m| m.method == "cash").map(|m| m.amount_minor).sum();
    let (rn, rt): (i64, i64) = c.query_row(
        "SELECT COUNT(*), COALESCE(SUM(total_minor),0) FROM refunds WHERE shift_id=?1",
        [shift_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    s.refund_count = rn;
    s.refunds_total_minor = rt;
    s.cash_refunds_minor = c.query_row(
        "SELECT COALESCE(SUM(t.amount_minor),0) FROM refund_tenders t JOIN refunds r ON r.refund_id=t.refund_id WHERE r.shift_id=?1 AND t.method='cash'",
        [shift_id],
        |r| r.get(0),
    )?;
    let mut st = c.prepare("SELECT type, COALESCE(SUM(amount_minor),0), COUNT(*) FROM cash_events WHERE shift_id=?1 GROUP BY type")?;
    let evs = st
        .query_map([shift_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (t, amt, cnt) in evs {
        match t.as_str() {
            "paid_in" => s.paid_in_minor = amt,
            "paid_out" => s.paid_out_minor = amt,
            "safe_drop" => s.safe_drop_minor = amt,
            "no_sale" => s.no_sale_count = cnt,
            _ => {}
        }
    }
    s.expected_cash_minor = s.opening_float_minor + s.cash_sales_minor - s.cash_refunds_minor + s.paid_in_minor
        - s.paid_out_minor
        - s.safe_drop_minor;
    Ok(s)
}

impl AppCore {
    fn hide_expected(&self, c: &Connection, s: &crate::auth::Session, mut sum: ShiftSummary) -> AppResult<ShiftSummary> {
        let cfg: settings::ShiftSettings = settings::get(c, settings::KEY_SHIFT)?;
        if sum.status == "open" && cfg.blind_close && !s.has("shift.view_expected") {
            sum.expected_cash_minor = 0;
            sum.expected_visible = false;
        }
        Ok(sum)
    }

    /// The caller's open shift on this terminal, if any.
    pub fn shift_current(&self, token: &str) -> AppResult<Option<ShiftSummary>> {
        let s = self.session(token)?;
        self.db.read(|c| {
            // Another user's open shift on this device blocks opening a new one.
            match open_shift_for(c, &s)? {
                Some(id) => Ok(Some(self.hide_expected(c, &s, shift_summary(c, &id)?)?)),
                None => Ok(None),
            }
        })
    }

    pub fn shift_open(&self, token: &str, opening_float_minor: i64, operation_id: &str) -> AppResult<ShiftSummary> {
        let s = self.session(token)?;
        s.require("shift.open")?;
        validate::money_non_negative(opening_float_minor, "Opening float")?;
        let device = self.require_device()?;
        let actor = self.actor(&s, None);
        let payload = json!({ "opening_float_minor": opening_float_minor });
        let shift_id = self.db.write(|tx| {
            if let Check::Replay { result } = idempotency::check(tx, operation_id, "shift.open", &payload)? {
                return Ok(result["shift_id"].as_str().unwrap_or_default().to_string());
            }
            let hash = idempotency::payload_hash("shift.open", &payload)?;
            let other: Option<(String, String)> = tx
                .query_row(
                    "SELECT s.user_id, COALESCE(u.display_name,'') FROM shifts s LEFT JOIN users u ON u.user_id=s.user_id WHERE s.device_id=?1 AND s.status='open'",
                    [&s.device_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((uid, name)) = other {
                if uid == s.user_id {
                    return Err(AppError::conflict("You already have an open shift on this terminal."));
                }
                return Err(AppError::conflict(format!(
                    "{name} still has an open shift on this terminal. They (or a manager) must close it first."
                ))
                .with_details(json!({ "open_shift_user": name })));
            }
            let tz = self.store_timezone(tx)?;
            let now = time::now();
            let id = new_id();
            let number = format!("{}-S{:05}", device.device_code, next_seq(tx, &format!("shift:{}", device.device_id))?);
            tx.execute(
                "INSERT INTO shifts(shift_id, shift_number, user_id, branch_id, device_id, business_date, opening_float_minor, opened_at, status)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'open')",
                params![id, number, s.user_id, s.branch_id, s.device_id, time::business_date(now, &tz)?, opening_float_minor, time::fmt(now)],
            )?;
            // Attach the cashier's active cart to the new shift.
            tx.execute(
                "UPDATE carts SET shift_id=?1 WHERE device_id=?2 AND user_id=?3 AND status IN ('active','held')",
                params![id, s.device_id, s.user_id],
            )?;
            let result = json!({ "shift_id": id, "shift_number": number });
            audit::record(tx, &actor, "shift.opened", "shift", Some(&id), None, Some(&json!({ "shift_number": number, "opening_float_minor": opening_float_minor })))?;
            idempotency::complete(tx, operation_id, "shift.open", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &result)?;
            Ok(id)
        })?;
        self.db.read(|c| self.hide_expected(c, &s, shift_summary(c, &shift_id)?))
    }

    pub fn shift_get(&self, token: &str, shift_id: &str) -> AppResult<ShiftSummary> {
        let s = self.session(token)?;
        let id = validate::id(shift_id, "Shift")?;
        self.db.read(|c| {
            let sum = shift_summary(c, &id)?;
            if sum.user_id != s.user_id && !s.has("sales.view") {
                return Err(AppError::forbidden("sales.view"));
            }
            self.hide_expected(c, &s, sum)
        })
    }

    /// Close a shift with the physical count. Large variances need
    /// `shift.approve_variance` (or a manager approval token).
    pub fn shift_close(&self, token: &str, shift_id: &str, req: ShiftCloseRequest) -> AppResult<(ShiftSummary, PrintOutcome)> {
        let s = self.session(token)?;
        let id = validate::id(shift_id, "Shift")?;
        validate::money_non_negative(req.counted_cash_minor, "Counted cash")?;
        let note = crate::setup::clean_opt(&req.note, "Note", 300)?;
        let (sum, cfg) = self.db.read(|c| Ok((shift_summary(c, &id)?, settings::get::<settings::ShiftSettings>(c, settings::KEY_SHIFT)?)))?;
        if sum.status != "open" {
            // Idempotent replay of an already-closed shift.
            let replay = self.db.read(|c| idempotency::check(c, &req.operation_id, "shift.close", &(&id, &req)))?;
            if let Check::Replay { .. } = replay {
                let sm = self.db.read(|c| shift_summary(c, &id))?;
                let po = self.db.read(|c| crate::printing::latest_job_outcome(c, "shift_report", &id))?.unwrap_or_else(PrintOutcome::queued);
                return Ok((sm, po));
            }
            return Err(AppError::conflict("This shift is already closed."));
        }
        if sum.user_id != s.user_id {
            s.require("shift.approve_variance")?;
        } else {
            s.require("shift.close")?;
        }
        let variance = req.counted_cash_minor - sum.expected_cash_minor;
        let approved = if variance.abs() > cfg.variance_approval_minor && !s.has("shift.approve_variance") {
            self.authorize(&s, "shift.approve_variance", req.approval_token.as_deref(), "Acknowledge a cash variance on shift close")?
        } else {
            None
        };
        let actor = self.actor(&s, approved.clone());
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "shift.close", &(&id, &req))? {
                Check::Replay { .. } => return Ok(()),
                Check::New { payload_hash } => payload_hash,
            };
            let sum = shift_summary(tx, &id)?;
            if sum.status != "open" {
                return Err(AppError::conflict("This shift is already closed."));
            }
            let variance = req.counted_cash_minor - sum.expected_cash_minor;
            let held: i64 = tx.query_row("SELECT COUNT(*) FROM carts WHERE shift_id=?1 AND status='active' AND EXISTS (SELECT 1 FROM cart_lines l WHERE l.cart_id=carts.cart_id)", [&id], |r| r.get(0))?;
            if held > 0 {
                return Err(AppError::conflict("Finish, hold or cancel the sale in progress before closing the shift."));
            }
            tx.execute(
                "UPDATE shifts SET status='closed', closed_at=?2, expected_cash_minor=?3, counted_cash_minor=?4, variance_minor=?5,
                    closed_by=?6, variance_approved_by=?7, close_note=?8 WHERE shift_id=?1 AND status='open'",
                params![id, time::now_str(), sum.expected_cash_minor, req.counted_cash_minor, variance, s.user_id, approved, note],
            )?;
            crate::printing::enqueue(tx, "shift_report", &id, None, Some(&s.user_id))?;
            let result = json!({ "shift_id": id, "expected_cash_minor": sum.expected_cash_minor, "counted_cash_minor": req.counted_cash_minor, "variance_minor": variance });
            audit::record(tx, &actor, "shift.closed", "shift", Some(&id), None, Some(&result))?;
            idempotency::complete(tx, &req.operation_id, "shift.close", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &result)?;
            Ok(())
        })?;
        self.print_pending_for(&id);
        let sm = self.db.read(|c| shift_summary(c, &id))?;
        let po = self.db.read(|c| crate::printing::latest_job_outcome(c, "shift_report", &id))?.unwrap_or_else(PrintOutcome::queued);
        Ok((sm, po))
    }

    /// Record a cash movement in the open shift (exactly once per operation id).
    pub fn cash_event(&self, token: &str, req: CashEventRequest) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        let perm = match req.kind.as_str() {
            "paid_in" => "cash.paid_in",
            "paid_out" => "cash.paid_out",
            "safe_drop" => "cash.safe_drop",
            "no_sale" => "pos.no_sale",
            _ => return Err(AppError::validation("Unknown cash event type.")),
        };
        let reason = clean(&req.reason, "Reason", 200, true)?;
        if req.kind == "no_sale" {
            if req.amount_minor != 0 {
                return Err(AppError::validation("A no-sale drawer opening has no amount."));
            }
        } else {
            validate::money_non_negative(req.amount_minor, "Amount")?;
            if req.amount_minor == 0 {
                return Err(AppError::validation("Enter an amount greater than zero."));
            }
        }
        idempotency::validate_operation_id(&req.operation_id)?;
        if let Check::Replay { result } = self.db.read(|c| idempotency::check(c, &req.operation_id, "cash.event", &req))? {
            return Ok(result);
        }
        let cfg: settings::ShiftSettings = self.db.read(|c| settings::get(c, settings::KEY_SHIFT))?;
        let mut approved = self.authorize(&s, perm, req.approval_token.as_deref(), &format!("{} {}", req.kind.replace('_', " "), reason))?;
        if req.kind == "paid_out" && cfg.paid_out_approval_minor > 0 && req.amount_minor > cfg.paid_out_approval_minor && approved.is_none() && !s.has("shift.approve_variance") {
            approved = self.authorize(&s, "shift.approve_variance", req.approval_token.as_deref(), "Large paid-out")?;
        }
        let actor = self.actor(&s, approved.clone());
        let result = self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "cash.event", &req)? {
                Check::Replay { result } => return Ok(result),
                Check::New { payload_hash } => payload_hash,
            };
            let shift_id = open_shift_for(tx, &s)?.ok_or_else(shift_required)?;
            if matches!(req.kind.as_str(), "paid_out" | "safe_drop") {
                let expected = shift_summary(tx, &shift_id)?.expected_cash_minor;
                if req.amount_minor > expected {
                    return Err(AppError::new(ErrorCode::Validation, "The drawer does not have that much cash according to the records."));
                }
            }
            let id = new_id();
            let now = time::now_str();
            tx.execute(
                "INSERT INTO cash_events(cash_event_id, shift_id, type, amount_minor, reason, actor_user_id, approved_by, operation_id, device_id, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![id, shift_id, req.kind, req.amount_minor, reason, s.user_id, approved, req.operation_id, s.device_id, now],
            )?;
            crate::printing::enqueue_drawer_pulse(tx, Some(&s.user_id), &id)?;
            let result = json!({ "cash_event_id": id, "shift_id": shift_id, "kind": req.kind, "amount_minor": req.amount_minor, "created_at": now });
            audit::record(tx, &actor, &format!("cash.{}", req.kind), "cash_event", Some(&id), None, Some(&result))?;
            idempotency::complete(tx, &req.operation_id, "cash.event", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &result)?;
            Ok(result)
        })?;
        if let Some(id) = result["cash_event_id"].as_str() {
            self.print_pending_for(id);
        }
        Ok(result)
    }

    pub fn shifts_list(&self, token: &str, from: Option<String>, to: Option<String>, limit: Option<i64>) -> AppResult<Vec<ShiftSummary>> {
        let s = self.session(token)?;
        s.require("sales.view")?;
        let limit = validate::limit(limit, 100, 500);
        self.db.read(|c| {
            let tz = self.store_timezone(c)?;
            let (a, b) = match (&from, &to) {
                (None, None) => ("0".to_string(), "9".to_string()),
                _ => time::local_date_range_utc(from.as_deref().unwrap_or("2000-01-01"), to.as_deref().unwrap_or("2999-12-31"), &tz)?,
            };
            let mut st = c.prepare(&format!("SELECT shift_id FROM shifts WHERE opened_at>=?1 AND opened_at<?2 ORDER BY opened_at DESC LIMIT {limit}"))?;
            let ids = st.query_map(params![a, b], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            ids.iter().map(|id| shift_summary(c, id)).collect()
        })
    }

    pub fn cash_events_list(&self, token: &str, shift_id: Option<String>, from: Option<String>, to: Option<String>) -> AppResult<Vec<CashEventRow>> {
        let s = self.session(token)?;
        s.require("sales.view")?;
        self.db.read(|c| {
            let tz = self.store_timezone(c)?;
            let (a, b) = match (&from, &to) {
                (None, None) => ("0".to_string(), "9".to_string()),
                _ => time::local_date_range_utc(from.as_deref().unwrap_or("2000-01-01"), to.as_deref().unwrap_or("2999-12-31"), &tz)?,
            };
            let mut st = c.prepare(
                "SELECT e.cash_event_id, e.created_at, e.type, e.amount_minor, e.reason, u.display_name, a.display_name, s.shift_number
                 FROM cash_events e JOIN shifts s ON s.shift_id=e.shift_id LEFT JOIN users u ON u.user_id=e.actor_user_id
                 LEFT JOIN users a ON a.user_id=e.approved_by
                 WHERE (?1 IS NULL OR e.shift_id=?1) AND e.created_at>=?2 AND e.created_at<?3 ORDER BY e.created_at DESC LIMIT 1000",
            )?;
            let rows = st
                .query_map(params![shift_id.filter(|x| !x.is_empty()), a, b], |r| {
                    Ok(CashEventRow {
                        cash_event_id: r.get(0)?,
                        created_at: r.get(1)?,
                        kind: r.get(2)?,
                        amount_minor: r.get(3)?,
                        reason: r.get(4)?,
                        user_name: r.get(5)?,
                        approver_name: r.get(6)?,
                        shift_number: r.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }
}
