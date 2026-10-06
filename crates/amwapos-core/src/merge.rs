//! Likely duplicate products and the product merge
//! (docs/PRICING_AND_CATALOGUE.md).
//!
//! Duplicates: products are grouped by blocking keys (the same words and
//! size, the same barcode number written differently, the same supplier
//! item code, the same brand and size), so only products that share a key
//! are compared — never every product with every other. Each suggested pair
//! shows the evidence behind it. Words that change what a product is
//! ("Zero", "Light", a different size) are never treated as noise:
//! "Coca-Cola Zero 330ml" is not a duplicate of "Coca-Cola 330ml".
//!
//! Merge (owner only, cannot be undone): everything that points at the
//! retired product moves to the kept one, in one transaction:
//! - stock per branch and location, with paired `adjust` movements (the
//!   total never changes);
//! - batches: what is left of each batch leaves it and enters a new batch of
//!   the kept product (provenance `merge`, pointing at the old batch);
//! - barcodes, aliases, supplier mappings; supplier terms and price/PLU
//!   conflicts need an explicit choice;
//! - sales, receipts and every other historical record stay as they were.
//!
//! Open documents (orders, purchase orders, counts, transfers, returns,
//! drafts, sales in progress, reservations, WhatsApp catalogue listings)
//! block the merge until they are finished or cancelled.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::{new_id, next_seq};
use crate::inventory::{apply_movement_lot, current_qty, Movement};
use crate::lots;
use crate::money;
use crate::service::AppCore;
use crate::time;
use crate::validate;

// ------------------------------------------------------------------ normalization

const NOISE: [&str; 6] = ["pcs", "pc", "piece", "pieces", "the", "and"];

/// A product name as words plus its size (canonical: ml, g or units).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameKey {
    pub tokens: Vec<String>,
    pub size: Option<String>,
}

fn unit_factor(u: &str) -> Option<(&'static str, i64)> {
    Some(match u {
        "ml" | "مل" => ("ml", 1),
        "cl" => ("ml", 10),
        "l" | "lt" | "ltr" | "litre" | "liter" | "lit" | "لتر" | "ل" => ("ml", 1000),
        "g" | "gm" | "gr" | "gram" | "grams" | "غ" | "غم" | "جم" | "غرام" => ("g", 1),
        "kg" | "kgs" | "kilo" | "كغ" | "كجم" | "كيلو" => ("g", 1000),
        _ => return None,
    })
}

/// "1.5" → 1500 thousandths.
fn milli(num: &str) -> Option<i64> {
    let (i, f) = num.split_once('.').unwrap_or((num, ""));
    if i.is_empty() && f.is_empty() || f.len() > 3 {
        return None;
    }
    let i: i64 = if i.is_empty() { 0 } else { i.parse().ok()? };
    let f: i64 = if f.is_empty() { 0 } else { format!("{f:0<3}").parse().ok()? };
    i.checked_mul(1000)?.checked_add(f)
}

pub fn name_key(name: &str) -> NameKey {
    let s = crate::barcodes::ascii_digits(&name.to_lowercase());
    // Words: letters/digits; a '.' between digits stays (1.5l).
    let chars: Vec<char> = s.chars().collect();
    let mut words: Vec<String> = vec![];
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let keep = c.is_alphanumeric()
            || (c == '.' && i > 0 && chars[i - 1].is_ascii_digit() && chars.get(i + 1).map(|n| n.is_ascii_digit()).unwrap_or(false));
        if keep {
            // A boundary between digits and letters splits words: 330ml → 330 ml.
            if let Some(last) = cur.chars().last() {
                let digitish = |x: char| x.is_ascii_digit() || x == '.';
                if digitish(last) != digitish(c) {
                    words.push(std::mem::take(&mut cur));
                }
            }
            cur.push(c);
        } else if !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    let mut tokens = vec![];
    let mut size = None;
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        let is_num = w.chars().all(|c| c.is_ascii_digit() || c == '.');
        if is_num {
            // 6 x 330 ml: a pack count.
            if words.get(i + 1).map(|x| x == "x").unwrap_or(false)
                && words.get(i + 2).map(|x| x.chars().all(|c| c.is_ascii_digit())).unwrap_or(false)
            {
                tokens.push(format!("pack{w}"));
                i += 2;
                continue;
            }
            if let Some((unit, f)) = words.get(i + 1).and_then(|u| unit_factor(u)) {
                if let Some(m) = milli(w) {
                    // thousandths × factor / 1000 = canonical amount.
                    size = Some(format!("{}{unit}", m * f / 1000));
                    i += 2;
                    continue;
                }
            }
        }
        if !NOISE.contains(&w.as_str()) && w != "x" {
            tokens.push(w.clone());
        }
        i += 1;
    }
    tokens.sort();
    tokens.dedup();
    NameKey { tokens, size }
}

fn lev1(a: &str, b: &str) -> bool {
    // Edit distance ≤ 1 (one typo), for words of five letters or more.
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().min(b.len()) < 5 || a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
            continue;
        }
        edits += 1;
        if edits > 1 {
            return false;
        }
        match a.len().cmp(&b.len()) {
            std::cmp::Ordering::Greater => i += 1,
            std::cmp::Ordering::Less => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    edits + (a.len() - i) + (b.len() - j) <= 1
}

/// Name evidence between two products: "same" (same words and size),
/// "typo" (one-letter differences only), or None (a word or the size
/// differs — a different product).
pub fn name_match(a: &NameKey, b: &NameKey) -> Option<&'static str> {
    if a.size != b.size || a.tokens.is_empty() {
        return None;
    }
    if a.tokens == b.tokens {
        return Some("same");
    }
    if a.tokens.len() != b.tokens.len() {
        return None;
    }
    let covered = |x: &[String], y: &[String]| x.iter().all(|t| y.iter().any(|u| u == t || lev1(t, u)));
    (covered(&a.tokens, &b.tokens) && covered(&b.tokens, &a.tokens)).then_some("typo")
}

