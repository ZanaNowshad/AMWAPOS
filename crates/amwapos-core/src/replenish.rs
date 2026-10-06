//! Replenishment: what to order, from whom, and why (docs/PROCUREMENT.md).
//!
//! One deterministic engine. It reads the facts for every product in a
//! fixed number of grouped queries (no query per product), then `assess`
//! decides each product with plain integer arithmetic. Nothing here orders:
//! a suggestion becomes a requisition only through `requisitions.rs`, which
//! recomputes it.
//!
//! Stock position (never counted twice):
//!   usable   = on hand − active order holds − expired use-by stock
//!   inbound  = open purchase orders (ordered, not received, not cancelled;
//!              drafts included) + transfers on the way in + requisition
//!              lines not yet turned into orders
//!   position = usable + inbound
//! Best-before stock past its date is still usable; use-by stock is not.
//!
//! Demand is the Wave 3 definition (`lots::demand_all`): net units sold per
//! day over the demand window, from at least 7 days of history.
//!   reorder point = the product's reorder point when set, else
//!                   daily demand × (lead time + safety days)
//!   order up to   = the product's maximum stock when set, else
//!                   daily demand × (lead time + safety days + days between orders)
//! An order is suggested when position ≤ reorder point, for
//! (order up to − position), rounded up to whole packs and the minimum order.

use std::collections::HashMap;

use chrono::NaiveDate;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalogue::{self, Terms};
use crate::error::{AppError, AppResult};
use crate::lots::{self, Demand, MIN_HISTORY_DAYS};
use crate::service::AppCore;
use crate::settings::{self, InventorySettings, PurchasingSettings};
use crate::time;
use crate::validate;

/// The facts about one product that the decision uses.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Facts {
    pub product_id: String,
    pub active: bool,
    pub allow_decimal: bool,
    pub reorder_point_milli: i64,
    pub max_stock_milli: Option<i64>,
    pub on_hand_milli: i64,
    pub held_milli: i64,
    pub expired_use_by_milli: i64,
    pub open_po_milli: i64,
    pub draft_po_milli: i64,
    pub transfers_in_milli: i64,
    pub requisitioned_milli: i64,
    pub net_sold_milli: i64,
    pub history_days: i64,
    pub window_days: i64,
    /// Stock in batches whose date falls before the next delivery could be
    /// used up (lead time + safety days).
    pub expiring_soon_milli: i64,
    pub waste_milli: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SupplierChoice {
    pub supplier_id: String,
    pub supplier_name: String,
    pub units_per_case: Option<i64>,
    pub pack_source: Option<String>,
    pub moq_packs: Option<i64>,
    pub lead_time_days: Option<i64>,
    pub preferred: bool,
    pub last_cost_minor: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Assessment {
    /// order | covered | insufficient_history | no_demand | no_supplier |
    /// no_lead_time | inactive | invalid_pack
    pub state: &'static str,
    pub reasons: Vec<&'static str>,
    pub warnings: Vec<&'static str>,
    pub usable_milli: i64,
    pub inbound_milli: i64,
    pub position_milli: i64,
    /// Milli-units per day over the days used (0 when unknown).
    pub per_day_milli: i64,
    pub days_used: i64,
    pub reorder_point_milli: Option<i64>,
    /// product | demand
    pub reorder_point_source: Option<&'static str>,
    pub order_up_to_milli: Option<i64>,
    /// max_stock | demand | reorder_point
    pub order_up_to_source: Option<&'static str>,
    pub need_milli: i64,
    pub supplier: Option<SupplierChoice>,
    /// preferred_supplier | only_supplier | lowest_last_cost | shortest_lead_time | first_by_name
    pub supplier_reason: Option<&'static str>,
    pub alternatives: Vec<SupplierChoice>,
    pub packs: Option<i64>,
    pub suggested_milli: i64,
    pub estimated_cost_minor: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub safety_days: i64,
    pub cycle_days: i64,
}

fn ceil_div(a: i128, b: i128) -> i64 {
    if b <= 0 {
        return 0;
    }
    ((a + b - 1).div_euclid(b)) as i64
}

/// The deterministic supplier order: preferred first, then a known lead
/// time, then the lowest last confirmed cost, then the shortest lead time,
/// then the name and id. Inactive terms and inactive suppliers are left out.
pub fn rank_suppliers(terms: &[Terms], costs: &HashMap<(String, String), i64>) -> Vec<SupplierChoice> {
    let mut v: Vec<SupplierChoice> = terms
        .iter()
        .filter(|t| t.active && t.supplier_active)
        .map(|t| SupplierChoice {
            supplier_id: t.supplier_id.clone(),
            supplier_name: t.supplier_name.clone(),
            units_per_case: t.units_per_case,
            pack_source: t.pack_source.clone(),
            moq_packs: t.moq_packs,
            lead_time_days: t.lead_time_days,
            preferred: t.preferred,
            last_cost_minor: costs.get(&(t.supplier_id.clone(), t.product_id.clone())).copied(),
        })
        .collect();
    v.sort_by(|a, b| {
        (
            !a.preferred,
            a.lead_time_days.is_none(),
            a.last_cost_minor.is_none(),
            a.last_cost_minor,
            a.lead_time_days,
            a.supplier_name.to_lowercase(),
            &a.supplier_id,
        )
            .cmp(&(
                !b.preferred,
                b.lead_time_days.is_none(),
                b.last_cost_minor.is_none(),
                b.last_cost_minor,
                b.lead_time_days,
                b.supplier_name.to_lowercase(),
                &b.supplier_id,
            ))
    });
    v
}

fn supplier_reason(ranked: &[SupplierChoice]) -> Option<&'static str> {
    let first = ranked.first()?;
    Some(if first.preferred {
        "preferred_supplier"
    } else if ranked.len() == 1 {
        "only_supplier"
    } else {
        let second = &ranked[1];
        if first.lead_time_days.is_some() && second.lead_time_days.is_none() {
            "has_lead_time"
        } else if first.last_cost_minor.is_some() && (second.last_cost_minor.is_none() || first.last_cost_minor < second.last_cost_minor) {
            "lowest_last_cost"
        } else if first.lead_time_days < second.lead_time_days {
            "shortest_lead_time"
        } else {
            "first_by_name"
        }
    })
}

