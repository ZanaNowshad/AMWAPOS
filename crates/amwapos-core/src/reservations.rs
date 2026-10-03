//! Stock reservations for digital orders (any channel).
//!
//! Lifecycle: an order is *confirmed for fulfilment* by a person → its lines
//! are reserved from free stock (on hand minus other orders' active
//! reservations) → the reservation ends as `released` (the order was
//! cancelled), `expired` (not sold in time) or `converted` (the order became
//! a sale, which moves the stock itself). Reservations never change
//! `stock_levels` and never make free stock negative: a line asks for more
//! than is free only as an explicitly acknowledged shortage, and then only
//! the free part is reserved.
//!
//! Untracked products (no inventory) are never reserved. The till does not
//! read reservations: walk-in sales keep working exactly as before; the
//! reservation protects other *orders* from promising the same units.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::ids::new_id;
use crate::time;

/// Hours a confirmed order holds its stock without being sold.
pub const DEFAULT_TTL_HOURS: i64 = 48;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Shortage {
    pub line_no: i64,
    pub product_id: String,
    pub name: String,
    pub wanted_milli: i64,
    /// Free stock for this order (on hand minus other orders' reservations).
    pub free_milli: i64,
}

/// Units of a product held by active reservations, other than `exclude_order`'s.
pub fn reserved_milli(c: &Connection, product_id: &str, branch_id: &str, exclude_order: Option<&str>) -> AppResult<i64> {
    Ok(c.query_row(
        "SELECT COALESCE(SUM(qty_milli),0) FROM stock_reservations WHERE product_id=?1 AND branch_id=?2 AND status='active'
           AND (?3 IS NULL OR order_id<>?3)",
        params![product_id, branch_id, exclude_order],
        |r| r.get(0),
    )?)
}

/// Free stock of a tracked product (None: not tracked or unknown product).
pub fn free_milli(c: &Connection, product_id: &str, branch_id: &str, exclude_order: Option<&str>) -> AppResult<Option<i64>> {
    let row: Option<(i64, Option<i64>)> = c
        .query_row(
            "SELECT p.track_inventory, (SELECT qty_milli FROM stock_levels s WHERE s.product_id=p.product_id AND s.branch_id=?2)
             FROM products p WHERE p.product_id=?1",
            params![product_id, branch_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        Some((1, q)) => Some(q.unwrap_or(0) - reserved_milli(c, product_id, branch_id, exclude_order)?),
        _ => None,
    })
}