// ------------------------------------------------------------------ duplicates

#[derive(Debug, Clone, Serialize)]
pub struct DupProduct {
    pub product_id: String,
    pub name: String,
    pub sku: String,
    pub price_minor: Option<i64>,
    pub barcodes: Vec<String>,
    pub category_id: Option<String>,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    /// gtin | supplier_code | same_name | similar_name | same_size | same_category | similar_price | same_unit
    pub kind: String,
    pub detail: String,
    pub points: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DupPair {
    pub a: DupProduct,
    pub b: DupProduct,
    pub score: i64,
    pub evidence: Vec<Evidence>,
    /// None (new) | later
    pub decision: Option<String>,
}

struct Prod {
    p: DupProduct,
    key: NameKey,
    /// supplier_id:code, lower-case.
    codes: Vec<String>,
}

fn gtin_norm(b: &str) -> Option<String> {
    (matches!(b.len(), 8 | 12 | 13 | 14) && b.bytes().all(|c| c.is_ascii_digit())).then(|| b.trim_start_matches('0').to_string())
}

/// Suggested duplicate pairs with their evidence. Linear in the number of
/// products (plus the size of each block, which is capped).
pub fn find_duplicates(c: &Connection, include_later: bool, limit: usize) -> AppResult<Vec<DupPair>> {
    const MAX_BLOCK: usize = 40;
    let mut st = c.prepare(&format!(
        "SELECT p.product_id, p.name, p.sku, {}, p.category_id, p.unit FROM products p
         WHERE p.active=1 AND p.merged_into_product_id IS NULL",
        crate::catalog::PRICE_SQL
    ))?;
    let mut prods: Vec<Prod> = st
        .query_map([], |r| {
            Ok(DupProduct {
                product_id: r.get(0)?,
                name: r.get(1)?,
                sku: r.get(2)?,
                price_minor: r.get(3)?,
                barcodes: vec![],
                category_id: r.get(4)?,
                unit: r.get(5)?,
            })
        })?
        .map(|p| p.map(|p| Prod { key: name_key(&p.name), p, codes: vec![] }))
        .collect::<Result<Vec<_>, _>>()?;
    let index: HashMap<String, usize> = prods.iter().enumerate().map(|(i, p)| (p.p.product_id.clone(), i)).collect();
    let mut st = c.prepare("SELECT product_id, barcode FROM product_barcodes")?;
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (pid, b) = row?;
        if let Some(&i) = index.get(&pid) {
            prods[i].p.barcodes.push(b);
        }
    }
    // Blocking keys → members.
    let mut blocks: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, p) in prods.iter().enumerate() {
        if !p.key.tokens.is_empty() {
            blocks.entry(format!("n:{}|{}", p.key.tokens.join(" "), p.key.size.clone().unwrap_or_default())).or_default().push(i);
            let brand: Vec<&str> = p.key.tokens.iter().take(1).map(String::as_str).collect();
            blocks
                .entry(format!("b:{}|{}|{}", brand.join(" "), p.key.tokens.len(), p.key.size.clone().unwrap_or_default()))
                .or_default()
                .push(i);
        }
        for b in &p.p.barcodes {
            if let Some(g) = gtin_norm(b) {
                blocks.entry(format!("g:{g}")).or_default().push(i);
            }
        }
    }
    let mut st = c.prepare(
        "SELECT supplier_id, lower(trim(supplier_code)), product_id FROM supplier_products WHERE supplier_code IS NOT NULL AND trim(supplier_code)<>''",
    )?;
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
        let (sid, code, pid) = row?;
        if let Some(&i) = index.get(&pid) {
            blocks.entry(format!("s:{sid}:{code}")).or_default().push(i);
            prods[i].codes.push(format!("{sid}:{code}"));
        }
    }
    let mut decided: HashMap<(String, String), String> = HashMap::new();
    let mut st = c.prepare("SELECT product_a, product_b, decision FROM product_duplicate_decisions")?;
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
        let (a, b, d) = row?;
        decided.insert((a, b), d);
    }
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut out: Vec<DupPair> = vec![];
    let mut keys: Vec<&String> = blocks.keys().collect();
    keys.sort();
    for k in keys {
        let members = &blocks[k];
        if members.len() < 2 || members.len() > MAX_BLOCK {
            continue;
        }
        for x in 0..members.len() {
            for y in x + 1..members.len() {
                let (i, j) = (members[x].min(members[y]), members[x].max(members[y]));
                if i == j || !seen.insert((i, j)) {
                    continue;
                }
                let (pa, pb) = (&prods[i], &prods[j]);
                let (a, b) = if pa.p.product_id < pb.p.product_id { (pa, pb) } else { (pb, pa) };
                let decision = decided.get(&(a.p.product_id.clone(), b.p.product_id.clone())).cloned();
                if decision.as_deref() == Some("not_duplicates") || (decision.as_deref() == Some("later") && !include_later) {
                    continue;
                }
                if let Some(pair) = score(a, b) {
                    out.push(DupPair { a: a.p.clone(), b: b.p.clone(), score: pair.0, evidence: pair.1, decision });
                }
            }
        }
    }
    out.sort_by(|x, y| y.score.cmp(&x.score).then_with(|| x.a.name.cmp(&y.a.name)));
    out.truncate(limit);
    Ok(out)
}

