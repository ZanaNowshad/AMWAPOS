//! The Send loop: one record chain from a person to a closed order.
//!
//! Person → channel → **ticket** → **drop** → close.
//!
//! * A *ticket* is an order the shop must fulfil: a digital order (flag
//!   `orders.digital`) or a committed sale the cashier marked *Send*.
//! * A *drop* is the fulfilment job (a `delivery_orders` row), linked to
//!   exactly one ticket; one active drop per ticket.
//! * One payment state everywhere: unpaid | recorded | screenshot_pending |
//!   paid. A screenshot under review shows as `screenshot_pending`; review
//!   and "Record payment" move it on; nothing here settles a bank.
//!
//! The till (pos.sell) sees and works its own branch's drops; the admin
//! board (deliveries.view / deliveries.manage) sees them all in scope.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::new_id;
use crate::service::AppCore;
use crate::settings;
use crate::setup::clean_opt;
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TicketRow {
    /// "drop" (a sale or desk delivery with a drop) or "order" (a digital
    /// order not rung up yet).
    pub kind: String,
    /// delivery_id for a drop, order_id for an order.
    pub ticket_id: String,
    pub delivery_id: Option<String>,
    pub order_id: Option<String>,
    pub sale_id: Option<String>,
    /// Receipt number, order number or drop number (what people say).
    pub number: String,
    pub delivery_number: Option<String>,
    pub customer_id: Option<String>,
    pub customer_name: Option<String>,
    pub phone: Option<String>,
    pub area: Option<String>,
    pub address: Option<String>,
    pub amount_minor: i64,
    /// Still to collect (pay on delivery minus what was collected).
    pub outstanding_minor: i64,
    pub pay_state: String,
    /// Drop: pending | preparing | dispatched | delivered | cancelled.
    /// Order: draft | confirmed.
    pub status: String,
    pub channel: Option<String>,
    pub assigned_user_id: Option<String>,
    pub assigned_name: Option<String>,
    pub branch_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub delivered_at: Option<String>,
    /// not_delivered (the rider could not deliver) | unpaid_out (out or
    /// delivered, money not in) | notice_failed
    pub problem: Option<String>,
    /// Why the rider could not deliver (set by "Unable to deliver").
    #[serde(default)]
    pub failed_note: Option<String>,
    /// How a closed drop ended when not delivered: "not_delivered".
    #[serde(default)]
    pub outcome: Option<String>,
    /// The rider still holding cash collected for this ticket (not yet
    /// counted into a drawer at a hand-over).
    #[serde(default)]
    pub cash_with: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct TicketFilter {
    /// now | out | done | board (default now)
    pub tab: Option<String>,
    pub area: Option<String>,
    pub rider: Option<String>,
    pub pay_state: Option<String>,
    pub channel: Option<String>,
    pub customer_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NotDelivered {
    pub delivery_id: String,
    pub reason: String,
    /// true: the goods go back on the shelf; false: written off (damaged).
    pub restock: bool,
    /// How to give back money already taken for the ticket (cash by
    /// default). What is still to collect is cancelled, not paid out.
    #[serde(default)]
    pub refund_method: Option<String>,
    pub operation_id: String,
    #[serde(default)]
    pub approval_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordPayment {
    pub delivery_id: String,
    pub method: String,
    /// Defaults to everything still outstanding.
    #[serde(default)]
    pub amount_minor: Option<i64>,
    #[serde(default)]
    pub reference: Option<String>,
    pub operation_id: String,
}

const DROP_SELECT: &str = "SELECT d.delivery_id, d.order_id, d.sale_id, COALESCE(s.receipt_number, d.delivery_number), d.delivery_number,
        d.customer_id, cu.name, d.phone, d.area, d.address, d.amount_minor,
        COALESCE(d.pay_state, CASE d.payment_status WHEN 'paid' THEN 'paid' ELSE 'unpaid' END),
        d.status, d.channel, d.assigned_user_id, u.display_name, d.branch_id, d.created_at, d.updated_at, d.delivered_at,
        EXISTS(SELECT 1 FROM payment_reviews r WHERE r.delivery_id=d.delivery_id AND r.status NOT IN ('confirmed','rejected')),
        EXISTS(SELECT 1 FROM wa_outbox w WHERE w.delivery_id=d.delivery_id AND w.status='failed'),
        CASE WHEN d.sale_id IS NULL THEN d.amount_minor
             ELSE COALESCE((SELECT SUM(p.amount_minor) FROM payments p WHERE p.sale_id=d.sale_id AND p.method='pay_on_delivery'),0) END,
        COALESCE((SELECT SUM(k.amount_minor) FROM sale_collections k WHERE k.delivery_id=d.delivery_id),0),
        (SELECT hu.display_name FROM sale_collections hk JOIN users hu ON hu.user_id=hk.held_by
          WHERE hk.delivery_id=d.delivery_id AND hk.collection_id NOT IN (SELECT collection_id FROM rider_handover_items) LIMIT 1),
        d.failed_note, d.outcome
    FROM delivery_orders d LEFT JOIN sales s ON s.sale_id=d.sale_id LEFT JOIN customers cu ON cu.customer_id=d.customer_id
    LEFT JOIN users u ON u.user_id=d.assigned_user_id";

fn drop_row(r: &rusqlite::Row) -> rusqlite::Result<TicketRow> {
    let delivery_id: String = r.get(0)?;
    let stored: String = r.get(11)?;
    let review_open: bool = r.get(20)?;
    let failed: bool = r.get(21)?;
    let due: i64 = r.get(22)?;
    let collected: i64 = r.get(23)?;
    let status: String = r.get(12)?;
    let pay_state = if stored == "unpaid" && review_open { "screenshot_pending".to_string() } else { stored };
    let settled = pay_state == "paid" || pay_state == "recorded";
    let outstanding = if settled { 0 } else { (due - collected).max(0) };
    let failed_note: Option<String> = r.get(25)?;
    let problem = if failed_note.is_some() && matches!(status.as_str(), "pending" | "preparing" | "dispatched") {
        Some("not_delivered".to_string())
    } else if matches!(status.as_str(), "dispatched" | "delivered") && !settled {
        Some("unpaid_out".to_string())
    } else if failed && status != "cancelled" {
        Some("notice_failed".to_string())
    } else {
        None
    };
    Ok(TicketRow {
        kind: "drop".into(),
        ticket_id: delivery_id.clone(),
        delivery_id: Some(delivery_id),
        order_id: r.get(1)?,
        sale_id: r.get(2)?,
        number: r.get(3)?,
        delivery_number: r.get(4)?,
        customer_id: r.get(5)?,
        customer_name: r.get(6)?,
        phone: r.get(7)?,
        area: r.get(8)?,
        address: r.get(9)?,
        amount_minor: r.get(10)?,
        outstanding_minor: outstanding,
        pay_state,
        status,
        channel: r.get(13)?,
        assigned_user_id: r.get(14)?,
        assigned_name: r.get(15)?,
        branch_id: r.get(16)?,
        created_at: r.get(17)?,
        updated_at: r.get(18)?,
        delivered_at: r.get(19)?,
        problem,
        cash_with: r.get(24)?,
        failed_note,
        outcome: r.get(26)?,
    })
}

fn order_select() -> String {
    format!(
        "SELECT o.order_id, o.order_number, o.customer_id, cu.name, COALESCE(o.phone, cu.phone), cu.area,
            COALESCE(o.address, cu.address), o.payment_state, o.status, o.channel, o.branch_id, o.created_at, o.updated_at,
            COALESCE((SELECT SUM(l.qty_milli * COALESCE({price},0) / 1000) FROM digital_order_lines l
                      LEFT JOIN products p ON p.product_id=l.product_id WHERE l.order_id=o.order_id),0)
         FROM digital_orders o LEFT JOIN customers cu ON cu.customer_id=o.customer_id",
        price = crate::catalog::PRICE_SQL
    )
}

fn order_row(r: &rusqlite::Row) -> rusqlite::Result<TicketRow> {
    let order_id: String = r.get(0)?;
    let amount: i64 = r.get(13)?;
    Ok(TicketRow {
        kind: "order".into(),
        ticket_id: order_id.clone(),
        delivery_id: None,
        order_id: Some(order_id),
        sale_id: None,
        number: r.get(1)?,
        delivery_number: None,
        customer_id: r.get(2)?,
        customer_name: r.get(3)?,
        phone: r.get(4)?,
        area: r.get(5)?,
        address: r.get(6)?,
        amount_minor: amount,
        outstanding_minor: amount,
        pay_state: r.get(7)?,
        status: r.get(8)?,
        channel: r.get(9)?,
        assigned_user_id: None,
        assigned_name: None,
        branch_id: r.get(10)?,
        created_at: r.get(11)?,
        updated_at: r.get(12)?,
        delivered_at: None,
        problem: None,
        cash_with: None,
        failed_note: None,
        outcome: None,
    })
}

pub(crate) fn load_ticket(c: &Connection, delivery_id: &str) -> AppResult<TicketRow> {
    c.query_row(&format!("{DROP_SELECT} WHERE d.delivery_id=?1"), [delivery_id], drop_row)
        .optional()?
        .ok_or_else(|| AppError::not_found("Ticket"))
}

/// Start of today (business timezone) as a UTC timestamp.
fn today_start(c: &Connection) -> AppResult<String> {
    let tz: String =
        c.query_row("SELECT timezone FROM business LIMIT 1", [], |r| r.get(0)).optional()?.unwrap_or_else(|| "Asia/Bahrain".into());
    let day = time::business_date(time::now(), &tz)?;
    Ok(time::local_date_range_utc(&day, &day, &tz)?.0)
}

/// Which drops this person sees: None = every branch in their scope.
struct Scope {
    branch: Option<String>,
    only_assigned: Option<String>,
}

fn scope(c: &Connection, s: &Session) -> AppResult<Scope> {
    if s.has("deliveries.manage") {
        return Ok(Scope { branch: crate::branches::list_scope(c, s)?, only_assigned: None });
    }
    if s.has("pos.sell") {
        return Ok(Scope { branch: Some(s.branch_id.clone()), only_assigned: None });
    }
    if s.has("deliveries.view") {
        // A rider sees the drops given to them.
        return Ok(Scope { branch: None, only_assigned: Some(s.user_id.clone()) });
    }
    Err(AppError::forbidden("deliveries.view"))
}

/// Legal next statuses for a drop (forward only; cancel with deliveries.manage).
pub fn next_statuses(status: &str, manage: bool) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = match status {
        "pending" => vec!["preparing", "dispatched"],
        "preparing" => vec!["dispatched"],
        "dispatched" => vec!["delivered"],
        _ => vec![],
    };
    if manage && matches!(status, "pending" | "preparing" | "dispatched") {
        v.push("cancelled");
    }
    v
}

impl AppCore {
    fn orders_on(&self) -> bool {
        self.features().map(|f| f.is_on("orders.digital")).unwrap_or(false)
    }

    /// Tickets for the till's Send rail (tabs now / out / done) and the admin
    /// board (tab "board": everything open plus today's closed ones).
    pub fn tickets_list(&self, token: &str, filter: TicketFilter) -> AppResult<Vec<TicketRow>> {
        let s = self.session(token)?;
        let orders_on = self.orders_on();
        let tab = filter.tab.clone().unwrap_or_else(|| "now".into());
        if !["now", "out", "done", "board"].contains(&tab.as_str()) {
            return Err(AppError::validation("Tab must be now, out, done or board."));
        }
        self.db.read(|c| {
            let sc = scope(c, &s)?;
            let today = today_start(c)?;
            let statuses = match tab.as_str() {
                "now" => "('pending','preparing')",
                "out" => "('dispatched')",
                "done" => "('delivered','cancelled')",
                _ => "('pending','preparing','dispatched','delivered','cancelled')",
            };
            // Closed drops count for today only, except unpaid ones on the board (Problem).
            let sql = format!(
                "{DROP_SELECT} WHERE d.status IN {statuses}
                   AND (d.status IN ('pending','preparing','dispatched') OR d.updated_at >= ?1
                        OR (?7 = 1 AND d.status='delivered' AND COALESCE(d.pay_state,'unpaid') NOT IN ('paid','recorded')))
                   AND (?2 IS NULL OR d.branch_id IS NULL OR d.branch_id=?2)
                   AND (?3 IS NULL OR d.assigned_user_id=?3)
                   AND (?4 IS NULL OR d.area=?4)
                   AND (?5 IS NULL OR d.assigned_user_id=?5)
                   AND (?6 IS NULL OR d.channel=?6)
                   AND (?8 IS NULL OR d.customer_id=?8)
                 ORDER BY d.created_at DESC LIMIT 500"
            );
            let blank = |v: &Option<String>| v.clone().filter(|x| !x.trim().is_empty());
            let mut st = c.prepare(&sql)?;
            let mut rows = st
                .query_map(
                    params![
                        today,
                        sc.branch,
                        sc.only_assigned,
                        blank(&filter.area),
                        blank(&filter.rider),
                        blank(&filter.channel),
                        (tab == "board") as i64,
                        blank(&filter.customer_id)
                    ],
                    drop_row,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            // Digital orders not rung up yet wait in Now as draft tickets.
            if orders_on && (tab == "now" || tab == "board") && sc.only_assigned.is_none() {
                let mut st = c.prepare(&format!(
                    "{} WHERE o.status IN ('draft','confirmed') AND (?1 IS NULL OR o.branch_id=?1)
                       AND (?2 IS NULL OR o.channel=?2) AND (?3 IS NULL OR o.customer_id=?3)
                     ORDER BY o.created_at DESC LIMIT 200",
                    order_select()
                ))?;
                let orders = st
                    .query_map(params![sc.branch, blank(&filter.channel), blank(&filter.customer_id)], order_row)?
                    .collect::<Result<Vec<_>, _>>()?;
                rows.extend(orders.into_iter().filter(|o| blank(&filter.area).is_none_or(|a| o.area.as_deref() == Some(a.as_str()))));
            }
            if let Some(p) = blank(&filter.pay_state) {
                rows.retain(|r| r.pay_state == p);
            }
            rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            Ok(rows)
        })
    }

    /// Top-bar badge and tab counts for the till.
    pub fn tickets_counts(&self, token: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        let orders_on = self.orders_on();
        self.db.read(|c| {
            let sc = scope(c, &s)?;
            let today = today_start(c)?;
            let q = |sql: &str| -> AppResult<i64> {
                Ok(c.query_row(sql, params![sc.branch, sc.only_assigned, today, s.user_id], |r| r.get(0))?)
            };
            let base = "FROM delivery_orders d WHERE (?1 IS NULL OR d.branch_id IS NULL OR d.branch_id=?1) AND (?2 IS NULL OR d.assigned_user_id=?2)
                        AND ?3 IS NOT NULL AND ?4 IS NOT NULL";
            let now = q(&format!("SELECT COUNT(*) {base} AND d.status IN ('pending','preparing')"))?;
            let out = q(&format!("SELECT COUNT(*) {base} AND d.status='dispatched'"))?;
            let done = q(&format!("SELECT COUNT(*) {base} AND d.status IN ('delivered','cancelled') AND d.updated_at >= ?3"))?;
            // Badge: open drops for this person or nobody yet.
            let badge = q(&format!(
                "SELECT COUNT(*) {base} AND d.status IN ('pending','preparing','dispatched') AND (d.assigned_user_id IS NULL OR d.assigned_user_id=?4)"
            ))?;
            let orders: i64 = if orders_on && sc.only_assigned.is_none() {
                c.query_row(
                    "SELECT COUNT(*) FROM digital_orders WHERE status IN ('draft','confirmed') AND (?1 IS NULL OR branch_id=?1)",
                    params![sc.branch],
                    |r| r.get(0),
                )?
            } else {
                0
            };
            Ok(json!({ "badge": badge + orders, "now": now + orders, "out": out, "done": done }))
        })
    }

    /// The order journey at a glance, for the guide bar on the order pages:
    /// chats to read, orders to confirm, orders to pack, on the way, and
    /// payments to check. A step this person may not see (or whose module is
    /// off) is null, so the bar shows only what they can act on.
    pub fn orders_flow(&self, token: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        let f = self.features()?;
        let wa = f.is_on("orders.whatsapp_ai") && ["orders.manage", "whatsapp.manage", "whatsapp.send"].iter().any(|p| s.has(p));
        let orders = f.is_on("orders.digital") && (s.has("orders.manage") || s.has("pos.sell"));
        let drops = ["deliveries.manage", "deliveries.view", "pos.sell"].iter().any(|p| s.has(p));
        let pay = s.has("whatsapp.manage") || s.has("payments.review");
        self.db.read(|c| {
            let n = |sql: &str, p: &[&dyn rusqlite::ToSql]| -> AppResult<i64> { Ok(c.query_row(sql, p, |r| r.get(0))?) };
            let branch = crate::branches::list_scope(c, &s)?;
            let chats = if wa {
                Some(n("SELECT COUNT(*) FROM wa_order_sessions WHERE state IN ('collecting','clarifying','ready')", &[])?)
            } else {
                None
            };
            let waiting = if wa {
                Some(n("SELECT COUNT(*) FROM wa_order_sessions WHERE state IN ('collecting','clarifying','ready') AND handled=0", &[])?)
            } else {
                None
            };
            let to_confirm = if orders {
                Some(n("SELECT COUNT(*) FROM digital_orders WHERE status='draft' AND (?1 IS NULL OR branch_id=?1)", &[&branch])?)
            } else {
                None
            };
            let (to_pack, out) = if drops {
                let sc = scope(c, &s)?;
                let base = "FROM delivery_orders d WHERE (?1 IS NULL OR d.branch_id IS NULL OR d.branch_id=?1) AND (?2 IS NULL OR d.assigned_user_id=?2)";
                let pack = n(&format!("SELECT COUNT(*) {base} AND d.status IN ('pending','preparing')"), &[&sc.branch, &sc.only_assigned])?;
                let ready = if orders && sc.only_assigned.is_none() {
                    n("SELECT COUNT(*) FROM digital_orders WHERE status='confirmed' AND (?1 IS NULL OR branch_id=?1)", &[&sc.branch])?
                } else {
                    0
                };
                (Some(pack + ready), Some(n(&format!("SELECT COUNT(*) {base} AND d.status='dispatched'"), &[&sc.branch, &sc.only_assigned])?))
            } else {
                (None, None)
            };
            let payments = if pay {
                Some(n("SELECT COUNT(*) FROM payment_reviews WHERE status IN ('pending','matched','mismatch','needs_review')", &[])?)
            } else {
                None
            };
            Ok(json!({ "chats": chats, "waiting": waiting, "to_confirm": to_confirm, "to_pack": to_pack, "out": out, "payments": payments }))
        })
    }

    /// One ticket with everything the sheet shows: lines, drop, payments,
    /// screenshots, notices, and what this person may do next.
    pub fn ticket_get(&self, token: &str, id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        let id = validate::id(id, "Ticket")?;
        let orders_on = self.orders_on();
        let wa_on = self.features().map(|f| f.is_on("whatsapp.delivery_notices")).unwrap_or(false);
        self.db.read(|c| {
            let sc = scope(c, &s)?;
            let manage = s.has("deliveries.manage");
            let ticket = match c.query_row(&format!("{DROP_SELECT} WHERE d.delivery_id=?1"), [&id], drop_row).optional()? {
                Some(t) => t,
                None => {
                    // A digital order: its drop when rung up, else the order itself.
                    let drop: Option<String> = c
                        .query_row(
                            "SELECT delivery_id FROM delivery_orders WHERE order_id=?1 AND status<>'cancelled' ORDER BY created_at DESC LIMIT 1",
                            [&id],
                            |r| r.get(0),
                        )
                        .optional()?;
                    match drop {
                        Some(d) => load_ticket(c, &d)?,
                        None if orders_on => c
                            .query_row(&format!("{} WHERE o.order_id=?1", order_select()), [&id], order_row)
                            .optional()?
                            .ok_or_else(|| AppError::not_found("Ticket"))?,
                        None => return Err(AppError::not_found("Ticket")),
                    }
                }
            };
            let visible = match (&sc.branch, &sc.only_assigned) {
                (_, Some(u)) => ticket.assigned_user_id.as_deref() == Some(u.as_str()),
                (Some(b), None) => ticket.branch_id.as_deref().is_none_or(|x| x == b),
                (None, None) => true,
            };
            if !visible {
                return Err(AppError::not_found("Ticket"));
            }
            let lines: Vec<Value> = match (&ticket.sale_id, &ticket.order_id) {
                (Some(sid), _) => {
                    let mut st =
                        c.prepare("SELECT product_name_snapshot, qty_milli, line_total_minor FROM sale_items WHERE sale_id=?1 ORDER BY line_no")?;
                    let rows = st
                        .query_map([sid], |r| {
                            Ok(json!({ "name": r.get::<_, String>(0)?, "qty_milli": r.get::<_, i64>(1)?, "line_total_minor": r.get::<_, i64>(2)? }))
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                    rows
                }
                (None, Some(oid)) => {
                    let mut st = c.prepare(&format!(
                        "SELECT COALESCE(p.name, l.description), l.qty_milli, l.qty_milli * COALESCE({},0) / 1000
                         FROM digital_order_lines l LEFT JOIN products p ON p.product_id=l.product_id WHERE l.order_id=?1 ORDER BY l.line_no",
                        crate::catalog::PRICE_SQL
                    ))?;
                    let rows = st
                        .query_map([oid], |r| {
                            Ok(json!({ "name": r.get::<_, String>(0)?, "qty_milli": r.get::<_, i64>(1)?, "line_total_minor": r.get::<_, i64>(2)? }))
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                    rows
                }
                _ => vec![],
            };
            let did = ticket.delivery_id.clone();
            let list = |sql: &str, key: &Option<String>, f: &dyn Fn(&rusqlite::Row) -> rusqlite::Result<Value>| -> AppResult<Vec<Value>> {
                match key {
                    Some(k) => {
                        let mut st = c.prepare(sql)?;
                        let rows = st.query_map([k], |r| f(r))?.collect::<Result<Vec<_>, _>>()?;
                        Ok(rows)
                    }
                    None => Ok(vec![]),
                }
            };
            let events = list(
                "SELECT e.previous_status, e.new_status, e.note, e.created_at, u.display_name FROM delivery_events e
                 LEFT JOIN users u ON u.user_id=e.user_id WHERE e.delivery_id=?1 ORDER BY e.created_at, e.rowid",
                &did,
                &|r| {
                    Ok(json!({ "from": r.get::<_, Option<String>>(0)?, "to": r.get::<_, String>(1)?, "note": r.get::<_, Option<String>>(2)?,
                        "at": r.get::<_, String>(3)?, "user": r.get::<_, Option<String>>(4)? }))
                },
            )?;
            let payments = list("SELECT method, amount_minor, reference FROM payments WHERE sale_id=?1 ORDER BY rowid", &ticket.sale_id, &|r| {
                Ok(json!({ "method": r.get::<_, String>(0)?, "amount_minor": r.get::<_, i64>(1)?, "reference": r.get::<_, Option<String>>(2)? }))
            })?;
            let collections = list(
                "SELECT k.method, k.amount_minor, k.reference, k.created_at, u.display_name, hu.display_name,
                    EXISTS(SELECT 1 FROM rider_handover_items hi WHERE hi.collection_id=k.collection_id)
                 FROM sale_collections k LEFT JOIN users u ON u.user_id=k.user_id LEFT JOIN users hu ON hu.user_id=k.held_by
                 WHERE k.delivery_id=?1 ORDER BY k.created_at",
                &did,
                &|r| {
                    Ok(json!({ "method": r.get::<_, String>(0)?, "amount_minor": r.get::<_, i64>(1)?, "reference": r.get::<_, Option<String>>(2)?,
                        "at": r.get::<_, String>(3)?, "user": r.get::<_, Option<String>>(4)?,
                        "held_by": r.get::<_, Option<String>>(5)?, "handed_over": r.get::<_, bool>(6)? }))
                },
            )?;
            let reviews = list(
                "SELECT review_id, review_number, status, detected_minor, created_at FROM payment_reviews WHERE delivery_id=?1 ORDER BY created_at DESC",
                &did,
                &|r| {
                    Ok(json!({ "review_id": r.get::<_, String>(0)?, "review_number": r.get::<_, String>(1)?, "status": r.get::<_, String>(2)?,
                        "detected_minor": r.get::<_, Option<i64>>(3)?, "at": r.get::<_, String>(4)? }))
                },
            )?;
            let notices = list(
                "SELECT message_id, kind, status, last_error, created_at FROM wa_outbox WHERE delivery_id=?1 ORDER BY created_at DESC LIMIT 20",
                &did,
                &|r| {
                    Ok(json!({ "message_id": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?, "status": r.get::<_, String>(2)?,
                        "error": r.get::<_, Option<String>>(3)?, "at": r.get::<_, String>(4)? }))
                },
            )?;
            let works = manage
                || (s.has("pos.sell") && ticket.branch_id.as_deref().is_none_or(|b| b == s.branch_id))
                || (ticket.assigned_user_id.as_deref() == Some(s.user_id.as_str()));
            let is_drop = ticket.kind == "drop";
            let open = matches!(ticket.status.as_str(), "pending" | "preparing" | "dispatched");
            let next: Vec<&str> = if is_drop && works { next_statuses(&ticket.status, manage) } else { vec![] };
            let riders: Vec<Value> = if manage && is_drop {
                let mut st = c.prepare(
                    "SELECT DISTINCT u.user_id, u.display_name FROM users u JOIN role_permissions rp ON rp.role_id=u.role_id
                     WHERE u.active=1 AND rp.permission_code IN ('deliveries.view','deliveries.manage') ORDER BY u.display_name COLLATE NOCASE",
                )?;
                let rows = st.query_map([], |r| Ok(json!({ "user_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)? })))?.collect::<Result<Vec<_>, _>>()?;
                rows
            } else {
                vec![]
            };
            let settled = ticket.pay_state == "paid" || ticket.pay_state == "recorded";
            Ok(json!({
                "ticket": ticket,
                "lines": lines,
                "events": events,
                "payments": payments,
                "collections": collections,
                "reviews": reviews,
                "notices": notices,
                "next": next,
                "riders": riders,
                "can": {
                    "advance": !next.is_empty(),
                    "cancel": next.contains(&"cancelled"),
                    "assign": manage && is_drop && open,
                    "record_payment": is_drop && works && !settled && ticket.status != "cancelled" && ticket.outstanding_minor > 0,
                    "attach_screenshot": is_drop && s.has("payments.review") && !settled,
                    "message": is_drop && wa_on && (s.has("whatsapp.send") || s.has("whatsapp.manage")),
                    "ring_up": ticket.kind == "order" && s.has("pos.sell"),
                    "undo": is_drop && manage && ticket.outcome.is_none(),
                    "unable": is_drop && works && open && ticket.failed_note.is_none(),
                    "not_delivered": is_drop && manage && open,
                },
            }))
        })
    }

    /// Record money for a pay-on-delivery ticket (at the door or later at the
    /// till). Idempotent on `operation_id`. Cash goes into this shift's drawer.
    pub fn ticket_record_payment(&self, token: &str, req: RecordPayment) -> AppResult<TicketRow> {
        let s = self.session(token)?;
        let id = validate::id(&req.delivery_id, "Ticket")?;
        let method = req.method.trim().to_string();
        if method == crate::sales::PAY_ON_DELIVERY || method == "account" || method.is_empty() {
            return Err(AppError::validation("Choose how the customer paid."));
        }
        let pay_cfg: settings::PaymentSettings = self.db.read(settings::payments)?;
        let cfg =
            pay_cfg.tender(&method).filter(|t| t.enabled).cloned().ok_or_else(|| {
                AppError::validation(format!("{} is not an enabled payment method.", crate::receipt::method_label(&method)))
            })?;
        let reference = clean_opt(&req.reference, "Reference", 60)?;
        if cfg.requires_reference && reference.is_none() {
            return Err(AppError::validation(format!("Enter the {} reference.", cfg.label)));
        }
        let device = self.require_device()?;
        let actor = self.actor(&s, None);
        let payload = json!({ "delivery_id": id, "method": method, "amount_minor": req.amount_minor, "reference": reference });
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "ticket.record_payment", &payload)? {
                Check::Replay { .. } => return Ok(()),
                Check::New { payload_hash } => payload_hash,
            };
            let t = load_ticket(tx, &id)?;
            let d = crate::customers::load_delivery_row(tx, &id)?;
            if !s.has("deliveries.manage") && !crate::customers::can_work_drop(&s, &d) && d.assigned_user_id.as_deref() != Some(&s.user_id) {
                return Err(AppError::forbidden("pos.sell"));
            }
            if t.status == "cancelled" {
                return Err(AppError::conflict("This ticket is cancelled."));
            }
            if t.outstanding_minor <= 0 {
                return Err(AppError::conflict("Nothing is left to collect on this ticket."));
            }
            let amount = req.amount_minor.unwrap_or(t.outstanding_minor);
            if amount <= 0 {
                return Err(AppError::validation("The amount must be greater than zero."));
            }
            if amount > t.outstanding_minor {
                return Err(AppError::validation("That is more than is left to collect.")
                    .with_details(json!({ "outstanding_minor": t.outstanding_minor })));
            }
            let shift = crate::sales::open_shift_for(tx, &s)?;
            // The rider on the drop, with no drawer of their own, keeps the
            // cash until a cashier counts it in at a hand-over.
            let held_by = (method == "cash" && shift.is_none() && d.assigned_user_id.as_deref() == Some(s.user_id.as_str()))
                .then(|| s.user_id.clone());
            if method == "cash" && shift.is_none() && held_by.is_none() {
                return Err(crate::sales::shift_required());
            }
            let now = time::now_str();
            let cid = new_id();
            tx.execute(
                "INSERT INTO sale_collections(collection_id, sale_id, delivery_id, method, amount_minor, reference, shift_id, branch_id, device_id, user_id,
                    operation_id, created_at, held_by) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    cid,
                    t.sale_id,
                    id,
                    method,
                    amount,
                    reference,
                    if held_by.is_some() { None } else { shift.clone() },
                    s.branch_id,
                    device.device_id,
                    s.user_id,
                    req.operation_id,
                    now,
                    held_by
                ],
            )?;
            let full = amount == t.outstanding_minor;
            if full {
                // Cash or card taken by the shop is paid; anything else (a
                // transfer reference) is recorded until someone checks the bank.
                let state = if method == "cash" || method == "card" { "paid" } else { "recorded" };
                tx.execute(
                    "UPDATE delivery_orders SET pay_state=?2, payment_status='paid', updated_at=?3 WHERE delivery_id=?1",
                    params![id, state, now],
                )?;
            }
            let label = crate::receipt::method_label(&method);
            let note = if held_by.is_some() {
                format!("Cash collected at the door: {amount} (with the rider)")
            } else {
                format!("Payment recorded: {label} {amount}")
            };
            tx.execute(
                "INSERT INTO delivery_events(event_id, delivery_id, previous_status, new_status, note, user_id, created_at) VALUES (?1,?2,?3,?3,?4,?5,?6)",
                params![new_id(), id, t.status, note, s.user_id, now],
            )?;
            audit::record(
                tx,
                &actor,
                "ticket.payment_recorded",
                "delivery",
                Some(&id),
                Some(&json!({ "pay_state": t.pay_state, "outstanding_minor": t.outstanding_minor })),
                Some(&json!({ "method": method, "amount_minor": amount, "collection_id": cid, "shift_id": shift, "held_by": held_by })),
            )?;
            if method == "cash" && held_by.is_none() {
                crate::printing::enqueue_drawer_pulse(tx, Some(&s.user_id), &cid)?;
            }
            idempotency::complete(
                tx,
                &req.operation_id,
                "ticket.record_payment",
                Some(&s.user_id),
                Some(&device.device_id),
                &hash,
                Some(&cid),
                &json!({ "collection_id": cid }),
            )?;
            Ok(())
        })?;
        self.db.read(|c| load_ticket(c, &id))
    }

    /// "Unable to deliver": the rider (or the till) flags an open drop they
    /// could not deliver. The drop stays open and shows as a problem until a
    /// manager closes it as not delivered, or it is delivered after all.
    pub fn ticket_unable(&self, token: &str, delivery_id: &str, reason: &str) -> AppResult<TicketRow> {
        let s = self.session(token)?;
        let id = validate::id(delivery_id, "Ticket")?;
        let reason = crate::setup::clean(reason, "Reason", 200, true)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let d = crate::customers::load_delivery_row(tx, &id)?;
            if !s.has("deliveries.manage") && !crate::customers::can_work_drop(&s, &d) && d.assigned_user_id.as_deref() != Some(&s.user_id) {
                return Err(AppError::forbidden("deliveries.view"));
            }
            if !matches!(d.status.as_str(), "pending" | "preparing" | "dispatched") {
                return Err(AppError::conflict("Only an open drop can be marked as not delivered."));
            }
            let now = time::now_str();
            tx.execute("UPDATE delivery_orders SET failed_note=?2, failed_at=?3, updated_at=?3 WHERE delivery_id=?1", params![id, reason, now])?;
            tx.execute(
                "INSERT INTO delivery_events(event_id, delivery_id, previous_status, new_status, note, user_id, created_at) VALUES (?1,?2,?3,?3,?4,?5,?6)",
                params![new_id(), id, d.status, format!("Unable to deliver: {reason}"), s.user_id, now],
            )?;
            audit::record(tx, &actor, "delivery.unable", "delivery", Some(&id), None, Some(&json!({ "reason": reason })))?;
            Ok(())
        })?;
        self.db.read(|c| load_ticket(c, &id))
    }

    /// Close a drop as not delivered: refund what is left of its sale (the
    /// amount still to collect is cancelled; money already taken goes back by
    /// `refund_method`), goods back on the shelf or written off, and the drop
    /// cancelled with outcome `not_delivered`. Needs deliveries.manage and
    /// refund rights (or a manager's approval). Retry-safe on `operation_id`.
    pub fn ticket_not_delivered(&self, token: &str, req: NotDelivered) -> AppResult<TicketRow> {
        let s = self.session(token)?;
        s.require("deliveries.manage")?;
        let id = validate::id(&req.delivery_id, "Ticket")?;
        let reason = crate::setup::clean(&req.reason, "Reason", 200, true)?;
        idempotency::validate_operation_id(&req.operation_id)?;
        let t = self.db.read(|c| load_ticket(c, &id))?;
        if t.outcome.as_deref() == Some("not_delivered") {
            return Ok(t);
        }
        if !matches!(t.status.as_str(), "pending" | "preparing" | "dispatched") {
            return Err(AppError::conflict("Only an open drop can be closed as not delivered."));
        }
        if let Some(r) = &t.cash_with {
            return Err(AppError::conflict(format!("{r} still holds the cash for this ticket. Do the rider hand-over first.")));
        }
        // The refund: every line still refundable, one call to the refund
        // engine (its own operation id derived from this one, so a retry
        // replays it instead of refunding twice).
        let mut refund_id: Option<String> = None;
        if let Some(sid) = &t.sale_id {
            let (lines, total, pod_paid): (Vec<crate::refunds::RefundLineInput>, i64, i64) = self.db.read(|c| {
                let mut st = c.prepare(
                    "SELECT i.sale_item_id, i.qty_milli - COALESCE((SELECT SUM(r.qty_milli) FROM refund_items r WHERE r.original_sale_item_id=i.sale_item_id),0),
                        i.line_total_minor - COALESCE((SELECT SUM(r.amount_minor) FROM refund_items r WHERE r.original_sale_item_id=i.sale_item_id),0)
                     FROM sale_items i WHERE i.sale_id=?1 ORDER BY i.line_no",
                )?;
                let rows: Vec<(String, i64, i64)> = st.query_map([sid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
                let lines = rows
                    .iter()
                    .filter(|r| r.1 > 0)
                    .map(|r| crate::refunds::RefundLineInput { sale_item_id: r.0.clone(), qty_milli: r.1, restock: req.restock })
                    .collect();
                let total = rows.iter().filter(|r| r.1 > 0).map(|r| r.2).sum();
                let pod: i64 = c.query_row(
                    "SELECT COALESCE(SUM(amount_minor),0) FROM payments WHERE sale_id=?1 AND method='pay_on_delivery'",
                    [sid],
                    |r| r.get(0),
                )?;
                Ok((lines, total, pod))
            })?;
            if !lines.is_empty() {
                // Money never taken is cancelled against the pay-on-delivery
                // tender; the rest is paid back. The exact line prorating is
                // done by the refund engine, so the split is checked there.
                let uncollected = t.outstanding_minor.min(pod_paid).min(total).max(0);
                let method = req.refund_method.clone().filter(|m| !m.trim().is_empty()).unwrap_or_else(|| "cash".into());
                let mut tenders = vec![];
                if uncollected > 0 {
                    tenders.push(crate::refunds::RefundTenderInput {
                        method: crate::sales::PAY_ON_DELIVERY.into(),
                        amount_minor: uncollected,
                        reference: None,
                    });
                }
                if total - uncollected > 0 {
                    tenders.push(crate::refunds::RefundTenderInput { method, amount_minor: total - uncollected, reference: None });
                }
                let r = self.refund_create(
                    token,
                    crate::refunds::RefundRequest {
                        sale_id: sid.clone(),
                        lines,
                        reason: format!("Not delivered: {reason}"),
                        tenders,
                        operation_id: format!("{}-nd", req.operation_id),
                        approval_token: req.approval_token.clone(),
                    },
                )?;
                refund_id = Some(r.refund_id);
            }
        }
        let actor = self.actor(&s, None);
        let payload = json!({ "delivery_id": id, "reason": reason, "restock": req.restock });
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "ticket.not_delivered", &payload)? {
                Check::Replay { .. } => return Ok(()),
                Check::New { payload_hash } => payload_hash,
            };
            let d = crate::customers::load_delivery_row(tx, &id)?;
            if !matches!(d.status.as_str(), "pending" | "preparing" | "dispatched") {
                return Err(AppError::conflict("Only an open drop can be closed as not delivered."));
            }
            let now = time::now_str();
            tx.execute(
                "UPDATE delivery_orders SET status='cancelled', outcome='not_delivered', refund_id=COALESCE(?2, refund_id),
                    failed_note=COALESCE(failed_note, ?3), updated_at=?4 WHERE delivery_id=?1",
                params![id, refund_id, reason, now],
            )?;
            let note = format!("Not delivered: {reason}. {}", if req.restock { "Goods back on the shelf." } else { "Goods written off as damaged." });
            tx.execute(
                "INSERT INTO delivery_events(event_id, delivery_id, previous_status, new_status, note, user_id, created_at) VALUES (?1,?2,?3,'cancelled',?4,?5,?6)",
                params![new_id(), id, d.status, note, s.user_id, now],
            )?;
            audit::record(
                tx,
                &actor,
                "delivery.not_delivered",
                "delivery",
                Some(&id),
                Some(&json!({ "status": d.status })),
                Some(&json!({ "reason": reason, "restock": req.restock, "refund_id": refund_id })),
            )?;
            idempotency::complete(tx, &req.operation_id, "ticket.not_delivered", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &json!({}))?;
            Ok(())
        })?;
        self.db.read(|c| load_ticket(c, &id))
    }

    /// Link a WhatsApp chat to a customer by hand (or unlink with none). A
    /// number match needs no link.
    pub fn wa_link_customer(&self, token: &str, chat: &str, customer_id: Option<String>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("whatsapp.manage")?;
        let chat = chat.trim();
        if chat.is_empty() || chat.len() > 200 {
            return Err(AppError::validation("Choose a conversation."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            match customer_id.as_deref().filter(|x| !x.is_empty()) {
                Some(cid) => {
                    let cid = validate::id(cid, "Customer")?;
                    let exists: bool = tx.query_row("SELECT 1 FROM customers WHERE customer_id=?1", [&cid], |_| Ok(true)).optional()?.unwrap_or(false);
                    if !exists {
                        return Err(AppError::not_found("Customer"));
                    }
                    tx.execute(
                        "INSERT INTO wa_chat_links(chat, customer_id, linked_by, linked_at) VALUES (?1,?2,?3,?4)
                         ON CONFLICT(chat) DO UPDATE SET customer_id=excluded.customer_id, linked_by=excluded.linked_by, linked_at=excluded.linked_at",
                        params![chat, cid, s.user_id, time::now_str()],
                    )?;
                    tx.execute("UPDATE wa_inbox SET customer_id=?2 WHERE chat=?1", params![chat, cid])?;
                    audit::record(tx, &actor, "whatsapp.chat_linked", "customer", Some(&cid), None, Some(&json!({ "chat": chat })))?;
                }
                None => {
                    tx.execute("DELETE FROM wa_chat_links WHERE chat=?1", [chat])?;
                    tx.execute(
                        "UPDATE wa_inbox SET customer_id=(SELECT cu.customer_id FROM customers cu WHERE wa_inbox.phone IS NOT NULL
                            AND (cu.whatsapp=wa_inbox.phone OR cu.phone=wa_inbox.phone) ORDER BY cu.active DESC LIMIT 1) WHERE chat=?1",
                        [chat],
                    )?;
                    audit::record(tx, &actor, "whatsapp.chat_unlinked", "whatsapp_chat", None, None, Some(&json!({ "chat": chat })))?;
                }
            }
            Ok(())
        })?;
        self.wa_thread_context(token, chat)
    }

    /// The person behind a WhatsApp chat: the linked or number-matched
    /// customer (never the raw chat id), their last ticket and open ones.
    pub fn wa_thread_context(&self, token: &str, chat: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("whatsapp.manage")?;
        let orders_on = self.orders_on();
        let chat = chat.trim().to_string();
        let (customer, how, phone, push) = self.db.read(|c| {
            let push: Option<String> =
                c.query_row("SELECT push_name FROM wa_inbox WHERE chat=?1 AND push_name IS NOT NULL ORDER BY seq DESC LIMIT 1", [&chat], |r| r.get(0))
                    .optional()?;
            let phone: Option<String> = c
                .query_row("SELECT phone FROM wa_inbox WHERE chat=?1 AND phone IS NOT NULL ORDER BY seq DESC LIMIT 1", [&chat], |r| r.get(0))
                .optional()?
                .or_else(|| crate::wa_contacts::phone_of(&chat));
            let linked: Option<String> = c.query_row("SELECT customer_id FROM wa_chat_links WHERE chat=?1", [&chat], |r| r.get(0)).optional()?;
            let (cid, how) = match linked {
                Some(l) => (Some(l), Some("linked")),
                None => match &phone {
                    Some(p) => {
                        let m: Option<String> = c
                            .query_row(
                                "SELECT customer_id FROM customers WHERE whatsapp=?1 OR phone=?1 ORDER BY active DESC, updated_at DESC LIMIT 1",
                                [p],
                                |r| r.get(0),
                            )
                            .optional()?;
                        let how = m.as_ref().map(|_| "number");
                        (m, how)
                    }
                    None => (None, None),
                },
            };
            let customer: Option<Value> = match &cid {
                Some(id) => c
                    .query_row("SELECT customer_id, name, phone, area, address FROM customers WHERE customer_id=?1", [id], |r| {
                        Ok(json!({ "customer_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "phone": r.get::<_, Option<String>>(2)?,
                            "area": r.get::<_, Option<String>>(3)?, "address": r.get::<_, Option<String>>(4)? }))
                    })
                    .optional()?,
                None => None,
            };
            Ok((customer, how, phone, push))
        })?;
        let tickets = match customer.as_ref().and_then(|c| c["customer_id"].as_str()) {
            Some(cid) if s.has("pos.sell") || s.has("deliveries.view") || s.has("deliveries.manage") => {
                let mut open = self
                    .tickets_list(token, TicketFilter { tab: Some("board".into()), customer_id: Some(cid.into()), ..Default::default() })?;
                open.truncate(20);
                open
            }
            _ => vec![],
        };
        let last = tickets.first().cloned();
        Ok(json!({
            "chat": chat,
            "phone": phone,
            "push_name": push,
            "customer": customer,
            "match": how,
            "last_ticket": last,
            "tickets": tickets,
            "orders_digital": orders_on,
        }))
    }
}
