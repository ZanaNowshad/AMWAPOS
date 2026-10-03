//! Customers and deliveries.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult};
use crate::ids::{new_id, next_seq};
use crate::service::AppCore;
use crate::setup::{clean, clean_opt};
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomerInput {
    pub name: String,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub whatsapp: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub area: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    /// Flat / Building / Road / Block; when given, `address` is composed from them.
    #[serde(default, deserialize_with = "crate::address::null_as_default")]
    pub address_parts: crate::address::AddressParts,
    #[serde(default = "yes")]
    pub active: bool,
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct CustomerRow {
    pub customer_id: String,
    #[serde(flatten)]
    pub info: CustomerInput,
    pub created_at: String,
    pub purchase_count: i64,
    pub total_spent_minor: i64,
    pub last_purchase_at: Option<String>,
    /// Loyalty balance (only when loyalty is on).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loyalty_points: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeliveryRow {
    pub delivery_id: String,
    pub delivery_number: String,
    pub sale_id: Option<String>,
    pub receipt_number: Option<String>,
    pub customer_id: Option<String>,
    pub customer_name: Option<String>,
    pub phone: Option<String>,
    pub area: Option<String>,
    pub address: Option<String>,
    pub status: String,
    pub payment_status: String,
    pub amount_minor: i64,
    pub assigned_user_id: Option<String>,
    pub assigned_name: Option<String>,
    pub notes: Option<String>,
    pub created_at: String,
    pub dispatched_at: Option<String>,
    pub delivered_at: Option<String>,
    /// Ticket payment state: unpaid | recorded | screenshot_pending | paid.
    pub pay_state: String,
    /// Digital order this drop fulfils (when the ticket came from one).
    pub order_id: Option<String>,
    /// walk_in | phone | whatsapp | web | other
    pub channel: Option<String>,
    pub branch_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeliveryCreate {
    #[serde(default)]
    pub sale_id: Option<String>,
    #[serde(default)]
    pub customer_id: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub area: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    /// paid | pending | cod
    #[serde(default)]
    pub payment_status: Option<String>,
    #[serde(default)]
    pub amount_minor: Option<i64>,
    #[serde(default)]
    pub notes: Option<String>,
    /// Digital order this drop fulfils.
    #[serde(default)]
    pub order_id: Option<String>,
    /// Flat / Building / Road / Block; when given, `address` is composed from them.
    #[serde(default, deserialize_with = "crate::address::null_as_default")]
    pub address_parts: crate::address::AddressParts,
    /// walk_in | phone | whatsapp | web | other
    #[serde(default)]
    pub channel: Option<String>,
    /// unpaid | recorded | screenshot_pending | paid (default from payment_status).
    #[serde(default)]
    pub pay_state: Option<String>,
}

/// Normalize a phone number: keep digits and a leading '+'. Bahrain local
/// 8-digit numbers get +973.
pub fn normalize_phone(p: &str) -> AppResult<Option<String>> {
    let t = p.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let plus = t.starts_with('+') || t.starts_with("00");
    let digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = if t.starts_with("00") { digits[2..].to_string() } else { digits };
    if digits.len() < 7 || digits.len() > 15 {
        return Err(AppError::validation("Enter a valid phone number."));
    }
    if !plus && digits.len() == 8 {
        return Ok(Some(format!("+973{digits}")));
    }
    Ok(Some(format!("+{digits}")))
}

fn validate_customer(c: &CustomerInput) -> AppResult<CustomerInput> {
    let email = clean_opt(&c.email, "Email", 120)?;
    if let Some(e) = &email {
        if !e.contains('@') || e.contains(' ') {
            return Err(AppError::validation("Enter a valid email address."));
        }
    }
    let parts = c.address_parts.cleaned()?;
    Ok(CustomerInput {
        address: match parts.line() {
            Some(line) => Some(line),
            None => clean_opt(&c.address, "Address", 300)?,
        },
        address_parts: parts,
        name: clean(&c.name, "Customer name", 120, true)?,
        phone: normalize_phone(c.phone.as_deref().unwrap_or(""))?,
        whatsapp: normalize_phone(c.whatsapp.as_deref().unwrap_or(""))?,
        email,
        area: clean_opt(&c.area, "Area", 80)?,
        active: c.active,
    })
}

fn load_customer(c: &Connection, id: &str) -> AppResult<CustomerRow> {
    c.query_row(
        "SELECT cu.customer_id, cu.name, cu.phone, cu.whatsapp, cu.email, cu.area, cu.address, cu.active, cu.created_at,
                (SELECT COUNT(*) FROM sales s WHERE s.customer_id=cu.customer_id),
                COALESCE((SELECT SUM(total_minor) FROM sales s WHERE s.customer_id=cu.customer_id),0)
                  - COALESCE((SELECT SUM(r.total_minor) FROM refunds r JOIN sales s ON s.sale_id=r.original_sale_id WHERE s.customer_id=cu.customer_id),0),
                (SELECT MAX(completed_at) FROM sales s WHERE s.customer_id=cu.customer_id)
         FROM customers cu WHERE cu.customer_id=?1",
        [id],
        |r| {
            Ok(CustomerRow {
                customer_id: r.get(0)?,
                info: CustomerInput {
                    name: r.get(1)?,
                    phone: r.get(2)?,
                    whatsapp: r.get(3)?,
                    email: r.get(4)?,
                    area: r.get(5)?,
                    address: r.get(6)?,
                    address_parts: Default::default(),
                    active: r.get::<_, i64>(7)? == 1,
                },
                created_at: r.get(8)?,
                purchase_count: r.get(9)?,
                total_spent_minor: r.get(10)?,
                last_purchase_at: r.get(11)?,
                loyalty_points: None,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("Customer"))
    .and_then(|mut row| {
        row.info.address_parts = crate::address::read_parts(c, "customers", "customer_id", &row.customer_id)?;
        if crate::loyalty::enabled(c)? {
            row.loyalty_points = Some(crate::loyalty::balance(c, &row.customer_id)?);
        }
        Ok(row)
    })
}

pub(crate) fn load_delivery_row(c: &Connection, id: &str) -> AppResult<DeliveryRow> {
    load_delivery(c, id)
}

fn load_delivery(c: &Connection, id: &str) -> AppResult<DeliveryRow> {
    c.query_row(
        "SELECT d.delivery_id, d.delivery_number, d.sale_id, s.receipt_number, d.customer_id, cu.name, d.phone, d.area, d.address, d.status,
                d.payment_status, d.amount_minor, d.assigned_user_id, u.display_name, d.notes, d.created_at, d.dispatched_at, d.delivered_at,
                COALESCE(d.pay_state, CASE d.payment_status WHEN 'paid' THEN 'paid' ELSE 'unpaid' END), d.order_id, d.channel, d.branch_id
         FROM delivery_orders d LEFT JOIN sales s ON s.sale_id=d.sale_id LEFT JOIN customers cu ON cu.customer_id=d.customer_id
         LEFT JOIN users u ON u.user_id=d.assigned_user_id WHERE d.delivery_id=?1",
        [id],
        |r| {
            Ok(DeliveryRow {
                delivery_id: r.get(0)?,
                delivery_number: r.get(1)?,
                sale_id: r.get(2)?,
                receipt_number: r.get(3)?,
                customer_id: r.get(4)?,
                customer_name: r.get(5)?,
                phone: r.get(6)?,
                area: r.get(7)?,
                address: r.get(8)?,
                status: r.get(9)?,
                payment_status: r.get(10)?,
                amount_minor: r.get(11)?,
                assigned_user_id: r.get(12)?,
                assigned_name: r.get(13)?,
                notes: r.get(14)?,
                created_at: r.get(15)?,
                dispatched_at: r.get(16)?,
                delivered_at: r.get(17)?,
                pay_state: r.get(18)?,
                order_id: r.get(19)?,
                channel: r.get(20)?,
                branch_id: r.get(21)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("Delivery"))
}

impl AppCore {
    pub fn customers_search(
        &self,
        token: &str,
        q: Option<String>,
        include_inactive: bool,
        limit: Option<i64>,
    ) -> AppResult<Vec<CustomerRow>> {
        let s = self.session(token)?;
        s.require("customers.view")?;
        let limit = validate::limit(limit, 50, 500);
        self.db.read(|c| {
            let text = q.unwrap_or_default().trim().to_string();
            let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
            // "0097333112233" is stored as "+97333112233".
            let digits = if text.starts_with("00") { digits.trim_start_matches("00").to_string() } else { digits };
            let like = format!("%{}%", text.replace('%', ""));
            let dlike = if digits.len() >= 3 { format!("%{digits}%") } else { "\u{0}".into() };
            let mut st = c.prepare(&format!(
                "SELECT customer_id FROM customers WHERE (?1 OR active=1) AND (?2 = '%%' OR name LIKE ?2 OR phone LIKE ?3 OR whatsapp LIKE ?3)
                 ORDER BY name COLLATE NOCASE LIMIT {limit}"
            ))?;
            let ids = st.query_map(params![include_inactive, like, dlike], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            ids.iter().map(|id| load_customer(c, id)).collect()
        })
    }

    pub fn customer_get(&self, token: &str, customer_id: &str) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        s.require("customers.view")?;
        let id = validate::id(customer_id, "Customer")?;
        self.db.read(|c| {
            let cust = load_customer(c, &id)?;
            let mut st = c.prepare(
                "SELECT n.note_id, n.note, n.created_at, u.display_name FROM customer_notes n LEFT JOIN users u ON u.user_id=n.author_id
                 WHERE n.customer_id=?1 ORDER BY n.created_at DESC LIMIT 200",
            )?;
            let notes = st
                .query_map([&id], |r| {
                    Ok(json!({ "note_id": r.get::<_, String>(0)?, "note": r.get::<_, String>(1)?, "created_at": r.get::<_, String>(2)?, "author": r.get::<_, Option<String>>(3)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let purchases = if s.has("sales.view") {
                let mut st = c.prepare(
                    "SELECT sale_id, receipt_number, completed_at, total_minor, item_count_milli FROM sales WHERE customer_id=?1 ORDER BY completed_at DESC LIMIT 200",
                )?;
                let rows = st
                    .query_map([&id], |r| {
                        Ok(json!({ "sale_id": r.get::<_, String>(0)?, "receipt_number": r.get::<_, String>(1)?, "completed_at": r.get::<_, String>(2)?,
                            "total_minor": r.get::<_, i64>(3)?, "item_count_milli": r.get::<_, i64>(4)? }))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            } else {
                vec![]
            };
            let mut st = c.prepare("SELECT delivery_id FROM delivery_orders WHERE customer_id=?1 ORDER BY created_at DESC LIMIT 100")?;
            let dids = st.query_map([&id], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            let deliveries: Vec<DeliveryRow> = dids.iter().map(|d| load_delivery(c, d)).collect::<AppResult<_>>()?;
            Ok(json!({ "customer": cust, "notes": notes, "purchases": purchases, "deliveries": deliveries }))
        })
    }

    pub fn customer_save(&self, token: &str, customer_id: Option<String>, input: CustomerInput) -> AppResult<CustomerRow> {
        let s = self.session(token)?;
        s.require("customers.manage")?;
        let mut v = validate_customer(&input)?;
        // No area typed: take a known place name from the address.
        if v.area.is_none() {
            v.area = v.address.as_deref().and_then(area_from_text).map(str::to_string);
        }
        let actor = self.actor(&s, None);
        let id = self.db.write(|tx| {
            if v.area.is_none() {
                if let Some(b) = &v.address_parts.block {
                    v.area = crate::address::area_for_block(tx, b)?;
                }
            }
            if let Some(p) = &v.phone {
                let other: Option<String> = tx
                    .query_row("SELECT name FROM customers WHERE phone=?1 AND customer_id IS NOT ?2", params![p, customer_id], |r| r.get(0))
                    .optional()?;
                if let Some(o) = other {
                    return Err(AppError::duplicate(format!("This phone number is already saved for {o}.")));
                }
            }
            let now = time::now_str();
            match &customer_id {
                Some(id) => {
                    let id = validate::id(id, "Customer")?;
                    let before = serde_json::to_value(load_customer(tx, &id)?.info)?;
                    tx.execute(
                        "UPDATE customers SET name=?2, phone=?3, whatsapp=?4, email=?5, area=?6, address=?7, active=?8, updated_at=?9 WHERE customer_id=?1",
                        params![id, v.name, v.phone, v.whatsapp, v.email, v.area, v.address, v.active as i64, now],
                    )?;
                    crate::address::write_parts(tx, "customers", "customer_id", &id, &v.address_parts)?;
                    audit::record(tx, &actor, "customer.updated", "customer", Some(&id), Some(&before), Some(&json!({ "name": v.name })))?;
                    Ok(id)
                }
                None => {
                    let id = new_id();
                    tx.execute(
                        "INSERT INTO customers(customer_id, name, phone, whatsapp, email, area, address, active, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)",
                        params![id, v.name, v.phone, v.whatsapp.clone().or(v.phone.clone()), v.email, v.area, v.address, v.active as i64, now],
                    )?;
                    crate::address::write_parts(tx, "customers", "customer_id", &id, &v.address_parts)?;
                    audit::record(tx, &actor, "customer.created", "customer", Some(&id), None, Some(&json!({ "name": v.name })))?;
                    Ok(id)
                }
            }
        })?;
        self.db.read(|c| load_customer(c, &id))
    }

    pub fn customer_add_note(&self, token: &str, customer_id: &str, note: &str) -> AppResult<()> {
        let s = self.session(token)?;
        s.require("customers.manage")?;
        let id = validate::id(customer_id, "Customer")?;
        let note = clean(note, "Note", 2000, true)?;
        self.db.write(|tx| {
            load_customer(tx, &id)?;
            tx.execute(
                "INSERT INTO customer_notes(note_id, customer_id, author_id, note, created_at) VALUES (?1,?2,?3,?4,?5)",
                params![new_id(), id, s.user_id, note, time::now_str()],
            )?;
            Ok(())
        })
    }

    // ---- deliveries ----

    pub fn deliveries_list(&self, token: &str, status: Option<String>, include_closed: bool) -> AppResult<Vec<DeliveryRow>> {
        let s = self.session(token)?;
        s.require("deliveries.view")?;
        let only_mine = !s.has("deliveries.manage") && !s.has("sales.view") && !s.has("pos.sell");
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT delivery_id FROM delivery_orders WHERE (?1 IS NULL OR status=?1) AND (?2 OR status IN ('pending','preparing','dispatched'))
                   AND (?3 = 0 OR assigned_user_id=?4)
                 ORDER BY created_at DESC LIMIT 500",
            )?;
            let ids = st
                .query_map(params![status.filter(|x| !x.is_empty()), include_closed, only_mine as i64, s.user_id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids.iter().map(|id| load_delivery(c, id)).collect()
        })
    }

    pub fn delivery_get(&self, token: &str, delivery_id: &str) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        s.require("deliveries.view")?;
        let id = validate::id(delivery_id, "Delivery")?;
        self.db.read(|c| {
            let d = load_delivery(c, &id)?;
            let mut st = c.prepare(
                "SELECT e.previous_status, e.new_status, e.note, e.created_at, u.display_name FROM delivery_events e LEFT JOIN users u ON u.user_id=e.user_id
                 WHERE e.delivery_id=?1 ORDER BY e.created_at",
            )?;
            let events = st
                .query_map([&id], |r| {
                    Ok(json!({ "from": r.get::<_, Option<String>>(0)?, "to": r.get::<_, String>(1)?, "note": r.get::<_, Option<String>>(2)?,
                        "at": r.get::<_, String>(3)?, "user": r.get::<_, Option<String>>(4)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let items = match &d.sale_id {
                Some(sid) => {
                    let mut st = c.prepare("SELECT product_name_snapshot, qty_milli, line_total_minor FROM sale_items WHERE sale_id=?1 ORDER BY line_no")?;
                    let rows = st
                        .query_map([sid], |r| Ok(json!({ "name": r.get::<_, String>(0)?, "qty_milli": r.get::<_, i64>(1)?, "line_total_minor": r.get::<_, i64>(2)? })))?
                        .collect::<Result<Vec<_>, _>>()?;
                    rows
                }
                None => vec![],
            };
            Ok(json!({ "delivery": d, "events": events, "items": items }))
        })
    }

    pub fn delivery_create(&self, token: &str, req: DeliveryCreate) -> AppResult<DeliveryRow> {
        let s = self.session(token)?;
        if !s.has("deliveries.manage") && !s.has("pos.sell") {
            return Err(AppError::forbidden("deliveries.manage"));
        }
        let actor = self.actor(&s, None);
        let id = self.db.write(|tx| insert_delivery(tx, &s, &actor, &req))?;
        self.wa_after_delivery(&s, &id, "pending");
        self.db.read(|c| load_delivery(c, &id))
    }

    /// Step a delivery back to the status it had before one forward move
    /// (the compensating command of an AI undo). Needs deliveries.manage.
    pub fn delivery_revert(&self, token: &str, delivery_id: &str, to: &str, note: Option<String>) -> AppResult<DeliveryRow> {
        let s = self.session(token)?;
        s.require("deliveries.manage")?;
        let id = validate::id(delivery_id, "Delivery")?;
        let note = clean_opt(&note, "Note", 500)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let d = load_delivery(tx, &id)?;
            let outcome: Option<String> = tx.query_row("SELECT outcome FROM delivery_orders WHERE delivery_id=?1", [&id], |r| r.get(0))?;
            if outcome.as_deref() == Some("not_delivered") {
                return Err(AppError::conflict("This drop was closed as not delivered and its sale refunded; it cannot be reopened."));
            }
            let last: Option<(Option<String>, String)> = tx
                .query_row(
                    "SELECT previous_status, new_status FROM delivery_events WHERE delivery_id=?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let ok = matches!(&last, Some((Some(prev), new)) if prev == to && *new == d.status);
            if !ok {
                return Err(AppError::conflict(format!("This delivery has moved on since; it cannot go back to {to}.")));
            }
            let now = time::now_str();
            tx.execute(
                "UPDATE delivery_orders SET status=?2, updated_at=?3,
                    dispatched_at=CASE WHEN ?2 IN ('pending','preparing') THEN NULL ELSE dispatched_at END,
                    delivered_at=CASE WHEN ?2='delivered' THEN delivered_at ELSE NULL END
                 WHERE delivery_id=?1",
                params![id, to, now],
            )?;
            tx.execute(
                "INSERT INTO delivery_events(event_id, delivery_id, previous_status, new_status, note, user_id, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![new_id(), id, d.status, to, note, s.user_id, now],
            )?;
            audit::record(tx, &actor, "delivery.reverted", "delivery", Some(&id), Some(&json!({ "status": d.status })), Some(&json!({ "status": to })))?;
            Ok(())
        })?;
        self.db.read(|c| load_delivery(c, &id))
    }

    /// Advance a delivery. Allowed: pending→preparing→dispatched→delivered,
    /// any open state → cancelled.
    pub fn delivery_update(
        &self,
        token: &str,
        delivery_id: &str,
        status: Option<String>,
        assigned_user_id: Option<String>,
        payment_status: Option<String>,
        note: Option<String>,
    ) -> AppResult<DeliveryRow> {
        let s = self.session(token)?;
        if !s.has("pos.sell") {
            s.require("deliveries.view")?;
        }
        let id = validate::id(delivery_id, "Delivery")?;
        let note = clean_opt(&note, "Note", 500)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let d = load_delivery(tx, &id)?;
            let manage = s.has("deliveries.manage");
            // The till moves its own branch's drops forward (never cancels or assigns).
            let till = can_work_drop(&s, &d);
            if !manage && !till && d.assigned_user_id.as_deref() != Some(&s.user_id) {
                return Err(AppError::forbidden("deliveries.manage"));
            }
            let now = time::now_str();
            if let Some(a) = assigned_user_id.as_ref() {
                if !manage {
                    return Err(AppError::forbidden("deliveries.manage"));
                }
                let a = if a.is_empty() { None } else { Some(validate::id(a, "User")?) };
                tx.execute("UPDATE delivery_orders SET assigned_user_id=?2, updated_at=?3 WHERE delivery_id=?1", params![id, a, now])?;
            }
            if let Some(p) = payment_status.as_ref() {
                if !["paid", "pending", "cod"].contains(&p.as_str()) {
                    return Err(AppError::validation("Unknown payment status."));
                }
                tx.execute(
                    "UPDATE delivery_orders SET payment_status=?2, updated_at=?3,
                        pay_state=CASE WHEN ?2='paid' THEN (CASE WHEN pay_state='paid' THEN 'paid' ELSE 'recorded' END) ELSE 'unpaid' END
                     WHERE delivery_id=?1",
                    params![id, p, now],
                )?;
            }
            if let Some(st) = status.as_ref() {
                let ok = matches!(
                    (d.status.as_str(), st.as_str()),
                    ("pending", "preparing") | ("preparing", "dispatched") | ("pending", "dispatched") | ("dispatched", "delivered")
                        | ("pending", "cancelled") | ("preparing", "cancelled") | ("dispatched", "cancelled")
                );
                if !ok {
                    return Err(AppError::conflict(format!("A delivery that is {} cannot become {st}.", d.status)));
                }
                if st == "cancelled" && !manage {
                    return Err(AppError::forbidden("deliveries.manage"));
                }
                tx.execute(
                    "UPDATE delivery_orders SET status=?2, updated_at=?3,
                        dispatched_at=CASE WHEN ?2='dispatched' THEN ?3 ELSE dispatched_at END,
                        delivered_at=CASE WHEN ?2='delivered' THEN ?3 ELSE delivered_at END,
                        failed_note=CASE WHEN ?2='delivered' THEN NULL ELSE failed_note END,
                        failed_at=CASE WHEN ?2='delivered' THEN NULL ELSE failed_at END
                     WHERE delivery_id=?1",
                    params![id, st, now],
                )?;
                tx.execute(
                    "INSERT INTO delivery_events(event_id, delivery_id, previous_status, new_status, note, user_id, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![new_id(), id, d.status, st, note, s.user_id, now],
                )?;
            }
            audit::record(tx, &actor, "delivery.updated", "delivery", Some(&id), Some(&json!({ "status": d.status })),
                Some(&json!({ "status": status, "assigned_user_id": assigned_user_id, "payment_status": payment_status })))?;
            Ok(())
        })?;
        if let Some(st) = status.as_deref() {
            self.wa_after_delivery(&s, &id, st);
        }
        self.db.read(|c| load_delivery(c, &id))
    }
}

/// Bahrain places a drop is sent to. Longer names first so "Riffa East"
/// wins over "Riffa". Each entry: canonical name, then spellings (English
/// matched case-insensitively on word boundaries, and Arabic).
pub const AREAS: &[(&str, &[&str])] = &[
    ("Riffa East", &["riffa east", "east riffa", "الرفاع الشرقي"]),
    ("Riffa West", &["riffa west", "west riffa", "الرفاع الغربي"]),
    ("Isa Town", &["isa town", "isatown", "madinat isa", "مدينة عيسى"]),
    ("Hamad Town", &["hamad town", "hamadtown", "madinat hamad", "مدينة حمد"]),
    ("Riffa", &["riffa", "rifa", "الرفاع"]),
    ("Muharraq", &["muharraq", "muharaq", "المحرق"]),
    ("Manama", &["manama", "المنامة"]),
    ("Sitra", &["sitra", "سترة"]),
    ("A'ali", &["a'ali", "aali", "a’ali", "عالي"]),
    ("Budaiya", &["budaiya", "budaiyah", "البديع"]),
    ("Saar", &["saar", "سار"]),
    ("Juffair", &["juffair", "jufair", "الجفير"]),
    ("Seef", &["seef", "السيف"]),
    ("Amwaj", &["amwaj", "أمواج", "امواج"]),
    ("Tubli", &["tubli", "توبلي"]),
    ("Sanad", &["sanad", "سند"]),
    ("Galali", &["galali", "qalali", "قلالي"]),
    ("Duraz", &["duraz", "diraz", "الدراز"]),
    ("Janabiyah", &["janabiyah", "janabiya", "الجنبية"]),
    ("Hidd", &["hidd", "الحد"]),
    ("Diyya", &["diyya", "diyyah", "الديه"]),
    ("Samaheej", &["samaheej", "سماهيج"]),
];

/// The known area named in free text, e.g. "Maryam 1203/45 Riffa" → Riffa.
/// No match → none (the address is never guessed).
pub fn area_from_text(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '\'' && c != '’');
    for (name, spellings) in AREAS {
        for sp in *spellings {
            let mut from = 0;
            while let Some(i) = lower[from..].find(sp) {
                let start = from + i;
                let end = start + sp.len();
                let before = lower[..start].chars().next_back();
                let after = lower[end..].chars().next();
                // Arabic words may carry a one-letter prefix ("بالرفاع").
                let prefixed = !sp.is_ascii() && matches!(before, Some('ب' | 'و' | 'ل'));
                if (prefixed || boundary(before)) && boundary(after) {
                    return Some(name);
                }
                from = end;
            }
        }
    }
    None
}

/// Text without the area name found in it ("Maryam 1203/45 Riffa" minus Riffa).
pub fn strip_area(text: &str, area: &str) -> String {
    let lower = text.to_lowercase();
    let Some((_, spellings)) = AREAS.iter().find(|(n, _)| *n == area) else { return text.trim().to_string() };
    for sp in *spellings {
        if let Some(i) = lower.find(sp) {
            // to_lowercase keeps byte offsets for the scripts in the lexicon.
            if text.is_char_boundary(i) && text.is_char_boundary(i + sp.len()) {
                let out = format!("{} {}", &text[..i], &text[i + sp.len()..]);
                return out
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .trim_matches(|c: char| c == ',' || c == '-' || c == ' ')
                    .to_string();
            }
        }
    }
    text.trim().to_string()
}

/// A cashier (pos.sell) works the drops of the branch they sell in.
pub(crate) fn can_work_drop(s: &Session, d: &DeliveryRow) -> bool {
    s.has("pos.sell") && d.branch_id.as_deref().is_none_or(|b| b == s.branch_id)
}

/// "Save on customer" from the Send sheet: this drop's address and area.
pub(crate) fn save_drop_address_on_customer(tx: &Connection, customer_id: &str, delivery_id: &str) -> AppResult<()> {
    tx.execute(
        "UPDATE customers SET address=COALESCE(d.address, customers.address), area=COALESCE(d.area, customers.area),
            flat=d.flat, building=d.building, road=d.road, block=d.block, landmark=d.landmark, updated_at=?3
         FROM (SELECT address, area, flat, building, road, block, landmark FROM delivery_orders WHERE delivery_id=?2) AS d
         WHERE customers.customer_id=?1",
        params![customer_id, delivery_id, time::now_str()],
    )?;
    Ok(())
}

/// The one payment-state enum shown on tickets, drops and the WhatsApp header.
pub const PAY_STATES: &[&str] = &["unpaid", "recorded", "screenshot_pending", "paid"];
pub const CHANNELS: &[&str] = &["walk_in", "phone", "whatsapp", "web", "other"];

impl AppCore {
    /// The area a Bahrain block number is in (the shop's own drops first).
    pub fn block_area(&self, token: &str, block: &str) -> AppResult<Option<String>> {
        self.session(token)?;
        self.db.read(|c| crate::address::area_for_block(c, block))
    }
}

/// Insert a delivery inside an open transaction (delivery desk, the Send
/// sale and digital order conversion share this).
pub(crate) fn insert_delivery(tx: &Connection, s: &Session, actor: &audit::Actor, req: &DeliveryCreate) -> AppResult<String> {
    let (sale_id, sale_total, sale_customer) = match req.sale_id.as_ref().filter(|x| !x.is_empty()) {
        Some(sid) => {
            let sid = validate::id(sid, "Sale")?;
            let (t, cu): (i64, Option<String>) = tx
                .query_row("SELECT total_minor, customer_id FROM sales WHERE sale_id=?1", [&sid], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| AppError::not_found("Sale"))?;
            let exists: bool = tx
                .query_row("SELECT 1 FROM delivery_orders WHERE sale_id=?1 AND status<>'cancelled'", [&sid], |_| Ok(true))
                .optional()?
                .unwrap_or(false);
            if exists {
                return Err(AppError::conflict("A delivery already exists for this sale."));
            }
            (Some(sid), Some(t), cu)
        }
        None => (None, None, None),
    };
    let customer_id = match req.customer_id.clone().filter(|x| !x.is_empty()).or(sale_customer) {
        Some(cid) => Some(load_customer(tx, &validate::id(&cid, "Customer")?)?),
        None => None,
    };
    let phone = match req.phone.as_deref().filter(|x| !x.trim().is_empty()) {
        Some(p) => normalize_phone(p)?,
        None => customer_id.as_ref().and_then(|c| c.info.phone.clone()),
    };
    // Parts typed for this drop win; else a typed line; else the customer's saved address.
    let typed_parts = req.address_parts.cleaned()?;
    let typed_line = clean_opt(&req.address, "Address", 300)?;
    let parts = if typed_parts.is_structured() {
        typed_parts
    } else if typed_line.is_none() {
        customer_id.as_ref().map(|c| c.info.address_parts.clone()).unwrap_or_default()
    } else {
        Default::default()
    };
    let address = parts.line().or(typed_line).or_else(|| customer_id.as_ref().and_then(|c| c.info.address.clone()));
    let block_area = match &parts.block {
        Some(b) => crate::address::area_for_block(tx, b)?,
        None => None,
    };
    let area = clean_opt(&req.area, "Area", 80)?
        .or(block_area)
        .or_else(|| customer_id.as_ref().and_then(|c| c.info.area.clone()))
        .or_else(|| address.as_deref().and_then(area_from_text).map(str::to_string));
    if address.is_none() && area.is_none() {
        return Err(AppError::validation("Enter a delivery address or area."));
    }
    let pay = req.payment_status.clone().unwrap_or_else(|| if sale_id.is_some() { "paid".into() } else { "cod".into() });
    if !["paid", "pending", "cod"].contains(&pay.as_str()) {
        return Err(AppError::validation("Payment status must be paid, pending or cash on delivery."));
    }
    let amount = req.amount_minor.or(sale_total).unwrap_or(0);
    validate::money_non_negative(amount, "Amount")?;
    let pay_state = req.pay_state.clone().unwrap_or_else(|| if pay == "paid" { "paid".into() } else { "unpaid".into() });
    if !PAY_STATES.contains(&pay_state.as_str()) {
        return Err(AppError::validation("Unknown payment state."));
    }
    let channel = req.channel.clone().filter(|c| !c.is_empty());
    if let Some(c) = &channel {
        if !CHANNELS.contains(&c.as_str()) {
            return Err(AppError::validation("Unknown channel."));
        }
    }
    let branch: Option<String> = match &sale_id {
        Some(sid) => tx.query_row("SELECT branch_id FROM sales WHERE sale_id=?1", [sid], |r| r.get(0)).optional()?,
        None => Some(s.branch_id.clone()),
    };
    let id = new_id();
    let number = format!("D-{:05}", next_seq(tx, "delivery")?);
    let now = time::now_str();
    tx.execute(
        "INSERT INTO delivery_orders(delivery_id, delivery_number, sale_id, customer_id, address, area, phone, status, payment_status, amount_minor, notes,
            created_by, created_at, updated_at, order_id, branch_id, channel, pay_state)
         VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',?8,?9,?10,?11,?12,?12,?13,?14,?15,?16)",
        params![
            id, number, sale_id, customer_id.map(|c| c.customer_id), address, area, phone, pay, amount, clean_opt(&req.notes, "Notes", 500)?,
            s.user_id, now, req.order_id.clone().filter(|x| !x.is_empty()), branch, channel, pay_state
        ],
    )?;
    crate::address::write_parts(tx, "delivery_orders", "delivery_id", &id, &parts)?;
    tx.execute(
        "INSERT INTO delivery_events(event_id, delivery_id, previous_status, new_status, note, user_id, created_at) VALUES (?1,?2,NULL,'pending',NULL,?3,?4)",
        params![new_id(), id, s.user_id, now],
    )?;
    audit::record(tx, actor, "delivery.created", "delivery", Some(&id), None, Some(&json!({ "number": number, "sale_id": sale_id })))?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::normalize_phone;
    #[test]
    fn phones() {
        assert_eq!(normalize_phone("3312 3456").unwrap().unwrap(), "+97333123456");
        assert_eq!(normalize_phone("+973 3312-3456").unwrap().unwrap(), "+97333123456");
        assert_eq!(normalize_phone("0097333123456").unwrap().unwrap(), "+97333123456");
        assert!(normalize_phone("12").is_err());
        assert!(normalize_phone("").unwrap().is_none());
    }
}