/// Decide one product. Pure: the same facts always give the same answer.
pub fn assess(f: &Facts, ranked: Vec<SupplierChoice>, cfg: Config) -> Assessment {
    let usable = f.on_hand_milli - f.held_milli - f.expired_use_by_milli;
    let inbound = f.open_po_milli + f.draft_po_milli + f.transfers_in_milli + f.requisitioned_milli;
    let position = usable + inbound;
    let days = f.history_days.min(f.window_days);
    let mut reasons: Vec<&'static str> = vec![];
    let mut warnings: Vec<&'static str> = vec![];
    if f.expired_use_by_milli > 0 {
        reasons.push("expired_stock_not_counted");
    }
    if f.held_milli > 0 {
        reasons.push("holds_not_counted");
    }
    if f.open_po_milli + f.draft_po_milli > 0 {
        reasons.push("on_order");
    }
    if f.transfers_in_milli > 0 {
        reasons.push("transfer_on_the_way");
    }
    if f.requisitioned_milli > 0 {
        reasons.push("already_requested");
    }
    if f.expiring_soon_milli > 0 {
        warnings.push("expiry_risk");
    }
    // Waste is a warning only: it never changes the quantity.
    if f.waste_milli > 0 && f.net_sold_milli > 0 && f.waste_milli * 10 > f.net_sold_milli {
        warnings.push("high_waste");
    }
    let per_day = if days > 0 { (f.net_sold_milli.max(0) as i128 / days as i128) as i64 } else { 0 };
    let supplier = ranked.first().cloned();
    let reason = supplier_reason(&ranked);
    let alternatives: Vec<SupplierChoice> = ranked.into_iter().skip(1).collect();
    let mut a = Assessment {
        state: "covered",
        reasons,
        warnings,
        usable_milli: usable,
        inbound_milli: inbound,
        position_milli: position,
        per_day_milli: per_day,
        days_used: days,
        reorder_point_milli: None,
        reorder_point_source: None,
        order_up_to_milli: None,
        order_up_to_source: None,
        need_milli: 0,
        supplier,
        supplier_reason: reason,
        alternatives,
        packs: None,
        suggested_milli: 0,
        estimated_cost_minor: None,
    };
    if !f.active {
        a.state = "inactive";
        return a;
    }
    let manual_min = (f.reorder_point_milli > 0).then_some(f.reorder_point_milli);
    let has_demand = days >= MIN_HISTORY_DAYS && f.net_sold_milli > 0;
    if !has_demand && manual_min.is_none() && f.max_stock_milli.is_none() {
        a.state = if days < MIN_HISTORY_DAYS { "insufficient_history" } else { "no_demand" };
        return a;
    }
    if !has_demand {
        a.reasons.push(if days < MIN_HISTORY_DAYS {
            "insufficient_history_using_product_levels"
        } else {
            "no_demand_using_product_levels"
        });
    }
    let lead = a.supplier.as_ref().and_then(|s| s.lead_time_days);
    // Demand-based levels need the supplier's lead time.
    let demand_levels = if has_demand {
        match lead {
            Some(l) => {
                let rp = ceil_div(f.net_sold_milli as i128 * (l + cfg.safety_days) as i128, days as i128);
                let up = ceil_div(f.net_sold_milli as i128 * (l + cfg.safety_days + cfg.cycle_days) as i128, days as i128);
                Some((rp, up))
            }
            None if manual_min.is_none() || f.max_stock_milli.is_none() => {
                a.state = if a.supplier.is_none() { "no_supplier" } else { "no_lead_time" };
                return a;
            }
            None => None,
        }
    } else {
        None
    };
    let (rp, rp_src) = match (manual_min, demand_levels) {
        (Some(m), _) => (m, "product"),
        (None, Some((rp, _))) => (rp, "demand"),
        (None, None) => (0, "product"),
    };
    let (up, up_src) = match (f.max_stock_milli, demand_levels) {
        (Some(m), _) => (m, "max_stock"),
        (None, Some((_, up))) if up >= rp => (up, "demand"),
        _ => (rp, "reorder_point"),
    };
    a.reorder_point_milli = Some(rp);
    a.reorder_point_source = Some(rp_src);
    a.order_up_to_milli = Some(up);
    a.order_up_to_source = Some(up_src);
    // At or below the reorder point triggers an order; when there is nothing
    // to fill above the reorder point, only below it.
    if position > rp || (position == rp && up <= rp) {
        a.state = "covered";
        return a;
    }
    a.reasons.push("at_or_below_reorder_point");
    a.need_milli = (up - position).max(0);
    if a.need_milli == 0 {
        a.state = "covered";
        return a;
    }
    let Some(sup) = a.supplier.clone() else {
        a.state = "no_supplier";
        return a;
    };
    match catalogue::order_quantity(a.need_milli, sup.units_per_case, sup.moq_packs, f.allow_decimal) {
        Ok((packs, qty)) => {
            if sup.units_per_case.is_some() {
                a.reasons.push("rounded_to_packs");
            }
            if sup.moq_packs.is_some_and(|m| packs.unwrap_or(qty / crate::money::QTY_SCALE) <= m) {
                a.reasons.push("minimum_order");
            }
            if lead.is_none() {
                a.warnings.push("no_lead_time");
            }
            a.packs = packs;
            a.suggested_milli = qty;
            a.estimated_cost_minor = sup.last_cost_minor.and_then(|c| crate::money::extend(c, qty).ok());
            a.state = "order";
        }
        Err(_) => a.state = "invalid_pack",
    }
    a
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReplenishFilter {
    #[serde(default)]
    pub branch_id: Option<String>,
    /// Only these states (default: order, no_supplier, no_lead_time, invalid_pack).
    #[serde(default)]
    pub states: Option<Vec<String>>,
    #[serde(default)]
    pub supplier_id: Option<String>,
    #[serde(default)]
    pub product_ids: Option<Vec<String>>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// One row of the engine's output, with the product's names.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub product_id: String,
    pub name: String,
    pub name_ar: Option<String>,
    pub sku: String,
    pub category: Option<String>,
    pub facts: Facts,
    #[serde(flatten)]
    pub assessment: Assessment,
}

fn sum_by_product(c: &Connection, sql: &str, p: &[&dyn rusqlite::ToSql]) -> AppResult<HashMap<String, i64>> {
    let mut st = c.prepare(sql)?;
    let rows = st.query_map(p, |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = HashMap::new();
    for r in rows {
        let (k, v) = r?;
        out.insert(k, v);
    }
    Ok(out)
}

/// Run the engine for a branch. A fixed number of grouped queries, then one
/// pure decision per product.
pub fn run(c: &Connection, branch: &str, today: &str, only: Option<&[String]>) -> AppResult<Vec<Row>> {
    let ps: PurchasingSettings = settings::get(c, settings::KEY_PURCHASING)?;
    let inv: InventorySettings = settings::get(c, settings::KEY_INVENTORY)?;
    let cfg = Config { safety_days: ps.safety_days, cycle_days: ps.order_cycle_days };
    let window = ps.demand_window_days;
    let t = NaiveDate::parse_from_str(today, "%Y-%m-%d").map_err(|_| AppError::internal("bad date"))?;
    let from = (t - chrono::Duration::days(window)).to_string();
    let to = (t - chrono::Duration::days(1)).to_string();

    let on_hand = sum_by_product(c, "SELECT product_id, qty_milli FROM stock_levels WHERE branch_id=?1", &[&branch])?;
    let held = sum_by_product(
        c,
        "SELECT product_id, SUM(qty_milli) FROM stock_reservations WHERE branch_id=?1 AND status='active' GROUP BY product_id",
        &[&branch],
    )?;
    let open_po = sum_by_product(
        c,
        "SELECT i.product_id, SUM(MAX(i.qty_ordered_milli - i.qty_received_milli - i.qty_cancelled_milli, 0)) FROM purchase_order_items i
         JOIN purchase_orders o ON o.po_id=i.po_id WHERE o.branch_id=?1 AND o.status IN ('ordered','partially_received') GROUP BY i.product_id",
        &[&branch],
    )?;
    let draft_po = sum_by_product(
        c,
        "SELECT i.product_id, SUM(i.qty_ordered_milli) FROM purchase_order_items i
         JOIN purchase_orders o ON o.po_id=i.po_id WHERE o.branch_id=?1 AND o.status='draft' GROUP BY i.product_id",
        &[&branch],
    )?;
    let transfers = sum_by_product(
        c,
        "SELECT l.product_id, SUM(MAX(l.qty_milli - l.qty_received_milli, 0)) FROM stock_transfer_lines l
         JOIN stock_transfers t ON t.transfer_id=l.transfer_id WHERE t.to_branch_id=?1 AND t.status='shipped' GROUP BY l.product_id",
        &[&branch],
    )?;
    // Requisition lines count until they become order lines (then the order counts).
    let requested = sum_by_product(
        c,
        "SELECT l.product_id, SUM(l.qty_milli) FROM requisition_lines l JOIN requisitions r ON r.requisition_id=l.requisition_id
         WHERE r.branch_id=?1 AND r.status IN ('draft','submitted','approved') AND l.po_item_id IS NULL GROUP BY l.product_id",
        &[&branch],
    )?;
    let waste = sum_by_product(
        c,
        "SELECT product_id, SUM(qty_milli) FROM waste_records WHERE branch_id=?1 AND status='recorded' AND business_date>=?2 AND business_date<=?3
         GROUP BY product_id",
        &[&branch, &from, &to],
    )?;
    let demand: HashMap<String, Demand> = lots::demand_all(c, branch, today, window)?;
    let terms = catalogue::all_terms(c)?;
    let costs = catalogue::last_confirmed_costs(c)?;

    // Batch facts only for products that have dated batches here: replay
    // those (and only those) to find expired use-by stock and stock expiring
    // before the next delivery could be used.
    let mut expired: HashMap<String, i64> = HashMap::new();
    let mut expiring: HashMap<String, i64> = HashMap::new();
    let lead_max: i64 = 365;
    let horizon = (t + chrono::Duration::days(lead_max.min(ps.safety_days + 60))).to_string();
    let dated: Vec<String> = {
        let mut st =
            c.prepare("SELECT DISTINCT product_id FROM stock_lots WHERE branch_id=?1 AND expires_on IS NOT NULL AND expires_on <= ?2")?;
        let r = st.query_map(params![branch, horizon], |r| r.get(0))?.collect::<Result<_, _>>()?;
        r
    };
    for pid in dated {
        if only.is_some_and(|o| !o.contains(&pid)) {
            continue;
        }
        let pk: Option<String> = c.query_row("SELECT expiry_kind FROM products WHERE product_id=?1", [&pid], |r| r.get(0))?;
        let lead = terms.get(&pid).and_then(|v| v.iter().filter_map(|t| t.lead_time_days).min()).unwrap_or(0);
        let soon = (t + chrono::Duration::days(lead + ps.safety_days)).to_string();
        let pl = lots::replay(c, &pid, branch)?;
        for l in pl.lots.iter().filter(|l| l.balance_milli > 0) {
            let kind = l.facts.expiry_kind.clone().or(pk.clone());
            let (st, _) = lots::expiry_status(l.facts.expires_on.as_deref(), kind.as_deref(), l.balance_milli, today, &inv);
            if st == "expired" {
                *expired.entry(pid.clone()).or_default() += l.balance_milli;
            } else if l.facts.expires_on.as_deref().is_some_and(|e| e <= soon.as_str()) {
                *expiring.entry(pid.clone()).or_default() += l.balance_milli;
            }
        }
    }

    let mut st = c.prepare(
        "SELECT p.product_id, p.name, p.name_ar, p.sku, (SELECT name FROM categories WHERE category_id=p.category_id),
                p.active, p.allow_decimal_quantity, p.reorder_point_milli, p.max_stock_milli
         FROM products p WHERE p.track_inventory=1 AND p.archived_at IS NULL ORDER BY p.name COLLATE NOCASE, p.product_id",
    )?;
    type P = (String, String, Option<String>, String, Option<String>, i64, i64, i64, Option<i64>);
    let products: Vec<P> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)))?
        .collect::<Result<_, _>>()?;
    let empty: Vec<Terms> = vec![];
    let mut out = Vec::with_capacity(products.len());
    for (pid, name, name_ar, sku, cat, active, dec, rp, max) in products {
        if only.is_some_and(|o| !o.contains(&pid)) {
            continue;
        }
        let d = demand.get(&pid);
        let g = |m: &HashMap<String, i64>| m.get(&pid).copied().unwrap_or(0);
        let facts = Facts {
            product_id: pid.clone(),
            active: active == 1,
            allow_decimal: dec == 1,
            reorder_point_milli: rp,
            max_stock_milli: max,
            on_hand_milli: g(&on_hand),
            held_milli: g(&held),
            expired_use_by_milli: g(&expired),
            open_po_milli: g(&open_po),
            draft_po_milli: g(&draft_po),
            transfers_in_milli: g(&transfers),
            requisitioned_milli: g(&requested),
            net_sold_milli: d.map(|d| d.net_sold_milli).unwrap_or(0),
            history_days: d.map(|d| d.history_days).unwrap_or(0),
            window_days: window,
            expiring_soon_milli: g(&expiring),
            waste_milli: g(&waste),
        };
        let ranked = rank_suppliers(terms.get(&pid).unwrap_or(&empty), &costs);
        let assessment = assess(&facts, ranked, cfg);
        out.push(Row { product_id: pid, name, name_ar, sku, category: cat, facts, assessment });
    }
    Ok(out)
}

