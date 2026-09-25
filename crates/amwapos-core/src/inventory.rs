//! Inventory: append-only movement ledger (authoritative), cached stock
//! levels, weighted-average costing, adjustments, receiving and stocktakes.

use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::auth::Session;
use crate::catalog::Page;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::idempotency::{self, Check};
use crate::ids::{new_id, next_seq};
use crate::money::{self, QTY_SCALE};
use crate::service::AppCore;
use crate::settings;
use crate::setup::{clean, clean_opt};
use crate::time;
use crate::validate;

pub struct Movement<'a> {
    pub product_id: &'a str,
    pub branch_id: &'a str,
    pub kind: &'a str,
    pub qty_delta_milli: i64,
    pub unit_cost_minor: Option<i64>,
    pub source_type: &'a str,
    pub source_id: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub user_id: Option<&'a str>,
    pub device_id: Option<&'a str>,
}

/// Append a movement and update the cached stock level. Returns the new balance.
pub fn apply_movement(c: &Connection, m: &Movement) -> AppResult<i64> {
    if m.qty_delta_milli == 0 {
        return current_qty(c, m.product_id, m.branch_id);
    }
    let now = time::now_str();
    c.execute(
        "INSERT INTO stock_levels(product_id, branch_id, qty_milli, last_movement_at, updated_at) VALUES (?1,?2,?3,?4,?4)
         ON CONFLICT(product_id, branch_id) DO UPDATE SET qty_milli = qty_milli + ?3, last_movement_at=?4, updated_at=?4",
        params![m.product_id, m.branch_id, m.qty_delta_milli, now],
    )?;
    let balance = current_qty(c, m.product_id, m.branch_id)?;
    c.execute(
        "INSERT INTO stock_movements(movement_id, product_id, branch_id, type, qty_delta_milli, unit_cost_minor, balance_after_milli,
             source_type, source_id, reason, user_id, device_id, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            new_id(),
            m.product_id,
            m.branch_id,
            m.kind,
            m.qty_delta_milli,
            m.unit_cost_minor,
            balance,
            m.source_type,
            m.source_id,
            m.reason,
            m.user_id,
            m.device_id,
            now
        ],
    )?;
    Ok(balance)
}

pub fn current_qty(c: &Connection, product_id: &str, branch_id: &str) -> AppResult<i64> {
    Ok(c.query_row("SELECT qty_milli FROM stock_levels WHERE product_id=?1 AND branch_id=?2", params![product_id, branch_id], |r| r.get(0))
        .optional()?
        .unwrap_or(0))
}

pub fn avg_cost(c: &Connection, product_id: &str, branch_id: &str) -> AppResult<i64> {
    Ok(c.query_row("SELECT avg_cost_minor FROM product_costs WHERE product_id=?1 AND branch_id=?2", params![product_id, branch_id], |r| {
        r.get(0)
    })
    .optional()?
    .unwrap_or(0))
}

/// Moving weighted-average cost. Call BEFORE applying the receipt movement.
/// Negative or zero on-hand stock does not dilute the new cost.
pub fn update_avg_cost(c: &Connection, product_id: &str, branch_id: &str, qty_milli: i64, unit_cost_minor: i64) -> AppResult<i64> {
    let on_hand = current_qty(c, product_id, branch_id)?.max(0);
    let old = avg_cost(c, product_id, branch_id)?;
    let new_avg = if on_hand == 0 {
        unit_cost_minor
    } else {
        let num = on_hand as i128 * old as i128 + qty_milli as i128 * unit_cost_minor as i128;
        let den = on_hand as i128 + qty_milli as i128;
        money::div_round(num, den) as i64
    };
    c.execute(
        "INSERT INTO product_costs(product_id, branch_id, avg_cost_minor, last_cost_minor, updated_at) VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(product_id, branch_id) DO UPDATE SET avg_cost_minor=?3, last_cost_minor=?4, updated_at=?5",
        params![product_id, branch_id, new_avg, unit_cost_minor, time::now_str()],
    )?;
    Ok(new_avg)
}

