//! Reports. Every figure derives from committed records (sales, sale_items,
//! payments, refunds, cash_events, stock_movements) — never from current
//! catalogue prices. Date filters are local calendar dates in the store
//! timezone; refunds count on the day they were made.
//!
//! Every report returns the same shape (`Report`) so the UI has one viewer
//! and CSV export is uniform.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::Session;
use crate::error::{AppError, AppResult};
use crate::money::format_decimal;
use crate::service::AppCore;
use crate::time;

#[derive(Debug, Clone, Serialize)]
pub struct Column {
    pub key: String,
    pub label: String,
    /// text | money | qty | int | percent_bp | datetime | date
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Kpi {
    pub label: String,
    pub value: i64,
    pub kind: String,
    pub previous: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub key: String,
    pub title: String,
    pub from: String,
    pub to: String,
    pub kpis: Vec<Kpi>,
    pub columns: Vec<Column>,
    pub rows: Vec<Value>,
    pub totals: Option<Value>,
    pub series: Option<Vec<Value>>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ReportParams {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub group_by: Option<String>,
    #[serde(default)]
    pub category_id: Option<String>,
    #[serde(default)]
    pub cashier_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub days: Option<i64>,
    /// Multi-branch: one branch (default: the user's scope; owners see all).
    #[serde(default)]
    pub branch_id: Option<String>,
}

fn col(key: &str, label: &str, kind: &str) -> Column {
    Column { key: key.into(), label: label.into(), kind: kind.into() }
}
fn kpi(label: &str, value: i64, kind: &str, previous: Option<i64>) -> Kpi {
    Kpi { label: label.into(), value, kind: kind.into(), previous }
}

pub const REPORTS: &[(&str, &str, &str, &str)] = &[
    ("sales", "Sales", "Sales", "Sales by day, hour, cashier, terminal or payment method"),
    ("products", "Product sales", "Sales", "Quantity and revenue per product"),
    ("categories", "Category sales", "Sales", "Revenue per category"),
    ("payments", "Payment methods", "Financial", "Tenders recorded per method, net of refunds"),
    ("tax", "VAT", "Financial", "Taxable amounts and VAT by rate, net of refunds"),
    ("margin", "Margin & profit", "Financial", "Revenue, cost and gross profit per product"),
    ("operating_profit", "Operating profit", "Financial", "Sales, cost of goods and expenses: what the business really made"),
    ("expenses", "Expenses", "Financial", "Approved and paid expenses by category"),
    ("receivables", "Customer balances", "Financial", "What customers owe, by how late it is"),
    ("refunds", "Refunds", "Sales", "Refunds with reasons and approvals"),
    ("channels", "Sales by channel", "Sales", "Sales, basket, refunds and gross margin per sales channel (till, WhatsApp, phone, web)"),
    ("price_changes", "Price changes", "Financial", "Every applied price change with its list, reason, policy and who made it"),
    (
        "promotions",
        "Promotions",
        "Sales",
        "Per offer: sales it was used in, discount given, revenue and gross margin of the lines it touched",
    ),
    ("coupons", "Coupon redemptions", "Sales", "Per coupon code: completed sales that used it and the discount it gave"),
    ("bundles", "Bundles", "Sales", "Per bundle: units sold, revenue, cost of the items used, gross margin and the items taken from stock"),
    ("cash", "Cash & shifts", "Financial", "Shift reconciliation and cash variances"),
    ("inventory", "Stock valuation", "Inventory", "On-hand quantity and value at average cost"),
    ("dead_stock", "Dead stock", "Inventory", "Stocked items with no sales in the period"),
    ("stock_movements", "Stock movements", "Inventory", "Ledger totals by movement type"),
    ("purchasing", "Purchasing", "Purchasing", "Goods received per supplier"),
    ("supplier_performance", "Supplier performance", "Purchasing", "Ordered vs received quantity and on-time deliveries per supplier"),
    ("deliveries", "Deliveries", "Operations", "Deliveries by status"),
    ("audit", "Audit summary", "Audit", "Audited events by type and user"),
];

fn permission_for(key: &str) -> &'static str {
    match key {
        "margin" | "cash" | "payments" | "price_changes" | "promotions" | "bundles" => "reports.financial",
        "operating_profit" => "reports.profit",
        "expenses" => "expenses.view",
        "receivables" => "reports.financial",
        "tax" => "reports.tax",
        "audit" => "audit.view",
        "inventory" | "dead_stock" | "stock_movements" => "inventory.view",
        "purchasing" | "supplier_performance" => "purchasing.manage",
        "deliveries" => "deliveries.view",
        _ => "reports.sales",
    }
}

struct Range {
    from: String,
    to: String,
    a: String,
    b: String,
    tz: String,
    day: time::Day,
}

fn range(c: &Connection, core: &AppCore, p: &ReportParams) -> AppResult<Range> {
    let tz = core.store_timezone(c)?;
    let day = time::day(c)?;
    let today = time::business_date(time::now(), &day)?;
    let from = p.from.clone().filter(|x| !x.is_empty()).unwrap_or_else(|| today.clone());
    let to = p.to.clone().filter(|x| !x.is_empty()).unwrap_or_else(|| today.clone());
    let (a, b) = time::local_date_range_utc(&from, &to, &day)?;
    let days = (chrono::NaiveDate::parse_from_str(&to, "%Y-%m-%d").unwrap()
        - chrono::NaiveDate::parse_from_str(&from, "%Y-%m-%d").unwrap())
    .num_days();
    if days > 3660 {
        return Err(AppError::validation("Reports are limited to 10 years per run."));
    }
    Ok(Range { from, to, a, b, tz, day })
}

/// SQL expression converting a UTC timestamp column into local time text.
fn local_expr(col: &str, tz: &str) -> String {
    // Offset in seconds for zones without DST (the GCC). For zones with DST the
    // offset at the range start is used; business_date columns are exact.
    let z = time::tz(tz).ok();
    let off = z
        .map(|z| {
            use chrono::Offset;
            use chrono::TimeZone;
            z.offset_from_utc_datetime(&time::now().naive_utc()).fix().local_minus_utc()
        })
        .unwrap_or(0);
    format!("datetime({col}, '{off:+} seconds')")
}

fn sales_totals(c: &Connection, a: &str, b: &str, cashier: Option<&str>) -> AppResult<(i64, i64, i64, i64, i64, i64)> {
    Ok(c.query_row(
        "SELECT COUNT(*), COALESCE(SUM(total_minor),0), COALESCE(SUM(tax_minor),0), COALESCE(SUM(cost_total_minor),0),
                COALESCE(SUM(discount_minor),0), COALESCE(SUM(item_count_milli),0)
         FROM sales WHERE completed_at>=?1 AND completed_at<?2 AND (amw_rbranch() IS NULL OR branch_id=amw_rbranch()) AND (?3 IS NULL OR cashier_user_id=?3)",
        params![a, b, cashier],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    )?)
}

fn refund_totals(c: &Connection, a: &str, b: &str) -> AppResult<(i64, i64, i64, i64)> {
    Ok(c.query_row(
        "SELECT COUNT(*), COALESCE(SUM(total_minor),0), COALESCE(SUM(tax_minor),0), COALESCE(SUM(cost_total_minor),0)
         FROM refunds WHERE created_at>=?1 AND created_at<?2 AND (amw_rbranch() IS NULL OR branch_id=amw_rbranch())",
        params![a, b],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?)
}

fn previous_period(r: &Range) -> AppResult<(String, String)> {
    let f = chrono::NaiveDate::parse_from_str(&r.from, "%Y-%m-%d").unwrap();
    let t = chrono::NaiveDate::parse_from_str(&r.to, "%Y-%m-%d").unwrap();
    let len = (t - f).num_days() + 1;
    let pf = f - chrono::Duration::days(len);
    let pt = f - chrono::Duration::days(1);
    time::local_date_range_utc(&pf.to_string(), &pt.to_string(), &r.day)
}

impl AppCore {
    pub fn reports_catalog(&self, token: &str) -> AppResult<Vec<Value>> {
        let s = self.session(token)?;
        Ok(REPORTS
            .iter()
            .filter(|r| s.has(permission_for(r.0)))
            .map(|(k, t, g, d)| json!({ "key": k, "title": t, "group": g, "description": d }))
            .collect())
    }

    pub fn report_run(&self, token: &str, key: &str, p: ReportParams) -> AppResult<Report> {
        let s = self.session(token)?;
        if !REPORTS.iter().any(|r| r.0 == key) {
            return Err(AppError::validation("Unknown report."));
        }
        s.require(permission_for(key))?;
        let scope = self.db.read(|c| crate::branches::report_scope(c, &s, p.branch_id.as_deref()))?;
        crate::db::with_report_branch(scope, || self.report_run_scoped(&s, key, &p))
    }

    fn report_run_scoped(&self, s: &Session, key: &str, p: &ReportParams) -> AppResult<Report> {
        self.db.read(|c| match key {
            "sales" => self.rep_sales(c, s, p),
            "products" => self.rep_products(c, s, p, false),
            "margin" => self.rep_products(c, s, p, true),
            "categories" => self.rep_categories(c, s, p),
            "payments" => self.rep_payments(c, p),
            "tax" => self.rep_tax(c, p),
            "refunds" => self.rep_refunds(c, p),
            "channels" => self.rep_channels(c, s, p),
            "price_changes" => self.rep_price_changes(c, p),
            "promotions" => self.rep_promotions(c, p),
            "coupons" => self.rep_coupons(c, p),
            "bundles" => self.rep_bundles(c, p),
            "operating_profit" => self.rep_operating_profit(c, p),
            "expenses" => self.rep_expenses(c, p),
            "receivables" => self.rep_receivables(c, p),
            "cash" => self.rep_cash(c, p),
            "inventory" => self.rep_inventory(c, s, p),
            "dead_stock" => self.rep_dead_stock(c, s, p),
            "stock_movements" => self.rep_movements(c, p),
            "purchasing" => self.rep_purchasing(c, s, p),
            "supplier_performance" => self.rep_supplier_performance(c, p),
            "deliveries" => self.rep_deliveries(c, p),
            "audit" => self.rep_audit(c, p),
            _ => Err(AppError::validation("Unknown report.")),
        })
    }

    /// CSV export of any report (money as fixed decimals, barcodes as text).
    pub fn report_csv(&self, token: &str, key: &str, p: ReportParams) -> AppResult<String> {
        let rep = self.report_run(token, key, p)?;
        let digits = self.db.read(|c| self.currency(c))?.1;
        let mut w = csv::WriterBuilder::new().from_writer(vec![]);
        w.write_record(rep.columns.iter().map(|c| crate::validate::csv_safe_cell(c.label.clone())))
            .map_err(|e| AppError::internal(e.to_string()))?;
        let fmt = |c: &Column, v: &Value| -> String {
            match (c.kind.as_str(), v) {
                (_, Value::Null) => String::new(),
                ("money", Value::Number(n)) => format_decimal(n.as_i64().unwrap_or(0), digits),
                ("qty", Value::Number(n)) => format_decimal(n.as_i64().unwrap_or(0), 3),
                ("percent_bp", Value::Number(n)) => format_decimal(n.as_i64().unwrap_or(0), 2),
                (_, Value::String(s)) => crate::validate::csv_safe_cell(s.clone()),
                (_, other) => other.to_string(),
            }
        };
        for r in rep.rows.iter().chain(rep.totals.iter()) {
            let rec: Vec<String> = rep.columns.iter().map(|c| fmt(c, r.get(&c.key).unwrap_or(&Value::Null))).collect();
            w.write_record(&rec).map_err(|e| AppError::internal(e.to_string()))?;
        }
        let bytes = w.into_inner().map_err(|e| AppError::internal(e.to_string()))?;
        String::from_utf8(bytes).map_err(|e| AppError::internal(e.to_string()))
    }

    fn rep_sales(&self, c: &Connection, s: &Session, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let cashier = p.cashier_id.as_deref().filter(|x| !x.is_empty());
        let (n, total, tax, cost, disc, items) = sales_totals(c, &r.a, &r.b, cashier)?;
        let (rn, rtotal, rtax, rcost) = refund_totals(c, &r.a, &r.b)?;
        let (pa, pb) = previous_period(&r)?;
        let (pn, ptotal, ..) = sales_totals(c, &pa, &pb, cashier)?;
        let prev_ref = refund_totals(c, &pa, &pb)?.1;
        let net = total - rtotal;
        let mut kpis = vec![
            kpi("Net sales", net, "money", Some(ptotal - prev_ref)),
            kpi("Transactions", n, "int", Some(pn)),
            kpi("Average basket", if n > 0 { total / n } else { 0 }, "money", Some(if pn > 0 { ptotal / pn } else { 0 })),
            kpi("Items sold", items, "qty", None),
            kpi("Discounts", disc, "money", None),
            kpi("Refunds", rtotal, "money", None),
            kpi("VAT (net of refunds)", tax - rtax, "money", None),
        ];
        if s.has("reports.financial") {
            kpis.push(kpi("Gross profit", (total - tax - cost) - (rtotal - rtax - rcost), "money", None));
        }
        let group = p.group_by.clone().unwrap_or_else(|| "day".into());
        let local = local_expr("s.completed_at", &r.tz);
        let (key_sql, label, join) = match group.as_str() {
            "day" => ("s.business_date".to_string(), "Date", ""),
            "hour" => (format!("strftime('%H:00', {local})"), "Hour", ""),
            "cashier" => {
                ("COALESCE(u.display_name, s.cashier_user_id)".to_string(), "Cashier", "LEFT JOIN users u ON u.user_id=s.cashier_user_id")
            }
            "device" => ("COALESCE(d.name, s.device_id)".to_string(), "Terminal", "LEFT JOIN devices d ON d.device_id=s.device_id"),
            "weekday" => (format!("strftime('%w', {local})"), "Weekday", ""),
            _ => return Err(AppError::validation("Group by day, hour, weekday, cashier or device.")),
        };
        let sql = format!(
            "SELECT {key_sql} AS k, COUNT(*), SUM(s.total_minor), SUM(s.tax_minor), SUM(s.discount_minor), SUM(s.item_count_milli), SUM(s.cost_total_minor)
             FROM sales s {join} WHERE s.completed_at>=?1 AND s.completed_at<?2 AND (amw_rbranch() IS NULL OR s.branch_id=amw_rbranch()) AND (?3 IS NULL OR s.cashier_user_id=?3) GROUP BY k ORDER BY k"
        );
        let mut st = c.prepare(&sql)?;
        let show_profit = s.has("reports.financial");
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b, cashier], |row| {
                let n: i64 = row.get(1)?;
                let t: i64 = row.get(2)?;
                let tax: i64 = row.get(3)?;
                let cost: i64 = row.get(6)?;
                let mut k: String = row.get(0)?;
                if group == "weekday" {
                    k = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"][k.parse::<usize>().unwrap_or(0) % 7].to_string();
                }
                Ok(json!({ "key": k, "transactions": n, "total": t, "tax": tax, "discount": row.get::<_, i64>(4)?, "items": row.get::<_, i64>(5)?,
                    "average": if n > 0 { t / n } else { 0 }, "profit": if show_profit { Some(t - tax - cost) } else { None } }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut columns = vec![
            col("key", label, "text"),
            col("transactions", "Transactions", "int"),
            col("items", "Items", "qty"),
            col("discount", "Discounts", "money"),
            col("tax", "VAT", "money"),
            col("total", "Sales", "money"),
            col("average", "Avg basket", "money"),
        ];
        if show_profit {
            columns.push(col("profit", "Gross profit", "money"));
        }
        let series = rows.iter().map(|r| json!({ "label": r["key"], "value": r["total"] })).collect();
        Ok(Report {
            key: "sales".into(),
            title: "Sales".into(),
            from: r.from,
            to: r.to,
            kpis,
            columns,
            totals: Some(json!({ "key": "Total", "transactions": n, "items": items, "discount": disc, "tax": tax, "total": total,
                "average": if n > 0 { total / n } else { 0 }, "profit": if show_profit { Some(total - tax - cost) } else { None } })),
            rows,
            series: Some(series),
            notes: vec![format!("{rn} refund(s) totalling {} are deducted in Net sales.", format_decimal(rtotal, self.currency(c)?.1))],
        })
    }

    fn rep_products(&self, c: &Connection, s: &Session, p: &ReportParams, margin: bool) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let show_cost = s.has("reports.financial");
        if margin && !show_cost {
            return Err(AppError::forbidden("reports.financial"));
        }
        let limit = p.limit.unwrap_or(1000).clamp(1, 20000);
        let cat = p.category_id.as_deref().filter(|x| !x.is_empty());
        let sql = format!(
            "WITH sold AS (
                SELECT COALESCE(i.product_id, 'custom:' || i.product_name_snapshot) AS pid, MAX(i.product_name_snapshot) AS name, MAX(i.sku_snapshot) AS sku,
                       MAX(i.category_id_snapshot) AS cat, SUM(i.qty_milli) AS qty, SUM(i.line_total_minor) AS total, SUM(i.tax_minor) AS tax,
                       SUM(i.cost_snapshot_minor) AS cost, SUM(i.discount_minor) AS disc
                FROM sale_items i JOIN sales s ON s.sale_id=i.sale_id
                WHERE s.completed_at>=?1 AND s.completed_at<?2 AND (amw_rbranch() IS NULL OR s.branch_id=amw_rbranch()) AND (?3 IS NULL OR i.category_id_snapshot=?3)
                GROUP BY pid),
             ref AS (
                SELECT COALESCE(ri.product_id, 'custom:' || si.product_name_snapshot) AS pid, SUM(ri.qty_milli) AS qty, SUM(ri.amount_minor) AS total,
                       SUM(ri.tax_minor) AS tax, SUM(ri.cost_minor) AS cost
                FROM refund_items ri JOIN refunds rf ON rf.refund_id=ri.refund_id JOIN sale_items si ON si.sale_item_id=ri.original_sale_item_id
                WHERE rf.created_at>=?1 AND rf.created_at<?2 AND (amw_rbranch() IS NULL OR rf.branch_id=amw_rbranch()) AND (?3 IS NULL OR si.category_id_snapshot=?3)
                GROUP BY pid)
             SELECT sold.pid, sold.name, sold.sku, COALESCE(c.name,''), sold.qty - COALESCE(ref.qty,0), sold.total - COALESCE(ref.total,0),
                    sold.tax - COALESCE(ref.tax,0), sold.cost - COALESCE(ref.cost,0), sold.disc
             FROM sold LEFT JOIN ref ON ref.pid=sold.pid LEFT JOIN categories c ON c.category_id=sold.cat
             ORDER BY {} DESC LIMIT {limit}",
            if margin { "(sold.total - COALESCE(ref.total,0)) - (sold.tax - COALESCE(ref.tax,0)) - (sold.cost - COALESCE(ref.cost,0))" } else { "sold.total - COALESCE(ref.total,0)" }
        );
        let mut st = c.prepare(&sql)?;
        let mut tq = 0;
        let mut tt = 0;
        let mut ttax = 0;
        let mut tc = 0;
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b, cat], |row| {
                let qty: i64 = row.get(4)?;
                let total: i64 = row.get(5)?;
                let tax: i64 = row.get(6)?;
                let cost: i64 = row.get(7)?;
                let net = total - tax;
                let profit = net - cost;
                Ok(json!({
                    "product_id": row.get::<_, String>(0)?, "name": row.get::<_, String>(1)?, "sku": row.get::<_, Option<String>>(2)?,
                    "category": row.get::<_, String>(3)?, "qty": qty, "total": total, "net": net, "discount": row.get::<_, i64>(8)?,
                    "cost": if show_cost { Some(cost) } else { None }, "profit": if show_cost { Some(profit) } else { None },
                    "margin_bp": if show_cost && net != 0 { Some(profit * 10000 / net) } else { None },
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for row in &rows {
            tq += row["qty"].as_i64().unwrap_or(0);
            tt += row["total"].as_i64().unwrap_or(0);
            ttax += row["total"].as_i64().unwrap_or(0) - row["net"].as_i64().unwrap_or(0);
            tc += row["cost"].as_i64().unwrap_or(0);
        }
        let mut columns = vec![
            col("name", "Product", "text"),
            col("sku", "SKU", "text"),
            col("category", "Category", "text"),
            col("qty", "Qty sold", "qty"),
            col("discount", "Discounts", "money"),
            col("total", "Sales (incl. VAT)", "money"),
        ];
        if show_cost {
            columns.extend([
                col("net", "Revenue (ex. VAT)", "money"),
                col("cost", "Cost", "money"),
                col("profit", "Gross profit", "money"),
                col("margin_bp", "Margin %", "percent_bp"),
            ]);
        }
        let mut kpis =
            vec![kpi("Products sold", rows.len() as i64, "int", None), kpi("Units", tq, "qty", None), kpi("Sales", tt, "money", None)];
        if show_cost {
            let net = tt - ttax;
            kpis.push(kpi("Revenue (ex. VAT)", net, "money", None));
            kpis.push(kpi("Cost", tc, "money", None));
            kpis.push(kpi("Gross profit", net - tc, "money", None));
            kpis.push(kpi("Margin %", if net != 0 { (net - tc) * 10000 / net } else { 0 }, "percent_bp", None));
        }
        let negative = rows.iter().filter(|r| r["profit"].as_i64().map(|p| p < 0).unwrap_or(false)).count();
        Ok(Report {
            key: if margin { "margin".into() } else { "products".into() },
            title: if margin { "Margin & profit".into() } else { "Product sales".into() },
            from: r.from,
            to: r.to,
            kpis,
            columns,
            totals: Some(
                json!({ "name": "Total", "qty": tq, "total": tt, "net": tt - ttax, "cost": if show_cost { Some(tc) } else { None },
                "profit": if show_cost { Some(tt - ttax - tc) } else { None } }),
            ),
            series: Some(rows.iter().take(10).map(|r| json!({ "label": r["name"], "value": r["total"] })).collect()),
            rows,
            notes: if negative > 0 { vec![format!("{negative} product(s) sold below cost in this period.")] } else { vec![] },
        })
    }

    fn rep_categories(&self, c: &Connection, s: &Session, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let show_cost = s.has("reports.financial");
        let mut st = c.prepare(
            "SELECT COALESCE(c.name, 'Uncategorised'), SUM(i.qty_milli), SUM(i.line_total_minor), SUM(i.tax_minor), SUM(i.cost_snapshot_minor), COUNT(DISTINCT i.sale_id)
             FROM sale_items i JOIN sales s ON s.sale_id=i.sale_id LEFT JOIN categories c ON c.category_id=i.category_id_snapshot
             WHERE s.completed_at>=?1 AND s.completed_at<?2 AND (amw_rbranch() IS NULL OR s.branch_id=amw_rbranch()) GROUP BY 1 ORDER BY 3 DESC",
        )?;
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |row| {
                let total: i64 = row.get(2)?;
                let tax: i64 = row.get(3)?;
                let cost: i64 = row.get(4)?;
                Ok(json!({ "category": row.get::<_, String>(0)?, "qty": row.get::<_, i64>(1)?, "total": total, "baskets": row.get::<_, i64>(5)?,
                    "profit": if show_cost { Some(total - tax - cost) } else { None },
                    "margin_bp": if show_cost && total - tax != 0 { Some((total - tax - cost) * 10000 / (total - tax)) } else { None } }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let sum: i64 = rows.iter().map(|r| r["total"].as_i64().unwrap_or(0)).sum();
        let rows: Vec<Value> = rows
            .into_iter()
            .map(|mut r| {
                r["share_bp"] = json!(if sum > 0 { r["total"].as_i64().unwrap_or(0) * 10000 / sum } else { 0 });
                r
            })
            .collect();
        let mut columns = vec![
            col("category", "Category", "text"),
            col("qty", "Qty", "qty"),
            col("baskets", "Baskets", "int"),
            col("total", "Sales", "money"),
            col("share_bp", "Share %", "percent_bp"),
        ];
        if show_cost {
            columns.extend([col("profit", "Gross profit", "money"), col("margin_bp", "Margin %", "percent_bp")]);
        }
        Ok(Report {
            key: "categories".into(),
            title: "Category sales".into(),
            from: r.from,
            to: r.to,
            kpis: vec![kpi("Sales", sum, "money", None), kpi("Categories", rows.len() as i64, "int", None)],
            series: Some(rows.iter().map(|r| json!({ "label": r["category"], "value": r["total"] })).collect()),
            columns,
            totals: Some(json!({ "category": "Total", "total": sum })),
            rows,
            notes: vec!["Gross category sales before refunds.".into()],
        })
    }

    fn rep_payments(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare(
            "WITH pay AS (SELECT p.method m, SUM(p.amount_minor) a, COUNT(*) n FROM payments p JOIN sales s ON s.sale_id=p.sale_id
                          WHERE s.completed_at>=?1 AND s.completed_at<?2 AND (amw_rbranch() IS NULL OR s.branch_id=amw_rbranch()) GROUP BY p.method),
                  ref AS (SELECT t.method m, SUM(t.amount_minor) a FROM refund_tenders t JOIN refunds r ON r.refund_id=t.refund_id
                          WHERE r.created_at>=?1 AND r.created_at<?2 AND (amw_rbranch() IS NULL OR r.branch_id=amw_rbranch()) GROUP BY t.method),
                  ms AS (SELECT m FROM pay UNION SELECT m FROM ref)
             SELECT ms.m, COALESCE(pay.n,0), COALESCE(pay.a,0), COALESCE(ref.a,0) FROM ms LEFT JOIN pay ON pay.m=ms.m LEFT JOIN ref ON ref.m=ms.m ORDER BY 3 DESC",
        )?;
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |row| {
                let a: i64 = row.get(2)?;
                let rf: i64 = row.get(3)?;
                Ok(json!({ "method": crate::receipt::method_label(&row.get::<_, String>(0)?), "count": row.get::<_, i64>(1)?, "received": a, "refunded": rf, "net": a - rf }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let net: i64 = rows.iter().map(|r| r["net"].as_i64().unwrap_or(0)).sum();
        Ok(Report {
            key: "payments".into(),
            title: "Payment methods".into(),
            from: r.from,
            to: r.to,
            kpis: vec![kpi("Net tenders", net, "money", None)],
            columns: vec![
                col("method", "Method", "text"),
                col("count", "Payments", "int"),
                col("received", "Received", "money"),
                col("refunded", "Refunded", "money"),
                col("net", "Net", "money"),
            ],
            series: Some(rows.iter().map(|r| json!({ "label": r["method"], "value": r["net"] })).collect()),
            totals: Some(json!({ "method": "Total", "received": rows.iter().map(|r| r["received"].as_i64().unwrap_or(0)).sum::<i64>(),
                "refunded": rows.iter().map(|r| r["refunded"].as_i64().unwrap_or(0)).sum::<i64>(), "net": net })),
            rows,
            notes: vec!["Card and BenefitPay amounts are recorded tenders, not verified bank settlements.".into()],
        })
    }

    fn rep_tax(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare(
            "WITH s AS (SELECT i.tax_rate_bp rate, i.tax_inclusive incl, SUM(i.line_total_minor) gross, SUM(i.tax_minor) tax FROM sale_items i JOIN sales x ON x.sale_id=i.sale_id
                        WHERE x.completed_at>=?1 AND x.completed_at<?2 AND (amw_rbranch() IS NULL OR x.branch_id=amw_rbranch()) GROUP BY 1,2),
                  r AS (SELECT si.tax_rate_bp rate, si.tax_inclusive incl, SUM(ri.amount_minor) gross, SUM(ri.tax_minor) tax FROM refund_items ri
                        JOIN refunds rf ON rf.refund_id=ri.refund_id JOIN sale_items si ON si.sale_item_id=ri.original_sale_item_id
                        WHERE rf.created_at>=?1 AND rf.created_at<?2 AND (amw_rbranch() IS NULL OR rf.branch_id=amw_rbranch()) GROUP BY 1,2),
                  k AS (SELECT rate, incl FROM s UNION SELECT rate, incl FROM r)
             SELECT k.rate, k.incl, COALESCE(s.gross,0), COALESCE(s.tax,0), COALESCE(r.gross,0), COALESCE(r.tax,0)
             FROM k LEFT JOIN s ON s.rate=k.rate AND s.incl=k.incl LEFT JOIN r ON r.rate=k.rate AND r.incl=k.incl ORDER BY k.rate DESC",
        )?;
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |row| {
                let rate: i64 = row.get(0)?;
                let sg: i64 = row.get(2)?;
                let stx: i64 = row.get(3)?;
                let rg: i64 = row.get(4)?;
                let rtx: i64 = row.get(5)?;
                let gross = sg - rg;
                let tax = stx - rtx;
                Ok(json!({ "rate": format!("{}%{}", format_decimal(rate, 2).trim_end_matches('0').trim_end_matches('.'), if row.get::<_, i64>(1)? == 1 { " (incl.)" } else { "" }),
                    "rate_bp": rate, "sales_gross": sg, "refunds_gross": rg, "net_taxable": gross - tax, "vat": tax, "gross": gross }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let vat: i64 = rows.iter().map(|r| r["vat"].as_i64().unwrap_or(0)).sum();
        let net: i64 = rows.iter().map(|r| r["net_taxable"].as_i64().unwrap_or(0)).sum();
        let gross: i64 = rows.iter().map(|r| r["gross"].as_i64().unwrap_or(0)).sum();
        Ok(Report {
            key: "tax".into(),
            title: "VAT".into(),
            from: r.from,
            to: r.to,
            kpis: vec![kpi("Taxable sales (net)", net, "money", None), kpi("VAT", vat, "money", None), kpi("Gross", gross, "money", None)],
            columns: vec![col("rate", "Rate", "text"), col("sales_gross", "Sales (gross)", "money"), col("refunds_gross", "Refunds (gross)", "money"),
                col("net_taxable", "Net taxable", "money"), col("vat", "VAT", "money"), col("gross", "Gross", "money")],
            totals: Some(json!({ "rate": "Total", "net_taxable": net, "vat": vat, "gross": gross })),
            rows,
            series: None,
            notes: vec!["VAT uses the rate snapshotted on each sale line. Refunds reduce the period in which they were issued. Verify filing treatment with your tax adviser.".into()],
        })
    }

    /// Operating profit for a business-date range. Definitions (stated on
    /// the report): revenue = sales less refunds and voids, excluding VAT;
    /// cost of goods = cost at sale less cost of goods returned; gross
    /// profit = revenue − cost of goods; operating expenses = approved and
    /// paid expenses excluding VAT; operating profit = gross profit −
    /// operating expenses. Taxes on profit, depreciation and interest are
    /// not modelled, so this is not called net profit.
    fn rep_operating_profit(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let (_, total, tax, cost, _, _) = sales_totals(c, &r.a, &r.b, None)?;
        let (_, rtotal, rtax, rcost) = refund_totals(c, &r.a, &r.b)?;
        let revenue = (total - tax) - (rtotal - rtax);
        let cogs = cost - rcost;
        let gross = revenue - cogs;
        let branch: Option<String> = c.query_row("SELECT amw_rbranch()", [], |row| row.get(0)).ok().flatten();
        let (opex, by_cat) = crate::expenses::operating_expenses(c, &r.from, &r.to, branch.as_deref())?;
        let operating = gross - opex;
        let mut rows = vec![
            json!({ "line": "Revenue (excluding VAT)", "amount": revenue }),
            json!({ "line": "Cost of goods sold", "amount": -cogs }),
            json!({ "line": "Gross profit", "amount": gross, "strong": true }),
        ];
        for (_, name, _, v) in &by_cat {
            rows.push(json!({ "line": name, "amount": -v, "indent": true }));
        }
        rows.push(json!({ "line": "Operating expenses", "amount": -opex }));
        rows.push(json!({ "line": "Operating profit", "amount": operating, "strong": true }));
        let pct = |v: i64| if revenue != 0 { v * 10_000 / revenue } else { 0 };
        Ok(Report {
            key: "operating_profit".into(),
            title: "Operating profit".into(),
            from: r.from.clone(),
            to: r.to.clone(),
            kpis: vec![
                kpi("Revenue", revenue, "money", None),
                kpi("Gross profit", gross, "money", None),
                kpi("Operating expenses", opex, "money", None),
                kpi("Operating profit", operating, "money", None),
                kpi("Operating margin", pct(operating), "percent_bp", None),
            ],
            columns: vec![col("line", "", "label"), col("amount", "Amount", "money")],
            totals: None,
            series: Some(by_cat.iter().map(|(_, n, _, v)| json!({ "label": n, "value": v })).collect()),
            rows,
            notes: vec![
                "Revenue is sales less refunds and voids, without VAT. Cost of goods is the cost recorded at each sale, less goods returned.".into(),
                "Operating expenses are approved and paid expenses without VAT, by the date they belong to.".into(),
                "Tax on profit, depreciation and interest are not included, so this is operating profit, not net profit.".into(),
            ],
        })
    }

    /// Customer balances aged as of the end of the range (FIFO, see credit.rs).
    fn rep_receivables(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare(
            "SELECT cu.customer_id, cu.name, cu.phone FROM customers cu
             WHERE EXISTS (SELECT 1 FROM customer_ledger l WHERE l.customer_id=cu.customer_id) ORDER BY cu.name",
        )?;
        let custs = st
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut rows = vec![];
        let mut t = crate::credit::Ageing::default();
        for (id, name, phone) in custs {
            let a = crate::credit::customer_ageing(c, &id, &r.to)?;
            if a.total() == 0 {
                continue;
            }
            t.current_minor += a.current_minor;
            t.d1_30_minor += a.d1_30_minor;
            t.d31_60_minor += a.d31_60_minor;
            t.d61_90_minor += a.d61_90_minor;
            t.d90_plus_minor += a.d90_plus_minor;
            rows.push(
                json!({ "customer": name, "phone": phone, "current": a.current_minor, "d1_30": a.d1_30_minor, "d31_60": a.d31_60_minor,
                "d61_90": a.d61_90_minor, "d90": a.d90_plus_minor, "total": a.total() }),
            );
        }
        Ok(Report {
            key: "receivables".into(),
            title: "Customer balances".into(),
            from: r.from,
            to: r.to,
            kpis: vec![kpi("Owed by customers", t.total(), "money", None), kpi("Overdue", t.overdue(), "money", None), kpi("Customers", rows.len() as i64, "int", None)],
            columns: vec![
                col("customer", "Customer", "text"),
                col("phone", "Phone", "text"),
                col("current", "Not due yet", "money"),
                col("d1_30", "1–30 days late", "money"),
                col("d31_60", "31–60 days late", "money"),
                col("d61_90", "61–90 days late", "money"),
                col("d90", "Over 90 days late", "money"),
                col("total", "Balance", "money"),
            ],
            totals: Some(json!({ "customer": "Total", "current": t.current_minor, "d1_30": t.d1_30_minor, "d31_60": t.d31_60_minor,
                "d61_90": t.d61_90_minor, "d90": t.d90_plus_minor, "total": t.total() })),
            series: None,
            rows,
            notes: vec!["Balances are aged as of the end date. Payments and credits clear the oldest charges first; lateness counts from each charge plus the customer's terms.".into()],
        })
    }

    fn rep_expenses(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare(
            "SELECT e.number, e.business_date, c.name, COALESCE(e.payee, s.name), e.description, e.net_minor, e.vat_minor, e.total_minor, e.status, e.payment_method
             FROM expenses e JOIN expense_categories c ON c.category_id=e.category_id LEFT JOIN suppliers s ON s.supplier_id=e.supplier_id
             WHERE e.status IN ('approved','paid') AND e.business_date>=?1 AND e.business_date<=?2 AND (amw_rbranch() IS NULL OR e.branch_id=amw_rbranch())
             ORDER BY e.business_date, e.number LIMIT 20000",
        )?;
        let rows: Vec<Value> = st
            .query_map(params![r.from, r.to], |row| {
                Ok(json!({ "number": row.get::<_, String>(0)?, "date": row.get::<_, String>(1)?, "category": row.get::<_, String>(2)?,
                    "payee": row.get::<_, Option<String>>(3)?, "description": row.get::<_, String>(4)?, "net": row.get::<_, i64>(5)?,
                    "vat": row.get::<_, i64>(6)?, "total": row.get::<_, i64>(7)?, "status": row.get::<_, String>(8)?, "method": row.get::<_, Option<String>>(9)? }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let net: i64 = rows.iter().map(|x| x["net"].as_i64().unwrap_or(0)).sum();
        let vat: i64 = rows.iter().map(|x| x["vat"].as_i64().unwrap_or(0)).sum();
        let branch: Option<String> = c.query_row("SELECT amw_rbranch()", [], |row| row.get(0)).ok().flatten();
        let (_, by_cat) = crate::expenses::operating_expenses(c, &r.from, &r.to, branch.as_deref())?;
        Ok(Report {
            key: "expenses".into(),
            title: "Expenses".into(),
            from: r.from,
            to: r.to,
            kpis: vec![
                kpi("Expenses", net, "money", None),
                kpi("VAT paid on expenses", vat, "money", None),
                kpi("Entries", rows.len() as i64, "int", None),
            ],
            columns: vec![
                col("number", "No.", "text"),
                col("date", "Date", "date"),
                col("category", "Category", "label"),
                col("payee", "Paid to", "text"),
                col("description", "Description", "text"),
                col("status", "Status", "status"),
                col("net", "Without VAT", "money"),
                col("vat", "VAT", "money"),
                col("total", "Total", "money"),
            ],
            totals: Some(json!({ "number": "Total", "net": net, "vat": vat, "total": net + vat })),
            series: Some(by_cat.iter().map(|(_, n, _, v)| json!({ "label": n, "value": v })).collect()),
            rows,
            notes: vec![],
        })
    }

    /// Sales by where they came from. Sales before Wave 5 have no channel and
    /// are shown as "Not recorded", never assigned. Refunds count against the
    /// channel of the sale they refund, on the day they were made. Gross
    /// margin is sales minus cost of goods only — not a channel profit.
    fn rep_channels(&self, c: &Connection, s: &Session, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let fin = s.has("reports.financial");
        let mut st = c.prepare(
            "WITH sold AS (
                SELECT COALESCE(channel,'') AS ch, COUNT(*) AS n, SUM(total_minor) AS total, SUM(tax_minor) AS tax, SUM(cost_total_minor) AS cost,
                       SUM(item_count_milli) AS items
                FROM sales WHERE completed_at>=?1 AND completed_at<?2 AND (amw_rbranch() IS NULL OR branch_id=amw_rbranch()) GROUP BY ch),
             ref AS (
                SELECT COALESCE(s.channel,'') AS ch, COUNT(*) AS n, SUM(rf.total_minor) AS total, SUM(rf.tax_minor) AS tax, SUM(rf.cost_total_minor) AS cost
                FROM refunds rf JOIN sales s ON s.sale_id=rf.original_sale_id
                WHERE rf.created_at>=?1 AND rf.created_at<?2 AND (amw_rbranch() IS NULL OR rf.branch_id=amw_rbranch()) GROUP BY ch),
             chans AS (SELECT ch FROM sold UNION SELECT ch FROM ref)
             SELECT chans.ch, COALESCE(sold.n,0), COALESCE(sold.total,0), COALESCE(sold.tax,0), COALESCE(sold.cost,0), COALESCE(sold.items,0),
                    COALESCE(ref.n,0), COALESCE(ref.total,0), COALESCE(ref.tax,0), COALESCE(ref.cost,0)
             FROM chans LEFT JOIN sold ON sold.ch=chans.ch LEFT JOIN ref ON ref.ch=chans.ch
             ORDER BY COALESCE(sold.total,0) DESC",
        )?;
        let label = |ch: &str| match ch {
            "pos" => "Till",
            "whatsapp" => "WhatsApp",
            "phone" => "Phone",
            "web" => "Web",
            "other" => "Other",
            _ => "Not recorded",
        };
        let mut sum = [0i64; 9];
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |row| {
                let ch: String = row.get(0)?;
                let v: Vec<i64> = (1..10).map(|i| row.get::<_, i64>(i)).collect::<Result<_, _>>()?;
                Ok((ch, v))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(ch, v)| {
                for (i, x) in v.iter().enumerate() {
                    sum[i] += x;
                }
                let (n, total, tax, cost, items, rn, rtotal, rtax, rcost) = (v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8]);
                let net = total - rtotal;
                let gm = (total - tax - cost) - (rtotal - rtax - rcost);
                let revenue = (total - tax) - (rtotal - rtax);
                json!({ "channel": ch, "key": label(&ch), "transactions": n, "items": items, "sales": total, "average": if n > 0 { total / n } else { 0 },
                    "refunds": rtotal, "refund_count": rn, "net": net, "revenue": revenue,
                    "gross_margin": if fin { Some(gm) } else { None },
                    "gross_margin_bp": if fin && revenue > 0 { Some(gm * 10_000 / revenue) } else { None } })
            })
            .collect();
        let mut columns = vec![
            col("key", "Channel", "text"),
            col("transactions", "Transactions", "int"),
            col("items", "Items", "qty"),
            col("sales", "Sales", "money"),
            col("average", "Avg basket", "money"),
            col("refunds", "Refunds", "money"),
            col("net", "Net sales", "money"),
        ];
        if fin {
            columns.push(col("gross_margin", "Gross margin", "money"));
            columns.push(col("gross_margin_bp", "Gross margin %", "percent_bp"));
        }
        let (n, total) = (sum[0], sum[1]);
        let series = rows.iter().map(|x| json!({ "label": x["key"], "value": x["net"] })).collect();
        let mut kpis = vec![
            kpi("Net sales", total - sum[6], "money", None),
            kpi("Transactions", n, "int", None),
            kpi("Average basket", if n > 0 { total / n } else { 0 }, "money", None),
        ];
        let unrecorded: i64 = rows.iter().filter(|x| x["channel"] == "").map(|x| x["transactions"].as_i64().unwrap_or(0)).sum();
        if unrecorded > 0 {
            kpis.push(kpi("Sales with no channel recorded", unrecorded, "int", None));
        }
        Ok(Report {
            key: "channels".into(),
            title: "Sales by channel".into(),
            from: r.from,
            to: r.to,
            kpis,
            columns,
            totals: Some(json!({ "key": "Total", "transactions": n, "items": sum[4], "sales": total, "average": if n > 0 { total / n } else { 0 },
                "refunds": sum[6], "net": total - sum[6],
                "gross_margin": if fin { Some((total - sum[2] - sum[3]) - (sum[6] - sum[7] - sum[8])) } else { None } })),
            rows,
            series: Some(series),
            notes: vec![
                "Gross margin is sales minus the cost of the goods. It is not a profit per channel: delivery, fees and commissions are not included.".into(),
                "Sales made before channels were recorded are shown as Not recorded.".into(),
            ],
        })
    }

    /// Applied price changes (the price history), newest first.
    fn rep_price_changes(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let limit = p.limit.unwrap_or(1000).clamp(1, 20000);
        let mut st = c.prepare(&format!(
            "SELECT pp.created_at, pr.name, pr.sku, pp.price_type, pp.branch_id, pp.amount_minor,
                (SELECT o.amount_minor FROM product_prices o WHERE o.product_id=pp.product_id AND o.price_type=pp.price_type
                   AND o.branch_id IS pp.branch_id AND o.price_id<>pp.price_id AND o.effective_from<=pp.effective_from
                 ORDER BY o.effective_from DESC, o.price_id DESC LIMIT 1),
                pp.reason, u.display_name, pol.name, pp.batch_id
             FROM product_prices pp JOIN products pr ON pr.product_id=pp.product_id LEFT JOIN users u ON u.user_id=pp.created_by
             LEFT JOIN pricing_policies pol ON pol.policy_id=pp.policy_id
             WHERE pp.created_at>=?1 AND pp.created_at<?2 ORDER BY pp.created_at DESC, pp.price_id DESC LIMIT {limit}"
        ))?;
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |x| {
                let new: i64 = x.get(5)?;
                let old: Option<i64> = x.get(6)?;
                Ok(json!({ "at": x.get::<_, String>(0)?, "name": x.get::<_, String>(1)?, "sku": x.get::<_, String>(2)?,
                    "list": x.get::<_, String>(3)?, "branch_id": x.get::<_, Option<String>>(4)?, "new": new, "old": old,
                    "change": old.map(|o| new - o), "reason": x.get::<_, Option<String>>(7)?, "by": x.get::<_, Option<String>>(8)?,
                    "policy": x.get::<_, Option<String>>(9)?, "batch": x.get::<_, Option<String>>(10)? }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let n = rows.len() as i64;
        let ups = rows.iter().filter(|x| x["change"].as_i64().is_some_and(|d| d > 0)).count() as i64;
        let downs = rows.iter().filter(|x| x["change"].as_i64().is_some_and(|d| d < 0)).count() as i64;
        Ok(Report {
            key: "price_changes".into(),
            title: "Price changes".into(),
            from: r.from,
            to: r.to,
            kpis: vec![kpi("Price changes", n, "int", None), kpi("Increases", ups, "int", None), kpi("Decreases", downs, "int", None)],
            columns: vec![
                col("at", "When", "datetime"),
                col("name", "Product", "text"),
                col("list", "Price list", "text"),
                col("old", "Old price", "money"),
                col("new", "New price", "money"),
                col("change", "Change", "money"),
                col("reason", "Reason", "text"),
                col("policy", "Policy", "text"),
                col("by", "By", "text"),
            ],
            totals: None,
            rows,
            series: None,
            notes: vec!["Only applied changes are listed; recommendations that were not applied leave no history.".into()],
        })
    }

    fn rep_promotions(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare(
            "WITH lines AS (
                SELECT sp.promotion_id, MAX(sp.promotion_name) AS name, sp.sale_id, sp.sale_item_id, SUM(sp.amount_minor) AS disc
                FROM sale_item_promotions sp JOIN sales s ON s.sale_id=sp.sale_id
                WHERE s.completed_at>=?1 AND s.completed_at<?2 AND (amw_rbranch() IS NULL OR s.branch_id=amw_rbranch())
                GROUP BY sp.promotion_id, sp.sale_item_id)
             SELECT l.promotion_id, MAX(l.name), COUNT(DISTINCT l.sale_id), SUM(l.disc), SUM(si.line_total_minor), SUM(si.tax_minor),
                    SUM(si.cost_snapshot_minor)
             FROM lines l JOIN sale_items si ON si.sale_item_id=l.sale_item_id
             GROUP BY l.promotion_id ORDER BY SUM(l.disc) DESC, l.promotion_id",
        )?;
        let mut tot = [0i64; 4];
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |x| {
                let (sales, disc, rev, tax, cost): (i64, i64, i64, i64, i64) = (x.get(2)?, x.get(3)?, x.get(4)?, x.get(5)?, x.get(6)?);
                Ok((
                    sales,
                    disc,
                    rev - tax,
                    cost,
                    json!({ "name": x.get::<_, String>(1)?, "sales": sales, "discount": disc,
                    "revenue": rev - tax, "cost": cost, "gross_margin": rev - tax - cost }),
                ))
            })?
            .map(|r| {
                r.map(|(n, d, rv, c, v)| {
                    tot[0] += n;
                    tot[1] += d;
                    tot[2] += rv;
                    tot[3] += c;
                    v
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Report {
            key: "promotions".into(),
            title: "Promotions".into(),
            from: r.from,
            to: r.to,
            kpis: vec![kpi("Discount given", tot[1], "money", None), kpi("Revenue of offer lines", tot[2], "money", None)],
            columns: vec![
                col("name", "Offer", "text"),
                col("sales", "Sales", "int"),
                col("discount", "Discount given", "money"),
                col("revenue", "Revenue (ex VAT)", "money"),
                col("cost", "Cost", "money"),
                col("gross_margin", "Gross margin", "money"),
            ],
            totals: Some(json!({ "name": "Total", "discount": tot[1], "revenue": tot[2], "cost": tot[3], "gross_margin": tot[2] - tot[3] })),
            rows,
            series: None,
            notes: vec![
                "Figures are what the sales recorded for the lines each offer touched; they do not say what would have sold without the offer.".into(),
                "Refunds are not deducted here.".into(),
            ],
        })
    }

    fn rep_coupons(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare(
            "SELECT cr.code, COALESCE(pr.name,''), cp.kind, cp.max_redemptions, COUNT(*), SUM(cr.amount_minor),
                    (SELECT COUNT(*) FROM coupon_redemptions a WHERE a.coupon_id=cr.coupon_id)
             FROM coupon_redemptions cr LEFT JOIN coupons cp ON cp.coupon_id=cr.coupon_id LEFT JOIN promotions pr ON pr.promotion_id=cr.promotion_id
             WHERE cr.created_at>=?1 AND cr.created_at<?2 AND (amw_rbranch() IS NULL OR cr.branch_id=amw_rbranch())
             GROUP BY cr.coupon_id ORDER BY COUNT(*) DESC, cr.code",
        )?;
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |x| {
                let max: Option<i64> = x.get(3)?;
                let all: i64 = x.get(6)?;
                Ok(json!({ "code": x.get::<_, String>(0)?, "offer": x.get::<_, String>(1)?, "kind": x.get::<_, Option<String>>(2)?,
                    "redemptions": x.get::<_, i64>(4)?, "discount": x.get::<_, i64>(5)?, "left": max.map(|m| (m - all).max(0)) }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let n: i64 = rows.iter().map(|x| x["redemptions"].as_i64().unwrap_or(0)).sum();
        let d: i64 = rows.iter().map(|x| x["discount"].as_i64().unwrap_or(0)).sum();
        Ok(Report {
            key: "coupons".into(),
            title: "Coupon redemptions".into(),
            from: r.from,
            to: r.to,
            kpis: vec![kpi("Redemptions", n, "int", None), kpi("Discount given", d, "money", None)],
            columns: vec![
                col("code", "Code", "text"),
                col("offer", "Offer", "text"),
                col("redemptions", "Redemptions", "int"),
                col("discount", "Discount given", "money"),
                col("left", "Uses left", "int"),
            ],
            totals: None,
            rows,
            series: None,
            notes: vec!["Only completed sales redeem a coupon; previews and held sales are not counted.".into()],
        })
    }

    fn rep_bundles(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare(
            "WITH b AS (
                SELECT si.bundle_product_id AS pid, MAX(si.bundle_name) AS name, si.sale_id, si.bundle_line_no,
                       MAX(si.bundle_qty_milli) AS qty, SUM(si.line_total_minor) AS total, SUM(si.tax_minor) AS tax, SUM(si.cost_snapshot_minor) AS cost
                FROM sale_items si JOIN sales s ON s.sale_id=si.sale_id
                WHERE si.bundle_product_id IS NOT NULL AND s.completed_at>=?1 AND s.completed_at<?2
                  AND (amw_rbranch() IS NULL OR s.branch_id=amw_rbranch())
                GROUP BY si.bundle_product_id, si.sale_id, si.bundle_line_no)
             SELECT pid, MAX(name), SUM(qty), SUM(total), SUM(tax), SUM(cost) FROM b GROUP BY pid ORDER BY SUM(total) DESC, pid",
        )?;
        let rows: Vec<(String, Value)> = st
            .query_map(params![r.a, r.b], |x| {
                let (qty, total, tax, cost): (i64, i64, i64, i64) = (x.get(2)?, x.get(3)?, x.get(4)?, x.get(5)?);
                Ok((
                    x.get::<_, String>(0)?,
                    json!({ "name": x.get::<_, String>(1)?, "units": qty, "revenue": total - tax, "cost": cost,
                    "gross_margin": total - tax - cost }),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = vec![];
        let mut comp_st = c.prepare(
            "SELECT si.product_name_snapshot, SUM(si.qty_milli) FROM sale_items si JOIN sales s ON s.sale_id=si.sale_id
             WHERE si.bundle_product_id=?3 AND s.completed_at>=?1 AND s.completed_at<?2 AND (amw_rbranch() IS NULL OR s.branch_id=amw_rbranch())
             GROUP BY si.product_id ORDER BY si.product_name_snapshot",
        )?;
        for (pid, mut v) in rows {
            let used: Vec<String> = comp_st
                .query_map(params![r.a, r.b, pid], |x| {
                    Ok(format!("{} × {}", x.get::<_, String>(0)?, crate::money::format_qty(x.get::<_, i64>(1)?)))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            v["items_used"] = json!(used.join(", "));
            out.push(v);
        }
        let units: i64 = out.iter().map(|x| x["units"].as_i64().unwrap_or(0)).sum();
        let rev: i64 = out.iter().map(|x| x["revenue"].as_i64().unwrap_or(0)).sum();
        Ok(Report {
            key: "bundles".into(),
            title: "Bundles".into(),
            from: r.from,
            to: r.to,
            kpis: vec![kpi("Bundles sold", units, "qty", None), kpi("Revenue (ex VAT)", rev, "money", None)],
            columns: vec![
                col("name", "Bundle", "text"),
                col("units", "Sold", "qty"),
                col("revenue", "Revenue (ex VAT)", "money"),
                col("cost", "Cost of items", "money"),
                col("gross_margin", "Gross margin", "money"),
                col("items_used", "Items taken from stock", "text"),
            ],
            totals: None,
            rows: out,
            series: None,
            notes: vec!["Refunds are not deducted here.".into()],
        })
    }

    fn rep_refunds(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare(
            "SELECT rf.refund_receipt_number, s.receipt_number, rf.created_at, u.display_name, a.display_name, rf.reason, rf.total_minor, rf.tax_minor, rf.kind
             FROM refunds rf JOIN sales s ON s.sale_id=rf.original_sale_id LEFT JOIN users u ON u.user_id=rf.user_id LEFT JOIN users a ON a.user_id=rf.approved_by
             WHERE rf.created_at>=?1 AND rf.created_at<?2 AND (amw_rbranch() IS NULL OR rf.branch_id=amw_rbranch()) ORDER BY rf.created_at DESC LIMIT 20000",
        )?;
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |row| {
                Ok(json!({ "refund": row.get::<_, String>(0)?, "receipt": row.get::<_, String>(1)?, "at": row.get::<_, String>(2)?, "user": row.get::<_, Option<String>>(3)?,
                    "approver": row.get::<_, Option<String>>(4)?, "reason": row.get::<_, String>(5)?, "total": row.get::<_, i64>(6)?, "tax": row.get::<_, i64>(7)?,
                    "kind": if row.get::<_, String>(8)? == "void" { "voided_sale" } else { "refund" } }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let total: i64 = rows.iter().map(|r| r["total"].as_i64().unwrap_or(0)).sum();
        // Voids (whole sale cancelled the same day) are counted apart from refunds.
        let voids: Vec<&Value> = rows.iter().filter(|r| r["kind"] == "voided_sale").collect();
        let void_total: i64 = voids.iter().map(|r| r["total"].as_i64().unwrap_or(0)).sum();
        let mut reasons: std::collections::BTreeMap<String, i64> = Default::default();
        for row in &rows {
            *reasons.entry(row["reason"].as_str().unwrap_or("").to_string()).or_default() += row["total"].as_i64().unwrap_or(0);
        }
        Ok(Report {
            key: "refunds".into(),
            title: "Refunds".into(),
            from: r.from,
            to: r.to,
            kpis: vec![
                kpi("Refunds", (rows.len() - voids.len()) as i64, "int", None),
                kpi("Refunded", total - void_total, "money", None),
                kpi("Voided sales", voids.len() as i64, "int", None),
                kpi("Voided", void_total, "money", None),
            ],
            columns: vec![
                col("refund", "Refund", "text"),
                col("kind", "Type", "status"),
                col("receipt", "Original receipt", "text"),
                col("at", "Time", "datetime"),
                col("user", "By", "text"),
                col("approver", "Approved by", "text"),
                col("reason", "Reason", "text"),
                col("tax", "VAT", "money"),
                col("total", "Amount", "money"),
            ],
            totals: Some(json!({ "refund": "Total", "total": total })),
            series: Some(reasons.into_iter().map(|(k, v)| json!({ "label": k, "value": v })).collect()),
            rows,
            notes: vec![],
        })
    }

    fn rep_cash(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare("SELECT shift_id FROM shifts WHERE opened_at>=?1 AND opened_at<?2 AND (amw_rbranch() IS NULL OR branch_id=amw_rbranch()) ORDER BY opened_at")?;
        let ids = st.query_map(params![r.a, r.b], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        let mut rows = vec![];
        let mut tv = 0;
        for id in ids {
            let sm = crate::shifts::shift_summary(c, &id)?;
            tv += sm.variance_minor.unwrap_or(0);
            rows.push(json!({ "shift": sm.shift_number, "cashier": sm.cashier_name, "terminal": sm.device_name, "opened": sm.opened_at, "closed": sm.closed_at,
                "status": sm.status, "float": sm.opening_float_minor, "cash_sales": sm.cash_sales_minor, "cash_refunds": sm.cash_refunds_minor,
                "paid_in": sm.paid_in_minor, "paid_out": sm.paid_out_minor, "safe_drops": sm.safe_drop_minor, "expected": sm.expected_cash_minor,
                "counted": sm.counted_cash_minor, "variance": sm.variance_minor, "no_sales": sm.no_sale_count }));
        }
        let discrepancies = rows.iter().filter(|r| r["variance"].as_i64().map(|v| v != 0).unwrap_or(false)).count() as i64;
        Ok(Report {
            key: "cash".into(),
            title: "Cash & shifts".into(),
            from: r.from,
            to: r.to,
            kpis: vec![
                kpi("Shifts", rows.len() as i64, "int", None),
                kpi("Shifts with variance", discrepancies, "int", None),
                kpi("Net variance", tv, "money", None),
            ],
            columns: vec![
                col("shift", "Shift", "text"),
                col("cashier", "Cashier", "text"),
                col("terminal", "Terminal", "text"),
                col("opened", "Opened", "datetime"),
                col("closed", "Closed", "datetime"),
                col("float", "Float", "money"),
                col("cash_sales", "Cash sales", "money"),
                col("cash_refunds", "Cash refunds", "money"),
                col("paid_in", "Paid in", "money"),
                col("paid_out", "Paid out", "money"),
                col("safe_drops", "Safe drops", "money"),
                col("expected", "Expected", "money"),
                col("counted", "Counted", "money"),
                col("variance", "Variance", "money"),
                col("no_sales", "No-sales", "int"),
            ],
            totals: Some(json!({ "shift": "Total", "variance": tv })),
            rows,
            series: None,
            notes: vec![],
        })
    }

    fn rep_inventory(&self, c: &Connection, s: &Session, p: &ReportParams) -> AppResult<Report> {
        let show_cost = s.has("products.view_cost");
        let cat = p.category_id.as_deref().filter(|x| !x.is_empty());
        let mut st = c.prepare(
            "SELECT p.name, p.sku, COALESCE(c.name,''), COALESCE(sl.qty_milli,0), p.reorder_point_milli, COALESCE(pc.avg_cost_minor,0), sl.last_movement_at
             FROM products p LEFT JOIN stock_levels sl ON sl.product_id=p.product_id AND sl.branch_id=?1
             LEFT JOIN product_costs pc ON pc.product_id=p.product_id AND pc.branch_id=?1 LEFT JOIN categories c ON c.category_id=p.category_id
             WHERE p.track_inventory=1 AND p.active=1 AND (?2 IS NULL OR p.category_id=?2) ORDER BY p.name COLLATE NOCASE",
        )?;
        let mut value = 0i64;
        let mut low = 0;
        let mut out = 0;
        let mut neg = 0;
        let rows: Vec<Value> = st
            .query_map(params![s.branch_id, cat], |row| {
                let q: i64 = row.get(3)?;
                let ro: i64 = row.get(4)?;
                let cost: i64 = row.get(5)?;
                let v = crate::money::extend(cost, q.max(0)).unwrap_or(0);
                Ok(json!({ "name": row.get::<_, String>(0)?, "sku": row.get::<_, String>(1)?, "category": row.get::<_, String>(2)?, "qty": q, "reorder": ro,
                    "avg_cost": if show_cost { Some(cost) } else { None }, "value": if show_cost { Some(v) } else { None },
                    "status": crate::catalog::stock_status(true, q, ro), "last_movement": row.get::<_, Option<String>>(6)? }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for r in &rows {
            value += r["value"].as_i64().unwrap_or(0);
            match r["status"].as_str() {
                Some("low_stock") => low += 1,
                Some("out_of_stock") => out += 1,
                Some("negative") => neg += 1,
                _ => {}
            }
        }
        let mut kpis = vec![
            kpi("Tracked SKUs", rows.len() as i64, "int", None),
            kpi("Low stock", low, "int", None),
            kpi("Out of stock", out, "int", None),
            kpi("Negative", neg, "int", None),
        ];
        let mut columns = vec![
            col("name", "Product", "text"),
            col("sku", "SKU", "text"),
            col("category", "Category", "text"),
            col("qty", "On hand", "qty"),
            col("reorder", "Reorder point", "qty"),
            col("status", "Status", "text"),
            col("last_movement", "Last movement", "datetime"),
        ];
        if show_cost {
            kpis.push(kpi("Stock value (avg cost)", value, "money", None));
            columns.extend([col("avg_cost", "Avg cost", "money"), col("value", "Value", "money")]);
        }
        let today = time::business_date(time::now(), &time::day(c)?)?;
        Ok(Report {
            key: "inventory".into(),
            title: "Stock valuation".into(),
            from: today.clone(),
            to: today,
            kpis,
            columns,
            totals: Some(json!({ "name": "Total", "value": if show_cost { Some(value) } else { None } })),
            rows,
            series: None,
            notes: vec!["Negative quantities are excluded from valuation.".into()],
        })
    }

    fn rep_dead_stock(&self, c: &Connection, s: &Session, p: &ReportParams) -> AppResult<Report> {
        let days = p.days.unwrap_or(60).clamp(7, 3650);
        let since = time::fmt(time::now() - chrono::Duration::days(days));
        let show_cost = s.has("products.view_cost");
        let mut st = c.prepare(
            "SELECT p.name, p.sku, COALESCE(sl.qty_milli,0), COALESCE(pc.avg_cost_minor,0),
                    (SELECT MAX(s.completed_at) FROM sale_items i JOIN sales s ON s.sale_id=i.sale_id WHERE i.product_id=p.product_id)
             FROM products p JOIN stock_levels sl ON sl.product_id=p.product_id AND sl.branch_id=?1
             LEFT JOIN product_costs pc ON pc.product_id=p.product_id AND pc.branch_id=?1
             WHERE p.active=1 AND p.track_inventory=1 AND sl.qty_milli > 0
               AND NOT EXISTS (SELECT 1 FROM sale_items i JOIN sales s ON s.sale_id=i.sale_id WHERE i.product_id=p.product_id AND s.completed_at>=?2)
             ORDER BY sl.qty_milli * COALESCE(pc.avg_cost_minor,0) DESC LIMIT 5000",
        )?;
        let rows: Vec<Value> = st
            .query_map(params![s.branch_id, since], |row| {
                let q: i64 = row.get(2)?;
                let cost: i64 = row.get(3)?;
                Ok(json!({ "name": row.get::<_, String>(0)?, "sku": row.get::<_, String>(1)?, "qty": q,
                    "value": if show_cost { Some(crate::money::extend(cost, q).unwrap_or(0)) } else { None }, "last_sold": row.get::<_, Option<String>>(4)? }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let value: i64 = rows.iter().map(|r| r["value"].as_i64().unwrap_or(0)).sum();
        let today = time::business_date(time::now(), &time::day(c)?)?;
        let mut columns = vec![
            col("name", "Product", "text"),
            col("sku", "SKU", "text"),
            col("qty", "On hand", "qty"),
            col("last_sold", "Last sold", "datetime"),
        ];
        let mut kpis = vec![kpi("Dead-stock items", rows.len() as i64, "int", None)];
        if show_cost {
            columns.push(col("value", "Value", "money"));
            kpis.push(kpi("Value tied up", value, "money", None));
        }
        Ok(Report {
            key: "dead_stock".into(),
            title: format!("Dead stock (no sales in {days} days)"),
            from: today.clone(),
            to: today,
            kpis,
            columns,
            totals: None,
            rows,
            series: None,
            notes: vec![
                "Evidence: on-hand stock with zero sales in the window. Suggested action: promote, return to supplier or stop reordering."
                    .into(),
            ],
        })
    }

    fn rep_movements(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare(
            "SELECT type, COUNT(*), SUM(CASE WHEN qty_delta_milli>0 THEN qty_delta_milli ELSE 0 END), SUM(CASE WHEN qty_delta_milli<0 THEN -qty_delta_milli ELSE 0 END),
                    SUM(qty_delta_milli * COALESCE(unit_cost_minor,0) / 1000)
             FROM stock_movements WHERE created_at>=?1 AND created_at<?2 AND (amw_rbranch() IS NULL OR branch_id=amw_rbranch()) GROUP BY type ORDER BY type",
        )?;
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |row| {
                Ok(json!({ "type": row.get::<_, String>(0)?, "movements": row.get::<_, i64>(1)?, "in": row.get::<_, i64>(2)?, "out": row.get::<_, i64>(3)?, "value": row.get::<_, i64>(4)? }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Report {
            key: "stock_movements".into(),
            title: "Stock movements".into(),
            from: r.from,
            to: r.to,
            kpis: vec![kpi("Movements", rows.iter().map(|r| r["movements"].as_i64().unwrap_or(0)).sum(), "int", None)],
            columns: vec![
                col("type", "Type", "text"),
                col("movements", "Movements", "int"),
                col("in", "Qty in", "qty"),
                col("out", "Qty out", "qty"),
                col("value", "Net value at cost", "money"),
            ],
            totals: None,
            rows,
            series: None,
            notes: vec![],
        })
    }

    fn rep_purchasing(&self, c: &Connection, s: &Session, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let show_cost = s.has("products.view_cost");
        let mut st = c.prepare(
            "SELECT COALESCE(sp.name, 'No supplier'), COUNT(*), SUM(g.total_cost_minor), MAX(g.created_at)
             FROM goods_receipts g LEFT JOIN suppliers sp ON sp.supplier_id=g.supplier_id
             WHERE g.created_at>=?1 AND g.created_at<?2 AND (amw_rbranch() IS NULL OR g.branch_id=amw_rbranch()) GROUP BY 1 ORDER BY 3 DESC",
        )?;
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |row| {
                Ok(json!({ "supplier": row.get::<_, String>(0)?, "receipts": row.get::<_, i64>(1)?,
                    "cost": if show_cost { Some(row.get::<_, i64>(2)?) } else { None }, "last": row.get::<_, String>(3)? }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let total: i64 = rows.iter().map(|r| r["cost"].as_i64().unwrap_or(0)).sum();
        let open: i64 =
            c.query_row("SELECT COUNT(*) FROM purchase_orders WHERE status IN ('ordered','partially_received')", [], |r| r.get(0))?;
        let mut columns = vec![
            col("supplier", "Supplier", "text"),
            col("receipts", "Deliveries received", "int"),
            col("last", "Last received", "datetime"),
        ];
        let mut kpis = vec![kpi("Open purchase orders", open, "int", None)];
        if show_cost {
            columns.push(col("cost", "Cost received", "money"));
            kpis.push(kpi("Received at cost", total, "money", None));
        }
        Ok(Report {
            key: "purchasing".into(),
            title: "Purchasing".into(),
            from: r.from,
            to: r.to,
            kpis,
            columns,
            totals: None,
            rows,
            series: None,
            notes: vec![],
        })
    }

    /// Purchase orders placed in the period: quantity received against
    /// ordered (fill rate) and first delivery against the expected date.
    fn rep_supplier_performance(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare(
            "WITH po AS (
                SELECT p.supplier_id, p.expected_at, COALESCE(p.ordered_at, p.created_at) AS ordered_at,
                       (SELECT COALESCE(SUM(qty_ordered_milli),0) FROM purchase_order_items i WHERE i.po_id=p.po_id) AS ordered,
                       (SELECT COALESCE(SUM(qty_received_milli),0) FROM purchase_order_items i WHERE i.po_id=p.po_id) AS received,
                       (SELECT MIN(created_at) FROM goods_receipts g WHERE g.po_id=p.po_id) AS first_rcv
                FROM purchase_orders p
                WHERE p.status NOT IN ('draft','cancelled') AND COALESCE(p.ordered_at, p.created_at)>=?1 AND COALESCE(p.ordered_at, p.created_at)<?2
                  AND (amw_rbranch() IS NULL OR p.branch_id=amw_rbranch()))
             SELECT sp.name, COUNT(*), SUM(po.ordered), SUM(po.received),
                    SUM(CASE WHEN po.expected_at IS NOT NULL AND po.first_rcv IS NOT NULL AND substr(po.first_rcv,1,10) <= substr(po.expected_at,1,10) THEN 1 ELSE 0 END),
                    SUM(CASE WHEN po.expected_at IS NOT NULL AND ((po.first_rcv IS NULL AND substr(po.expected_at,1,10) < substr(amw_now(),1,10))
                                  OR substr(po.first_rcv,1,10) > substr(po.expected_at,1,10)) THEN 1 ELSE 0 END),
                    SUM(CASE WHEN po.expected_at IS NULL THEN 1 ELSE 0 END),
                    po.supplier_id,
                    CAST(ROUND(AVG(CASE WHEN po.first_rcv IS NOT NULL THEN julianday(substr(po.first_rcv,1,10)) - julianday(substr(po.ordered_at,1,10)) END) * 10) AS INTEGER)
             FROM po JOIN suppliers sp ON sp.supplier_id=po.supplier_id GROUP BY po.supplier_id ORDER BY sp.name COLLATE NOCASE",
        )?;
        // Per supplier, over the same period: what was delivered and what
        // differed, returns, cost differences and invoices that needed review.
        let by_supplier = |sql: &str| -> AppResult<std::collections::HashMap<String, i64>> {
            let mut q = c.prepare(sql)?;
            let rows = q.query_map(params![r.a, r.b], |x| Ok((x.get::<_, String>(0)?, x.get::<_, Option<i64>>(1)?.unwrap_or(0))))?;
            let mut m = std::collections::HashMap::new();
            for row in rows {
                let (k, v) = row?;
                m.insert(k, v);
            }
            Ok(m)
        };
        let accepted = by_supplier(
            "SELECT g.supplier_id, SUM(i.qty_milli) FROM goods_receipt_items i JOIN goods_receipts g ON g.receipt_id=i.receipt_id
             WHERE g.created_at>=?1 AND g.created_at<?2 AND g.supplier_id IS NOT NULL AND (amw_rbranch() IS NULL OR g.branch_id=amw_rbranch()) GROUP BY g.supplier_id",
        )?;
        let disc = |kind: &str| {
            by_supplier(&format!(
                "SELECT d.supplier_id, SUM(d.qty_milli) FROM receipt_discrepancies d JOIN goods_receipts g ON g.receipt_id=d.receipt_id
                 WHERE d.kind='{kind}' AND d.created_at>=?1 AND d.created_at<?2 AND (amw_rbranch() IS NULL OR g.branch_id=amw_rbranch()) GROUP BY d.supplier_id"
            ))
        };
        let (shortage, overage, damaged, rejected) = (disc("shortage")?, disc("overage")?, disc("damaged")?, disc("rejected")?);
        let returned = by_supplier(
            "SELECT r.supplier_id, SUM(l.qty_milli) FROM supplier_return_lines l JOIN supplier_returns r ON r.return_id=l.return_id
             WHERE r.status IN ('confirmed','credited') AND r.confirmed_at>=?1 AND r.confirmed_at<?2 AND (amw_rbranch() IS NULL OR r.branch_id=amw_rbranch()) GROUP BY r.supplier_id",
        )?;
        let cost_var = by_supplier(
            "SELECT g.supplier_id, CAST(AVG(ABS(i.unit_cost_minor - o.unit_cost_minor) * 10000.0 / o.unit_cost_minor) AS INTEGER)
             FROM goods_receipt_items i JOIN goods_receipts g ON g.receipt_id=i.receipt_id JOIN purchase_order_items o ON o.po_item_id=i.po_item_id
             WHERE o.unit_cost_minor > 0 AND i.substitute_for_product_id IS NULL AND g.created_at>=?1 AND g.created_at<?2
               AND (amw_rbranch() IS NULL OR g.branch_id=amw_rbranch()) GROUP BY g.supplier_id",
        )?;
        let reviewed = by_supplier(
            "SELECT supplier_id, COUNT(*) FROM supplier_invoices WHERE match_accepted_at>=?1 AND match_accepted_at<?2 GROUP BY supplier_id",
        )?;
        let configured = {
            let mut q = c.prepare(
                "SELECT supplier_id, CAST(ROUND(AVG(lead_time_days) * 10) AS INTEGER) FROM supplier_products WHERE lead_time_days IS NOT NULL AND active=1 GROUP BY supplier_id",
            )?;
            let rows = q.query_map([], |x| Ok((x.get::<_, String>(0)?, x.get::<_, i64>(1)?)))?;
            let mut m = std::collections::HashMap::new();
            for row in rows {
                let (k, v) = row?;
                m.insert(k, v);
            }
            m
        };
        let rate = |part: i64, whole: i64| if whole > 0 { part * 10_000 / whole } else { 0 };
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |row| {
                let ordered: i64 = row.get(2)?;
                let received: i64 = row.get(3)?;
                let on_time: i64 = row.get(4)?;
                let late: i64 = row.get(5)?;
                let sid: String = row.get(7)?;
                let observed: Option<i64> = row.get(8)?;
                let g = |m: &std::collections::HashMap<String, i64>| m.get(&sid).copied().unwrap_or(0);
                let acc = g(&accepted);
                let delivered = acc + g(&rejected);
                Ok(json!({ "supplier": row.get::<_, String>(0)?, "orders": row.get::<_, i64>(1)?, "ordered": ordered, "received": received,
                    "fill_bp": if ordered > 0 { received.min(ordered) * 10_000 / ordered } else { 0 },
                    "on_time": on_time, "late": late, "no_date": row.get::<_, i64>(6)?,
                    "on_time_bp": if on_time + late > 0 { on_time * 10_000 / (on_time + late) } else { 0 },
                    "lead_configured": configured.get(&sid).map(|x| format!("{}.{}", x / 10, x % 10)),
                    "lead_observed": observed.map(|x| format!("{}.{}", x / 10, x % 10)),
                    "shortage_bp": rate(g(&shortage), ordered), "overage_bp": rate(g(&overage), ordered),
                    "damage_bp": rate(g(&damaged), delivered), "rejected_bp": rate(g(&rejected), delivered),
                    "return_bp": rate(g(&returned), acc), "cost_variance_bp": g(&cost_var), "invoice_reviews": g(&reviewed) }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let (o, rc, ot, lt) = rows.iter().fold((0, 0, 0, 0), |a, x| {
            (
                a.0 + x["ordered"].as_i64().unwrap_or(0),
                a.1 + x["received"].as_i64().unwrap_or(0).min(x["ordered"].as_i64().unwrap_or(0)),
                a.2 + x["on_time"].as_i64().unwrap_or(0),
                a.3 + x["late"].as_i64().unwrap_or(0),
            )
        });
        Ok(Report {
            key: "supplier_performance".into(),
            title: "Supplier performance".into(),
            from: r.from,
            to: r.to,
            kpis: vec![
                kpi("Fill rate", if o > 0 { rc * 10_000 / o } else { 0 }, "percent_bp", None),
                kpi("On time", if ot + lt > 0 { ot * 10_000 / (ot + lt) } else { 0 }, "percent_bp", None),
                kpi("Late orders", lt, "int", None),
            ],
            columns: vec![
                col("supplier", "Supplier", "text"),
                col("orders", "Orders", "int"),
                col("ordered", "Qty ordered", "qty"),
                col("received", "Qty received", "qty"),
                col("fill_bp", "Fill rate", "percent_bp"),
                col("on_time", "On time", "int"),
                col("late", "Late", "int"),
                col("no_date", "No expected date", "int"),
                col("on_time_bp", "On-time rate", "percent_bp"),
                col("lead_configured", "Lead time set (days)", "text"),
                col("lead_observed", "Lead time seen (days)", "text"),
                col("shortage_bp", "Short", "percent_bp"),
                col("overage_bp", "Extra", "percent_bp"),
                col("damage_bp", "Damaged", "percent_bp"),
                col("rejected_bp", "Refused", "percent_bp"),
                col("return_bp", "Returned", "percent_bp"),
                col("cost_variance_bp", "Cost difference (avg)", "percent_bp"),
                col("invoice_reviews", "Invoices reviewed", "int"),
            ],
            totals: None,
            rows,
            series: None,
            notes: vec![
                "On time: first delivery on or before the expected date. Late includes orders past their date with nothing received."
                    .into(),
            ],
        })
    }

    fn rep_deliveries(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare(
            "SELECT status, COUNT(*), SUM(amount_minor), AVG(CASE WHEN delivered_at IS NOT NULL THEN (julianday(delivered_at)-julianday(created_at))*1440 END)
             FROM delivery_orders WHERE created_at>=?1 AND created_at<?2 GROUP BY status",
        )?;
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |row| {
                Ok(json!({ "status": row.get::<_, String>(0)?, "count": row.get::<_, i64>(1)?, "amount": row.get::<_, i64>(2)?,
                    "avg_minutes": row.get::<_, Option<f64>>(3)?.map(|m| m.round() as i64) }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Report {
            key: "deliveries".into(),
            title: "Deliveries".into(),
            from: r.from,
            to: r.to,
            kpis: vec![kpi("Deliveries", rows.iter().map(|r| r["count"].as_i64().unwrap_or(0)).sum(), "int", None)],
            columns: vec![
                col("status", "Status", "text"),
                col("count", "Deliveries", "int"),
                col("amount", "Amount", "money"),
                col("avg_minutes", "Avg minutes to deliver", "int"),
            ],
            totals: None,
            rows,
            series: None,
            notes: vec![],
        })
    }

    fn rep_audit(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare(
            "SELECT a.event_type, COALESCE(u.display_name,'System'), COUNT(*), SUM(a.approved_by IS NOT NULL)
             FROM audit_logs a LEFT JOIN users u ON u.user_id=a.user_id WHERE a.created_at>=?1 AND a.created_at<?2 GROUP BY 1,2 ORDER BY 3 DESC",
        )?;
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |row| {
                Ok(json!({ "event": row.get::<_, String>(0)?, "user": row.get::<_, String>(1)?, "count": row.get::<_, i64>(2)?, "approved": row.get::<_, i64>(3)? }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Report {
            key: "audit".into(),
            title: "Audit summary".into(),
            from: r.from,
            to: r.to,
            kpis: vec![kpi("Events", rows.iter().map(|r| r["count"].as_i64().unwrap_or(0)).sum(), "int", None)],
            columns: vec![
                col("event", "Event", "text"),
                col("user", "User", "text"),
                col("count", "Count", "int"),
                col("approved", "With approval", "int"),
            ],
            totals: None,
            rows,
            series: None,
            notes: vec![],
        })
    }

    /// Owner/manager dashboard: today's performance and exceptions.
    pub fn dashboard(&self, token: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("reports.sales") && !s.has("admin.access") {
            return Err(AppError::forbidden("reports.sales"));
        }
        let scope = self.db.read(|c| crate::branches::list_scope(c, &s))?;
        crate::db::with_report_branch(scope, || self.dashboard_for(&s))
    }

    pub(crate) fn dashboard_for(&self, s: &Session) -> AppResult<Value> {
        let fin = s.has("reports.financial");
        self.db.read(|c| {
            let day = time::day(c)?;
            let today = time::business_date(time::now(), &day)?;
            let (a, b) = time::local_date_range_utc(&today, &today, &day)?;
            let last_week = (chrono::NaiveDate::parse_from_str(&today, "%Y-%m-%d").unwrap() - chrono::Duration::days(7)).to_string();
            let (pa, pb) = time::local_date_range_utc(&last_week, &last_week, &day)?;
            let (n, total, tax, cost, _d, _i) = sales_totals(c, &a, &b, None)?;
            let (pn, ptotal, ptax, pcost, _, _) = sales_totals(c, &pa, &pb, None)?;
            let (rn, rtotal, rtax, rcost) = refund_totals(c, &a, &b)?;
            let local = local_expr("completed_at", &day.tz);
            let mut st = c.prepare(&format!(
                "SELECT CAST(strftime('%H', {local}) AS INTEGER), COUNT(*), SUM(total_minor) FROM sales WHERE completed_at>=?1 AND completed_at<?2 AND (amw_rbranch() IS NULL OR branch_id=amw_rbranch()) GROUP BY 1"
            ))?;
            let mut hourly = vec![json!(null); 24];
            for row in st.query_map(params![a, b], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))? {
                let (h, cnt, t) = row?;
                hourly[h.clamp(0, 23) as usize] = json!({ "transactions": cnt, "sales": t });
            }
            let hourly: Vec<Value> = hourly
                .into_iter()
                .enumerate()
                .map(|(h, v)| json!({ "hour": h, "transactions": v.get("transactions").cloned().unwrap_or(json!(0)), "sales": v.get("sales").cloned().unwrap_or(json!(0)) }))
                .collect();
            let mut st = c.prepare(
                "SELECT i.product_name_snapshot, SUM(i.qty_milli), SUM(i.line_total_minor) FROM sale_items i JOIN sales s ON s.sale_id=i.sale_id
                 WHERE s.completed_at>=?1 AND s.completed_at<?2 AND (amw_rbranch() IS NULL OR s.branch_id=amw_rbranch()) GROUP BY COALESCE(i.product_id, i.product_name_snapshot) ORDER BY 3 DESC LIMIT 8",
            )?;
            let top: Vec<Value> = st
                .query_map(params![a, b], |r| Ok(json!({ "name": r.get::<_, String>(0)?, "qty": r.get::<_, i64>(1)?, "total": r.get::<_, i64>(2)? })))?
                .collect::<Result<Vec<_>, _>>()?;
            let mut st = c.prepare(
                "SELECT p.product_id, p.name, COALESCE(sl.qty_milli,0), p.reorder_point_milli FROM products p
                 LEFT JOIN stock_levels sl ON sl.product_id=p.product_id AND sl.branch_id=?1
                 WHERE p.active=1 AND p.track_inventory=1 AND COALESCE(sl.qty_milli,0) <= p.reorder_point_milli
                 ORDER BY COALESCE(sl.qty_milli,0) - p.reorder_point_milli LIMIT 8",
            )?;
            let low: Vec<Value> = st
                .query_map([&s.branch_id], |r| Ok(json!({ "product_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "qty": r.get::<_, i64>(2)?, "reorder": r.get::<_, i64>(3)? })))?
                .collect::<Result<Vec<_>, _>>()?;
            let low_count: i64 = c.query_row(
                "SELECT COUNT(*) FROM products p LEFT JOIN stock_levels sl ON sl.product_id=p.product_id AND sl.branch_id=?1
                 WHERE p.active=1 AND p.track_inventory=1 AND COALESCE(sl.qty_milli,0) <= p.reorder_point_milli",
                [&s.branch_id],
                |r| r.get(0),
            )?;
            let negative: i64 = c.query_row("SELECT COUNT(*) FROM stock_levels WHERE qty_milli < 0 AND branch_id=?1", [&s.branch_id], |r| r.get(0))?;
            let unknown: i64 = c.query_row("SELECT COUNT(*) FROM unknown_barcodes WHERE status='open'", [], |r| r.get(0))?;
            let open_deliveries: i64 = c.query_row("SELECT COUNT(*) FROM delivery_orders WHERE status IN ('pending','preparing','dispatched')", [], |r| r.get(0))?;
            let active_shifts: i64 = c.query_row("SELECT COUNT(*) FROM shifts WHERE status='open'", [], |r| r.get(0))?;
            let discrepancies: i64 = c.query_row(
                "SELECT COUNT(*) FROM shifts WHERE closed_at>=?1 AND closed_at<?2 AND (amw_rbranch() IS NULL OR branch_id=amw_rbranch()) AND variance_minor<>0",
                params![a, b],
                |r| r.get(0),
            )?;
            let failed_prints: i64 = c.query_row("SELECT COUNT(*) FROM print_jobs WHERE status='failed' AND kind<>'drawer'", [], |r| r.get(0))?;
            let mut attention = vec![];
            if low_count > 0 {
                attention.push(json!({ "kind": "low_stock", "severity": "warning", "count": low_count, "text": format!("{low_count} product(s) at or below reorder point"), "link": "/admin/inventory?stock=attention" }));
            }
            if negative > 0 {
                attention.push(json!({ "kind": "negative_stock", "severity": "error", "count": negative, "text": format!("{negative} product(s) with negative stock"), "link": "/admin/inventory?stock=negative" }));
            }
            if unknown > 0 {
                attention.push(json!({ "kind": "unknown_barcodes", "severity": "warning", "count": unknown, "text": format!("{unknown} unknown barcode(s) scanned"), "link": "/admin/unknown-barcodes" }));
            }
            let _ = failed_prints;
            // Operational problems are cases (the Alert Centre): cash
            // differences, records that could not be saved, tills not seen,
            // backups, print failures. The Dashboard shows the most urgent
            // open ones (bounded, indexed) and never recomputes them.
            if s.has("cases.view") {
                let open_total: i64 = c.query_row(
                    "SELECT COUNT(*) FROM cases WHERE status NOT IN ('resolved','dismissed') AND (amw_rbranch() IS NULL OR branch_id=amw_rbranch())",
                    [],
                    |r| r.get(0),
                )?;
                let mut st = c.prepare(
                    "SELECT case_id, kind, severity, title FROM cases WHERE status NOT IN ('resolved','dismissed')
                       AND (amw_rbranch() IS NULL OR branch_id=amw_rbranch())
                     ORDER BY CASE severity WHEN 'high' THEN 0 WHEN 'medium' THEN 1 ELSE 2 END, created_at DESC LIMIT 5",
                )?;
                let top = st
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                for (id, kind, sev, title) in &top {
                    let severity = match sev.as_str() { "high" => "error", "medium" => "warning", _ => "info" };
                    attention.push(json!({ "kind": "case", "case_kind": kind, "severity": severity, "text": title, "link": format!("/admin/cases?case={id}") }));
                }
                if open_total > top.len() as i64 {
                    let more = open_total - top.len() as i64;
                    attention.push(json!({ "kind": "cases_more", "severity": "info", "count": more, "text": format!("{more} more open case(s) in the Alert Centre"), "link": "/admin/cases" }));
                }
            }
            let backup = self.backup_diagnostic()?;
            let _ = discrepancies;
            // Work waiting for a person in the order and supplier flows, shown
            // only to people who may act on it (and only with the module on).
            let flags = self.features()?;
            let count = |sql: &str| -> AppResult<i64> { Ok(c.query_row(sql, [], |r| r.get(0))?) };
            if flags.is_on("orders.digital") && s.has("orders.manage") {
                let k = count("SELECT COUNT(*) FROM digital_orders WHERE status='draft'")?;
                if k > 0 {
                    attention.push(json!({ "kind": "orders", "severity": "warning", "count": k, "text": format!("{k} order(s) waiting to be confirmed"), "link": "/admin/orders" }));
                }
            }
            if flags.is_on("ocr.payment_screenshots") && (s.has("whatsapp.manage") || s.has("payments.review")) {
                let k = count("SELECT COUNT(*) FROM payment_reviews WHERE status IN ('pending','matched','mismatch','needs_review')")?;
                if k > 0 {
                    attention.push(json!({ "kind": "payments", "severity": "warning", "count": k, "text": format!("{k} payment screenshot(s) to check"), "link": "/admin/payment-reviews" }));
                }
            }
            // Procurement (Wave 4): only what someone can act on, each with
            // its workflow.
            if s.has("purchasing.approve") {
                let k = count("SELECT COUNT(*) FROM requisitions WHERE status='submitted'")?;
                if k > 0 {
                    attention.push(json!({ "kind": "requisitions", "severity": "warning", "count": k, "text": format!("{k} requisition(s) waiting for approval"), "link": "/admin/requisitions?status=submitted" }));
                }
                let mut st = c.prepare("SELECT po_id FROM purchase_orders WHERE status='draft'")?;
                let drafts: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
                let mut waiting = 0;
                for id in drafts {
                    let a = crate::purchasing::approval_status(c, &id)?;
                    if a.required && !a.valid {
                        waiting += 1;
                    }
                }
                if waiting > 0 {
                    attention.push(json!({ "kind": "po_approval", "severity": "warning", "count": waiting, "text": format!("{waiting} purchase order(s) waiting for approval"), "link": "/admin/purchase-orders?status=draft" }));
                }
            }
            if s.has("purchasing.manage") {
                let k = count("SELECT COUNT(*) FROM receipt_discrepancies WHERE kind='shortage' AND resolution='open'")?;
                if k > 0 {
                    attention.push(json!({ "kind": "shortages", "severity": "warning", "count": k, "text": format!("{k} missing delivery quantity(ies) need a decision"), "link": "/admin/purchase-orders?status=partially_received" }));
                }
                let k = count("SELECT COUNT(*) FROM requisitions WHERE status='approved'")?;
                if k > 0 {
                    attention.push(json!({ "kind": "requisitions_approved", "severity": "info", "count": k, "text": format!("{k} approved requisition(s) to turn into purchase orders"), "link": "/admin/requisitions?status=approved" }));
                }
            }
            if s.has("supplier_returns.manage") {
                let k = count("SELECT COUNT(*) FROM supplier_returns WHERE status='confirmed' AND credit_invoice_id IS NULL")?;
                if k > 0 {
                    attention.push(json!({ "kind": "returns_credit", "severity": "info", "count": k, "text": format!("{k} supplier return(s) waiting for a credit note"), "link": "/admin/supplier-returns?status=confirmed" }));
                }
            }
            if s.has("requisitions.create") || s.has("purchasing.manage") {
                let today = time::business_date(time::now(), &time::day(c)?)?;
                let to_order = self.to_order_count(c, &s.branch_id, &today, false)?;
                if to_order > 0 {
                    attention.push(json!({ "kind": "suggested_orders", "severity": "info", "count": to_order, "text": format!("{to_order} product(s) to order"), "link": "/admin/suggested-orders" }));
                }
            }
            if s.has("payables.view") {
                let overdue: i64 = c.query_row(
                    "SELECT COUNT(*) FROM ap_liabilities l WHERE l.status='open' AND l.due_date < ?1
                       AND l.amount_minor > (SELECT COALESCE(SUM(a.amount_minor),0) FROM ap_allocations a WHERE a.liability_id=l.liability_id AND a.status='active')",
                    [&today],
                    |r| r.get(0),
                )?;
                if overdue > 0 {
                    attention.push(json!({ "kind": "payables", "severity": "warning", "count": overdue, "text": format!("{overdue} supplier invoice(s) overdue"), "link": "/admin/payables" }));
                }
            }
            if s.has("payables.post") {
                let k = count("SELECT COUNT(*) FROM supplier_invoices WHERE status='approved' AND posting='not_posted'")?;
                if k > 0 {
                    attention.push(json!({ "kind": "payables_post", "severity": "info", "count": k, "text": format!("{k} supplier invoice(s) ready to post"), "link": "/admin/payables" }));
                }
            }
            Ok(json!({
                "business_date": today,
                "kpis": {
                    "sales": total - rtotal, "sales_prev": ptotal,
                    "transactions": n, "transactions_prev": pn,
                    "average_basket": if n > 0 { total / n } else { 0 }, "average_basket_prev": if pn > 0 { ptotal / pn } else { 0 },
                    "gross_profit": if fin { Some((total - tax - cost) - (rtotal - rtax - rcost)) } else { None },
                    "gross_profit_prev": if fin { Some(ptotal - ptax - pcost) } else { None },
                    "refunds": rtotal, "refund_count": rn,
                },
                "hourly": hourly, "top_products": top, "low_stock": low,
                "counts": { "low_stock": low_count, "negative_stock": negative, "unknown_barcodes": unknown, "open_deliveries": open_deliveries,
                    "active_shifts": active_shifts, "cash_discrepancies": discrepancies },
                "attention": attention,
                "health": { "backup": backup, "sync": self.sync_diagnostic()? },
            }))
        })
    }
}
