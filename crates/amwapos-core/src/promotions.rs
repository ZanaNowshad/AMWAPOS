//! Promotions and coupons: the promotional step of the one pricing pipeline
//! (docs/PROMOTIONS_AND_BUNDLES.md).
//!
//! Order of the pipeline (every figure is computed here and in `pricing`):
//! 1. the resolved price of each line (Wave 5: branch/channel price lists);
//! 2. **item promotions** — percent, amount, fixed price, quantity deals,
//!    Buy-X-Get-Y: at most one per line;
//! 3. **one basket promotion** on the eligible lines' value after item
//!    promotions;
//! 4. **one coupon promotion** (a promotion unlocked by the sale's coupon);
//! 5. the manual line discount, the manual cart discount and loyalty
//!    (`pricing::price_cart`, unchanged), then VAT per line.
//!
//! Lines that never take an automatic offer: custom items, price overrides,
//! lines with a manual discount, and price-embedded scale labels (the price
//! printed on the pack is the price). Weight labels take offers like any line.
//!
//! Conflicts: candidates are ordered by priority (high first), then by the
//! benefit they would give on their own (larger first), then by promotion id.
//! A line takes at most one item promotion. A later layer reaches a line that
//! already has a promotion only when every promotion involved is stackable.
//! The result never depends on line order: lines are put in a canonical
//! order (product, price, quantity, line id) before any grouping or split.
//!
//! Money is integer fils; rates are basis points. No promotion can take more
//! than a line is worth.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::TimeZone;
use rusqlite::{params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::AppResult;
use crate::money::{allocate, percent_of};

pub const KINDS: [&str; 6] = ["percent", "amount", "fixed_price", "quantity", "bxgy", "basket"];

/// A line as the promotion engine sees it.
#[derive(Debug, Clone)]
pub struct PLine {
    pub line_id: String,
    pub product_id: Option<String>,
    pub category_id: Option<String>,
    pub unit_price_minor: i64,
    pub qty_milli: i64,
    pub allow_decimal: bool,
    /// Why this line never takes an automatic offer (None: it can).
    pub excluded: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Promotion {
    #[serde(default)]
    pub promotion_id: String,
    pub name: String,
    pub name_ar: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "draft")]
    pub status: String,
    pub kind: String,
    #[serde(default = "items")]
    pub target: String,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    pub branches: Option<Vec<String>>,
    pub channels: Option<Vec<String>>,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub stackable: bool,
    #[serde(default)]
    pub requires_coupon: bool,
    pub percent_bp: Option<i64>,
    pub amount_minor: Option<i64>,
    pub price_minor: Option<i64>,
    pub buy_qty: Option<i64>,
    pub get_qty: Option<i64>,
    pub max_uses: Option<i64>,
    pub threshold_minor: Option<i64>,
    #[serde(default)]
    pub buy_products: Vec<String>,
    #[serde(default)]
    pub buy_categories: Vec<String>,
    #[serde(default)]
    pub get_products: Vec<String>,
    #[serde(default)]
    pub get_categories: Vec<String>,
    #[serde(default)]
    pub version: i64,
}

fn draft() -> String {
    "draft".into()
}

fn items() -> String {
    "items".into()
}

impl Promotion {
    pub fn layer(&self) -> &'static str {
        if self.requires_coupon {
            "coupon"
        } else if self.kind == "basket" {
            "basket"
        } else {
            "item"
        }
    }
}

