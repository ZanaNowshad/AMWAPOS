//! Deterministic helpers the assistant (and the page) can use. None of them
//! writes anything: suggestions become proposals a person confirms, and the
//! anomaly checks only add items to the action inbox.
//!
//! - B3 anomaly checks (refund spike, discount spike, negative stock, hub lag,
//!   overdue backup) with thresholds from Settings → AI.
//! - B4 reorder suggestions: reorder point, on hand, in transit, last supplier.
//! - B5 price from cost and a target margin (VAT-aware, rounded up).
//! - B8 branch comparison for one product (only with `org.multi_branch`).

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::service::AppCore;
use crate::{time, validate};

/// Price for a target margin: net = cost / (1 − margin); VAT added when
/// prices include it; rounded up to `round` minor units.
pub fn price_for_margin(cost_minor: i64, margin_bp: i64, tax_rate_bp: i64, tax_inclusive: bool, round: i64) -> Option<i64> {
    if cost_minor <= 0 || !(0..9_500).contains(&margin_bp) {
        return None;
    }
    let up = |a: i128, b: i128| (a + b - 1) / b;
    let net = up(cost_minor as i128 * 10_000, (10_000 - margin_bp) as i128);
    let gross = if tax_inclusive { up(net * (10_000 + tax_rate_bp as i128), 10_000) } else { net };
    let r = round.max(1) as i128;
    Some(((gross + r - 1) / r * r) as i64)
}

/// Minor units as a decimal string ("1.500" for 1500 with 3 digits).
pub fn minor_to_decimal(minor: i64, digits: u32) -> String {
    if digits == 0 {
        return minor.to_string();
    }
    let p = 10i64.pow(digits);
    let sign = if minor < 0 { "-" } else { "" };
    format!("{sign}{}.{:0w$}", minor.abs() / p, minor.abs() % p, w = digits as usize)
}

impl AppCore {
    // ---- B4 -------------------------------------------------------------

    /// Products at or below their reorder point in the user's branch, with
    /// what is already on order, the last supplier and cost, and a suggested
    /// quantity (up to twice the reorder point). Grouped by supplier.
    pub fn reorder_suggestions(&self, token: &str, supplier_id: Option<&str>) -> AppResult<Value> {
        // One engine: the replenishment engine's suggestions (replenish.rs),
        // grouped by the supplier it chose. No arithmetic of its own.
        let v = self.replenishment(
            token,
            crate::replenish::ReplenishFilter {
                supplier_id: supplier_id.map(str::to_string),
                states: Some(vec!["order".into()]),
                ..Default::default()
            },
        )?;
        let mut by_supplier: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
        for r in v["rows"].as_array().cloned().unwrap_or_default() {
            let sid = r["supplier"]["supplier_id"].as_str().unwrap_or("").to_string();
            by_supplier.entry(sid).or_default().push(json!({
                "product_id": r["product_id"], "name": r["name"], "sku": r["sku"],
                "position_milli": r["position_milli"], "reorder_point_milli": r["reorder_point_milli"], "order_up_to_milli": r["order_up_to_milli"],
                "suggested_qty_milli": r["suggested_milli"], "packs": r["packs"], "reasons": r["reasons"], "warnings": r["warnings"],
                "estimated_cost_minor": r["estimated_cost_minor"],
            }));
        }
        let groups: Vec<Value> = by_supplier
            .into_iter()
            .map(|(sid, lines)| json!({ "supplier_id": if sid.is_empty() { Value::Null } else { json!(sid) }, "lines": lines }))
            .collect();
        Ok(json!({
            "rule": "The app's replenishment engine (Suggested orders): order when stock position ≤ reorder point, up to the target, in whole packs and at least the minimum order.",
            "groups": groups,
        }))
    }

