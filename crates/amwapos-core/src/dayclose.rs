//! The trading day: current totals (X), the closing checks, the permanent
//! close (Z) and the opening checklist.
//!
//! Scope. A close covers one branch and one business date. The business date
//! is the one stored on each sale, refund and shift when it was made (Wave 1,
//! `time::business_date`); nothing here recomputes it.
//!
//! What a close counts. Every sale, refund (including voids) and closed shift
//! is counted by exactly one close (`day_close_items` primary key). The close
//! of date D counts:
//! - records of date D not yet counted ("the day");
//! - records of an earlier date that reached this computer after their own day
//!   was closed ("after close"): they keep their real time and business date,
//!   the earlier close is never changed, and they are shown on this close as
//!   after-close adjustments.
//!
//! Dates are closed in order, from the first close on. Records dated before
//! the branch's first close are not part of any close.
//!
//! X is the same calculation without writing anything.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check as Idem};
use crate::ids::{new_id, next_seq};
use crate::money::format_decimal;
use crate::receipt::ReceiptDoc;
use crate::service::AppCore;
use crate::settings;
use crate::shifts::shift_summary;
use crate::time;
use crate::validate;

pub const CLOSE_FORMAT: i64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Tender {
    pub method: String,
    pub sales_minor: i64,
    pub refunds_minor: i64,
    pub net_minor: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct VatLine {
    pub rate_bp: i64,
    pub gross_minor: i64,
    pub tax_minor: i64,
    pub net_minor: i64,
}

/// Sales, refunds and voids of one set of records. Money includes VAT unless
/// named `ex_vat`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Totals {
    pub sale_count: i64,
    /// Before discounts.
    pub gross_minor: i64,
    pub discount_minor: i64,
    /// What customers were charged (after discounts).
    pub sales_minor: i64,
    pub refund_count: i64,
    pub refunds_minor: i64,
    pub void_count: i64,
    pub voids_minor: i64,
    /// Sales less refunds and voids.
    pub net_sales_minor: i64,
    pub tax_minor: i64,
    pub net_ex_vat_minor: i64,
    pub cash_sales_minor: i64,
    pub non_cash_sales_minor: i64,
    pub pay_on_delivery_minor: i64,
    pub account_sales_minor: i64,
    pub tenders: Vec<Tender>,
    pub vat: Vec<VatLine>,
}

impl Totals {
    fn add(&mut self, o: &Totals) {
        self.sale_count += o.sale_count;
        self.gross_minor += o.gross_minor;
        self.discount_minor += o.discount_minor;
        self.sales_minor += o.sales_minor;
        self.refund_count += o.refund_count;
        self.refunds_minor += o.refunds_minor;
        self.void_count += o.void_count;
        self.voids_minor += o.voids_minor;
        self.net_sales_minor += o.net_sales_minor;
        self.tax_minor += o.tax_minor;
        self.net_ex_vat_minor += o.net_ex_vat_minor;
        self.cash_sales_minor += o.cash_sales_minor;
        self.non_cash_sales_minor += o.non_cash_sales_minor;
        self.pay_on_delivery_minor += o.pay_on_delivery_minor;
        self.account_sales_minor += o.account_sales_minor;
        for t in &o.tenders {
            match self.tenders.iter_mut().find(|x| x.method == t.method) {
                Some(x) => {
                    x.sales_minor += t.sales_minor;
                    x.refunds_minor += t.refunds_minor;
                    x.net_minor += t.net_minor;
                }
                None => self.tenders.push(t.clone()),
            }
        }
        for v in &o.vat {
            match self.vat.iter_mut().find(|x| x.rate_bp == v.rate_bp) {
                Some(x) => {
                    x.gross_minor += v.gross_minor;
                    x.tax_minor += v.tax_minor;
                    x.net_minor += v.net_minor;
                }
                None => self.vat.push(v.clone()),
            }
        }
        self.tenders.sort_by(|a, b| a.method.cmp(&b.method));
        self.vat.sort_by_key(|v| std::cmp::Reverse(v.rate_bp));
    }
}

/// One drawer (shift): who held the cash, and what it should hold.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DrawerLine {
    pub shift_id: String,
    pub shift_number: String,
    pub business_date: String,
    pub status: String,
    pub register_name: Option<String>,
    pub drawer_name: Option<String>,
    pub device_name: Option<String>,
    pub cashier_name: String,
    pub opened_at: String,
    pub closed_at: Option<String>,
    pub opening_float_minor: i64,
    pub cash_sales_minor: i64,
    pub cash_refunds_minor: i64,
    pub paid_in_minor: i64,
    pub paid_out_minor: i64,
    pub safe_drop_minor: i64,
    pub delivery_collections_minor: i64,
    pub rider_handover_minor: i64,
    pub expected_cash_minor: i64,
    pub counted_cash_minor: Option<i64>,
    pub variance_minor: Option<i64>,
    pub late: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct CashTotals {
    pub opening_float_minor: i64,
    pub cash_sales_minor: i64,
    pub cash_refunds_minor: i64,
    pub paid_in_minor: i64,
    pub paid_out_minor: i64,
    pub safe_drop_minor: i64,
    pub delivery_collections_minor: i64,
    pub rider_handover_minor: i64,
    pub expected_cash_minor: i64,
    /// Counted drawers only.
    pub counted_cash_minor: i64,
    pub variance_minor: i64,
    pub counted_drawers: i64,
    pub open_drawers: i64,
    /// Part of the paid-outs: expenses paid from the till.
    pub expenses_from_till_minor: i64,
    /// Part of the paid-ins: customer account payments in cash.
    pub account_payments_cash_minor: i64,
}

/// A record counted after its own day was closed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LateLine {
    pub kind: String,
    pub number: String,
    pub business_date: String,
    pub at: String,
    pub total_minor: i64,
    pub device_name: Option<String>,
}