/// Where and when the cart is priced.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub branch_id: String,
    /// The sale's channel (pos, whatsapp, phone, web, other).
    pub channel: String,
    /// The sale's time in the store's timezone, 'YYYY-MM-DDTHH:MM'.
    pub local_now: String,
    pub coupon: Option<String>,
    /// The main computer (hub or a standalone store): the only place a
    /// limited coupon can be checked against every redemption.
    pub is_main: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Applied {
    pub promotion_id: String,
    pub name: String,
    pub name_ar: Option<String>,
    pub layer: String,
    pub coupon_code: Option<String>,
    pub amount_minor: i64,
    /// (line index, amount) — what it took off each line.
    #[serde(skip)]
    pub lines: Vec<(usize, i64)>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CouponState {
    pub code: String,
    /// applied | invalid | expired | not_started | already_used | needs_main |
    /// wrong_channel | wrong_branch | not_eligible | inactive
    pub status: String,
    pub message: String,
    pub saved_minor: i64,
    #[serde(skip)]
    pub coupon_id: Option<String>,
    #[serde(skip)]
    pub promotion_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Explain {
    pub promotion_id: String,
    pub name: String,
    /// applied | not_started | ended | paused | draft | archived | wrong_channel |
    /// wrong_branch | needs_coupon | not_enough | threshold | outranked | excluded_lines
    pub reason: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Outcome {
    /// Promotional discount per line (same order as the input lines).
    #[serde(skip)]
    pub per_line: Vec<i64>,
    pub applied: Vec<Applied>,
    pub coupon: Option<CouponState>,
    pub explain: Vec<Explain>,
}

impl Outcome {
    pub fn total(&self) -> i64 {
        self.per_line.iter().sum()
    }
}

// ------------------------------------------------------------------ normalization and time

/// A coupon code as stored and compared: trimmed, inner spaces removed,
/// upper case, Arabic digits as digits. Hyphens and other characters are
/// kept, so "SAVE-10" and "SAVE10" stay different codes.
pub fn normalize_code(raw: &str) -> String {
    crate::barcodes::ascii_digits(raw).chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase()
}

/// 'YYYY-MM-DDTHH:MM' for a UTC instant in the store's timezone.
pub fn local_minute(t: chrono::DateTime<chrono::Utc>, tz: &str) -> String {
    match crate::time::tz(tz) {
        Ok(z) => z.from_utc_datetime(&t.naive_utc()).format("%Y-%m-%dT%H:%M").to_string(),
        Err(_) => t.format("%Y-%m-%dT%H:%M").to_string(),
    }
}

/// Status + schedule + scope check, with the merchant-facing reason when it
/// does not apply.
pub fn eligible_now(p: &Promotion, ctx: &Ctx) -> Option<(&'static str, String)> {
    match p.status.as_str() {
        "active" => {}
        "paused" => return Some(("paused", "Paused.".into())),
        "draft" => return Some(("draft", "Not switched on yet (draft).".into())),
        "ended" => return Some(("ended", "Ended.".into())),
        _ => return Some(("archived", "Archived.".into())),
    }
    if let Some(s) = p.starts_at.as_deref().filter(|s| *s > ctx.local_now.as_str()) {
        return Some(("not_started", format!("Not active yet (starts {}).", s.replace('T', " "))));
    }
    if p.ends_at.as_deref().is_some_and(|e| e <= ctx.local_now.as_str()) {
        return Some(("ended", "Ended.".into()));
    }
    if let Some(b) = &p.branches {
        if !b.iter().any(|x| x == &ctx.branch_id) {
            return Some(("wrong_branch", "Not for this branch.".into()));
        }
    }
    if let Some(ch) = &p.channels {
        if !ch.iter().any(|x| x == &ctx.channel) {
            let names: Vec<&str> = ch.iter().map(|c| channel_name(c)).collect();
            return Some(("wrong_channel", format!("Only for {} sales.", names.join(", "))));
        }
    }
    None
}

fn channel_name(c: &str) -> &'static str {
    match c {
        "pos" => "till",
        "whatsapp" => "WhatsApp",
        "phone" => "phone",
        "web" => "web",
        _ => "other",
    }
}

// ------------------------------------------------------------------ loading

const COLS: &str = "promotion_id, name, name_ar, status, kind, target, starts_at, ends_at, branches_json, channels_json, priority,
    stackable, requires_coupon, percent_bp, amount_minor, price_minor, buy_qty, get_qty, max_uses, threshold_minor, description, version";

fn map(r: &rusqlite::Row) -> rusqlite::Result<Promotion> {
    let list = |s: Option<String>| s.and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok()).filter(|v| !v.is_empty());
    Ok(Promotion {
        promotion_id: r.get(0)?,
        name: r.get(1)?,
        name_ar: r.get(2)?,
        status: r.get(3)?,
        kind: r.get(4)?,
        target: r.get(5)?,
        starts_at: r.get(6)?,
        ends_at: r.get(7)?,
        branches: list(r.get(8)?),
        channels: list(r.get(9)?),
        priority: r.get(10)?,
        stackable: r.get::<_, i64>(11)? == 1,
        requires_coupon: r.get::<_, i64>(12)? == 1,
        percent_bp: r.get(13)?,
        amount_minor: r.get(14)?,
        price_minor: r.get(15)?,
        buy_qty: r.get(16)?,
        get_qty: r.get(17)?,
        max_uses: r.get(18)?,
        threshold_minor: r.get(19)?,
        description: r.get(20)?,
        version: r.get(21)?,
        buy_products: vec![],
        buy_categories: vec![],
        get_products: vec![],
        get_categories: vec![],
    })
}

fn attach_targets(c: &Connection, ps: &mut [Promotion]) -> AppResult<()> {
    if ps.is_empty() {
        return Ok(());
    }
    let idx: HashMap<String, usize> = ps.iter().enumerate().map(|(i, p)| (p.promotion_id.clone(), i)).collect();
    let ph = vec!["?"; ps.len()].join(",");
    let mut st = c.prepare(&format!(
        "SELECT promotion_id, role, ref_kind, ref_id FROM promotion_targets WHERE promotion_id IN ({ph}) ORDER BY promotion_id, role, ref_kind, ref_id"
    ))?;
    let ids: Vec<String> = ps.iter().map(|p| p.promotion_id.clone()).collect();
    let rows = st.query_map(params_from_iter(ids.iter()), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
    })?;
    for row in rows {
        let (pid, role, kind, rid) = row?;
        if let Some(&i) = idx.get(&pid) {
            let p = &mut ps[i];
            match (role.as_str(), kind.as_str()) {
                ("buy", "product") => p.buy_products.push(rid),
                ("buy", _) => p.buy_categories.push(rid),
                ("get", "product") => p.get_products.push(rid),
                _ => p.get_categories.push(rid),
            }
        }
    }
    Ok(())
}

