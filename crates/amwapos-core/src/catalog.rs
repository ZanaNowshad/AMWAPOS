//! Product catalogue: products, barcodes, categories, tax rules, prices and costs.

use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult};
use crate::ids::{new_id, next_seq};
use crate::inventory;
use crate::service::AppCore;
use crate::setup::{clean, clean_opt};
use crate::time;
use crate::validate;

/// Current retail price of `p.product_id` (effective-dated).
pub const PRICE_SQL: &str = "(SELECT pp.amount_minor FROM product_prices pp
    WHERE pp.product_id = p.product_id AND pp.price_type = 'retail'
      AND pp.effective_from <= strftime('%Y-%m-%dT%H:%M:%fZ','now')
      AND (pp.effective_to IS NULL OR pp.effective_to > strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    ORDER BY pp.effective_from DESC LIMIT 1)";

#[derive(Debug, Clone, Serialize)]
pub struct Page<T> {
    pub rows: Vec<T>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProductRow {
    pub product_id: String,
    pub sku: String,
    pub name: String,
    pub name_ar: Option<String>,
    pub category_id: Option<String>,
    pub category_name: Option<String>,
    pub primary_barcode: Option<String>,
    pub barcode_count: i64,
    pub price_minor: Option<i64>,
    pub cost_minor: Option<i64>,
    pub stock_milli: i64,
    pub reorder_point_milli: i64,
    pub unit: String,
    pub track_inventory: bool,
    pub allow_decimal_quantity: bool,
    pub active: bool,
    pub is_favorite: bool,
    pub tax_rule_id: String,
    pub tax_rate_bp: i64,
    pub tax_inclusive: bool,
    pub stock_status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BarcodeRow {
    pub barcode_id: String,
    pub barcode: String,
    pub is_primary: bool,
    pub source: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PriceRow {
    pub price_id: String,
    pub amount_minor: i64,
    pub effective_from: String,
    pub effective_to: Option<String>,
    pub reason: Option<String>,
    pub created_by_name: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CostRow {
    pub cost_id: String,
    pub cost_minor: i64,
    pub source: String,
    pub supplier_name: Option<String>,
    pub effective_at: String,
    pub created_by_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProductDetail {
    #[serde(flatten)]
    pub row: ProductRow,
    pub description: Option<String>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub barcodes: Vec<BarcodeRow>,
    pub price_history: Vec<PriceRow>,
    pub cost_history: Option<Vec<CostRow>>,
    pub avg_cost_minor: Option<i64>,
    pub last_cost_minor: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProductQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub category_id: Option<String>,
    /// active | archived | all (default active)
    #[serde(default)]
    pub status: Option<String>,
    /// low | out | negative | in_stock
    #[serde(default)]
    pub stock: Option<String>,
    #[serde(default)]
    pub favorites: Option<bool>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProductInput {
    #[serde(default)]
    pub sku: Option<String>,
    pub name: String,
    #[serde(default)]
    pub name_ar: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub category_id: Option<String>,
    pub tax_rule_id: String,
    #[serde(default = "default_unit")]
    pub unit: String,
    #[serde(default = "default_true")]
    pub track_inventory: bool,
    #[serde(default)]
    pub allow_decimal_quantity: bool,
    #[serde(default)]
    pub reorder_point_milli: i64,
    #[serde(default)]
    pub is_favorite: bool,
}

fn default_unit() -> String {
    "pcs".into()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProductCreate {
    #[serde(flatten)]
    pub product: ProductInput,
    pub price_minor: i64,
    #[serde(default)]
    pub cost_minor: Option<i64>,
    #[serde(default)]
    pub barcodes: Vec<String>,
    #[serde(default)]
    pub opening_stock_milli: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BulkPrice {
    pub product_id: String,
    pub amount_minor: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProductUpdate {
    pub product_id: String,
    pub expected_version: i64,
    #[serde(flatten)]
    pub product: ProductInput,
}

pub(crate) const PRODUCT_ROW_SQL: &str = "SELECT p.product_id, p.sku, p.name, p.name_ar, p.category_id, c.name,
    (SELECT barcode FROM product_barcodes b WHERE b.product_id = p.product_id ORDER BY b.is_primary DESC, b.created_at LIMIT 1),
    (SELECT COUNT(*) FROM product_barcodes b WHERE b.product_id = p.product_id),
    PRICE_PLACEHOLDER,
    (SELECT avg_cost_minor FROM product_costs pc WHERE pc.product_id = p.product_id AND pc.branch_id = ?1),
    COALESCE((SELECT qty_milli FROM stock_levels s WHERE s.product_id = p.product_id AND s.branch_id = ?1), 0),
    p.reorder_point_milli, p.unit, p.track_inventory, p.allow_decimal_quantity, p.active, p.is_favorite,
    p.tax_rule_id, t.rate_bp, t.inclusive
  FROM products p
  JOIN tax_rules t ON t.tax_rule_id = p.tax_rule_id
  LEFT JOIN categories c ON c.category_id = p.category_id";

pub(crate) fn product_row_sql() -> String {
    PRODUCT_ROW_SQL.replace("PRICE_PLACEHOLDER", PRICE_SQL)
}

pub(crate) fn stock_status(track: bool, qty: i64, reorder: i64) -> &'static str {
    if !track {
        "not_tracked"
    } else if qty < 0 {
        "negative"
    } else if qty == 0 {
        "out_of_stock"
    } else if qty <= reorder {
        "low_stock"
    } else {
        "in_stock"
    }
}

pub(crate) fn map_product_row(r: &Row, show_cost: bool) -> rusqlite::Result<ProductRow> {
    let track: bool = r.get::<_, i64>(13)? == 1;
    let qty: i64 = r.get(10)?;
    let reorder: i64 = r.get(11)?;
    Ok(ProductRow {
        product_id: r.get(0)?,
        sku: r.get(1)?,
        name: r.get(2)?,
        name_ar: r.get(3)?,
        category_id: r.get(4)?,
        category_name: r.get(5)?,
        primary_barcode: r.get(6)?,
        barcode_count: r.get(7)?,
        price_minor: r.get(8)?,
        cost_minor: if show_cost { r.get(9)? } else { None },
        stock_milli: qty,
        reorder_point_milli: reorder,
        unit: r.get(12)?,
        track_inventory: track,
        allow_decimal_quantity: r.get::<_, i64>(14)? == 1,
        active: r.get::<_, i64>(15)? == 1,
        is_favorite: r.get::<_, i64>(16)? == 1,
        tax_rule_id: r.get(17)?,
        tax_rate_bp: r.get(18)?,
        tax_inclusive: r.get::<_, i64>(19)? == 1,
        stock_status: stock_status(track, qty, reorder).to_string(),
    })
}

pub(crate) fn load_product_row(c: &Connection, branch: &str, product_id: &str, show_cost: bool) -> AppResult<ProductRow> {
    let sql = format!("{} WHERE p.product_id = ?2", product_row_sql());
    c.query_row(&sql, params![branch, product_id], |r| map_product_row(r, show_cost))
        .optional()?
        .ok_or_else(|| AppError::not_found("Product"))
}

fn product_json(c: &Connection, product_id: &str) -> AppResult<serde_json::Value> {
    c.query_row(
        "SELECT sku, name, name_ar, description, category_id, tax_rule_id, unit, track_inventory,
                allow_decimal_quantity, reorder_point_milli, active, is_favorite
         FROM products WHERE product_id = ?1",
        [product_id],
        |r| {
            Ok(json!({
                "sku": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "name_ar": r.get::<_, Option<String>>(2)?,
                "description": r.get::<_, Option<String>>(3)?, "category_id": r.get::<_, Option<String>>(4)?,
                "tax_rule_id": r.get::<_, String>(5)?, "unit": r.get::<_, String>(6)?,
                "track_inventory": r.get::<_, i64>(7)? == 1, "allow_decimal_quantity": r.get::<_, i64>(8)? == 1,
                "reorder_point_milli": r.get::<_, i64>(9)?, "active": r.get::<_, i64>(10)? == 1,
                "is_favorite": r.get::<_, i64>(11)? == 1,
            }))
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("Product"))
}

pub fn current_price(c: &Connection, product_id: &str) -> AppResult<Option<i64>> {
    let sql = format!("SELECT {PRICE_SQL} FROM products p WHERE p.product_id = ?1");
    Ok(c.query_row(&sql, [product_id], |r| r.get::<_, Option<i64>>(0)).optional()?.flatten())
}

/// Returns (product_id, product_name) owning `barcode`, if any.
pub fn barcode_owner(c: &Connection, barcode: &str) -> AppResult<Option<(String, String)>> {
    Ok(c.query_row(
        "SELECT p.product_id, p.name FROM product_barcodes b JOIN products p ON p.product_id = b.product_id WHERE b.barcode = ?1",
        [barcode],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

fn validate_product_input(c: &Connection, p: &ProductInput) -> AppResult<ProductInput> {
    let name = clean(&p.name, "Product name", 160, true)?;
    let unit = clean(&p.unit, "Unit", 12, true)?;
    let tax = validate::id(&p.tax_rule_id, "Tax rule")?;
    let tax_ok: bool = c
        .query_row("SELECT active FROM tax_rules WHERE tax_rule_id=?1", [&tax], |r| r.get::<_, i64>(0))
        .optional()?
        .map(|a| a == 1)
        .unwrap_or(false);
    if !tax_ok {
        return Err(AppError::validation("Choose an active tax rule."));
    }
    let category_id = match &p.category_id {
        Some(cid) if !cid.trim().is_empty() => {
            let cid = validate::id(cid, "Category")?;
            let ok: bool = c.query_row("SELECT 1 FROM categories WHERE category_id=?1", [&cid], |_| Ok(true)).optional()?.unwrap_or(false);
            if !ok {
                return Err(AppError::validation("The selected category does not exist."));
            }
            Some(cid)
        }
        _ => None,
    };
    if p.reorder_point_milli < 0 {
        return Err(AppError::validation("Reorder point cannot be negative."));
    }
    if !p.allow_decimal_quantity && p.reorder_point_milli % crate::money::QTY_SCALE != 0 {
        return Err(AppError::validation("Reorder point must be a whole number for this product."));
    }
    Ok(ProductInput {
        sku: p.sku.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        name,
        name_ar: clean_opt(&p.name_ar, "Arabic name", 160)?,
        description: clean_opt(&p.description, "Description", 2000)?,
        category_id,
        tax_rule_id: tax,
        unit,
        track_inventory: p.track_inventory,
        allow_decimal_quantity: p.allow_decimal_quantity,
        reorder_point_milli: p.reorder_point_milli,
        is_favorite: p.is_favorite,
    })
}

fn validate_sku(s: &str) -> AppResult<String> {
    let t = s.trim();
    if t.is_empty() || t.len() > 40 || !t.chars().all(|c| c.is_ascii_graphic()) {
        return Err(AppError::validation("SKU must be 1–40 characters without spaces."));
    }
    Ok(t.to_string())
}

fn sku_taken(c: &Connection, sku: &str, except: Option<&str>) -> AppResult<Option<String>> {
    Ok(c.query_row("SELECT name FROM products WHERE sku = ?1 COLLATE NOCASE AND product_id IS NOT ?2", params![sku, except], |r| r.get(0))
        .optional()?)
}

pub(crate) fn generate_sku(c: &Connection) -> AppResult<String> {
    loop {
        let n = next_seq(c, "sku")?;
        let candidate = format!("{:06}", 100000 + n);
        if sku_taken(c, &candidate, None)?.is_none() {
            return Ok(candidate);
        }
    }
}

/// Insert a product with its first price, cost, barcodes and opening stock.
/// Shared by the product editor and the importer.
pub(crate) fn insert_product(c: &Connection, s: &Session, req: &ProductCreate, barcode_source: &str) -> AppResult<String> {
    let p = validate_product_input(c, &req.product)?;
    validate::money_non_negative(req.price_minor, "Price")?;
    let sku = match &p.sku {
        Some(sk) => {
            let sk = validate_sku(sk)?;
            if let Some(other) = sku_taken(c, &sk, None)? {
                return Err(AppError::duplicate(format!("SKU {sk} is already used by {other}.")));
            }
            sk
        }
        None => generate_sku(c)?,
    };
    let mut barcodes = Vec::new();
    for b in &req.barcodes {
        let b = validate::barcode(b)?;
        if barcodes.contains(&b) {
            continue;
        }
        if let Some((_, name)) = barcode_owner(c, &b)? {
            return Err(AppError::duplicate(format!("Barcode {b} already belongs to {name}."))
                .with_details(json!({ "barcode": b, "product_name": name })));
        }
        barcodes.push(b);
    }
    let now = time::now_str();
    let pid = new_id();
    c.execute(
        "INSERT INTO products(product_id, sku, name, name_ar, description, category_id, tax_rule_id, unit, track_inventory,
             allow_decimal_quantity, reorder_point_milli, active, is_favorite, created_at, updated_at, version)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,1,?12,?13,?13,1)",
        params![
            pid,
            sku,
            p.name,
            p.name_ar,
            p.description,
            p.category_id,
            p.tax_rule_id,
            p.unit,
            p.track_inventory as i64,
            p.allow_decimal_quantity as i64,
            p.reorder_point_milli,
            p.is_favorite as i64,
            now
        ],
    )?;
    for (i, b) in barcodes.iter().enumerate() {
        c.execute(
            "INSERT INTO product_barcodes(barcode_id, product_id, barcode, is_primary, source, created_at, created_by) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![new_id(), pid, b, (i == 0) as i64, barcode_source, now, s.user_id],
        )?;
        // A newly assigned barcode resolves any open unknown-barcode record.
        c.execute(
            "UPDATE unknown_barcodes SET status='resolved', resolved_product_id=?2, resolved_by=?3, resolved_at=?4 WHERE barcode=?1 AND status='open'",
            params![b, pid, s.user_id, now],
        )?;
    }
    c.execute(
        "INSERT INTO product_prices(price_id, product_id, branch_id, price_type, amount_minor, effective_from, reason, created_by, created_at)
         VALUES (?1,?2,NULL,'retail',?3,?4,'Initial price',?5,?4)",
        params![new_id(), pid, req.price_minor, now, s.user_id],
    )?;
    let cost = req.cost_minor.unwrap_or(0);
    validate::money_non_negative(cost, "Cost")?;
    c.execute(
        "INSERT INTO product_costs(product_id, branch_id, avg_cost_minor, last_cost_minor, updated_at) VALUES (?1,?2,?3,?3,?4)",
        params![pid, s.branch_id, cost, now],
    )?;
    if req.cost_minor.is_some() {
        c.execute(
            "INSERT INTO product_cost_history(cost_id, product_id, supplier_id, cost_minor, source, effective_at, created_by)
             VALUES (?1,?2,NULL,?3,'initial',?4,?5)",
            params![new_id(), pid, cost, now, s.user_id],
        )?;
    }
    if let Some(q) = req.opening_stock_milli {
        if q != 0 {
            if !p.track_inventory {
                return Err(AppError::validation("Opening stock requires stock tracking."));
            }
            if !p.allow_decimal_quantity && q % crate::money::QTY_SCALE != 0 {
                return Err(AppError::validation("Opening stock must be a whole number for this product."));
            }
            inventory::apply_movement(
                c,
                &inventory::Movement {
                    product_id: &pid,
                    branch_id: &s.branch_id,
                    kind: "opening",
                    qty_delta_milli: q,
                    unit_cost_minor: Some(cost),
                    source_type: "product",
                    source_id: Some(&pid),
                    reason: Some("Opening stock"),
                    user_id: Some(&s.user_id),
                    device_id: Some(&s.device_id),
                },
            )?;
        }
    }
    Ok(pid)
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryRow {
    pub category_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub sort_order: i64,
    pub active: bool,
    pub product_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaxRuleRow {
    pub tax_rule_id: String,
    pub name: String,
    pub rate_bp: i64,
    pub inclusive: bool,
    pub active: bool,
    pub effective_from: String,
    pub product_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnknownBarcodeRow {
    pub barcode: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub scan_count: i64,
    pub last_device_name: Option<String>,
    pub status: String,
    pub resolved_product_id: Option<String>,
    pub resolved_product_name: Option<String>,
}

impl AppCore {
    pub fn products_search(&self, token: &str, q: ProductQuery) -> AppResult<Page<ProductRow>> {
        let s = self.session(token)?;
        s.require("products.view")?;
        let show_cost = s.has("products.view_cost");
        let limit = validate::limit(q.limit, 50, 500);
        let offset = validate::offset(q.offset);
        self.db.read(|c| {
            let mut wheres: Vec<String> = vec![];
            let mut args: Vec<rusqlite::types::Value> = vec![s.branch_id.clone().into()];
            match q.status.as_deref().unwrap_or("active") {
                "active" => wheres.push("p.active = 1".into()),
                "archived" => wheres.push("p.active = 0".into()),
                "all" => {}
                _ => return Err(AppError::validation("Unknown status filter.")),
            }
            if let Some(cid) = q.category_id.as_ref().filter(|x| !x.is_empty()) {
                args.push(cid.clone().into());
                wheres.push(format!("p.category_id = ?{}", args.len()));
            }
            if q.favorites == Some(true) {
                wheres.push("p.is_favorite = 1".into());
            }
            let stock_expr = "COALESCE((SELECT qty_milli FROM stock_levels s WHERE s.product_id=p.product_id AND s.branch_id=?1),0)";
            match q.stock.as_deref() {
                None | Some("") | Some("any") => {}
                Some("low") => wheres.push(format!("p.track_inventory=1 AND {stock_expr} > 0 AND {stock_expr} <= p.reorder_point_milli")),
                Some("out") => wheres.push(format!("p.track_inventory=1 AND {stock_expr} = 0")),
                Some("negative") => wheres.push(format!("p.track_inventory=1 AND {stock_expr} < 0")),
                Some("attention") => wheres.push(format!("p.track_inventory=1 AND {stock_expr} <= p.reorder_point_milli")),
                Some("in_stock") => wheres.push(format!("{stock_expr} > 0")),
                _ => return Err(AppError::validation("Unknown stock filter.")),
            }
            if let Some(text) = q.q.as_ref().map(|t| t.trim()).filter(|t| !t.is_empty()) {
                let mut ors = vec![];
                args.push(text.to_string().into());
                let n = args.len();
                ors.push(format!("p.product_id IN (SELECT product_id FROM product_barcodes WHERE barcode = ?{n})"));
                ors.push(format!("p.sku = ?{n} COLLATE NOCASE"));
                args.push(format!("{}%", text.replace(['%', '_'], "")).into());
                ors.push(format!("p.sku LIKE ?{} ESCAPE '\\'", args.len()));
                if let Some(f) = validate::fts_query(text) {
                    args.push(f.into());
                    ors.push(format!("p.product_id IN (SELECT product_id FROM products_fts WHERE products_fts MATCH ?{})", args.len()));
                }
                wheres.push(format!("({})", ors.join(" OR ")));
            }
            // ?1 (branch) is always referenced so the bound parameter count matches.
            wheres.insert(0, "?1 IS NOT NULL".into());
            let where_sql = format!(" WHERE {}", wheres.join(" AND "));
            let order = match q.sort.as_deref() {
                Some("sku") => "p.sku COLLATE NOCASE",
                Some("updated") => "p.updated_at DESC",
                Some("stock") => "11 ASC",
                _ => "p.name COLLATE NOCASE",
            };
            let count_sql = format!("SELECT COUNT(*) FROM products p{where_sql}");
            let total: i64 = c.query_row(&count_sql, params_from_iter(args.iter()), |r| r.get(0))?;
            let sql = format!("{}{where_sql} ORDER BY {order} LIMIT {limit} OFFSET {offset}", product_row_sql());
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(args.iter()), |r| map_product_row(r, show_cost))?.collect::<Result<Vec<_>, _>>()?;
            Ok(Page { rows, total, limit, offset })
        })
    }

    pub fn product_get(&self, token: &str, product_id: &str) -> AppResult<ProductDetail> {
        let s = self.session(token)?;
        s.require("products.view")?;
        let show_cost = s.has("products.view_cost");
        self.db.read(|c| {
            let row = load_product_row(c, &s.branch_id, product_id, show_cost)?;
            let (description, version, created_at, updated_at, archived_at): (Option<String>, i64, String, String, Option<String>) = c
                .query_row(
                    "SELECT description, version, created_at, updated_at, archived_at FROM products WHERE product_id=?1",
                    [product_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )?;
            let mut st = c.prepare(
                "SELECT barcode_id, barcode, is_primary, source, created_at FROM product_barcodes WHERE product_id=?1 ORDER BY is_primary DESC, created_at",
            )?;
            let barcodes = st
                .query_map([product_id], |r| {
                    Ok(BarcodeRow {
                        barcode_id: r.get(0)?,
                        barcode: r.get(1)?,
                        is_primary: r.get::<_, i64>(2)? == 1,
                        source: r.get(3)?,
                        created_at: r.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut st = c.prepare(
                "SELECT pp.price_id, pp.amount_minor, pp.effective_from, pp.effective_to, pp.reason, u.display_name, pp.created_at
                 FROM product_prices pp LEFT JOIN users u ON u.user_id = pp.created_by
                 WHERE pp.product_id=?1 ORDER BY pp.effective_from DESC LIMIT 200",
            )?;
            let price_history = st
                .query_map([product_id], |r| {
                    Ok(PriceRow {
                        price_id: r.get(0)?,
                        amount_minor: r.get(1)?,
                        effective_from: r.get(2)?,
                        effective_to: r.get(3)?,
                        reason: r.get(4)?,
                        created_by_name: r.get(5)?,
                        created_at: r.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let (cost_history, avg, last) = if show_cost {
                let mut st = c.prepare(
                    "SELECT h.cost_id, h.cost_minor, h.source, s.name, h.effective_at, u.display_name
                     FROM product_cost_history h LEFT JOIN suppliers s ON s.supplier_id = h.supplier_id
                     LEFT JOIN users u ON u.user_id = h.created_by
                     WHERE h.product_id=?1 ORDER BY h.effective_at DESC LIMIT 200",
                )?;
                let rows = st
                    .query_map([product_id], |r| {
                        Ok(CostRow {
                            cost_id: r.get(0)?,
                            cost_minor: r.get(1)?,
                            source: r.get(2)?,
                            supplier_name: r.get(3)?,
                            effective_at: r.get(4)?,
                            created_by_name: r.get(5)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                let costs: Option<(i64, i64)> = c
                    .query_row(
                        "SELECT avg_cost_minor, last_cost_minor FROM product_costs WHERE product_id=?1 AND branch_id=?2",
                        params![product_id, s.branch_id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                (Some(rows), costs.map(|x| x.0), costs.map(|x| x.1))
            } else {
                (None, None, None)
            };
            Ok(ProductDetail {
                row,
                description,
                version,
                created_at,
                updated_at,
                archived_at,
                barcodes,
                price_history,
                cost_history,
                avg_cost_minor: avg,
                last_cost_minor: last,
            })
        })
    }

    pub fn product_create(&self, token: &str, req: ProductCreate) -> AppResult<ProductDetail> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        if req.cost_minor.is_some() && !s.has("products.view_cost") {
            return Err(AppError::forbidden("products.view_cost"));
        }
        let actor = self.actor(&s, None);
        let pid = self.db.write(|tx| {
            let pid = insert_product(tx, &s, &req, "manual")?;
            let after = product_json(tx, &pid)?;
            audit::record(
                tx,
                &actor,
                "product.created",
                "product",
                Some(&pid),
                None,
                Some(&json!({
                    "product": after, "price_minor": req.price_minor, "barcodes": req.barcodes,
                    "opening_stock_milli": req.opening_stock_milli
                })),
            )?;
            Ok(pid)
        })?;
        self.product_get(token, &pid)
    }

    pub fn product_update(&self, token: &str, req: ProductUpdate) -> AppResult<ProductDetail> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let pid = validate::id(&req.product_id, "Product")?;
        self.db.write(|tx| {
            let p = validate_product_input(tx, &req.product)?;
            let before = product_json(tx, &pid)?;
            let (version, old_sku, old_track): (i64, String, i64) =
                tx.query_row("SELECT version, sku, track_inventory FROM products WHERE product_id=?1", [&pid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
            if version != req.expected_version {
                return Err(AppError::conflict("This product was changed by someone else. Reload it and apply your changes again."));
            }
            let sku = match &p.sku {
                Some(sk) => validate_sku(sk)?,
                None => old_sku,
            };
            if let Some(other) = sku_taken(tx, &sku, Some(&pid))? {
                return Err(AppError::duplicate(format!("SKU {sku} is already used by {other}.")));
            }
            if old_track == 1 && !p.track_inventory {
                let qty: i64 = tx
                    .query_row(
                        "SELECT COALESCE(qty_milli,0) FROM stock_levels WHERE product_id=?1 AND branch_id=?2",
                        params![pid, s.branch_id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or(0);
                if qty != 0 {
                    return Err(AppError::validation(
                        "Stock tracking cannot be turned off while the product has stock. Adjust stock to zero first.",
                    ));
                }
            }
            tx.execute(
                "UPDATE products SET sku=?2, name=?3, name_ar=?4, description=?5, category_id=?6, tax_rule_id=?7, unit=?8,
                    track_inventory=?9, allow_decimal_quantity=?10, reorder_point_milli=?11, is_favorite=?12,
                    updated_at=?13, version=version+1
                 WHERE product_id=?1",
                params![
                    pid,
                    sku,
                    p.name,
                    p.name_ar,
                    p.description,
                    p.category_id,
                    p.tax_rule_id,
                    p.unit,
                    p.track_inventory as i64,
                    p.allow_decimal_quantity as i64,
                    p.reorder_point_milli,
                    p.is_favorite as i64,
                    time::now_str()
                ],
            )?;
            let after = product_json(tx, &pid)?;
            audit::record(tx, &actor, "product.updated", "product", Some(&pid), Some(&before), Some(&after))?;
            Ok(())
        })?;
        self.product_get(token, &pid)
    }

    pub fn product_set_active(&self, token: &str, product_id: &str, active: bool) -> AppResult<ProductDetail> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let pid = validate::id(product_id, "Product")?;
        self.db.write(|tx| {
            let now = time::now_str();
            let n = tx.execute(
                "UPDATE products SET active=?2, archived_at=CASE WHEN ?2=1 THEN NULL ELSE ?3 END, updated_at=?3, version=version+1
                 WHERE product_id=?1 AND active<>?2",
                params![pid, active as i64, now],
            )?;
            if n == 0 {
                let exists: bool =
                    tx.query_row("SELECT 1 FROM products WHERE product_id=?1", [&pid], |_| Ok(true)).optional()?.unwrap_or(false);
                if !exists {
                    return Err(AppError::not_found("Product"));
                }
                return Ok(());
            }
            audit::record(tx, &actor, if active { "product.restored" } else { "product.archived" }, "product", Some(&pid), None, None)?;
            Ok(())
        })?;
        self.product_get(token, &pid)
    }

    pub fn product_bulk_archive(&self, token: &str, product_ids: Vec<String>, active: bool) -> AppResult<i64> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        if product_ids.len() > 5000 {
            return Err(AppError::validation("Select at most 5,000 products at a time."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let now = time::now_str();
            let mut n = 0;
            for pid in &product_ids {
                let pid = validate::id(pid, "Product")?;
                n += tx.execute(
                    "UPDATE products SET active=?2, archived_at=CASE WHEN ?2=1 THEN NULL ELSE ?3 END, updated_at=?3, version=version+1
                     WHERE product_id=?1 AND active<>?2",
                    params![pid, active as i64, now],
                )? as i64;
            }
            audit::record(
                tx,
                &actor,
                if active { "product.bulk_restored" } else { "product.bulk_archived" },
                "product",
                None,
                None,
                Some(&json!({ "count": n, "product_ids": product_ids })),
            )?;
            Ok(n)
        })
    }

    pub fn barcode_add(&self, token: &str, product_id: &str, barcode: &str, make_primary: bool) -> AppResult<ProductDetail> {
        let s = self.session(token)?;
        if !s.has("products.manage") && !s.has("barcodes.resolve") {
            return Err(AppError::forbidden("products.manage"));
        }
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let pid = validate::id(product_id, "Product")?;
        let b = validate::barcode(barcode)?;
        self.db.write(|tx| {
            add_barcode(tx, &s, &pid, &b, make_primary, "manual")?;
            audit::record(
                tx,
                &actor,
                "barcode.added",
                "product",
                Some(&pid),
                None,
                Some(&json!({ "barcode": b, "primary": make_primary })),
            )?;
            Ok(())
        })?;
        self.product_get(token, &pid)
    }

    pub fn barcode_remove(&self, token: &str, barcode_id: &str) -> AppResult<ProductDetail> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let bid = validate::id(barcode_id, "Barcode")?;
        let pid = self.db.write(|tx| {
            let (pid, code, primary): (String, String, i64) = tx
                .query_row(
                    "SELECT product_id, barcode, is_primary FROM product_barcodes WHERE barcode_id=?1",
                    [&bid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Barcode"))?;
            tx.execute("DELETE FROM product_barcodes WHERE barcode_id=?1", [&bid])?;
            if primary == 1 {
                tx.execute(
                    "UPDATE product_barcodes SET is_primary=1 WHERE barcode_id = (SELECT barcode_id FROM product_barcodes WHERE product_id=?1 ORDER BY created_at LIMIT 1)",
                    [&pid],
                )?;
            }
            tx.execute("UPDATE products SET updated_at=?2, version=version+1 WHERE product_id=?1", params![pid, time::now_str()])?;
            audit::record(tx, &actor, "barcode.removed", "product", Some(&pid), Some(&json!({ "barcode": code })), None)?;
            Ok(pid)
        })?;
        self.product_get(token, &pid)
    }

    pub fn barcode_set_primary(&self, token: &str, barcode_id: &str) -> AppResult<ProductDetail> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let bid = validate::id(barcode_id, "Barcode")?;
        let pid = self.db.write(|tx| {
            let (pid, code): (String, String) = tx
                .query_row("SELECT product_id, barcode FROM product_barcodes WHERE barcode_id=?1", [&bid], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| AppError::not_found("Barcode"))?;
            tx.execute("UPDATE product_barcodes SET is_primary = (barcode_id = ?2) WHERE product_id=?1", params![pid, bid])?;
            audit::record(tx, &actor, "barcode.primary_set", "product", Some(&pid), None, Some(&json!({ "barcode": code })))?;
            Ok(pid)
        })?;
        self.product_get(token, &pid)
    }

    /// Change the selling price. The previous price row is closed, never deleted.
    pub fn product_price_update(
        &self,
        token: &str,
        product_id: &str,
        amount_minor: i64,
        reason: Option<String>,
        effective_from: Option<String>,
    ) -> AppResult<ProductDetail> {
        let s = self.session(token)?;
        s.require("prices.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let pid = validate::id(product_id, "Product")?;
        validate::money_non_negative(amount_minor, "Price")?;
        let reason = clean_opt(&reason, "Reason", 200)?;
        let eff = match effective_from {
            Some(e) if !e.trim().is_empty() => {
                let t = time::parse(&e)?;
                if t < time::now() - chrono::Duration::minutes(1) {
                    return Err(AppError::validation("A price change cannot be back-dated."));
                }
                time::fmt(t)
            }
            _ => time::now_str(),
        };
        self.db.write(|tx| {
            set_price(tx, &s, &pid, amount_minor, reason.as_deref(), &eff, &actor)?;
            Ok(())
        })?;
        self.product_get(token, &pid)
    }

    /// Manually set the standard cost (overrides the weighted average).
    pub fn product_cost_update(&self, token: &str, product_id: &str, cost_minor: i64, reason: Option<String>) -> AppResult<ProductDetail> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        s.require("products.view_cost")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let pid = validate::id(product_id, "Product")?;
        validate::money_non_negative(cost_minor, "Cost")?;
        let reason = clean_opt(&reason, "Reason", 200)?;
        self.db.write(|tx| {
            let now = time::now_str();
            let before: Option<i64> = tx
                .query_row(
                    "SELECT avg_cost_minor FROM product_costs WHERE product_id=?1 AND branch_id=?2",
                    params![pid, s.branch_id],
                    |r| r.get(0),
                )
                .optional()?;
            tx.execute(
                "INSERT INTO product_costs(product_id, branch_id, avg_cost_minor, last_cost_minor, updated_at) VALUES (?1,?2,?3,?3,?4)
                 ON CONFLICT(product_id, branch_id) DO UPDATE SET avg_cost_minor=?3, last_cost_minor=?3, updated_at=?4",
                params![pid, s.branch_id, cost_minor, now],
            )?;
            tx.execute(
                "INSERT INTO product_cost_history(cost_id, product_id, supplier_id, cost_minor, source, source_id, effective_at, created_by)
                 VALUES (?1,?2,NULL,?3,'manual',NULL,?4,?5)",
                params![new_id(), pid, cost_minor, now, s.user_id],
            )?;
            audit::record(
                tx,
                &actor,
                "product.cost_set",
                "product",
                Some(&pid),
                Some(&json!({ "avg_cost_minor": before })),
                Some(&json!({ "cost_minor": cost_minor, "reason": reason })),
            )?;
            Ok(())
        })?;
        self.product_get(token, &pid)
    }

    /// Apply many price changes atomically (previewed in the UI first).
    pub fn product_bulk_price(
        &self,
        token: &str,
        changes: Vec<BulkPrice>,
        reason: Option<String>,
        operation_id: &str,
    ) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        s.require("prices.manage")?;
        self.require_back_office_writable()?;
        if changes.is_empty() || changes.len() > 20000 {
            return Err(AppError::validation("Select between 1 and 20,000 products."));
        }
        let reason = clean_opt(&reason, "Reason", 200)?;
        let actor = self.actor(&s, None);
        let payload = json!({ "changes": changes, "reason": reason });
        self.db.write(|tx| {
            if let crate::idempotency::Check::Replay { result } = crate::idempotency::check(tx, operation_id, "prices.bulk", &payload)? {
                return Ok(result);
            }
            let hash = crate::idempotency::payload_hash("prices.bulk", &payload)?;
            let now = time::now_str();
            let mut changed = 0;
            for c in &changes {
                let pid = validate::id(&c.product_id, "Product")?;
                validate::money_non_negative(c.amount_minor, "Price")?;
                if current_price(tx, &pid)? == Some(c.amount_minor) {
                    continue;
                }
                set_price(tx, &s, &pid, c.amount_minor, reason.as_deref(), &now, &actor)?;
                changed += 1;
            }
            let result = json!({ "changed": changed });
            audit::record(tx, &actor, "price.bulk_changed", "product", None, None, Some(&json!({ "changed": changed, "reason": reason })))?;
            crate::idempotency::complete(tx, operation_id, "prices.bulk", Some(&s.user_id), Some(&s.device_id), &hash, None, &result)?;
            Ok(result)
        })
    }

    // ---- categories ----

    pub fn categories_list(&self, token: &str, include_inactive: bool) -> AppResult<Vec<CategoryRow>> {
        let s = self.session(token)?;
        if !s.has("products.view") && !s.has("pos.sell") {
            return Err(AppError::forbidden("products.view"));
        }
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT c.category_id, c.parent_id, c.name, c.sort_order, c.active,
                        (SELECT COUNT(*) FROM products p WHERE p.category_id=c.category_id AND p.active=1)
                 FROM categories c WHERE (?1 OR c.active=1) ORDER BY c.sort_order, c.name COLLATE NOCASE",
            )?;
            let rows = st
                .query_map([include_inactive], |r| {
                    Ok(CategoryRow {
                        category_id: r.get(0)?,
                        parent_id: r.get(1)?,
                        name: r.get(2)?,
                        sort_order: r.get(3)?,
                        active: r.get::<_, i64>(4)? == 1,
                        product_count: r.get(5)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn category_save(
        &self,
        token: &str,
        category_id: Option<String>,
        name: &str,
        parent_id: Option<String>,
        sort_order: i64,
    ) -> AppResult<CategoryRow> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let name = clean(name, "Category name", 80, true)?;
        let parent = match parent_id.filter(|p| !p.is_empty()) {
            Some(p) => Some(validate::id(&p, "Parent category")?),
            None => None,
        };
        let id = self.db.write(|tx| {
            let now = time::now_str();
            if let Some(p) = &parent {
                let ok: bool = tx.query_row("SELECT 1 FROM categories WHERE category_id=?1", [p], |_| Ok(true)).optional()?.unwrap_or(false);
                if !ok {
                    return Err(AppError::validation("The parent category does not exist."));
                }
                if Some(p) == category_id.as_ref() {
                    return Err(AppError::validation("A category cannot be its own parent."));
                }
            }
            let dup: Option<String> = tx
                .query_row(
                    "SELECT category_id FROM categories WHERE COALESCE(parent_id,'')=COALESCE(?1,'') AND name=?2 COLLATE NOCASE AND category_id IS NOT ?3",
                    params![parent, name, category_id],
                    |r| r.get(0),
                )
                .optional()?;
            if dup.is_some() {
                return Err(AppError::duplicate(format!("A category named {name} already exists.")));
            }
            match &category_id {
                Some(cid) => {
                    let cid = validate::id(cid, "Category")?;
                    let n = tx.execute(
                        "UPDATE categories SET name=?2, parent_id=?3, sort_order=?4, updated_at=?5 WHERE category_id=?1",
                        params![cid, name, parent, sort_order, now],
                    )?;
                    if n == 0 {
                        return Err(AppError::not_found("Category"));
                    }
                    audit::record(tx, &actor, "category.updated", "category", Some(&cid), None, Some(&json!({ "name": name })))?;
                    Ok(cid)
                }
                None => {
                    let cid = new_id();
                    tx.execute(
                        "INSERT INTO categories(category_id,parent_id,name,sort_order,active,created_at,updated_at) VALUES (?1,?2,?3,?4,1,?5,?5)",
                        params![cid, parent, name, sort_order, now],
                    )?;
                    audit::record(tx, &actor, "category.created", "category", Some(&cid), None, Some(&json!({ "name": name })))?;
                    Ok(cid)
                }
            }
        })?;
        self.categories_list(token, true)?.into_iter().find(|c| c.category_id == id).ok_or_else(|| AppError::not_found("Category"))
    }

    /// Archive a category. Active products must be reassigned first (or via `reassign_to`).
    pub fn category_archive(&self, token: &str, category_id: &str, reassign_to: Option<String>) -> AppResult<()> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let cid = validate::id(category_id, "Category")?;
        self.db.write(|tx| {
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM products WHERE category_id=?1", [&cid], |r| r.get(0))?;
            if count > 0 {
                match reassign_to.as_ref().filter(|r| !r.is_empty()) {
                    Some(target) => {
                        let target = validate::id(target, "Category")?;
                        if target == cid {
                            return Err(AppError::validation("Choose a different category to move products into."));
                        }
                        let ok: bool = tx
                            .query_row("SELECT 1 FROM categories WHERE category_id=?1 AND active=1", [&target], |_| Ok(true))
                            .optional()?
                            .unwrap_or(false);
                        if !ok {
                            return Err(AppError::validation("The target category does not exist."));
                        }
                        tx.execute(
                            "UPDATE products SET category_id=?2, updated_at=?3, version=version+1 WHERE category_id=?1",
                            params![cid, target, time::now_str()],
                        )?;
                    }
                    None => {
                        return Err(AppError::conflict(format!(
                            "{count} product(s) use this category. Choose a category to move them to first."
                        ))
                        .with_details(json!({ "product_count": count })));
                    }
                }
            }
            let children: i64 = tx.query_row("SELECT COUNT(*) FROM categories WHERE parent_id=?1 AND active=1", [&cid], |r| r.get(0))?;
            if children > 0 {
                return Err(AppError::conflict("Archive or move the sub-categories first."));
            }
            tx.execute("UPDATE categories SET active=0, updated_at=?2 WHERE category_id=?1", params![cid, time::now_str()])?;
            audit::record(
                tx,
                &actor,
                "category.archived",
                "category",
                Some(&cid),
                None,
                Some(&json!({ "moved_products": count, "to": reassign_to })),
            )?;
            Ok(())
        })
    }

    // ---- tax rules ----

    pub fn tax_rules_list(&self, token: &str) -> AppResult<Vec<TaxRuleRow>> {
        self.session(token)?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT t.tax_rule_id, t.name, t.rate_bp, t.inclusive, t.active, t.effective_from,
                        (SELECT COUNT(*) FROM products p WHERE p.tax_rule_id=t.tax_rule_id)
                 FROM tax_rules t ORDER BY t.active DESC, t.rate_bp DESC, t.name",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(TaxRuleRow {
                        tax_rule_id: r.get(0)?,
                        name: r.get(1)?,
                        rate_bp: r.get(2)?,
                        inclusive: r.get::<_, i64>(3)? == 1,
                        active: r.get::<_, i64>(4)? == 1,
                        effective_from: r.get(5)?,
                        product_count: r.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Tax rules are versioned: a rate change is a new rule; products are
    /// moved to it explicitly. Sold lines keep their snapshot rate.
    pub fn tax_rule_create(
        &self,
        token: &str,
        name: &str,
        rate_bp: i64,
        inclusive: bool,
        replace_rule_id: Option<String>,
    ) -> AppResult<Vec<TaxRuleRow>> {
        let s = self.session(token)?;
        s.require("settings.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let name = clean(name, "Tax rule name", 60, true)?;
        if !(0..=10000).contains(&rate_bp) {
            return Err(AppError::validation("Tax rate must be between 0% and 100%."));
        }
        self.db.write(|tx| {
            let now = time::now_str();
            let id = new_id();
            tx.execute(
                "INSERT INTO tax_rules(tax_rule_id,name,rate_bp,inclusive,active,effective_from,created_at) VALUES (?1,?2,?3,?4,1,?5,?5)",
                params![id, name, rate_bp, inclusive as i64, now],
            )?;
            let mut moved = 0;
            if let Some(old) = replace_rule_id.filter(|r| !r.is_empty()) {
                let old = validate::id(&old, "Tax rule")?;
                moved = tx.execute(
                    "UPDATE products SET tax_rule_id=?2, updated_at=?3, version=version+1 WHERE tax_rule_id=?1",
                    params![old, id, now],
                )?;
                tx.execute("UPDATE tax_rules SET active=0, effective_to=?2 WHERE tax_rule_id=?1", params![old, now])?;
            }
            audit::record(
                tx,
                &actor,
                "tax_rule.created",
                "tax_rule",
                Some(&id),
                None,
                Some(&json!({ "name": name, "rate_bp": rate_bp, "inclusive": inclusive, "products_moved": moved })),
            )?;
            Ok(())
        })?;
        self.tax_rules_list(token)
    }

    pub fn tax_rule_set_active(&self, token: &str, tax_rule_id: &str, active: bool) -> AppResult<Vec<TaxRuleRow>> {
        let s = self.session(token)?;
        s.require("settings.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let id = validate::id(tax_rule_id, "Tax rule")?;
        self.db.write(|tx| {
            if !active {
                let used: i64 = tx.query_row("SELECT COUNT(*) FROM products WHERE tax_rule_id=?1 AND active=1", [&id], |r| r.get(0))?;
                if used > 0 {
                    return Err(AppError::conflict(format!("{used} active product(s) use this tax rule. Move them first.")));
                }
            }
            tx.execute("UPDATE tax_rules SET active=?2 WHERE tax_rule_id=?1", params![id, active as i64])?;
            audit::record(tx, &actor, "tax_rule.active_set", "tax_rule", Some(&id), None, Some(&json!({ "active": active })))?;
            Ok(())
        })?;
        self.tax_rules_list(token)
    }

    // ---- unknown barcodes ----

    pub fn unknown_barcodes_list(&self, token: &str, status: Option<String>) -> AppResult<Vec<UnknownBarcodeRow>> {
        let s = self.session(token)?;
        if !s.has("barcodes.resolve") && !s.has("products.manage") {
            return Err(AppError::forbidden("barcodes.resolve"));
        }
        let status = status.unwrap_or_else(|| "open".into());
        if !["open", "resolved", "dismissed", "all"].contains(&status.as_str()) {
            return Err(AppError::validation("Unknown status filter."));
        }
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT u.barcode, u.first_seen_at, u.last_seen_at, u.scan_count, d.name, u.status, u.resolved_product_id, p.name
                 FROM unknown_barcodes u
                 LEFT JOIN devices d ON d.device_id = u.last_device_id
                 LEFT JOIN products p ON p.product_id = u.resolved_product_id
                 WHERE (?1 = 'all' OR u.status = ?1)
                 ORDER BY u.last_seen_at DESC LIMIT 1000",
            )?;
            let rows = st
                .query_map([&status], |r| {
                    Ok(UnknownBarcodeRow {
                        barcode: r.get(0)?,
                        first_seen_at: r.get(1)?,
                        last_seen_at: r.get(2)?,
                        scan_count: r.get(3)?,
                        last_device_name: r.get(4)?,
                        status: r.get(5)?,
                        resolved_product_id: r.get(6)?,
                        resolved_product_name: r.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn unknown_barcode_dismiss(&self, token: &str, barcode: &str) -> AppResult<()> {
        let s = self.session(token)?;
        if !s.has("barcodes.resolve") && !s.has("products.manage") {
            return Err(AppError::forbidden("barcodes.resolve"));
        }
        let actor = self.actor(&s, None);
        let b = validate::barcode(barcode)?;
        self.db.write(|tx| {
            let n = tx.execute(
                "UPDATE unknown_barcodes SET status='dismissed', resolved_by=?2, resolved_at=?3 WHERE barcode=?1 AND status='open'",
                params![b, s.user_id, time::now_str()],
            )?;
            if n == 0 {
                return Err(AppError::conflict("This barcode is no longer open. Refresh the list."));
            }
            audit::record(tx, &actor, "unknown_barcode.dismissed", "barcode", Some(&b), None, None)?;
            Ok(())
        })
    }

    /// Export the catalogue as CSV (barcodes as text, money as decimals).
    pub fn products_export_csv(&self, token: &str, include_archived: bool) -> AppResult<String> {
        let s = self.session(token)?;
        s.require("products.view")?;
        let show_cost = s.has("products.view_cost");
        self.db.read(|c| {
            let (_cur, digits) = self.currency(c)?;
            let mut w = csv::WriterBuilder::new().from_writer(vec![]);
            let mut header = vec!["sku", "name", "name_ar", "category", "barcodes", "price", "tax_rule", "unit", "track_inventory", "allow_decimal", "reorder_point", "stock", "active"];
            if show_cost {
                header.push("cost");
            }
            w.write_record(&header).map_err(|e| AppError::internal(e.to_string()))?;
            let sql = format!(
                "SELECT p.sku, p.name, COALESCE(p.name_ar,''), COALESCE(c.name,''),
                    COALESCE((SELECT group_concat(barcode, '|') FROM (SELECT barcode FROM product_barcodes b WHERE b.product_id=p.product_id ORDER BY is_primary DESC, created_at)),''),
                    {PRICE_SQL}, t.name, p.unit, p.track_inventory, p.allow_decimal_quantity, p.reorder_point_milli,
                    COALESCE((SELECT qty_milli FROM stock_levels s WHERE s.product_id=p.product_id AND s.branch_id=?1),0), p.active,
                    COALESCE((SELECT avg_cost_minor FROM product_costs pc WHERE pc.product_id=p.product_id AND pc.branch_id=?1),0)
                 FROM products p JOIN tax_rules t ON t.tax_rule_id=p.tax_rule_id LEFT JOIN categories c ON c.category_id=p.category_id
                 WHERE (?2 OR p.active=1) ORDER BY p.name COLLATE NOCASE"
            );
            let mut st = c.prepare(&sql)?;
            let mut rows = st.query(params![s.branch_id, include_archived])?;
            while let Some(r) = rows.next()? {
                let price: Option<i64> = r.get(5)?;
                let mut rec = vec![
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    price.map(|p| crate::money::format_decimal(p, digits)).unwrap_or_default(),
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    (r.get::<_, i64>(8)? == 1).to_string(),
                    (r.get::<_, i64>(9)? == 1).to_string(),
                    crate::money::format_decimal(r.get::<_, i64>(10)?, 3),
                    crate::money::format_decimal(r.get::<_, i64>(11)?, 3),
                    (r.get::<_, i64>(12)? == 1).to_string(),
                ];
                if show_cost {
                    rec.push(crate::money::format_decimal(r.get::<_, i64>(13)?, digits));
                }
                w.write_record(&rec).map_err(|e| AppError::internal(e.to_string()))?;
            }
            let bytes = w.into_inner().map_err(|e| AppError::internal(e.to_string()))?;
            String::from_utf8(bytes).map_err(|e| AppError::internal(e.to_string()))
        })
    }
}

pub(crate) fn add_barcode(c: &Connection, s: &Session, pid: &str, b: &str, make_primary: bool, source: &str) -> AppResult<()> {
    let exists: bool = c.query_row("SELECT 1 FROM products WHERE product_id=?1", [pid], |_| Ok(true)).optional()?.unwrap_or(false);
    if !exists {
        return Err(AppError::not_found("Product"));
    }
    if let Some((owner, name)) = barcode_owner(c, b)? {
        if owner == pid {
            return Err(AppError::duplicate(format!("Barcode {b} is already on this product.")));
        }
        return Err(AppError::duplicate(format!("This barcode belongs to {name}."))
            .with_details(json!({ "barcode": b, "product_id": owner, "product_name": name })));
    }
    let count: i64 = c.query_row("SELECT COUNT(*) FROM product_barcodes WHERE product_id=?1", [pid], |r| r.get(0))?;
    if count >= 50 {
        return Err(AppError::validation("A product can have at most 50 barcodes."));
    }
    let primary = make_primary || count == 0;
    if primary {
        c.execute("UPDATE product_barcodes SET is_primary=0 WHERE product_id=?1", [pid])?;
    }
    let now = time::now_str();
    c.execute(
        "INSERT INTO product_barcodes(barcode_id, product_id, barcode, is_primary, source, created_at, created_by) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![new_id(), pid, b, primary as i64, source, now, s.user_id],
    )?;
    c.execute("UPDATE products SET updated_at=?2, version=version+1 WHERE product_id=?1", params![pid, now])?;
    c.execute(
        "UPDATE unknown_barcodes SET status='resolved', resolved_product_id=?2, resolved_by=?3, resolved_at=?4 WHERE barcode=?1 AND status='open'",
        params![b, pid, s.user_id, now],
    )?;
    Ok(())
}

pub(crate) fn set_price(
    c: &Connection,
    s: &Session,
    pid: &str,
    amount_minor: i64,
    reason: Option<&str>,
    effective_from: &str,
    actor: &audit::Actor,
) -> AppResult<Option<i64>> {
    let exists: bool = c.query_row("SELECT 1 FROM products WHERE product_id=?1", [pid], |_| Ok(true)).optional()?.unwrap_or(false);
    if !exists {
        return Err(AppError::not_found("Product"));
    }
    let old = current_price(c, pid)?;
    // Close any price rows still open at the new effective time; drop future ones superseded.
    c.execute(
        "UPDATE product_prices SET effective_to=?2
         WHERE product_id=?1 AND price_type='retail' AND (effective_to IS NULL OR effective_to > ?2) AND effective_from < ?2",
        params![pid, effective_from],
    )?;
    c.execute(
        "UPDATE product_prices SET effective_to=effective_from
         WHERE product_id=?1 AND price_type='retail' AND effective_from >= ?2 AND (effective_to IS NULL OR effective_to > effective_from)",
        params![pid, effective_from],
    )?;
    c.execute(
        "INSERT INTO product_prices(price_id, product_id, branch_id, price_type, amount_minor, effective_from, reason, created_by, created_at)
         VALUES (?1,?2,NULL,'retail',?3,?4,?5,?6,?7)",
        params![new_id(), pid, amount_minor, effective_from, reason, s.user_id, time::now_str()],
    )?;
    c.execute("UPDATE products SET updated_at=?2, version=version+1 WHERE product_id=?1", params![pid, time::now_str()])?;
    audit::record(
        c,
        actor,
        "price.changed",
        "product",
        Some(pid),
        Some(&json!({ "price_minor": old })),
        Some(&json!({ "price_minor": amount_minor, "effective_from": effective_from, "reason": reason })),
    )?;
    Ok(old)
}
