//! Digital orders (flag `orders.digital`, default off).
//!
//! An order taken by phone, WhatsApp, a website form or any other channel is
//! recorded here first. It never becomes a sale by itself:
//!
//! * draft → confirmed by a person (every line matched to a product);
//! * confirmed → loaded into a till's sale (`orders.convert`, idempotent on
//!   its operation id); the cashier takes payment as for any sale;
//! * the sale's commit marks the order converted and, when the order asked
//!   for it, opens a delivery in the same transaction.
//!
//! A WhatsApp message can seed a draft: the text is matched line by line to
//! products as a suggestion, and a person reviews it before confirming.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ids::{new_id, next_seq};
use crate::pos::CartView;
use crate::service::AppCore;
use crate::setup::clean_opt;
use crate::time;
use crate::validate;

pub const CHANNELS: [&str; 4] = ["phone", "whatsapp", "web", "other"];
pub const PAYMENT_STATES: [&str; 3] = ["unpaid", "recorded", "screenshot_pending"];

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrderLineInput {
    #[serde(default)]
    pub product_id: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub qty_milli: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderInput {
    pub channel: String,
    #[serde(default)]
    pub external_ref: Option<String>,
    #[serde(default)]
    pub customer_id: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub payment_state: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub delivery_wanted: bool,
    #[serde(default)]
    pub lines: Vec<OrderLineInput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderLineView {
    pub line_no: i64,
    pub product_id: Option<String>,
    pub product_name: Option<String>,
    pub description: String,
    pub qty_milli: i64,
    pub unit_price_minor: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderView {
    pub order_id: String,
    pub order_number: String,
    pub branch_id: String,
    pub channel: String,
    pub external_ref: Option<String>,
    pub customer_id: Option<String>,
    pub customer_name: Option<String>,
    pub phone: Option<String>,
    pub status: String,
    pub payment_state: String,
    pub inbox_seq: Option<i64>,
    pub note: Option<String>,
    pub address: Option<String>,
    pub delivery_wanted: bool,
    pub sale_id: Option<String>,
    pub receipt_number: Option<String>,
    pub delivery_id: Option<String>,
    pub created_by_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub lines: Vec<OrderLineView>,
    /// Sum of catalogue prices, before any till discount (a guide only).
    pub estimate_minor: i64,
}

fn load_order(c: &Connection, id: &str) -> AppResult<OrderView> {
    let mut o = c
        .query_row(
            "SELECT o.order_id, o.order_number, o.branch_id, o.channel, o.external_ref, o.customer_id, cu.name, o.phone, o.status,
                    o.payment_state, o.inbox_seq, o.note, o.address, o.delivery_wanted, o.sale_id, sa.receipt_number, o.delivery_id,
                    u.display_name, o.created_at, o.updated_at
             FROM digital_orders o LEFT JOIN customers cu ON cu.customer_id=o.customer_id LEFT JOIN users u ON u.user_id=o.created_by
             LEFT JOIN sales sa ON sa.sale_id=o.sale_id WHERE o.order_id=?1",
            [id],
            |r| {
                Ok(OrderView {
                    order_id: r.get(0)?,
                    order_number: r.get(1)?,
                    branch_id: r.get(2)?,
                    channel: r.get(3)?,
                    external_ref: r.get(4)?,
                    customer_id: r.get(5)?,
                    customer_name: r.get(6)?,
                    phone: r.get(7)?,
                    status: r.get(8)?,
                    payment_state: r.get(9)?,
                    inbox_seq: r.get(10)?,
                    note: r.get(11)?,
                    address: r.get(12)?,
                    delivery_wanted: r.get::<_, i64>(13)? == 1,
                    sale_id: r.get(14)?,
                    receipt_number: r.get(15)?,
                    delivery_id: r.get(16)?,
                    created_by_name: r.get(17)?,
                    created_at: r.get(18)?,
                    updated_at: r.get(19)?,
                    lines: vec![],
                    estimate_minor: 0,
                })
            },
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("Order"))?;
    let sql = format!(
        "SELECT l.line_no, l.product_id, p.name, l.description, l.qty_milli, {} FROM digital_order_lines l
         LEFT JOIN products p ON p.product_id=l.product_id WHERE l.order_id=?1 ORDER BY l.line_no",
        crate::catalog::PRICE_SQL
    );
    let mut st = c.prepare(&sql)?;
    o.lines = st
        .query_map([id], |r| {
            Ok(OrderLineView {
                line_no: r.get(0)?,
                product_id: r.get(1)?,
                product_name: r.get(2)?,
                description: r.get(3)?,
                qty_milli: r.get(4)?,
                unit_price_minor: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    o.estimate_minor = o.lines.iter().map(|l| l.unit_price_minor.unwrap_or(0) * l.qty_milli / 1000).sum();
    Ok(o)
}

fn check_lines(c: &Connection, lines: &[OrderLineInput]) -> AppResult<Vec<(Option<String>, String, i64)>> {
    if lines.len() > 200 {
        return Err(AppError::validation("An order can have at most 200 lines."));
    }
    let mut out = vec![];
    for l in lines {
        if l.qty_milli <= 0 {
            return Err(AppError::validation("Quantities must be more than zero."));
        }
        let pid = match l.product_id.as_ref().filter(|x| !x.is_empty()) {
            Some(p) => Some(validate::id(p, "Product")?),
            None => None,
        };
        let name: Option<String> = match &pid {
            Some(p) => Some(
                c.query_row("SELECT name FROM products WHERE product_id=?1", [p], |r| r.get(0))
                    .optional()?
                    .ok_or_else(|| AppError::not_found("Product"))?,
            ),
            None => None,
        };
        let desc = clean_opt(&l.description, "Line", 200)?
            .or(name)
            .ok_or_else(|| AppError::validation("Every line needs a product or a description."))?;
        out.push((pid, desc, l.qty_milli));
    }
    Ok(out)
}

/// The session may act on this order: its branch when multi-branch is on.
fn in_scope(c: &Connection, s: &Session, branch_id: &str) -> AppResult<()> {
    crate::branches::require_branch(c, s, branch_id)
}

/// Deterministic suggestion from a message: one item per line, with an
/// optional quantity before or after ("2 x milk", "milk 2", "3 bread").
/// Matched by barcode, SKU, then a unique name match. Unmatched text stays
/// as a description for a person to resolve.
pub fn suggest_lines(c: &Connection, text: &str) -> AppResult<Vec<OrderLineInput>> {
    let mut out = vec![];
    for raw in text.lines().take(50) {
        let line = raw.trim().trim_start_matches(['-', '*', '•']).trim();
        if line.is_empty() {
            continue;
        }
        let mut words: Vec<&str> = line.split_whitespace().collect();
        let mut qty = 1i64;
        let is_qty = |w: &str| {
            let w = w.trim_end_matches(['x', 'X', '×']).trim_start_matches(['x', 'X', '×']);
            if w.is_empty() || w.len() > 4 {
                None
            } else {
                w.parse::<i64>().ok().filter(|n| (1..=999).contains(n))
            }
        };
        if let Some(n) = words.first().and_then(|w| is_qty(w)) {
            qty = n;
            words.remove(0);
        } else if let Some(n) = words.last().and_then(|w| is_qty(w)) {
            qty = n;
            words.pop();
        }
        if words.first().is_some_and(|w| matches!(*w, "x" | "X" | "×")) {
            words.remove(0);
        }
        let query = words.join(" ");
        if query.is_empty() {
            continue;
        }
        let q = query.to_lowercase();
        let mut pid: Option<String> = c
            .query_row(
                "SELECT p.product_id FROM product_barcodes b JOIN products p ON p.product_id=b.product_id WHERE b.barcode=?1 AND p.active=1",
                [&query],
                |r| r.get(0),
            )
            .optional()?;
        if pid.is_none() {
            pid = c.query_row("SELECT product_id FROM products WHERE lower(sku)=?1 AND active=1", [&q], |r| r.get(0)).optional()?;
        }
        if pid.is_none() {
            let like = format!("%{}%", q.replace(['%', '_'], ""));
            let mut st =
                c.prepare("SELECT product_id FROM products WHERE active=1 AND (lower(name) LIKE ?1 OR name_ar LIKE ?1) LIMIT 2")?;
            let hits: Vec<String> = st.query_map([&like], |r| r.get(0))?.collect::<Result<_, _>>()?;
            if hits.len() == 1 {
                pid = hits.into_iter().next();
            }
        }
        out.push(OrderLineInput { product_id: pid, description: Some(query.chars().take(200).collect()), qty_milli: qty * 1000 });
    }
    Ok(out)
}

/// Called inside the sale's commit when the cart came from an order.
pub(crate) fn on_sale_committed(tx: &Connection, s: &Session, actor: &audit::Actor, cart_id: &str, sale_id: &str) -> AppResult<()> {
    let order: Option<String> = tx.query_row("SELECT digital_order_id FROM carts WHERE cart_id=?1", [cart_id], |r| r.get(0))?;
    let Some(order_id) = order else { return Ok(()) };
    let (status, wanted, address, phone, customer, cart): (String, i64, Option<String>, Option<String>, Option<String>, Option<String>) =
        tx.query_row(
            "SELECT status, delivery_wanted, address, phone, customer_id, cart_id FROM digital_orders WHERE order_id=?1",
            [&order_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("Order"))?;
    if status != "confirmed" || cart.as_deref() != Some(cart_id) {
        // The order was cancelled or re-loaded elsewhere; the sale stands on its own.
        return Ok(());
    }
    let now = time::now_str();
    let mut delivery_id = None;
    if wanted == 1 {
        let req = crate::customers::DeliveryCreate {
            sale_id: Some(sale_id.to_string()),
            customer_id: customer,
            address: address.clone(),
            area: None,
            phone,
            payment_status: Some("paid".into()),
            amount_minor: None,
            notes: None,
        };
        delivery_id = Some(crate::customers::insert_delivery(tx, s, actor, &req)?);
    }
    tx.execute(
        "UPDATE digital_orders SET status='converted', sale_id=?2, delivery_id=?3, updated_at=?4 WHERE order_id=?1",
        params![order_id, sale_id, delivery_id, now],
    )?;
    audit::record(
        tx,
        actor,
        "order.converted",
        "digital_order",
        Some(&order_id),
        None,
        Some(&json!({ "sale_id": sale_id, "delivery_id": delivery_id })),
    )?;
    Ok(())
}

impl AppCore {
    fn order_session(&self, token: &str, perm: &str) -> AppResult<Session> {
        let s = self.session(token)?;
        self.require_feature("orders.digital")?;
        if !s.has(perm) && !(perm == "orders.view" && s.has("orders.manage")) {
            return Err(AppError::forbidden(perm));
        }
        Ok(s)
    }

    /// Product lookup for order lines (order desk staff may not have catalogue access).
    pub fn orders_product_search(&self, token: &str, q: &str) -> AppResult<Vec<serde_json::Value>> {
        let s = self.session(token)?;
        self.require_feature("orders.digital")?;
        if !s.has("orders.manage") && !s.has("pos.sell") && !s.has("products.view") {
            return Err(AppError::forbidden("orders.manage"));
        }
        let q = q.trim().to_lowercase();
        if q.is_empty() {
            return Ok(vec![]);
        }
        let like = format!("%{}%", q.replace(['%', '_'], ""));
        self.db.read(|c| {
            let sql = format!(
                "SELECT p.product_id, p.name, p.sku, {} FROM products p WHERE p.active=1 AND (lower(p.name) LIKE ?1 OR p.name_ar LIKE ?1 OR lower(p.sku)=?2
                   OR EXISTS (SELECT 1 FROM product_barcodes b WHERE b.product_id=p.product_id AND b.barcode=?2))
                 ORDER BY p.name COLLATE NOCASE LIMIT 20",
                crate::catalog::PRICE_SQL
            );
            let mut st = c.prepare(&sql)?;
            let rows = st
                .query_map(params![like, q], |r| {
                    Ok(json!({ "product_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "sku": r.get::<_, String>(2)?,
                               "price_minor": r.get::<_, Option<i64>>(3)? }))
                })?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }

    pub fn orders_list(&self, token: &str, status: Option<String>) -> AppResult<Vec<OrderView>> {
        let s = self.session(token)?;
        self.require_feature("orders.digital")?;
        if !s.has("orders.manage") && !s.has("pos.sell") {
            return Err(AppError::forbidden("orders.manage"));
        }
        self.db.read(|c| {
            let branch = crate::branches::list_scope(c, &s)?;
            let mut st = c.prepare(
                "SELECT order_id FROM digital_orders WHERE (?1 IS NULL AND status IN ('draft','confirmed') OR status=?1)
                 AND (?2 IS NULL OR branch_id=?2) ORDER BY created_at DESC LIMIT 300",
            )?;
            let ids: Vec<String> = st.query_map(params![status, branch], |r| r.get(0))?.collect::<Result<_, _>>()?;
            ids.iter().map(|id| load_order(c, id)).collect()
        })
    }

    pub fn order_get(&self, token: &str, order_id: &str) -> AppResult<OrderView> {
        let s = self.session(token)?;
        self.require_feature("orders.digital")?;
        if !s.has("orders.manage") && !s.has("pos.sell") {
            return Err(AppError::forbidden("orders.manage"));
        }
        let id = validate::id(order_id, "Order")?;
        self.db.read(|c| {
            let o = load_order(c, &id)?;
            if crate::branches::list_scope(c, &s)?.is_some_and(|b| b != o.branch_id) {
                return Err(AppError::not_found("Order"));
            }
            Ok(o)
        })
    }

    /// Create (no id) or edit a draft order.
    pub fn order_save(&self, token: &str, order_id: Option<String>, input: OrderInput) -> AppResult<OrderView> {
        let s = self.order_session(token, "orders.manage")?;
        if !CHANNELS.contains(&input.channel.as_str()) {
            return Err(AppError::validation("Channel must be phone, WhatsApp, web or other."));
        }
        let pay = input.payment_state.clone().unwrap_or_else(|| "unpaid".into());
        if !PAYMENT_STATES.contains(&pay.as_str()) {
            return Err(AppError::validation("Payment state must be unpaid, recorded or screenshot pending."));
        }
        let ext = clean_opt(&input.external_ref, "Reference", 80)?;
        let note = clean_opt(&input.note, "Note", 1000)?;
        let address = clean_opt(&input.address, "Address", 300)?;
        let phone = match input.phone.as_deref().filter(|p| !p.trim().is_empty()) {
            Some(p) => crate::customers::normalize_phone(p)?,
            None => None,
        };
        let actor = self.actor(&s, None);
        let id = self.db.write(|tx| {
            let customer = match input.customer_id.as_ref().filter(|x| !x.is_empty()) {
                Some(cid) => {
                    let cid = validate::id(cid, "Customer")?;
                    tx.query_row("SELECT 1 FROM customers WHERE customer_id=?1", [&cid], |_| Ok(()))
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Customer"))?;
                    Some(cid)
                }
                None => None,
            };
            if input.delivery_wanted && address.is_none() && customer.is_none() {
                return Err(AppError::validation("Enter a delivery address or choose the customer."));
            }
            let lines = check_lines(tx, &input.lines)?;
            let now = time::now_str();
            let (id, before) = match order_id.as_ref().filter(|x| !x.is_empty()) {
                Some(id) => {
                    let id = validate::id(id, "Order")?;
                    let o = load_order(tx, &id)?;
                    in_scope(tx, &s, &o.branch_id)?;
                    if o.status != "draft" {
                        return Err(AppError::conflict("Only a draft order can be edited."));
                    }
                    tx.execute(
                        "UPDATE digital_orders SET channel=?2, external_ref=?3, customer_id=?4, phone=?5, payment_state=?6, note=?7, address=?8,
                            delivery_wanted=?9, updated_at=?10 WHERE order_id=?1",
                        params![id, input.channel, ext, customer, phone, pay, note, address, input.delivery_wanted as i64, now],
                    )?;
                    tx.execute("DELETE FROM digital_order_lines WHERE order_id=?1", [&id])?;
                    (id, Some(serde_json::to_value(&o.lines).unwrap_or_default()))
                }
                None => {
                    let id = new_id();
                    let number = format!("O-{:05}", next_seq(tx, "digital_order")?);
                    tx.execute(
                        "INSERT INTO digital_orders(order_id, order_number, branch_id, channel, external_ref, customer_id, phone, status, payment_state,
                            note, address, delivery_wanted, created_by, created_at, updated_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,'draft',?8,?9,?10,?11,?12,?13,?13)",
                        params![id, number, s.branch_id, input.channel, ext, customer, phone, pay, note, address, input.delivery_wanted as i64, s.user_id, now],
                    )?;
                    (id, None)
                }
            };
            for (i, (pid, desc, qty)) in lines.iter().enumerate() {
                tx.execute(
                    "INSERT INTO digital_order_lines(order_id, line_no, product_id, description, qty_milli) VALUES (?1,?2,?3,?4,?5)",
                    params![id, i as i64 + 1, pid, desc, qty],
                )?;
            }
            audit::record(
                tx,
                &actor,
                if before.is_some() { "order.updated" } else { "order.created" },
                "digital_order",
                Some(&id),
                before.as_ref(),
                Some(&json!({ "channel": input.channel, "lines": lines.len(), "payment_state": pay })),
            )?;
            Ok(id)
        })?;
        self.db.read(|c| load_order(c, &id))
    }

    /// Start a draft from a WhatsApp message. The suggested lines are only a
    /// starting point; a person edits and confirms the order.
    pub fn order_from_inbox(&self, token: &str, seq: i64) -> AppResult<OrderView> {
        let s = self.order_session(token, "orders.manage")?;
        let actor = self.actor(&s, None);
        let id = self.db.write(|tx| {
            let (body, phone, customer): (Option<String>, Option<String>, Option<String>) = tx
                .query_row("SELECT COALESCE(body, caption), phone, customer_id FROM wa_inbox WHERE seq=?1", [seq], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Message"))?;
            if let Some(existing) = tx
                .query_row("SELECT order_id FROM digital_orders WHERE inbox_seq=?1 AND status<>'cancelled'", [seq], |r| r.get::<_, String>(0))
                .optional()?
            {
                return Ok(existing);
            }
            let text = body.unwrap_or_default();
            let lines = suggest_lines(tx, &text)?;
            let id = new_id();
            let number = format!("O-{:05}", next_seq(tx, "digital_order")?);
            let now = time::now_str();
            let note: String = text.chars().take(1000).collect();
            tx.execute(
                "INSERT INTO digital_orders(order_id, order_number, branch_id, channel, customer_id, phone, status, payment_state, inbox_seq,
                    note, created_by, created_at, updated_at)
                 VALUES (?1,?2,?3,'whatsapp',?4,?5,'draft','unpaid',?6,?7,?8,?9,?9)",
                params![id, number, s.branch_id, customer, phone, seq, (!note.is_empty()).then_some(note), s.user_id, now],
            )?;
            for (i, l) in lines.iter().enumerate() {
                tx.execute(
                    "INSERT INTO digital_order_lines(order_id, line_no, product_id, description, qty_milli) VALUES (?1,?2,?3,?4,?5)",
                    params![id, i as i64 + 1, l.product_id, l.description.clone().unwrap_or_default(), l.qty_milli],
                )?;
            }
            audit::record(tx, &actor, "order.created", "digital_order", Some(&id), None, Some(&json!({ "channel": "whatsapp", "inbox_seq": seq })))?;
            Ok(id)
        })?;
        self.db.read(|c| load_order(c, &id))
    }

    pub fn order_confirm(&self, token: &str, order_id: &str) -> AppResult<OrderView> {
        let s = self.order_session(token, "orders.manage")?;
        let id = validate::id(order_id, "Order")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let o = load_order(tx, &id)?;
            in_scope(tx, &s, &o.branch_id)?;
            if o.status != "draft" {
                return Err(AppError::conflict("Only a draft order can be confirmed."));
            }
            if o.lines.is_empty() {
                return Err(AppError::validation("Add at least one item."));
            }
            if o.lines.iter().any(|l| l.product_id.is_none()) {
                return Err(AppError::validation("Match every line to a product before confirming."));
            }
            tx.execute("UPDATE digital_orders SET status='confirmed', updated_at=?2 WHERE order_id=?1", params![id, time::now_str()])?;
            audit::record(tx, &actor, "order.confirmed", "digital_order", Some(&id), None, Some(&json!({ "lines": o.lines.len() })))?;
            Ok(())
        })?;
        self.db.read(|c| load_order(c, &id))
    }

    pub fn order_cancel(&self, token: &str, order_id: &str, reason: Option<String>) -> AppResult<OrderView> {
        let s = self.order_session(token, "orders.manage")?;
        let id = validate::id(order_id, "Order")?;
        let reason = clean_opt(&reason, "Reason", 300)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let o = load_order(tx, &id)?;
            in_scope(tx, &s, &o.branch_id)?;
            if !["draft", "confirmed"].contains(&o.status.as_str()) {
                return Err(AppError::conflict("This order can no longer be cancelled."));
            }
            tx.execute("UPDATE digital_orders SET status='cancelled', updated_at=?2 WHERE order_id=?1", params![id, time::now_str()])?;
            // A till that loaded it keeps its sale, which is no longer tied to the order.
            tx.execute("UPDATE carts SET digital_order_id=NULL WHERE digital_order_id=?1 AND status IN ('active','held')", [&id])?;
            audit::record(
                tx,
                &actor,
                "order.cancelled",
                "digital_order",
                Some(&id),
                Some(&json!({ "status": o.status })),
                Some(&json!({ "reason": reason })),
            )?;
            Ok(())
        })?;
        self.db.read(|c| load_order(c, &id))
    }

    pub fn order_set_payment(&self, token: &str, order_id: &str, payment_state: &str) -> AppResult<OrderView> {
        let s = self.order_session(token, "orders.manage")?;
        let id = validate::id(order_id, "Order")?;
        if !PAYMENT_STATES.contains(&payment_state) {
            return Err(AppError::validation("Payment state must be unpaid, recorded or screenshot pending."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let o = load_order(tx, &id)?;
            in_scope(tx, &s, &o.branch_id)?;
            if !["draft", "confirmed"].contains(&o.status.as_str()) {
                return Err(AppError::conflict("This order is closed."));
            }
            tx.execute(
                "UPDATE digital_orders SET payment_state=?2, updated_at=?3 WHERE order_id=?1",
                params![id, payment_state, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "order.payment_state",
                "digital_order",
                Some(&id),
                Some(&json!({ "payment_state": o.payment_state })),
                Some(&json!({ "payment_state": payment_state })),
            )?;
            Ok(())
        })?;
        self.db.read(|c| load_order(c, &id))
    }

    /// Load a confirmed order into this till's sale. Repeating the same
    /// operation id returns the same sale in progress; the order is marked
    /// converted when that sale is paid.
    pub fn order_convert(&self, token: &str, order_id: &str, operation_id: &str) -> AppResult<CartView> {
        let s = self.session(token)?;
        self.require_feature("orders.digital")?;
        s.require("pos.sell")?;
        crate::idempotency::validate_operation_id(operation_id)?;
        let id = validate::id(order_id, "Order")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let o = load_order(tx, &id)?;
            in_scope(tx, &s, &o.branch_id)?;
            let (op, cart): (Option<String>, Option<String>) =
                tx.query_row("SELECT convert_operation_id, cart_id FROM digital_orders WHERE order_id=?1", [&id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            let cart_state: Option<(String, String, String)> = match &cart {
                Some(cid) => tx
                    .query_row("SELECT status, device_id, user_id FROM carts WHERE cart_id=?1", [cid], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })
                    .optional()?,
                None => None,
            };
            if op.as_deref() == Some(operation_id) {
                // Replay: hand back the same sale in progress on this till.
                return match (&cart, &cart_state) {
                    (Some(cid), Some((st, dev, user))) if st == "active" && *dev == s.device_id && *user == s.user_id => {
                        crate::pos::cart_view(tx, &s, cid, vec![])
                    }
                    _ if o.status == "converted" => Err(AppError::conflict("This order has already been sold.")),
                    _ => Err(AppError::conflict("This order is open on another till.")),
                };
            }
            if o.status == "converted" {
                return Err(AppError::conflict("This order has already been sold."));
            }
            if o.status != "confirmed" {
                return Err(AppError::conflict("Confirm the order before selling it."));
            }
            // Another conversion is still live (a sale in progress or on hold).
            if matches!(&cart_state, Some((st, _, _)) if st == "active" || st == "held") {
                return Err(AppError::new(ErrorCode::IdempotencyMismatch, "This order is already open on a till."));
            }
            let lines: Vec<(String, i64)> = o.lines.iter().filter_map(|l| l.product_id.clone().map(|p| (p, l.qty_milli))).collect();
            let cart_id = crate::pos::load_order_cart(tx, &s, &id, o.customer_id.as_deref(), &lines)?;
            tx.execute(
                "UPDATE digital_orders SET convert_operation_id=?2, cart_id=?3, updated_at=?4 WHERE order_id=?1",
                params![id, operation_id, cart_id, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "order.loaded",
                "digital_order",
                Some(&id),
                None,
                Some(&json!({ "cart_id": cart_id, "operation_id": operation_id })),
            )?;
            crate::pos::cart_view(tx, &s, &cart_id, vec![])
        })
    }
}