pub fn load(c: &Connection, promotion_id: &str) -> AppResult<Option<Promotion>> {
    let p = c.query_row(&format!("SELECT {COLS} FROM promotions WHERE promotion_id=?1"), [promotion_id], map).optional()?;
    match p {
        Some(p) => {
            let mut v = vec![p];
            attach_targets(c, &mut v)?;
            Ok(v.pop())
        }
        None => Ok(None),
    }
}

/// The promotions that could touch these lines: by an index on their
/// products and categories, plus the 'whole basket' ones. Never every
/// promotion against every line. Only non-archived ones are read.
pub fn candidates(c: &Connection, lines: &[PLine], with_inactive: bool) -> AppResult<Vec<Promotion>> {
    let mut refs: Vec<String> = lines.iter().flat_map(|l| [l.product_id.clone(), l.category_id.clone()]).flatten().collect();
    refs.sort();
    refs.dedup();
    let status = if with_inactive { "status <> 'archived'" } else { "status = 'active'" };
    let mut ids: Vec<String> = vec![];
    {
        let mut st =
            c.prepare_cached(&format!("SELECT promotion_id FROM promotions WHERE {status} AND target='all' AND requires_coupon=0"))?;
        for r in st.query_map([], |r| r.get::<_, String>(0))? {
            ids.push(r?);
        }
    }
    for chunk in refs.chunks(400) {
        let ph = vec!["?"; chunk.len()].join(",");
        let mut st = c.prepare(&format!(
            "SELECT DISTINCT t.promotion_id FROM promotion_targets t JOIN promotions p ON p.promotion_id=t.promotion_id
             WHERE t.ref_id IN ({ph}) AND p.{status}"
        ))?;
        for r in st.query_map(params_from_iter(chunk.iter()), |r| r.get::<_, String>(0))? {
            ids.push(r?);
        }
    }
    ids.sort();
    ids.dedup();
    let mut out = vec![];
    for chunk in ids.chunks(400) {
        let ph = vec!["?"; chunk.len()].join(",");
        let mut st = c.prepare(&format!("SELECT {COLS} FROM promotions WHERE promotion_id IN ({ph}) ORDER BY promotion_id"))?;
        for p in st.query_map(params_from_iter(chunk.iter()), map)? {
            out.push(p?);
        }
    }
    attach_targets(c, &mut out)?;
    Ok(out)
}

// ------------------------------------------------------------------ evaluation

/// Lines in canonical order (independent of the order they were scanned).
fn canonical(lines: &[PLine]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&lines[a], &lines[b]);
        x.product_id
            .cmp(&y.product_id)
            .then(x.unit_price_minor.cmp(&y.unit_price_minor))
            .then(x.qty_milli.cmp(&y.qty_milli))
            .then(x.line_id.cmp(&y.line_id))
    });
    order
}

/// Whether a line is a qualifying ('buy') or reward ('get') item. Without a
/// separate reward set, the rewards are drawn from the qualifying set.
fn matches(p: &Promotion, l: &PLine, role: &str) -> bool {
    let (prods, cats) = if role == "get" && (!p.get_products.is_empty() || !p.get_categories.is_empty()) {
        (&p.get_products, &p.get_categories)
    } else if p.target == "all" {
        return true;
    } else {
        (&p.buy_products, &p.buy_categories)
    };
    l.product_id.as_ref().is_some_and(|x| prods.contains(x)) || l.category_id.as_ref().is_some_and(|x| cats.contains(x))
}

/// Bound on the whole units one quantity or Buy-X-Get-Y offer looks at in a
/// sale, so evaluation stays bounded for any basket. Units beyond it are
/// charged at their normal price (no retail basket comes near it).
pub const MAX_OFFER_UNITS: usize = 10_000;

/// Whole units of a line (quantity offers count pieces, not weight).
fn units(l: &PLine) -> i64 {
    if l.allow_decimal || l.qty_milli % 1000 != 0 {
        0
    } else {
        l.qty_milli / 1000
    }
}

