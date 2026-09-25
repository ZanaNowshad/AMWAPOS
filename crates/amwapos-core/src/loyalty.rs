//! Loyalty points (flag `loyalty.enabled`, default off).
//!
//! * The ledger is append-only integers; a balance is the sum.
//! * Points are earned only inside the sale's commit, on what was paid after
//!   discounts (optionally skipping lines that already carry a discount), and
//!   reversed in proportion when the sale is refunded.
//! * Redemption is a discount, never cash from the drawer: its value is added
//!   to the cart discount and allocated to the lines by the normal pricing
//!   engine, so VAT is computed on the discounted lines exactly as for any
//!   other discount. The loyalty share of each line is kept as a snapshot.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult};
use crate::ids::new_id;
use crate::money::allocate;
use crate::pos::{line_inputs, LineRecord};
use crate::pricing::{self, LineResult, Totals};
use crate::service::AppCore;
use crate::settings::{self, LoyaltySettings};
use crate::time;
use crate::validate;

pub fn enabled(c: &Connection) -> AppResult<bool> {
    Ok(settings::get::<settings::FeatureFlags>(c, settings::KEY_FEATURES)?.is_on("loyalty.enabled"))
}

pub fn balance(c: &Connection, customer_id: &str) -> AppResult<i64> {
    Ok(c.query_row("SELECT COALESCE(SUM(points),0) FROM loyalty_ledger WHERE customer_id=?1", [customer_id], |r| r.get(0))?)
}

/// A priced cart including any loyalty redemption.
pub struct Priced {
    pub lines: Vec<LineResult>,
    pub totals: Totals,
    /// Loyalty share of each line's discount (snapshot).
    pub loyalty_alloc: Vec<i64>,
    pub loyalty_minor: i64,
    /// Points actually redeemed (capped to what the sale can absorb).
    pub points: i64,
}

/// Price a cart; `points` are redeemed only when the module is on and the
/// cart has a customer. Without a redemption this is exactly `price_cart`.
pub(crate) fn price(
    c: &Connection,
    lines: &[LineRecord],
    cd_minor: i64,
    cd_bp: i64,
    customer: Option<&str>,
    points: i64,
) -> AppResult<Priced> {
    let inputs = line_inputs(lines);
    let (first, first_totals) = pricing::price_cart(&inputs, cd_minor, cd_bp)?;
    let plain =
        |l: Vec<LineResult>, t: Totals| Priced { loyalty_alloc: vec![0; l.len()], lines: l, totals: t, loyalty_minor: 0, points: 0 };
    if points <= 0 || customer.is_none() || !enabled(c)? {
        return Ok(plain(first, first_totals));
    }
    let cfg: LoyaltySettings = settings::get(c, settings::KEY_LOYALTY)?;
    let nets: Vec<i64> = first.iter().map(|l| l.net_minor).collect();
    let room: i64 = nets.iter().sum();
    let points = points.min(room / cfg.redeem_minor_per_point.max(1));
    if points <= 0 {
        return Ok(plain(first, first_totals));
    }
    let value = points * cfg.redeem_minor_per_point;
    let cart_disc: i64 = first.iter().map(|l| l.cart_discount_minor).sum();
    let (lines_out, totals) = pricing::price_cart(&inputs, cart_disc + value, 0)?;
    Ok(Priced { loyalty_alloc: allocate(value, &nets), lines: lines_out, totals, loyalty_minor: value, points })
}

/// Points earned by a priced sale.
pub fn earned(cfg: &LoyaltySettings, p: &Priced) -> i64 {
    let eligible: i64 = p
        .lines
        .iter()
        .zip(&p.loyalty_alloc)
        .filter(|(l, loy)| !cfg.exclude_discounted_lines || (l.line_discount_minor == 0 && l.cart_discount_minor - **loy == 0))
        .map(|(l, _)| l.net_minor)
        .sum();
    (eligible / cfg.earn_minor_per_point.max(1)).max(0)
}

#[allow(clippy::too_many_arguments)]
fn post(
    tx: &Connection,
    s: &Session,
    customer: &str,
    kind: &str,
    points: i64,
    sale: Option<&str>,
    refund: Option<&str>,
    note: Option<&str>,
) -> AppResult<()> {
    tx.execute(
        "INSERT INTO loyalty_ledger(entry_id, customer_id, kind, points, sale_id, refund_id, note, user_id, device_id, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![new_id(), customer, kind, points, sale, refund, note, s.user_id, s.device_id, time::now_str()],
    )?;
    Ok(())
}

/// Inside the sale commit: record the redemption and the points earned.
pub fn record_sale(
    tx: &Connection,
    s: &Session,
    actor: &audit::Actor,
    customer: Option<&str>,
    sale_id: &str,
    p: &Priced,
) -> AppResult<i64> {
    let Some(cid) = customer else { return Ok(0) };
    if !enabled(tx)? {
        return Ok(0);
    }
    let cfg: LoyaltySettings = settings::get(tx, settings::KEY_LOYALTY)?;
    if p.points > 0 {
        let have = balance(tx, cid)?;
        if have < p.points {
            return Err(AppError::conflict("The customer no longer has enough points. Remove the redemption and try again."));
        }
        post(tx, s, cid, "redeem", -p.points, Some(sale_id), None, None)?;
    }
    let earn = earned(&cfg, p);
    if earn > 0 {
        post(tx, s, cid, "earn", earn, Some(sale_id), None, None)?;
    }
    if p.points > 0 || earn > 0 {
        audit::record(
            tx,
            actor,
            "loyalty.sale",
            "customer",
            Some(cid),
            None,
            Some(&json!({ "sale_id": sale_id, "earned": earn, "redeemed": p.points })),
        )?;
    }
    Ok(earn)
}