fn score(a: &Prod, b: &Prod) -> Option<(i64, Vec<Evidence>)> {
    let mut ev = vec![];
    let gtin: Option<String> =
        a.p.barcodes.iter().filter_map(|x| gtin_norm(x)).find(|g| b.p.barcodes.iter().filter_map(|y| gtin_norm(y)).any(|h| &h == g));
    if let Some(g) = &gtin {
        ev.push(Evidence { kind: "gtin".into(), detail: format!("The same barcode number ({g}) written differently"), points: 70 });
    }
    let name = name_match(&a.key, &b.key);
    match name {
        Some("same") => ev.push(Evidence { kind: "same_name".into(), detail: "The same words and size".into(), points: 55 }),
        Some(_) => ev.push(Evidence {
            kind: "similar_name".into(),
            detail: "The same words apart from a one-letter difference".into(),
            points: 40,
        }),
        None => {}
    }
    let code = a.codes.iter().find(|c| b.codes.contains(c));
    if code.is_some() {
        ev.push(Evidence { kind: "supplier_code".into(), detail: "The same supplier item code".into(), points: 50 });
    }
    // Without a barcode match, the names must agree (words and size) — or
    // the supplier's item code must, with the same size.
    if gtin.is_none() && name.is_none() && (code.is_none() || a.key.size != b.key.size) {
        return None;
    }
    if let Some(sz) = a.key.size.as_ref().filter(|_| a.key.size == b.key.size) {
        ev.push(Evidence { kind: "same_size".into(), detail: format!("Same size ({sz})"), points: 10 });
    }
    if a.p.category_id.is_some() && a.p.category_id == b.p.category_id {
        ev.push(Evidence { kind: "same_category".into(), detail: "Same category".into(), points: 10 });
    }
    if let (Some(x), Some(y)) = (a.p.price_minor, b.p.price_minor) {
        if x > 0 && y > 0 && (x - y).abs() * 10 <= x.max(y) {
            ev.push(Evidence { kind: "similar_price".into(), detail: "Prices within 10%".into(), points: 10 });
        }
    }
    if a.p.unit == b.p.unit {
        ev.push(Evidence { kind: "same_unit".into(), detail: format!("Same unit ({})", a.p.unit), points: 5 });
    }
    let total: i64 = ev.iter().map(|e| e.points).sum::<i64>().min(100);
    Some((total, ev))
}

