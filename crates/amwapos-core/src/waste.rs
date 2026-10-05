//! Waste, the expiry view and days of stock left.
//!
//! Waste is stock that left without being sold, with its reason and cost at
//! the time. Recording it writes one stock movement (type `waste`); a
//! mistake is reversed by a compensating movement and the record stays.
//! Expiry is a condition, not an action: expired stock stays on hand until
//! a person records what happened to it.
//!
//! Days of stock left = stock available ÷ average daily demand, where demand
//! is units sold less units refunded or voided over completed days — the
//! same definition as the sales reports. When that cannot be said honestly
//! the answer says why (no stock, no demand, not enough history).

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::{new_id, next_seq};
use crate::inventory::{apply_movement_lot, avg_cost, current_qty, Movement};
use crate::lots::{self, expiry_status, replay};
use crate::money;
use crate::service::AppCore;
use crate::settings::{self, InventorySettings};
use crate::time;
use crate::validate;

pub const REASONS: [&str; 8] = ["expired", "damaged", "spoiled", "broken", "shrinkage", "internal_use", "receiving_rejection", "other"];

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WasteRequest {
    pub product_id: String,
    #[serde(default)]
    pub lot_id: Option<String>,
    pub qty_milli: i64,
    pub reason: String,
    #[serde(default)]
    pub note: Option<String>,
    pub operation_id: String,
    #[serde(default, skip_serializing)]
    pub approval_token: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WasteRow {
    pub waste_id: String,
    pub waste_number: String,
    pub product_id: String,
    pub product_name: String,
    pub lot_id: Option<String>,
    pub lot_number: Option<String>,
    pub qty_milli: i64,
    pub unit_cost_minor: Option<i64>,
    pub cost_minor: Option<i64>,
    pub reason: String,
    pub business_date: String,
    pub note: Option<String>,
    pub user_name: Option<String>,
    pub approved_by_name: Option<String>,
    pub status: String,
    pub reversed_by_name: Option<String>,
    pub reversed_at: Option<String>,
    pub reversal_reason: Option<String>,
    pub created_at: String,
}

const ROW_SQL: &str = "SELECT w.waste_id, w.waste_number, w.product_id, p.name, w.lot_id, l.lot_number, w.qty_milli, w.unit_cost_minor, w.cost_minor,
        w.reason, w.business_date, w.note, u.display_name, a.display_name, w.status, r.display_name, w.reversed_at, w.reversal_reason, w.created_at
     FROM waste_records w JOIN products p ON p.product_id=w.product_id LEFT JOIN stock_lots l ON l.lot_id=w.lot_id
     LEFT JOIN users u ON u.user_id=w.user_id LEFT JOIN users a ON a.user_id=w.approved_by LEFT JOIN users r ON r.user_id=w.reversed_by";

fn row(show_cost: bool) -> impl Fn(&rusqlite::Row) -> rusqlite::Result<WasteRow> {
    move |r| {
        Ok(WasteRow {
            waste_id: r.get(0)?,
            waste_number: r.get(1)?,
            product_id: r.get(2)?,
            product_name: r.get(3)?,
            lot_id: r.get(4)?,
            lot_number: r.get(5)?,
            qty_milli: r.get(6)?,
            unit_cost_minor: if show_cost { r.get(7)? } else { None },
            cost_minor: if show_cost { r.get(8)? } else { None },
            reason: r.get(9)?,
            business_date: r.get(10)?,
            note: r.get(11)?,
            user_name: r.get(12)?,
            approved_by_name: r.get(13)?,
            status: r.get(14)?,
            reversed_by_name: r.get(15)?,
            reversed_at: r.get(16)?,
            reversal_reason: r.get(17)?,
            created_at: r.get(18)?,
        })
    }
}

/// The unit cost a waste is valued at: the batch's own cost when the batch
/// is known, else the current average cost.
fn unit_cost(c: &Connection, product_id: &str, branch_id: &str, lot_id: Option<&str>) -> AppResult<i64> {
    match lot_id {
        Some(l) => Ok(lots::lot_facts(c, l)?.unit_cost_minor),
        None => avg_cost(c, product_id, branch_id),
    }
}

/// Plain words for a reason (English; the screen translates).
pub fn reason_words(r: &str) -> &'static str {
    match r {
        "expired" => "Expired",
        "damaged" => "Damaged",
        "spoiled" => "Spoiled",
        "broken" => "Broken",
        "shrinkage" => "Shrinkage / unexplained difference",
        "internal_use" => "Used in the shop",
        "receiving_rejection" => "Rejected at receiving",
        _ => "Other",
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExpiryFilter {
    #[serde(default)]
    pub category_id: Option<String>,
    #[serde(default)]
    pub supplier_id: Option<String>,
    #[serde(default)]
    pub product_id: Option<String>,
    #[serde(default)]
    pub location_id: Option<String>,
    #[serde(default)]
    pub branch_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CoverFilter {
    #[serde(default)]
    pub window_days: Option<i64>,
    #[serde(default)]
    pub category_id: Option<String>,
    #[serde(default)]
    pub product_id: Option<String>,
    #[serde(default)]
    pub branch_id: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

impl AppCore {
    /// Record waste. One stock movement, exactly once per operation id.
    /// Above the policy (value, or shrinkage) a manager approves this exact
    /// request.
    pub fn waste_record(&self, token: &str, req: WasteRequest) -> AppResult<WasteRow> {
        let s = self.session(token)?;
        s.require("waste.record")?;
        self.lots_writable()?;
        let pid = validate::id(&req.product_id, "Product")?;
        let lot = req.lot_id.as_deref().filter(|l| !l.is_empty()).map(|l| validate::id(l, "Batch")).transpose()?;
        if !REASONS.contains(&req.reason.as_str()) {
            return Err(AppError::validation("Choose why the stock is being written off."));
        }
        let note = crate::setup::clean_opt(&req.note, "Note", 500)?;
        idempotency::validate_operation_id(&req.operation_id)?;
        if let Check::Replay { result } = self.db.read(|c| idempotency::check(c, &req.operation_id, "waste.record", &req))? {
            let id = result["waste_id"].as_str().unwrap_or_default().to_string();
            return self.waste_get(token, &id);
        }
        let (name, cost, inv) = self.db.read(|c| {
            let name: String = c
                .query_row("SELECT name FROM products WHERE product_id=?1", [&pid], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Product"))?;
            let unit = unit_cost(c, &pid, &s.branch_id, lot.as_deref())?;
            Ok((name, money::extend(unit, req.qty_milli.max(0))?, settings::get::<InventorySettings>(c, settings::KEY_INVENTORY)?))
        })?;
        let needs = (inv.waste_approval_cost_minor > 0 && cost > inv.waste_approval_cost_minor)
            || (req.reason == "shrinkage" && inv.waste_shrinkage_needs_approval);
        let approved = if needs {
            let summary = format!(
                "Write off {} × {} ({}) as {}",
                money::format_qty(req.qty_milli),
                name,
                money::format_decimal(cost, 3),
                reason_words(&req.reason)
            );
            self.authorize_bound(
                &s,
                "waste.approve",
                req.approval_token.as_deref(),
                &summary,
                "waste.record",
                &pid,
                &json!({ "lot_id": lot, "qty_milli": req.qty_milli, "reason": req.reason }),
            )?
        } else {
            None
        };
        let actor = self.actor(&s, approved.clone());
        let id = self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "waste.record", &req)? {
                Check::Replay { result } => return Ok(result["waste_id"].as_str().unwrap_or_default().to_string()),
                Check::New { payload_hash } => payload_hash,
            };
            let (track, dec): (i64, i64) =
                tx.query_row("SELECT track_inventory, allow_decimal_quantity FROM products WHERE product_id=?1", [&pid], |r| Ok((r.get(0)?, r.get(1)?)))?;
            if track == 0 {
                return Err(AppError::validation("This product does not track stock."));
            }
            validate::qty_positive(req.qty_milli, dec == 1, "Quantity")?;
            let on_hand = current_qty(tx, &pid, &s.branch_id)?;
            if req.qty_milli > on_hand {
                return Err(AppError::validation(format!("Only {} is in stock.", money::format_qty(on_hand.max(0)))));
            }
            if let Some(l) = &lot {
                let f = lots::lot_facts(tx, l)?;
                if f.product_id != pid || f.branch_id != s.branch_id {
                    return Err(AppError::validation("That batch is not this product in this branch."));
                }
                let r = replay(tx, &pid, &s.branch_id)?;
                let bal = r.lots.iter().find(|x| &x.facts.lot_id == l).map(|x| x.balance_milli + x.estimated_out_milli).unwrap_or(0);
                // Evidence may take back what the estimate had used, but never
                // more than the batch ever held after recorded removals.
                if req.qty_milli > bal {
                    return Err(AppError::validation(format!("Batch {} has at most {} left.", f.lot_number, money::format_qty(bal.max(0)))));
                }
            }
            let unit = unit_cost(tx, &pid, &s.branch_id, lot.as_deref())?;
            let cost = money::extend(unit, req.qty_milli)?;
            let id = new_id();
            let number = format!("W-{:05}", next_seq(tx, "waste")?);
            let reason_line = format!("Waste {number}: {}", reason_words(&req.reason));
            let (_, movement) = apply_movement_lot(
                tx,
                &Movement {
                    product_id: &pid,
                    branch_id: &s.branch_id,
                    kind: "waste",
                    qty_delta_milli: -req.qty_milli,
                    unit_cost_minor: Some(unit),
                    source_type: "waste",
                    source_id: Some(&id),
                    reason: Some(&reason_line),
                    user_id: Some(&s.user_id),
                    device_id: Some(&s.device_id),
                },
                None,
                lot.as_deref(),
            )?;
            let today = time::business_date(time::now(), &time::day(tx)?)?;
            tx.execute(
                "INSERT INTO waste_records(waste_id, waste_number, branch_id, product_id, lot_id, qty_milli, unit_cost_minor, cost_minor, reason,
                    business_date, note, user_id, approved_by, movement_id, operation_id, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![id, number, s.branch_id, pid, lot, req.qty_milli, unit, cost, req.reason, today, note, s.user_id, approved, movement, req.operation_id, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "waste.recorded",
                "waste",
                Some(&id),
                None,
                Some(&json!({ "waste_number": number, "product_id": pid, "lot_id": lot, "qty_milli": req.qty_milli, "cost_minor": cost, "reason": req.reason, "note": note })),
            )?;
            idempotency::complete(tx, &req.operation_id, "waste.record", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &json!({ "waste_id": id }))?;
            Ok(id)
        })?;
        self.waste_get(token, &id)
    }

    /// Reverse waste recorded by mistake: the stock comes back by a new
    /// movement; the record stays, marked reversed.
    pub fn waste_reverse(&self, token: &str, waste_id: &str, reason: &str, operation_id: &str) -> AppResult<WasteRow> {
        let s = self.session(token)?;
        s.require("waste.approve")?;
        self.lots_writable()?;
        let id = validate::id(waste_id, "Waste")?;
        let why = crate::setup::clean(reason, "Reason", 200, true)?;
        let actor = self.actor(&s, None);
        let payload = json!({ "waste_id": id, "reason": why });
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, operation_id, "waste.reverse", &payload)? {
                Check::Replay { .. } => return Ok(()),
                Check::New { payload_hash } => payload_hash,
            };
            let (status, pid, lot, qty, unit, number, branch): (String, String, Option<String>, i64, i64, String, String) = tx
                .query_row(
                    "SELECT status, product_id, lot_id, qty_milli, unit_cost_minor, waste_number, branch_id FROM waste_records WHERE waste_id=?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Waste"))?;
            if status != "recorded" {
                return Err(AppError::conflict("This waste was already reversed."));
            }
            let line = format!("Reversal of waste {number}: {why}");
            let (_, movement) = apply_movement_lot(
                tx,
                &Movement {
                    product_id: &pid,
                    branch_id: &branch,
                    kind: "waste",
                    qty_delta_milli: qty,
                    unit_cost_minor: Some(unit),
                    source_type: "waste_reversal",
                    source_id: Some(&id),
                    reason: Some(&line),
                    user_id: Some(&s.user_id),
                    device_id: Some(&s.device_id),
                },
                None,
                lot.as_deref(),
            )?;
            tx.execute(
                "UPDATE waste_records SET status='reversed', reversed_by=?2, reversed_at=?3, reversal_reason=?4, reversal_movement_id=?5 WHERE waste_id=?1",
                params![id, s.user_id, time::now_str(), why, movement],
            )?;
            audit::record(tx, &actor, "waste.reversed", "waste", Some(&id), Some(&json!({ "status": "recorded" })), Some(&json!({ "status": "reversed", "reason": why })))?;
            idempotency::complete(tx, operation_id, "waste.reverse", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &json!({ "waste_id": id }))?;
            Ok(())
        })?;
        self.waste_get(token, &id)
    }

    pub fn waste_get(&self, token: &str, waste_id: &str) -> AppResult<WasteRow> {
        let s = self.session(token)?;
        s.require("inventory.view")?;
        let id = validate::id(waste_id, "Waste")?;
        let show = s.has("products.view_cost");
        self.db.read(|c| {
            c.query_row(&format!("{ROW_SQL} WHERE w.waste_id=?1"), [&id], row(show)).optional()?.ok_or_else(|| AppError::not_found("Waste"))
        })
    }

    pub fn waste_list(&self, token: &str, from: Option<String>, to: Option<String>, reason: Option<String>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("inventory.view")?;
        let show = s.has("products.view_cost");
        self.db.read(|c| {
            let today = time::business_date(time::now(), &time::day(c)?)?;
            let from = from.filter(|x| !x.is_empty()).unwrap_or_else(|| format!("{}-01", &today[..7]));
            let to = to.filter(|x| !x.is_empty()).unwrap_or(today);
            time::validate_date(&from)?;
            time::validate_date(&to)?;
            let scope = crate::branches::list_scope(c, &s)?;
            let mut st = c.prepare(&format!(
                "{ROW_SQL} WHERE w.business_date>=?1 AND w.business_date<=?2 AND (?3 IS NULL OR w.reason=?3) AND (?4 IS NULL OR w.branch_id=?4)
                 ORDER BY w.created_at DESC LIMIT 1000"
            ))?;
            let rows = st.query_map(params![from, to, reason.filter(|r| !r.is_empty()), scope], row(show))?.collect::<Result<Vec<_>, _>>()?;
            let live: Vec<&WasteRow> = rows.iter().filter(|r| r.status == "recorded").collect();
            let cost: i64 = live.iter().map(|r| r.cost_minor.unwrap_or(0)).sum();
            Ok(json!({ "from": from, "to": to, "rows": rows, "count": live.len(), "cost_minor": if show { json!(cost) } else { Value::Null } }))
        })
    }

    /// What is expiring: every batch with stock, its one current state, how
    /// long it lasts at recent sales, and what may be left at expiry.
    pub fn expiry_overview(&self, token: &str, f: ExpiryFilter) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("inventory.view")?;
        let show_cost = s.has("products.view_cost");
        self.db.read(|c| {
            let inv: InventorySettings = settings::get(c, settings::KEY_INVENTORY)?;
            let today = time::business_date(time::now(), &time::day(c)?)?;
            let branch = crate::branches::report_scope(c, &s, f.branch_id.as_deref())?.unwrap_or_else(|| s.branch_id.clone());
            let mut st = c.prepare(
                "SELECT DISTINCT l.product_id FROM stock_lots l JOIN products p ON p.product_id=l.product_id
                 WHERE l.branch_id=?1 AND (?2 IS NULL OR p.category_id=?2) AND (?3 IS NULL OR l.product_id=?3)
                   AND (?4 IS NULL OR l.supplier_id=?4) AND (?5 IS NULL OR COALESCE(l.location_id,'')=?5)",
            )?;
            let nz = |x: &Option<String>| x.clone().filter(|v| !v.is_empty());
            let pids = st
                .query_map(params![branch, nz(&f.category_id), nz(&f.product_id), nz(&f.supplier_id), nz(&f.location_id)], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let mut rows = vec![];
            for pid in pids {
                let r = replay(c, &pid, &branch)?;
                let (name, name_ar, category, price, rate, incl): (String, Option<String>, Option<String>, Option<i64>, i64, i64) = c.query_row(
                    "SELECT p.name, p.name_ar, (SELECT name FROM categories WHERE category_id=p.category_id), NULL, COALESCE(t.rate_bp,0), COALESCE(t.inclusive,1)
                     FROM products p LEFT JOIN tax_rules t ON t.tax_rule_id=p.tax_rule_id WHERE p.product_id=?1",
                    [&pid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )?;
                let price = price.or(crate::catalog::current_price(c, &pid)?);
                let d = lots::demand(c, &pid, &branch, &today, 30)?;
                let cov = lots::cover(r.stock_milli, &d, &today);
                let mut ahead = r.unlotted_milli.max(0);
                for l in &r.lots {
                    let (status, days) = expiry_status(l.facts.expires_on.as_deref(), l.facts.expiry_kind.as_deref(), l.balance_milli, &today, &inv);
                    if l.balance_milli <= 0 {
                        continue;
                    }
                    if let Some(want) = f.supplier_id.as_ref().filter(|x| !x.is_empty()) {
                        if l.facts.supplier_id.as_ref() != Some(want) {
                            ahead += l.balance_milli;
                            continue;
                        }
                    }
                    // What recent sales would use before this batch expires
                    // (older stock and earlier batches go first).
                    let leftover = match (days, cov.state) {
                        (Some(dl), "ok") if dl >= 0 => {
                            let can = (cov.per_day_milli as i128 * dl as i128) as i64;
                            let from_this = (can - ahead).clamp(0, l.balance_milli);
                            l.balance_milli - from_this
                        }
                        (Some(dl), _) if dl < 0 => l.balance_milli,
                        (None, _) => 0,
                        _ => l.balance_milli,
                    };
                    ahead += l.balance_milli;
                    let net_price = price.map(|p| if incl == 1 { p - money::tax_from_inclusive(p, rate).unwrap_or(0) } else { p });
                    let scenarios: Vec<Value> = match (net_price, show_cost) {
                        (Some(np), true) if status != "no_date" => [10i64, 20, 30, 50]
                            .iter()
                            .map(|pct| {
                                let gross = price.unwrap_or(0) * (100 - pct) / 100;
                                let net = np * (100 - pct) / 100;
                                json!({ "percent_off": pct, "price_minor": gross, "margin_minor": net - l.facts.unit_cost_minor,
                                        "below_cost": net < l.facts.unit_cost_minor })
                            })
                            .collect(),
                        _ => vec![],
                    };
                    rows.push(json!({
                        "lot_id": l.facts.lot_id, "lot_number": l.facts.lot_number, "supplier_lot_code": l.facts.supplier_lot_code,
                        "product_id": pid, "product_name": name, "product_name_ar": name_ar, "category": category,
                        "supplier_id": l.facts.supplier_id,
                        "supplier_name": l.facts.supplier_id.as_ref().and_then(|sid| c.query_row("SELECT name FROM suppliers WHERE supplier_id=?1", [sid], |r| r.get::<_, String>(0)).ok()),
                        "location_name": l.facts.location_id.as_ref().and_then(|lid| c.query_row("SELECT name FROM stock_locations WHERE location_id=?1", [lid], |r| r.get::<_, String>(0)).ok()),
                        "expires_on": l.facts.expires_on, "expiry_kind": l.facts.expiry_kind, "days_left": days, "status": status,
                        "balance_milli": l.balance_milli, "estimated_out_milli": l.estimated_out_milli,
                        "unit_cost_minor": if show_cost { json!(l.facts.unit_cost_minor) } else { Value::Null },
                        "value_minor": if show_cost { json!(money::extend(l.facts.unit_cost_minor, l.balance_milli)?) } else { Value::Null },
                        "per_day_milli": cov.per_day_milli, "demand_state": cov.state,
                        "likely_left_milli": leftover,
                        "at_risk_minor": if show_cost { json!(money::extend(l.facts.unit_cost_minor, leftover)?) } else { Value::Null },
                        "price_minor": price, "markdowns": scenarios,
                    }));
                }
            }
            let order = |s: &str| ["expired", "past_best_before", "urgent", "soon", "later", "healthy", "no_date"].iter().position(|x| *x == s).unwrap_or(9);
            rows.sort_by(|a, b| {
                order(a["status"].as_str().unwrap_or("")).cmp(&order(b["status"].as_str().unwrap_or(""))).then(
                    a["expires_on"].as_str().unwrap_or("9999").cmp(b["expires_on"].as_str().unwrap_or("9999")),
                )
            });
            let sum = |pred: &dyn Fn(&Value) -> bool, key: &str| -> i64 { rows.iter().filter(|r| pred(r)).map(|r| r[key].as_i64().unwrap_or(0)).sum() };
            let within = |n: i64| {
                let n2 = n;
                move |r: &Value| r["days_left"].as_i64().map(|d| (0..=n2).contains(&d)).unwrap_or(false)
            };
            let buckets: Vec<Value> = [7i64, 14, 30, 60, 90]
                .iter()
                .map(|n| json!({ "days": n, "qty_milli": sum(&within(*n), "balance_milli"),
                                 "value_minor": if show_cost { json!(sum(&within(*n), "value_minor")) } else { Value::Null } }))
                .collect();
            let expired = |r: &Value| matches!(r["status"].as_str(), Some("expired" | "past_best_before"));
            let lost: i64 = c.query_row(
                "SELECT COALESCE(SUM(cost_minor),0) FROM waste_records WHERE branch_id=?1 AND reason='expired' AND status='recorded' AND business_date>=?2",
                params![branch, format!("{}-01", &today[..7])],
                |r| r.get(0),
            )?;
            Ok(json!({
                "today": today, "branch_id": branch, "rows": rows, "buckets": buckets,
                "expired_on_hand_milli": sum(&expired, "balance_milli"),
                "expired_on_hand_minor": if show_cost { json!(sum(&expired, "value_minor")) } else { Value::Null },
                "at_risk_minor": if show_cost { json!(sum(&|_| true, "at_risk_minor")) } else { Value::Null },
                "expired_waste_this_month_minor": if show_cost { json!(lost) } else { Value::Null },
                "thresholds": { "urgent": inv.expiry_urgent_days, "soon": inv.expiry_soon_days, "later": inv.expiry_later_days },
            }))
        })
    }

    /// Days of stock left for products that track stock.
    pub fn stock_cover(&self, token: &str, f: CoverFilter) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("inventory.view")?;
        let window = f.window_days.unwrap_or(30);
        if ![7, 30, 60, 90].contains(&window) {
            return Err(AppError::validation("Choose 7, 30, 60 or 90 days of sales."));
        }
        let limit = validate::limit(f.limit, 300, 2000);
        self.db.read(|c| {
            let today = time::business_date(time::now(), &time::day(c)?)?;
            let branch = crate::branches::report_scope(c, &s, f.branch_id.as_deref())?.unwrap_or_else(|| s.branch_id.clone());
            let search = f.search.clone().filter(|x| !x.trim().is_empty()).map(|x| format!("%{}%", x.trim()));
            let mut st = c.prepare(&format!(
                "SELECT p.product_id, p.name, p.name_ar, (SELECT name FROM categories WHERE category_id=p.category_id) FROM products p
                 WHERE p.active=1 AND p.track_inventory=1 AND (?1 IS NULL OR p.category_id=?1) AND (?2 IS NULL OR p.product_id=?2)
                   AND (?3 IS NULL OR p.name LIKE ?3 OR p.sku LIKE ?3)
                 ORDER BY p.name COLLATE NOCASE LIMIT {limit}"
            ))?;
            let nz = |x: &Option<String>| x.clone().filter(|v| !v.is_empty());
            let prods = st
                .query_map(params![nz(&f.category_id), nz(&f.product_id), search], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut rows = vec![];
            for (pid, name, name_ar, cat) in prods {
                let on_hand = current_qty(c, &pid, &branch)?;
                let held = crate::reservations::reserved_milli(c, &pid, &branch, None)?;
                let available = on_hand - held;
                let d = lots::demand(c, &pid, &branch, &today, window)?;
                let cv = lots::cover(available, &d, &today);
                let inbound: i64 = c.query_row(
                    "SELECT COALESCE(SUM(i.qty_ordered_milli - i.qty_received_milli),0) FROM purchase_order_items i JOIN purchase_orders o ON o.po_id=i.po_id
                     WHERE i.product_id=?1 AND o.branch_id=?2 AND o.status IN ('ordered','partially_received')",
                    params![pid, branch],
                    |r| r.get(0),
                )?;
                let with_inbound = (inbound > 0).then(|| lots::cover(available + inbound, &d, &today));
                rows.push(json!({
                    "product_id": pid, "name": name, "name_ar": name_ar, "category": cat,
                    "on_hand_milli": on_hand, "held_milli": held, "available_milli": available,
                    "net_sold_milli": d.net_sold_milli, "window_days": window, "history_days": d.history_days,
                    "per_day_milli": cv.per_day_milli, "state": cv.state, "cover_tenths": cv.cover_tenths, "stockout_on": cv.stockout_on,
                    "inbound_milli": inbound,
                    "cover_with_inbound_tenths": with_inbound.as_ref().and_then(|w| w.cover_tenths),
                }));
            }
            // Soonest to run out first; then the rest by state.
            let rank = |r: &Value| match r["state"].as_str() {
                Some("ok") => 0,
                Some("no_stock") => 1,
                Some("not_enough_history") => 2,
                _ => 3,
            };
            rows.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a["cover_tenths"].as_i64().unwrap_or(i64::MAX).cmp(&b["cover_tenths"].as_i64().unwrap_or(i64::MAX))));
            Ok(json!({ "today": today, "branch_id": branch, "window_days": window, "rows": rows }))
        })
    }

    /// Waste totals for a period, by reason, product, category, supplier and
    /// day, with the two ratios defined on the report.
    pub fn waste_summary(&self, token: &str, from: Option<String>, to: Option<String>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("inventory.view")?;
        if !s.has("products.view_cost") {
            return Err(AppError::forbidden("products.view_cost"));
        }
        self.db.read(|c| {
            let today = time::business_date(time::now(), &time::day(c)?)?;
            let from = from.filter(|x| !x.is_empty()).unwrap_or_else(|| format!("{}-01", &today[..7]));
            let to = to.filter(|x| !x.is_empty()).unwrap_or(today);
            time::validate_date(&from)?;
            time::validate_date(&to)?;
            let scope = crate::branches::list_scope(c, &s)?;
            let group = |expr: &str| -> AppResult<Vec<Value>> {
                let mut st = c.prepare(&format!(
                    "SELECT {expr} k, COUNT(*), COALESCE(SUM(w.qty_milli),0), COALESCE(SUM(w.cost_minor),0)
                     FROM waste_records w JOIN products p ON p.product_id=w.product_id LEFT JOIN stock_lots l ON l.lot_id=w.lot_id
                     WHERE w.status='recorded' AND w.business_date>=?1 AND w.business_date<=?2 AND (?3 IS NULL OR w.branch_id=?3)
                     GROUP BY 1 ORDER BY 4 DESC LIMIT 50"
                ))?;
                let rows = st
                    .query_map(params![from, to, scope], |r| {
                        Ok(json!({ "key": r.get::<_, Option<String>>(0)?, "count": r.get::<_, i64>(1)?, "qty_milli": r.get::<_, i64>(2)?, "cost_minor": r.get::<_, i64>(3)? }))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            };
            let by_reason = group("w.reason")?;
            let by_product = group("p.name")?;
            let by_category = group("(SELECT name FROM categories WHERE category_id=p.category_id)")?;
            let by_supplier = group("(SELECT name FROM suppliers WHERE supplier_id=l.supplier_id)")?;
            let by_day = group("w.business_date")?;
            let cost: i64 = by_reason.iter().map(|r| r["cost_minor"].as_i64().unwrap_or(0)).sum();
            let qty: i64 = by_reason.iter().map(|r| r["qty_milli"].as_i64().unwrap_or(0)).sum();
            // Denominators: net sales excluding VAT (sales less refunds and
            // voids, by business date) and goods received at cost.
            let (sales, stax): (i64, i64) = c.query_row(
                "SELECT COALESCE(SUM(total_minor),0), COALESCE(SUM(tax_minor),0) FROM sales WHERE business_date>=?1 AND business_date<=?2 AND (?3 IS NULL OR branch_id=?3)",
                params![from, to, scope],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let (refunds, rtax): (i64, i64) = c.query_row(
                "SELECT COALESCE(SUM(total_minor),0), COALESCE(SUM(tax_minor),0) FROM refunds WHERE business_date>=?1 AND business_date<=?2 AND (?3 IS NULL OR branch_id=?3)",
                params![from, to, scope],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let net_sales = (sales - stax) - (refunds - rtax);
            let (a, b) = time::local_date_range_utc(&from, &to, &time::day(c)?)?;
            let purchased: i64 = c.query_row(
                "SELECT COALESCE(SUM(total_cost_minor),0) FROM goods_receipts WHERE created_at>=?1 AND created_at<?2 AND (?3 IS NULL OR branch_id=?3)",
                params![a, b, scope],
                |r| r.get(0),
            )?;
            let pct = |num: i64, den: i64| if den > 0 { Some((num as i128 * 10_000 / den as i128) as i64) } else { None };
            Ok(json!({
                "from": from, "to": to, "cost_minor": cost, "qty_milli": qty,
                "by_reason": by_reason, "by_product": by_product, "by_category": by_category, "by_supplier": by_supplier, "by_day": by_day,
                "net_sales_ex_vat_minor": net_sales, "purchased_cost_minor": purchased,
                "pct_of_sales_bp": pct(cost, net_sales), "pct_of_purchases_bp": pct(cost, purchased),
                "definitions": [
                    "Waste is valued at cost (the batch cost when the batch is known, else the average cost at the time).",
                    "% of sales = waste at cost ÷ net sales excluding VAT (sales less refunds and voids) for the same days.",
                    "% of purchases = waste at cost ÷ goods received at cost for the same days.",
                    "Reversed waste is not counted."
                ],
            }))
        })
    }
}
