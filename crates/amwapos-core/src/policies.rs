//! Pricing policies, rounding and margin protection
//! (docs/PRICING_AND_CATALOGUE.md).
//!
//! A policy recommends; it never changes a price by itself. Prices change
//! only when a person applies recommendations (one or many, atomically,
//! audited, with price history for what was applied).
//!
//! Money is integer fils; rates are basis points (1% = 100 bp).
//! - Markup is on cost: net = cost × (1 + markup).
//! - Margin is on the selling price: net = cost ÷ (1 − margin).
//! - "net" is the price without VAT; when prices include VAT the shelf price
//!   is net × (1 + VAT). Margins are always measured on the net price.
//! - Rounding is applied to the shelf price (what the customer pays), to a
//!   step (0.005, 0.010, 0.025, 0.050, 0.100 …) and optionally to a preferred
//!   ending (e.g. x.950); the result is then moved up, never down, until it
//!   is at or above every minimum margin that applies.
//!
//! Which policy applies (deterministic): of the active policies that match
//! the product, the most specific scope wins — channel, then branch, then
//! the product's preferred supplier, then its category, then global — and
//! within a scope the highest priority. Two policies with the same scope and
//! priority are an ambiguity: shown to the person, and settled meanwhile by
//! name, then id. The minimum margin is the highest of all matching
//! policies' minimums (a broader rule's floor still applies).

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::catalog::{self, PriceList};
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::new_id;
use crate::service::AppCore;
use crate::time;
use crate::validate;

pub const STEPS: [i64; 9] = [1, 5, 10, 25, 50, 100, 250, 500, 1000];
const SCOPES: [&str; 5] = ["global", "category", "supplier", "branch", "channel"];

fn rank(scope: &str) -> i64 {
    match scope {
        "channel" => 5,
        "branch" => 4,
        "supplier" => 3,
        "category" => 2,
        _ => 1,
    }
}

// ------------------------------------------------------------------ arithmetic

/// The shelf-price denominator: 10000 + VAT when prices include VAT.
fn denom(tax_rate_bp: i64, inclusive: bool) -> i128 {
    if inclusive {
        10_000 + tax_rate_bp as i128
    } else {
        10_000
    }
}

/// Margin of a shelf price on cost, in bp of the net price (rounded down, so
/// a reported margin is never better than the real one). None without a price.
pub fn margin_bp(shelf: i64, cost: i64, tax_rate_bp: i64, inclusive: bool) -> Option<i64> {
    if shelf <= 0 {
        return None;
    }
    let num = shelf as i128 * 10_000 - cost as i128 * denom(tax_rate_bp, inclusive);
    Some(num.div_euclid(shelf as i128) as i64)
}

/// Markup of a shelf price on cost, in bp of cost (rounded down).
pub fn markup_bp(shelf: i64, cost: i64, tax_rate_bp: i64, inclusive: bool) -> Option<i64> {
    if cost <= 0 {
        return None;
    }
    // net = shelf × 10000 / D; markup = (net − cost) / cost.
    let d = denom(tax_rate_bp, inclusive);
    let num = (shelf as i128 * 10_000 - cost as i128 * d) * 10_000;
    Some(num.div_euclid(cost as i128 * d) as i64)
}

/// Exact check: the shelf price keeps at least `min_bp` margin on its net.
/// shelf × (10000 − min) ≥ cost × D.
pub fn meets_floor(shelf: i64, cost: i64, min_bp: i64, tax_rate_bp: i64, inclusive: bool) -> bool {
    shelf as i128 * (10_000 - min_bp as i128) >= cost as i128 * denom(tax_rate_bp, inclusive)
}

/// The lowest shelf price that keeps `min_bp` margin.
pub fn floor_price(cost: i64, min_bp: i64, tax_rate_bp: i64, inclusive: bool) -> i64 {
    let num = cost as i128 * denom(tax_rate_bp, inclusive);
    let den = (10_000 - min_bp) as i128;
    ((num + den - 1) / den) as i64
}

fn half_up(num: i128, den: i128) -> i128 {
    (2 * num + den).div_euclid(2 * den)
}

/// The unrounded target shelf price for a markup or a margin.
pub fn target_price(cost: i64, markup: Option<i64>, margin: Option<i64>, tax_rate_bp: i64, inclusive: bool) -> Option<i64> {
    let d = denom(tax_rate_bp, inclusive);
    let c = cost as i128;
    match (markup, margin) {
        (Some(m), None) => Some(half_up(c * (10_000 + m as i128) * d, 100_000_000) as i64),
        (None, Some(m)) if m < 10_000 => Some(half_up(c * d, (10_000 - m) as i128) as i64),
        _ => None,
    }
}

/// Round a shelf price to `step` (nearest, halves up), then to the
/// preferred `ending` (the nearest price whose last three digits are the
/// ending), then move up — never down — until it meets `floor`.
pub fn round_price(price: i64, step: i64, ending: Option<i64>, floor: i64) -> i64 {
    let step = step.max(1);
    let up_to_step = |p: i64| (p + step - 1).div_euclid(step) * step;
    let mut p = match ending {
        Some(e) => {
            // Candidates: the ending in this BHD and the one below/above.
            let base = price.div_euclid(1000) * 1000;
            let cands = [base - 1000 + e, base + e, base + 1000 + e];
            *cands.iter().filter(|c| **c > 0).min_by_key(|c| ((*c - price).abs(), -**c)).unwrap_or(&(base + e))
        }
        None => half_up(price as i128, step as i128) as i64 * step,
    };
    if p < floor {
        p = match ending {
            Some(e) => {
                let base = floor.div_euclid(1000) * 1000 + e;
                if base >= floor {
                    base
                } else {
                    base + 1000
                }
            }
            None => up_to_step(floor),
        };
    }
    // A price is never rounded to nothing.
    if p <= 0 {
        p = ending.filter(|e| *e > 0).unwrap_or(step).max(floor);
    }
    p
}

