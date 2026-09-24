//! Exact money and quantity arithmetic.
//!
//! * Money is always an `i64` count of minor units (fils for BHD: 1 BHD = 1000 fils).
//! * Quantities are `i64` thousandths ("milli-units"): 1 piece = 1000, 0.750 kg = 750.
//! * Tax rates are basis points: 10% = 1000 bp.
//!
//! No binary floating point is used anywhere in this module. All intermediate
//! products use `i128`, and rounding is "half away from zero", applied once
//! per line so every stored figure is reproducible.

use crate::error::{AppError, AppResult};

pub const QTY_SCALE: i64 = 1000;
pub const BP_SCALE: i64 = 10_000;

/// Divide rounding half away from zero. `d` must be positive.
pub fn div_round(n: i128, d: i128) -> i128 {
    debug_assert!(d > 0);
    let q = n / d;
    let r = n % d;
    if r.abs() * 2 >= d {
        q + n.signum()
    } else {
        q
    }
}

fn to_i64(v: i128) -> AppResult<i64> {
    i64::try_from(v).map_err(|_| AppError::validation("Amount is out of range."))
}

/// `unit_price * qty` where qty is in milli-units. Rounded to minor units.
pub fn extend(unit_price_minor: i64, qty_milli: i64) -> AppResult<i64> {
    to_i64(div_round(unit_price_minor as i128 * qty_milli as i128, QTY_SCALE as i128))
}

/// Tax contained in a tax-inclusive amount: `amount * r / (1 + r)`.
pub fn tax_from_inclusive(amount_minor: i64, rate_bp: i64) -> AppResult<i64> {
    if rate_bp == 0 {
        return Ok(0);
    }
    to_i64(div_round(amount_minor as i128 * rate_bp as i128, (BP_SCALE + rate_bp) as i128))
}

/// Tax to add on top of a tax-exclusive amount: `amount * r`.
pub fn tax_on_exclusive(amount_minor: i64, rate_bp: i64) -> AppResult<i64> {
    to_i64(div_round(amount_minor as i128 * rate_bp as i128, BP_SCALE as i128))
}

/// `amount * bp / 10000`, rounded. Used for percentage discounts.
pub fn percent_of(amount_minor: i64, bp: i64) -> AppResult<i64> {
    to_i64(div_round(amount_minor as i128 * bp as i128, BP_SCALE as i128))
}

/// Split `total` over `weights` proportionally so the parts sum exactly to
/// `total` (largest-remainder method; ties go to the earliest index).
/// Weights must be non-negative. If all weights are zero, the total goes to
/// the first element.
pub fn allocate(total: i64, weights: &[i64]) -> Vec<i64> {
    if weights.is_empty() {
        return vec![];
    }
    let sum: i128 = weights.iter().map(|w| *w as i128).sum();
    if sum <= 0 {
        let mut out = vec![0; weights.len()];
        out[0] = total;
        return out;
    }
    let mut parts: Vec<i64> = Vec::with_capacity(weights.len());
    let mut rems: Vec<(i128, usize)> = Vec::with_capacity(weights.len());
    let mut assigned: i128 = 0;
    for (i, w) in weights.iter().enumerate() {
        let num = total as i128 * *w as i128;
        let q = num.div_euclid(sum);
        let r = num.rem_euclid(sum);
        parts.push(q as i64);
        assigned += q;
        rems.push((r, i));
    }
    let mut left = total as i128 - assigned;
    rems.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut k = 0;
    while left > 0 {
        parts[rems[k % rems.len()].1] += 1;
        left -= 1;
        k += 1;
    }
    parts
}

/// Parse a decimal string with at most `digits` fractional digits into an
/// integer scaled by 10^digits. Accepts an optional leading '-'. Rejects
/// exponents, thousands separators, and excess precision (never silently
/// rounds input).
pub fn parse_decimal(s: &str, digits: u32) -> AppResult<i64> {
    let s = s.trim();
    if s.is_empty() {
        return Err(AppError::validation("A number is required."));
    }
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (int_part, frac_part) = match body.split_once('.') {
        Some((a, b)) => (a, b),
        None => (body, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(AppError::validation(format!("'{s}' is not a valid number.")));
    }
    if !int_part.chars().all(|c| c.is_ascii_digit()) || !frac_part.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::validation(format!("'{s}' is not a valid number.")));
    }
    if frac_part.len() as u32 > digits {
        return Err(AppError::validation(format!("'{s}' has more than {digits} decimal places.")));
    }
    let scale = 10i128.pow(digits);
    let ip: i128 = if int_part.is_empty() {
        0
    } else {
        int_part.parse::<i128>().map_err(|_| AppError::validation(format!("'{s}' is out of range.")))?
    };
    let mut frac = frac_part.to_string();
    while (frac.len() as u32) < digits {
        frac.push('0');
    }
    let fp: i128 = if frac.is_empty() { 0 } else { frac.parse().unwrap_or(0) };
    let v = ip.checked_mul(scale).and_then(|x| x.checked_add(fp)).ok_or_else(|| AppError::validation(format!("'{s}' is out of range.")))?;
    let v = if neg { -v } else { v };
    to_i64(v)
}