/// The X or Z report. A Z keeps this exactly as closed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DayReport {
    /// "x" (current totals, nothing changed) or "z" (permanent close).
    pub kind: String,
    pub business_name: String,
    pub vat_number: Option<String>,
    pub branch_id: String,
    pub branch_name: String,
    pub currency: String,
    pub digits: u32,
    pub timezone: String,
    pub cutoff_minutes: i64,
    pub business_date: String,
    /// The day already has a close (X after a close shows what the next close
    /// will count).
    pub day_closed: bool,
    pub day: Totals,
    pub after_close: Totals,
    pub total: Totals,
    pub late: Vec<LateLine>,
    pub drawers: Vec<DrawerLine>,
    pub cash: CashTotals,
    pub generated_at: String,
    pub close_number: Option<String>,
    pub closed_by_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DayCheck {
    pub code: String,
    /// blocking | warning | info | ok
    pub level: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DayChecks {
    pub business_date: String,
    pub branch_id: String,
    pub can_close: bool,
    pub needs_acknowledgement: bool,
    pub checks: Vec<DayCheck>,
    pub last_close: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Opening {
    pub business_date: String,
    /// ready | attention
    pub verdict: String,
    pub checks: Vec<DayCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloseRequest {
    pub business_date: String,
    #[serde(default)]
    pub branch_id: Option<String>,
    pub operation_id: String,
    /// The person has seen the warnings.
    #[serde(default)]
    pub acknowledge_warnings: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CloseView {
    pub close_id: String,
    pub close_number: String,
    pub branch_id: String,
    pub business_date: String,
    pub closed_at: String,
    pub closed_by_name: Option<String>,
    pub sha256: String,
    /// The stored record still matches its fingerprint.
    pub verified: bool,
    pub report: DayReport,
    pub doc: ReceiptDoc,
}

#[derive(Debug, Clone, Serialize)]
pub struct CloseRow {
    pub close_id: String,
    pub close_number: String,
    pub branch_id: String,
    pub branch_name: String,
    pub business_date: String,
    pub sale_count: i64,
    pub net_sales_minor: i64,
    pub tax_minor: i64,
    pub late_count: i64,
    pub variance_minor: i64,
    pub closed_by_name: Option<String>,
    pub closed_at: String,
}

fn check(code: &str, level: &str, message: impl Into<String>) -> DayCheck {
    DayCheck { code: code.into(), level: level.into(), message: message.into() }
}

// ---------------------------------------------------------------- sets

/// Bound parameters of every set query: ?1 branch, ?2 date, ?3 first close
/// date (or the date itself when the branch has none), ?4 1 when the date is
/// already closed.
struct Bounds {
    branch: String,
    date: String,
    start: String,
    closed: i64,
}

impl Bounds {
    fn load(c: &Connection, branch: &str, date: &str) -> AppResult<Self> {
        let start: Option<String> = c.query_row("SELECT MIN(business_date) FROM day_closes WHERE branch_id=?1", [branch], |r| r.get(0))?;
        let closed: bool = c
            .query_row("SELECT 1 FROM day_closes WHERE branch_id=?1 AND business_date=?2", params![branch, date], |_| Ok(true))
            .optional()?
            .is_some();
        let start = match start {
            Some(s) if s.as_str() <= date => s,
            _ => date.to_string(),
        };
        Ok(Self { branch: branch.into(), date: date.into(), start, closed: closed as i64 })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Set {
    Day,
    Late,
}

/// WHERE for sales (`x`) or refunds (`x`) in a set, not yet counted.
fn set_where(kind: &str, id_col: &str, set: Set) -> String {
    let base = format!(
        "x.branch_id=?1 AND x.business_date>=?3 AND NOT EXISTS (SELECT 1 FROM day_close_items i WHERE i.ref_kind='{kind}' AND i.ref_id=x.{id_col})"
    );
    match set {
        Set::Day => format!("{base} AND x.business_date=?2 AND ?4=0"),
        Set::Late => format!("{base} AND (x.business_date<?2 OR (x.business_date=?2 AND ?4=1))"),
    }
}

/// Closed shifts not yet counted (both sets), or open shifts (for X).
fn shift_where(open: bool) -> &'static str {
    if open {
        "x.branch_id=?1 AND x.business_date<=?2 AND x.status='open'"
    } else {
        "x.branch_id=?1 AND x.business_date<=?2 AND x.business_date>=?3 AND x.status='closed'
         AND NOT EXISTS (SELECT 1 FROM day_close_items i WHERE i.ref_kind='shift' AND i.ref_id=x.shift_id)"
    }
}

fn args(b: &Bounds) -> [&dyn rusqlite::ToSql; 4] {
    [&b.branch, &b.date, &b.start, &b.closed]
}

fn totals(c: &Connection, b: &Bounds, set: Set) -> AppResult<Totals> {
    let sw = set_where("sale", "sale_id", set);
    let rw = set_where("refund", "refund_id", set);
    let mut t = Totals::default();
    let (n, sales, disc, tax): (i64, i64, i64, i64) = c.query_row(
        &format!(
            "SELECT COUNT(*), COALESCE(SUM(total_minor),0), COALESCE(SUM(discount_minor + loyalty_discount_minor),0), COALESCE(SUM(tax_minor),0)
             FROM sales x WHERE {sw}"
        ),
        args(b).as_slice(),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    t.sale_count = n;
    t.sales_minor = sales;
    t.discount_minor = disc;
    t.gross_minor = sales + disc;
    let mut refund_tax = 0;
    let mut st = c.prepare(&format!(
        "SELECT kind, COUNT(*), COALESCE(SUM(total_minor),0), COALESCE(SUM(tax_minor),0) FROM refunds x WHERE {rw} GROUP BY kind"
    ))?;
    for row in
        st.query_map(args(b).as_slice(), |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)))?
    {
        let (kind, n, total, tx) = row?;
        refund_tax += tx;
        if kind == "void" {
            t.void_count = n;
            t.voids_minor = total;
        } else {
            t.refund_count += n;
            t.refunds_minor += total;
        }
    }
    t.net_sales_minor = t.sales_minor - t.refunds_minor - t.voids_minor;
    t.tax_minor = tax - refund_tax;
    t.net_ex_vat_minor = t.net_sales_minor - t.tax_minor;
    let mut st = c.prepare(&format!(
        "SELECT p.method, SUM(p.amount_minor) FROM payments p JOIN sales x ON x.sale_id=p.sale_id WHERE {sw} GROUP BY p.method"
    ))?;
    for row in st.query_map(args(b).as_slice(), |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (method, amt) = row?;
        t.tenders.push(Tender { method, sales_minor: amt, refunds_minor: 0, net_minor: amt });
    }
    let mut st = c.prepare(&format!(
        "SELECT t.method, SUM(t.amount_minor) FROM refund_tenders t JOIN refunds x ON x.refund_id=t.refund_id WHERE {rw} GROUP BY t.method"
    ))?;
    for row in st.query_map(args(b).as_slice(), |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (method, amt) = row?;
        match t.tenders.iter_mut().find(|x| x.method == method) {
            Some(x) => {
                x.refunds_minor += amt;
                x.net_minor -= amt;
            }
            None => t.tenders.push(Tender { method, sales_minor: 0, refunds_minor: amt, net_minor: -amt }),
        }
    }
    t.tenders.sort_by(|a, b| a.method.cmp(&b.method));
    for x in &t.tenders {
        match x.method.as_str() {
            "cash" => t.cash_sales_minor += x.net_minor,
            m => {
                t.non_cash_sales_minor += x.net_minor;
                if m == crate::sales::PAY_ON_DELIVERY {
                    t.pay_on_delivery_minor += x.net_minor;
                }
                if m == "account" {
                    t.account_sales_minor += x.net_minor;
                }
            }
        }
    }
    let mut st = c.prepare(&format!(
        "WITH s AS (SELECT i.tax_rate_bp rate, SUM(i.line_total_minor) gross, SUM(i.tax_minor) tax FROM sale_items i JOIN sales x ON x.sale_id=i.sale_id
                    WHERE {sw} GROUP BY 1),
              r AS (SELECT si.tax_rate_bp rate, SUM(ri.amount_minor) gross, SUM(ri.tax_minor) tax FROM refund_items ri
                    JOIN refunds x ON x.refund_id=ri.refund_id JOIN sale_items si ON si.sale_item_id=ri.original_sale_item_id WHERE {rw} GROUP BY 1),
              k AS (SELECT rate FROM s UNION SELECT rate FROM r)
         SELECT k.rate, COALESCE(s.gross,0) - COALESCE(r.gross,0), COALESCE(s.tax,0) - COALESCE(r.tax,0)
         FROM k LEFT JOIN s ON s.rate=k.rate LEFT JOIN r ON r.rate=k.rate ORDER BY k.rate DESC"
    ))?;
    for row in st.query_map(args(b).as_slice(), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))? {
        let (rate_bp, gross, tax) = row?;
        t.vat.push(VatLine { rate_bp, gross_minor: gross, tax_minor: tax, net_minor: gross - tax });
    }
    Ok(t)
}

fn late_lines(c: &Connection, b: &Bounds) -> AppResult<Vec<LateLine>> {
    let sw = set_where("sale", "sale_id", Set::Late);
    let rw = set_where("refund", "refund_id", Set::Late);
    let mut st = c.prepare(&format!(
        "SELECT 'sale', x.receipt_number, x.business_date, x.completed_at, x.total_minor, d.name FROM sales x LEFT JOIN devices d ON d.device_id=x.device_id WHERE {sw}
         UNION ALL
         SELECT CASE x.kind WHEN 'void' THEN 'void' ELSE 'refund' END, x.refund_receipt_number, x.business_date, x.created_at, x.total_minor, d.name
         FROM refunds x LEFT JOIN devices d ON d.device_id=x.device_id WHERE {rw}
         ORDER BY 3, 4"
    ))?;
    let rows = st
        .query_map(args(b).as_slice(), |r| {
            Ok(LateLine {
                kind: r.get(0)?,
                number: r.get(1)?,
                business_date: r.get(2)?,
                at: r.get(3)?,
                total_minor: r.get(4)?,
                device_name: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn drawer_line(c: &Connection, shift_id: &str, b: &Bounds) -> AppResult<DrawerLine> {
    let s = shift_summary(c, shift_id)?;
    let late = s.status == "closed" && (s.business_date < b.date || (s.business_date == b.date && b.closed == 1));
    Ok(DrawerLine {
        shift_id: s.shift_id,
        shift_number: s.shift_number,
        business_date: s.business_date,
        status: s.status,
        register_name: s.register_name,
        drawer_name: s.drawer_name,
        device_name: s.device_name,
        cashier_name: s.cashier_name,
        opened_at: s.opened_at,
        closed_at: s.closed_at,
        opening_float_minor: s.opening_float_minor,
        cash_sales_minor: s.cash_sales_minor,
        cash_refunds_minor: s.cash_refunds_minor,
        paid_in_minor: s.paid_in_minor,
        paid_out_minor: s.paid_out_minor,
        safe_drop_minor: s.safe_drop_minor,
        delivery_collections_minor: s.cash_collections_minor,
        rider_handover_minor: s.rider_handover_minor,
        expected_cash_minor: s.expected_cash_minor,
        counted_cash_minor: s.counted_cash_minor,
        variance_minor: s.variance_minor,
        late,
    })
}

fn shift_ids(c: &Connection, b: &Bounds, open: bool) -> AppResult<Vec<String>> {
    let mut st = c.prepare(&format!("SELECT x.shift_id FROM shifts x WHERE {} ORDER BY x.opened_at", shift_where(open)))?;
    let ids = if open {
        st.query_map(params![b.branch, b.date], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?
    } else {
        st.query_map(params![b.branch, b.date, b.start], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?
    };
    Ok(ids)
}

fn cash(c: &Connection, drawers: &[DrawerLine]) -> AppResult<CashTotals> {
    let mut t = CashTotals::default();
    for d in drawers {
        t.opening_float_minor += d.opening_float_minor;
        t.cash_sales_minor += d.cash_sales_minor;
        t.cash_refunds_minor += d.cash_refunds_minor;
        t.paid_in_minor += d.paid_in_minor;
        t.paid_out_minor += d.paid_out_minor;
        t.safe_drop_minor += d.safe_drop_minor;
        t.delivery_collections_minor += d.delivery_collections_minor;
        t.rider_handover_minor += d.rider_handover_minor;
        t.expected_cash_minor += d.expected_cash_minor;
        match (d.counted_cash_minor, d.status.as_str()) {
            (Some(n), "closed") => {
                t.counted_cash_minor += n;
                t.variance_minor += d.variance_minor.unwrap_or(0);
                t.counted_drawers += 1;
            }
            _ => t.open_drawers += 1,
        }
        t.expenses_from_till_minor += c.query_row(
            "SELECT COALESCE(SUM(e.total_minor),0) FROM expenses e JOIN cash_events ce ON ce.cash_event_id=e.cash_event_id
             WHERE ce.shift_id=?1 AND e.status IN ('paid')",
            [&d.shift_id],
            |r| r.get::<_, i64>(0),
        )?;
        t.account_payments_cash_minor += c
            .query_row(
                "SELECT COALESCE(SUM(l.amount_minor),0) FROM customer_ledger l JOIN cash_events ce ON ce.cash_event_id=l.ref_id
             WHERE l.kind='payment' AND l.ref_type='cash_event' AND ce.shift_id=?1",
                [&d.shift_id],
                |r| r.get::<_, i64>(0),
            )?
            .abs();
    }
    Ok(t)
}

fn report(c: &Connection, b: &Bounds, kind: &str) -> AppResult<DayReport> {
    let (business_name, bvat, currency, digits): (String, Option<String>, String, i64) =
        c.query_row("SELECT name, vat_number, currency, currency_digits FROM business LIMIT 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
    let (branch_name, brvat): (String, Option<String>) = c
        .query_row("SELECT name, vat_number FROM branches WHERE branch_id=?1", [&b.branch], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?
        .unwrap_or_default();
    let day = time::day(c)?;
    let d = totals(c, b, Set::Day)?;
    let l = totals(c, b, Set::Late)?;
    let mut total = Totals::default();
    total.add(&d);
    total.add(&l);
    let mut drawers = vec![];
    for id in shift_ids(c, b, false)? {
        drawers.push(drawer_line(c, &id, b)?);
    }
    if kind == "x" {
        for id in shift_ids(c, b, true)? {
            drawers.push(drawer_line(c, &id, b)?);
        }
    }
    let cash = cash(c, &drawers)?;
    Ok(DayReport {
        kind: kind.into(),
        business_name,
        vat_number: brvat.or(bvat),
        branch_id: b.branch.clone(),
        branch_name,
        currency,
        digits: digits as u32,
        timezone: day.tz.clone(),
        cutoff_minutes: day.cutoff_minutes,
        business_date: b.date.clone(),
        day_closed: b.closed == 1,
        day: d,
        after_close: l,
        total,
        late: late_lines(c, b)?,
        drawers,
        cash,
        generated_at: time::now_str(),
        close_number: None,
        closed_by_name: None,
    })
}

fn money(c: &Connection, v: i64) -> AppResult<String> {
    let (cur, digits): (String, i64) =
        c.query_row("SELECT currency, currency_digits FROM business LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(format!("{cur} {}", format_decimal(v, digits as u32)))
}

/// "Drawer is BHD 6.250 short" / "over". Facts only.
pub(crate) fn variance_words(c: &Connection, v: i64) -> AppResult<String> {
    Ok(if v < 0 { format!("Drawer is {} short", money(c, -v)?) } else { format!("Drawer is {} over", money(c, v)?) })
}

/// What this computer knows that affects closing `b.date`.
struct Context {
    device_id: String,
    is_hub: bool,
    backup_overdue: bool,
    today: String,
}

fn checks(c: &Connection, b: &Bounds, ctx: &Context) -> AppResult<Vec<DayCheck>> {
    let mut out = vec![];
    let last: Option<(String, String)> = c
        .query_row(
            "SELECT business_date, close_number FROM day_closes WHERE branch_id=?1 ORDER BY business_date DESC LIMIT 1",
            [&b.branch],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    // ---- blocking
    if b.date > ctx.today {
        out.push(check("future_date", "blocking", "This trading day has not started yet."));
    }
    if b.closed == 1 {
        let n: String =
            c.query_row("SELECT close_number FROM day_closes WHERE branch_id=?1 AND business_date=?2", params![b.branch, b.date], |r| {
                r.get(0)
            })?;
        out.push(check("already_closed", "blocking", format!("This trading day is already closed ({n}).")));
    } else if let Some((d, _)) = last.as_ref().filter(|(d, _)| d.as_str() > b.date.as_str()) {
        out.push(check("closed_in_order", "blocking", format!("Trading days are closed in order, and {d} is already closed.")));
    }
    if last.is_some() && b.closed == 0 {
        // Dates are closed in order: an earlier day with records must be closed first.
        let earlier: Option<String> = c.query_row(
            "SELECT MIN(d) FROM (
               SELECT x.business_date d FROM sales x WHERE x.branch_id=?1 AND x.business_date>=?3 AND x.business_date<?2
                 AND NOT EXISTS (SELECT 1 FROM day_close_items i WHERE i.ref_kind='sale' AND i.ref_id=x.sale_id)
               UNION ALL
               SELECT x.business_date FROM refunds x WHERE x.branch_id=?1 AND x.business_date>=?3 AND x.business_date<?2
                 AND NOT EXISTS (SELECT 1 FROM day_close_items i WHERE i.ref_kind='refund' AND i.ref_id=x.refund_id))
             WHERE NOT EXISTS (SELECT 1 FROM day_closes z WHERE z.branch_id=?1 AND z.business_date=d)",
            params![b.branch, b.date, b.start],
            |r| r.get(0),
        )?;
        if let Some(d) = earlier {
            out.push(check("earlier_day_open", "blocking", format!("Close {d} first.")));
        }
    }
    let mut st = c.prepare(
        "SELECT x.shift_number, COALESCE(u.display_name,''), x.device_id, COALESCE(d.name,'') FROM shifts x
         LEFT JOIN users u ON u.user_id=x.user_id LEFT JOIN devices d ON d.device_id=x.device_id
         WHERE x.branch_id=?1 AND x.business_date<=?2 AND x.status='open' ORDER BY x.opened_at",
    )?;
    let open = st
        .query_map(params![b.branch, b.date], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (number, who, device, dname) in open {
        if device == ctx.device_id {
            out.push(check("open_shift", "blocking", format!("Count and close shift {number} ({who}) first.")));
        } else {
            out.push(check(
                "open_shift_elsewhere",
                "warning",
                format!("Shift {number} ({who}) on {dname} is still open. Its cash will be counted in a later close."),
            ));
        }
    }
    // ---- warnings
    if ctx.is_hub {
        let mut st = c.prepare(
            "SELECT d.name, h.pending_count, h.last_seen_at FROM devices d LEFT JOIN device_heartbeats h ON h.device_id=d.device_id
             WHERE d.operating_mode='terminal' AND d.active=1 AND d.branch_id=?1 ORDER BY d.name",
        )?;
        let rows = st
            .query_map([&b.branch], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<String>>(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let stale = time::fmt(time::now() - chrono::Duration::minutes(30));
        for (name, pending, seen) in rows {
            if pending.unwrap_or(0) > 0 {
                out.push(check(
                    "terminal_pending",
                    "warning",
                    format!("{name} has {} records still to send. They will be counted when they arrive.", pending.unwrap_or(0)),
                ));
            } else if seen.as_deref().map(|s| s < stale.as_str()).unwrap_or(true) {
                out.push(check(
                    "terminal_silent",
                    "warning",
                    format!(
                        "{name} has not been in touch for over 30 minutes. Sales it has not sent yet will be counted when they arrive."
                    ),
                ));
            }
        }
    }
    let dead: i64 = c.query_row("SELECT COUNT(*) FROM sync_dead_letters WHERE status='open'", [], |r| r.get(0))?;
    if dead > 0 {
        out.push(check("sync_failures", "warning", format!("{dead} records from other computers could not be saved. See System → Sync.")));
    }
    let cfg: settings::ShiftSettings = settings::get(c, settings::KEY_SHIFT)?;
    for id in shift_ids(c, b, false)? {
        let s = shift_summary(c, &id)?;
        let v = s.variance_minor.unwrap_or(0);
        if v != 0 {
            let words = variance_words(c, v)?;
            let level = if v.abs() > cfg.variance_approval_minor { "warning" } else { "info" };
            out.push(check("drawer_variance", level, format!("{words} (shift {}).", s.shift_number)));
        }
    }
    let cases: i64 = c.query_row(
        "SELECT COUNT(*) FROM cases WHERE kind='cash_variance' AND branch_id=?1 AND status NOT IN ('resolved','dismissed')",
        [&b.branch],
        |r| r.get(0),
    )?;
    if cases > 0 {
        out.push(check("open_cases", "warning", format!("{cases} cash differences are still being looked into.")));
    }
    let riders: i64 =
        c.query_row("SELECT COUNT(*) FROM sale_collections WHERE held_by IS NOT NULL AND branch_id=?1", [&b.branch], |r| r.get(0))?;
    if riders > 0 {
        out.push(check("rider_cash", "warning", format!("Riders still hold cash from {riders} deliveries.")));
    }
    let reviews: i64 = c.query_row(
        "SELECT COUNT(*) FROM payment_reviews WHERE status IN ('pending','ocr_match','likely_match','mismatch','needs_review')",
        [],
        |r| r.get(0),
    )?;
    if reviews > 0 {
        out.push(check("payment_reviews", "warning", format!("{reviews} payment screenshots are waiting to be checked.")));
    }
    if ctx.backup_overdue {
        out.push(check("backup", "warning", "The last backup is overdue. Back up before you close."));
    }
    // ---- information
    let expenses: i64 = c.query_row(
        "SELECT COUNT(*) FROM expenses WHERE branch_id=?1 AND business_date<=?2 AND (status='submitted' OR (status='approved' AND payment_method IS NULL))",
        params![b.branch, b.date],
        |r| r.get(0),
    )?;
    if expenses > 0 {
        out.push(check("expenses_waiting", "info", format!("{expenses} expenses are waiting for approval or payment.")));
    }
    let late = late_lines(c, b)?.len();
    if late > 0 {
        out.push(check(
            "late_records",
            "info",
            format!("{late} records arrived after their day was closed. This close counts them as after-close adjustments."),
        ));
    }
    if last.is_none() {
        out.push(check("first_close", "info", "This is the first close. Sales before this day are not part of any close."));
    }
    if b.date == ctx.today && b.closed == 0 {
        out.push(check("still_trading", "info", "You can keep selling. Sales made after you close count in the next close."));
    }
    Ok(out)
}

fn canonical_sha(v: &Value) -> AppResult<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_string(v)?.as_bytes())))
}

fn load_close(c: &Connection, close_id: &str) -> AppResult<CloseView> {
    type Row = (String, String, String, String, String, String, Option<String>);
    let row: Option<Row> = c
        .query_row(
            "SELECT z.close_number, z.branch_id, z.business_date, z.closed_at, z.snapshot_json, z.sha256, u.display_name
             FROM day_closes z LEFT JOIN users u ON u.user_id=z.closed_by WHERE z.close_id=?1",
            [close_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .optional()?;
    let (number, branch, date, at, snap, sha, who) = row.ok_or_else(|| AppError::not_found("Close"))?;
    let v: Value = serde_json::from_str(&snap)?;
    let verified = canonical_sha(&v)? == sha;
    let report: DayReport = serde_json::from_value(v["report"].clone())?;
    let doc: ReceiptDoc = serde_json::from_value(v["doc"].clone())?;
    Ok(CloseView {
        close_id: close_id.into(),
        close_number: number,
        branch_id: branch,
        business_date: date,
        closed_at: at,
        closed_by_name: who,
        sha256: sha,
        verified,
        report,
        doc,
    })
}

impl AppCore {
    /// Day closes are the hub's records; a terminal shows the hub's.
    fn require_closing_computer(&self) -> AppResult<()> {
        if self.device().map(|d| d.mode == "terminal").unwrap_or(false) {
            return Err(AppError::conflict("Trading days are closed on the hub computer. Open End of day there."));
        }
        Ok(())
    }

    fn day_branch(&self, c: &Connection, s: &Session, requested: Option<&str>) -> AppResult<String> {
        match requested.filter(|b| !b.is_empty()) {
            Some(b) => {
                let b = validate::id(b, "Branch")?;
                if b != s.branch_id && !crate::branches::user_may_work_in(c, &s.user_id, s.has("branches.all"), &b)? {
                    return Err(AppError::forbidden("branches.all"));
                }
                let exists: bool = c.query_row("SELECT 1 FROM branches WHERE branch_id=?1", [&b], |_| Ok(true)).optional()?.is_some();
                if !exists {
                    return Err(AppError::not_found("Branch"));
                }
                Ok(b)
            }
            None => Ok(s.branch_id.clone()),
        }
    }

    fn day_date(c: &Connection, date: Option<&str>) -> AppResult<(String, String)> {
        let today = time::business_date(time::now(), &time::day(c)?)?;
        let d = match date.filter(|d| !d.is_empty()) {
            Some(d) => {
                time::validate_date(d)?;
                d.to_string()
            }
            None => today.clone(),
        };
        Ok((d, today))
    }

    fn day_context(&self, today: String) -> AppResult<Context> {
        let backup_overdue = self.backup_diagnostic().map(|d| d.state != "ok").unwrap_or(false);
        let d = self.device();
        Ok(Context {
            device_id: d.as_ref().map(|d| d.device_id.clone()).unwrap_or_default(),
            is_hub: d.map(|d| d.mode == "hub").unwrap_or(false),
            backup_overdue,
            today,
        })
    }

    /// X report: what the next close of this day would count, right now.
    /// Reads only; it can be run any number of times.
    pub fn day_x(&self, token: &str, date: Option<String>, branch_id: Option<String>) -> AppResult<DayReport> {
        let s = self.session(token)?;
        s.require("day.x_report")?;
        self.require_closing_computer()?;
        self.db.read(|c| {
            let branch = self.day_branch(c, &s, branch_id.as_deref())?;
            let (date, _) = Self::day_date(c, date.as_deref())?;
            report(c, &Bounds::load(c, &branch, &date)?, "x")
        })
    }

    /// The closing checks for a day: blocking, warning, information.
    pub fn day_checks(&self, token: &str, date: Option<String>, branch_id: Option<String>) -> AppResult<DayChecks> {
        let s = self.session(token)?;
        s.require("day.x_report")?;
        self.require_closing_computer()?;
        let today = self.db.read(|c| Self::day_date(c, None))?.1;
        let ctx = self.day_context(today)?;
        self.db.read(|c| {
            let branch = self.day_branch(c, &s, branch_id.as_deref())?;
            let (date, _) = Self::day_date(c, date.as_deref())?;
            let b = Bounds::load(c, &branch, &date)?;
            let checks = checks(c, &b, &ctx)?;
            let last_close = c
                .query_row(
                    "SELECT close_id, close_number, business_date, closed_at FROM day_closes WHERE branch_id=?1 ORDER BY business_date DESC LIMIT 1",
                    [&branch],
                    |r| {
                        Ok(json!({ "close_id": r.get::<_, String>(0)?, "close_number": r.get::<_, String>(1)?,
                                   "business_date": r.get::<_, String>(2)?, "closed_at": r.get::<_, String>(3)? }))
                    },
                )
                .optional()?;
            Ok(DayChecks {
                business_date: date,
                branch_id: branch,
                can_close: !checks.iter().any(|c| c.level == "blocking"),
                needs_acknowledgement: checks.iter().any(|c| c.level == "warning"),
                checks,
                last_close,
            })
        })
    }

    /// Z close: the permanent record of a trading day. Exactly once per
    /// branch and date; a retry with the same operation id returns the same
    /// close; nothing in it changes afterwards.
    pub fn day_close(&self, token: &str, req: CloseRequest) -> AppResult<CloseView> {
        let s = self.session(token)?;
        s.require("day.close")?;
        self.require_closing_computer()?;
        idempotency::validate_operation_id(&req.operation_id)?;
        time::validate_date(&req.business_date)?;
        let branch = self.db.read(|c| self.day_branch(c, &s, req.branch_id.as_deref()))?;
        let payload = json!({ "branch_id": branch, "business_date": req.business_date });
        if let Idem::Replay { result } = self.db.read(|c| idempotency::check(c, &req.operation_id, "day.close", &payload))? {
            let id = result["close_id"].as_str().unwrap_or_default().to_string();
            return self.db.read(|c| load_close(c, &id));
        }
        // Cash differences become cases before the checks look at them.
        if self.device().map(|d| d.mode != "terminal").unwrap_or(true) {
            self.db.write(crate::cases::sweep_cash_variances)?;
        }
        let today = self.db.read(|c| Self::day_date(c, None))?.1;
        let ctx = self.day_context(today)?;
        let actor = self.actor(&s, None);
        let close_id = self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "day.close", &payload)? {
                Idem::Replay { result } => return Ok(result["close_id"].as_str().unwrap_or_default().to_string()),
                Idem::New { payload_hash } => payload_hash,
            };
            let b = Bounds::load(tx, &branch, &req.business_date)?;
            let found = checks(tx, &b, &ctx)?;
            let blocking: Vec<&DayCheck> = found.iter().filter(|c| c.level == "blocking").collect();
            if let Some(first) = blocking.first() {
                return Err(AppError::conflict(first.message.clone()).with_details(json!({ "checks": found })));
            }
            if found.iter().any(|c| c.level == "warning") && !req.acknowledge_warnings {
                return Err(AppError::conflict("Some things need your attention before closing. Read them and confirm.")
                    .with_details(json!({ "checks": found, "needs_acknowledgement": true })));
            }
            let mut rep = report(tx, &b, "z")?;
            let code: String =
                tx.query_row("SELECT code FROM branches WHERE branch_id=?1", [&branch], |r| r.get(0)).unwrap_or_else(|_| "B".into());
            let number = format!("Z-{code}-{:05}", next_seq(tx, &format!("zclose:{branch}"))?);
            let closed_at = time::now_str();
            rep.close_number = Some(number.clone());
            rep.closed_by_name = Some(s.display_name.clone());
            rep.generated_at = closed_at.clone();
            let doc = crate::receipt::day_close_doc(tx, &rep)?;
            let snapshot = json!({ "format": CLOSE_FORMAT, "report": rep, "doc": doc });
            let sha = canonical_sha(&snapshot)?;
            let id = new_id();
            tx.execute(
                "INSERT INTO day_closes(close_id, close_number, branch_id, business_date, format_version, snapshot_json, sha256, sale_count,
                    net_sales_minor, tax_minor, late_count, variance_minor, closed_by, closed_at, operation_id)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    id,
                    number,
                    branch,
                    req.business_date,
                    CLOSE_FORMAT,
                    serde_json::to_string(&snapshot)?,
                    sha,
                    rep.total.sale_count,
                    rep.total.net_sales_minor,
                    rep.total.tax_minor,
                    rep.late.len() as i64,
                    rep.cash.variance_minor,
                    s.user_id,
                    closed_at,
                    req.operation_id
                ],
            )?;
            // The records this close counted (the same sets as the totals,
            // read in the same transaction).
            for (set, late) in [(Set::Day, 0), (Set::Late, 1)] {
                tx.execute(
                    &format!(
                        "INSERT INTO day_close_items(ref_kind, ref_id, close_id, business_date, late)
                         SELECT 'sale', x.sale_id, ?5, x.business_date, {late} FROM sales x WHERE {}",
                        set_where("sale", "sale_id", set)
                    ),
                    params![b.branch, b.date, b.start, b.closed, id],
                )?;
                tx.execute(
                    &format!(
                        "INSERT INTO day_close_items(ref_kind, ref_id, close_id, business_date, late)
                         SELECT 'refund', x.refund_id, ?5, x.business_date, {late} FROM refunds x WHERE {}",
                        set_where("refund", "refund_id", set)
                    ),
                    params![b.branch, b.date, b.start, b.closed, id],
                )?;
            }
            for d in &rep.drawers {
                tx.execute(
                    "INSERT INTO day_close_items(ref_kind, ref_id, close_id, business_date, late) VALUES ('shift',?1,?2,?3,?4)",
                    params![d.shift_id, id, d.business_date, d.late as i64],
                )?;
            }
            let result = json!({ "close_id": id, "close_number": number });
            audit::record(
                tx,
                &actor,
                "day.closed",
                "day_close",
                Some(&id),
                None,
                Some(&json!({ "close_number": number, "branch_id": branch, "business_date": req.business_date, "sha256": sha,
                              "net_sales_minor": rep.total.net_sales_minor, "late_count": rep.late.len() })),
            )?;
            idempotency::complete(tx, &req.operation_id, "day.close", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &result)?;
            Ok(id)
        })?;
        self.db.read(|c| load_close(c, &close_id))
    }

    pub fn day_closes_list(&self, token: &str, branch_id: Option<String>, limit: Option<i64>) -> AppResult<Vec<CloseRow>> {
        let s = self.session(token)?;
        s.require("day.x_report")?;
        let limit = validate::limit(limit, 60, 400);
        self.db.read(|c| {
            let scope = match branch_id.as_deref().filter(|b| !b.is_empty()) {
                Some(_) => Some(self.day_branch(c, &s, branch_id.as_deref())?),
                None => crate::branches::list_scope(c, &s)?,
            };
            let mut st = c.prepare(&format!(
                "SELECT z.close_id, z.close_number, z.branch_id, COALESCE(b.name,''), z.business_date, z.sale_count, z.net_sales_minor, z.tax_minor,
                        z.late_count, z.variance_minor, u.display_name, z.closed_at
                 FROM day_closes z LEFT JOIN branches b ON b.branch_id=z.branch_id LEFT JOIN users u ON u.user_id=z.closed_by
                 WHERE (?1 IS NULL OR z.branch_id=?1) ORDER BY z.business_date DESC, z.closed_at DESC LIMIT {limit}"
            ))?;
            let rows = st
                .query_map(params![scope], |r| {
                    Ok(CloseRow {
                        close_id: r.get(0)?,
                        close_number: r.get(1)?,
                        branch_id: r.get(2)?,
                        branch_name: r.get(3)?,
                        business_date: r.get(4)?,
                        sale_count: r.get(5)?,
                        net_sales_minor: r.get(6)?,
                        tax_minor: r.get(7)?,
                        late_count: r.get(8)?,
                        variance_minor: r.get(9)?,
                        closed_by_name: r.get(10)?,
                        closed_at: r.get(11)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// A close exactly as it was closed (from its stored record).
    pub fn day_close_get(&self, token: &str, close_id: &str) -> AppResult<CloseView> {
        let s = self.session(token)?;
        s.require("day.x_report")?;
        let id = validate::id(close_id, "Close")?;
        self.db.read(|c| {
            let v = load_close(c, &id)?;
            self.day_branch(c, &s, Some(&v.branch_id))?;
            Ok(v)
        })
    }

    /// The close as a PDF, drawn from the stored document (never rebuilt
    /// from today's settings).
    pub fn day_close_pdf(&self, token: &str, close_id: &str) -> AppResult<Value> {
        let v = self.day_close_get(token, close_id)?;
        let bytes = crate::pdf::bitmap_pdf(&v.doc.to_bitmap(), &v.close_number);
        Ok(
            json!({ "file_name": format!("{}-{}.pdf", v.close_number, v.business_date), "base64": crate::ids::b64(&bytes), "text": v.doc.to_text() }),
        )
    }

    /// The X report as a PDF (current totals; marked as not a close).
    pub fn day_x_pdf(&self, token: &str, date: Option<String>, branch_id: Option<String>) -> AppResult<Value> {
        let rep = self.day_x(token, date, branch_id)?;
        let doc = self.db.read(|c| crate::receipt::day_close_doc(c, &rep))?;
        let bytes = crate::pdf::bitmap_pdf(&doc.to_bitmap(), "X report");
        Ok(json!({ "file_name": format!("X-{}.pdf", rep.business_date), "base64": crate::ids::b64(&bytes), "text": doc.to_text() }))
    }

    /// Opening the store: what to look at before trading. Advice, never a
    /// hard stop; selling never depends on it.
    pub fn day_opening(&self, token: &str) -> AppResult<Opening> {
        let s = self.session(token)?;
        s.require("day.x_report")?;
        let today = self.db.read(|c| Self::day_date(c, None))?.1;
        let ctx = self.day_context(today.clone())?;
        let terminal = self.device().map(|d| d.mode == "terminal").unwrap_or(false);
        let out = self.db.read(|c| {
            let mut out = vec![];
            let branch = s.branch_id.clone();
            if !terminal {
                let any_close: bool = c.query_row("SELECT 1 FROM day_closes WHERE branch_id=?1 LIMIT 1", [&branch], |_| Ok(true)).optional()?.is_some();
                let unclosed: Option<String> = c.query_row(
                    "SELECT MAX(x.business_date) FROM sales x WHERE x.branch_id=?1 AND x.business_date<?2
                       AND NOT EXISTS (SELECT 1 FROM day_close_items i WHERE i.ref_kind='sale' AND i.ref_id=x.sale_id)
                       AND x.business_date >= COALESCE((SELECT MIN(business_date) FROM day_closes WHERE branch_id=?1), ?2)",
                    params![branch, today],
                    |r| r.get(0),
                )?;
                match (any_close, unclosed) {
                    (_, Some(d)) => out.push(check("prior_day", "warning", format!("{d} is not closed yet. Close it in End of day."))),
                    (false, None) => out.push(check("prior_day", "info", "Close each trading day to keep a permanent record of it.")),
                    (true, None) => out.push(check("prior_day", "ok", "The last trading day is closed.")),
                }
                let high: i64 = c.query_row(
                    "SELECT COUNT(*) FROM cases WHERE branch_id=?1 AND status NOT IN ('resolved','dismissed') AND severity IN ('medium','high')",
                    [&branch],
                    |r| r.get(0),
                )?;
                if high > 0 {
                    out.push(check("open_cases", "warning", format!("{high} cash differences are still being looked into.")));
                }
            }
            out.push(if ctx.backup_overdue {
                check("backup", "warning", "The last backup is overdue. Back up before trading.")
            } else {
                check("backup", "ok", "Backups are up to date.")
            });
            let dead: i64 = c.query_row("SELECT COUNT(*) FROM sync_dead_letters WHERE status='open'", [], |r| r.get(0))?;
            if dead > 0 {
                out.push(check("sync", "warning", format!("{dead} records from other computers could not be saved. See System → Sync.")));
            }
            if terminal {
                let ss: crate::sync::SyncSettings = settings::get(c, crate::sync::KEY_SYNC)?;
                if ss.blocked_reason.is_some() {
                    out.push(check("sync", "warning", "Sync with the hub is paused. See System → Sync."));
                }
            }
            match crate::registers::for_device(c, &ctx.device_id)? {
                (Some(r), drawer) => {
                    let name: String = c.query_row("SELECT name FROM registers WHERE register_id=?1", [&r], |x| x.get(0))?;
                    if drawer.is_none() {
                        out.push(check("register", "warning", format!("This computer is {name}, but the register has no cash drawer.")));
                    } else {
                        out.push(check("register", "ok", format!("This computer is {name}.")));
                    }
                }
                (None, _) => out.push(check("register", "warning", "This computer is not set as a register. Shifts here will not name one.")),
            }
            let open: Option<(String, i64)> = c
                .query_row(
                    "SELECT shift_number, opening_float_minor FROM shifts WHERE device_id=?1 AND status='open' ORDER BY opened_at DESC LIMIT 1",
                    [&ctx.device_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            out.push(match open {
                Some((n, f)) => check("float", "ok", format!("Shift {n} is open with a float of {}.", money(c, f)?)),
                None => check("float", "info", "Open a shift with your float to start selling."),
            });
            let failed: i64 = c.query_row(
                "SELECT COUNT(*) FROM print_jobs WHERE status='failed' AND updated_at >= ?1",
                [time::fmt(time::now() - chrono::Duration::hours(24))],
                |r| r.get(0),
            )?;
            if failed > 0 {
                out.push(check("printer", "warning", format!("{failed} print jobs failed in the last day. Check the printer.")));
            }
            Ok(out)
        })?;
        let attention = out.iter().any(|c| c.level == "warning" || c.level == "blocking");
        Ok(Opening { business_date: today, verdict: if attention { "attention" } else { "ready" }.into(), checks: out })
    }
}