// ------------------------------------------------------------------ merge

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MergeChoices {
    /// Retail price to keep when the two differ: "target" | "source".
    #[serde(default)]
    pub price: Option<String>,
    /// PLU to keep when both have one: "target" | "source".
    #[serde(default)]
    pub plu: Option<String>,
    /// Per supplier with terms on both products: "target" | "source".
    #[serde(default)]
    pub supplier_terms: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MergeRequest {
    pub source_product_id: String,
    pub target_product_id: String,
    #[serde(default)]
    pub choices: MergeChoices,
    /// The preview the person saw; the merge is refused if anything changed.
    pub preview_hash: String,
    pub operation_id: String,
}

fn count(c: &Connection, sql: &str, pid: &str) -> AppResult<i64> {
    Ok(c.query_row(sql, [pid], |r| r.get(0))?)
}

fn refs(c: &Connection, sql: &str, pid: &str) -> AppResult<Vec<String>> {
    let mut st = c.prepare(sql)?;
    let v = st.query_map([pid], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// Open documents naming `pid`: (kind, label, references).
fn blockers(c: &Connection, pid: &str) -> AppResult<Vec<Value>> {
    let checks: [(&str, &str, &str); 12] = [
        ("digital_order", "Open customer orders", "SELECT DISTINCT o.order_number FROM digital_order_lines l JOIN digital_orders o ON o.order_id=l.order_id WHERE l.product_id=?1 AND o.status IN ('draft','confirmed')"),
        ("purchase_order", "Open purchase orders", "SELECT DISTINCT o.po_number FROM purchase_order_items l JOIN purchase_orders o ON o.po_id=l.po_id WHERE l.product_id=?1 AND o.status IN ('draft','ordered','partially_received')"),
        ("requisition", "Open requisitions", "SELECT DISTINCT r.number FROM requisition_lines l JOIN requisitions r ON r.requisition_id=l.requisition_id WHERE l.product_id=?1 AND r.status IN ('draft','submitted','approved')"),
        ("stocktake", "Stock counts in progress", "SELECT DISTINCT s.stocktake_number FROM stocktake_lines l JOIN stocktakes s ON s.stocktake_id=l.stocktake_id WHERE l.product_id=?1 AND s.status IN ('counting','review')"),
        ("transfer", "Transfers not yet received", "SELECT DISTINCT t.transfer_number FROM stock_transfer_lines l JOIN stock_transfers t ON t.transfer_id=l.transfer_id WHERE l.product_id=?1 AND t.status IN ('draft','shipped')"),
        ("receiving_draft", "Receiving drafts", "SELECT DISTINCT d.draft_id FROM receiving_draft_lines l JOIN receiving_drafts d ON d.draft_id=l.draft_id WHERE l.product_id=?1 AND d.status='draft'"),
        ("supplier_return", "Supplier returns not yet sent", "SELECT DISTINCT r.number FROM supplier_return_lines l JOIN supplier_returns r ON r.return_id=l.return_id WHERE l.product_id=?1 AND r.status='draft'"),
        ("supplier_invoice", "Supplier invoices not yet posted", "SELECT DISTINCT i.number FROM supplier_invoice_lines l JOIN supplier_invoices i ON i.invoice_id=l.invoice_id WHERE l.product_id=?1 AND (i.status='draft' OR (i.status='approved' AND i.posting='not_posted'))"),
        ("invoice_scan", "Invoice scans being reviewed", "SELECT DISTINCT l.scan_id FROM invoice_scan_lines l JOIN invoice_scans s ON s.scan_id=l.scan_id WHERE l.product_id=?1 AND s.status IN ('imported','read','review')"),
        ("cart", "Sales in progress or on hold", "SELECT DISTINCT c.cart_id FROM cart_lines l JOIN carts c ON c.cart_id=l.cart_id WHERE l.product_id=?1 AND c.status IN ('active','held')"),
        ("reservation", "Stock reserved for orders", "SELECT DISTINCT order_id FROM stock_reservations WHERE product_id=?1 AND status='active'"),
        ("wa_catalog", "Listed in the WhatsApp catalogue", "SELECT account FROM wa_catalog_products WHERE product_id=?1 AND status IN ('queued','syncing','synced','failed')"),
    ];
    let mut out = vec![];
    for (kind, label, sql) in checks {
        let r = refs(c, sql, pid)?;
        if !r.is_empty() {
            out.push(json!({ "kind": kind, "label": label, "count": r.len(), "refs": r.into_iter().take(10).collect::<Vec<_>>() }));
        }
    }
    Ok(out)
}

struct Side {
    id: String,
    name: String,
    sku: String,
    plu: Option<String>,
    price: Option<i64>,
    active: bool,
    merged_into: Option<String>,
    unit: String,
    allow_decimal: bool,
    track: bool,
}

fn side(c: &Connection, pid: &str) -> AppResult<Side> {
    c.query_row(
        &format!(
            "SELECT p.product_id, p.name, p.sku, p.plu, {}, p.active, p.merged_into_product_id, p.unit, p.allow_decimal_quantity, p.track_inventory
             FROM products p WHERE p.product_id=?1",
            crate::catalog::PRICE_SQL
        ),
        [pid],
        |r| {
            Ok(Side {
                id: r.get(0)?,
                name: r.get(1)?,
                sku: r.get(2)?,
                plu: r.get(3)?,
                price: r.get(4)?,
                active: r.get::<_, i64>(5)? == 1,
                merged_into: r.get(6)?,
                unit: r.get(7)?,
                allow_decimal: r.get::<_, i64>(8)? == 1,
                track: r.get::<_, i64>(9)? == 1,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("Product"))
}

/// Stock of `pid` per branch and location, from its movements.
fn stock_by_location(c: &Connection, pid: &str) -> AppResult<Vec<(String, Option<String>, i64)>> {
    let mut st = c.prepare(
        "SELECT branch_id, location_id, SUM(qty_delta_milli) FROM stock_movements WHERE product_id=?1
         GROUP BY branch_id, location_id HAVING SUM(qty_delta_milli)<>0 ORDER BY branch_id, location_id",
    )?;
    let v = st.query_map([pid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

fn branches_of(c: &Connection, pid: &str) -> AppResult<Vec<String>> {
    refs(c, "SELECT branch_id FROM stock_levels WHERE product_id=?1 AND qty_milli<>0 ORDER BY branch_id", pid)
}

/// Everything a merge would touch, and what it needs decided.
pub fn preview(c: &Connection, source: &str, target: &str) -> AppResult<Value> {
    if source == target {
        return Err(AppError::validation("Choose two different products."));
    }
    let s = side(c, source)?;
    let t = side(c, target)?;
    if s.merged_into.is_some() {
        return Err(AppError::conflict(format!("{} was already merged into another product.", s.name)));
    }
    if t.merged_into.is_some() || !t.active {
        return Err(AppError::conflict(format!("{} is archived or merged; keep an active product.", t.name)));
    }
    let mut issues: Vec<String> = vec![];
    if s.unit != t.unit || s.allow_decimal != t.allow_decimal {
        issues.push(format!(
            "The products are sold differently ({} / {}). Make them the same before merging.",
            if s.allow_decimal { format!("{} by weight", s.unit) } else { s.unit.clone() },
            if t.allow_decimal { format!("{} by weight", t.unit) } else { t.unit.clone() }
        ));
    }
    if s.track != t.track {
        issues.push("One product tracks stock and the other does not. Make them the same before merging.".into());
    }
    let mut stock = vec![];
    for b in branches_of(c, source)? {
        let bname: Option<String> = c.query_row("SELECT name FROM branches WHERE branch_id=?1", [&b], |r| r.get(0)).optional()?;
        let r = lots::replay(c, source, &b)?;
        let lots: Vec<Value> = r
            .lots
            .iter()
            .filter(|l| l.balance_milli > 0)
            .map(|l| json!({ "lot_id": l.facts.lot_id, "lot_number": l.facts.lot_number, "qty_milli": l.balance_milli, "expires_on": l.facts.expires_on }))
            .collect();
        stock.push(json!({
            "branch_id": b, "branch_name": bname, "source_milli": current_qty(c, source, &b)?, "target_milli": current_qty(c, target, &b)?,
            "lots": lots,
        }));
    }
    let barcodes = refs(c, "SELECT barcode FROM product_barcodes WHERE product_id=?1 ORDER BY barcode", source)?;
    let aliases = count(c, "SELECT COUNT(*) FROM product_aliases WHERE product_id=?1", source)?;
    let maps = count(c, "SELECT COUNT(*) FROM supplier_product_map WHERE product_id=?1", source)?;
    let mut st = c.prepare(
        "SELECT sp.supplier_id, su.name, sp.supplier_code, sp.units_per_case, sp.moq_packs, sp.lead_time_days, sp.preferred,
            (SELECT json_object('supplier_code', t.supplier_code, 'units_per_case', t.units_per_case, 'moq_packs', t.moq_packs,
                'lead_time_days', t.lead_time_days, 'preferred', t.preferred)
             FROM supplier_products t WHERE t.supplier_id=sp.supplier_id AND t.product_id=?2)
         FROM supplier_products sp JOIN suppliers su ON su.supplier_id=sp.supplier_id WHERE sp.product_id=?1 ORDER BY su.name",
    )?;
    let terms: Vec<Value> = st
        .query_map(params![source, target], |r| {
            let target_terms: Option<String> = r.get(7)?;
            Ok(json!({
                "supplier_id": r.get::<_, String>(0)?, "supplier_name": r.get::<_, String>(1)?,
                "source": { "supplier_code": r.get::<_, Option<String>>(2)?, "units_per_case": r.get::<_, Option<i64>>(3)?,
                    "moq_packs": r.get::<_, Option<i64>>(4)?, "lead_time_days": r.get::<_, Option<i64>>(5)?, "preferred": r.get::<_, i64>(6)? == 1 },
                "target": target_terms.and_then(|t| serde_json::from_str::<Value>(&t).ok()),
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let term_conflicts: Vec<&Value> = terms.iter().filter(|t| !t["target"].is_null()).collect();
    let channel_prices = count(
        c,
        "SELECT COUNT(*) FROM product_prices WHERE product_id=?1 AND price_type<>'retail' AND (effective_to IS NULL OR effective_to > amw_now())",
        source,
    )?;
    let history = json!({
        "sale_lines": count(c, "SELECT COUNT(*) FROM sale_items WHERE product_id=?1", source)?,
        "refund_lines": count(c, "SELECT COUNT(*) FROM refund_items WHERE product_id=?1", source)?,
        "movements": count(c, "SELECT COUNT(*) FROM stock_movements WHERE product_id=?1", source)?,
        "purchase_lines": count(c, "SELECT COUNT(*) FROM purchase_order_items WHERE product_id=?1", source)?,
        "lots": count(c, "SELECT COUNT(*) FROM stock_lots WHERE product_id=?1", source)?,
    });
    let conflicts = json!({
        "price": (s.price != t.price).then(|| json!({ "source": s.price, "target": t.price })),
        "plu": (s.plu.is_some() && t.plu.is_some()).then(|| json!({ "source": s.plu, "target": t.plu })),
        "supplier_terms": term_conflicts,
    });
    let mut v = json!({
        "source": { "product_id": s.id, "name": s.name, "sku": s.sku, "plu": s.plu, "price_minor": s.price, "unit": s.unit },
        "target": { "product_id": t.id, "name": t.name, "sku": t.sku, "plu": t.plu, "price_minor": t.price, "unit": t.unit },
        "blockers": blockers(c, source)?,
        "issues": issues,
        "moves": { "stock": stock, "barcodes": barcodes, "aliases": aliases, "supplier_maps": maps, "supplier_terms": terms,
            "plu_moves": s.plu.is_some() && t.plu.is_none() },
        "not_carried": { "channel_prices": channel_prices },
        "history": history,
        "conflicts": conflicts,
    });
    v["preview_hash"] = json!(idempotency::payload_hash("product.merge_preview", &v)?);
    v["can_merge"] = json!(
        v["blockers"].as_array().map(|a| a.is_empty()).unwrap_or(true) && v["issues"].as_array().map(|a| a.is_empty()).unwrap_or(true)
    );
    Ok(v)
}

fn choice<'a>(v: Option<&'a str>, what: &str) -> AppResult<&'a str> {
    match v {
        Some(x @ ("target" | "source")) => Ok(x),
        _ => Err(AppError::validation(format!("Choose which {what} to keep."))),
    }
}

struct Ctx<'a> {
    source: &'a str,
    target: &'a str,
    merge_id: &'a str,
    user: &'a str,
    device: &'a str,
}

/// Where a merge movement lands: stock location, batch, and why.
struct At<'a> {
    location: Option<&'a str>,
    lot: Option<&'a str>,
    reason: &'a str,
}

fn mv(c: &Connection, x: &Ctx, pid: &str, branch: &str, qty: i64, cost: i64, at: At) -> AppResult<()> {
    let At { location, lot, reason } = at;
    apply_movement_lot(
        c,
        &Movement {
            product_id: pid,
            branch_id: branch,
            kind: "adjust",
            qty_delta_milli: qty,
            unit_cost_minor: Some(cost),
            source_type: "product_merge",
            source_id: Some(x.merge_id),
            reason: Some(reason),
            user_id: Some(x.user),
            device_id: Some(x.device),
        },
        location,
        lot,
    )?;
    Ok(())
}

/// Move stock, batches and costs from source to target in every branch.
fn move_stock(c: &Connection, x: &Ctx) -> AppResult<Value> {
    let mut moved = vec![];
    let reason = "Product merge";
    let mut branches: Vec<String> = branches_of(c, x.source)?;
    for (b, _, _) in stock_by_location(c, x.source)? {
        if !branches.contains(&b) {
            branches.push(b);
        }
    }
    for b in branches {
        let s_qty = current_qty(c, x.source, &b)?;
        let t_qty = current_qty(c, x.target, &b)?;
        let s_cost = crate::inventory::avg_cost(c, x.source, &b)?;
        let t_cost = crate::inventory::avg_cost(c, x.target, &b)?;
        // Weighted-average cost of what the kept product now holds.
        let new_cost = if s_qty > 0 && t_qty > 0 {
            money::div_round(s_qty as i128 * s_cost as i128 + t_qty as i128 * t_cost as i128, (s_qty + t_qty) as i128) as i64
        } else if s_qty > 0 {
            s_cost
        } else {
            t_cost
        };
        let mut lot_moves = vec![];
        let r = lots::replay(c, x.source, &b)?;
        for l in r.lots.iter().filter(|l| l.balance_milli > 0) {
            let f = &l.facts;
            let new_lot = new_id();
            let number = format!("L-{:05}", next_seq(c, "lot")?);
            c.execute(
                "INSERT INTO stock_lots(lot_id, lot_number, product_id, branch_id, location_id, supplier_id, po_id, receipt_id, supplier_lot_code,
                     received_at, manufactured_on, expires_on, expiry_kind, expiry_source, qty_received_milli, unit_cost_minor, provenance,
                     created_by, created_at, merged_from_lot_id)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,'merge',?17,?18,?19)",
                params![
                    new_lot,
                    number,
                    x.target,
                    b,
                    f.location_id,
                    f.supplier_id,
                    f.po_id,
                    f.receipt_id,
                    f.supplier_lot_code,
                    f.received_at,
                    f.manufactured_on,
                    f.expires_on,
                    f.expiry_kind,
                    f.expiry_source,
                    l.balance_milli,
                    f.unit_cost_minor,
                    x.user,
                    time::now_str(),
                    f.lot_id
                ],
            )?;
            let why = format!("Product merge: batch {} → {number}", f.lot_number);
            let loc = f.location_id.as_deref();
            mv(c, x, x.source, &b, -l.balance_milli, s_cost, At { location: loc, lot: Some(&f.lot_id), reason: &why })?;
            mv(c, x, x.target, &b, l.balance_milli, s_cost, At { location: loc, lot: Some(&new_lot), reason: &why })?;
            lot_moves.push(json!({ "from_lot": f.lot_number, "to_lot": number, "qty_milli": l.balance_milli }));
        }
        // What remains, per location (lot-free; may be negative).
        let mut loc_moves = vec![];
        for (bb, loc, q) in stock_by_location(c, x.source)? {
            if bb != b || q == 0 {
                continue;
            }
            mv(c, x, x.source, &b, -q, s_cost, At { location: loc.as_deref(), lot: None, reason })?;
            mv(c, x, x.target, &b, q, s_cost, At { location: loc.as_deref(), lot: None, reason })?;
            loc_moves.push(json!({ "location_id": loc, "qty_milli": q }));
        }
        let left = current_qty(c, x.source, &b)?;
        if left != 0 || current_qty(c, x.target, &b)? != t_qty + s_qty {
            return Err(AppError::internal("Stock did not balance during the merge; nothing was changed."));
        }
        c.execute(
            "INSERT INTO product_costs(product_id, branch_id, avg_cost_minor, last_cost_minor, updated_at) VALUES (?1,?2,?3,?3,?4)
             ON CONFLICT(product_id, branch_id) DO UPDATE SET avg_cost_minor=?3, updated_at=?4",
            params![x.target, b, new_cost, time::now_str()],
        )?;
        moved.push(json!({ "branch_id": b, "qty_milli": s_qty, "target_before_milli": t_qty, "avg_cost_minor": new_cost,
            "lots": lot_moves, "locations": loc_moves }));
    }
    Ok(json!(moved))
}

impl AppCore {
    pub fn duplicates_list(&self, token: &str, include_later: bool, limit: Option<i64>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        let limit = validate::limit(limit, 100, 500) as usize;
        self.db.read(|c| {
            let pairs = find_duplicates(c, include_later, limit)?;
            let later: i64 = c.query_row("SELECT COUNT(*) FROM product_duplicate_decisions WHERE decision='later'", [], |r| r.get(0))?;
            Ok(json!({ "pairs": pairs, "later_count": later }))
        })
    }

    /// "Not duplicates" (remembered) or "Review later" for a suggested pair;
    /// `None` clears the decision.
    pub fn duplicate_decide(&self, token: &str, a: &str, b: &str, decision: Option<String>, note: Option<String>) -> AppResult<()> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let (a, b) = (validate::id(a, "Product")?, validate::id(b, "Product")?);
        if a == b {
            return Err(AppError::validation("Choose two different products."));
        }
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        if let Some(d) = &decision {
            if !matches!(d.as_str(), "not_duplicates" | "later") {
                return Err(AppError::validation("Choose Not duplicates or Review later."));
            }
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            match &decision {
                Some(d) => tx.execute(
                    "INSERT INTO product_duplicate_decisions(product_a, product_b, decision, note, decided_by, decided_at) VALUES (?1,?2,?3,?4,?5,?6)
                     ON CONFLICT(product_a, product_b) DO UPDATE SET decision=?3, note=?4, decided_by=?5, decided_at=?6",
                    params![a, b, d, note, s.user_id, time::now_str()],
                )?,
                None => tx.execute("DELETE FROM product_duplicate_decisions WHERE product_a=?1 AND product_b=?2", params![a, b])?,
            };
            audit::record(tx, &actor, "product.duplicate_decided", "product", Some(&a), None, Some(&json!({ "other": b, "decision": decision })))?;
            Ok(())
        })
    }

    pub fn product_merge_preview(&self, token: &str, source: &str, target: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        let (src, tgt) = (validate::id(source, "Product")?, validate::id(target, "Product")?);
        self.db.read(|c| preview(c, &src, &tgt))
    }

    /// Merge `source` into `target` (owner only; cannot be undone).
    pub fn product_merge(&self, token: &str, req: MergeRequest) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("catalog.merge")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let src = validate::id(&req.source_product_id, "Product")?;
        let tgt = validate::id(&req.target_product_id, "Product")?;
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "product.merge", &req)? {
                Check::Replay { result } => return Ok(result),
                Check::New { payload_hash } => payload_hash,
            };
            let p = preview(tx, &src, &tgt)?;
            if p["preview_hash"].as_str() != Some(req.preview_hash.as_str()) {
                return Err(AppError::conflict("Something changed since the preview. Review the merge again."));
            }
            if let Some(b) = p["blockers"].as_array().filter(|b| !b.is_empty()) {
                let what: Vec<&str> = b.iter().filter_map(|x| x["label"].as_str()).collect();
                return Err(AppError::conflict(format!("Finish or cancel these first: {}.", what.join(", ")))
                    .with_details(json!({ "blockers": b })));
            }
            if let Some(i) = p["issues"].as_array().and_then(|i| i.first()) {
                return Err(AppError::conflict(i.as_str().unwrap_or("These products cannot be merged.")));
            }
            let ch = &req.choices;
            let price_choice = if p["conflicts"]["price"].is_null() { None } else { Some(choice(ch.price.as_deref(), "price")?) };
            let plu_choice = if p["conflicts"]["plu"].is_null() { None } else { Some(choice(ch.plu.as_deref(), "PLU")?) };
            let mut term_choices: HashMap<String, &str> = HashMap::new();
            for t in p["conflicts"]["supplier_terms"].as_array().cloned().unwrap_or_default() {
                let sid = t["supplier_id"].as_str().unwrap_or_default().to_string();
                let name = t["supplier_name"].as_str().unwrap_or_default();
                let v = choice(ch.supplier_terms.get(&sid).map(String::as_str), &format!("terms of {name}"))?;
                term_choices.insert(sid, v);
            }
            let merge_id = new_id();
            let x = Ctx { source: &src, target: &tgt, merge_id: &merge_id, user: &s.user_id, device: &s.device_id };
            let now = time::now_str();
            // 1. Stock, batches and cost.
            let stock = move_stock(tx, &x)?;
            // 2. Barcodes, aliases and supplier mappings now point at the kept product.
            let bc = tx.execute("UPDATE product_barcodes SET product_id=?2, is_primary=0 WHERE product_id=?1", params![src, tgt])?;
            let primaries: i64 = tx.query_row("SELECT COUNT(*) FROM product_barcodes WHERE product_id=?1 AND is_primary=1", [&tgt], |r| r.get(0))?;
            if primaries == 0 {
                tx.execute(
                    "UPDATE product_barcodes SET is_primary=1 WHERE barcode_id=(SELECT barcode_id FROM product_barcodes WHERE product_id=?1 ORDER BY created_at LIMIT 1)",
                    [&tgt],
                )?;
            }
            let al = tx.execute("UPDATE product_aliases SET product_id=?2 WHERE product_id=?1", params![src, tgt])?;
            let maps = tx.execute("UPDATE supplier_product_map SET product_id=?2 WHERE product_id=?1", params![src, tgt])?;
            tx.execute("UPDATE unknown_barcodes SET resolved_product_id=?2 WHERE resolved_product_id=?1", params![src, tgt])?;
            // 3. Supplier terms: moved, or decided per supplier.
            let mut terms = vec![];
            let sids: Vec<String> = refs(tx, "SELECT supplier_id FROM supplier_products WHERE product_id=?1", &src)?;
            for sid in sids {
                let keep = term_choices.get(&sid).copied();
                match keep {
                    Some("target") => {
                        tx.execute("UPDATE supplier_products SET active=0, preferred=0, updated_at=?3 WHERE supplier_id=?1 AND product_id=?2", params![sid, src, now])?;
                    }
                    Some(_) => {
                        // The retired product's terms replace the kept product's.
                        let pref: i64 = tx.query_row(
                            "SELECT preferred FROM supplier_products WHERE supplier_id=?1 AND product_id=?2",
                            params![sid, tgt],
                            |r| r.get(0),
                        )?;
                        tx.execute(
                            "UPDATE supplier_products SET (supplier_code, units_per_case, pack_source, moq_packs, lead_time_days, terms_confirmed_by,
                                terms_confirmed_at) = (SELECT supplier_code, units_per_case, pack_source, moq_packs, lead_time_days, terms_confirmed_by,
                                terms_confirmed_at FROM supplier_products WHERE supplier_id=?1 AND product_id=?2),
                                updated_at=?4, version=version+1, preferred=?5
                             WHERE supplier_id=?1 AND product_id=?3",
                            params![sid, src, tgt, now, pref],
                        )?;
                        tx.execute("UPDATE supplier_products SET active=0, preferred=0, updated_at=?3 WHERE supplier_id=?1 AND product_id=?2", params![sid, src, now])?;
                    }
                    None => {
                        // Only the retired product had terms: they move, kept
                        // preferred only if the kept product has no preferred supplier.
                        let has_pref: bool = tx
                            .query_row("SELECT 1 FROM supplier_products WHERE product_id=?1 AND preferred=1", [&tgt], |_| Ok(true))
                            .optional()?
                            .unwrap_or(false);
                        tx.execute(
                            "INSERT INTO supplier_products(supplier_id, product_id, supplier_code, units_per_case, pack_source, moq_packs, lead_time_days,
                                preferred, active, terms_confirmed_by, terms_confirmed_at, created_at, updated_at, version)
                             SELECT supplier_id, ?3, supplier_code, units_per_case, pack_source, moq_packs, lead_time_days,
                                CASE WHEN ?4 THEN 0 ELSE preferred END, active, terms_confirmed_by, terms_confirmed_at, ?5, ?5, 1
                             FROM supplier_products WHERE supplier_id=?1 AND product_id=?2",
                            params![sid, src, tgt, has_pref, now],
                        )?;
                        tx.execute("UPDATE supplier_products SET active=0, preferred=0, updated_at=?3 WHERE supplier_id=?1 AND product_id=?2", params![sid, src, now])?;
                    }
                }
                terms.push(json!({ "supplier_id": sid, "kept": keep.unwrap_or("moved") }));
            }
            // 4. Price and PLU, as chosen.
            if price_choice == Some("source") {
                if let Some(price) = p["conflicts"]["price"]["source"].as_i64() {
                    crate::catalog::set_price(tx, &s, &tgt, price, Some("Product merge: price of the merged product kept"), &now, &actor)?;
                }
            }
            let source_plu = p["source"]["plu"].as_str().map(String::from);
            tx.execute("UPDATE products SET plu=NULL WHERE product_id=?1", [&src])?;
            if let Some(plu) = source_plu.filter(|_| plu_choice == Some("source") || p["target"]["plu"].is_null()) {
                tx.execute("UPDATE products SET plu=?2 WHERE product_id=?1", params![tgt, plu])?;
            }
            // 5. The retired product: archived, pointing at the kept one.
            tx.execute(
                "UPDATE products SET active=0, archived_at=?2, merged_into_product_id=?3, merged_at=?2, is_favorite=0, updated_at=?2,
                    version=version+1 WHERE product_id=?1",
                params![src, now, tgt],
            )?;
            tx.execute("UPDATE products SET updated_at=?2, version=version+1 WHERE product_id=?1", params![tgt, now])?;
            tx.execute(
                "DELETE FROM product_duplicate_decisions WHERE product_a=?1 OR product_b=?1",
                [&src],
            )?;
            let moved = json!({ "stock": stock, "barcodes": bc, "aliases": al, "supplier_maps": maps, "supplier_terms": terms,
                "price": price_choice, "plu": plu_choice });
            tx.execute(
                "INSERT INTO product_merges(merge_id, source_product_id, target_product_id, preview_json, choices_json, moved_json, user_id,
                    device_id, operation_id, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    merge_id,
                    src,
                    tgt,
                    p.to_string(),
                    serde_json::to_string(&req.choices)?,
                    moved.to_string(),
                    s.user_id,
                    s.device_id,
                    req.operation_id,
                    now
                ],
            )?;
            audit::record(
                tx,
                &actor,
                "product.merged",
                "product",
                Some(&src),
                Some(&json!({ "source": p["source"], "target": p["target"] })),
                Some(&json!({ "merge_id": merge_id, "merged_into": tgt, "moved": moved })),
            )?;
            let result = json!({ "merge_id": merge_id, "source_product_id": src, "target_product_id": tgt, "moved": moved });
            idempotency::complete(tx, &req.operation_id, "product.merge", Some(&s.user_id), Some(&s.device_id), &hash, Some(&merge_id), &result)?;
            Ok(result)
        })
    }

    pub fn product_merges_list(&self, token: &str, product_id: Option<String>) -> AppResult<Vec<Value>> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT m.merge_id, m.source_product_id, ps.name, m.target_product_id, pt.name, u.display_name, m.created_at, m.moved_json
                 FROM product_merges m LEFT JOIN products ps ON ps.product_id=m.source_product_id
                 LEFT JOIN products pt ON pt.product_id=m.target_product_id LEFT JOIN users u ON u.user_id=m.user_id
                 WHERE ?1 IS NULL OR m.source_product_id=?1 OR m.target_product_id=?1 ORDER BY m.created_at DESC LIMIT 200",
            )?;
            let rows = st
                .query_map([&product_id], |r| {
                    Ok(json!({ "merge_id": r.get::<_, String>(0)?, "source_product_id": r.get::<_, String>(1)?, "source_name": r.get::<_, Option<String>>(2)?,
                        "target_product_id": r.get::<_, String>(3)?, "target_name": r.get::<_, Option<String>>(4)?, "by": r.get::<_, Option<String>>(5)?,
                        "created_at": r.get::<_, String>(6)?, "moved": serde_json::from_str::<Value>(&r.get::<_, String>(7)?).unwrap_or(Value::Null) }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_normalize_units_digits_and_word_order() {
        assert_eq!(name_key("Coca-Cola 330ml"), name_key("coca cola 330 ML"));
        assert_eq!(name_key("Milk 1L").size.as_deref(), Some("1000ml"));
        assert_eq!(name_key("Milk 1.5 ltr").size.as_deref(), Some("1500ml"));
        assert_eq!(name_key("Rice ٥ kg").size.as_deref(), Some("5000g"));
        assert_eq!(name_key("Laban 500 مل").size.as_deref(), Some("500ml"));
        assert_eq!(name_key("Water 6x330ml").tokens, vec!["pack6".to_string(), "water".to_string()]);
        assert_eq!(name_key("Tea Lipton").tokens, name_key("Lipton Tea").tokens);
    }

    #[test]
    fn distinguishing_words_and_sizes_are_never_noise() {
        let m = |a: &str, b: &str| name_match(&name_key(a), &name_key(b));
        assert_eq!(m("Coca-Cola Zero 330ml", "Coca-Cola 330ml"), None);
        assert_eq!(m("Coca-Cola 330ml", "Coca-Cola 500ml"), None);
        assert_eq!(m("Almarai Milk Full Fat 1L", "Almarai Milk Low Fat 1L"), None);
        assert_eq!(m("Coca-Cola 330ml", "COCA COLA 330 ml"), Some("same"));
        assert_eq!(m("Almarai Laban 1L", "Almari Laban 1L"), Some("typo"));
        assert_eq!(m("Lipton Tea 100 pcs", "Lipton Tea 100"), Some("same"), "'pcs' is noise");
        assert_eq!(m("Lipton Tea 100", "Lipton Tea 50"), None);
    }
}