#[derive(Debug, Clone, Serialize)]
pub struct MovementRow {
    pub movement_id: String,
    pub created_at: String,
    pub product_id: String,
    pub product_name: String,
    pub sku: String,
    pub kind: String,
    pub qty_delta_milli: i64,
    pub balance_after_milli: i64,
    pub unit_cost_minor: Option<i64>,
    pub source_type: String,
    pub source_id: Option<String>,
    pub source_ref: Option<String>,
    pub reason: Option<String>,
    pub user_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MovementQuery {
    #[serde(default)]
    pub product_id: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AdjustRequest {
    pub product_id: String,
    /// increase | decrease | set
    pub mode: String,
    pub qty_milli: i64,
    pub reason: String,
    pub operation_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReceiveLine {
    pub product_id: String,
    pub qty_milli: i64,
    pub unit_cost_minor: i64,
    #[serde(default)]
    pub po_item_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReceiveRequest {
    #[serde(default)]
    pub supplier_id: Option<String>,
    #[serde(default)]
    pub reference: Option<String>,
    pub lines: Vec<ReceiveLine>,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StocktakeRow {
    pub stocktake_id: String,
    pub stocktake_number: String,
    pub name: String,
    pub scope_type: String,
    pub status: String,
    pub blind: bool,
    pub created_at: String,
    pub created_by_name: Option<String>,
    pub line_count: i64,
    pub counted_count: i64,
    pub variance_lines: i64,
    pub finalized_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StocktakeLine {
    pub product_id: String,
    pub sku: String,
    pub name: String,
    pub primary_barcode: Option<String>,
    pub unit: String,
    pub allow_decimal_quantity: bool,
    pub expected_qty_milli: Option<i64>,
    pub system_qty_at_count_milli: Option<i64>,
    pub counted_qty_milli: Option<i64>,
    pub variance_milli: Option<i64>,
    pub unit_cost_minor: Option<i64>,
    pub counted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StocktakeDetail {
    #[serde(flatten)]
    pub header: StocktakeRow,
    pub lines: Vec<StocktakeLine>,
    pub expected_value_minor: Option<i64>,
    pub counted_value_minor: Option<i64>,
    pub variance_value_minor: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StocktakeCreate {
    pub name: String,
    /// all | category | products
    pub scope_type: String,
    #[serde(default)]
    pub category_id: Option<String>,
    #[serde(default)]
    pub product_ids: Vec<String>,
    #[serde(default)]
    pub blind: Option<bool>,
}

/// Validate and apply goods received: WAC cost update, cost history and
/// `receive` movements. Shared by direct receiving and PO receiving.
pub(crate) fn receive_lines(
    c: &Connection,
    s: &Session,
    receipt_id: &str,
    supplier_id: Option<&str>,
    lines: &[ReceiveLine],
) -> AppResult<i64> {
    if lines.is_empty() {
        return Err(AppError::validation("Add at least one item to receive."));
    }
    if lines.len() > 2000 {
        return Err(AppError::validation("Receive at most 2,000 lines at a time."));
    }
    let mut total = 0i64;
    let now = time::now_str();
    let costing = crate::settings::get::<crate::settings::InventorySettings>(c, crate::settings::KEY_INVENTORY)?.costing_method;
    for l in lines {
        let pid = validate::id(&l.product_id, "Product")?;
        let (track, dec, name): (i64, i64, String) = c
            .query_row("SELECT track_inventory, allow_decimal_quantity, name FROM products WHERE product_id=?1", [&pid], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?
            .ok_or_else(|| AppError::not_found("Product"))?;
        validate::qty_positive(l.qty_milli, dec == 1, &format!("Quantity for {name}"))?;
        validate::money_non_negative(l.unit_cost_minor, &format!("Cost for {name}"))?;
        let line_cost = money::extend(l.unit_cost_minor, l.qty_milli)?;
        total += line_cost;
        c.execute(
            "INSERT INTO goods_receipt_items(receipt_item_id, receipt_id, po_item_id, product_id, qty_milli, unit_cost_minor) VALUES (?1,?2,?3,?4,?5,?6)",
            params![new_id(), receipt_id, l.po_item_id, pid, l.qty_milli, l.unit_cost_minor],
        )?;
        c.execute(
            "INSERT INTO product_cost_history(cost_id, product_id, supplier_id, cost_minor, source, source_id, effective_at, created_by)
             VALUES (?1,?2,?3,?4,'receiving',?5,?6,?7)",
            params![new_id(), pid, supplier_id, l.unit_cost_minor, receipt_id, now, s.user_id],
        )?;
        // Cost changes only under the configured costing method, and every
        // change is audited (old → new).
        if costing == "weighted_average" {
            let old = avg_cost(c, &pid, &s.branch_id)?;
            let new = update_avg_cost(c, &pid, &s.branch_id, l.qty_milli, l.unit_cost_minor)?;
            if new != old {
                let actor = crate::audit::Actor {
                    user_id: Some(s.user_id.clone()),
                    device_id: Some(s.device_id.clone()),
                    branch_id: Some(s.branch_id.clone()),
                    approved_by: None,
                };
                crate::audit::record(
                    c,
                    &actor,
                    "cost.updated",
                    "product",
                    Some(&pid),
                    Some(&serde_json::json!({ "avg_cost_minor": old })),
                    Some(&serde_json::json!({ "avg_cost_minor": new, "source": "receiving", "receipt_id": receipt_id })),
                )?;
            }
        }
        if track == 1 {
            apply_movement(
                c,
                &Movement {
                    product_id: &pid,
                    branch_id: &s.branch_id,
                    kind: "receive",
                    qty_delta_milli: l.qty_milli,
                    unit_cost_minor: Some(l.unit_cost_minor),
                    source_type: "goods_receipt",
                    source_id: Some(receipt_id),
                    reason: None,
                    user_id: Some(&s.user_id),
                    device_id: Some(&s.device_id),
                },
            )?;
        }
    }
    Ok(total)
}

fn stocktake_header(c: &Connection, id: &str) -> AppResult<StocktakeRow> {
    c.query_row(
        "SELECT t.stocktake_id, t.stocktake_number, t.name, t.scope_type, t.status, t.blind, t.created_at, u.display_name,
                (SELECT COUNT(*) FROM stocktake_lines l WHERE l.stocktake_id=t.stocktake_id),
                (SELECT COUNT(*) FROM stocktake_lines l WHERE l.stocktake_id=t.stocktake_id AND l.counted_qty_milli IS NOT NULL),
                (SELECT COUNT(*) FROM stocktake_lines l WHERE l.stocktake_id=t.stocktake_id AND l.counted_qty_milli IS NOT NULL
                    AND l.counted_qty_milli <> l.system_qty_at_count_milli),
                t.finalized_at
         FROM stocktakes t LEFT JOIN users u ON u.user_id=t.created_by WHERE t.stocktake_id=?1",
        [id],
        |r| {
            Ok(StocktakeRow {
                stocktake_id: r.get(0)?,
                stocktake_number: r.get(1)?,
                name: r.get(2)?,
                scope_type: r.get(3)?,
                status: r.get(4)?,
                blind: r.get::<_, i64>(5)? == 1,
                created_at: r.get(6)?,
                created_by_name: r.get(7)?,
                line_count: r.get(8)?,
                counted_count: r.get(9)?,
                variance_lines: r.get(10)?,
                finalized_at: r.get(11)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("Stocktake"))
}

impl AppCore {
    pub fn inventory_movements(&self, token: &str, q: MovementQuery) -> AppResult<Page<MovementRow>> {
        let s = self.session(token)?;
        s.require("inventory.view")?;
        let show_cost = s.has("products.view_cost");
        let limit = validate::limit(q.limit, 100, 1000);
        let offset = validate::offset(q.offset);
        self.db.read(|c| {
            let tz = self.store_timezone(c)?;
            let mut wheres = vec!["m.branch_id = ?1".to_string()];
            let mut args: Vec<rusqlite::types::Value> = vec![s.branch_id.clone().into()];
            if let Some(p) = q.product_id.as_ref().filter(|x| !x.is_empty()) {
                args.push(validate::id(p, "Product")?.into());
                wheres.push(format!("m.product_id = ?{}", args.len()));
            }
            if let Some(k) = q.kind.as_ref().filter(|x| !x.is_empty()) {
                args.push(k.clone().into());
                wheres.push(format!("m.type = ?{}", args.len()));
            }
            if let Some(u) = q.user_id.as_ref().filter(|x| !x.is_empty()) {
                args.push(u.clone().into());
                wheres.push(format!("m.user_id = ?{}", args.len()));
            }
            if q.from.is_some() || q.to.is_some() {
                let from = q.from.clone().unwrap_or_else(|| "2000-01-01".into());
                let to = q.to.clone().unwrap_or_else(|| "2999-12-31".into());
                let (a, b) = time::local_date_range_utc(&from, &to, &tz)?;
                args.push(a.into());
                wheres.push(format!("m.created_at >= ?{}", args.len()));
                args.push(b.into());
                wheres.push(format!("m.created_at < ?{}", args.len()));
            }
            let w = wheres.join(" AND ");
            let total: i64 = c.query_row(&format!("SELECT COUNT(*) FROM stock_movements m WHERE {w}"), params_from_iter(args.iter()), |r| r.get(0))?;
            let sql = format!(
                "SELECT m.movement_id, m.created_at, m.product_id, p.name, p.sku, m.type, m.qty_delta_milli, m.balance_after_milli,
                        m.unit_cost_minor, m.source_type, m.source_id, m.reason, u.display_name,
                        CASE m.source_type
                          WHEN 'sale' THEN (SELECT receipt_number FROM sales WHERE sale_id=m.source_id)
                          WHEN 'refund' THEN (SELECT refund_receipt_number FROM refunds WHERE refund_id=m.source_id)
                          WHEN 'stocktake' THEN (SELECT stocktake_number FROM stocktakes WHERE stocktake_id=m.source_id)
                          WHEN 'goods_receipt' THEN (SELECT COALESCE((SELECT po_number FROM purchase_orders po WHERE po.po_id=g.po_id), g.reference) FROM goods_receipts g WHERE g.receipt_id=m.source_id)
                          ELSE NULL END
                 FROM stock_movements m JOIN products p ON p.product_id=m.product_id
                 LEFT JOIN users u ON u.user_id=m.user_id
                 WHERE {w} ORDER BY m.created_at DESC, m.movement_id DESC LIMIT {limit} OFFSET {offset}"
            );
            let mut st = c.prepare(&sql)?;
            let rows = st
                .query_map(params_from_iter(args.iter()), |r| {
                    Ok(MovementRow {
                        movement_id: r.get(0)?,
                        created_at: r.get(1)?,
                        product_id: r.get(2)?,
                        product_name: r.get(3)?,
                        sku: r.get(4)?,
                        kind: r.get(5)?,
                        qty_delta_milli: r.get(6)?,
                        balance_after_milli: r.get(7)?,
                        unit_cost_minor: if show_cost { r.get(8)? } else { None },
                        source_type: r.get(9)?,
                        source_id: r.get(10)?,
                        reason: r.get(11)?,
                        user_name: r.get(12)?,
                        source_ref: r.get(13)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Page { rows, total, limit, offset })
        })
    }

    pub fn inventory_adjust(&self, token: &str, req: AdjustRequest) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        s.require("inventory.adjust")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let pid = validate::id(&req.product_id, "Product")?;
        let inv: settings::InventorySettings = self.db.read(|c| settings::get(c, settings::KEY_INVENTORY))?;
        let reason = clean(&req.reason, "Reason", 200, inv.require_adjust_reason)?;
        self.db.write(|tx| {
            if let Check::Replay { result } = idempotency::check(tx, &req.operation_id, "inventory.adjust", &req)? {
                return Ok(result);
            }
            let hash = idempotency::payload_hash("inventory.adjust", &req)?;
            let (track, dec, name): (i64, i64, String) = tx
                .query_row("SELECT track_inventory, allow_decimal_quantity, name FROM products WHERE product_id=?1", [&pid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Product"))?;
            if track == 0 {
                return Err(AppError::validation(format!("{name} does not track stock.")));
            }
            let before = current_qty(tx, &pid, &s.branch_id)?;
            let delta = match req.mode.as_str() {
                "increase" => validate::qty_positive(req.qty_milli, dec == 1, "Quantity")?,
                "decrease" => -validate::qty_positive(req.qty_milli, dec == 1, "Quantity")?,
                "set" => {
                    if req.qty_milli < 0 {
                        return Err(AppError::validation("Counted quantity cannot be negative."));
                    }
                    if dec == 0 && req.qty_milli % QTY_SCALE != 0 {
                        return Err(AppError::validation("Quantity must be a whole number for this product."));
                    }
                    req.qty_milli - before
                }
                _ => return Err(AppError::validation("Adjustment type must be increase, decrease or set.")),
            };
            if delta == 0 {
                return Err(AppError::validation("The stock is already at that quantity."));
            }
            let cost = avg_cost(tx, &pid, &s.branch_id)?;
            let after = apply_movement(
                tx,
                &Movement {
                    product_id: &pid,
                    branch_id: &s.branch_id,
                    kind: "adjust",
                    qty_delta_milli: delta,
                    unit_cost_minor: Some(cost),
                    source_type: "adjustment",
                    source_id: Some(&req.operation_id),
                    reason: Some(&reason),
                    user_id: Some(&s.user_id),
                    device_id: Some(&s.device_id),
                },
            )?;
            let result = json!({ "product_id": pid, "before_milli": before, "after_milli": after, "delta_milli": delta });
            audit::record(
                tx,
                &actor,
                "stock.adjusted",
                "product",
                Some(&pid),
                Some(&json!({ "qty_milli": before })),
                Some(&json!({ "qty_milli": after, "delta_milli": delta, "reason": reason })),
            )?;
            idempotency::complete(
                tx,
                &req.operation_id,
                "inventory.adjust",
                Some(&s.user_id),
                Some(&s.device_id),
                &hash,
                Some(&pid),
                &result,
            )?;
            Ok(result)
        })
    }

    /// Receive goods without a purchase order.
    pub fn inventory_receive(&self, token: &str, req: ReceiveRequest) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        s.require("inventory.receive")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let reference = clean_opt(&req.reference, "Reference", 80)?;
        self.db.write(|tx| {
            if let Check::Replay { result } = idempotency::check(tx, &req.operation_id, "inventory.receive", &req)? {
                return Ok(result);
            }
            let hash = idempotency::payload_hash("inventory.receive", &req)?;
            let supplier = match req.supplier_id.as_ref().filter(|x| !x.is_empty()) {
                Some(sid) => {
                    let sid = validate::id(sid, "Supplier")?;
                    let ok: bool = tx.query_row("SELECT 1 FROM suppliers WHERE supplier_id=?1", [&sid], |_| Ok(true)).optional()?.unwrap_or(false);
                    if !ok {
                        return Err(AppError::not_found("Supplier"));
                    }
                    Some(sid)
                }
                None => None,
            };
            let rid = new_id();
            tx.execute(
                "INSERT INTO goods_receipts(receipt_id, po_id, supplier_id, branch_id, reference, total_cost_minor, operation_id, user_id, device_id, created_at)
                 VALUES (?1,NULL,?2,?3,?4,0,?5,?6,?7,?8)",
                params![rid, supplier, s.branch_id, reference, req.operation_id, s.user_id, s.device_id, time::now_str()],
            )?;
            let total = receive_lines(tx, &s, &rid, supplier.as_deref(), &req.lines)?;
            // goods_receipts is not append-only protected; total is filled after lines are validated.
            tx.execute("UPDATE goods_receipts SET total_cost_minor=?2 WHERE receipt_id=?1", params![rid, total])?;
            let result = json!({ "receipt_id": rid, "total_cost_minor": total, "lines": req.lines.len() });
            audit::record(tx, &actor, "stock.received", "goods_receipt", Some(&rid), None, Some(&result))?;
            idempotency::complete(tx, &req.operation_id, "inventory.receive", Some(&s.user_id), Some(&s.device_id), &hash, Some(&rid), &result)?;
            Ok(result)
        })
    }

    // ---- stocktake ----

    pub fn stocktakes_list(&self, token: &str) -> AppResult<Vec<StocktakeRow>> {
        let s = self.session(token)?;
        s.require("stocktake.manage")?;
        self.db.read(|c| {
            let mut st = c.prepare("SELECT stocktake_id FROM stocktakes ORDER BY created_at DESC LIMIT 200")?;
            let ids = st.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            ids.iter().map(|id| stocktake_header(c, id)).collect()
        })
    }

    pub fn stocktake_create(&self, token: &str, req: StocktakeCreate) -> AppResult<StocktakeDetail> {
        let s = self.session(token)?;
        s.require("stocktake.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let name = clean(&req.name, "Stocktake name", 80, true)?;
        let inv: settings::InventorySettings = self.db.read(|c| settings::get(c, settings::KEY_INVENTORY))?;
        let blind = req.blind.unwrap_or(inv.stocktake_blind_default);
        let id = self.db.write(|tx| {
            let open: i64 = tx.query_row("SELECT COUNT(*) FROM stocktakes WHERE status IN ('counting','review')", [], |r| r.get(0))?;
            if open >= 5 {
                return Err(AppError::conflict("Finish or cancel existing stocktakes before starting another."));
            }
            let id = new_id();
            let number = format!("ST-{:05}", next_seq(tx, "stocktake")?);
            let now = time::now_str();
            let (scope_ref, product_sql, args): (Option<String>, String, Vec<rusqlite::types::Value>) = match req.scope_type.as_str() {
                "all" => (None, "SELECT product_id FROM products WHERE active=1 AND track_inventory=1".into(), vec![]),
                "category" => {
                    let cid = validate::id(req.category_id.as_deref().unwrap_or(""), "Category")?;
                    (
                        Some(cid.clone()),
                        "SELECT product_id FROM products WHERE active=1 AND track_inventory=1 AND category_id=?1".into(),
                        vec![cid.into()],
                    )
                }
                "products" => {
                    if req.product_ids.is_empty() || req.product_ids.len() > 5000 {
                        return Err(AppError::validation("Select between 1 and 5,000 products."));
                    }
                    let ids: Vec<String> = req.product_ids.iter().map(|p| validate::id(p, "Product")).collect::<AppResult<_>>()?;
                    let ph = (1..=ids.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
                    (
                        Some(format!("{} products", ids.len())),
                        format!("SELECT product_id FROM products WHERE track_inventory=1 AND product_id IN ({ph})"),
                        ids.into_iter().map(|x| x.into()).collect(),
                    )
                }
                _ => return Err(AppError::validation("Scope must be all, category or products.")),
            };
            tx.execute(
                "INSERT INTO stocktakes(stocktake_id, stocktake_number, name, branch_id, scope_type, scope_ref, status, blind, created_by, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,'counting',?7,?8,?9)",
                params![id, number, name, s.branch_id, req.scope_type, scope_ref, blind as i64, s.user_id, now],
            )?;
            let pids: Vec<String> = {
                let mut st = tx.prepare(&product_sql)?;
                let rows = st.query_map(params_from_iter(args.iter()), |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                rows
            };
            if pids.is_empty() {
                return Err(AppError::validation("No stock-tracked products match this scope."));
            }
            for pid in &pids {
                let q = current_qty(tx, pid, &s.branch_id)?;
                let cost = avg_cost(tx, pid, &s.branch_id)?;
                tx.execute(
                    "INSERT INTO stocktake_lines(stocktake_id, product_id, expected_qty_milli, unit_cost_minor) VALUES (?1,?2,?3,?4)",
                    params![id, pid, q, cost],
                )?;
            }
            audit::record(tx, &actor, "stocktake.created", "stocktake", Some(&id), None, Some(&json!({ "name": name, "scope": req.scope_type, "lines": pids.len() })))?;
            Ok(id)
        })?;
        self.stocktake_get(token, &id)
    }

    pub fn stocktake_get(&self, token: &str, stocktake_id: &str) -> AppResult<StocktakeDetail> {
        let s = self.session(token)?;
        s.require("stocktake.manage")?;
        let show_cost = s.has("products.view_cost");
        let id = validate::id(stocktake_id, "Stocktake")?;
        self.db.read(|c| {
            let header = stocktake_header(c, &id)?;
            let hide_expected = header.blind && header.status == "counting";
            let mut st = c.prepare(
                "SELECT l.product_id, p.sku, p.name,
                    (SELECT barcode FROM product_barcodes b WHERE b.product_id=p.product_id ORDER BY is_primary DESC LIMIT 1),
                    p.unit, p.allow_decimal_quantity, l.expected_qty_milli, l.system_qty_at_count_milli, l.counted_qty_milli,
                    l.unit_cost_minor, l.counted_at
                 FROM stocktake_lines l JOIN products p ON p.product_id=l.product_id
                 WHERE l.stocktake_id=?1 ORDER BY p.name COLLATE NOCASE",
            )?;
            let mut ev = 0i64;
            let mut cv = 0i64;
            let mut vv = 0i64;
            let lines = st
                .query_map([&id], |r| {
                    let expected: i64 = r.get(6)?;
                    let sys: Option<i64> = r.get(7)?;
                    let counted: Option<i64> = r.get(8)?;
                    let cost: i64 = r.get(9)?;
                    Ok((
                        StocktakeLine {
                            product_id: r.get(0)?,
                            sku: r.get(1)?,
                            name: r.get(2)?,
                            primary_barcode: r.get(3)?,
                            unit: r.get(4)?,
                            allow_decimal_quantity: r.get::<_, i64>(5)? == 1,
                            expected_qty_milli: if hide_expected { None } else { Some(expected) },
                            system_qty_at_count_milli: if hide_expected { None } else { sys },
                            counted_qty_milli: counted,
                            variance_milli: if hide_expected { None } else { counted.zip(sys).map(|(a, b)| a - b) },
                            unit_cost_minor: if show_cost { Some(cost) } else { None },
                            counted_at: r.get(10)?,
                        },
                        expected,
                        sys,
                        counted,
                        cost,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut out = Vec::with_capacity(lines.len());
            for (l, expected, sys, counted, cost) in lines {
                ev += money::extend(cost, expected).unwrap_or(0);
                if let (Some(cq), Some(sq)) = (counted, sys) {
                    cv += money::extend(cost, cq).unwrap_or(0);
                    vv += money::extend(cost, cq - sq).unwrap_or(0);
                }
                out.push(l);
            }
            let show_values = show_cost && !hide_expected;
            Ok(StocktakeDetail {
                header,
                lines: out,
                expected_value_minor: show_values.then_some(ev),
                counted_value_minor: show_values.then_some(cv),
                variance_value_minor: show_values.then_some(vv),
            })
        })
    }

    /// Record a count (autosave). `barcode` or `product_id` identifies the item.
    /// mode "set" replaces the count, "add" adds to it (scan-to-count).
    pub fn stocktake_count(
        &self,
        token: &str,
        stocktake_id: &str,
        product_id: Option<String>,
        barcode: Option<String>,
        qty_milli: i64,
        mode: &str,
    ) -> AppResult<StocktakeLine> {
        let s = self.session(token)?;
        s.require("stocktake.manage")?;
        let id = validate::id(stocktake_id, "Stocktake")?;
        self.db.write(|tx| {
            let status: String = tx
                .query_row("SELECT status FROM stocktakes WHERE stocktake_id=?1", [&id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Stocktake"))?;
            if status != "counting" {
                return Err(AppError::conflict("Counting is closed for this stocktake."));
            }
            let pid = match (product_id.filter(|p| !p.is_empty()), barcode.filter(|b| !b.is_empty())) {
                (Some(p), _) => validate::id(&p, "Product")?,
                (None, Some(b)) => {
                    let b = validate::barcode(&b)?;
                    crate::catalog::barcode_owner(tx, &b)?
                        .map(|x| x.0)
                        .ok_or_else(|| AppError::new(ErrorCode::NotFound, format!("Barcode {b} was not found.")))?
                }
                _ => return Err(AppError::validation("Scan a barcode or choose a product.")),
            };
            let (dec, prev): (i64, Option<i64>) = tx
                .query_row(
                    "SELECT p.allow_decimal_quantity, l.counted_qty_milli FROM stocktake_lines l JOIN products p ON p.product_id=l.product_id
                     WHERE l.stocktake_id=?1 AND l.product_id=?2",
                    params![id, pid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| AppError::validation("This product is not part of this stocktake."))?;
            let new_count = match mode {
                "set" => qty_milli,
                "add" => prev.unwrap_or(0) + qty_milli,
                _ => return Err(AppError::validation("Mode must be set or add.")),
            };
            if new_count < 0 {
                return Err(AppError::validation("A count cannot be negative."));
            }
            if dec == 0 && new_count % QTY_SCALE != 0 {
                return Err(AppError::validation("This product is counted in whole units."));
            }
            let sys = current_qty(tx, &pid, &s.branch_id)?;
            tx.execute(
                "UPDATE stocktake_lines SET counted_qty_milli=?3, system_qty_at_count_milli=?4, counted_by=?5, counted_at=?6
                 WHERE stocktake_id=?1 AND product_id=?2",
                params![id, pid, new_count, sys, s.user_id, time::now_str()],
            )?;
            Ok(())
        })?;
        let d = self.stocktake_get(token, &id)?;
        let pid_lookup = d.lines.into_iter().filter(|l| l.counted_at.is_some()).max_by(|a, b| a.counted_at.cmp(&b.counted_at));
        pid_lookup.ok_or_else(|| AppError::internal("count not recorded"))
    }

    pub fn stocktake_set_status(&self, token: &str, stocktake_id: &str, status: &str) -> AppResult<StocktakeDetail> {
        let s = self.session(token)?;
        s.require("stocktake.manage")?;
        let actor = self.actor(&s, None);
        let id = validate::id(stocktake_id, "Stocktake")?;
        self.db.write(|tx| {
            let cur: String = tx
                .query_row("SELECT status FROM stocktakes WHERE stocktake_id=?1", [&id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Stocktake"))?;
            let ok = matches!(
                (cur.as_str(), status),
                ("counting", "review") | ("review", "counting") | ("counting", "cancelled") | ("review", "cancelled")
            );
            if !ok {
                return Err(AppError::conflict(format!("A stocktake in {cur} cannot move to {status}.")));
            }
            tx.execute("UPDATE stocktakes SET status=?2 WHERE stocktake_id=?1", params![id, status])?;
            audit::record(
                tx,
                &actor,
                "stocktake.status",
                "stocktake",
                Some(&id),
                Some(&json!({ "status": cur })),
                Some(&json!({ "status": status })),
            )?;
            Ok(())
        })?;
        self.stocktake_get(token, &id)
    }

    /// Finalize: each counted line produces a `stocktake` movement of
    /// (counted − system qty at count time). Uncounted lines are untouched.
    pub fn stocktake_finalize(&self, token: &str, stocktake_id: &str, operation_id: &str) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        s.require("stocktake.manage")?;
        s.require("inventory.adjust")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let id = validate::id(stocktake_id, "Stocktake")?;
        let payload = json!({ "stocktake_id": id });
        self.db.write(|tx| {
            if let Check::Replay { result } = idempotency::check(tx, operation_id, "stocktake.finalize", &payload)? {
                return Ok(result);
            }
            let hash = idempotency::payload_hash("stocktake.finalize", &payload)?;
            let (status, number): (String, String) = tx
                .query_row("SELECT status, stocktake_number FROM stocktakes WHERE stocktake_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| AppError::not_found("Stocktake"))?;
            if status != "review" {
                return Err(AppError::conflict("Move the stocktake to review before finalizing."));
            }
            let lines: Vec<(String, i64, i64, i64)> = {
                let mut st = tx.prepare(
                    "SELECT product_id, counted_qty_milli, system_qty_at_count_milli, unit_cost_minor FROM stocktake_lines
                     WHERE stocktake_id=?1 AND counted_qty_milli IS NOT NULL",
                )?;
                let rows = st.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
                rows
            };
            let mut adjusted = 0;
            let mut net_value = 0i64;
            for (pid, counted, sys, cost) in &lines {
                let delta = counted - sys;
                if delta == 0 {
                    continue;
                }
                apply_movement(
                    tx,
                    &Movement {
                        product_id: pid,
                        branch_id: &s.branch_id,
                        kind: "stocktake",
                        qty_delta_milli: delta,
                        unit_cost_minor: Some(*cost),
                        source_type: "stocktake",
                        source_id: Some(&id),
                        reason: Some(&number),
                        user_id: Some(&s.user_id),
                        device_id: Some(&s.device_id),
                    },
                )?;
                adjusted += 1;
                net_value += money::extend(*cost, delta)?;
            }
            tx.execute(
                "UPDATE stocktakes SET status='completed', finalized_by=?2, finalized_at=?3 WHERE stocktake_id=?1",
                params![id, s.user_id, time::now_str()],
            )?;
            let result = json!({ "stocktake_id": id, "counted_lines": lines.len(), "adjusted_lines": adjusted, "net_variance_value_minor": net_value });
            audit::record(tx, &actor, "stocktake.finalized", "stocktake", Some(&id), None, Some(&result))?;
            idempotency::complete(tx, operation_id, "stocktake.finalize", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &result)?;
            Ok(result)
        })
    }
}