// ------------------------------------------------------------------ policies

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub policy_id: String,
    pub name: String,
    pub scope: String,
    #[serde(default)]
    pub scope_id: Option<String>,
    #[serde(default)]
    pub markup_bp: Option<i64>,
    #[serde(default)]
    pub target_margin_bp: Option<i64>,
    #[serde(default)]
    pub min_margin_bp: Option<i64>,
    #[serde(default = "one")]
    pub rounding_step_minor: i64,
    #[serde(default)]
    pub ending_minor: Option<i64>,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "average")]
    pub cost_basis: String,
    #[serde(default = "yes")]
    pub active: bool,
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub updated_at: Option<String>,
}

fn one() -> i64 {
    1
}
fn yes() -> bool {
    true
}
fn average() -> String {
    "average".into()
}

const COLS: &str = "policy_id, name, scope, scope_id, markup_bp, target_margin_bp, min_margin_bp, rounding_step_minor, ending_minor,
    priority, cost_basis, active, version, updated_at";

fn map_policy(r: &rusqlite::Row) -> rusqlite::Result<Policy> {
    Ok(Policy {
        policy_id: r.get(0)?,
        name: r.get(1)?,
        scope: r.get(2)?,
        scope_id: r.get(3)?,
        markup_bp: r.get(4)?,
        target_margin_bp: r.get(5)?,
        min_margin_bp: r.get(6)?,
        rounding_step_minor: r.get(7)?,
        ending_minor: r.get(8)?,
        priority: r.get(9)?,
        cost_basis: r.get(10)?,
        active: r.get::<_, i64>(11)? == 1,
        version: r.get(12)?,
        updated_at: r.get(13)?,
    })
}

pub fn validate_policy(c: &Connection, p: &Policy) -> AppResult<()> {
    let name = p.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(AppError::validation("Give the policy a name (at most 80 characters)."));
    }
    if !SCOPES.contains(&p.scope.as_str()) {
        return Err(AppError::validation("Choose what the policy applies to."));
    }
    let sid = p.scope_id.as_deref().filter(|x| !x.is_empty());
    if (p.scope == "global") != sid.is_none() {
        return Err(AppError::validation(if p.scope == "global" {
            "A policy for all products has no category, supplier, branch or channel."
        } else {
            "Choose the category, supplier, branch or channel the policy applies to."
        }));
    }
    if let Some(id) = sid {
        let ok = match p.scope.as_str() {
            "category" => c.query_row("SELECT 1 FROM categories WHERE category_id=?1", [id], |_| Ok(true)).optional()?.is_some(),
            "supplier" => c.query_row("SELECT 1 FROM suppliers WHERE supplier_id=?1", [id], |_| Ok(true)).optional()?.is_some(),
            "branch" => c.query_row("SELECT 1 FROM branches WHERE branch_id=?1", [id], |_| Ok(true)).optional()?.is_some(),
            "channel" => crate::pos::SALE_CHANNELS.contains(&id),
            _ => false,
        };
        if !ok {
            return Err(AppError::validation("The category, supplier, branch or channel was not found."));
        }
    }
    if p.markup_bp.is_some() && p.target_margin_bp.is_some() {
        return Err(AppError::validation("Use either a markup on cost or a margin on the price, not both."));
    }
    if p.markup_bp.is_some_and(|m| !(0..=100_000).contains(&m)) {
        return Err(AppError::validation("Markup is between 0% and 1000%."));
    }
    for (v, what) in [(p.target_margin_bp, "Target margin"), (p.min_margin_bp, "Minimum margin")] {
        if v.is_some_and(|m| !(0..=9_500).contains(&m)) {
            return Err(AppError::validation(format!("{what} is between 0% and 95%.")));
        }
    }
    if p.markup_bp.is_none() && p.target_margin_bp.is_none() && p.min_margin_bp.is_none() {
        return Err(AppError::validation("Set a markup, a target margin or a minimum margin."));
    }
    // A target below the policy's own minimum could never be met.
    if let (Some(t), Some(m)) = (p.target_margin_bp, p.min_margin_bp) {
        if t < m {
            return Err(AppError::validation("The target margin is below the minimum margin."));
        }
    }
    if !STEPS.contains(&p.rounding_step_minor) {
        return Err(AppError::validation("Choose a rounding step from the list."));
    }
    if p.ending_minor.is_some_and(|e| !(0..=999).contains(&e)) {
        return Err(AppError::validation("The preferred ending is between .000 and .999."));
    }
    if !matches!(p.cost_basis.as_str(), "average" | "last") {
        return Err(AppError::validation("Choose the cost the policy works from: average or last."));
    }
    if p.priority.abs() > 1000 {
        return Err(AppError::validation("Priority is between -1000 and 1000."));
    }
    Ok(())
}