/// What `p` would take off each line, given the value left on each line and
/// which lines it may touch. Returns (per-line amounts, touched lines, reason
/// when nothing applies).
fn evaluate(p: &Promotion, lines: &[PLine], value: &[i64], allowed: &[bool]) -> (Vec<i64>, Vec<usize>, Option<&'static str>) {
    let n = lines.len();
    let mut out = vec![0i64; n];
    let mut touched = vec![];
    let order = canonical(lines);
    let mut pos = vec![0usize; n];
    for (k, &i) in order.iter().enumerate() {
        pos[i] = k;
    }
    let elig = |i: usize, role: &str| allowed[i] && value[i] > 0 && matches(p, &lines[i], role);
    match p.kind.as_str() {
        "percent" | "amount" | "fixed_price" => {
            for &i in &order {
                if !elig(i, "buy") {
                    continue;
                }
                let l = &lines[i];
                let d = match p.kind.as_str() {
                    "percent" => percent_of(value[i], p.percent_bp.unwrap_or(0)).unwrap_or(0),
                    "amount" => crate::money::extend(p.amount_minor.unwrap_or(0), l.qty_milli).unwrap_or(0),
                    _ => value[i] - crate::money::extend(p.price_minor.unwrap_or(0), l.qty_milli).unwrap_or(value[i]),
                };
                let d = d.clamp(0, value[i]);
                if d > 0 {
                    out[i] = d;
                    touched.push(i);
                }
            }
            if touched.is_empty() {
                return (out, touched, Some("not_enough"));
            }
        }
        "quantity" => {
            // Every whole unit of the eligible lines, most valuable first; groups of N.
            let need = p.buy_qty.unwrap_or(1).max(1);
            let mut pool: Vec<(i64, usize)> = vec![];
            for &i in &order {
                if elig(i, "buy") {
                    let u = units(&lines[i]);
                    let unit_value = if u > 0 { value[i] / u } else { 0 };
                    for _ in 0..u {
                        if pool.len() >= MAX_OFFER_UNITS {
                            break;
                        }
                        pool.push((unit_value, i));
                    }
                }
            }
            pool.sort_by(|a, b| b.0.cmp(&a.0).then(pos[a.1].cmp(&pos[b.1])));
            let max_groups = p.max_uses.unwrap_or(i64::MAX);
            let mut groups = 0;
            for g in pool.chunks(need as usize) {
                if g.len() < need as usize || groups >= max_groups {
                    break;
                }
                let normal: i64 = g.iter().map(|x| x.0).sum();
                let disc = (normal - p.price_minor.unwrap_or(normal)).max(0);
                if disc == 0 {
                    continue;
                }
                let weights: Vec<i64> = g.iter().map(|x| x.0).collect();
                for (k, a) in allocate(disc, &weights).into_iter().enumerate() {
                    out[g[k].1] += a;
                }
                for x in g {
                    touched.push(x.1);
                }
                groups += 1;
            }
            if groups == 0 {
                return (out, vec![], Some("not_enough"));
            }
        }
        "bxgy" => {
            let x = p.buy_qty.unwrap_or(1).max(1) as usize;
            let y = p.get_qty.unwrap_or(1).max(1) as usize;
            let bp = p.percent_bp.unwrap_or(10_000);
            // Units: (value, line, is_buy, is_get)
            let mut pool: Vec<(i64, usize, bool, bool)> = vec![];
            for &i in &order {
                let (b, g) = (elig(i, "buy"), elig(i, "get"));
                if !b && !g {
                    continue;
                }
                let u = units(&lines[i]);
                let uv = if u > 0 { value[i] / u } else { 0 };
                for _ in 0..u {
                    if pool.len() >= MAX_OFFER_UNITS {
                        break;
                    }
                    pool.push((uv, i, b, g));
                }
            }
            // Units from cheapest to dearest (pre-offer unit value, then the
            // canonical line order): rewards are taken from the cheap end,
            // qualifiers from the dear end. Two cursors: linear in units.
            let mut by_value: Vec<usize> = (0..pool.len()).collect();
            by_value.sort_by(|&a, &b| pool[a].0.cmp(&pool[b].0).then(pos[pool[a].1].cmp(&pos[pool[b].1])).then(a.cmp(&b)));
            let mut used = vec![false; pool.len()];
            let max_uses = p.max_uses.unwrap_or(i64::MAX);
            let mut uses = 0;
            let (mut lo, mut hi) = (0usize, by_value.len());
            'deal: while uses < max_uses {
                let mut rewards = Vec::with_capacity(y);
                let mut r = lo;
                while rewards.len() < y && r < by_value.len() {
                    let k = by_value[r];
                    if !used[k] && pool[k].3 {
                        rewards.push(k);
                    }
                    r += 1;
                }
                if rewards.len() < y {
                    break;
                }
                let mut quals = Vec::with_capacity(x);
                let mut q = hi;
                while quals.len() < x && q > 0 {
                    q -= 1;
                    let k = by_value[q];
                    if !used[k] && pool[k].2 && !rewards.contains(&k) {
                        quals.push(k);
                    }
                }
                if quals.len() < x {
                    break 'deal;
                }
                for &k in rewards.iter().chain(quals.iter()) {
                    used[k] = true;
                    touched.push(pool[k].1);
                }
                for &k in &rewards {
                    out[pool[k].1] += percent_of(pool[k].0, bp).unwrap_or(0);
                }
                // Everything before the last reward is used or not a reward
                // unit; everything after the last qualifier likewise.
                lo = r;
                hi = q;
                uses += 1;
            }
            if uses == 0 {
                return (out, vec![], Some("not_enough"));
            }
        }
        "basket" => {
            let idx: Vec<usize> = order.iter().copied().filter(|&i| elig(i, "buy")).collect();
            let base: i64 = idx.iter().map(|&i| value[i]).sum();
            if idx.is_empty() {
                return (out, vec![], Some("excluded_lines"));
            }
            if base < p.threshold_minor.unwrap_or(0) {
                return (out, vec![], Some("threshold"));
            }
            let disc = match (p.percent_bp, p.amount_minor) {
                (Some(bp), _) => percent_of(base, bp).unwrap_or(0),
                (None, Some(a)) => a.min(base),
                _ => 0,
            };
            let weights: Vec<i64> = idx.iter().map(|&i| value[i]).collect();
            for (k, a) in allocate(disc, &weights).into_iter().enumerate() {
                out[idx[k]] += a;
            }
            touched = idx;
        }
        _ => {}
    }
    for i in 0..n {
        out[i] = out[i].clamp(0, value[i]);
    }
    touched.sort();
    touched.dedup();
    (out, touched, None)
}