    /// The requisition a reorder proposal records: the products the engine
    /// suggests ordering from this supplier. The quantities are recomputed by
    /// the app when the person confirms; the model chooses nothing.
    pub(crate) fn reorder_requisition_args(&self, token: &str, supplier_id: &str) -> AppResult<Value> {
        let sid = validate::id(supplier_id, "Supplier")?;
        let v = self.reorder_suggestions(token, Some(&sid))?;
        let ids: Vec<Value> = v["groups"]
            .as_array()
            .and_then(|g| g.first())
            .and_then(|g| g["lines"].as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|l| l["product_id"].clone())
            .collect();
        if ids.is_empty() {
            return Err(AppError::validation("Nothing from this supplier needs ordering now."));
        }
        Ok(json!({ "supplier_id": sid, "product_ids": ids, "note": "Suggested by the AI assistant from Suggested orders." }))
    }

    // ---- B5 -------------------------------------------------------------

    /// Suggested selling price from the product's cost and the target margin.
    pub fn margin_price(&self, token: &str, product_id: &str, margin_bp: Option<i64>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("products.view")?;
        let st = self.ai_settings_pub()?;
        let pid = validate::id(product_id, "Product")?;
        let margin = margin_bp.unwrap_or(st.target_margin_bp);
        if !(0..9_500).contains(&margin) {
            return Err(AppError::validation("The target margin must be between 0% and 95%."));
        }
        let (name, cost, rate, inclusive, current): (String, Option<i64>, i64, bool, Option<i64>) = self.db.read(|c| {
            let (name, rate, incl): (String, i64, i64) = c
                .query_row(
                    "SELECT p.name, t.rate_bp, t.inclusive FROM products p JOIN tax_rules t ON t.tax_rule_id=p.tax_rule_id WHERE p.product_id=?1",
                    [&pid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Product"))?;
            let cost: Option<i64> = c
                .query_row(
                    "SELECT CASE WHEN avg_cost_minor>0 THEN avg_cost_minor ELSE last_cost_minor END FROM product_costs WHERE product_id=?1 AND branch_id=?2",
                    params![pid, s.branch_id],
                    |r| r.get(0),
                )
                .optional()?;
            Ok((name, cost, rate, incl != 0, crate::catalog::current_price(c, &pid)?))
        })?;
        let cost =
            cost.filter(|c| *c > 0).ok_or_else(|| AppError::validation("This product has no cost yet, so no price can be suggested."))?;
        let price = price_for_margin(cost, margin, rate, inclusive, st.price_round_minor)
            .ok_or_else(|| AppError::validation("No price can be suggested for this cost and margin."))?;
        Ok(json!({ "product_id": pid, "name": name, "cost_minor": cost, "target_margin_bp": margin, "tax_rate_bp": rate,
                   "tax_inclusive": inclusive, "current_price_minor": current, "suggested_price_minor": price,
                   "rule": "price = cost ÷ (1 − margin), plus VAT when prices include it, rounded up" }))
    }

    // ---- B8 -------------------------------------------------------------

    /// One product across branches: price, stock, cost and the last 30 days
    /// of sales and margin. Only with multiple branches switched on.
    pub fn branch_compare(&self, token: &str, product_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("products.view")?;
        if !self.features()?.is_on("org.multi_branch") {
            return Ok(json!({ "enabled": false, "branches": [] }));
        }
        let pid = validate::id(product_id, "Product")?;
        let prices = self.branch_prices_get(token, &pid).unwrap_or_default();
        let from = time::fmt(time::now() - chrono::Duration::days(30));
        let rows: Vec<Value> = self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT b.branch_id, b.name, COALESCE(sl.qty_milli,0),
                        (SELECT CASE WHEN pc.avg_cost_minor>0 THEN pc.avg_cost_minor ELSE pc.last_cost_minor END FROM product_costs pc
                          WHERE pc.product_id=?1 AND pc.branch_id=b.branch_id),
                        (SELECT COALESCE(SUM(i.qty_milli),0) FROM sale_items i JOIN sales x ON x.sale_id=i.sale_id
                          WHERE i.product_id=?1 AND x.branch_id=b.branch_id AND x.created_at>=?2),
                        (SELECT COALESCE(SUM(i.line_total_minor - i.tax_minor),0) FROM sale_items i JOIN sales x ON x.sale_id=i.sale_id
                          WHERE i.product_id=?1 AND x.branch_id=b.branch_id AND x.created_at>=?2),
                        (SELECT COALESCE(SUM(i.cost_snapshot_minor * i.qty_milli / 1000),0) FROM sale_items i JOIN sales x ON x.sale_id=i.sale_id
                          WHERE i.product_id=?1 AND x.branch_id=b.branch_id AND x.created_at>=?2)
                 FROM branches b LEFT JOIN stock_levels sl ON sl.branch_id=b.branch_id AND sl.product_id=?1 ORDER BY b.name",
            )?;
            let rows = st
                .query_map(params![pid, from], |r| {
                    let net: i64 = r.get(5)?;
                    let cost: i64 = r.get(6)?;
                    let margin_bp = if net > 0 { Some((net - cost) * 10_000 / net) } else { None };
                    Ok(json!({ "branch_id": r.get::<_, String>(0)?, "branch": r.get::<_, String>(1)?, "stock_milli": r.get::<_, i64>(2)?,
                               "cost_minor": r.get::<_, Option<i64>>(3)?, "sold_30d_milli": r.get::<_, i64>(4)?,
                               "net_sales_30d_minor": net, "cost_30d_minor": cost, "margin_30d_bp": margin_bp }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        let _ = s;
        Ok(json!({ "enabled": true, "product_id": pid, "prices": prices, "branches": rows }))
    }

    // ---- B3 -------------------------------------------------------------

    /// Former B3 inbox writer. Since Wave 7 operational alerts are cases made
    /// by the Alert Centre checks (`ops_evaluate`); the old inbox table is
    /// read-only and nothing writes to it. Refund and discount spikes and
    /// negative stock are business figures shown on the Dashboard, not cases.
    pub fn ai_anomaly_scan(&self) -> AppResult<usize> {
        Ok(0)
    }

    pub fn ai_alerts(&self, token: &str, include_dismissed: bool) -> AppResult<Vec<Value>> {
        let s = self.session(token)?;
        s.require("admin.access")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT alert_id, kind, day_key, severity, title, detail_json, created_at, dismissed_at FROM ai_alerts
                 WHERE (?1 OR dismissed_at IS NULL) ORDER BY created_at DESC LIMIT 100",
            )?;
            let rows = st
                .query_map([include_dismissed], |r| {
                    Ok(json!({ "alert_id": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?, "day": r.get::<_, String>(2)?,
                               "severity": r.get::<_, String>(3)?, "title": r.get::<_, String>(4)?,
                               "detail": serde_json::from_str::<Value>(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
                               "created_at": r.get::<_, String>(6)?, "dismissed_at": r.get::<_, Option<String>>(7)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// The old inbox is read-only: its items are legacy cases now, handled in
    /// the Alert Centre.
    pub fn ai_alert_dismiss(&self, token: &str, alert_id: &str) -> AppResult<Vec<Value>> {
        let s = self.session(token)?;
        s.require("admin.access")?;
        validate::id(alert_id, "Alert")?;
        Err(AppError::conflict("This alert is now a case in the Alert Centre. Handle it there."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn margin_price_is_vat_aware_and_rounded_up() {
        // cost 1.000, 25% margin → net 1.334 (rounded up), +10% VAT → 1.468 → 1.470.
        assert_eq!(price_for_margin(1000, 2500, 1000, true, 5), Some(1470));
        assert_eq!(price_for_margin(1000, 2500, 1000, false, 5), Some(1335));
        assert_eq!(price_for_margin(0, 2500, 1000, true, 5), None);
        assert_eq!(price_for_margin(1000, 9600, 0, false, 1), None);
        assert_eq!(minor_to_decimal(1470, 3), "1.470");
        assert_eq!(minor_to_decimal(5, 3), "0.005");
    }
}
