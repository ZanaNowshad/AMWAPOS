//! POS carts. Carts are persisted on every mutation so a crash or restart
//! never loses the basket. Pricing is always recomputed by `pricing`.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::auth::Session;
use crate::catalog::{self, PRICE_SQL};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ids::{new_id, next_seq};
use crate::pricing::{self, LineInput, Totals};
use crate::service::AppCore;
use crate::settings;
use crate::setup::{clean, clean_opt};
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Serialize)]
pub struct CartLineView {
    pub line_id: String,
    pub line_no: i64,
    pub product_id: Option<String>,
    pub name: String,
    pub sku: Option<String>,
    pub barcode: Option<String>,
    pub unit: String,
    pub qty_milli: i64,
    pub allow_decimal_quantity: bool,
    pub catalog_unit_price_minor: i64,
    pub unit_price_minor: i64,
    pub price_overridden: bool,
    pub line_discount_bp: i64,
    pub gross_minor: i64,
    pub discount_minor: i64,
    pub tax_minor: i64,
    pub line_total_minor: i64,
    pub tax_rate_bp: i64,
    pub tax_inclusive: bool,
    pub is_custom: bool,
    pub stock_milli: Option<i64>,
    /// Reorder point of a tracked product (for the low-stock hint on the line).
    pub reorder_point_milli: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CustomerRef {
    pub customer_id: String,
    pub name: String,
    pub phone: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CartView {
    pub cart_id: Option<String>,
    pub status: String,
    pub customer: Option<CustomerRef>,
    pub lines: Vec<CartLineView>,
    pub totals: Totals,
    pub cart_discount_minor: i64,
    pub cart_discount_bp: i64,
    pub hold_number: Option<i64>,
    pub hold_note: Option<String>,
    pub version: i64,
    pub notices: Vec<String>,
    /// Loyalty (module on and a customer on the sale): balance, points
    /// redeemed on this sale and their discount value.
    pub loyalty: Option<serde_json::Value>,
}

impl CartView {
    fn empty() -> Self {
        CartView {
            cart_id: None,
            status: "active".into(),
            customer: None,
            lines: vec![],
            totals: Totals::default(),
            cart_discount_minor: 0,
            cart_discount_bp: 0,
            hold_number: None,
            hold_note: None,
            version: 0,
            notices: vec![],
            loyalty: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanResult {
    /// added | unknown | inactive
    pub outcome: String,
    pub barcode: String,
    pub product_name: Option<String>,
    pub line_id: Option<String>,
    pub cart: CartView,
}

#[derive(Debug, Clone, Serialize)]
pub struct PosSearchRow {
    pub product_id: String,
    pub sku: String,
    pub name: String,
    pub name_ar: Option<String>,
    pub category_name: Option<String>,
    pub primary_barcode: Option<String>,
    pub price_minor: Option<i64>,
    pub stock_milli: i64,
    pub track_inventory: bool,
    pub unit: String,
    pub stock_status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeldCartRow {
    pub cart_id: String,
    pub hold_number: Option<i64>,
    pub held_at: Option<String>,
    pub user_id: String,
    pub cashier_name: String,
    pub customer_name: Option<String>,
    pub item_count_milli: i64,
    pub total_minor: i64,
    pub note: Option<String>,
    pub locked: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CustomItemRequest {
    pub name: String,
    pub unit_price_minor: i64,
    #[serde(default = "one")]
    pub qty_milli: i64,
    #[serde(default)]
    pub tax_rule_id: Option<String>,
    #[serde(default)]
    pub approval_token: Option<String>,
}
fn one() -> i64 {
    1000
}

pub(crate) fn active_cart_id(c: &Connection, s: &Session) -> AppResult<Option<String>> {
    Ok(c.query_row(
        "SELECT cart_id FROM carts WHERE device_id=?1 AND user_id=?2 AND status='active' ORDER BY created_at DESC LIMIT 1",
        params![s.device_id, s.user_id],
        |r| r.get(0),
    )
    .optional()?)
}

fn ensure_cart(c: &Connection, s: &Session) -> AppResult<String> {
    if let Some(id) = active_cart_id(c, s)? {
        return Ok(id);
    }
    let id = new_id();
    let now = time::now_str();
    let shift: Option<String> = c
        .query_row(
            "SELECT shift_id FROM shifts WHERE device_id=?1 AND user_id=?2 AND status='open'",
            params![s.device_id, s.user_id],
            |r| r.get(0),
        )
        .optional()?;
    c.execute(
        "INSERT INTO carts(cart_id, device_id, branch_id, user_id, shift_id, status, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,'active',?6,?6)",
        params![id, s.device_id, s.branch_id, s.user_id, shift, now],
    )?;
    Ok(id)
}

/// Load a confirmed digital order into this till's sale. The till must not
/// have a sale with items in progress. Returns the cart id.
pub(crate) fn load_order_cart(
    c: &Connection,
    s: &Session,
    order_id: &str,
    customer_id: Option<&str>,
    lines: &[(String, i64)],
) -> AppResult<String> {
    if let Some(open) = active_cart_id(c, s)? {
        let n: i64 = c.query_row("SELECT COUNT(*) FROM cart_lines WHERE cart_id=?1", [&open], |r| r.get(0))?;
        if n > 0 {
            return Err(AppError::conflict("Finish or hold the current sale first."));
        }
    }
    let cart_id = ensure_cart(c, s)?;
    for (pid, qty) in lines {
        let p = product_for_sale(c, pid)?;
        add_product_line(c, &cart_id, &p, *qty, None)?;
    }
    c.execute(
        "UPDATE carts SET customer_id=?2, digital_order_id=?3, loyalty_points=0 WHERE cart_id=?1",
        params![cart_id, customer_id, order_id],
    )?;
    touch(c, &cart_id)?;
    Ok(cart_id)
}

fn touch(c: &Connection, cart_id: &str) -> AppResult<()> {
    c.execute("UPDATE carts SET updated_at=?2, version=version+1 WHERE cart_id=?1", params![cart_id, time::now_str()])?;
    Ok(())
}

/// Load the cart for a mutation and verify ownership and state.
fn cart_for_edit(c: &Connection, s: &Session, cart_id: Option<&str>) -> AppResult<String> {
    let id = match cart_id {
        Some(id) => validate::id(id, "Cart")?,
        None => active_cart_id(c, s)?.ok_or_else(|| AppError::conflict("There is no sale in progress."))?,
    };
    let (status, dev, user): (String, String, String) = c
        .query_row("SELECT status, device_id, user_id FROM carts WHERE cart_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?
        .ok_or_else(|| AppError::not_found("Sale"))?;
    if dev != s.device_id || user != s.user_id {
        return Err(AppError::conflict("This sale belongs to another cashier or terminal."));
    }
    if status != "active" {
        return Err(AppError::conflict(match status.as_str() {
            "completed" => "This sale has already been completed.",
            "held" => "This sale is on hold. Resume it first.",
            _ => "This sale was cancelled.",
        }));
    }
    Ok(id)
}

pub(crate) struct LineRecord {
    pub line_id: String,
    pub line_no: i64,
    pub product_id: Option<String>,
    pub name: String,
    pub sku: Option<String>,
    pub barcode: Option<String>,
    pub unit: String,
    pub qty_milli: i64,
    pub catalog_unit_price_minor: i64,
    pub unit_price_minor: i64,
    pub price_override_by: Option<String>,
    pub line_discount_minor: i64,
    pub line_discount_bp: i64,
    pub discount_approved_by: Option<String>,
    pub tax_rule_id: Option<String>,
    pub tax_rate_bp: i64,
    pub tax_inclusive: bool,
    pub is_custom: bool,
    pub allow_decimal: bool,
    pub track_inventory: bool,
    pub category_id: Option<String>,
}

pub(crate) fn load_lines(c: &Connection, cart_id: &str) -> AppResult<Vec<LineRecord>> {
    let mut st = c.prepare_cached(
        "SELECT l.line_id, l.line_no, l.product_id, l.name, l.sku, l.barcode, l.unit, l.qty_milli, l.catalog_unit_price_minor,
                l.unit_price_minor, l.price_override_by, l.line_discount_minor, l.line_discount_bp, l.discount_approved_by,
                l.tax_rule_id, l.tax_rate_bp, l.tax_inclusive, l.is_custom,
                COALESCE(p.allow_decimal_quantity, 0), COALESCE(p.track_inventory, 0), p.category_id
         FROM cart_lines l LEFT JOIN products p ON p.product_id = l.product_id
         WHERE l.cart_id=?1 ORDER BY l.line_no",
    )?;
    let rows = st
        .query_map([cart_id], |r| {
            Ok(LineRecord {
                line_id: r.get(0)?,
                line_no: r.get(1)?,
                product_id: r.get(2)?,
                name: r.get(3)?,
                sku: r.get(4)?,
                barcode: r.get(5)?,
                unit: r.get(6)?,
                qty_milli: r.get(7)?,
                catalog_unit_price_minor: r.get(8)?,
                unit_price_minor: r.get(9)?,
                price_override_by: r.get(10)?,
                line_discount_minor: r.get(11)?,
                line_discount_bp: r.get(12)?,
                discount_approved_by: r.get(13)?,
                tax_rule_id: r.get(14)?,
                tax_rate_bp: r.get(15)?,
                tax_inclusive: r.get::<_, i64>(16)? == 1,
                is_custom: r.get::<_, i64>(17)? == 1,
                allow_decimal: r.get::<_, i64>(18)? == 1,
                track_inventory: r.get::<_, i64>(19)? == 1,
                category_id: r.get(20)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn line_inputs(lines: &[LineRecord]) -> Vec<LineInput> {
    lines
        .iter()
        .map(|l| LineInput {
            unit_price_minor: l.unit_price_minor,
            qty_milli: l.qty_milli,
            line_discount_minor: l.line_discount_minor,
            line_discount_bp: l.line_discount_bp,
            tax_rate_bp: l.tax_rate_bp,
            tax_inclusive: l.tax_inclusive,
        })
        .collect()
}

pub(crate) fn cart_view(c: &Connection, s: &Session, cart_id: &str, notices: Vec<String>) -> AppResult<CartView> {
    type CartHead = (String, Option<String>, i64, i64, Option<i64>, Option<String>, i64, i64);
    let (status, customer_id, cd_minor, cd_bp, hold_no, hold_note, version, points): CartHead = c
        .query_row(
            "SELECT status, customer_id, cart_discount_minor, cart_discount_bp, hold_number, hold_note, version, loyalty_points FROM carts WHERE cart_id=?1",
            [cart_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("Sale"))?;
    let lines = load_lines(c, cart_id)?;
    let lp = crate::loyalty::price(c, &lines, cd_minor, cd_bp, customer_id.as_deref(), points)?;
    let loyalty = match &customer_id {
        Some(cid) if crate::loyalty::enabled(c)? => {
            let cfg: crate::settings::LoyaltySettings = crate::settings::get(c, crate::settings::KEY_LOYALTY)?;
            Some(serde_json::json!({ "balance": crate::loyalty::balance(c, cid)?, "points": lp.points, "discount_minor": lp.loyalty_minor,
                "earn_estimate": crate::loyalty::earned(&cfg, &lp), "redeem_minor_per_point": cfg.redeem_minor_per_point,
                "min_redeem_points": cfg.min_redeem_points }))
        }
        _ => None,
    };
    let (priced, totals) = (lp.lines, lp.totals);
    let customer = match customer_id {
        Some(cid) => c
            .query_row("SELECT customer_id, name, phone FROM customers WHERE customer_id=?1", [&cid], |r| {
                Ok(CustomerRef { customer_id: r.get(0)?, name: r.get(1)?, phone: r.get(2)? })
            })
            .optional()?,
        None => None,
    };
    let mut views = Vec::with_capacity(lines.len());
    for (l, p) in lines.iter().zip(priced) {
        let stock = match (&l.product_id, l.track_inventory) {
            (Some(pid), true) => Some(crate::inventory::current_qty(c, pid, &s.branch_id)?),
            _ => None,
        };
        let reorder: Option<i64> = match (&l.product_id, l.track_inventory) {
            (Some(pid), true) => {
                c.query_row("SELECT reorder_point_milli FROM products WHERE product_id=?1", [pid], |r| r.get(0)).optional()?
            }
            _ => None,
        };
        views.push(CartLineView {
            line_id: l.line_id.clone(),
            line_no: l.line_no,
            product_id: l.product_id.clone(),
            name: l.name.clone(),
            sku: l.sku.clone(),
            barcode: l.barcode.clone(),
            unit: l.unit.clone(),
            qty_milli: l.qty_milli,
            allow_decimal_quantity: l.allow_decimal || l.is_custom,
            catalog_unit_price_minor: l.catalog_unit_price_minor,
            unit_price_minor: l.unit_price_minor,
            price_overridden: l.price_override_by.is_some(),
            line_discount_bp: l.line_discount_bp,
            gross_minor: p.gross_minor,
            discount_minor: p.discount_minor,
            tax_minor: p.tax_minor,
            line_total_minor: p.line_total_minor,
            tax_rate_bp: l.tax_rate_bp,
            tax_inclusive: l.tax_inclusive,
            is_custom: l.is_custom,
            stock_milli: stock,
            reorder_point_milli: reorder,
        });
    }
    Ok(CartView {
        cart_id: Some(cart_id.to_string()),
        status,
        customer,
        lines: views,
        totals,
        cart_discount_minor: cd_minor,
        cart_discount_bp: cd_bp,
        hold_number: hold_no,
        hold_note,
        version,
        notices,
        loyalty,
    })
}

struct ProductForSale {
    product_id: String,
    name: String,
    sku: String,
    unit: String,
    active: bool,
    price: Option<i64>,
    tax_rule_id: String,
    rate_bp: i64,
    inclusive: bool,
    allow_decimal: bool,
}

fn product_for_sale(c: &Connection, product_id: &str) -> AppResult<ProductForSale> {
    let sql = format!(
        "SELECT p.product_id, p.name, p.sku, p.unit, p.active, {PRICE_SQL}, p.tax_rule_id, t.rate_bp, t.inclusive, p.allow_decimal_quantity
         FROM products p JOIN tax_rules t ON t.tax_rule_id=p.tax_rule_id WHERE p.product_id=?1"
    );
    c.query_row(&sql, [product_id], |r| {
        Ok(ProductForSale {
            product_id: r.get(0)?,
            name: r.get(1)?,
            sku: r.get(2)?,
            unit: r.get(3)?,
            active: r.get::<_, i64>(4)? == 1,
            price: r.get(5)?,
            tax_rule_id: r.get(6)?,
            rate_bp: r.get(7)?,
            inclusive: r.get::<_, i64>(8)? == 1,
            allow_decimal: r.get::<_, i64>(9)? == 1,
        })
    })
    .optional()?
    .ok_or_else(|| AppError::not_found("Product"))
}

/// Add a product line; merges into the last line of the same product when
/// that line has no override or discount.
fn add_product_line(c: &Connection, cart_id: &str, p: &ProductForSale, qty: i64, barcode: Option<&str>) -> AppResult<String> {
    if !p.active {
        return Err(AppError::conflict(format!("{} is archived and cannot be sold.", p.name)));
    }
    let price = p
        .price
        .ok_or_else(|| AppError::new(ErrorCode::Validation, format!("{} has no selling price. Ask a manager to set one.", p.name)))?;
    validate::qty_positive(qty, p.allow_decimal, "Quantity")?;
    let last: Option<(String, String, i64, Option<String>, i64, i64)> = c
        .query_row(
            "SELECT line_id, COALESCE(product_id,''), unit_price_minor, price_override_by, line_discount_minor, line_discount_bp
             FROM cart_lines WHERE cart_id=?1 ORDER BY line_no DESC LIMIT 1",
            [cart_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()?;
    if let Some((lid, pid, up, ov, ld, lbp)) = last {
        if pid == p.product_id && up == price && ov.is_none() && ld == 0 && lbp == 0 && !p.allow_decimal {
            c.execute("UPDATE cart_lines SET qty_milli = qty_milli + ?2 WHERE line_id=?1", params![lid, qty])?;
            touch(c, cart_id)?;
            return Ok(lid);
        }
    }
    let line_no: i64 = c.query_row("SELECT COALESCE(MAX(line_no),0)+1 FROM cart_lines WHERE cart_id=?1", [cart_id], |r| r.get(0))?;
    if line_no > 500 {
        return Err(AppError::validation("A sale can have at most 500 lines."));
    }
    let lid = new_id();
    let bc: Option<String> = match barcode {
        Some(b) => Some(b.to_string()),
        None => c
            .query_row("SELECT barcode FROM product_barcodes WHERE product_id=?1 ORDER BY is_primary DESC LIMIT 1", [&p.product_id], |r| {
                r.get(0)
            })
            .optional()?,
    };
    c.execute(
        "INSERT INTO cart_lines(line_id, cart_id, line_no, product_id, name, sku, barcode, unit, qty_milli, catalog_unit_price_minor,
             unit_price_minor, tax_rule_id, tax_rate_bp, tax_inclusive, is_custom, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10,?11,?12,?13,0,?14)",
        params![
            lid,
            cart_id,
            line_no,
            p.product_id,
            p.name,
            p.sku,
            bc,
            p.unit,
            qty,
            price,
            p.tax_rule_id,
            p.rate_bp,
            p.inclusive as i64,
            time::now_str()
        ],
    )?;
    touch(c, cart_id)?;
    Ok(lid)
}

fn record_unknown(c: &Connection, s: &Session, barcode: &str) -> AppResult<()> {
    let now = time::now_str();
    c.execute(
        "INSERT INTO unknown_barcodes(barcode, first_seen_at, last_seen_at, scan_count, last_device_id, last_user_id, status)
         VALUES (?1,?2,?2,1,?3,?4,'open')
         ON CONFLICT(barcode) DO UPDATE SET last_seen_at=?2, scan_count=scan_count+1, last_device_id=?3, last_user_id=?4,
            status=CASE WHEN status='dismissed' THEN 'open' ELSE status END",
        params![barcode, now, s.device_id, s.user_id],
    )?;
    Ok(())
}

fn require_sell(s: &Session) -> AppResult<()> {
    s.require("pos.sell")
}

impl AppCore {
    pub fn pos_get_cart(&self, token: &str) -> AppResult<CartView> {
        let s = self.session(token)?;
        require_sell(&s)?;
        self.db.read(|c| match active_cart_id(c, &s)? {
            Some(id) => cart_view(c, &s, &id, vec![]),
            None => Ok(CartView::empty()),
        })
    }

    /// Exact barcode lookup → add to cart. Unknown barcodes are recorded for
    /// management review; never fuzzy-matched.
    pub fn pos_scan(&self, token: &str, raw_barcode: &str, qty_milli: Option<i64>) -> AppResult<ScanResult> {
        let s = self.session(token)?;
        require_sell(&s)?;
        let barcode = validate::barcode(raw_barcode)?;
        let qty = qty_milli.unwrap_or(1000);
        self.db.write(|tx| {
            let owner = catalog::barcode_owner(tx, &barcode)?;
            match owner {
                None => {
                    record_unknown(tx, &s, &barcode)?;
                    let cart = match active_cart_id(tx, &s)? {
                        Some(id) => cart_view(tx, &s, &id, vec![])?,
                        None => CartView::empty(),
                    };
                    Ok(ScanResult { outcome: "unknown".into(), barcode, product_name: None, line_id: None, cart })
                }
                Some((pid, name)) => {
                    let p = product_for_sale(tx, &pid)?;
                    if !p.active {
                        let cart = match active_cart_id(tx, &s)? {
                            Some(id) => cart_view(tx, &s, &id, vec![])?,
                            None => CartView::empty(),
                        };
                        return Ok(ScanResult { outcome: "inactive".into(), barcode, product_name: Some(name), line_id: None, cart });
                    }
                    let q = if p.allow_decimal && qty_milli.is_none() { 1000 } else { qty };
                    let cart_id = ensure_cart(tx, &s)?;
                    let lid = add_product_line(tx, &cart_id, &p, q, Some(&barcode))?;
                    let cart = cart_view(tx, &s, &cart_id, vec![])?;
                    Ok(ScanResult { outcome: "added".into(), barcode, product_name: Some(name), line_id: Some(lid), cart })
                }
            }
        })
    }

    pub fn pos_search(
        &self,
        token: &str,
        q: &str,
        category_id: Option<String>,
        favorites: bool,
        limit: Option<i64>,
    ) -> AppResult<Vec<PosSearchRow>> {
        let s = self.session(token)?;
        require_sell(&s)?;
        let limit = validate::limit(limit, 40, 200);
        let text = q.trim().to_string();
        self.db.read(|c| {
            let base = format!(
                "SELECT p.product_id, p.sku, p.name, p.name_ar, c.name,
                    (SELECT barcode FROM product_barcodes b WHERE b.product_id=p.product_id ORDER BY b.is_primary DESC LIMIT 1),
                    {PRICE_SQL},
                    COALESCE((SELECT qty_milli FROM stock_levels s WHERE s.product_id=p.product_id AND s.branch_id=?1),0),
                    p.track_inventory, p.unit, p.reorder_point_milli
                 FROM products p LEFT JOIN categories c ON c.category_id=p.category_id
                 WHERE p.active=1"
            );
            let map = |r: &rusqlite::Row| -> rusqlite::Result<PosSearchRow> {
                let track = r.get::<_, i64>(8)? == 1;
                let qty: i64 = r.get(7)?;
                Ok(PosSearchRow {
                    product_id: r.get(0)?,
                    sku: r.get(1)?,
                    name: r.get(2)?,
                    name_ar: r.get(3)?,
                    category_name: r.get(4)?,
                    primary_barcode: r.get(5)?,
                    price_minor: r.get(6)?,
                    stock_milli: qty,
                    track_inventory: track,
                    unit: r.get(9)?,
                    stock_status: catalog::stock_status(track, qty, r.get(10)?).to_string(),
                })
            };
            if text.is_empty() {
                let mut sql = base.clone();
                let mut args: Vec<rusqlite::types::Value> = vec![s.branch_id.clone().into()];
                if let Some(cid) = category_id.filter(|x| !x.is_empty()) {
                    args.push(cid.into());
                    sql.push_str(" AND p.category_id=?2");
                }
                if favorites {
                    sql.push_str(" AND p.is_favorite=1");
                }
                sql.push_str(&format!(" ORDER BY p.is_favorite DESC, p.name COLLATE NOCASE LIMIT {limit}"));
                let mut st = c.prepare(&sql)?;
                let rows = st.query_map(rusqlite::params_from_iter(args.iter()), map)?.collect::<Result<Vec<_>, _>>()?;
                return Ok(rows);
            }
            let mut out: Vec<PosSearchRow> = Vec::new();
            // 1. exact barcode / SKU
            let exact_sql = format!(
                "{base} AND (p.product_id IN (SELECT product_id FROM product_barcodes WHERE barcode=?2) OR p.sku=?2 COLLATE NOCASE) LIMIT 5"
            );
            let mut st = c.prepare_cached(&exact_sql)?;
            for r in st.query_map(params![s.branch_id, text], map)? {
                out.push(r?);
            }
            // 2. full-text (name, Arabic name, sku, barcodes) ranked by bm25
            if let Some(f) = validate::fts_query(&text) {
                let fts_sql = format!(
                    "{base} AND p.product_id IN (SELECT product_id FROM products_fts WHERE products_fts MATCH ?2 ORDER BY bm25(products_fts, 0, 10.0, 5.0, 1.0) LIMIT ?3)"
                );
                let mut st = c.prepare_cached(&fts_sql)?;
                let mut ranked: Vec<PosSearchRow> = st.query_map(params![s.branch_id, f, limit * 3], map)?.collect::<Result<Vec<_>, _>>()?;
                let lower = text.to_lowercase();
                ranked.sort_by_key(|r| {
                    let n = r.name.to_lowercase();
                    (if n.starts_with(&lower) { 0 } else if n.contains(&lower) { 1 } else { 2 }, n.len())
                });
                for r in ranked {
                    if !out.iter().any(|o| o.product_id == r.product_id) {
                        out.push(r);
                    }
                }
            }
            out.truncate(limit as usize);
            Ok(out)
        })
    }

    pub fn pos_add_product(&self, token: &str, product_id: &str, qty_milli: Option<i64>) -> AppResult<CartView> {
        let s = self.session(token)?;
        require_sell(&s)?;
        let pid = validate::id(product_id, "Product")?;
        self.db.write(|tx| {
            let p = product_for_sale(tx, &pid)?;
            let cart_id = ensure_cart(tx, &s)?;
            add_product_line(tx, &cart_id, &p, qty_milli.unwrap_or(1000), None)?;
            cart_view(tx, &s, &cart_id, vec![])
        })
    }

    pub fn pos_add_custom_item(&self, token: &str, req: CustomItemRequest) -> AppResult<CartView> {
        let s = self.session(token)?;
        require_sell(&s)?;
        let pos: settings::PosSettings = self.db.read(|c| settings::get(c, settings::KEY_POS))?;
        if !pos.allow_custom_item {
            return Err(AppError::conflict("Custom items are disabled in POS settings."));
        }
        let name = clean(&req.name, "Item name", 80, true)?;
        validate::money_non_negative(req.unit_price_minor, "Price")?;
        if req.unit_price_minor == 0 {
            return Err(AppError::validation("Enter a price for the custom item."));
        }
        validate::qty_positive(req.qty_milli, true, "Quantity")?;
        let approved_by = self.authorize(&s, "pos.custom_item", req.approval_token.as_deref(), &format!("Custom item {name}"))?;
        let actor = self.actor(&s, approved_by.clone());
        self.db.write(|tx| {
            let (tax_id, rate, incl): (String, i64, i64) = match req.tax_rule_id.as_ref().filter(|t| !t.is_empty()) {
                Some(t) => tx
                    .query_row("SELECT tax_rule_id, rate_bp, inclusive FROM tax_rules WHERE tax_rule_id=?1 AND active=1", [t], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })
                    .optional()?
                    .ok_or_else(|| AppError::validation("Choose an active tax rule."))?,
                None => tx.query_row(
                    "SELECT tax_rule_id, rate_bp, inclusive FROM tax_rules WHERE active=1 ORDER BY rate_bp DESC, created_at LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?,
            };
            let cart_id = ensure_cart(tx, &s)?;
            let line_no: i64 = tx.query_row("SELECT COALESCE(MAX(line_no),0)+1 FROM cart_lines WHERE cart_id=?1", [&cart_id], |r| r.get(0))?;
            tx.execute(
                "INSERT INTO cart_lines(line_id, cart_id, line_no, product_id, name, sku, barcode, unit, qty_milli, catalog_unit_price_minor,
                     unit_price_minor, price_override_by, tax_rule_id, tax_rate_bp, tax_inclusive, is_custom, created_at)
                 VALUES (?1,?2,?3,NULL,?4,NULL,NULL,'pcs',?5,?6,?6,?7,?8,?9,?10,1,?11)",
                params![new_id(), cart_id, line_no, name, req.qty_milli, req.unit_price_minor, approved_by, tax_id, rate, incl, time::now_str()],
            )?;
            touch(tx, &cart_id)?;
            audit::record(tx, &actor, "pos.custom_item", "cart", Some(&cart_id), None,
                Some(&json!({ "name": name, "unit_price_minor": req.unit_price_minor, "qty_milli": req.qty_milli })))?;
            cart_view(tx, &s, &cart_id, vec![])
        })
    }

    pub fn pos_set_quantity(&self, token: &str, line_id: &str, qty_milli: i64, approval_token: Option<String>) -> AppResult<CartView> {
        let s = self.session(token)?;
        require_sell(&s)?;
        let lid = validate::id(line_id, "Line")?;
        // Reducing quantity is a partial removal and follows the same permission.
        let (cart_id, old_qty, dec, name) = self.db.read(|c| {
            c.query_row(
                "SELECT l.cart_id, l.qty_milli, COALESCE(p.allow_decimal_quantity, l.is_custom), l.name FROM cart_lines l
                 LEFT JOIN products p ON p.product_id=l.product_id WHERE l.line_id=?1",
                [&lid],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?)),
            )
            .optional()?
            .ok_or_else(|| AppError::not_found("Line"))
        })?;
        validate::qty_positive(qty_milli, dec == 1, "Quantity")?;
        let approved = if qty_milli < old_qty {
            self.authorize(&s, "pos.remove_line", approval_token.as_deref(), &format!("Reduce quantity of {name}"))?
        } else {
            None
        };
        self.db.write(|tx| {
            let cid = cart_for_edit(tx, &s, Some(&cart_id))?;
            tx.execute("UPDATE cart_lines SET qty_milli=?2 WHERE line_id=?1", params![lid, qty_milli])?;
            touch(tx, &cid)?;
            if qty_milli < old_qty {
                audit::record(
                    tx,
                    &self.actor(&s, approved.clone()),
                    "pos.qty_reduced",
                    "cart",
                    Some(&cid),
                    Some(&json!({ "line": name, "qty_milli": old_qty })),
                    Some(&json!({ "qty_milli": qty_milli })),
                )?;
            }
            cart_view(tx, &s, &cid, vec![])
        })
    }

    pub fn pos_remove_line(&self, token: &str, line_id: &str, approval_token: Option<String>) -> AppResult<CartView> {
        let s = self.session(token)?;
        require_sell(&s)?;
        let lid = validate::id(line_id, "Line")?;
        let name: String = self.db.read(|c| {
            c.query_row("SELECT name FROM cart_lines WHERE line_id=?1", [&lid], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Line"))
        })?;
        let approved = self.authorize(&s, "pos.remove_line", approval_token.as_deref(), &format!("Remove {name}"))?;
        let actor = self.actor(&s, approved);
        self.db.write(|tx| {
            let cart_id: String = tx.query_row("SELECT cart_id FROM cart_lines WHERE line_id=?1", [&lid], |r| r.get(0))?;
            let cid = cart_for_edit(tx, &s, Some(&cart_id))?;
            let (qty, price): (i64, i64) =
                tx.query_row("SELECT qty_milli, unit_price_minor FROM cart_lines WHERE line_id=?1", [&lid], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            tx.execute("DELETE FROM cart_lines WHERE line_id=?1", [&lid])?;
            touch(tx, &cid)?;
            audit::record(
                tx,
                &actor,
                "pos.line_removed",
                "cart",
                Some(&cid),
                Some(&json!({ "line": name, "qty_milli": qty, "unit_price_minor": price })),
                None,
            )?;
            cart_view(tx, &s, &cid, vec![])
        })
    }

    /// Line discount: fixed amount or basis points. Above the cashier limit a
    /// `pos.discount_override` approval is required.
    pub fn pos_line_discount(
        &self,
        token: &str,
        line_id: &str,
        discount_minor: i64,
        discount_bp: i64,
        approval_token: Option<String>,
    ) -> AppResult<CartView> {
        let s = self.session(token)?;
        require_sell(&s)?;
        let lid = validate::id(line_id, "Line")?;
        let pos: settings::PosSettings = self.db.read(|c| settings::get(c, settings::KEY_POS))?;
        let (cart_id, gross, name) = self.db.read(|c| {
            let (cid, price, qty, name): (String, i64, i64, String) = c
                .query_row("SELECT cart_id, unit_price_minor, qty_milli, name FROM cart_lines WHERE line_id=?1", [&lid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Line"))?;
            Ok((cid, crate::money::extend(price, qty)?, name))
        })?;
        if discount_minor < 0 || !(0..=10000).contains(&discount_bp) || (discount_minor > 0 && discount_bp > 0) {
            return Err(AppError::validation("Enter either an amount or a percentage discount."));
        }
        if discount_minor > gross {
            return Err(AppError::validation("A discount cannot exceed the line amount."));
        }
        let effective_bp = if discount_bp > 0 {
            discount_bp
        } else if gross > 0 {
            ((discount_minor as i128 * 10000) / gross as i128) as i64
        } else {
            0
        };
        let approved = if discount_minor == 0 && discount_bp == 0 {
            None
        } else {
            s.require("pos.discount").or_else(|_| {
                self.authorize(&s, "pos.discount_override", approval_token.as_deref(), &format!("Discount on {name}")).map(|_| ())
            })?;
            if effective_bp > pos.cashier_max_discount_bp {
                self.authorize(
                    &s,
                    "pos.discount_override",
                    approval_token.as_deref(),
                    &format!("{}% discount on {name}", crate::money::format_decimal(effective_bp, 2)),
                )?
            } else {
                None
            }
        };
        let actor = self.actor(&s, approved.clone());
        self.db.write(|tx| {
            let cid = cart_for_edit(tx, &s, Some(&cart_id))?;
            tx.execute(
                "UPDATE cart_lines SET line_discount_minor=?2, line_discount_bp=?3, discount_approved_by=?4 WHERE line_id=?1",
                params![lid, discount_minor, discount_bp, approved],
            )?;
            touch(tx, &cid)?;
            audit::record(
                tx,
                &actor,
                "pos.line_discount",
                "cart",
                Some(&cid),
                None,
                Some(&json!({ "line": name, "discount_minor": discount_minor, "discount_bp": discount_bp })),
            )?;
            cart_view(tx, &s, &cid, vec![])
        })
    }

    pub fn pos_cart_discount(
        &self,
        token: &str,
        discount_minor: i64,
        discount_bp: i64,
        approval_token: Option<String>,
    ) -> AppResult<CartView> {
        let s = self.session(token)?;
        require_sell(&s)?;
        let pos: settings::PosSettings = self.db.read(|c| settings::get(c, settings::KEY_POS))?;
        let (cart_id, base) = self.db.read(|c| {
            let cid = active_cart_id(c, &s)?.ok_or_else(|| AppError::conflict("There is no sale in progress."))?;
            let lines = load_lines(c, &cid)?;
            let (_, t) = pricing::price_cart(&line_inputs(&lines), 0, 0)?;
            Ok((cid, t.subtotal_minor - t.discount_minor))
        })?;
        if discount_minor < 0 || !(0..=10000).contains(&discount_bp) || (discount_minor > 0 && discount_bp > 0) {
            return Err(AppError::validation("Enter either an amount or a percentage discount."));
        }
        if discount_minor > base {
            return Err(AppError::validation("The discount cannot exceed the sale amount."));
        }
        let effective_bp = if discount_bp > 0 {
            discount_bp
        } else if base > 0 {
            ((discount_minor as i128 * 10000) / base as i128) as i64
        } else {
            0
        };
        let approved = if discount_minor == 0 && discount_bp == 0 {
            None
        } else if !s.has("pos.discount") || effective_bp > pos.cashier_max_discount_bp {
            self.authorize(
                &s,
                "pos.discount_override",
                approval_token.as_deref(),
                &format!("{}% discount on sale", crate::money::format_decimal(effective_bp, 2)),
            )?
        } else {
            None
        };
        let actor = self.actor(&s, approved.clone());
        self.db.write(|tx| {
            let cid = cart_for_edit(tx, &s, Some(&cart_id))?;
            tx.execute(
                "UPDATE carts SET cart_discount_minor=?2, cart_discount_bp=?3, cart_discount_approved_by=?4 WHERE cart_id=?1",
                params![cid, discount_minor, discount_bp, approved],
            )?;
            touch(tx, &cid)?;
            audit::record(
                tx,
                &actor,
                "pos.cart_discount",
                "cart",
                Some(&cid),
                None,
                Some(&json!({ "discount_minor": discount_minor, "discount_bp": discount_bp })),
            )?;
            cart_view(tx, &s, &cid, vec![])
        })
    }

    pub fn pos_price_override(
        &self,
        token: &str,
        line_id: &str,
        unit_price_minor: i64,
        reason: Option<String>,
        approval_token: Option<String>,
    ) -> AppResult<CartView> {
        let s = self.session(token)?;
        require_sell(&s)?;
        let lid = validate::id(line_id, "Line")?;
        validate::money_non_negative(unit_price_minor, "Price")?;
        let reason = clean_opt(&reason, "Reason", 200)?;
        let (cart_id, name, catalog_price): (String, String, i64) = self.db.read(|c| {
            c.query_row("SELECT cart_id, name, catalog_unit_price_minor FROM cart_lines WHERE line_id=?1", [&lid], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?
            .ok_or_else(|| AppError::not_found("Line"))
        })?;
        let approved = self.authorize(&s, "pos.price_override", approval_token.as_deref(), &format!("Change price of {name}"))?;
        let approver = approved.clone().unwrap_or_else(|| s.user_id.clone());
        let actor = self.actor(&s, approved);
        self.db.write(|tx| {
            let cid = cart_for_edit(tx, &s, Some(&cart_id))?;
            let override_by = if unit_price_minor == catalog_price { None } else { Some(approver.clone()) };
            tx.execute(
                "UPDATE cart_lines SET unit_price_minor=?2, price_override_by=?3 WHERE line_id=?1",
                params![lid, unit_price_minor, override_by],
            )?;
            touch(tx, &cid)?;
            audit::record(
                tx,
                &actor,
                "pos.price_override",
                "cart",
                Some(&cid),
                Some(&json!({ "line": name, "unit_price_minor": catalog_price })),
                Some(&json!({ "unit_price_minor": unit_price_minor, "reason": reason })),
            )?;
            cart_view(tx, &s, &cid, vec![])
        })
    }

    pub fn pos_set_customer(&self, token: &str, customer_id: Option<String>) -> AppResult<CartView> {
        let s = self.session(token)?;
        require_sell(&s)?;
        s.require("customers.view")?;
        self.db.write(|tx| {
            let cid = match customer_id.as_ref().filter(|x| !x.is_empty()) {
                Some(c) => {
                    let c = validate::id(c, "Customer")?;
                    let ok: bool = tx
                        .query_row("SELECT 1 FROM customers WHERE customer_id=?1 AND active=1", [&c], |_| Ok(true))
                        .optional()?
                        .unwrap_or(false);
                    if !ok {
                        return Err(AppError::not_found("Customer"));
                    }
                    Some(c)
                }
                None => None,
            };
            let cart = if cid.is_some() { ensure_cart(tx, &s)? } else { cart_for_edit(tx, &s, None)? };
            // A redemption belongs to the previous customer's points.
            tx.execute("UPDATE carts SET customer_id=?2, loyalty_points=0 WHERE cart_id=?1", params![cart, cid])?;
            touch(tx, &cart)?;
            cart_view(tx, &s, &cart, vec![])
        })
    }

    /// Redeem loyalty points on the current sale as a discount (0 clears it).
    /// Capped to the customer's balance and to what the sale can absorb; the
    /// ledger entry is written when the sale commits.
    pub fn pos_loyalty_redeem(&self, token: &str, points: i64) -> AppResult<CartView> {
        let s = self.session(token)?;
        require_sell(&s)?;
        self.require_feature("loyalty.enabled")?;
        if points < 0 {
            return Err(AppError::validation("Points cannot be negative."));
        }
        self.db.write(|tx| {
            let cart = cart_for_edit(tx, &s, None)?;
            let customer: Option<String> = tx.query_row("SELECT customer_id FROM carts WHERE cart_id=?1", [&cart], |r| r.get(0))?;
            let cid = customer.ok_or_else(|| AppError::validation("Choose the customer first."))?;
            if points > 0 {
                let cfg: crate::settings::LoyaltySettings = crate::settings::get(tx, crate::settings::KEY_LOYALTY)?;
                let bal = crate::loyalty::balance(tx, &cid)?;
                if points > bal {
                    return Err(AppError::validation(format!("The customer has {bal} points.")));
                }
                if points < cfg.min_redeem_points {
                    return Err(AppError::validation(format!("Redeem at least {} points.", cfg.min_redeem_points)));
                }
            }
            tx.execute("UPDATE carts SET loyalty_points=?2 WHERE cart_id=?1", params![cart, points])?;
            touch(tx, &cart)?;
            cart_view(tx, &s, &cart, vec![])
        })
    }

    pub fn pos_hold(&self, token: &str, note: Option<String>) -> AppResult<CartView> {
        let s = self.session(token)?;
        require_sell(&s)?;
        s.require("pos.hold")?;
        let note = clean_opt(&note, "Note", 120)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let cid = cart_for_edit(tx, &s, None)?;
            let n: i64 = tx.query_row("SELECT COUNT(*) FROM cart_lines WHERE cart_id=?1", [&cid], |r| r.get(0))?;
            if n == 0 {
                return Err(AppError::validation("The sale is empty. There is nothing to hold."));
            }
            let hold_no = next_seq(tx, &format!("hold:{}", s.device_id))?;
            tx.execute(
                "UPDATE carts SET status='held', hold_number=?2, hold_note=?3, held_at=?4 WHERE cart_id=?1",
                params![cid, hold_no, note, time::now_str()],
            )?;
            touch(tx, &cid)?;
            audit::record(tx, &actor, "pos.held", "cart", Some(&cid), None, Some(&json!({ "hold_number": hold_no, "note": note })))?;
            Ok(CartView::empty())
        })
    }

    pub fn pos_held_list(&self, token: &str) -> AppResult<Vec<HeldCartRow>> {
        let s = self.session(token)?;
        require_sell(&s)?;
        let others = s.has("pos.held_others");
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT c.cart_id, c.hold_number, c.held_at, c.user_id, u.display_name, cu.name, c.hold_note, c.cart_discount_minor, c.cart_discount_bp
                 FROM carts c JOIN users u ON u.user_id=c.user_id LEFT JOIN customers cu ON cu.customer_id=c.customer_id
                 WHERE c.status='held' AND c.device_id=?1 ORDER BY c.held_at",
            )?;
            #[allow(clippy::type_complexity)]
            let rows: Vec<(String, Option<i64>, Option<String>, String, String, Option<String>, Option<String>, i64, i64)> = st
                .query_map([&s.device_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut out = vec![];
            for (cid, hn, ha, uid, uname, cust, note, dm, dbp) in rows {
                let lines = load_lines(c, &cid)?;
                let (_, t) = pricing::price_cart(&line_inputs(&lines), dm, dbp)?;
                out.push(HeldCartRow {
                    locked: uid != s.user_id && !others,
                    cart_id: cid,
                    hold_number: hn,
                    held_at: ha,
                    user_id: uid,
                    cashier_name: uname,
                    customer_name: cust,
                    item_count_milli: t.item_count_milli,
                    total_minor: t.total_minor,
                    note,
                });
            }
            Ok(out)
        })
    }

    /// Resume a held sale. Lines without a price override are re-priced at
    /// the current catalogue price and every change is reported.
    pub fn pos_restore(&self, token: &str, cart_id: &str) -> AppResult<CartView> {
        let s = self.session(token)?;
        require_sell(&s)?;
        s.require("pos.hold")?;
        let cid = validate::id(cart_id, "Held sale")?;
        let actor = self.actor(&s, None);
        let (cur, digits) = self.db.read(|c| self.currency(c))?;
        self.db.write(|tx| {
            if let Some(active) = active_cart_id(tx, &s)? {
                let n: i64 = tx.query_row("SELECT COUNT(*) FROM cart_lines WHERE cart_id=?1", [&active], |r| r.get(0))?;
                if n > 0 {
                    return Err(AppError::conflict("Finish or hold the current sale before resuming another."));
                }
                tx.execute("UPDATE carts SET status='cancelled', updated_at=?2 WHERE cart_id=?1", params![active, time::now_str()])?;
            }
            let (status, dev, uid): (String, String, String) = tx
                .query_row("SELECT status, device_id, user_id FROM carts WHERE cart_id=?1", [&cid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Held sale"))?;
            if status != "held" || dev != s.device_id {
                return Err(AppError::conflict("This held sale is no longer available."));
            }
            if uid != s.user_id && !s.has("pos.held_others") {
                return Err(AppError::forbidden("pos.held_others"));
            }
            let mut notices = vec![];
            for l in load_lines(tx, &cid)? {
                if l.is_custom {
                    continue;
                }
                let Some(pid) = &l.product_id else { continue };
                let p = product_for_sale(tx, pid)?;
                if !p.active {
                    tx.execute("DELETE FROM cart_lines WHERE line_id=?1", [&l.line_id])?;
                    notices.push(format!("{} was removed: it is no longer sold.", l.name));
                    continue;
                }
                let Some(now_price) = p.price else { continue };
                if now_price != l.catalog_unit_price_minor {
                    if l.price_override_by.is_none() {
                        tx.execute(
                            "UPDATE cart_lines SET catalog_unit_price_minor=?2, unit_price_minor=?2 WHERE line_id=?1",
                            params![l.line_id, now_price],
                        )?;
                        notices.push(format!(
                            "{}: price changed from {} to {}.",
                            l.name,
                            crate::money::format_money(l.catalog_unit_price_minor, &cur, digits),
                            crate::money::format_money(now_price, &cur, digits)
                        ));
                    } else {
                        tx.execute("UPDATE cart_lines SET catalog_unit_price_minor=?2 WHERE line_id=?1", params![l.line_id, now_price])?;
                        notices.push(format!("{}: catalogue price changed; the approved override was kept.", l.name));
                    }
                }
                if p.rate_bp != l.tax_rate_bp || p.inclusive != l.tax_inclusive {
                    tx.execute(
                        "UPDATE cart_lines SET tax_rule_id=?2, tax_rate_bp=?3, tax_inclusive=?4 WHERE line_id=?1",
                        params![l.line_id, p.tax_rule_id, p.rate_bp, p.inclusive as i64],
                    )?;
                    notices.push(format!("{}: tax rate updated.", l.name));
                }
            }
            let shift: Option<String> = tx
                .query_row(
                    "SELECT shift_id FROM shifts WHERE device_id=?1 AND user_id=?2 AND status='open'",
                    params![s.device_id, s.user_id],
                    |r| r.get(0),
                )
                .optional()?;
            tx.execute(
                "UPDATE carts SET status='active', user_id=?2, shift_id=?3, updated_at=?4, version=version+1 WHERE cart_id=?1",
                params![cid, s.user_id, shift, time::now_str()],
            )?;
            audit::record(tx, &actor, "pos.restored", "cart", Some(&cid), None, Some(&json!({ "notices": notices })))?;
            cart_view(tx, &s, &cid, notices)
        })
    }

    pub fn pos_held_delete(&self, token: &str, cart_id: &str, approval_token: Option<String>) -> AppResult<()> {
        let s = self.session(token)?;
        require_sell(&s)?;
        let cid = validate::id(cart_id, "Held sale")?;
        let uid: String = self.db.read(|c| {
            c.query_row("SELECT user_id FROM carts WHERE cart_id=?1 AND status='held'", [&cid], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Held sale"))
        })?;
        let approved = if uid != s.user_id {
            self.authorize(&s, "pos.held_others", approval_token.as_deref(), "Delete another cashier's held sale")?
        } else {
            self.authorize(&s, "pos.cancel_sale", approval_token.as_deref(), "Delete held sale")?
        };
        let actor = self.actor(&s, approved);
        self.db.write(|tx| {
            let n = tx.execute(
                "UPDATE carts SET status='cancelled', updated_at=?2 WHERE cart_id=?1 AND status='held'",
                params![cid, time::now_str()],
            )?;
            if n == 0 {
                return Err(AppError::conflict("This held sale is no longer available."));
            }
            audit::record(tx, &actor, "pos.held_deleted", "cart", Some(&cid), None, None)?;
            Ok(())
        })
    }

    pub fn pos_cancel_sale(&self, token: &str, approval_token: Option<String>) -> AppResult<CartView> {
        let s = self.session(token)?;
        require_sell(&s)?;
        let (cid, n, total) = self.db.read(|c| {
            let cid = active_cart_id(c, &s)?.ok_or_else(|| AppError::conflict("There is no sale in progress."))?;
            let lines = load_lines(c, &cid)?;
            let (_, t) = pricing::price_cart(&line_inputs(&lines), 0, 0)?;
            Ok((cid, lines.len(), t.total_minor))
        })?;
        let approved =
            if n > 0 { self.authorize(&s, "pos.cancel_sale", approval_token.as_deref(), "Cancel the current sale")? } else { None };
        let actor = self.actor(&s, approved);
        self.db.write(|tx| {
            let cid = cart_for_edit(tx, &s, Some(&cid))?;
            tx.execute("UPDATE carts SET status='cancelled', updated_at=?2 WHERE cart_id=?1", params![cid, time::now_str()])?;
            if n > 0 {
                audit::record(
                    tx,
                    &actor,
                    "pos.sale_cancelled",
                    "cart",
                    Some(&cid),
                    None,
                    Some(&json!({ "lines": n, "total_minor": total })),
                )?;
            }
            Ok(CartView::empty())
        })
    }
}