pub fn active_policies(c: &Connection) -> AppResult<Vec<Policy>> {
    let mut st = c.prepare_cached(&format!("SELECT {COLS} FROM pricing_policies WHERE active=1 ORDER BY policy_id"))?;
    let v = st.query_map([], map_policy)?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// What a product looks like to the policies.
#[derive(Debug, Clone)]
pub struct Subject<'a> {
    pub category_id: Option<&'a str>,
    pub supplier_id: Option<&'a str>,
    pub branch_id: &'a str,
    /// The sale channel of the price list (retail → pos).
    pub channel: &'a str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Resolved {
    pub policy: Option<Policy>,
    /// Highest minimum margin among all matching policies.
    pub floor_bp: Option<i64>,
    /// Names of policies tied with the winner (same scope and priority).
    pub ambiguous_with: Vec<String>,
}

fn matches(p: &Policy, s: &Subject) -> bool {
    let id = p.scope_id.as_deref();
    match p.scope.as_str() {
        "global" => true,
        "category" => id.is_some() && id == s.category_id,
        "supplier" => id.is_some() && id == s.supplier_id,
        "branch" => id == Some(s.branch_id),
        "channel" => id == Some(s.channel),
        _ => false,
    }
}

pub fn resolve(policies: &[Policy], s: &Subject) -> Resolved {
    let mut m: Vec<&Policy> = policies.iter().filter(|p| p.active && matches(p, s)).collect();
    let floor_bp = m.iter().filter_map(|p| p.min_margin_bp).max();
    m.sort_by(|a, b| {
        (rank(&b.scope), b.priority)
            .cmp(&(rank(&a.scope), a.priority))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.policy_id.cmp(&b.policy_id))
    });
    let Some(w) = m.first().copied() else { return Resolved { policy: None, floor_bp, ambiguous_with: vec![] } };
    let ambiguous_with =
        m.iter().skip(1).filter(|p| rank(&p.scope) == rank(&w.scope) && p.priority == w.priority).map(|p| p.name.clone()).collect();
    Resolved { policy: Some(w.clone()), floor_bp, ambiguous_with }
}

/// The recommended shelf price (None: the policy has no target and the
/// current price already meets the floor, or there is no cost).
pub fn recommend(r: &Resolved, cost: i64, current: Option<i64>, tax_rate_bp: i64, inclusive: bool) -> Option<i64> {
    let p = r.policy.as_ref()?;
    if cost <= 0 {
        return None;
    }
    let floor = r.floor_bp.map(|m| floor_price(cost, m, tax_rate_bp, inclusive)).unwrap_or(0);
    let target = target_price(cost, p.markup_bp, p.target_margin_bp, tax_rate_bp, inclusive);
    let raw = match (target, current) {
        (Some(t), _) => t,
        // Only a minimum: recommend a change only when the price is below it.
        (None, Some(cur)) if cur < floor => floor,
        _ => return None,
    };
    Some(round_price(raw, p.rounding_step_minor, p.ending_minor, floor))
}

// ------------------------------------------------------------------ review

#[derive(Debug, Clone, Serialize)]
pub struct ReviewRow {
    pub product_id: String,
    pub name: String,
    pub sku: String,
    pub price_type: String,
    pub current_minor: Option<i64>,
    pub recommended_minor: Option<i64>,
    pub cost_minor: i64,
    pub cost_basis: String,
    pub margin_bp: Option<i64>,
    pub recommended_margin_bp: Option<i64>,
    pub floor_bp: Option<i64>,
    pub floor_price_minor: Option<i64>,
    pub policy_id: Option<String>,
    pub policy_name: Option<String>,
    pub ambiguous_with: Vec<String>,
    /// below_min_margin | cost_changed | no_policy | channel_price_missing | recommendation
    pub groups: Vec<String>,
    pub tax_rate_bp: i64,
    pub tax_inclusive: bool,
}

struct Prod {
    id: String,
    name: String,
    sku: String,
    category: Option<String>,
    rate: i64,
    inclusive: bool,
    avg: i64,
    last: i64,
    price_set_at: Option<String>,
    prices: HashMap<String, i64>,
}

