//! Cash-flow Radar (Wave 8, docs/INTELLIGENCE_AND_EVIDENCE.md).
//!
//! What money the records say will go out and come in over the next 7, 14,
//! 30, 60 or 90 days, computed here, deterministically, in integer minor
//! units. The assistant only explains it; it never calculates it.
//!
//! It is not a bank balance: AMWAPOS does not see the bank. The only cash it
//! states is cash recorded in the store (drawers, petty cash, riders).
//!
//! Bands:
//! * Known: posted supplier invoices still open (by due date), approved
//!   expenses not yet paid; payments and credits already on the suppliers'
//!   accounts but not matched to an invoice reduce what is owed.
//! * Scheduled: repeating expenses on their next dates, until an expense is
//!   actually created for a date (then that expense counts, once).
//! * Exposure: likely but not owed yet: purchase orders not yet invoiced
//!   (less what has been invoiced against them, so nothing counts twice),
//!   approved supplier invoices not yet posted, expenses awaiting approval;
//!   and money that may come in: credits expected from supplier returns,
//!   what customers owe (by age, never with an invented date).
//! * Scenario: sales income if the last 28 days repeat. Shown only when
//!   there are 28 days of sales; labelled as a scenario, never as a fact.
//!
//! Anything due before today is shown as due today and marked overdue.
//! Anything without a date is listed apart, never spread over the days.

use chrono::{Duration, NaiveDate};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::auth::Session;
use crate::error::{AppError, AppResult};
use crate::service::AppCore;
use crate::time;

pub const HORIZONS: [i64; 5] = [7, 14, 30, 60, 90];
/// Days of sales history a scenario needs.
pub const SCENARIO_DAYS: i64 = 28;
/// A week holding at least this share of the next 30 days' known and
/// scheduled outflow is a pressure week when no scenario is available.
pub const PRESSURE_SHARE_PCT: i64 = 40;

pub const NOT_A_BANK_BALANCE: &str =
    "This is not your bank balance. AMWAPOS does not see your bank; it shows what your records say will go out and come in.";

fn d(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10).unwrap_or(s), "%Y-%m-%d").ok()
}

/// One line of the radar.
#[derive(Debug, Clone)]
struct Line {
    band: &'static str,
    /// out | in
    direction: &'static str,
    kind: &'static str,
    date: Option<NaiveDate>,
    overdue: bool,
    amount: i64,
    label: String,
    source_type: &'static str,
    source_id: String,
    link: String,
}

impl Line {
    fn json(&self) -> Value {
        json!({
            "band": self.band, "direction": self.direction, "kind": self.kind,
            "date": self.date.map(|x| x.to_string()), "overdue": self.overdue, "amount_minor": self.amount,
            "label": self.label, "source": { "type": self.source_type, "id": self.source_id, "link": self.link },
        })
    }
}

/// Place a dated item: before today means due today, overdue.
fn place(due: Option<NaiveDate>, today: NaiveDate) -> (Option<NaiveDate>, bool) {
    match due {
        Some(x) if x < today => (Some(today), true),
        other => (other, false),
    }
}

/// (id, number, description, total, status, date, recurring id)
type ExpenseRow = (String, String, String, i64, String, String, Option<String>);
/// (id, number, supplier, total, due date)
type UnpostedRow = (String, String, String, Option<i64>, Option<String>);

