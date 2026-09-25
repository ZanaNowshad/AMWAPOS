//! End-of-day pack and saved report date ranges.
//!
//! The pack puts the day's sales, tenders, shift variances, refunds and low
//! stock on one screen and exports the same sections as CSV files in a zip.
//! Every figure comes from the deterministic reports; nothing here is AI.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::ids::new_id;
use crate::reports::{Report, ReportParams};
use crate::service::AppCore;
use crate::setup::clean;
use crate::time;
use crate::validate;

pub const RANGE_KINDS: [&str; 7] = ["today", "yesterday", "last_7", "this_week", "this_month", "last_month", "fixed"];

#[derive(Debug, Clone, Serialize)]
pub struct EodPack {
    pub date: String,
    pub branch_id: Option<String>,
    pub sales: Option<Report>,
    pub tenders: Option<Report>,
    pub shifts: Option<Report>,
    pub refunds: Option<Report>,
    pub low_stock: Vec<Value>,
    /// Sections the user may not see (missing permission).
    pub hidden: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportPreset {
    #[serde(default)]
    pub preset_id: String,
    pub name: String,
    pub range_kind: String,
    #[serde(default)]
    pub from_date: Option<String>,
    #[serde(default)]
    pub to_date: Option<String>,
}

pub(crate) fn low_stock(c: &Connection, branch: &str) -> AppResult<Vec<Value>> {
    let mut st = c.prepare(
        "SELECT p.product_id, p.name, p.sku, COALESCE(sl.qty_milli,0), p.reorder_point_milli
         FROM products p LEFT JOIN stock_levels sl ON sl.product_id=p.product_id AND sl.branch_id=?1
         WHERE p.active=1 AND p.track_inventory=1 AND p.reorder_point_milli>0 AND COALESCE(sl.qty_milli,0) <= p.reorder_point_milli
         ORDER BY COALESCE(sl.qty_milli,0) - p.reorder_point_milli, p.name LIMIT 500",
    )?;
    let rows = st
        .query_map([branch], |r| {
            Ok(json!({ "product_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "sku": r.get::<_, String>(2)?,
                       "qty_milli": r.get::<_, i64>(3)?, "reorder_point_milli": r.get::<_, i64>(4)? }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

impl AppCore {
    pub fn eod_pack(&self, token: &str, date: Option<String>, branch_id: Option<String>) -> AppResult<EodPack> {
        let s = self.session(token)?;
        if !s.has("reports.sales") {
            return Err(AppError::forbidden("reports.sales"));
        }
        let (date, scope) = self.db.read(|c| {
            let tz = self.store_timezone(c)?;
            let d = match date.as_deref().filter(|d| !d.is_empty()) {
                Some(d) => chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
                    .map_err(|_| AppError::validation("Enter the date as YYYY-MM-DD."))?
                    .to_string(),
                None => time::business_date(time::now(), &tz)?,
            };
            Ok((d, crate::branches::report_scope(c, &s, branch_id.as_deref())?))
        })?;
        let p = || ReportParams {
            from: Some(date.clone()),
            to: Some(date.clone()),
            branch_id: scope.clone(),
            group_by: Some("hour".into()),
            ..Default::default()
        };
        let mut hidden = vec![];
        let mut section = |key: &str, perm: &str| -> AppResult<Option<Report>> {
            if s.has(perm) {
                self.report_run(token, key, p()).map(Some)
            } else {
                hidden.push(key.to_string());
                Ok(None)
            }
        };
        let sales = section("sales", "reports.sales")?;
        let tenders = section("payments", "reports.financial")?;
        let shifts = section("cash", "reports.financial")?;
        let refunds = section("refunds", "reports.sales")?;
        let low_stock = if s.has("inventory.view") {
            let b = scope.clone().unwrap_or_else(|| s.branch_id.clone());
            self.db.read(|c| low_stock(c, &b))?
        } else {
            hidden.push("low_stock".into());
            vec![]
        };
        Ok(EodPack { date, branch_id: scope, sales, tenders, shifts, refunds, low_stock, hidden })
    }

    /// The pack as CSV files in a zip (base64 for the UI to save).
    pub fn eod_zip(&self, token: &str, date: Option<String>, branch_id: Option<String>) -> AppResult<Value> {
        let pack = self.eod_pack(token, date, branch_id.clone())?;
        let p = ReportParams {
            from: Some(pack.date.clone()),
            to: Some(pack.date.clone()),
            branch_id: pack.branch_id.clone(),
            group_by: Some("hour".into()),
            ..Default::default()
        };
        let mut files: Vec<(String, String)> = vec![];
        for (key, name, present) in [
            ("sales", "sales.csv", pack.sales.is_some()),
            ("payments", "tenders.csv", pack.tenders.is_some()),
            ("cash", "shifts.csv", pack.shifts.is_some()),
            ("refunds", "refunds.csv", pack.refunds.is_some()),
        ] {
            if present {
                files.push((name.into(), self.report_csv(token, key, p.clone())?));
            }
        }
        if !pack.hidden.iter().any(|h| h == "low_stock") {
            let mut w = csv::WriterBuilder::new().from_writer(vec![]);
            let io = |e: csv::Error| AppError::internal(e.to_string());
            w.write_record(["Product", "SKU", "On hand", "Reorder point"]).map_err(io)?;
            for r in &pack.low_stock {
                w.write_record([
                    validate::csv_safe_cell(r["name"].as_str().unwrap_or("").to_string()),
                    validate::csv_safe_cell(r["sku"].as_str().unwrap_or("").to_string()),
                    crate::money::format_decimal(r["qty_milli"].as_i64().unwrap_or(0), 3),
                    crate::money::format_decimal(r["reorder_point_milli"].as_i64().unwrap_or(0), 3),
                ])
                .map_err(io)?;
            }
            let bytes = w.into_inner().map_err(|e| AppError::internal(e.to_string()))?;
            files.push(("low_stock.csv".into(), String::from_utf8_lossy(&bytes).into_owned()));
        }
        let mut buf = std::io::Cursor::new(vec![]);
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            for (name, body) in &files {
                z.start_file(name.as_str(), o).map_err(|e| AppError::internal(e.to_string()))?;
                std::io::Write::write_all(&mut z, body.as_bytes()).map_err(|e| AppError::internal(e.to_string()))?;
            }
            z.finish().map_err(|e| AppError::internal(e.to_string()))?;
        }
        let file_name = format!("end-of-day-{}.zip", pack.date);
        Ok(
            json!({ "file_name": file_name, "base64": crate::ids::b64(&buf.into_inner()), "files": files.iter().map(|f| &f.0).collect::<Vec<_>>() }),
        )
    }

    pub fn report_presets(&self, token: &str) -> AppResult<Vec<ReportPreset>> {
        let s = self.session(token)?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT preset_id, name, range_kind, from_date, to_date FROM report_presets WHERE user_id=?1 ORDER BY name COLLATE NOCASE",
            )?;
            let rows = st
                .query_map([&s.user_id], |r| {
                    Ok(ReportPreset {
                        preset_id: r.get(0)?,
                        name: r.get(1)?,
                        range_kind: r.get(2)?,
                        from_date: r.get(3)?,
                        to_date: r.get(4)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }

    /// Save a named range for this user on this computer (not synced).
    pub fn report_preset_save(&self, token: &str, preset: ReportPreset) -> AppResult<Vec<ReportPreset>> {
        let s = self.session(token)?;
        let name = clean(&preset.name, "Name", 60, true)?;
        if !RANGE_KINDS.contains(&preset.range_kind.as_str()) {
            return Err(AppError::validation("Unknown date range."));
        }
        let (from, to) = if preset.range_kind == "fixed" {
            let f = preset.from_date.clone().unwrap_or_default();
            let t = preset.to_date.clone().unwrap_or_default();
            for d in [&f, &t] {
                chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|_| AppError::validation("Enter dates as YYYY-MM-DD."))?;
            }
            if f > t {
                return Err(AppError::validation("The start date is after the end date."));
            }
            (Some(f), Some(t))
        } else {
            (None, None)
        };
        self.db.write(|tx| {
            let n: i64 = tx.query_row("SELECT COUNT(*) FROM report_presets WHERE user_id=?1", [&s.user_id], |r| r.get(0))?;
            if n >= 30 {
                return Err(AppError::validation("You can keep up to 30 saved ranges."));
            }
            tx.execute("DELETE FROM report_presets WHERE user_id=?1 AND name=?2", params![s.user_id, name])?;
            tx.execute(
                "INSERT INTO report_presets(preset_id, user_id, name, range_kind, from_date, to_date, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![new_id(), s.user_id, name, preset.range_kind, from, to, time::now_str()],
            )?;
            Ok(())
        })?;
        self.report_presets(token)
    }

    pub fn report_preset_delete(&self, token: &str, preset_id: &str) -> AppResult<Vec<ReportPreset>> {
        let s = self.session(token)?;
        let id = validate::id(preset_id, "Preset")?;
        self.db.write(|tx| {
            tx.execute("DELETE FROM report_presets WHERE preset_id=?1 AND user_id=?2", params![id, s.user_id])?;
            Ok(())
        })?;
        self.report_presets(token)
    }
}