fn reason_message(code: &str, p: &Promotion) -> String {
    match code {
        "not_enough" => match p.kind.as_str() {
            "quantity" => format!("Requires {} items.", p.buy_qty.unwrap_or(1)),
            "bxgy" => format!("Requires {} + {} items.", p.buy_qty.unwrap_or(1), p.get_qty.unwrap_or(1)),
            _ => "No item in the sale qualifies.".into(),
        },
        "threshold" => format!("Requires a basket of at least {}.", crate::money::format_decimal(p.threshold_minor.unwrap_or(0), 3)),
        "outranked" => "Another higher-priority offer was used.".into(),
        "excluded_lines" => "The qualifying items already have an offer or a manual discount.".into(),
        "needs_coupon" => "Coupon is required.".into(),
        _ => String::new(),
    }
}

/// Run the promotional step for a cart. Deterministic: the same lines (in
/// any order), configuration, channel, time and coupon give the same result.
pub fn run(c: &Connection, ctx: &Ctx, lines: &[PLine]) -> AppResult<Outcome> {
    let n = lines.len();
    let mut out = Outcome { per_line: vec![0; n], ..Default::default() };
    let mut value: Vec<i64> = lines.iter().map(|l| crate::money::extend(l.unit_price_minor, l.qty_milli).unwrap_or(0)).collect();
    let base_ok: Vec<bool> = lines.iter().map(|l| l.excluded.is_none() && l.product_id.is_some()).collect();
    // Per line: the promotions already on it, and whether they all stack.
    let mut on_line: Vec<Vec<bool>> = vec![vec![]; n];
    let all = candidates(c, lines, false)?;
    let (mut items, mut baskets): (Vec<&Promotion>, Vec<&Promotion>) = (vec![], vec![]);
    for p in &all {
        if let Some((code, msg)) = eligible_now(p, ctx) {
            out.explain.push(Explain { promotion_id: p.promotion_id.clone(), name: p.name.clone(), reason: code.into(), message: msg });
            continue;
        }
        if p.requires_coupon {
            // Unlocked only by its coupon (the coupon layer below).
            out.explain.push(Explain {
                promotion_id: p.promotion_id.clone(),
                name: p.name.clone(),
                reason: "needs_coupon".into(),
                message: reason_message("needs_coupon", p),
            });
            continue;
        }
        if p.kind == "basket" {
            baskets.push(p);
        } else {
            items.push(p);
        }
    }
    // Item layer: at most one promotion per line. Ranked by priority, then
    // the benefit each would give on its own, then id.
    let mut scored: Vec<(i64, &Promotion)> =
        items.iter().map(|p| (evaluate(p, lines, &value, &base_ok).0.iter().sum::<i64>(), *p)).collect();
    scored.sort_by(|a, b| b.1.priority.cmp(&a.1.priority).then(b.0.cmp(&a.0)).then(a.1.promotion_id.cmp(&b.1.promotion_id)));
    let mut claimed = vec![false; n];
    for (alone, p) in scored {
        let allowed: Vec<bool> = (0..n).map(|i| base_ok[i] && !claimed[i]).collect();
        let (amounts, touched, why) = evaluate(p, lines, &value, &allowed);
        let total: i64 = amounts.iter().sum();
        if total <= 0 {
            let code = if alone > 0 { "outranked" } else { why.unwrap_or("not_enough") };
            out.explain.push(not_applied(p, code, lines));
            continue;
        }
        for &i in &touched {
            claimed[i] = true;
            on_line[i].push(p.stackable);
        }
        apply(&mut out, &mut value, p, None, amounts);
    }
    // Basket layer: one promotion.
    let stack_ok = |on: &[bool], me: bool| on.is_empty() || (me && on.iter().all(|s| *s));
    let mut basket_done = false;
    let scored: Vec<(i64, &Promotion)> = {
        let mut v: Vec<(i64, &Promotion)> = baskets
            .iter()
            .map(|p| {
                let allowed: Vec<bool> = (0..n).map(|i| base_ok[i] && stack_ok(&on_line[i], p.stackable)).collect();
                (evaluate(p, lines, &value, &allowed).0.iter().sum::<i64>(), *p)
            })
            .collect();
        v.sort_by(|a, b| b.1.priority.cmp(&a.1.priority).then(b.0.cmp(&a.0)).then(a.1.promotion_id.cmp(&b.1.promotion_id)));
        v
    };
    for (_, p) in scored {
        let allowed: Vec<bool> = (0..n).map(|i| base_ok[i] && stack_ok(&on_line[i], p.stackable)).collect();
        let (amounts, touched, why) = evaluate(p, lines, &value, &allowed);
        let total: i64 = amounts.iter().sum();
        if basket_done || total <= 0 {
            let code = if basket_done { "outranked" } else { why.unwrap_or("not_enough") };
            out.explain.push(not_applied(p, code, lines));
            continue;
        }
        for &i in &touched {
            on_line[i].push(p.stackable);
        }
        apply(&mut out, &mut value, p, None, amounts);
        basket_done = true;
    }
    // Coupon layer: the sale's one coupon.
    if let Some(code) = ctx.coupon.as_deref().filter(|c| !c.is_empty()) {
        let state = run_coupon(c, ctx, lines, &base_ok, &on_line, &mut value, &mut out, code)?;
        if state.status == "applied" {
            out.explain.retain(|x| Some(&x.promotion_id) != state.promotion_id.as_ref());
        }
        out.coupon = Some(state);
    }
    Ok(out)
}