/// Inside the refund commit: take back earned points and give back redeemed
/// points in proportion to the refunded amount (never more than remains).
pub fn record_refund(
    tx: &Connection,
    s: &Session,
    actor: &audit::Actor,
    sale_id: &str,
    refund_id: &str,
    refund_total: i64,
) -> AppResult<()> {
    let (customer, sale_total): (Option<String>, i64) =
        tx.query_row("SELECT customer_id, total_minor FROM sales WHERE sale_id=?1", [sale_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let Some(cid) = customer else { return Ok(()) };
    if sale_total <= 0 {
        return Ok(());
    }
    let sum = |kind: &str| -> AppResult<i64> {
        Ok(tx.query_row(
            "SELECT COALESCE(SUM(points),0) FROM loyalty_ledger WHERE sale_id=?1 AND kind=?2",
            params![sale_id, kind],
            |r| r.get(0),
        )?)
    };
    let (earned, redeemed) = (sum("earn")?, -sum("redeem")?);
    let (rev_e, rev_r) = (-sum("reverse_earn")?, sum("reverse_redeem")?);
    let share = |v: i64| crate::money::div_round(v as i128 * refund_total as i128, sale_total as i128) as i64;
    let take = share(earned).min(earned - rev_e).max(0);
    let give = share(redeemed).min(redeemed - rev_r).max(0);
    if take > 0 {
        post(tx, s, &cid, "reverse_earn", -take, Some(sale_id), Some(refund_id), None)?;
    }
    if give > 0 {
        post(tx, s, &cid, "reverse_redeem", give, Some(sale_id), Some(refund_id), None)?;
    }
    if take > 0 || give > 0 {
        audit::record(
            tx,
            actor,
            "loyalty.refund",
            "customer",
            Some(&cid),
            None,
            Some(&json!({ "refund_id": refund_id, "points_taken": take, "points_returned": give })),
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct LoyaltyEntry {
    pub entry_id: String,
    pub kind: String,
    pub points: i64,
    pub sale_id: Option<String>,
    pub receipt_number: Option<String>,
    pub note: Option<String>,
    pub user_name: Option<String>,
    pub created_at: String,
}

impl AppCore {
    /// Balance and recent history of a customer.
    pub fn loyalty_customer(&self, token: &str, customer_id: &str) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        s.require("customers.view")?;
        self.require_feature("loyalty.enabled")?;
        let cid = validate::id(customer_id, "Customer")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT l.entry_id, l.kind, l.points, l.sale_id, sa.receipt_number, l.note, u.display_name, l.created_at
                 FROM loyalty_ledger l LEFT JOIN sales sa ON sa.sale_id=l.sale_id LEFT JOIN users u ON u.user_id=l.user_id
                 WHERE l.customer_id=?1 ORDER BY l.created_at DESC LIMIT 200",
            )?;
            let rows = st
                .query_map([&cid], |r| {
                    Ok(LoyaltyEntry {
                        entry_id: r.get(0)?,
                        kind: r.get(1)?,
                        points: r.get(2)?,
                        sale_id: r.get(3)?,
                        receipt_number: r.get(4)?,
                        note: r.get(5)?,
                        user_name: r.get(6)?,
                        created_at: r.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let cfg: LoyaltySettings = settings::get(c, settings::KEY_LOYALTY)?;
            let bal = balance(c, &cid)?;
            Ok(json!({ "balance": bal, "value_minor": bal * cfg.redeem_minor_per_point, "entries": rows, "settings": cfg }))
        })
    }

    /// Manual correction (owner / `loyalty.adjust`), with a note, audited.
    pub fn loyalty_adjust(&self, token: &str, customer_id: &str, points: i64, note: &str) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        s.require("loyalty.adjust")?;
        self.require_feature("loyalty.enabled")?;
        let cid = validate::id(customer_id, "Customer")?;
        let note = note.trim();
        if note.is_empty() || note.chars().count() > 300 {
            return Err(AppError::validation("Add a note (up to 300 characters) explaining the adjustment."));
        }
        if points == 0 || points.abs() > 10_000_000 {
            return Err(AppError::validation("Enter a non-zero number of points."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let exists: bool =
                tx.query_row("SELECT 1 FROM customers WHERE customer_id=?1", [&cid], |_| Ok(true)).optional()?.unwrap_or(false);
            if !exists {
                return Err(AppError::not_found("Customer"));
            }
            if balance(tx, &cid)? + points < 0 {
                return Err(AppError::validation("A balance cannot go below zero."));
            }
            post(tx, &s, &cid, "adjust", points, None, None, Some(note))?;
            audit::record(tx, &actor, "loyalty.adjusted", "customer", Some(&cid), None, Some(&json!({ "points": points, "note": note })))?;
            Ok(())
        })?;
        self.loyalty_customer(token, &cid)
    }
}
