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
    ("refunds", "Refunds", "Sales", "Refunds with reasons and approvals"),
    ("cash", "Cash & shifts", "Financial", "Shift reconciliation and cash variances"),
    ("inventory", "Stock valuation", "Inventory", "On-hand quantity and value at average cost"),
    ("dead_stock", "Dead stock", "Inventory", "Stocked items with no sales in the period"),
    ("stock_movements", "Stock movements", "Inventory", "Ledger totals by movement type"),
    ("purchasing", "Purchasing", "Purchasing", "Goods received per supplier"),
    ("deliveries", "Deliveries", "Operations", "Deliveries by status"),
    ("audit", "Audit summary", "Audit", "Audited events by type and user"),
];

fn permission_for(key: &str) -> &'static str {
    match key {
        "margin" | "cash" | "payments" => "reports.financial",
        "tax" => "reports.tax",
        "audit" => "audit.view",
        "inventory" | "dead_stock" | "stock_movements" => "inventory.view",
        "purchasing" => "purchasing.manage",
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
}

fn range(c: &Connection, core: &AppCore, p: &ReportParams) -> AppResult<Range> {
    let tz = core.store_timezone(c)?;
    let today = time::business_date(time::now(), &tz)?;
    let from = p.from.clone().filter(|x| !x.is_empty()).unwrap_or_else(|| today.clone());
    let to = p.to.clone().filter(|x| !x.is_empty()).unwrap_or_else(|| today.clone());
    let (a, b) = time::local_date_range_utc(&from, &to, &tz)?;
    let days = (chrono::NaiveDate::parse_from_str(&to, "%Y-%m-%d").unwrap()
        - chrono::NaiveDate::parse_from_str(&from, "%Y-%m-%d").unwrap())
    .num_days();
    if days > 3660 {
        return Err(AppError::validation("Reports are limited to 10 years per run."));
    }
    Ok(Range { from, to, a, b, tz })
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
    time::local_date_range_utc(&pf.to_string(), &pt.to_string(), &r.tz)
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
            "cash" => self.rep_cash(c, p),
            "inventory" => self.rep_inventory(c, s, p),
            "dead_stock" => self.rep_dead_stock(c, s, p),
            "stock_movements" => self.rep_movements(c, p),
            "purchasing" => self.rep_purchasing(c, s, p),
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

    fn rep_refunds(&self, c: &Connection, p: &ReportParams) -> AppResult<Report> {
        let r = range(c, self, p)?;
        let mut st = c.prepare(
            "SELECT rf.refund_receipt_number, s.receipt_number, rf.created_at, u.display_name, a.display_name, rf.reason, rf.total_minor, rf.tax_minor
             FROM refunds rf JOIN sales s ON s.sale_id=rf.original_sale_id LEFT JOIN users u ON u.user_id=rf.user_id LEFT JOIN users a ON a.user_id=rf.approved_by
             WHERE rf.created_at>=?1 AND rf.created_at<?2 AND (amw_rbranch() IS NULL OR rf.branch_id=amw_rbranch()) ORDER BY rf.created_at DESC LIMIT 20000",
        )?;
        let rows: Vec<Value> = st
            .query_map(params![r.a, r.b], |row| {
                Ok(json!({ "refund": row.get::<_, String>(0)?, "receipt": row.get::<_, String>(1)?, "at": row.get::<_, String>(2)?, "user": row.get::<_, Option<String>>(3)?,
                    "approver": row.get::<_, Option<String>>(4)?, "reason": row.get::<_, String>(5)?, "total": row.get::<_, i64>(6)?, "tax": row.get::<_, i64>(7)? }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let total: i64 = rows.iter().map(|r| r["total"].as_i64().unwrap_or(0)).sum();
        let mut reasons: std::collections::BTreeMap<String, i64> = Default::default();
        for row in &rows {
            *reasons.entry(row["reason"].as_str().unwrap_or("").to_string()).or_default() += row["total"].as_i64().unwrap_or(0);
        }
        Ok(Report {
            key: "refunds".into(),
            title: "Refunds".into(),
            from: r.from,
            to: r.to,
            kpis: vec![kpi("Refunds", rows.len() as i64, "int", None), kpi("Refunded", total, "money", None)],
            columns: vec![
                col("refund", "Refund", "text"),
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
        let today = time::business_date(time::now(), &self.store_timezone(c)?)?;
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
        let today = time::business_date(time::now(), &self.store_timezone(c)?)?;
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
            let tz = self.store_timezone(c)?;
            let today = time::business_date(time::now(), &tz)?;
            let (a, b) = time::local_date_range_utc(&today, &today, &tz)?;
            let last_week = (chrono::NaiveDate::parse_from_str(&today, "%Y-%m-%d").unwrap() - chrono::Duration::days(7)).to_string();
            let (pa, pb) = time::local_date_range_utc(&last_week, &last_week, &tz)?;
            let (n, total, tax, cost, _d, _i) = sales_totals(c, &a, &b, None)?;
            let (pn, ptotal, ptax, pcost, _, _) = sales_totals(c, &pa, &pb, None)?;
            let (rn, rtotal, rtax, rcost) = refund_totals(c, &a, &b)?;
            let local = local_expr("completed_at", &tz);
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
            if discrepancies > 0 {
                attention.push(json!({ "kind": "cash", "severity": "warning", "count": discrepancies, "text": format!("{discrepancies} shift(s) closed with a cash variance today"), "link": "/admin/shifts" }));
            }
            if failed_prints > 0 {
                attention.push(json!({ "kind": "printing", "severity": "warning", "count": failed_prints, "text": format!("{failed_prints} receipt(s) failed to print"), "link": "/admin/diagnostics" }));
            }
            let backup = self.backup_diagnostic()?;
            if backup.state != "ok" {
                attention.push(json!({ "kind": "backup", "severity": "warning", "text": backup.summary, "link": "/admin/backups" }));
            }
            let dead_letters: i64 = c.query_row("SELECT COUNT(*) FROM sync_dead_letters WHERE status='open'", [], |r| r.get(0))?;
            if dead_letters > 0 {
                attention.push(json!({ "kind": "sync", "severity": "error", "count": dead_letters, "text": format!("{dead_letters} change(s) could not be synchronized"), "link": "/admin/sync" }));
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