/// Why `p` gave nothing. When nothing qualified only because the matching
/// items cannot take offers, that is the reason given.
fn not_applied(p: &Promotion, code: &str, lines: &[PLine]) -> Explain {
    let excluded = lines.iter().find(|l| l.excluded.is_some() && l.product_id.is_some() && matches(p, l, "buy"));
    let (reason, message) = match (code, excluded) {
        ("not_enough" | "excluded_lines", Some(l)) => {
            let why = l.excluded.unwrap_or_default();
            let msg = match why {
                "scale_price" => "This item came from a fixed-price scale label.",
                "manual_discount" => "This item already has a manual discount.",
                "price_override" => "This item's price was changed by hand.",
                "bundle_component" => "Items inside a bundle take offers on the bundle only.",
                _ => "This item cannot take offers.",
            };
            (format!("excluded_{why}"), msg.to_string())
        }
        _ => (code.to_string(), reason_message(code, p)),
    };
    Explain { promotion_id: p.promotion_id.clone(), name: p.name.clone(), reason, message }
}

fn apply(out: &mut Outcome, value: &mut [i64], p: &Promotion, coupon: Option<&str>, amounts: Vec<i64>) {
    let mut lines = vec![];
    for (i, a) in amounts.into_iter().enumerate() {
        if a > 0 {
            value[i] -= a;
            out.per_line[i] += a;
            lines.push((i, a));
        }
    }
    out.applied.push(Applied {
        promotion_id: p.promotion_id.clone(),
        name: p.name.clone(),
        name_ar: p.name_ar.clone(),
        layer: p.layer().into(),
        coupon_code: coupon.map(String::from),
        amount_minor: lines.iter().map(|x| x.1).sum(),
        lines,
    });
}

#[allow(clippy::too_many_arguments)]
fn run_coupon(
    c: &Connection,
    ctx: &Ctx,
    lines: &[PLine],
    base_ok: &[bool],
    on_line: &[Vec<bool>],
    value: &mut [i64],
    out: &mut Outcome,
    code: &str,
) -> AppResult<CouponState> {
    let norm = normalize_code(code);
    let st = |status: &str, message: &str| CouponState {
        code: norm.clone(),
        status: status.into(),
        message: message.into(),
        saved_minor: 0,
        coupon_id: None,
        promotion_id: None,
    };
    let row: Option<(String, String, String, Option<i64>, i64)> = c
        .query_row("SELECT coupon_id, promotion_id, kind, max_redemptions, active FROM coupons WHERE code_norm=?1", [&norm], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .optional()?;
    let Some((cid, pid, kind, max, active)) = row else { return Ok(st("invalid", "This coupon code is not recognised.")) };
    if active != 1 {
        return Ok(st("inactive", "This coupon has been switched off."));
    }
    let Some(p) = load(c, &pid)? else { return Ok(st("invalid", "This coupon code is not recognised.")) };
    if let Some((reason, msg)) = eligible_now(&p, ctx) {
        let (status, msg) = match reason {
            "not_started" => ("not_started", msg),
            "ended" => ("expired", "This coupon has expired.".to_string()),
            "wrong_channel" => ("wrong_channel", msg),
            "wrong_branch" => ("wrong_branch", msg),
            _ => ("inactive", "This coupon's offer is not running.".to_string()),
        };
        return Ok(st(status, &msg));
    }
    if kind == "limited" {
        if !ctx.is_main {
            return Ok(st("needs_main", "This coupon needs the main computer to verify it."));
        }
        let used: i64 = c.query_row("SELECT COUNT(*) FROM coupon_redemptions WHERE coupon_id=?1", [&cid], |r| r.get(0))?;
        if used >= max.unwrap_or(1) {
            return Ok(st("already_used", "This coupon has already been used."));
        }
    }
    let n = lines.len();
    let allowed: Vec<bool> =
        (0..n).map(|i| base_ok[i] && (on_line[i].is_empty() || (p.stackable && on_line[i].iter().all(|s| *s)))).collect();
    let (amounts, _, why) = evaluate(&p, lines, value, &allowed);
    let total: i64 = amounts.iter().sum();
    if total <= 0 {
        let msg = match why {
            Some("threshold") => reason_message("threshold", &p),
            Some("excluded_lines") => "The items in this sale already have an offer.".into(),
            _ => "Not eligible for this basket.".into(),
        };
        let mut s = st("not_eligible", &msg);
        s.coupon_id = Some(cid);
        s.promotion_id = Some(pid);
        return Ok(s);
    }
    apply(out, value, &p, Some(&norm), amounts);
    Ok(CouponState {
        code: norm,
        status: "applied".into(),
        message: format!("Applied: {}", p.name),
        saved_minor: total,
        coupon_id: Some(cid),
        promotion_id: Some(pid),
    })
}

/// Freeze what the promotional step did into the sale being committed:
/// per line and promotion the amount taken, and the coupon redemption (only
/// here, at commit; never for a preview or a held cart). `item_ids` are the
/// sale lines in the same order as the priced cart lines.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_sale(
    tx: &Connection,
    s: &crate::auth::Session,
    sale_id: &str,
    operation_id: &str,
    customer_id: Option<&str>,
    item_ids: &[Vec<(String, i64)>],
    out: &Outcome,
    now: &str,
) -> AppResult<()> {
    for a in &out.applied {
        for &(i, amount) in &a.lines {
            let Some(items) = item_ids.get(i) else { continue };
            // A bundle line's offer is split over its component lines.
            let weights: Vec<i64> = items.iter().map(|x| x.1).collect();
            for ((item, _), part) in items.iter().zip(allocate(amount, &weights)) {
                tx.execute(
                    "INSERT INTO sale_item_promotions(sale_item_id, promotion_id, sale_id, layer, promotion_name, coupon_code, amount_minor)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    rusqlite::params![item, a.promotion_id, sale_id, a.layer, a.name, a.coupon_code, part],
                )?;
            }
        }
    }
    if let Some(cs) = out.coupon.as_ref().filter(|c| c.status == "applied") {
        if let (Some(cid), Some(pid)) = (&cs.coupon_id, &cs.promotion_id) {
            tx.execute(
                "INSERT INTO coupon_redemptions(redemption_id, coupon_id, promotion_id, code, sale_id, branch_id, device_id, user_id,
                    customer_id, amount_minor, operation_id, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                rusqlite::params![
                    crate::ids::new_id(),
                    cid,
                    pid,
                    cs.code,
                    sale_id,
                    s.branch_id,
                    s.device_id,
                    s.user_id,
                    customer_id,
                    cs.saved_minor,
                    operation_id,
                    now
                ],
            )?;
        }
    }
    Ok(())
}

