//! The supplier catalogue: which supplier sells which product, and on what
//! terms (docs/PROCUREMENT.md).
//!
//! One model, two kinds of record:
//! - `supplier_products`: one row per supplier and product, holding the terms
//!   the product is ordered on (pack size, minimum order, lead time,
//!   preferred supplier, the supplier's item code);
//! - `supplier_product_map`: the evidence of how the supplier's documents
//!   name the product (item codes and descriptions; many per product), kept
//!   by Document Intelligence exactly as before.
//!
//! A pack size a person typed or confirmed here wins over one read from a
//! document. Pack arithmetic is exact and deterministic (`order_quantity`).

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::money::QTY_SCALE;
use crate::service::AppCore;
use crate::setup::clean_opt;
use crate::time;
use crate::validate;

/// The quantity to order for a need, in whole packs where the pack size is
/// known, and never below the minimum order.
///
/// - `need_milli`: single units still needed (× 1000);
/// - `units_per_case`: single units in one pack (must be > 0 when given);
/// - `moq`: minimum order, in packs when a pack size is known, else in units;
/// - `decimal`: the product is sold by weight or length (no whole units).
///
/// Returns (packs, quantity in milli-units). Example: a case of 24, a need of
/// 37, a minimum of 2 cases → 2 cases = 48 units.
pub fn order_quantity(need_milli: i64, units_per_case: Option<i64>, moq: Option<i64>, decimal: bool) -> AppResult<(Option<i64>, i64)> {
    if need_milli < 0 {
        return Err(AppError::validation("A need cannot be negative."));
    }
    if moq.is_some_and(|m| m <= 0) {
        return Err(AppError::validation("A minimum order must be more than zero."));
    }
    match units_per_case {
        Some(u) if u <= 0 => Err(AppError::validation("A pack size must be more than zero.")),
        Some(u) => {
            let pack = u.checked_mul(QTY_SCALE).ok_or_else(|| AppError::validation("The pack size is too large."))?;
            let packs = (need_milli + pack - 1) / pack;
            let packs = packs.max(moq.unwrap_or(0));
            let qty = packs.checked_mul(pack).ok_or_else(|| AppError::validation("The quantity is too large."))?;
            Ok((Some(packs), qty))
        }
        None => {
            let qty = if decimal { need_milli } else { (need_milli + QTY_SCALE - 1) / QTY_SCALE * QTY_SCALE };
            let min = moq.unwrap_or(0).checked_mul(QTY_SCALE).ok_or_else(|| AppError::validation("The minimum order is too large."))?;
            Ok((None, qty.max(min)))
        }
    }
}

/// Terms of one supplier for one product.
#[derive(Debug, Clone, Serialize)]
pub struct Terms {
    pub supplier_id: String,
    pub supplier_name: String,
    pub supplier_active: bool,
    pub product_id: String,
    pub supplier_code: Option<String>,
    pub units_per_case: Option<i64>,
    pub pack_source: Option<String>,
    pub moq_packs: Option<i64>,
    pub lead_time_days: Option<i64>,
    pub preferred: bool,
    pub active: bool,
    pub terms_confirmed_by: Option<String>,
    pub terms_confirmed_at: Option<String>,
    pub version: i64,
}

fn terms_row(r: &rusqlite::Row) -> rusqlite::Result<Terms> {
    Ok(Terms {
        supplier_id: r.get(0)?,
        supplier_name: r.get(1)?,
        supplier_active: r.get::<_, i64>(2)? == 1,
        product_id: r.get(3)?,
        supplier_code: r.get(4)?,
        units_per_case: r.get(5)?,
        pack_source: r.get(6)?,
        moq_packs: r.get(7)?,
        lead_time_days: r.get(8)?,
        preferred: r.get::<_, i64>(9)? == 1,
        active: r.get::<_, i64>(10)? == 1,
        terms_confirmed_by: r.get(11)?,
        terms_confirmed_at: r.get(12)?,
        version: r.get(13)?,
    })
}