/// Lines of an order that ask for more than is free now (current stock,
/// current reservations, inactive or deleted products).
pub fn shortages(c: &Connection, order_id: &str) -> AppResult<Vec<Shortage>> {
    let branch: String = c.query_row("SELECT branch_id FROM digital_orders WHERE order_id=?1", [order_id], |r| r.get(0))?;
    let mut st = c.prepare(
        "SELECT l.line_no, l.product_id, COALESCE(p.name, l.description), l.qty_milli, COALESCE(p.active, 0)
         FROM digital_order_lines l LEFT JOIN products p ON p.product_id=l.product_id WHERE l.order_id=?1 ORDER BY l.line_no",
    )?;
    let rows: Vec<(i64, Option<String>, String, i64, i64)> =
        st.query_map([order_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<_, _>>()?;
    let mut out = vec![];
    // Two lines of the same product draw on the same free stock.
    let mut used: std::collections::HashMap<String, i64> = Default::default();
    for (line_no, pid, name, qty, active) in rows {
        let Some(pid) = pid else { continue };
        if active != 1 {
            out.push(Shortage { line_no, product_id: pid, name, wanted_milli: qty, free_milli: 0 });
            continue;
        }
        if let Some(free) = free_milli(c, &pid, &branch, Some(order_id))? {
            let already = *used.get(&pid).unwrap_or(&0);
            let left = free - already;
            if qty > left {
                out.push(Shortage { line_no, product_id: pid.clone(), name, wanted_milli: qty, free_milli: left.max(0) });
            }
            *used.entry(pid).or_default() += qty;
        }
    }
    Ok(out)
}

/// Reserve every tracked line of an order from free stock. Called in the
/// confirming transaction. With shortages, only the free part is reserved
/// (the caller has made the person acknowledge them).
pub fn reserve_order(tx: &Connection, order_id: &str, actor: &audit::Actor, ttl_hours: i64) -> AppResult<Vec<(i64, i64, i64)>> {
    let branch: String = tx.query_row("SELECT branch_id FROM digital_orders WHERE order_id=?1", [order_id], |r| r.get(0))?;
    let lines: Vec<(i64, String, i64)> = {
        let mut st = tx.prepare(
            "SELECT l.line_no, l.product_id, l.qty_milli FROM digital_order_lines l JOIN products p ON p.product_id=l.product_id
             WHERE l.order_id=?1 AND p.track_inventory=1 AND p.active=1 ORDER BY l.line_no",
        )?;
        let r = st.query_map([order_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
        r
    };
    let now = time::now_str();
    let expires = time::fmt(time::now() + chrono::Duration::hours(ttl_hours.max(1)));
    let mut done = vec![];
    for (line_no, pid, qty) in lines {
        let free = free_milli(tx, &pid, &branch, Some(order_id))?.unwrap_or(0);
        let take = qty.min(free.max(0));
        if take <= 0 {
            done.push((line_no, qty, 0));
            continue;
        }
        tx.execute(
            "INSERT INTO stock_reservations(reservation_id, order_id, line_no, product_id, branch_id, qty_milli, wanted_milli, status, expires_at, created_by, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,'active',?8,?9,?10)",
            params![new_id(), order_id, line_no, pid, branch, take, qty, expires, actor.user_id, now],
        )?;
        done.push((line_no, qty, take));
    }
    audit::record(
        tx,
        actor,
        "order.reserved",
        "digital_order",
        Some(order_id),
        None,
        Some(
            &json!({ "lines": done.iter().map(|(l, w, r)| json!({ "line_no": l, "wanted_milli": w, "reserved_milli": r })).collect::<Vec<_>>(), "expires_at": expires }),
        ),
    )?;
    Ok(done)
}

/// End an order's active reservations (`released`, `expired` or `converted`).
pub fn close_order(tx: &Connection, order_id: &str, status: &str, actor: &audit::Actor, reason: &str) -> AppResult<usize> {
    if !matches!(status, "released" | "expired" | "converted") {
        return Err(AppError::internal("bad reservation status"));
    }
    let n = tx.execute(
        "UPDATE stock_reservations SET status=?2, closed_at=?3, closed_by=?4, close_reason=?5 WHERE order_id=?1 AND status='active'",
        params![order_id, status, time::now_str(), actor.user_id, reason],
    )?;
    if n > 0 {
        audit::record(
            tx,
            actor,
            &format!("order.reservation_{status}"),
            "digital_order",
            Some(order_id),
            None,
            Some(&json!({ "lines": n, "reason": reason })),
        )?;
    }
    Ok(n)
}

/// Expire reservations past their time (a background pass). Returns the orders touched.
pub fn expire_due(tx: &Connection) -> AppResult<Vec<String>> {
    let now = time::now_str();
    let orders: Vec<String> = {
        let mut st = tx.prepare(
            "SELECT DISTINCT order_id FROM stock_reservations WHERE status='active' AND expires_at IS NOT NULL AND expires_at < ?1",
        )?;
        let r = st.query_map([&now], |r| r.get(0))?.collect::<Result<_, _>>()?;
        r
    };
    let system = audit::Actor { user_id: None, device_id: None, branch_id: None, approved_by: None };
    for o in &orders {
        close_order(tx, o, "expired", &system, "not sold before the hold ended")?;
    }
    Ok(orders)
}

/// The order's reservations for display.
pub fn for_order(c: &Connection, order_id: &str) -> AppResult<Vec<serde_json::Value>> {
    let mut st = c.prepare(
        "SELECT line_no, product_id, qty_milli, wanted_milli, status, expires_at, closed_at, close_reason FROM stock_reservations
         WHERE order_id=?1 ORDER BY created_at, line_no",
    )?;
    let rows = st
        .query_map([order_id], |r| {
            Ok(json!({ "line_no": r.get::<_, i64>(0)?, "product_id": r.get::<_, String>(1)?, "qty_milli": r.get::<_, i64>(2)?,
                       "wanted_milli": r.get::<_, i64>(3)?, "status": r.get::<_, String>(4)?, "expires_at": r.get::<_, Option<String>>(5)?,
                       "closed_at": r.get::<_, Option<String>>(6)?, "close_reason": r.get::<_, Option<String>>(7)? }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