/// Validate a promotion before it is saved.
pub fn validate(p: &Promotion) -> Result<(), String> {
    let name = p.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("Give the offer a name (at most 80 characters).".into());
    }
    if !KINDS.contains(&p.kind.as_str()) {
        return Err("Choose the type of offer.".into());
    }
    if !matches!(p.target.as_str(), "items" | "all") {
        return Err("Choose which products the offer is for.".into());
    }
    if p.target == "items" && p.buy_products.is_empty() && p.buy_categories.is_empty() {
        return Err("Choose at least one product or category.".into());
    }
    if let (Some(s), Some(e)) = (&p.starts_at, &p.ends_at) {
        if e <= s {
            return Err("The end must be after the start.".into());
        }
    }
    for t in [&p.starts_at, &p.ends_at].into_iter().flatten() {
        if chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M").is_err() {
            return Err("Enter dates like 2026-12-31 18:00.".into());
        }
    }
    let pct = |v: Option<i64>| v.is_some_and(|b| (1..=10_000).contains(&b));
    let ok = match p.kind.as_str() {
        "percent" => pct(p.percent_bp),
        "amount" => p.amount_minor.is_some_and(|a| a > 0),
        "fixed_price" => p.price_minor.is_some_and(|a| a >= 0),
        "quantity" => p.buy_qty.is_some_and(|q| q >= 2) && p.price_minor.is_some_and(|a| a >= 0),
        "bxgy" => p.buy_qty.is_some_and(|q| q >= 1) && p.get_qty.is_some_and(|q| q >= 1) && (p.percent_bp.is_none() || pct(p.percent_bp)),
        "basket" => p.threshold_minor.is_some_and(|t| t > 0) && (pct(p.percent_bp) ^ p.amount_minor.is_some_and(|a| a > 0)),
        _ => false,
    };
    if !ok {
        return Err(match p.kind.as_str() {
            "percent" => "Enter a percentage between 0.01% and 100%.",
            "amount" => "Enter the amount off each item.",
            "fixed_price" => "Enter the offer price.",
            "quantity" => "Enter how many items (2 or more) and the price for them.",
            "bxgy" => "Enter how many to buy and how many the customer gets.",
            _ => "Enter the minimum basket and either a percentage or an amount off.",
        }
        .into());
    }
    if p.priority.abs() > 1000 {
        return Err("Priority is between -1000 and 1000.".into());
    }
    Ok(())
}

/// Per promotion and per day, for reports and the dashboard: lines with the
/// value it took. (Grouping helper kept here so every reader groups alike.)
pub fn group_totals(rows: &[(String, i64)]) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for (k, v) in rows {
        *m.entry(k.clone()).or_insert(0) += v;
    }
    m
}