const TERMS_SELECT: &str =
    "SELECT t.supplier_id, s.name, s.active, t.product_id, t.supplier_code, t.units_per_case, t.pack_source, t.moq_packs,
        t.lead_time_days, t.preferred, t.active, t.terms_confirmed_by, t.terms_confirmed_at, t.version
     FROM supplier_products t JOIN suppliers s ON s.supplier_id=t.supplier_id";

/// Every supplier's terms, grouped by product, in one query.
pub fn all_terms(c: &Connection) -> AppResult<HashMap<String, Vec<Terms>>> {
    let mut st = c.prepare(TERMS_SELECT)?;
    let mut out: HashMap<String, Vec<Terms>> = HashMap::new();
    for t in st.query_map([], terms_row)? {
        let t = t?;
        out.entry(t.product_id.clone()).or_default().push(t);
    }
    Ok(out)
}

pub fn terms_for(c: &Connection, supplier_id: &str, product_id: &str) -> AppResult<Option<Terms>> {
    Ok(c.query_row(&format!("{TERMS_SELECT} WHERE t.supplier_id=?1 AND t.product_id=?2"), params![supplier_id, product_id], terms_row)
        .optional()?)
}

/// A pack size a person confirmed for this supplier and product, if any.
/// It wins over a pack size read from a document.
pub fn confirmed_pack(c: &Connection, supplier_id: &str, product_id: &str) -> AppResult<Option<i64>> {
    Ok(c.query_row(
        "SELECT units_per_case FROM supplier_products WHERE supplier_id=?1 AND product_id=?2 AND pack_source='person' AND units_per_case > 0",
        params![supplier_id, product_id],
        |r| r.get(0),
    )
    .optional()?
    .flatten())
}

/// Record that a supplier sells a product, learned from a reviewed supplier
/// document. A pack size from the document fills an empty one; it never
/// replaces one a person confirmed.
pub fn learn_from_document(c: &Connection, supplier_id: &str, product_id: &str, units_per_case: Option<i64>) -> AppResult<()> {
    let now = time::now_str();
    let upc = units_per_case.filter(|u| *u > 0);
    c.execute(
        "INSERT INTO supplier_products(supplier_id, product_id, units_per_case, pack_source, created_at, updated_at)
         VALUES (?1,?2,?3,CASE WHEN ?3 IS NULL THEN NULL ELSE 'document' END,?4,?4)
         ON CONFLICT(supplier_id, product_id) DO UPDATE SET
           units_per_case = CASE WHEN supplier_products.pack_source='person' OR excluded.units_per_case IS NULL THEN supplier_products.units_per_case ELSE excluded.units_per_case END,
           pack_source = CASE WHEN supplier_products.pack_source='person' OR excluded.units_per_case IS NULL THEN supplier_products.pack_source ELSE 'document' END,
           updated_at = CASE WHEN supplier_products.pack_source='person' OR excluded.units_per_case IS NULL
                              OR excluded.units_per_case IS supplier_products.units_per_case THEN supplier_products.updated_at ELSE excluded.updated_at END",
        params![supplier_id, product_id, upc, now],
    )?;
    Ok(())
}

/// Record that a supplier sells a product (an order or a receipt), without
/// inventing any terms.
pub fn note_supplier_product(c: &Connection, supplier_id: &str, product_id: &str) -> AppResult<()> {
    let now = time::now_str();
    c.execute(
        "INSERT OR IGNORE INTO supplier_products(supplier_id, product_id, created_at, updated_at) VALUES (?1,?2,?3,?3)",
        params![supplier_id, product_id, now],
    )?;
    Ok(())
}

/// Cost baselines for a supplier and product (docs/PROCUREMENT.md):
/// - last confirmed: the newest cost a person confirmed (a receipt, or a
///   posted supplier invoice); never a figure read by OCR alone;
/// - typical: the median unit cost of the last five receipts from this
///   supplier (the lower middle one when there is an even number).
#[derive(Debug, Clone, Serialize, Default)]
pub struct CostBaselines {
    pub last_confirmed_minor: Option<i64>,
    pub last_confirmed_at: Option<String>,
    pub typical_minor: Option<i64>,
    pub typical_receipts: i64,
}