fn lines(c: &Connection, today: NaiveDate, end: NaiveDate) -> AppResult<Vec<Line>> {
    let mut out = vec![];
    // ---- Known: posted supplier invoices still open ---------------------
    for it in crate::payables::all_open_items(c)? {
        if it.outstanding <= 0 {
            continue;
        }
        let (date, overdue) = place(d(&it.due_date), today);
        out.push(Line {
            band: "known",
            direction: "out",
            kind: "supplier_invoice",
            date,
            overdue,
            amount: it.outstanding,
            label: format!("{} · {}", it.supplier_name, it.invoice_number.clone().unwrap_or(it.number.clone())),
            source_type: "supplier_invoice",
            source_id: it.invoice_id.clone(),
            link: "/admin/payables".into(),
        });
    }
    // Paid or credited on a supplier's account but not matched to an invoice:
    // it reduces what is owed (undated).
    let (credits, payments): (i64, i64) = c.query_row(
        "SELECT
           (SELECT COALESCE(SUM(cr.amount_minor - COALESCE((SELECT SUM(a.amount_minor) FROM ap_allocations a WHERE a.credit_id=cr.credit_id AND a.status='active'),0)),0)
              FROM ap_credits cr WHERE cr.status='open'),
           (SELECT COALESCE(SUM(p.amount_minor - COALESCE((SELECT SUM(a.amount_minor) FROM ap_allocations a WHERE a.payment_id=p.payment_id AND a.status='active'),0)),0)
              FROM ap_payments p WHERE p.status='posted')",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if credits + payments > 0 {
        out.push(Line {
            band: "known",
            direction: "in",
            kind: "unapplied_supplier_balance",
            date: None,
            overdue: false,
            amount: credits + payments,
            label: "Already paid or credited on supplier accounts, not yet matched to an invoice".into(),
            source_type: "payables",
            source_id: String::new(),
            link: "/admin/payables".into(),
        });
    }
    // ---- Expenses ----------------------------------------------------------
    let mut st = c.prepare(
        "SELECT expense_id, number, description, total_minor, status, business_date, recurring_id
         FROM expenses WHERE status IN ('draft','submitted','approved')",
    )?;
    let rows: Vec<ExpenseRow> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?
        .collect::<Result<_, _>>()?;
    for (id, number, desc, total, status, date, recurring) in rows {
        let (band, kind) = match (status.as_str(), recurring.is_some()) {
            ("approved", _) => ("known", "expense_approved"),
            ("submitted", _) => ("exposure", "expense_submitted"),
            // A draft a repeating expense made on its date.
            ("draft", true) => ("scheduled", "expense_recurring_draft"),
            _ => continue,
        };
        let (date, overdue) = place(d(&date), today);
        out.push(Line {
            band,
            direction: "out",
            kind,
            date,
            overdue,
            amount: total,
            label: format!("{number} · {desc}"),
            source_type: "expense",
            source_id: id,
            link: "/admin/expenses".into(),
        });
    }
    // ---- Scheduled: repeating expenses on their next dates ------------------
    let mut st =
        c.prepare("SELECT recurring_id, name, net_minor + vat_minor, cadence, day, next_date FROM expense_recurring WHERE active=1")?;
    let recs: Vec<(String, String, i64, String, i64, String)> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<Result<_, _>>()?;
    for (rid, name, amount, cadence, day, next) in recs {
        let Some(mut at) = d(&next) else { continue };
        let mut guard = 0;
        while at <= end && guard < 120 {
            guard += 1;
            // An expense already made for this date counts instead (once).
            let made = c
                .query_row(
                    "SELECT 1 FROM expenses WHERE recurring_id=?1 AND business_date=?2 AND status<>'void'",
                    params![rid, at.to_string()],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !made {
                let (date, overdue) = place(Some(at), today);
                out.push(Line {
                    band: "scheduled",
                    direction: "out",
                    kind: "recurring_expense",
                    date,
                    overdue,
                    amount,
                    label: name.clone(),
                    source_type: "recurring_expense",
                    source_id: rid.clone(),
                    link: "/admin/expenses".into(),
                });
            }
            let next = crate::expenses::next_due(&cadence, day, &(at + Duration::days(1)).to_string())?;
            match d(&next) {
                Some(n) if n > at => at = n,
                _ => break,
            }
        }
    }
    // ---- Exposure: purchase orders not yet invoiced ----------------------
    let mut st = c.prepare(
        "SELECT p.po_id, p.po_number, s.name, p.total_minor, p.expected_at,
                COALESCE((SELECT SUM(i.total_minor) FROM supplier_invoices i
                          WHERE i.po_id=p.po_id AND i.doc_type='invoice' AND i.status<>'rejected' AND i.posting<>'reversed'),0)
         FROM purchase_orders p JOIN suppliers s ON s.supplier_id=p.supplier_id
         WHERE p.status IN ('ordered','partially_received')",
    )?;
    let pos: Vec<(String, String, String, i64, Option<String>, i64)> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<Result<_, _>>()?;
    for (id, number, supplier, total, expected, invoiced) in pos {
        let rest = total - invoiced;
        if rest <= 0 {
            continue;
        }
        let (date, overdue) = place(expected.as_deref().and_then(d), today);
        out.push(Line {
            band: "exposure",
            direction: "out",
            kind: "purchase_order",
            date,
            overdue,
            amount: rest,
            label: format!("{number} · {supplier}"),
            source_type: "purchase_order",
            source_id: id.clone(),
            link: format!("/admin/purchase-orders/{id}"),
        });
    }
    // Approved supplier invoices not posted yet (not owed until posted).
    let mut st = c.prepare(
        "SELECT i.invoice_id, i.number, s.name, i.total_minor, i.due_date FROM supplier_invoices i JOIN suppliers s ON s.supplier_id=i.supplier_id
         WHERE i.doc_type='invoice' AND i.status='approved' AND i.posting='not_posted'",
    )?;
    let inv: Vec<UnpostedRow> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<_, _>>()?;
    for (id, number, supplier, total, due) in inv {
        let Some(total) = total.filter(|t| *t > 0) else { continue };
        let (date, overdue) = place(due.as_deref().and_then(d), today);
        out.push(Line {
            band: "exposure",
            direction: "out",
            kind: "supplier_invoice_unposted",
            date,
            overdue,
            amount: total,
            label: format!("{number} · {supplier}"),
            source_type: "supplier_invoice",
            source_id: id,
            link: "/admin/payables".into(),
        });
    }
    // Money that may come in: credits expected from supplier returns.
    let mut st = c.prepare(
        "SELECT r.return_id, r.number, s.name, r.expected_credit_minor FROM supplier_returns r JOIN suppliers s ON s.supplier_id=r.supplier_id
         WHERE r.status='confirmed' AND r.credit_invoice_id IS NULL AND r.expected_credit_minor > 0",
    )?;
    let rets: Vec<(String, String, String, i64)> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
    for (id, number, supplier, amount) in rets {
        out.push(Line {
            band: "exposure",
            direction: "in",
            kind: "expected_supplier_credit",
            date: None,
            overdue: false,
            amount,
            label: format!("{number} · {supplier}"),
            source_type: "supplier_return",
            source_id: id.clone(),
            link: format!("/admin/supplier-returns/{id}"),
        });
    }
    Ok(out)
}

fn sum(ls: &[&Line]) -> i64 {
    ls.iter().map(|l| l.amount).sum()
}

impl AppCore {
    pub fn cashflow_radar(&self, token: &str, horizon: Option<i64>) -> AppResult<Value> {
        let s: Session = self.session(token)?;
        s.require("cashflow.view")?;
        if self.require_back_office_writable().is_err() {
            return Err(AppError::conflict("The Cash-flow Radar is on the main computer. Open it there."));
        }
        let horizon = horizon.unwrap_or(30);
        if !HORIZONS.contains(&horizon) {
            return Err(AppError::validation("Choose 7, 14, 30, 60 or 90 days."));
        }
        self.db.read(|c| {
            let today = d(&time::business_date(time::now(), &time::day(c)?)?).ok_or_else(|| AppError::internal("business date"))?;
            let max_end = today + Duration::days(*HORIZONS.last().unwrap() - 1);
            let all = lines(c, today, max_end)?;
            let end = today + Duration::days(horizon - 1);
            let within = |l: &&Line| l.date.map(|x| x <= end).unwrap_or(false);
            let dated_out: Vec<&Line> = all.iter().filter(|l| l.direction == "out" && l.date.is_some()).collect();

            // ---- Cash recorded in the store (not the bank) ----------------
            let mut drawers = vec![];
            let open: Vec<String> = {
                let mut st = c.prepare("SELECT shift_id FROM shifts WHERE status='open' ORDER BY opened_at")?;
                let r = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
                r
            };
            for id in open {
                let sm = crate::shifts::shift_summary(c, &id)?;
                drawers.push(json!({ "shift_id": sm.shift_id, "shift_number": sm.shift_number, "cashier": sm.cashier_name,
                    "till": sm.device_name, "expected_cash_minor": sm.expected_cash_minor }));
            }
            let drawers_total: i64 = drawers.iter().filter_map(|x| x["expected_cash_minor"].as_i64()).sum();
            // The last count of each till: counted against expected.
            let mut st = c.prepare(
                "SELECT s.shift_number, s.counted_cash_minor, s.expected_cash_minor, s.variance_minor, s.closed_at, d.name
                 FROM shifts s LEFT JOIN devices d ON d.device_id=s.device_id
                 WHERE s.status='closed' AND s.closed_at = (SELECT MAX(x.closed_at) FROM shifts x WHERE x.device_id=s.device_id AND x.status='closed')",
            )?;
            let counts: Vec<Value> = st
                .query_map([], |r| {
                    Ok(json!({ "shift_number": r.get::<_, String>(0)?, "counted_cash_minor": r.get::<_, Option<i64>>(1)?,
                        "expected_cash_minor": r.get::<_, Option<i64>>(2)?, "difference_minor": r.get::<_, Option<i64>>(3)?,
                        "closed_at": r.get::<_, Option<String>>(4)?, "till": r.get::<_, Option<String>>(5)? }))
                })?
                .collect::<Result<_, _>>()?;
            let mut st = c.prepare(
                "SELECT f.fund_id, f.name, (SELECT COALESCE(SUM(e.amount_minor),0) FROM petty_cash_entries e WHERE e.fund_id=f.fund_id)
                 FROM petty_cash_funds f WHERE f.active=1 ORDER BY f.name",
            )?;
            let petty: Vec<Value> = st
                .query_map([], |r| Ok(json!({ "fund_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "balance_minor": r.get::<_, i64>(2)? })))?
                .collect::<Result<_, _>>()?;
            let petty_total: i64 = petty.iter().filter_map(|x| x["balance_minor"].as_i64()).sum();
            let riders: Vec<(String, String)> = {
                let mut st = c.prepare(
                    "SELECT DISTINCT u.user_id, u.display_name FROM users u
                     WHERE u.user_id IN (SELECT held_by FROM sale_collections WHERE held_by IS NOT NULL
                                           AND collection_id NOT IN (SELECT collection_id FROM rider_handover_items))",
                )?;
                let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
                r
            };
            let mut riders_held = 0i64;
            for (id, name) in riders {
                riders_held += crate::riders::rider_cash(c, &id, &name, &s.branch_id)?.held_minor;
            }

            // ---- What customers owe (by age; no invented dates) -----------
            let as_of = today.to_string();
            let custs: Vec<String> = {
                let mut st = c.prepare("SELECT DISTINCT customer_id FROM customer_ledger")?;
                let r = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
                r
            };
            let mut recv = crate::credit::Ageing::default();
            let mut owing = 0;
            for id in custs {
                let a = crate::credit::customer_ageing(c, &id, &as_of)?;
                if a.total() > 0 {
                    owing += 1;
                    recv.current_minor += a.current_minor;
                    recv.d1_30_minor += a.d1_30_minor;
                    recv.d31_60_minor += a.d31_60_minor;
                    recv.d61_90_minor += a.d61_90_minor;
                    recv.d90_plus_minor += a.d90_plus_minor;
                }
            }

            // ---- Scenario: the last 28 days of sales, repeated ------------
            let first: Option<String> = c.query_row("SELECT MIN(business_date) FROM sales WHERE status='completed'", [], |r| r.get(0))?;
            let days_of_history = first.as_deref().and_then(d).map(|f| (today - f).num_days()).unwrap_or(0);
            let scenario = if days_of_history >= SCENARIO_DAYS {
                let from = (today - Duration::days(SCENARIO_DAYS)).to_string();
                let to = (today - Duration::days(1)).to_string();
                let (sales, refunds): (i64, i64) = c.query_row(
                    "SELECT (SELECT COALESCE(SUM(s.total_minor),0) FROM sales s WHERE s.status='completed' AND s.business_date BETWEEN ?1 AND ?2
                               AND NOT EXISTS (SELECT 1 FROM sale_voids v WHERE v.sale_id=s.sale_id)),
                            (SELECT COALESCE(SUM(total_minor),0) FROM refunds WHERE business_date BETWEEN ?1 AND ?2)",
                    params![from, to],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let net = sales - refunds;
                Some(json!({
                    "available": true, "basis": "estimate", "days": SCENARIO_DAYS, "from": from, "to": to,
                    "net_sales_minor": net, "daily_average_minor": net / SCENARIO_DAYS,
                    "assumption": "If sales over the coming days are like the last 28 days (sales less refunds). Card and transfer money may reach the bank later.",
                }))
            } else {
                None
            };
            let daily = scenario.as_ref().and_then(|x| x["daily_average_minor"].as_i64());

            // ---- Totals per horizon -----------------------------------------
            let horizons: Vec<Value> = HORIZONS
                .iter()
                .map(|h| {
                    let e = today + Duration::days(h - 1);
                    let inw = |l: &&&Line| l.date.map(|x| x <= e).unwrap_or(false);
                    let band = |b: &str| sum(&dated_out.iter().filter(|l| l.band == b).filter(inw).copied().collect::<Vec<_>>());
                    json!({ "days": h, "until": e.to_string(), "known_out_minor": band("known"), "scheduled_out_minor": band("scheduled"),
                        "exposure_out_minor": band("exposure"), "scenario_in_minor": daily.map(|x| x * h) })
                })
                .collect();

            // ---- Weeks and pressure windows ---------------------------------
            let weeks_n = (horizon + 6) / 7;
            let next30: i64 = sum(
                &dated_out
                    .iter()
                    .filter(|l| matches!(l.band, "known" | "scheduled") && l.date.map(|x| x <= today + Duration::days(29)).unwrap_or(false))
                    .copied()
                    .collect::<Vec<_>>(),
            );
            let weeks: Vec<Value> = (0..weeks_n)
                .map(|w| {
                    let ws = today + Duration::days(w * 7);
                    let we = (ws + Duration::days(6)).min(end);
                    let days = (we - ws).num_days() + 1;
                    let inw = |l: &&&Line| l.date.map(|x| x >= ws && x <= we).unwrap_or(false);
                    let band = |b: &str| sum(&dated_out.iter().filter(|l| l.band == b).filter(inw).copied().collect::<Vec<_>>());
                    let (k, sch, ex) = (band("known"), band("scheduled"), band("exposure"));
                    let scen = daily.map(|x| x * days);
                    let pressure = match scen {
                        Some(sin) => k + sch > sin,
                        None => next30 > 0 && w < 5 && (k + sch) * 100 >= next30 * PRESSURE_SHARE_PCT,
                    };
                    json!({ "from": ws.to_string(), "to": we.to_string(), "known_out_minor": k, "scheduled_out_minor": sch,
                        "exposure_out_minor": ex, "scenario_in_minor": scen, "pressure": pressure })
                })
                .collect();

            let mut shown: Vec<&Line> = all.iter().filter(|l| l.date.is_none() || within(l)).collect();
            shown.sort_by_key(|l| (l.date.is_none(), l.date, std::cmp::Reverse(l.amount)));
            let undated_out: i64 = sum(&all.iter().filter(|l| l.direction == "out" && l.date.is_none()).collect::<Vec<_>>());
            let offsets: i64 = sum(&all.iter().filter(|l| l.kind == "unapplied_supplier_balance").collect::<Vec<_>>());
            let expected_in: i64 = sum(&all.iter().filter(|l| l.kind == "expected_supplier_credit").collect::<Vec<_>>());
            let overdue_out: i64 = sum(&all.iter().filter(|l| l.direction == "out" && l.overdue).collect::<Vec<_>>());
            Ok(json!({
                "as_of": today.to_string(),
                "horizon_days": horizon,
                "until": end.to_string(),
                "basis": "derived",
                "not_a_bank_balance": NOT_A_BANK_BALANCE,
                "cash_recorded": {
                    "drawers": drawers, "drawers_expected_minor": drawers_total, "last_counts": counts,
                    "petty_cash": petty, "petty_cash_minor": petty_total, "riders_held_minor": riders_held,
                    "total_minor": drawers_total + petty_total + riders_held,
                    "note": "Cash the store's records say is in drawers, petty cash and with riders now. Not the bank.",
                },
                "horizons": horizons,
                "weeks": weeks,
                "lines": shown.iter().map(|l| l.json()).collect::<Vec<_>>(),
                "overdue_out_minor": overdue_out,
                "undated_out_minor": undated_out,
                "unapplied_supplier_balance_minor": offsets,
                "expected_supplier_credits_minor": expected_in,
                "receivables": {
                    "basis": "fact", "customers": owing, "total_minor": recv.total(), "current_minor": recv.current_minor,
                    "d1_30_minor": recv.d1_30_minor, "d31_60_minor": recv.d31_60_minor, "d61_90_minor": recv.d61_90_minor,
                    "d90_plus_minor": recv.d90_plus_minor,
                    "note": "What customers owe, by age. When it will be paid is not known, so it has no date here.",
                },
                "scenario": scenario.unwrap_or_else(|| json!({ "available": false,
                    "reason": format!("Fewer than {SCENARIO_DAYS} days of sales: no sales scenario is shown.") })),
                "formulas": [
                    "Known out = open amount of each posted supplier invoice on its due date + approved expenses not yet paid on their date; anything due earlier counts today, marked overdue.",
                    "Already paid or credited on supplier accounts but not matched to an invoice reduces what is owed; it is shown apart, without a date.",
                    "Scheduled out = each repeating expense on each of its next dates in the period, unless an expense was already made for that date (then that expense counts instead, once).",
                    "Exposure out = purchase orders still open less what has been invoiced against them + approved supplier invoices not yet posted + expenses awaiting approval. Exposure in = credits expected from supplier returns.",
                    "Scenario in = (sales less refunds over the last 28 days) / 28 x days. Shown only with 28 days of sales.",
                    "A pressure week: known + scheduled out is more than the scenario income for that week; without a scenario, a week holding 40% or more of the next 30 days' known + scheduled out.",
                    "All amounts are integer minor units (fils). Nothing here is a bank balance.",
                ],
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earlier_dates_count_today_and_are_marked_overdue() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 10).unwrap();
        assert_eq!(place(NaiveDate::from_ymd_opt(2026, 10, 1), today), (Some(today), true));
        assert_eq!(place(Some(today), today), (Some(today), false));
        assert_eq!(place(None, today), (None, false));
    }
}