/// The set of product ids a promotion currently covers (for the editor's
/// coverage count). Categories are expanded through the catalogue.
pub fn coverage(c: &Connection, p: &Promotion) -> AppResult<i64> {
    if p.target == "all" {
        return Ok(c.query_row("SELECT COUNT(*) FROM products WHERE active=1", [], |r| r.get(0))?);
    }
    let mut set: HashSet<String> = p.buy_products.iter().cloned().collect();
    for cat in &p.buy_categories {
        let mut st = c.prepare_cached("SELECT product_id FROM products WHERE active=1 AND category_id=?1")?;
        for r in st.query_map([cat], |r| r.get::<_, String>(0))? {
            set.insert(r?);
        }
    }
    Ok(set.len() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str, pid: &str, price: i64, qty: i64) -> PLine {
        PLine {
            line_id: id.into(),
            product_id: Some(pid.into()),
            category_id: Some("cat".into()),
            unit_price_minor: price,
            qty_milli: qty,
            allow_decimal: false,
            excluded: None,
        }
    }

    fn promo(kind: &str) -> Promotion {
        Promotion {
            promotion_id: "p1".into(),
            name: "Offer".into(),
            name_ar: None,
            description: None,
            status: "active".into(),
            kind: kind.into(),
            target: "all".into(),
            starts_at: None,
            ends_at: None,
            branches: None,
            channels: None,
            priority: 0,
            stackable: false,
            requires_coupon: false,
            percent_bp: None,
            amount_minor: None,
            price_minor: None,
            buy_qty: None,
            get_qty: None,
            max_uses: None,
            threshold_minor: None,
            buy_products: vec![],
            buy_categories: vec![],
            get_products: vec![],
            get_categories: vec![],
            version: 0,
        }
    }

    fn run_one(p: &Promotion, lines: &[PLine]) -> Vec<i64> {
        let value: Vec<i64> = lines.iter().map(|l| l.unit_price_minor * l.qty_milli / 1000).collect();
        evaluate(p, lines, &value, &vec![true; lines.len()]).0
    }

    #[test]
    fn quantity_groups_are_formed_without_regard_to_line_order() {
        // "3 for 1.000" on 7 units at 0.400: two groups (2.400 → 2.000), one unit at full price.
        let mut p = promo("quantity");
        p.buy_qty = Some(3);
        p.price_minor = Some(1_000);
        let a = run_one(&p, &[line("l1", "x", 400, 4_000), line("l2", "x", 400, 3_000)]);
        assert_eq!(a.iter().sum::<i64>(), 400);
        let b = run_one(&p, &[line("l2", "x", 400, 3_000), line("l1", "x", 400, 4_000)]);
        assert_eq!(b.iter().sum::<i64>(), 400);
    }

    #[test]
    fn buy_x_get_y_rewards_the_cheapest_and_never_depends_on_order() {
        let mut p = promo("bxgy");
        p.buy_qty = Some(2);
        p.get_qty = Some(1);
        let ls = [line("a", "pa", 1_000, 1_000), line("b", "pb", 700, 1_000), line("c", "pc", 500, 1_000)];
        let d = run_one(&p, &ls);
        assert_eq!(d, vec![0, 0, 500], "the cheapest item is free");
        let rev = [ls[2].clone(), ls[1].clone(), ls[0].clone()];
        assert_eq!(run_one(&p, &rev), vec![500, 0, 0]);
    }

    #[test]
    fn fixed_amounts_never_exceed_the_line() {
        let mut p = promo("amount");
        p.amount_minor = Some(2_000);
        assert_eq!(run_one(&p, &[line("a", "x", 1_250, 1_000)]), vec![1_250]);
        let mut p = promo("basket");
        p.threshold_minor = Some(1_000);
        p.amount_minor = Some(5_000);
        let d = run_one(&p, &[line("a", "x", 1_250, 1_000), line("b", "y", 500, 1_000)]);
        assert_eq!(d.iter().sum::<i64>(), 1_750);
    }

    #[test]
    fn basket_threshold_is_measured_before_its_own_discount() {
        let mut p = promo("basket");
        p.threshold_minor = Some(10_000);
        p.percent_bp = Some(1_000);
        assert_eq!(run_one(&p, &[line("a", "x", 10_000, 1_000)]).iter().sum::<i64>(), 1_000);
        assert_eq!(run_one(&p, &[line("a", "x", 9_999, 1_000)]).iter().sum::<i64>(), 0);
    }

    #[test]
    fn codes_normalize_conservatively() {
        assert_eq!(normalize_code("  save 10 "), "SAVE10");
        assert_eq!(normalize_code("save-10"), "SAVE-10");
        assert_ne!(normalize_code("SAVE-10"), normalize_code("SAVE10"));
        assert_eq!(normalize_code("eid٢٠"), "EID20");
    }

    #[test]
    fn validation_bounds() {
        let mut p = promo("percent");
        p.percent_bp = Some(0);
        assert!(validate(&p).is_err());
        p.percent_bp = Some(10_001);
        assert!(validate(&p).is_err());
        p.percent_bp = Some(10_000);
        assert!(validate(&p).is_ok());
        let mut q = promo("quantity");
        q.buy_qty = Some(1);
        q.price_minor = Some(100);
        assert!(validate(&q).is_err());
        let mut b = promo("basket");
        b.threshold_minor = Some(100);
        b.percent_bp = Some(100);
        b.amount_minor = Some(100);
        assert!(validate(&b).is_err(), "either a percentage or an amount");
    }
}