pub fn cost_baselines(c: &Connection, supplier_id: &str, product_id: &str) -> AppResult<CostBaselines> {
    let last: Option<(i64, String)> = c
        .query_row(
            "SELECT cost_minor, effective_at FROM product_cost_history WHERE product_id=?1 AND supplier_id=?2 AND source IN ('receiving','supplier_invoice')
             ORDER BY effective_at DESC, cost_id DESC LIMIT 1",
            params![product_id, supplier_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let mut st = c.prepare_cached(
        "SELECT i.unit_cost_minor FROM goods_receipt_items i JOIN goods_receipts g ON g.receipt_id=i.receipt_id
         WHERE i.product_id=?1 AND g.supplier_id=?2 ORDER BY g.created_at DESC, i.receipt_item_id DESC LIMIT 5",
    )?;
    let mut costs: Vec<i64> = st.query_map(params![product_id, supplier_id], |r| r.get(0))?.collect::<Result<_, _>>()?;
    costs.sort_unstable();
    let typical = (!costs.is_empty()).then(|| costs[(costs.len() - 1) / 2]);
    Ok(CostBaselines {
        last_confirmed_minor: last.as_ref().map(|x| x.0),
        last_confirmed_at: last.map(|x| x.1),
        typical_minor: typical,
        typical_receipts: costs.len() as i64,
    })
}

/// Last confirmed cost for every (supplier, product), in one query.
pub fn last_confirmed_costs(c: &Connection) -> AppResult<HashMap<(String, String), i64>> {
    let mut st = c.prepare(
        "SELECT supplier_id, product_id, cost_minor FROM (
           SELECT supplier_id, product_id, cost_minor,
                  ROW_NUMBER() OVER (PARTITION BY supplier_id, product_id ORDER BY effective_at DESC, cost_id DESC) AS rn
           FROM product_cost_history WHERE supplier_id IS NOT NULL AND source IN ('receiving','supplier_invoice'))
         WHERE rn = 1",
    )?;
    let mut out = HashMap::new();
    for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))? {
        let (s, p, cost) = r?;
        out.insert((s, p), cost);
    }
    Ok(out)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TermsInput {
    pub supplier_id: String,
    pub product_id: String,
    #[serde(default)]
    pub supplier_code: Option<String>,
    #[serde(default)]
    pub units_per_case: Option<i64>,
    #[serde(default)]
    pub moq_packs: Option<i64>,
    #[serde(default)]
    pub lead_time_days: Option<i64>,
    #[serde(default)]
    pub preferred: bool,
    #[serde(default = "yes")]
    pub active: bool,
    /// The version the person edited (refused if someone changed it since).
    #[serde(default)]
    pub expected_version: Option<i64>,
}
fn yes() -> bool {
    true
}

