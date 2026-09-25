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
    Ok(CustomerInput {
        name: clean(&c.name, "Customer name", 120, true)?,
        phone: normalize_phone(c.phone.as_deref().unwrap_or(""))?,
        whatsapp: normalize_phone(c.whatsapp.as_deref().unwrap_or(""))?,
        email,
        area: clean_opt(&c.area, "Area", 80)?,
        address: clean_opt(&c.address, "Address", 300)?,
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
                    active: r.get::<_, i64>(7)? == 1,
                },
                created_at: r.get(8)?,
                purchase_count: r.get(9)?,
                total_spent_minor: r.get(10)?,
                last_purchase_at: r.get(11)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("Customer"))
}

fn load_delivery(c: &Connection, id: &str) -> AppResult<DeliveryRow> {
    c.query_row(
        "SELECT d.delivery_id, d.delivery_number, d.sale_id, s.receipt_number, d.customer_id, cu.name, d.phone, d.area, d.address, d.status,
                d.payment_status, d.amount_minor, d.assigned_user_id, u.display_name, d.notes, d.created_at, d.dispatched_at, d.delivered_at
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
        let v = validate_customer(&input)?;
        let actor = self.actor(&s, None);
        let id = self.db.write(|tx| {
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
                    audit::record(tx, &actor, "customer.updated", "customer", Some(&id), Some(&before), Some(&json!({ "name": v.name })))?;
                    Ok(id)
                }
                None => {
                    let id = new_id();
                    tx.execute(
                        "INSERT INTO customers(customer_id, name, phone, whatsapp, email, area, address, active, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)",
                        params![id, v.name, v.phone, v.whatsapp.clone().or(v.phone.clone()), v.email, v.area, v.address, v.active as i64, now],
                    )?;
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
        s.require("deliveries.view")?;
        let id = validate::id(delivery_id, "Delivery")?;
        let note = clean_opt(&note, "Note", 500)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let d = load_delivery(tx, &id)?;
            let manage = s.has("deliveries.manage");
            if !manage && d.assigned_user_id.as_deref() != Some(&s.user_id) {
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
                tx.execute("UPDATE delivery_orders SET payment_status=?2, updated_at=?3 WHERE delivery_id=?1", params![id, p, now])?;
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
                        delivered_at=CASE WHEN ?2='delivered' THEN ?3 ELSE delivered_at END
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

/// Insert a delivery inside an open transaction (delivery desk and digital
/// order conversion share this).
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
    let address = clean_opt(&req.address, "Address", 300)?.or_else(|| customer_id.as_ref().and_then(|c| c.info.address.clone()));
    let area = clean_opt(&req.area, "Area", 80)?.or_else(|| customer_id.as_ref().and_then(|c| c.info.area.clone()));
    if address.is_none() && area.is_none() {
        return Err(AppError::validation("Enter a delivery address or area."));
    }
    let pay = req.payment_status.clone().unwrap_or_else(|| if sale_id.is_some() { "paid".into() } else { "cod".into() });
    if !["paid", "pending", "cod"].contains(&pay.as_str()) {
        return Err(AppError::validation("Payment status must be paid, pending or cash on delivery."));
    }
    let amount = req.amount_minor.or(sale_total).unwrap_or(0);
    validate::money_non_negative(amount, "Amount")?;
    let id = new_id();
    let number = format!("D-{:05}", next_seq(tx, "delivery")?);
    let now = time::now_str();
    tx.execute(
        "INSERT INTO delivery_orders(delivery_id, delivery_number, sale_id, customer_id, address, area, phone, status, payment_status, amount_minor, notes,
            created_by, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',?8,?9,?10,?11,?12,?12)",
        params![id, number, sale_id, customer_id.map(|c| c.customer_id), address, area, phone, pay, amount, clean_opt(&req.notes, "Notes", 500)?, s.user_id, now],
    )?;
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