const HINT_TTL: std::time::Duration = std::time::Duration::from_secs(300);

impl AppCore {
    /// How many products the engine suggests ordering, for the dashboard. A
    /// hint kept for five minutes (the engine reads every product); the
    /// Suggested orders screen always computes live and refreshes it.
    pub(crate) fn to_order_count(&self, c: &Connection, branch: &str, today: &str, fresh: bool) -> AppResult<usize> {
        let key = (branch.to_string(), today.to_string());
        if !fresh {
            if let Some((at, n)) = self.to_order_hint.lock().ok().and_then(|m| m.get(&key).copied()) {
                if at.elapsed() < HINT_TTL {
                    return Ok(n);
                }
            }
        }
        let n = run(c, branch, today, None)?.iter().filter(|r| r.assessment.state == "order").count();
        self.remember_to_order(branch, today, n);
        Ok(n)
    }

    fn remember_to_order(&self, branch: &str, today: &str, n: usize) {
        if let Ok(mut m) = self.to_order_hint.lock() {
            m.insert((branch.to_string(), today.to_string()), (std::time::Instant::now(), n));
        }
    }

    /// Suggested orders: every product's replenishment decision with its
    /// evidence. Reading only; nothing is ordered.
    pub fn replenishment(&self, token: &str, f: ReplenishFilter) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("inventory.view") && !s.has("requisitions.create") && !s.has("purchasing.manage") {
            return Err(AppError::forbidden("inventory.view"));
        }
        let show_cost = s.has("products.view_cost");
        let limit = validate::limit(f.limit, 500, 5000) as usize;
        self.db.read(|c| {
            let today = time::business_date(time::now(), &time::day(c)?)?;
            let branch = crate::branches::report_scope(c, &s, f.branch_id.as_deref())?.unwrap_or_else(|| s.branch_id.clone());
            let rows = run(c, &branch, &today, f.product_ids.as_deref())?;
            let mut counts: HashMap<&str, i64> = HashMap::new();
            for r in &rows {
                *counts.entry(r.assessment.state).or_default() += 1;
            }
            if f.product_ids.is_none() {
                self.remember_to_order(&branch, &today, counts.get("order").copied().unwrap_or(0) as usize);
            }
            let default_states = ["order", "no_supplier", "no_lead_time", "invalid_pack"];
            let wanted: Vec<String> = f.states.clone().unwrap_or_else(|| default_states.iter().map(|x| x.to_string()).collect());
            let q = f.search.as_deref().map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty());
            let sup = f.supplier_id.clone().filter(|x| !x.is_empty());
            let mut picked: Vec<Value> = vec![];
            let mut total_cost = 0i64;
            for r in rows {
                if !wanted.iter().any(|w| w == r.assessment.state) {
                    continue;
                }
                if let Some(q) = &q {
                    if !r.name.to_lowercase().contains(q) && !r.sku.to_lowercase().contains(q) {
                        continue;
                    }
                }
                if let Some(sid) = &sup {
                    if r.assessment.supplier.as_ref().map(|x| &x.supplier_id) != Some(sid) {
                        continue;
                    }
                }
                if picked.len() >= limit {
                    break;
                }
                total_cost += r.assessment.estimated_cost_minor.unwrap_or(0);
                let mut v = serde_json::to_value(&r)?;
                if !show_cost {
                    v["estimated_cost_minor"] = Value::Null;
                    for k in ["supplier"] {
                        if v[k].is_object() {
                            v[k]["last_cost_minor"] = Value::Null;
                        }
                    }
                    if let Some(a) = v["alternatives"].as_array_mut() {
                        a.iter_mut().for_each(|x| x["last_cost_minor"] = Value::Null);
                    }
                }
                picked.push(v);
            }
            let ps: PurchasingSettings = settings::get(c, settings::KEY_PURCHASING)?;
            Ok(json!({
                "today": today, "branch_id": branch, "rows": picked, "counts": counts,
                "estimated_cost_minor": if show_cost { json!(total_cost) } else { Value::Null },
                "settings": { "safety_days": ps.safety_days, "order_cycle_days": ps.order_cycle_days, "demand_window_days": ps.demand_window_days },
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sup(id: &str, upc: Option<i64>, moq: Option<i64>, lead: Option<i64>, pref: bool, cost: Option<i64>) -> SupplierChoice {
        SupplierChoice {
            supplier_id: id.into(),
            supplier_name: id.into(),
            units_per_case: upc,
            pack_source: None,
            moq_packs: moq,
            lead_time_days: lead,
            preferred: pref,
            last_cost_minor: cost,
        }
    }
    fn base() -> Facts {
        Facts { product_id: "p".into(), active: true, window_days: 30, history_days: 30, net_sold_milli: 30_000, ..Default::default() }
    }
    const CFG: Config = Config { safety_days: 3, cycle_days: 7 };

    #[test]
    fn orders_whole_packs_up_to_the_target() {
        // 1/day, lead 4 + safety 3 → reorder point 7; + cycle 7 → up to 14.
        let mut f = base();
        f.on_hand_milli = 5_000;
        let a = assess(&f, vec![sup("s", Some(6), None, Some(4), true, Some(100))], CFG);
        assert_eq!(a.state, "order");
        assert_eq!(a.reorder_point_milli, Some(7_000));
        assert_eq!(a.order_up_to_milli, Some(14_000));
        assert_eq!(a.need_milli, 9_000);
        assert_eq!((a.packs, a.suggested_milli), (Some(2), 12_000));
        assert_eq!(a.estimated_cost_minor, Some(1_200));
    }

    #[test]
    fn inbound_and_requests_are_counted_once() {
        let mut f = base();
        f.on_hand_milli = 2_000;
        f.open_po_milli = 3_000;
        f.draft_po_milli = 1_000;
        f.transfers_in_milli = 500;
        f.requisitioned_milli = 500;
        let a = assess(&f, vec![sup("s", None, None, Some(4), true, None)], CFG);
        assert_eq!(a.position_milli, 7_000);
        assert_eq!(a.state, "order");
        assert_eq!(a.need_milli, 7_000);
        // Once the requisition covers it, nothing more is suggested.
        f.requisitioned_milli += 7_000;
        assert_eq!(assess(&f, vec![sup("s", None, None, Some(4), true, None)], CFG).state, "covered");
    }

    #[test]
    fn expired_use_by_and_holds_are_not_available() {
        let mut f = base();
        f.on_hand_milli = 20_000;
        assert_eq!(assess(&f, vec![sup("s", None, None, Some(4), true, None)], CFG).state, "covered");
        f.expired_use_by_milli = 10_000;
        f.held_milli = 4_000;
        let a = assess(&f, vec![sup("s", None, None, Some(4), true, None)], CFG);
        assert_eq!(a.usable_milli, 6_000);
        assert_eq!(a.state, "order");
        assert!(a.reasons.contains(&"expired_stock_not_counted"));
    }

    #[test]
    fn explicit_states() {
        let s = || vec![sup("s", None, None, Some(4), true, None)];
        let mut f = base();
        f.history_days = 3;
        assert_eq!(assess(&f, s(), CFG).state, "insufficient_history");
        let mut f = base();
        f.net_sold_milli = 0;
        assert_eq!(assess(&f, s(), CFG).state, "no_demand");
        let f = base();
        assert_eq!(assess(&f, vec![], CFG).state, "no_supplier");
        assert_eq!(assess(&f, vec![sup("s", None, None, None, true, None)], CFG).state, "no_lead_time");
        let mut f = base();
        f.active = false;
        assert_eq!(assess(&f, s(), CFG).state, "inactive");
        let mut f = base();
        f.on_hand_milli = 100_000;
        assert_eq!(assess(&f, s(), CFG).state, "covered");
        let f = base();
        assert_eq!(assess(&f, vec![sup("s", Some(0), None, Some(4), true, None)], CFG).state, "invalid_pack");
    }

    #[test]
    fn product_levels_work_without_history() {
        let mut f = base();
        f.history_days = 0;
        f.net_sold_milli = 0;
        f.reorder_point_milli = 10_000;
        f.max_stock_milli = Some(24_000);
        f.on_hand_milli = 4_000;
        let a = assess(&f, vec![sup("s", Some(12), Some(1), None, true, None)], CFG);
        assert_eq!(a.state, "order");
        assert_eq!(a.order_up_to_source, Some("max_stock"));
        assert_eq!((a.packs, a.suggested_milli), (Some(2), 24_000));
    }

    #[test]
    fn supplier_choice_is_deterministic_with_alternatives() {
        let t = |id: &str, name: &str, pref: bool, lead: Option<i64>| Terms {
            supplier_id: id.into(),
            supplier_name: name.into(),
            supplier_active: true,
            product_id: "p".into(),
            supplier_code: None,
            units_per_case: None,
            pack_source: None,
            moq_packs: None,
            lead_time_days: lead,
            preferred: pref,
            active: true,
            terms_confirmed_by: None,
            terms_confirmed_at: None,
            version: 1,
        };
        let mut costs = HashMap::new();
        costs.insert(("b".to_string(), "p".to_string()), 90);
        costs.insert(("c".to_string(), "p".to_string()), 120);
        let r = rank_suppliers(&[t("c", "C", false, Some(2)), t("b", "B", false, Some(5)), t("a", "A", false, None)], &costs);
        assert_eq!(r.iter().map(|x| x.supplier_id.as_str()).collect::<Vec<_>>(), ["b", "c", "a"]);
        assert_eq!(supplier_reason(&r), Some("lowest_last_cost"));
        let r = rank_suppliers(&[t("c", "C", false, Some(2)), t("a", "A", true, None)], &costs);
        assert_eq!(r[0].supplier_id, "a");
        assert_eq!(supplier_reason(&r), Some("preferred_supplier"));
    }

    #[test]
    fn waste_is_a_warning_only() {
        let mut f = base();
        f.on_hand_milli = 5_000;
        let q = assess(&f, vec![sup("s", None, None, Some(4), true, None)], CFG).suggested_milli;
        f.waste_milli = 20_000;
        let a = assess(&f, vec![sup("s", None, None, Some(4), true, None)], CFG);
        assert!(a.warnings.contains(&"high_waste"));
        assert_eq!(a.suggested_milli, q);
    }
}