impl AppCore {
    /// Supplier catalogue rows, for a supplier or a product, with the
    /// document evidence and cost baselines.
    pub fn supplier_catalogue(&self, token: &str, supplier_id: Option<String>, product_id: Option<String>) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("suppliers.manage") && !s.has("purchasing.manage") && !s.has("requisitions.create") {
            return Err(AppError::forbidden("suppliers.manage"));
        }
        let show_cost = s.has("products.view_cost");
        let sid = supplier_id.filter(|x| !x.is_empty()).map(|x| validate::id(&x, "Supplier")).transpose()?;
        let pid = product_id.filter(|x| !x.is_empty()).map(|x| validate::id(&x, "Product")).transpose()?;
        if sid.is_none() && pid.is_none() {
            return Err(AppError::validation("Choose a supplier or a product."));
        }
        self.db.read(|c| {
            let mut st = c.prepare(&format!(
                "{TERMS_SELECT} WHERE (?1 IS NULL OR t.supplier_id=?1) AND (?2 IS NULL OR t.product_id=?2) ORDER BY s.name COLLATE NOCASE LIMIT 2000"
            ))?;
            let rows: Vec<Terms> = st.query_map(params![sid, pid], terms_row)?.collect::<Result<_, _>>()?;
            let mut out = vec![];
            for t in rows {
                let (name, name_ar, sku, dec): (String, Option<String>, String, i64) = c.query_row(
                    "SELECT name, name_ar, sku, allow_decimal_quantity FROM products WHERE product_id=?1",
                    [&t.product_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?;
                let mut st = c.prepare_cached(
                    "SELECT key_kind, key_norm, units_per_case, uses, confirmed_at FROM supplier_product_map WHERE supplier_id=?1 AND product_id=?2 ORDER BY uses DESC LIMIT 20",
                )?;
                let evidence: Vec<Value> = st
                    .query_map(params![t.supplier_id, t.product_id], |r| {
                        Ok(json!({ "kind": r.get::<_, String>(0)?, "key": r.get::<_, String>(1)?, "units_per_case": r.get::<_, Option<i64>>(2)?,
                            "uses": r.get::<_, i64>(3)?, "confirmed_at": r.get::<_, String>(4)? }))
                    })?
                    .collect::<Result<_, _>>()?;
                let cb = if show_cost { Some(cost_baselines(c, &t.supplier_id, &t.product_id)?) } else { None };
                out.push(json!({ "terms": t, "product_name": name, "product_name_ar": name_ar, "sku": sku, "allow_decimal_quantity": dec == 1,
                    "document_evidence": evidence, "costs": cb }));
            }
            Ok(json!({ "rows": out }))
        })
    }

    /// Set a supplier's terms for a product. These are confirmed by a person,
    /// so they win over anything read from a document.
    pub fn supplier_terms_save(&self, token: &str, input: TermsInput) -> AppResult<Terms> {
        let s = self.session(token)?;
        s.require("suppliers.manage")?;
        self.require_back_office_writable()?;
        let sid = validate::id(&input.supplier_id, "Supplier")?;
        let pid = validate::id(&input.product_id, "Product")?;
        let code = clean_opt(&input.supplier_code, "Supplier item code", 60)?;
        if input.units_per_case.is_some_and(|u| u <= 0) {
            return Err(AppError::validation("A pack size must be more than zero."));
        }
        if input.units_per_case.is_some_and(|u| u > 100_000) {
            return Err(AppError::validation("A pack size can be at most 100,000."));
        }
        if input.moq_packs.is_some_and(|m| m <= 0 || m > 1_000_000) {
            return Err(AppError::validation("A minimum order must be more than zero."));
        }
        if input.lead_time_days.is_some_and(|d| !(0..=365).contains(&d)) {
            return Err(AppError::validation("Lead time must be 0–365 days."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            tx.query_row("SELECT 1 FROM suppliers WHERE supplier_id=?1", [&sid], |_| Ok(())).optional()?.ok_or_else(|| AppError::not_found("Supplier"))?;
            tx.query_row("SELECT 1 FROM products WHERE product_id=?1", [&pid], |_| Ok(())).optional()?.ok_or_else(|| AppError::not_found("Product"))?;
            let before = terms_for(tx, &sid, &pid)?;
            if let (Some(b), Some(v)) = (&before, input.expected_version) {
                if b.version != v {
                    return Err(AppError::conflict("Someone changed these terms meanwhile. Reload and try again."));
                }
            }
            let now = time::now_str();
            if input.preferred {
                tx.execute(
                    "UPDATE supplier_products SET preferred=0, updated_at=?3, version=version+1 WHERE product_id=?1 AND supplier_id<>?2 AND preferred=1",
                    params![pid, sid, now],
                )?;
            }
            tx.execute(
                "INSERT INTO supplier_products(supplier_id, product_id, supplier_code, units_per_case, pack_source, moq_packs, lead_time_days, preferred, active,
                    terms_confirmed_by, terms_confirmed_at, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,CASE WHEN ?4 IS NULL THEN NULL ELSE 'person' END,?5,?6,?7,?8,?9,?10,?10,?10)
                 ON CONFLICT(supplier_id, product_id) DO UPDATE SET supplier_code=excluded.supplier_code, units_per_case=excluded.units_per_case,
                    pack_source=excluded.pack_source, moq_packs=excluded.moq_packs, lead_time_days=excluded.lead_time_days, preferred=excluded.preferred,
                    active=excluded.active, terms_confirmed_by=excluded.terms_confirmed_by, terms_confirmed_at=excluded.terms_confirmed_at,
                    updated_at=excluded.updated_at, version=supplier_products.version+1",
                params![sid, pid, code, input.units_per_case, input.moq_packs, input.lead_time_days, input.preferred as i64, input.active as i64, s.user_id, now],
            )?;
            let after = terms_for(tx, &sid, &pid)?.ok_or_else(|| AppError::internal("terms vanished"))?;
            audit::record(
                tx,
                &actor,
                "supplier_terms.saved",
                "supplier_product",
                Some(&format!("{sid}:{pid}")),
                before.as_ref().map(serde_json::to_value).transpose()?.as_ref(),
                Some(&serde_json::to_value(&after)?),
            )?;
            Ok(after)
        })
    }

    /// A product's maximum stock (what a suggested order fills up to), or
    /// none. The reorder point stays on the product form, as before.
    pub fn product_max_stock(&self, token: &str, product_id: &str, max_stock_milli: Option<i64>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let pid = validate::id(product_id, "Product")?;
        if max_stock_milli.is_some_and(|m| m <= 0) {
            return Err(AppError::validation("Maximum stock must be more than zero."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let (reorder, before): (i64, Option<i64>) = tx
                .query_row("SELECT reorder_point_milli, max_stock_milli FROM products WHERE product_id=?1", [&pid], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Product"))?;
            if max_stock_milli.is_some_and(|m| m < reorder) {
                return Err(AppError::validation("Maximum stock cannot be below the reorder point."));
            }
            tx.execute(
                "UPDATE products SET max_stock_milli=?2, updated_at=?3, version=version+1 WHERE product_id=?1",
                params![pid, max_stock_milli, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "product.max_stock",
                "product",
                Some(&pid),
                Some(&json!({ "max_stock_milli": before })),
                Some(&json!({ "max_stock_milli": max_stock_milli })),
            )?;
            Ok(json!({ "product_id": pid, "reorder_point_milli": reorder, "max_stock_milli": max_stock_milli }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_round_up_and_respect_the_minimum() {
        // A case of 24, a need of 37, a minimum of 2 cases → 48 units.
        assert_eq!(order_quantity(37_000, Some(24), Some(2), false).unwrap(), (Some(2), 48_000));
        assert_eq!(order_quantity(37_000, Some(24), None, false).unwrap(), (Some(2), 48_000));
        assert_eq!(order_quantity(49_000, Some(24), Some(2), false).unwrap(), (Some(3), 72_000));
        assert_eq!(order_quantity(1_000, Some(24), Some(5), false).unwrap(), (Some(5), 120_000));
        assert_eq!(order_quantity(0, Some(24), None, false).unwrap(), (Some(0), 0));
        // No pack size: whole units, or exact for decimal products.
        assert_eq!(order_quantity(2_500, None, None, false).unwrap(), (None, 3_000));
        assert_eq!(order_quantity(2_500, None, None, true).unwrap(), (None, 2_500));
        assert_eq!(order_quantity(2_500, None, Some(10), false).unwrap(), (None, 10_000));
    }

    #[test]
    fn zero_or_negative_packs_are_refused() {
        assert!(order_quantity(1_000, Some(0), None, false).is_err());
        assert!(order_quantity(1_000, Some(-6), None, false).is_err());
        assert!(order_quantity(1_000, Some(6), Some(0), false).is_err());
        assert!(order_quantity(-1, Some(6), None, false).is_err());
    }
}