/// The pricing review for one branch: every active product with a cost,
/// grouped by what needs attention. One pass over the catalogue; policies
/// are held in memory (no per-product policy query).
pub fn review(c: &Connection, branch_id: &str) -> AppResult<Vec<ReviewRow>> {
    let policies = active_policies(c)?;
    let channel_lists: Vec<&str> = catalog::CHANNEL_PRICE_TYPES
        .iter()
        .copied()
        .filter(|t| policies.iter().any(|p| p.scope == "channel" && p.scope_id.as_deref() == Some(*t)))
        .collect();
    let mut st = c.prepare(
        "SELECT p.product_id, p.name, p.sku, p.category_id, t.rate_bp, t.inclusive,
                COALESCE(pc.avg_cost_minor,0), COALESCE(pc.last_cost_minor,0)
         FROM products p JOIN tax_rules t ON t.tax_rule_id=p.tax_rule_id
         LEFT JOIN product_costs pc ON pc.product_id=p.product_id AND pc.branch_id=?1
         WHERE p.active=1 AND p.merged_into_product_id IS NULL",
    )?;
    let mut prods: Vec<Prod> = st
        .query_map([branch_id], |r| {
            Ok(Prod {
                id: r.get(0)?,
                name: r.get(1)?,
                sku: r.get(2)?,
                category: r.get(3)?,
                rate: r.get(4)?,
                inclusive: r.get::<_, i64>(5)? == 1,
                avg: r.get(6)?,
                last: r.get(7)?,
                price_set_at: None,
                prices: HashMap::new(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let index: HashMap<String, usize> = prods.iter().enumerate().map(|(i, p)| (p.id.clone(), i)).collect();
    // Current own prices per list (shared, this branch's override wins), in one query.
    let mut st = c.prepare(
        "SELECT product_id, price_type, branch_id, amount_minor, effective_from FROM product_prices
         WHERE effective_from <= amw_now() AND (effective_to IS NULL OR effective_to > amw_now()) AND (branch_id IS NULL OR branch_id=?1)
         ORDER BY product_id, price_type, (branch_id IS NULL), effective_from DESC, price_id DESC",
    )?;
    let multi = crate::settings::get::<crate::settings::FeatureFlags>(c, crate::settings::KEY_FEATURES)?.is_on("org.multi_branch");
    let mut rows = st.query([branch_id])?;
    while let Some(r) = rows.next()? {
        let (pid, ty, b, amount, from): (String, String, Option<String>, i64, String) =
            (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
        if b.is_some() && !multi {
            continue;
        }
        if let Some(&i) = index.get(&pid) {
            if !prods[i].prices.contains_key(&ty) {
                if ty == "retail" {
                    prods[i].price_set_at = Some(from);
                }
                prods[i].prices.insert(ty, amount);
            }
        }
    }
    let mut preferred: HashMap<String, String> = HashMap::new();
    let mut st = c.prepare("SELECT product_id, supplier_id FROM supplier_products WHERE preferred=1 AND active=1")?;
    for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (p, s) = r?;
        preferred.insert(p, s);
    }
    let mut cost_changed: HashMap<String, String> = HashMap::new();
    let mut st = c.prepare("SELECT product_id, MAX(effective_at) FROM product_cost_history GROUP BY product_id")?;
    for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (p, at) = r?;
        cost_changed.insert(p, at);
    }
    let today = time::now_str();
    /// decision, cost and suggestion when decided, postponed-until date.
    type Decision = (String, Option<i64>, Option<i64>, Option<String>);
    let mut decisions: HashMap<(String, String), Decision> = HashMap::new();
    let mut st =
        c.prepare("SELECT product_id, price_type, decision, cost_minor, suggested_minor, until FROM price_recommendation_decisions")?;
    for r in st.query_map([], |r| Ok(((r.get::<_, String>(0)?, r.get::<_, String>(1)?), (r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))))? {
        let (k, v) = r?;
        decisions.insert(k, v);
    }
    let mut out = vec![];
    for p in &prods {
        let lists: Vec<&str> = std::iter::once("retail").chain(channel_lists.iter().copied()).collect();
        for ty in lists {
            let channel = if ty == "retail" { "pos" } else { ty };
            let subject =
                Subject { category_id: p.category.as_deref(), supplier_id: preferred.get(&p.id).map(String::as_str), branch_id, channel };
            let r = resolve(&policies, &subject);
            let basis = r.policy.as_ref().map(|x| x.cost_basis.as_str()).unwrap_or("average");
            let cost = if basis == "last" || p.avg <= 0 { p.last } else { p.avg };
            if cost <= 0 {
                continue;
            }
            let own = p.prices.get(ty).copied();
            let current = own.or_else(|| p.prices.get("retail").copied());
            let rec = recommend(&r, cost, current, p.rate, p.inclusive);
            let floor_price_minor = r.floor_bp.map(|m| floor_price(cost, m, p.rate, p.inclusive));
            let mut groups = vec![];
            if let (Some(cur), Some(fp)) = (current, floor_price_minor) {
                if cur < fp {
                    groups.push("below_min_margin".to_string());
                }
            }
            if r.policy.is_none() && ty == "retail" {
                groups.push("no_policy".into());
            }
            if ty != "retail" && own.is_none() {
                groups.push("channel_price_missing".into());
            }
            if let (Some(changed), Some(set)) = (cost_changed.get(&p.id), &p.price_set_at) {
                if changed > set && rec.is_some() && rec != current {
                    groups.push("cost_changed".into());
                }
            }
            if rec.is_some() && rec != current {
                groups.push("recommendation".into());
            }
            if groups.is_empty() {
                continue;
            }
            // Set aside by a person: dismissed while cost and suggestion are the
            // same; postponed until a date. Below-minimum stays visible.
            if let Some((d, dc, ds, until)) = decisions.get(&(p.id.clone(), ty.to_string())) {
                let hidden = match d.as_str() {
                    "dismissed" => *dc == Some(cost) && *ds == rec,
                    _ => until.as_deref().is_some_and(|u| u > today.as_str()),
                };
                if hidden && !groups.iter().any(|g| g == "below_min_margin") {
                    continue;
                }
            }
            out.push(ReviewRow {
                product_id: p.id.clone(),
                name: p.name.clone(),
                sku: p.sku.clone(),
                price_type: ty.to_string(),
                current_minor: current,
                recommended_minor: rec,
                cost_minor: cost,
                cost_basis: basis.to_string(),
                margin_bp: current.and_then(|x| margin_bp(x, cost, p.rate, p.inclusive)),
                recommended_margin_bp: rec.and_then(|x| margin_bp(x, cost, p.rate, p.inclusive)),
                floor_bp: r.floor_bp,
                floor_price_minor,
                policy_id: r.policy.as_ref().map(|x| x.policy_id.clone()),
                policy_name: r.policy.as_ref().map(|x| x.name.clone()),
                ambiguous_with: r.ambiguous_with.clone(),
                groups,
                tax_rate_bp: p.rate,
                tax_inclusive: p.inclusive,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.price_type.cmp(&b.price_type)));
    Ok(out)
}

/// Margin check for one price: the floor that applies to this product's
/// retail list in `branch_id`, and whether `price` meets it.
pub fn floor_for(c: &Connection, product_id: &str, branch_id: &str, price_type: &str) -> AppResult<Option<(i64, i64, String)>> {
    let policies = active_policies(c)?;
    if policies.is_empty() {
        return Ok(None);
    }
    let (cat, rate, incl, avg, last): (Option<String>, i64, i64, i64, i64) = c.query_row(
        "SELECT p.category_id, t.rate_bp, t.inclusive, COALESCE(pc.avg_cost_minor,0), COALESCE(pc.last_cost_minor,0)
         FROM products p JOIN tax_rules t ON t.tax_rule_id=p.tax_rule_id
         LEFT JOIN product_costs pc ON pc.product_id=p.product_id AND pc.branch_id=?2 WHERE p.product_id=?1",
        params![product_id, branch_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    let sup: Option<String> = c
        .query_row("SELECT supplier_id FROM supplier_products WHERE product_id=?1 AND preferred=1 AND active=1", [product_id], |r| r.get(0))
        .optional()?;
    let channel = if price_type == "retail" { "pos" } else { price_type };
    let r = resolve(&policies, &Subject { category_id: cat.as_deref(), supplier_id: sup.as_deref(), branch_id, channel });
    let Some(min) = r.floor_bp else { return Ok(None) };
    let basis = r.policy.as_ref().map(|x| x.cost_basis.as_str()).unwrap_or("average");
    let cost = if basis == "last" || avg <= 0 { last } else { avg };
    if cost <= 0 {
        return Ok(None);
    }
    Ok(Some((floor_price(cost, min, rate, incl == 1), min, r.policy.map(|p| p.name).unwrap_or_default())))
}

// ------------------------------------------------------------------ apply

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApplyItem {
    pub product_id: String,
    #[serde(default = "retail")]
    pub price_type: String,
    pub amount_minor: i64,
}

fn retail() -> String {
    "retail".into()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApplyRequest {
    pub items: Vec<ApplyItem>,
    #[serde(default)]
    pub reason: Option<String>,
    pub operation_id: String,
    #[serde(default, skip_serializing)]
    pub approval_token: Option<String>,
}

/// Each item with its old price, new price, margins and floor.
fn apply_preview_rows(c: &Connection, branch_id: &str, items: &[ApplyItem]) -> AppResult<Vec<Value>> {
    let mut out = vec![];
    for it in items {
        let pid = validate::id(&it.product_id, "Product")?;
        if !catalog::PRICE_TYPES.contains(&it.price_type.as_str()) {
            return Err(AppError::validation("Unknown price list."));
        }
        validate::money_non_negative(it.amount_minor, "Price")?;
        if it.amount_minor == 0 {
            return Err(AppError::validation("A price cannot be zero."));
        }
        let (name, rate, incl, cost): (String, i64, i64, i64) = c
            .query_row(
                "SELECT p.name, t.rate_bp, t.inclusive,
                    COALESCE((SELECT CASE WHEN avg_cost_minor>0 THEN avg_cost_minor ELSE last_cost_minor END FROM product_costs
                              WHERE product_id=p.product_id AND branch_id=?2),0)
                 FROM products p JOIN tax_rules t ON t.tax_rule_id=p.tax_rule_id WHERE p.product_id=?1 AND p.active=1",
                params![pid, branch_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| AppError::not_found("Product"))?;
        let old: Option<i64> = c
            .query_row(
                &format!("SELECT {} FROM products p WHERE p.product_id=?1", catalog::price_sql_for(&it.price_type, "amount_minor")),
                [&pid],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let floor = floor_for(c, &pid, branch_id, &it.price_type)?;
        let below = floor.as_ref().is_some_and(|(fp, _, _)| it.amount_minor < *fp);
        out.push(json!({
            "product_id": pid, "name": name, "price_type": it.price_type, "old_minor": old, "new_minor": it.amount_minor,
            "unchanged": old == Some(it.amount_minor), "cost_minor": cost,
            "old_margin_bp": old.and_then(|o| margin_bp(o, cost, rate, incl == 1)),
            "new_margin_bp": margin_bp(it.amount_minor, cost, rate, incl == 1),
            "floor_bp": floor.as_ref().map(|f| f.1), "floor_price_minor": floor.as_ref().map(|f| f.0), "below_min_margin": below,
        }));
    }
    Ok(out)
}

impl AppCore {
    pub fn pricing_policies_list(&self, token: &str) -> AppResult<Vec<Policy>> {
        let s = self.session(token)?;
        if !s.has("pricing.policy") && !s.has("prices.manage") {
            return Err(AppError::forbidden("pricing.policy"));
        }
        self.db.read(|c| {
            let mut st = c.prepare(&format!("SELECT {COLS} FROM pricing_policies ORDER BY active DESC, scope, priority DESC, name"))?;
            let v = st.query_map([], map_policy)?.collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
    }

    /// Create or change a policy. Saving a policy changes no price.
    pub fn pricing_policy_save(&self, token: &str, p: Policy) -> AppResult<Policy> {
        let s = self.session(token)?;
        s.require("pricing.policy")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let now = time::now_str();
        let id = self.db.write(|tx| {
            validate_policy(tx, &p)?;
            let sid = p.scope_id.clone().filter(|x| !x.is_empty());
            let name = p.name.trim().to_string();
            let vals = params![
                name,
                p.scope,
                sid,
                p.markup_bp,
                p.target_margin_bp,
                p.min_margin_bp,
                p.rounding_step_minor,
                p.ending_minor,
                p.priority,
                p.cost_basis,
                p.active as i64,
                now
            ];
            if p.policy_id.is_empty() {
                let id = new_id();
                tx.execute(
                    "INSERT INTO pricing_policies(name, scope, scope_id, markup_bp, target_margin_bp, min_margin_bp, rounding_step_minor, ending_minor,
                        priority, cost_basis, active, created_at, updated_at, policy_id, created_by, version)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12,?13,?14,1)",
                    rusqlite::params_from_iter(vals.iter().copied().chain([&id as &dyn rusqlite::ToSql, &s.user_id as &dyn rusqlite::ToSql])),
                )?;
                audit::record(tx, &actor, "pricing_policy.created", "pricing_policy", Some(&id), None, Some(&json!(p)))?;
                Ok(id)
            } else {
                let id = validate::id(&p.policy_id, "Policy")?;
                let before: Policy = tx
                    .query_row(&format!("SELECT {COLS} FROM pricing_policies WHERE policy_id=?1"), [&id], map_policy)
                    .optional()?
                    .ok_or_else(|| AppError::not_found("Pricing policy"))?;
                if before.version != p.version {
                    return Err(AppError::conflict("This policy was changed by someone else. Reload it and try again."));
                }
                tx.execute(
                    "UPDATE pricing_policies SET name=?1, scope=?2, scope_id=?3, markup_bp=?4, target_margin_bp=?5, min_margin_bp=?6,
                        rounding_step_minor=?7, ending_minor=?8, priority=?9, cost_basis=?10, active=?11, updated_at=?12, version=version+1
                     WHERE policy_id=?13",
                    rusqlite::params_from_iter(vals.iter().copied().chain([&id as &dyn rusqlite::ToSql])),
                )?;
                audit::record(tx, &actor, "pricing_policy.changed", "pricing_policy", Some(&id), Some(&json!(before)), Some(&json!(p)))?;
                Ok(id)
            }
        })?;
        self.db.read(|c| Ok(c.query_row(&format!("SELECT {COLS} FROM pricing_policies WHERE policy_id=?1"), [&id], map_policy)?))
    }

    /// The pricing review: counts per group and the rows of one group.
    pub fn pricing_review(&self, token: &str, group: Option<String>, limit: Option<i64>, offset: Option<i64>) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("prices.manage") && !s.has("pricing.policy") {
            return Err(AppError::forbidden("prices.manage"));
        }
        if !s.has("products.view_cost") {
            return Err(AppError::forbidden("products.view_cost"));
        }
        let limit = validate::limit(limit, 100, 500) as usize;
        let offset = offset.unwrap_or(0).max(0) as usize;
        self.db.read(|c| {
            let rows = review(c, &s.branch_id)?;
            let mut counts: HashMap<&str, i64> = HashMap::new();
            for r in &rows {
                for g in &r.groups {
                    *counts.entry(g.as_str()).or_default() += 1;
                }
            }
            let counts = json!({
                "below_min_margin": counts.get("below_min_margin").copied().unwrap_or(0),
                "cost_changed": counts.get("cost_changed").copied().unwrap_or(0),
                "no_policy": counts.get("no_policy").copied().unwrap_or(0),
                "channel_price_missing": counts.get("channel_price_missing").copied().unwrap_or(0),
                "recommendation": counts.get("recommendation").copied().unwrap_or(0),
            });
            let ambiguous: Vec<Value> = {
                let mut seen = std::collections::BTreeSet::new();
                rows.iter()
                    .filter(|r| !r.ambiguous_with.is_empty())
                    .filter_map(|r| {
                        let k = format!("{}|{}", r.policy_name.clone().unwrap_or_default(), r.ambiguous_with.join(","));
                        seen.insert(k).then(|| json!({ "policy": r.policy_name, "tied_with": r.ambiguous_with }))
                    })
                    .collect()
            };
            let selected: Vec<&ReviewRow> = match &group {
                Some(g) if !g.is_empty() => rows.iter().filter(|r| r.groups.iter().any(|x| x == g)).collect(),
                _ => rows.iter().collect(),
            };
            let total = selected.len();
            let page: Vec<&ReviewRow> = selected.into_iter().skip(offset).take(limit).collect();
            let policies: i64 = c.query_row("SELECT COUNT(*) FROM pricing_policies WHERE active=1", [], |r| r.get(0))?;
            Ok(json!({ "counts": counts, "total": total, "rows": page, "ambiguous": ambiguous, "active_policies": policies }))
        })
    }

    /// Set a recommendation aside: dismissed (until cost or suggestion
    /// changes) or postponed until a date. `None` clears it.
    pub fn pricing_decide(
        &self,
        token: &str,
        product_id: &str,
        price_type: &str,
        decision: Option<String>,
        until: Option<String>,
    ) -> AppResult<()> {
        let s = self.session(token)?;
        s.require("prices.manage")?;
        self.require_back_office_writable()?;
        let pid = validate::id(product_id, "Product")?;
        if !catalog::PRICE_TYPES.contains(&price_type) {
            return Err(AppError::validation("Unknown price list."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            match decision.as_deref() {
                None => {
                    tx.execute("DELETE FROM price_recommendation_decisions WHERE product_id=?1 AND price_type=?2", params![pid, price_type])?;
                }
                Some(d @ ("dismissed" | "postponed")) => {
                    let row = review(tx, &s.branch_id)?.into_iter().find(|r| r.product_id == pid && r.price_type == price_type);
                    let (cost, sugg) = row.map(|r| (Some(r.cost_minor), r.recommended_minor)).unwrap_or((None, None));
                    let until = if d == "postponed" {
                        let u = until.as_deref().ok_or_else(|| AppError::validation("Choose until when to postpone."))?;
                        let date = chrono::NaiveDate::parse_from_str(u, "%Y-%m-%d").map_err(|_| AppError::validation("Enter a date like 2026-12-31."))?;
                        Some(date.to_string())
                    } else {
                        None
                    };
                    tx.execute(
                        "INSERT INTO price_recommendation_decisions(product_id, price_type, decision, cost_minor, suggested_minor, until, decided_by, decided_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                         ON CONFLICT(product_id, price_type) DO UPDATE SET decision=?3, cost_minor=?4, suggested_minor=?5, until=?6, decided_by=?7, decided_at=?8",
                        params![pid, price_type, d, cost, sugg, until, s.user_id, time::now_str()],
                    )?;
                }
                Some(_) => return Err(AppError::validation("Choose Dismiss or Postpone.")),
            }
            audit::record(tx, &actor, "pricing.decided", "product", Some(&pid), None, Some(&json!({ "price_type": price_type, "decision": decision, "until": until })))?;
            Ok(())
        })
    }

    /// What applying these prices would do (nothing is changed).
    pub fn pricing_apply_preview(&self, token: &str, items: Vec<ApplyItem>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("prices.manage")?;
        if items.is_empty() || items.len() > 5_000 {
            return Err(AppError::validation("Choose between 1 and 5,000 prices."));
        }
        self.db.read(|c| {
            let rows = apply_preview_rows(c, &s.branch_id, &items)?;
            let below = rows.iter().filter(|r| r["below_min_margin"] == true).count();
            let changes = rows.iter().filter(|r| r["unchanged"] == false).count();
            Ok(json!({ "rows": rows, "changes": changes, "below_min_margin": below, "needs_approval": below > 0 && !s.has("pricing.policy") }))
        })
    }

    /// Apply prices, all or nothing, once per operation id. A price below a
    /// minimum margin needs `pricing.policy` (or a bound approval).
    pub fn pricing_apply(&self, token: &str, req: ApplyRequest) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("prices.manage")?;
        self.require_back_office_writable()?;
        if req.items.is_empty() || req.items.len() > 5_000 {
            return Err(AppError::validation("Choose between 1 and 5,000 prices."));
        }
        let mut seen = std::collections::HashSet::new();
        for it in &req.items {
            if !seen.insert((it.product_id.clone(), it.price_type.clone())) {
                return Err(AppError::validation("A product appears twice for the same price list."));
            }
        }
        let reason = crate::setup::clean_opt(&req.reason, "Reason", 200)?;
        let below: usize = self.db.read(|c| {
            Ok(apply_preview_rows(c, &s.branch_id, &req.items)?
                .iter()
                .filter(|r| r["below_min_margin"] == true && r["unchanged"] == false)
                .count())
        })?;
        let approved_by = if below > 0 {
            let payload = json!({ "items": req.items, "operation_id": req.operation_id });
            self.authorize_bound(
                &s,
                "pricing.policy",
                req.approval_token.as_deref(),
                &format!("{below} price(s) below the minimum margin"),
                "pricing.apply",
                "pricing",
                &payload,
            )?
        } else {
            None
        };
        let actor = self.actor(&s, approved_by.clone());
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "pricing.apply", &req)? {
                Check::Replay { result } => return Ok(result),
                Check::New { payload_hash } => payload_hash,
            };
            let rows = apply_preview_rows(tx, &s.branch_id, &req.items)?;
            let batch_id = new_id();
            let now = time::now_str();
            let policies = active_policies(tx)?;
            let mut applied = vec![];
            for (it, row) in req.items.iter().zip(&rows) {
                if row["unchanged"] == true {
                    continue;
                }
                // The policy that recommended it, when there is one (evidence).
                let policy_id = floor_policy_id(tx, &policies, &it.product_id, &s.branch_id, &it.price_type)?;
                let list = PriceList { price_type: &it.price_type, branch_id: None, policy_id: policy_id.as_deref(), batch_id: Some(&batch_id) };
                catalog::set_price_typed(tx, &s, &it.product_id, &list, Some(it.amount_minor), reason.as_deref().or(Some("Pricing review")), &now, &actor)?;
                applied.push(row.clone());
                tx.execute("DELETE FROM price_recommendation_decisions WHERE product_id=?1 AND price_type=?2", params![it.product_id, it.price_type])?;
            }
            let summary = json!({ "applied": applied.len(), "skipped_unchanged": rows.len() - applied.len(), "below_min_margin": below, "reason": reason });
            tx.execute(
                "INSERT INTO price_change_batches(batch_id, operation_id, item_count, summary_json, approved_by, user_id, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![batch_id, req.operation_id, applied.len() as i64, summary.to_string(), approved_by, s.user_id, now],
            )?;
            audit::record(tx, &actor, "pricing.applied", "pricing", Some(&batch_id), None, Some(&json!({ "summary": summary, "items": applied })))?;
            let result = json!({ "batch_id": batch_id, "applied": applied.len(), "skipped_unchanged": rows.len() - applied.len(), "rows": applied });
            idempotency::complete(tx, &req.operation_id, "pricing.apply", Some(&s.user_id), Some(&s.device_id), &hash, Some(&batch_id), &result)?;
            Ok(result)
        })
    }
}

fn floor_policy_id(c: &Connection, policies: &[Policy], pid: &str, branch: &str, price_type: &str) -> AppResult<Option<String>> {
    if policies.is_empty() {
        return Ok(None);
    }
    let cat: Option<String> = c.query_row("SELECT category_id FROM products WHERE product_id=?1", [pid], |r| r.get(0))?;
    let sup: Option<String> = c
        .query_row("SELECT supplier_id FROM supplier_products WHERE product_id=?1 AND preferred=1 AND active=1", [pid], |r| r.get(0))
        .optional()?;
    let channel = if price_type == "retail" { "pos" } else { price_type };
    Ok(resolve(policies, &Subject { category_id: cat.as_deref(), supplier_id: sup.as_deref(), branch_id: branch, channel })
        .policy
        .map(|p| p.policy_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_and_margin_are_different_things() {
        // Cost 1.000, no VAT: 25% markup → 1.250 (margin 20%); 25% margin → 1.333.
        assert_eq!(target_price(1_000, Some(2_500), None, 0, false), Some(1_250));
        assert_eq!(target_price(1_000, None, Some(2_500), 0, false), Some(1_333));
        assert_eq!(margin_bp(1_250, 1_000, 0, false), Some(2_000));
        assert_eq!(markup_bp(1_250, 1_000, 0, false), Some(2_500));
        // VAT 10% included: the margin is on the net price.
        assert_eq!(target_price(1_000, None, Some(2_500), 1_000, true), Some(1_467));
        assert_eq!(margin_bp(1_467, 1_000, 1_000, true), Some(2_501));
        assert_eq!(margin_bp(1_100, 1_000, 1_000, true), Some(0));
    }

    #[test]
    fn rounding_steps_and_endings() {
        assert_eq!(round_price(1_333, 5, None, 0), 1_335);
        assert_eq!(round_price(1_332, 5, None, 0), 1_330);
        assert_eq!(round_price(1_333, 25, None, 0), 1_325);
        assert_eq!(round_price(1_333, 100, None, 0), 1_300);
        assert_eq!(round_price(1_333, 50, Some(950), 0), 950);
        assert_eq!(round_price(1_700, 50, Some(950), 0), 1_950);
        // Never below the floor: 1.300 would breach a 1.320 floor.
        assert_eq!(round_price(1_333, 100, None, 1_320), 1_400);
        assert_eq!(round_price(1_333, 50, Some(950), 1_320), 1_950);
    }

    #[test]
    fn rounding_never_breaches_the_minimum_margin() {
        // Invariant 8, exhaustively over a grid of costs, VAT, steps, endings and minimums.
        for cost in (1..2_000).step_by(37) {
            for (rate, incl) in [(0, false), (1_000, true), (1_000, false), (500, true)] {
                for &step in &STEPS {
                    for ending in [None, Some(0), Some(500), Some(950), Some(995)] {
                        for min in [0, 500, 1_500, 3_000, 9_000] {
                            for margin in [None, Some(min), Some(min + 1_000)] {
                                let p = Policy {
                                    policy_id: "p".into(),
                                    name: "p".into(),
                                    scope: "global".into(),
                                    scope_id: None,
                                    markup_bp: None,
                                    target_margin_bp: margin.filter(|m| *m < 9_500),
                                    min_margin_bp: Some(min),
                                    rounding_step_minor: step,
                                    ending_minor: ending,
                                    priority: 0,
                                    cost_basis: "average".into(),
                                    active: true,
                                    version: 1,
                                    updated_at: None,
                                };
                                let r = Resolved { policy: Some(p), floor_bp: Some(min), ambiguous_with: vec![] };
                                if let Some(price) = recommend(&r, cost, Some(1), rate, incl) {
                                    assert!(
                                        meets_floor(price, cost, min, rate, incl),
                                        "cost {cost} rate {rate} step {step} ending {ending:?} min {min}: {price}"
                                    );
                                    assert!(margin_bp(price, cost, rate, incl).unwrap() >= min);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_most_specific_policy_wins_and_ties_are_surfaced() {
        let mk = |id: &str, scope: &str, sid: Option<&str>, prio: i64, min: Option<i64>| Policy {
            policy_id: id.into(),
            name: id.into(),
            scope: scope.into(),
            scope_id: sid.map(String::from),
            markup_bp: Some(2_000),
            target_margin_bp: None,
            min_margin_bp: min,
            rounding_step_minor: 5,
            ending_minor: None,
            priority: prio,
            cost_basis: "average".into(),
            active: true,
            version: 1,
            updated_at: None,
        };
        let ps = vec![
            mk("g", "global", None, 0, Some(1_000)),
            mk("cat", "category", Some("dairy"), 0, Some(500)),
            mk("sup", "supplier", Some("s1"), 0, None),
            mk("ch", "channel", Some("whatsapp"), 0, None),
        ];
        let s = Subject { category_id: Some("dairy"), supplier_id: Some("s1"), branch_id: "b", channel: "pos" };
        let r = resolve(&ps, &s);
        assert_eq!(r.policy.unwrap().policy_id, "sup");
        assert_eq!(r.floor_bp, Some(1_000), "the highest minimum of all matching policies");
        let r = resolve(&ps, &Subject { channel: "whatsapp", ..s.clone() });
        assert_eq!(r.policy.unwrap().policy_id, "ch");
        let mut tied = ps.clone();
        tied.push(mk("cat2", "category", Some("dairy"), 0, None));
        let r = resolve(&tied, &Subject { supplier_id: None, ..s.clone() });
        assert_eq!(r.policy.unwrap().policy_id, "cat", "settled by name");
        assert_eq!(r.ambiguous_with, vec!["cat2".to_string()]);
    }
}
