//! Lots (batches) and expiry, worked out from the one stock truth.
//!
//! Stock is `stock_movements`, nothing else. A lot holds no quantity of its
//! own. Its balance comes from replaying the product's movements in time
//! order (`replay`):
//! - a movement tagged with the lot is evidence (receiving into it, waste or
//!   a count from it);
//! - a movement without a lot (a till sale, a refund, an adjustment) first
//!   uses stock that is in no lot (the older stock from before batches were
//!   kept), then the lots, first-expiring-first-out. That part is an
//!   estimate: it is computed when read, never stored, and always shown as
//!   an estimate;
//! - when evidence says a lot still had stock the estimate had used, the
//!   estimate moves to the next lot. Evidence always wins.
//!
//! The balances of all lots plus the stock in no lot equal the stock on hand
//! exactly. Replaying the same movements gives the same answer in whatever
//! order they reached this computer, because only their own time counts.

use std::collections::HashMap;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::{new_id, next_seq};
use crate::inventory::{apply_movement_lot, current_qty, Movement};
use crate::service::AppCore;
use crate::settings::{self, InventorySettings};
use crate::time;
use crate::validate;

/// Batch details given at receiving (or when counting stock into a batch).
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct LotInput {
    #[serde(default)]
    pub supplier_lot_code: Option<String>,
    #[serde(default)]
    pub expires_on: Option<String>,
    #[serde(default)]
    pub manufactured_on: Option<String>,
    /// use_by | best_before (default: the product's setting)
    #[serde(default)]
    pub expiry_kind: Option<String>,
    /// "document" when the date was read from a supplier document and a
    /// person confirmed it.
    #[serde(default)]
    pub expiry_source: Option<String>,
    /// The person has seen the date warnings and keeps the dates.
    #[serde(default)]
    pub confirm_warnings: bool,
}