/// Format a scaled integer as a fixed-point decimal string.
pub fn format_decimal(v: i64, digits: u32) -> String {
    if digits == 0 {
        return v.to_string();
    }
    let scale = 10i128.pow(digits);
    let a = (v as i128).abs();
    let s = format!("{}.{:0width$}", a / scale, a % scale, width = digits as usize);
    if v < 0 {
        format!("-{s}")
    } else {
        s
    }
}

/// "BHD 1.250" / "−BHD 4.500" (uses U+2212 minus, per the visual spec).
pub fn format_money(v: i64, currency: &str, digits: u32) -> String {
    let body = format_decimal(v.abs(), digits);
    if v < 0 {
        format!("\u{2212}{currency} {body}")
    } else {
        format!("{currency} {body}")
    }
}

pub fn format_qty(q: i64) -> String {
    if q % QTY_SCALE == 0 {
        (q / QTY_SCALE).to_string()
    } else {
        format_decimal(q, 3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_half_away_from_zero() {
        assert_eq!(div_round(5, 10), 1);
        assert_eq!(div_round(4, 10), 0);
        assert_eq!(div_round(-5, 10), -1);
        assert_eq!(div_round(-4, 10), 0);
        assert_eq!(div_round(15, 10), 2);
        assert_eq!(div_round(-15, 10), -2);
    }

    #[test]
    fn extend_prices() {
        // 0.250 BHD x 3 = 0.750
        assert_eq!(extend(250, 3000).unwrap(), 750);
        // 1.999 BHD/kg x 0.750 kg = 1.49925 -> 1.499
        assert_eq!(extend(1999, 750).unwrap(), 1499);
        // 0.105 x 0.5 = 0.0525 -> 0.053 (half away)
        assert_eq!(extend(105, 500).unwrap(), 53);
    }

    #[test]
    fn vat_inclusive_and_exclusive() {
        // 1.100 incl 10% -> tax 0.100
        assert_eq!(tax_from_inclusive(1100, 1000).unwrap(), 100);
        // 0.250 incl 10% -> 22.727 fils -> 23
        assert_eq!(tax_from_inclusive(250, 1000).unwrap(), 23);
        // 1.000 excl 10% -> 0.100
        assert_eq!(tax_on_exclusive(1000, 1000).unwrap(), 100);
        assert_eq!(tax_on_exclusive(255, 1000).unwrap(), 26);
        assert_eq!(tax_from_inclusive(1000, 0).unwrap(), 0);
        // negative (refund) amounts are symmetric
        assert_eq!(tax_from_inclusive(-250, 1000).unwrap(), -23);
    }

    #[test]
    fn allocation_sums_exactly() {
        let parts = allocate(100, &[1, 1, 1]);
        assert_eq!(parts.iter().sum::<i64>(), 100);
        assert_eq!(parts, vec![34, 33, 33]);
        let parts = allocate(1000, &[250, 750, 333]);
        assert_eq!(parts.iter().sum::<i64>(), 1000);
        assert_eq!(allocate(7, &[0, 0]), vec![7, 0]);
        assert_eq!(allocate(0, &[5, 5]), vec![0, 0]);
        for total in 0..500 {
            let w = [17, 3, 999, 1, 40];
            assert_eq!(allocate(total, &w).iter().sum::<i64>(), total);
        }
    }

    #[test]
    fn parse_and_format() {
        assert_eq!(parse_decimal("1.250", 3).unwrap(), 1250);
        assert_eq!(parse_decimal("1.25", 3).unwrap(), 1250);
        assert_eq!(parse_decimal("0", 3).unwrap(), 0);
        assert_eq!(parse_decimal(".5", 3).unwrap(), 500);
        assert_eq!(parse_decimal("-4.5", 3).unwrap(), -4500);
        assert!(parse_decimal("1.2345", 3).is_err());
        assert!(parse_decimal("1e3", 3).is_err());
        assert!(parse_decimal("1,000", 3).is_err());
        assert!(parse_decimal("", 3).is_err());
        assert!(parse_decimal("abc", 3).is_err());
        assert!(parse_decimal(".", 3).is_err());
        assert_eq!(format_decimal(1250, 3), "1.250");
        assert_eq!(format_decimal(-5, 3), "-0.005");
        assert_eq!(format_money(18450, "BHD", 3), "BHD 18.450");
        assert_eq!(format_money(-4500, "BHD", 3), "\u{2212}BHD 4.500");
        assert_eq!(format_qty(2000), "2");
        assert_eq!(format_qty(750), "0.750");
    }
}
