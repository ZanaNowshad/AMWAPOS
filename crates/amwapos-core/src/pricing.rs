//! Authoritative cart pricing, tender validation and refund proration.
//!
//! Pure functions over integer money; no I/O. The frontend may display
//! estimates, but every persisted figure comes from here.
//!
//! Line algorithm:
//! 1. `gross = round(unit_price × qty)`
//! 2. `line_discount` = fixed amount or `round(gross × bp)`; must be ≤ gross
//! 3. cart discount (fixed or bp of the post-line-discount sum) is allocated
//!    across lines proportionally with the largest-remainder method, so line
//!    discounts always sum exactly to the header discount
//! 4. `net = gross − line_discount − allocated_cart_discount`
//! 5. inclusive tax: `tax = round(net × r/(1+r))`, `total = net`;
//!    exclusive tax: `tax = round(net × r)`, `total = net + tax`
//!
//! Header: `subtotal = Σgross`, `discount = Σdiscounts`, `tax = Σtax`,
//! `total = Σline_total`.

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::money::{allocate, div_round, extend, percent_of, tax_from_inclusive, tax_on_exclusive};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LineInput {
    pub unit_price_minor: i64,
    pub qty_milli: i64,
    pub line_discount_minor: i64,
    pub line_discount_bp: i64,
    pub tax_rate_bp: i64,
    pub tax_inclusive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LineResult {
    pub gross_minor: i64,
    pub line_discount_minor: i64,
    pub cart_discount_minor: i64,
    pub discount_minor: i64,
    pub net_minor: i64,
    pub tax_minor: i64,
    pub line_total_minor: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Totals {
    pub subtotal_minor: i64,
    pub discount_minor: i64,
    pub tax_minor: i64,
    pub total_minor: i64,
    pub item_count_milli: i64,
}

pub fn price_cart(
    lines: &[LineInput],
    cart_discount_minor: i64,
    cart_discount_bp: i64,
) -> AppResult<(Vec<LineResult>, Totals)> {
    if cart_discount_minor < 0 || !(0..=10000).contains(&cart_discount_bp) {
        return Err(AppError::validation("Invalid cart discount."));
    }
    if cart_discount_minor > 0 && cart_discount_bp > 0 {
        return Err(AppError::validation("Use either a fixed or a percentage cart discount, not both."));
    }
    let mut results = Vec::with_capacity(lines.len());
    let mut after_line = Vec::with_capacity(lines.len());
    for l in lines {
        if l.qty_milli <= 0 {
            return Err(AppError::validation("Quantity must be greater than zero."));
        }
        if l.unit_price_minor < 0 {
            return Err(AppError::validation("Price cannot be negative."));
        }
        if l.line_discount_minor < 0 || !(0..=10000).contains(&l.line_discount_bp) {
            return Err(AppError::validation("Invalid line discount."));
        }
        if !(0..=10000).contains(&l.tax_rate_bp) {
            return Err(AppError::validation("Invalid tax rate."));
        }
        let gross = extend(l.unit_price_minor, l.qty_milli)?;
        let ld = if l.line_discount_bp > 0 {
            percent_of(gross, l.line_discount_bp)?
        } else {
            l.line_discount_minor
        };
        if ld > gross {
            return Err(AppError::validation("A line discount cannot exceed the line amount."));
        }
        after_line.push(gross - ld);
        results.push(LineResult {
            gross_minor: gross,
            line_discount_minor: ld,
            ..Default::default()
        });
    }
    let base: i64 = after_line.iter().sum();
    let cart_disc = if cart_discount_bp > 0 {
        percent_of(base, cart_discount_bp)?
    } else {
        cart_discount_minor
    };
    if cart_disc > base {
        return Err(AppError::validation("The discount cannot exceed the sale amount."));
    }
    let alloc = allocate(cart_disc, &after_line);
    let mut totals = Totals::default();
    for (i, (r, l)) in results.iter_mut().zip(lines).enumerate() {
        r.cart_discount_minor = alloc.get(i).copied().unwrap_or(0);
        r.discount_minor = r.line_discount_minor + r.cart_discount_minor;
        r.net_minor = r.gross_minor - r.discount_minor;
        if l.tax_inclusive {
            r.tax_minor = tax_from_inclusive(r.net_minor, l.tax_rate_bp)?;
            r.line_total_minor = r.net_minor;
        } else {
            r.tax_minor = tax_on_exclusive(r.net_minor, l.tax_rate_bp)?;
            r.line_total_minor = r.net_minor + r.tax_minor;
        }
        totals.subtotal_minor += r.gross_minor;
        totals.discount_minor += r.discount_minor;
        totals.tax_minor += r.tax_minor;
        totals.total_minor += r.line_total_minor;
        totals.item_count_milli += l.qty_milli;
    }
    Ok((results, totals))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TenderInput {
    pub method: String,
    /// Amount handed over for this tender (for cash may exceed what is due).
    pub amount_minor: i64,
    #[serde(default)]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TenderApplied {
    pub method: String,
    pub tendered_minor: i64,
    /// Portion applied to the sale.
    pub amount_minor: i64,
    pub change_minor: i64,
    pub reference: Option<String>,
}

/// Validate tenders against the amount due. Only methods in `change_methods`
/// (cash) may be over-tendered; change is taken from those tenders.
pub fn apply_tenders(
    total_minor: i64,
    tenders: &[TenderInput],
    change_methods: &[&str],
) -> AppResult<(Vec<TenderApplied>, i64)> {
    if tenders.is_empty() {
        if total_minor == 0 {
            return Ok((vec![], 0));
        }
        return Err(AppError::validation("Add a payment before completing the sale."));
    }
    let mut sum = 0i64;
    let mut non_change = 0i64;
    let mut change_capable = 0i64;
    for t in tenders {
        if t.amount_minor <= 0 {
            return Err(AppError::validation("Each payment amount must be greater than zero."));
        }
        sum = sum
            .checked_add(t.amount_minor)
            .ok_or_else(|| AppError::validation("Payment total is out of range."))?;
        if change_methods.contains(&t.method.as_str()) {
            change_capable += t.amount_minor;
        } else {
            non_change += t.amount_minor;
        }
    }
    if non_change > total_minor {
        return Err(AppError::validation(
            "Card and other non-cash payments cannot exceed the amount due.",
        ));
    }
    if sum < total_minor {
        return Err(AppError::validation(format!(
            "Payments are short by {} (minor units).",
            total_minor - sum
        ))
        .with_details(serde_json::json!({ "remaining_minor": total_minor - sum })));
    }
    let mut change = sum - total_minor;
    if change > change_capable {
        return Err(AppError::validation("Change can only be given from cash."));
    }
    let total_change = change;
    // Take change from the last change-capable tenders first.
    let mut applied: Vec<TenderApplied> = tenders
        .iter()
        .map(|t| TenderApplied {
            method: t.method.clone(),
            tendered_minor: t.amount_minor,
            amount_minor: t.amount_minor,
            change_minor: 0,
            reference: t.reference.clone().filter(|r| !r.trim().is_empty()),
        })
        .collect();
    for a in applied.iter_mut().rev() {
        if change == 0 {
            break;
        }
        if change_methods.contains(&a.method.as_str()) {
            let c = change.min(a.amount_minor);
            a.change_minor = c;
            a.amount_minor -= c;
            change -= c;
        }
    }
    applied.retain(|a| a.amount_minor > 0);
    Ok((applied, total_change))
}

/// Prorate an original line amount for a partial refund. When the refund
/// takes the remaining quantity, the remainder of the original amount is
/// returned so that full refunds are always exact.
pub fn prorate(
    original_amount: i64,
    original_qty_milli: i64,
    already_refunded_amount: i64,
    already_refunded_qty_milli: i64,
    refund_qty_milli: i64,
) -> AppResult<i64> {
    if refund_qty_milli <= 0 {
        return Err(AppError::validation("Refund quantity must be greater than zero."));
    }
    let remaining_qty = original_qty_milli - already_refunded_qty_milli;
    if refund_qty_milli > remaining_qty {
        return Err(AppError::validation("Refund quantity exceeds the refundable quantity."));
    }
    if refund_qty_milli == remaining_qty {
        return Ok(original_amount - already_refunded_amount);
    }
    let v = div_round(
        original_amount as i128 * refund_qty_milli as i128,
        original_qty_milli as i128,
    );
    let v = i64::try_from(v).map_err(|_| AppError::validation("Amount out of range."))?;
    // Never refund more than what remains of the original amount.
    Ok(v.min(original_amount - already_refunded_amount))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(price: i64, qty: i64) -> LineInput {
        LineInput {
            unit_price_minor: price,
            qty_milli: qty,
            line_discount_minor: 0,
            line_discount_bp: 0,
            tax_rate_bp: 1000,
            tax_inclusive: true,
        }
    }

    #[test]
    fn simple_inclusive_cart() {
        let (lines, t) = price_cart(&[line(250, 3000), line(1100, 1000)], 0, 0).unwrap();
        assert_eq!(lines[0].gross_minor, 750);
        assert_eq!(lines[0].tax_minor, 68); // 750*1000/11000 = 68.18
        assert_eq!(lines[1].tax_minor, 100);
        assert_eq!(t.subtotal_minor, 1850);
        assert_eq!(t.total_minor, 1850);
        assert_eq!(t.tax_minor, 168);
        assert_eq!(t.discount_minor, 0);
        assert_eq!(t.item_count_milli, 4000);
    }

    #[test]
    fn exclusive_tax_adds() {
        let mut l = line(1000, 2000);
        l.tax_inclusive = false;
        let (_, t) = price_cart(&[l], 0, 0).unwrap();
        assert_eq!(t.subtotal_minor, 2000);
        assert_eq!(t.tax_minor, 200);
        assert_eq!(t.total_minor, 2200);
    }

    #[test]
    fn discounts_allocate_exactly() {
        let mut a = line(333, 1000);
        a.line_discount_bp = 1000; // 10% -> 33
        let b = line(500, 2000);
        let c = line(1, 1000);
        let (lines, t) = price_cart(&[a, b, c], 100, 0).unwrap();
        assert_eq!(lines[0].line_discount_minor, 33);
        let cart_alloc: i64 = lines.iter().map(|l| l.cart_discount_minor).sum();
        assert_eq!(cart_alloc, 100);
        assert_eq!(t.discount_minor, 133);
        assert_eq!(t.total_minor, t.subtotal_minor - t.discount_minor);
        for l in &lines {
            assert!(l.net_minor >= 0);
        }
    }

    #[test]
    fn percent_cart_discount() {
        let (_, t) = price_cart(&[line(1000, 1000), line(2000, 1000)], 0, 500).unwrap();
        assert_eq!(t.discount_minor, 150);
        assert_eq!(t.total_minor, 2850);
    }

    #[test]
    fn invalid_discounts_rejected() {
        let mut a = line(100, 1000);
        a.line_discount_minor = 101;
        assert!(price_cart(&[a], 0, 0).is_err());
        assert!(price_cart(&[line(100, 1000)], 101, 0).is_err());
        assert!(price_cart(&[line(100, 1000)], 10, 10).is_err());
        assert!(price_cart(&[line(100, 0)], 0, 0).is_err());
    }

    #[test]
    fn decimal_quantity() {
        let (l, t) = price_cart(&[line(1999, 750)], 0, 0).unwrap();
        assert_eq!(l[0].gross_minor, 1499);
        assert_eq!(t.total_minor, 1499);
    }

    #[test]
    fn invariants_hold_for_many_carts() {
        // Deterministic pseudo-random sweep.
        let mut seed: u64 = 42;
        let mut rnd = |m: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % m
        };
        for _ in 0..2000 {
            let n = 1 + rnd(8) as usize;
            let mut lines = vec![];
            for _ in 0..n {
                let mut l = line(rnd(20000) as i64, 1 + rnd(5000) as i64);
                l.tax_inclusive = rnd(2) == 0;
                l.tax_rate_bp = [0, 500, 1000][rnd(3) as usize];
                if rnd(3) == 0 {
                    l.line_discount_bp = rnd(3000) as i64;
                }
                lines.push(l);
            }
            let bp = if rnd(2) == 0 { rnd(2000) as i64 } else { 0 };
            let (res, t) = price_cart(&lines, 0, bp).unwrap();
            let excl_tax: i64 = res
                .iter()
                .zip(&lines)
                .filter(|(_, l)| !l.tax_inclusive)
                .map(|(r, _)| r.tax_minor)
                .sum();
            assert_eq!(t.total_minor, t.subtotal_minor - t.discount_minor + excl_tax);
            assert_eq!(t.total_minor, res.iter().map(|r| r.line_total_minor).sum::<i64>());
            assert!(res.iter().all(|r| r.net_minor >= 0 && r.tax_minor >= 0));
        }
    }

    #[test]
    fn tenders_cash_change() {
        let (a, change) = apply_tenders(
            18450,
            &[TenderInput { method: "cash".into(), amount_minor: 20000, reference: None }],
            &["cash"],
        )
        .unwrap();
        assert_eq!(change, 1550);
        assert_eq!(a[0].amount_minor, 18450);
        assert_eq!(a[0].change_minor, 1550);
    }

    #[test]
    fn tenders_split() {
        let (a, change) = apply_tenders(
            18450,
            &[
                TenderInput { method: "cash".into(), amount_minor: 10000, reference: None },
                TenderInput { method: "card".into(), amount_minor: 8450, reference: Some("1234".into()) },
            ],
            &["cash"],
        )
        .unwrap();
        assert_eq!(change, 0);
        assert_eq!(a.iter().map(|x| x.amount_minor).sum::<i64>(), 18450);
    }

    #[test]
    fn tenders_rejections() {
        let t = |m: &str, a: i64| TenderInput { method: m.into(), amount_minor: a, reference: None };
        assert!(apply_tenders(1000, &[t("card", 1500)], &["cash"]).is_err(), "card over-tender");
        assert!(apply_tenders(1000, &[t("cash", 500)], &["cash"]).is_err(), "short");
        assert!(apply_tenders(1000, &[t("cash", 0)], &["cash"]).is_err(), "zero");
        assert!(apply_tenders(1000, &[], &["cash"]).is_err());
        // cash 500 + card 1000 = 1500 on 1000 due: card alone covers the whole amount and cash would be pure change.
        let (a, c) = apply_tenders(1000, &[t("cash", 500), t("card", 1000)], &["cash"]).unwrap();
        assert_eq!(c, 500);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].method, "card");
        assert!(apply_tenders(0, &[], &["cash"]).is_ok());
    }

    #[test]
    fn proration_is_exact_on_final_refund() {
        // line total 1000 for qty 3
        let a = prorate(1000, 3000, 0, 0, 1000).unwrap();
        assert_eq!(a, 333);
        let b = prorate(1000, 3000, a, 1000, 1000).unwrap();
        assert_eq!(b, 333);
        let c = prorate(1000, 3000, a + b, 2000, 1000).unwrap();
        assert_eq!(c, 334);
        assert_eq!(a + b + c, 1000);
        assert!(prorate(1000, 3000, a + b + c, 3000, 1).is_err());
    }
}