impl LotInput {
    pub fn is_empty(&self) -> bool {
        [&self.supplier_lot_code, &self.expires_on, &self.manufactured_on]
            .iter()
            .all(|x| x.as_deref().map(str::trim).unwrap_or("").is_empty())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LotFacts {
    pub lot_id: String,
    pub lot_number: String,
    pub product_id: String,
    pub branch_id: String,
    pub location_id: Option<String>,
    pub supplier_id: Option<String>,
    pub po_id: Option<String>,
    pub receipt_id: Option<String>,
    pub supplier_lot_code: Option<String>,
    pub received_at: String,
    pub manufactured_on: Option<String>,
    pub expires_on: Option<String>,
    pub expiry_kind: Option<String>,
    pub expiry_source: String,
    pub qty_received_milli: i64,
    pub unit_cost_minor: i64,
    pub provenance: String,
    pub corrected: bool,
}

/// One lot after the replay.
#[derive(Debug, Clone, Serialize)]
pub struct LotState {
    #[serde(flatten)]
    pub facts: LotFacts,
    /// Received into this lot (receiving, or counted into it).
    pub in_milli: i64,
    /// Taken from this lot by recorded evidence (waste, count), net of reversals.
    pub explicit_out_milli: i64,
    /// Estimated as sold from this lot (first-expiring-first-out). Not observed.
    pub estimated_out_milli: i64,
    pub balance_milli: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProductLots {
    pub product_id: String,
    pub branch_id: String,
    pub stock_milli: i64,
    /// Stock in no batch (from before batches were kept, refunds, …).
    pub unlotted_milli: i64,
    pub lots: Vec<LotState>,
}

const DATE_MIN: &str = "2000-01-01";
const DATE_MAX: &str = "2100-12-31";

fn parse_date(s: &str, what: &str) -> AppResult<String> {
    let d =
        NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").map_err(|_| AppError::validation(format!("Enter the {what} as YYYY-MM-DD.")))?;
    let out = d.to_string();
    if out.as_str() < DATE_MIN || out.as_str() > DATE_MAX {
        return Err(AppError::validation(format!("The {what} is not a real date for stock.")));
    }
    Ok(out)
}

/// The batch details, checked. Impossible dates are refused; suspicious ones
/// need `confirm_warnings` (the error lists them).
pub(crate) fn check_lot(c: &Connection, input: &LotInput, received_on: &str, product_kind: Option<String>) -> AppResult<LotInput> {
    let inv: InventorySettings = settings::get(c, settings::KEY_INVENTORY)?;
    let code = input.supplier_lot_code.as_deref().map(str::trim).filter(|x| !x.is_empty()).map(|x| x.chars().take(60).collect::<String>());
    let exp = input.expires_on.as_deref().filter(|x| !x.trim().is_empty()).map(|x| parse_date(x, "expiry date")).transpose()?;
    let mfg = input.manufactured_on.as_deref().filter(|x| !x.trim().is_empty()).map(|x| parse_date(x, "production date")).transpose()?;
    let kind = match input.expiry_kind.as_deref().filter(|k| !k.is_empty()).map(String::from).or(product_kind) {
        Some(k) if k == "use_by" || k == "best_before" => Some(k),
        Some(_) => return Err(AppError::validation("The date kind must be expiry (use by) or best before.")),
        None => exp.as_ref().map(|_| "use_by".to_string()),
    };
    let source = match input.expiry_source.as_deref() {
        None | Some("person") => "person",
        Some("document") => "document",
        Some(_) => return Err(AppError::validation("Unknown source of the expiry date.")),
    };
    let mut warnings = vec![];
    if let (Some(e), Some(m)) = (&exp, &mfg) {
        if e < m {
            warnings.push(format!("The expiry date {e} is before the production date {m}."));
        }
    }
    if let Some(e) = &exp {
        if e.as_str() < received_on {
            warnings.push(format!("The expiry date {e} is before the day it was received ({received_on}): it arrived expired."));
        }
        let far = (NaiveDate::parse_from_str(received_on, "%Y-%m-%d").map_err(|_| AppError::internal("bad date"))?
            + chrono::Duration::days(365 * inv.expiry_max_years))
        .to_string();
        if e.as_str() > far.as_str() {
            warnings.push(format!("The expiry date {e} is more than {} years away.", inv.expiry_max_years));
        }
    }
    if let Some(m) = &mfg {
        if m.as_str() > received_on {
            warnings.push(format!("The production date {m} is after the day it was received."));
        }
    }
    if !warnings.is_empty() && !input.confirm_warnings {
        return Err(AppError::validation("Check the dates, then confirm to keep them.")
            .with_details(json!({ "kind": "lot_date_warnings", "warnings": warnings })));
    }
    Ok(LotInput {
        supplier_lot_code: code,
        expires_on: exp,
        manufactured_on: mfg,
        expiry_kind: kind,
        expiry_source: Some(source.into()),
        confirm_warnings: input.confirm_warnings,
    })
}

pub(crate) struct NewLot<'a> {
    pub product_id: &'a str,
    pub branch_id: &'a str,
    pub supplier_id: Option<&'a str>,
    pub po_id: Option<&'a str>,
    pub receipt_id: Option<&'a str>,
    pub qty_milli: i64,
    pub unit_cost_minor: i64,
    pub provenance: &'a str,
    pub user_id: &'a str,
}

/// Write a lot (its facts only; the stock movement is written by the caller).
pub(crate) fn create_lot(c: &Connection, n: &NewLot, lot: &LotInput) -> AppResult<(String, String)> {
    let id = new_id();
    let number = format!("L-{:05}", next_seq(c, "lot")?);
    let location: Option<String> = c
        .query_row("SELECT location_id FROM stock_locations WHERE branch_id=?1 AND is_default=1", [n.branch_id], |r| r.get(0))
        .optional()?;
    let now = time::now_str();
    c.execute(
        "INSERT INTO stock_lots(lot_id, lot_number, product_id, branch_id, location_id, supplier_id, po_id, receipt_id, supplier_lot_code, received_at,
             manufactured_on, expires_on, expiry_kind, expiry_source, qty_received_milli, unit_cost_minor, provenance, created_by, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?10)",
        params![
            id,
            number,
            n.product_id,
            n.branch_id,
            location,
            n.supplier_id,
            n.po_id,
            n.receipt_id,
            lot.supplier_lot_code,
            now,
            lot.manufactured_on,
            lot.expires_on,
            lot.expiry_kind,
            lot.expiry_source.as_deref().unwrap_or("person"),
            n.qty_milli,
            n.unit_cost_minor,
            n.provenance,
            n.user_id
        ],
    )?;
    Ok((id, number))
}

const FACTS_SQL: &str = "SELECT l.lot_id, l.lot_number, l.product_id, l.branch_id, l.location_id, l.supplier_id, l.po_id, l.receipt_id,
        COALESCE((SELECT k.supplier_lot_code FROM lot_corrections k WHERE k.lot_id=l.lot_id AND k.supplier_lot_code IS NOT NULL ORDER BY k.created_at DESC, k.correction_id DESC LIMIT 1), l.supplier_lot_code),
        l.received_at,
        COALESCE((SELECT k.manufactured_on FROM lot_corrections k WHERE k.lot_id=l.lot_id AND k.manufactured_on IS NOT NULL ORDER BY k.created_at DESC, k.correction_id DESC LIMIT 1), l.manufactured_on),
        COALESCE((SELECT k.expires_on FROM lot_corrections k WHERE k.lot_id=l.lot_id AND k.expires_on IS NOT NULL ORDER BY k.created_at DESC, k.correction_id DESC LIMIT 1), l.expires_on),
        COALESCE((SELECT k.expiry_kind FROM lot_corrections k WHERE k.lot_id=l.lot_id AND k.expiry_kind IS NOT NULL ORDER BY k.created_at DESC, k.correction_id DESC LIMIT 1), l.expiry_kind),
        l.expiry_source, l.qty_received_milli, l.unit_cost_minor, l.provenance,
        EXISTS (SELECT 1 FROM lot_corrections k WHERE k.lot_id=l.lot_id)
     FROM stock_lots l";

fn facts_row(r: &rusqlite::Row) -> rusqlite::Result<LotFacts> {
    Ok(LotFacts {
        lot_id: r.get(0)?,
        lot_number: r.get(1)?,
        product_id: r.get(2)?,
        branch_id: r.get(3)?,
        location_id: r.get(4)?,
        supplier_id: r.get(5)?,
        po_id: r.get(6)?,
        receipt_id: r.get(7)?,
        supplier_lot_code: r.get(8)?,
        received_at: r.get(9)?,
        manufactured_on: r.get(10)?,
        expires_on: r.get(11)?,
        expiry_kind: r.get(12)?,
        expiry_source: r.get(13)?,
        qty_received_milli: r.get(14)?,
        unit_cost_minor: r.get(15)?,
        provenance: r.get(16)?,
        corrected: r.get::<_, i64>(17)? == 1,
    })
}

pub(crate) fn lot_facts(c: &Connection, lot_id: &str) -> AppResult<LotFacts> {
    c.query_row(&format!("{FACTS_SQL} WHERE l.lot_id=?1"), [lot_id], facts_row).optional()?.ok_or_else(|| AppError::not_found("Batch"))
}

/// First-expiring-first-out order: earliest expiry, then no expiry, then
/// the earliest received, then the lot id.
fn fefo_key(f: &LotFacts) -> (bool, String, String, String) {
    (f.expires_on.is_none(), f.expires_on.clone().unwrap_or_default(), f.received_at.clone(), f.lot_id.clone())
}

/// Take `q` from the lots in FEFO order (skipping `except`), as estimated
/// sales. Returns what could not be taken.
fn take_fefo(lots: &mut [LotState], order: &[usize], mut q: i64, except: Option<usize>) -> i64 {
    for &i in order {
        if q == 0 {
            break;
        }
        if Some(i) == except || lots[i].balance_milli <= 0 {
            continue;
        }
        let t = q.min(lots[i].balance_milli);
        lots[i].balance_milli -= t;
        lots[i].estimated_out_milli += t;
        q -= t;
    }
    q
}

/// Replay one product's movements in one branch (see the module notes).
pub fn replay(c: &Connection, product_id: &str, branch_id: &str) -> AppResult<ProductLots> {
    let mut st = c.prepare(&format!("{FACTS_SQL} WHERE l.product_id=?1 AND l.branch_id=?2"))?;
    let facts = st.query_map(params![product_id, branch_id], facts_row)?.collect::<Result<Vec<_>, _>>()?;
    let stock = current_qty(c, product_id, branch_id)?;
    if facts.is_empty() {
        return Ok(ProductLots {
            product_id: product_id.into(),
            branch_id: branch_id.into(),
            stock_milli: stock,
            unlotted_milli: stock,
            lots: vec![],
        });
    }
    let mut lots: Vec<LotState> = facts
        .into_iter()
        .map(|f| LotState { facts: f, in_milli: 0, explicit_out_milli: 0, estimated_out_milli: 0, balance_milli: 0 })
        .collect();
    let mut order: Vec<usize> = (0..lots.len()).collect();
    order.sort_by_key(|&i| fefo_key(&lots[i].facts));
    let index: HashMap<String, usize> = lots.iter().enumerate().map(|(i, l)| (l.facts.lot_id.clone(), i)).collect();
    // Everything before the first lot movement is stock in no lot.
    let first: Option<(String, String)> = c
        .query_row(
            "SELECT created_at, movement_id FROM stock_movements WHERE product_id=?1 AND branch_id=?2 AND lot_id IS NOT NULL
             ORDER BY created_at, movement_id LIMIT 1",
            params![product_id, branch_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (t0, m0) = first.unwrap_or_else(|| ("9999".into(), "".into()));
    let mut unlotted: i64 = c.query_row(
        "SELECT COALESCE(SUM(qty_delta_milli),0) FROM stock_movements WHERE product_id=?1 AND branch_id=?2 AND (created_at, movement_id) < (?3, ?4)",
        params![product_id, branch_id, t0, m0],
        |r| r.get(0),
    )?;
    let mut st = c.prepare(
        "SELECT qty_delta_milli, lot_id, type FROM stock_movements WHERE product_id=?1 AND branch_id=?2 AND (created_at, movement_id) >= (?3, ?4)
         ORDER BY created_at, movement_id",
    )?;
    let moves = st
        .query_map(params![product_id, branch_id, t0, m0], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (q, lot, _kind) in moves {
        match lot.as_ref().and_then(|l| index.get(l)).copied() {
            Some(i) if q > 0 => {
                // Into a lot. Stock sold before it arrived (oversold, no lot)
                // is settled from it first.
                lots[i].in_milli += q;
                let covered = if unlotted < 0 { q.min(-unlotted) } else { 0 };
                unlotted += covered;
                lots[i].estimated_out_milli += covered;
                lots[i].balance_milli += q - covered;
            }
            Some(i) => {
                // Evidence: taken from this lot. If the estimate had used it
                // up, the estimate moves to the next lots.
                let need = -q;
                if lots[i].balance_milli < need {
                    let movable = (need - lots[i].balance_milli).min(lots[i].estimated_out_milli);
                    if movable > 0 {
                        lots[i].estimated_out_milli -= movable;
                        lots[i].balance_milli += movable;
                        let left = take_fefo(&mut lots, &order, movable, Some(i));
                        unlotted -= left;
                    }
                }
                lots[i].explicit_out_milli += need;
                lots[i].balance_milli -= need;
                if lots[i].balance_milli < 0 {
                    unlotted += lots[i].balance_milli;
                    lots[i].balance_milli = 0;
                }
            }
            None if q > 0 => unlotted += q,
            None => {
                // Out with no lot: stock in no lot first, then FEFO (estimate).
                let need = -q;
                let from_unlotted = need.min(unlotted.max(0));
                unlotted -= from_unlotted;
                let left = take_fefo(&mut lots, &order, need - from_unlotted, None);
                unlotted -= left;
            }
        }
    }
    let mut out: Vec<LotState> = order.into_iter().map(|i| lots[i].clone()).collect();
    out.iter_mut().for_each(|l| l.explicit_out_milli = l.explicit_out_milli.max(0));
    Ok(ProductLots { product_id: product_id.into(), branch_id: branch_id.into(), stock_milli: stock, unlotted_milli: unlotted, lots: out })
}

/// The one current state of a lot's date: depleted, no_date, expired (use
/// by passed), past_best_before, urgent, soon, later or healthy.
pub fn expiry_status(
    expires_on: Option<&str>,
    kind: Option<&str>,
    balance: i64,
    today: &str,
    inv: &InventorySettings,
) -> (&'static str, Option<i64>) {
    if balance <= 0 {
        return ("depleted", None);
    }
    let Some(e) = expires_on else { return ("no_date", None) };
    let days = match (NaiveDate::parse_from_str(e, "%Y-%m-%d"), NaiveDate::parse_from_str(today, "%Y-%m-%d")) {
        (Ok(a), Ok(b)) => (a - b).num_days(),
        _ => return ("no_date", None),
    };
    let st = if days < 0 {
        if kind == Some("best_before") {
            "past_best_before"
        } else {
            "expired"
        }
    } else if days <= inv.expiry_urgent_days {
        "urgent"
    } else if days <= inv.expiry_soon_days {
        "soon"
    } else if days <= inv.expiry_later_days {
        "later"
    } else {
        "healthy"
    };
    (st, Some(days))
}

/// Net units sold (sales less refunds and voids, each on its own business
/// date) in a branch for business dates `from..=to` — the same definition as
/// the sales reports.
pub fn net_units_sold(c: &Connection, product_id: &str, branch_id: &str, from: &str, to: &str) -> AppResult<i64> {
    let sold: i64 = c.query_row(
        "SELECT COALESCE(SUM(i.qty_milli),0) FROM sale_items i JOIN sales x ON x.sale_id=i.sale_id
         WHERE i.product_id=?1 AND x.branch_id=?2 AND x.business_date>=?3 AND x.business_date<=?4",
        params![product_id, branch_id, from, to],
        |r| r.get(0),
    )?;
    let back: i64 = c.query_row(
        "SELECT COALESCE(SUM(ri.qty_milli),0) FROM refund_items ri JOIN refunds r ON r.refund_id=ri.refund_id
         WHERE ri.product_id=?1 AND r.branch_id=?2 AND r.business_date>=?3 AND r.business_date<=?4",
        params![product_id, branch_id, from, to],
        |r| r.get(0),
    )?;
    Ok(sold - back)
}

/// Demand over the last `window` completed days (yesterday back), with how
/// many of those days the product has existed in this branch.
#[derive(Debug, Clone, Serialize)]
pub struct Demand {
    pub window_days: i64,
    pub history_days: i64,
    pub net_sold_milli: i64,
}

pub fn demand(c: &Connection, product_id: &str, branch_id: &str, today: &str, window: i64) -> AppResult<Demand> {
    let t = NaiveDate::parse_from_str(today, "%Y-%m-%d").map_err(|_| AppError::internal("bad date"))?;
    let from = (t - chrono::Duration::days(window)).to_string();
    let to = (t - chrono::Duration::days(1)).to_string();
    let first: Option<String> = c.query_row(
        "SELECT MIN(created_at) FROM stock_movements WHERE product_id=?1 AND branch_id=?2",
        params![product_id, branch_id],
        |r| r.get(0),
    )?;
    let first_sale: Option<String> = c.query_row(
        "SELECT MIN(x.business_date) FROM sale_items i JOIN sales x ON x.sale_id=i.sale_id WHERE i.product_id=?1 AND x.branch_id=?2",
        params![product_id, branch_id],
        |r| r.get(0),
    )?;
    let day = time::day(c)?;
    let since =
        [first.and_then(|f| time::parse(&f).ok()).and_then(|t| time::business_date(t, &day).ok()), first_sale].into_iter().flatten().min();
    let history_days =
        since.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()).map(|s| (t - s).num_days().clamp(0, window)).unwrap_or(0);
    Ok(Demand { window_days: window, history_days, net_sold_milli: net_units_sold(c, product_id, branch_id, &from, &to)? })
}

pub const MIN_HISTORY_DAYS: i64 = 7;

/// Days of stock left. `state`: ok | no_stock | no_demand | not_enough_history.
#[derive(Debug, Clone, Serialize)]
pub struct Cover {
    pub state: &'static str,
    pub available_milli: i64,
    /// Units per day × 1000 (milli-units per day), over the days used.
    pub per_day_milli: i64,
    pub days_used: i64,
    /// Days of cover × 10 (one decimal), when `state` is ok.
    pub cover_tenths: Option<i64>,
    pub stockout_on: Option<String>,
}

pub fn cover(available: i64, d: &Demand, today: &str) -> Cover {
    let days = d.history_days.min(d.window_days);
    let per_day = if days > 0 { (d.net_sold_milli.max(0) as i128 / days as i128) as i64 } else { 0 };
    let base =
        Cover { state: "ok", available_milli: available, per_day_milli: per_day, days_used: days, cover_tenths: None, stockout_on: None };
    if available <= 0 {
        return Cover { state: "no_stock", ..base };
    }
    if days < MIN_HISTORY_DAYS {
        return Cover { state: "not_enough_history", ..base };
    }
    if d.net_sold_milli <= 0 {
        return Cover { state: "no_demand", ..base };
    }
    let tenths = (available as i128 * days as i128 * 10 / d.net_sold_milli as i128) as i64;
    let whole = (available as i128 * days as i128 / d.net_sold_milli as i128) as i64;
    let stockout = NaiveDate::parse_from_str(today, "%Y-%m-%d").ok().map(|t| (t + chrono::Duration::days(whole)).to_string());
    Cover { cover_tenths: Some(tenths), stockout_on: stockout, ..base }
}

/// A date suggested by a supplier document line ("EXP 12/2026",
/// "Exp: 31/12/2026", "BB 2026-12-31"). Only a suggestion: a person confirms.
pub fn suggest_expiry(text: &str) -> Option<String> {
    let low = text.to_lowercase();
    let at = ["exp", "best before", "bb ", "use by", "انتهاء"].iter().filter_map(|k| low.find(k)).min()?;
    let rest: String = low[at..].chars().skip_while(|c| !c.is_ascii_digit()).take(12).collect();
    let rest = rest.trim();
    let parts: Vec<&str> = rest.split(['/', '-', '.']).filter(|p| !p.is_empty()).collect();
    let n: Vec<u32> =
        parts.iter().take(3).filter_map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()).collect();
    let date = match n.as_slice() {
        [y, m, d] if *y > 1999 => NaiveDate::from_ymd_opt(*y as i32, *m, *d),
        [d, m, y] if *y > 1999 => NaiveDate::from_ymd_opt(*y as i32, *m, *d),
        [m, y] if *y > 1999 => {
            NaiveDate::from_ymd_opt(*y as i32, *m, 1).and_then(|f| f.checked_add_months(chrono::Months::new(1))).and_then(|f| f.pred_opt())
        }
        _ => None,
    }?;
    let s = date.to_string();
    (s.as_str() >= DATE_MIN && s.as_str() <= DATE_MAX).then_some(s)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CountInRequest {
    pub product_id: String,
    pub qty_milli: i64,
    pub lot: LotInput,
    pub operation_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LotCorrection {
    pub lot_id: String,
    #[serde(default)]
    pub supplier_lot_code: Option<String>,
    #[serde(default)]
    pub expires_on: Option<String>,
    #[serde(default)]
    pub manufactured_on: Option<String>,
    #[serde(default)]
    pub expiry_kind: Option<String>,
    pub reason: String,
    pub operation_id: String,
    #[serde(default)]
    pub confirm_warnings: bool,
}

impl AppCore {
    /// Lots are kept on the hub, which receives goods.
    pub(crate) fn lots_writable(&self) -> AppResult<()> {
        if self.device().map(|d| d.mode == "terminal").unwrap_or(false) {
            return Err(AppError::conflict("Batches and waste are recorded on the hub computer."));
        }
        Ok(())
    }

    /// Per product: ask for batch and expiry at receiving, and what its date means.
    pub fn product_lot_settings(&self, token: &str, product_id: &str, track_lots: bool, expiry_kind: Option<String>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let pid = validate::id(product_id, "Product")?;
        let kind = match expiry_kind.as_deref().filter(|k| !k.is_empty()) {
            None => None,
            Some(k @ ("use_by" | "best_before")) => Some(k.to_string()),
            Some(_) => return Err(AppError::validation("The date kind must be expiry (use by) or best before.")),
        };
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let before: Option<(i64, Option<String>)> = tx
                .query_row("SELECT track_lots, expiry_kind FROM products WHERE product_id=?1", [&pid], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            let before = before.ok_or_else(|| AppError::not_found("Product"))?;
            tx.execute(
                "UPDATE products SET track_lots=?2, expiry_kind=?3, updated_at=?4, version=version+1 WHERE product_id=?1",
                params![pid, track_lots as i64, kind, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "product.lot_settings",
                "product",
                Some(&pid),
                Some(&json!({ "track_lots": before.0 == 1, "expiry_kind": before.1 })),
                Some(&json!({ "track_lots": track_lots, "expiry_kind": kind })),
            )?;
            Ok(json!({ "product_id": pid, "track_lots": track_lots, "expiry_kind": kind }))
        })
    }

    /// The batches of a product in this branch, worked out from its movements.
    pub fn product_lots(&self, token: &str, product_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("inventory.view")?;
        let pid = validate::id(product_id, "Product")?;
        let show_cost = s.has("products.view_cost");
        self.db.read(|c| {
            let inv: InventorySettings = settings::get(c, settings::KEY_INVENTORY)?;
            let today = time::business_date(time::now(), &time::day(c)?)?;
            let r = replay(c, &pid, &s.branch_id)?;
            let lots: Vec<Value> = r
                .lots
                .iter()
                .map(|l| {
                    let (status, days) =
                        expiry_status(l.facts.expires_on.as_deref(), l.facts.expiry_kind.as_deref(), l.balance_milli, &today, &inv);
                    let mut v = serde_json::to_value(l).unwrap_or(Value::Null);
                    v["status"] = json!(status);
                    v["days_left"] = json!(days);
                    if !show_cost {
                        v["unit_cost_minor"] = Value::Null;
                    }
                    v
                })
                .collect();
            let (track, kind): (i64, Option<String>) =
                c.query_row("SELECT track_lots, expiry_kind FROM products WHERE product_id=?1", [&pid], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(json!({ "product_id": pid, "stock_milli": r.stock_milli, "unlotted_milli": r.unlotted_milli, "lots": lots,
                       "track_lots": track == 1, "expiry_kind": kind }))
        })
    }

    /// One batch: its facts, corrections, recorded movements and estimate.
    pub fn lot_get(&self, token: &str, lot_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("inventory.view")?;
        let id = validate::id(lot_id, "Batch")?;
        let show_cost = s.has("products.view_cost");
        self.db.read(|c| {
            let f = lot_facts(c, &id)?;
            let r = replay(c, &f.product_id, &f.branch_id)?;
            let state = r.lots.iter().find(|l| l.facts.lot_id == id).cloned();
            let mut st = c.prepare(
                "SELECT m.created_at, m.type, m.qty_delta_milli, m.source_type, m.source_id, m.reason, u.display_name FROM stock_movements m
                 LEFT JOIN users u ON u.user_id=m.user_id WHERE m.lot_id=?1 ORDER BY m.created_at, m.movement_id",
            )?;
            let moves: Vec<Value> = st
                .query_map([&id], |r| {
                    Ok(json!({ "at": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?, "qty_milli": r.get::<_, i64>(2)?,
                               "source_type": r.get::<_, String>(3)?, "source_id": r.get::<_, Option<String>>(4)?, "reason": r.get::<_, Option<String>>(5)?,
                               "user_name": r.get::<_, Option<String>>(6)? }))
                })?
                .collect::<Result<_, _>>()?;
            let mut st = c.prepare(
                "SELECT k.created_at, k.supplier_lot_code, k.manufactured_on, k.expires_on, k.expiry_kind, k.reason, u.display_name FROM lot_corrections k
                 LEFT JOIN users u ON u.user_id=k.user_id WHERE k.lot_id=?1 ORDER BY k.created_at, k.correction_id",
            )?;
            let corrections: Vec<Value> = st
                .query_map([&id], |r| {
                    Ok(json!({ "at": r.get::<_, String>(0)?, "supplier_lot_code": r.get::<_, Option<String>>(1)?, "manufactured_on": r.get::<_, Option<String>>(2)?,
                               "expires_on": r.get::<_, Option<String>>(3)?, "expiry_kind": r.get::<_, Option<String>>(4)?, "reason": r.get::<_, String>(5)?,
                               "user_name": r.get::<_, Option<String>>(6)? }))
                })?
                .collect::<Result<_, _>>()?;
            let (name, supplier): (String, Option<String>) = c.query_row(
                "SELECT p.name, (SELECT name FROM suppliers WHERE supplier_id=?2) FROM products p WHERE p.product_id=?1",
                params![f.product_id, f.supplier_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let mut v = serde_json::to_value(state.unwrap_or(LotState { facts: f, in_milli: 0, explicit_out_milli: 0, estimated_out_milli: 0, balance_milli: 0 }))?;
            if !show_cost {
                v["unit_cost_minor"] = Value::Null;
            }
            v["product_name"] = json!(name);
            v["supplier_name"] = json!(supplier);
            v["movements"] = json!(moves);
            v["corrections"] = json!(corrections);
            Ok(v)
        })
    }

    /// Count existing stock that is in no batch into a batch (for example
    /// stock from before batches were kept, once its expiry is read off the
    /// pack). Stock on hand does not change.
    pub fn lot_count_in(&self, token: &str, req: CountInRequest) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("lots.manage")?;
        self.lots_writable()?;
        let pid = validate::id(&req.product_id, "Product")?;
        if req.lot.is_empty() {
            return Err(AppError::validation("Enter the batch code or a date."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "lot.count_in", &req)? {
                Check::Replay { result } => return Ok(result),
                Check::New { payload_hash } => payload_hash,
            };
            let (track, dec, kind): (i64, i64, Option<String>) = tx
                .query_row("SELECT track_inventory, allow_decimal_quantity, expiry_kind FROM products WHERE product_id=?1", [&pid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Product"))?;
            if track == 0 {
                return Err(AppError::validation("This product does not track stock."));
            }
            validate::qty_positive(req.qty_milli, dec == 1, "Quantity")?;
            let today = time::business_date(time::now(), &time::day(tx)?)?;
            let lot = check_lot(tx, &req.lot, &today, kind)?;
            let r = replay(tx, &pid, &s.branch_id)?;
            if req.qty_milli > r.unlotted_milli {
                return Err(AppError::validation(format!(
                    "Only {} is in no batch. Count at most that into a batch.",
                    crate::money::format_qty(r.unlotted_milli.max(0))
                )));
            }
            let cost = crate::inventory::avg_cost(tx, &pid, &s.branch_id)?;
            let (lot_id, number) = create_lot(
                tx,
                &NewLot {
                    product_id: &pid,
                    branch_id: &s.branch_id,
                    supplier_id: None,
                    po_id: None,
                    receipt_id: None,
                    qty_milli: req.qty_milli,
                    unit_cost_minor: cost,
                    provenance: "count",
                    user_id: &s.user_id,
                },
                &lot,
            )?;
            let reason = format!("Counted into batch {number}");
            for (q, l) in [(-req.qty_milli, None), (req.qty_milli, Some(lot_id.as_str()))] {
                apply_movement_lot(
                    tx,
                    &Movement {
                        product_id: &pid,
                        branch_id: &s.branch_id,
                        kind: "adjust",
                        qty_delta_milli: q,
                        unit_cost_minor: Some(cost),
                        source_type: "lot_count",
                        source_id: Some(&lot_id),
                        reason: Some(&reason),
                        user_id: Some(&s.user_id),
                        device_id: Some(&s.device_id),
                    },
                    None,
                    l,
                )?;
            }
            let result = json!({ "lot_id": lot_id, "lot_number": number });
            audit::record(
                tx,
                &actor,
                "lot.counted_in",
                "lot",
                Some(&lot_id),
                None,
                Some(&json!({ "product_id": pid, "qty_milli": req.qty_milli, "lot": lot })),
            )?;
            idempotency::complete(
                tx,
                &req.operation_id,
                "lot.count_in",
                Some(&s.user_id),
                Some(&s.device_id),
                &hash,
                Some(&lot_id),
                &result,
            )?;
            Ok(result)
        })
    }

    /// Correct a batch's code or dates. The original stays; the correction
    /// (who, when, why) applies from now on.
    pub fn lot_correct(&self, token: &str, req: LotCorrection) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("lots.manage")?;
        self.lots_writable()?;
        let id = validate::id(&req.lot_id, "Batch")?;
        let reason = crate::setup::clean(&req.reason, "Reason", 200, true)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "lot.correct", &req)? {
                Check::Replay { .. } => return Ok(()),
                Check::New { payload_hash } => payload_hash,
            };
            let f = lot_facts(tx, &id)?;
            let input = LotInput {
                supplier_lot_code: req.supplier_lot_code.clone(),
                expires_on: req.expires_on.clone().or(f.expires_on.clone()),
                manufactured_on: req.manufactured_on.clone().or(f.manufactured_on.clone()),
                expiry_kind: req.expiry_kind.clone().or(f.expiry_kind.clone()),
                expiry_source: None,
                confirm_warnings: req.confirm_warnings,
            };
            let received = f.received_at.get(..10).unwrap_or(&f.received_at).to_string();
            let checked = check_lot(tx, &input, &received, None)?;
            let changed = |new: &Option<String>, old: &Option<String>| new.clone().filter(|n| Some(n) != old.as_ref());
            let code = changed(&checked.supplier_lot_code, &f.supplier_lot_code).filter(|_| req.supplier_lot_code.is_some());
            let exp = changed(&checked.expires_on, &f.expires_on).filter(|_| req.expires_on.is_some());
            let mfg = changed(&checked.manufactured_on, &f.manufactured_on).filter(|_| req.manufactured_on.is_some());
            let kind = changed(&checked.expiry_kind, &f.expiry_kind).filter(|_| req.expiry_kind.is_some());
            if code.is_none() && exp.is_none() && mfg.is_none() && kind.is_none() {
                return Err(AppError::validation("Nothing to correct."));
            }
            tx.execute(
                "INSERT INTO lot_corrections(correction_id, lot_id, supplier_lot_code, manufactured_on, expires_on, expiry_kind, reason, user_id, operation_id, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![new_id(), id, code, mfg, exp, kind, reason, s.user_id, req.operation_id, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "lot.corrected",
                "lot",
                Some(&id),
                Some(&json!({ "supplier_lot_code": f.supplier_lot_code, "expires_on": f.expires_on, "manufactured_on": f.manufactured_on, "expiry_kind": f.expiry_kind })),
                Some(&json!({ "supplier_lot_code": code, "expires_on": exp, "manufactured_on": mfg, "expiry_kind": kind, "reason": reason })),
            )?;
            idempotency::complete(tx, &req.operation_id, "lot.correct", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &json!({ "lot_id": id }))?;
            Ok(())
        })?;
        self.lot_get(token, &id)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DraftLotInput {
    pub draft_id: String,
    pub line_no: i64,
    #[serde(default)]
    pub lot_code: Option<String>,
    #[serde(default)]
    pub expires_on: Option<String>,
    /// Keep the date read from the document as it is (a person checked it).
    #[serde(default)]
    pub confirm_document_date: bool,
    #[serde(default)]
    pub confirm_warnings: bool,
}

impl AppCore {
    /// Set, confirm or clear the batch and expiry on a receiving-draft line.
    /// A date the person types, or a document date they confirm, becomes
    /// usable; a document date nobody confirmed blocks receiving.
    pub fn receiving_draft_set_lot(&self, token: &str, req: DraftLotInput) -> AppResult<Value> {
        let s = self.session(token)?;
        self.require_back_office_writable()?;
        if !s.has("purchasing.manage") {
            s.require("inventory.receive")?;
        }
        let id = validate::id(&req.draft_id, "Receiving draft")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let (status, pid, old_exp, source): (String, String, Option<String>, Option<String>) = tx
                .query_row(
                    "SELECT d.status, l.product_id, l.expires_on, l.expiry_source FROM receiving_drafts d JOIN receiving_draft_lines l ON l.draft_id=d.draft_id
                     WHERE d.draft_id=?1 AND l.line_no=?2",
                    params![id, req.line_no],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Draft line"))?;
            if status != "draft" {
                return Err(AppError::conflict("Only a draft can be edited."));
            }
            let kind: Option<String> = tx.query_row("SELECT expiry_kind FROM products WHERE product_id=?1", [&pid], |r| r.get(0))?;
            let today = time::business_date(time::now(), &time::day(tx)?)?;
            let new_exp = req.expires_on.as_deref().map(str::trim).filter(|x| !x.is_empty()).map(String::from);
            let checked = check_lot(
                tx,
                &LotInput { supplier_lot_code: req.lot_code.clone(), expires_on: new_exp.clone(), confirm_warnings: req.confirm_warnings, ..Default::default() },
                &today,
                kind,
            )?;
            // Same document date + confirm → confirmed document date; any other date → the person's.
            let (src, confirmed) = match (&new_exp, &old_exp, source.as_deref()) {
                (None, _, _) => (None, 0),
                (Some(n), Some(o), Some("document")) if n == o => (Some("document"), req.confirm_document_date as i64),
                _ => (Some("person"), 1),
            };
            tx.execute(
                "UPDATE receiving_draft_lines SET lot_code=?3, expires_on=?4, expiry_source=?5, expiry_confirmed=?6 WHERE draft_id=?1 AND line_no=?2",
                params![id, req.line_no, checked.supplier_lot_code, checked.expires_on, src, confirmed],
            )?;
            tx.execute("UPDATE receiving_drafts SET revision=revision+1, updated_at=?2 WHERE draft_id=?1", params![id, time::now_str()])?;
            audit::record(
                tx,
                &actor,
                "receiving.draft_lot",
                "receiving_draft",
                Some(&id),
                Some(&json!({ "line_no": req.line_no, "expires_on": old_exp, "expiry_source": source })),
                Some(&json!({ "line_no": req.line_no, "lot_code": checked.supplier_lot_code, "expires_on": checked.expires_on, "expiry_source": src, "confirmed": confirmed == 1 })),
            )?;
            Ok(())
        })?;
        self.receiving_draft_get(token, &id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_suggestions_are_read_but_not_guessed() {
        assert_eq!(suggest_expiry("LABAN 1L x12 EXP 31/12/2026").as_deref(), Some("2026-12-31"));
        assert_eq!(suggest_expiry("Dates 1kg Exp: 2027-03-15").as_deref(), Some("2027-03-15"));
        assert_eq!(suggest_expiry("Rice 5kg BB 06/2027").as_deref(), Some("2027-06-30"));
        assert_eq!(suggest_expiry("Milk 2L 4.250"), None, "no expiry word, no date");
        assert_eq!(suggest_expiry("EXP 45/13/2026"), None, "impossible date");
    }

    #[test]
    fn cover_says_why_instead_of_infinity() {
        let d = |sold, hist| Demand { window_days: 30, history_days: hist, net_sold_milli: sold };
        assert_eq!(cover(0, &d(30_000, 30), "2026-10-05").state, "no_stock");
        assert_eq!(cover(10_000, &d(0, 30), "2026-10-05").state, "no_demand");
        assert_eq!(cover(10_000, &d(5_000, 3), "2026-10-05").state, "not_enough_history");
        let c = cover(10_000, &d(30_000, 30), "2026-10-05");
        assert_eq!((c.state, c.per_day_milli, c.cover_tenths, c.stockout_on.as_deref()), ("ok", 1_000, Some(100), Some("2026-10-15")));
        // A product only 10 days old is judged on its 10 days, not 30.
        let c = cover(10_000, &d(20_000, 10), "2026-10-05");
        assert_eq!((c.per_day_milli, c.cover_tenths), (2_000, Some(50)));
    }
}
