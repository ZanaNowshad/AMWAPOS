//! Virtual bundles, kits and hampers (docs/PROMOTIONS_AND_BUNDLES.md).
//!
//! A bundle is a sellable product (the "parent") made of fixed quantities of
//! other products. It is assembled when sold: the parent has no stock and no
//! stock movement; each sale takes the components from stock in the same
//! commit. Nothing is manufactured in advance.
//!
//! - Price: the parent's price from the normal price resolver (Wave 5). The
//!   bundle line may take promotions and coupons like any product line; its
//!   components never take product promotions of their own.
//! - Money split: the line's net is allocated over the components by their
//!   normal value (component price × quantity) with `money::allocate`, so the
//!   parts always sum exactly; VAT is computed per component at its own rate.
//! - Cost: the sum of the components' average costs.
//! - Versions: every change saves a new version. A sale keeps the version it
//!   was priced with; history never changes.
//! - Availability: the smallest of (component stock / quantity per bundle)
//!   over the stock-tracked components, in one query.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::service::AppCore;
use crate::time;
use crate::validate;

/// One component of a bundle version, as sold now.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Component {
    pub product_id: String,
    pub name: String,
    pub name_ar: Option<String>,
    pub sku: Option<String>,
    pub unit: String,
    /// Quantity in one bundle.
    pub qty_milli: i64,
    /// The component's normal (retail) unit price now; 0 without a price.
    pub unit_price_minor: i64,
    pub tax_rule_id: String,
    pub tax_rate_bp: i64,
    pub tax_inclusive: bool,
    pub track_inventory: bool,
    pub category_id: Option<String>,
}

impl Component {
    /// Quantity of this component in `bundle_qty_milli` bundles.
    pub fn total_qty(&self, bundle_qty_milli: i64) -> i64 {
        (self.qty_milli as i128 * bundle_qty_milli as i128 / 1000) as i64
    }

    /// Normal value of this component in `bundle_qty_milli` bundles: the
    /// weight the bundle's money is split by.
    pub fn weight(&self, bundle_qty_milli: i64) -> i64 {
        crate::money::extend(self.unit_price_minor, self.total_qty(bundle_qty_milli)).unwrap_or(0)
    }
}

/// The active version of a bundle (None: not a bundle, or switched off).
pub fn active_version(c: &Connection, product_id: &str) -> AppResult<Option<i64>> {
    Ok(c.query_row("SELECT version FROM bundles WHERE bundle_product_id=?1 AND active=1", [product_id], |r| r.get(0)).optional()?)
}

/// Whether a product is set up as a bundle at all (active or not).
pub fn is_bundle(c: &Connection, product_id: &str) -> AppResult<bool> {
    Ok(c.query_row("SELECT 1 FROM bundles WHERE bundle_product_id=?1", [product_id], |_| Ok(true)).optional()?.unwrap_or(false))
}

/// The components of one bundle version (one query), in a stable order.
pub fn components(c: &Connection, bundle_id: &str, version: i64) -> AppResult<Vec<Component>> {
    let sql = format!(
        "SELECT p.product_id, p.name, NULLIF(TRIM(p.name_ar),''), p.sku, p.unit, bc.qty_milli, COALESCE({}, 0), p.tax_rule_id, t.rate_bp, t.inclusive,
                p.track_inventory, p.category_id
         FROM bundle_components bc JOIN products p ON p.product_id=bc.component_product_id
         JOIN tax_rules t ON t.tax_rule_id=p.tax_rule_id
         WHERE bc.bundle_product_id=?1 AND bc.version=?2 ORDER BY bc.component_product_id",
        crate::catalog::price_sql_for("retail", "amount_minor")
    );
    let mut st = c.prepare_cached(&sql)?;
    let rows = st
        .query_map(params![bundle_id, version], |r| {
            Ok(Component {
                product_id: r.get(0)?,
                name: r.get(1)?,
                name_ar: r.get(2)?,
                sku: r.get(3)?,
                unit: r.get(4)?,
                qty_milli: r.get(5)?,
                unit_price_minor: r.get(6)?,
                tax_rule_id: r.get(7)?,
                tax_rate_bp: r.get(8)?,
                tax_inclusive: r.get::<_, i64>(9)? == 1,
                track_inventory: r.get::<_, i64>(10)? == 1,
                category_id: r.get(11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// How many whole bundles the branch's stock can make now, and the
/// component that limits it. None when no component is stock-tracked.
pub fn availability(c: &Connection, bundle_id: &str, version: i64, branch_id: &str) -> AppResult<Option<(i64, String)>> {
    let mut st = c.prepare_cached(
        "SELECT p.name, bc.qty_milli, COALESCE(sl.qty_milli, 0)
         FROM bundle_components bc JOIN products p ON p.product_id=bc.component_product_id
         LEFT JOIN stock_levels sl ON sl.product_id=bc.component_product_id AND sl.branch_id=?3
         WHERE bc.bundle_product_id=?1 AND bc.version=?2 AND p.track_inventory=1
         ORDER BY bc.component_product_id",
    )?;
    let mut best: Option<(i64, String)> = None;
    for row in
        st.query_map(params![bundle_id, version, branch_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))?
    {
        let (name, need, have) = row?;
        let n = if need > 0 { (have.max(0) / need).max(0) } else { i64::MAX };
        if best.as_ref().is_none_or(|b| n < b.0) {
            best = Some((n, name));
        }
    }
    Ok(best)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComponentInput {
    pub product_id: String,
    pub qty_milli: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BundleSave {
    pub bundle_product_id: String,
    pub components: Vec<ComponentInput>,
    #[serde(default = "yes")]
    pub active: bool,
    /// The version being edited (0 for a new bundle).
    #[serde(default)]
    pub version: i64,
    pub operation_id: String,
}

fn yes() -> bool {
    true
}

impl AppCore {
    /// Set up or change a bundle. A change in components saves a new version;
    /// switching it off or on keeps the version.
    pub fn bundles_save(&self, token: &str, req: BundleSave) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("bundles.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let pid = validate::id(&req.bundle_product_id, "Product")?;
        let mut comps = req.components.clone();
        comps.sort_by(|a, b| a.product_id.cmp(&b.product_id));
        if comps.is_empty() || comps.len() > 50 {
            return Err(AppError::validation("A bundle has between 1 and 50 items."));
        }
        if comps.windows(2).any(|w| w[0].product_id == w[1].product_id) {
            return Err(AppError::validation("Each item appears once in a bundle; set its quantity instead."));
        }
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "bundles.save", &req)? {
                Check::Replay { .. } => return Ok(()),
                Check::New { payload_hash } => payload_hash,
            };
            let parent: Option<(String, i64, i64, Option<String>)> = tx
                .query_row("SELECT name, allow_decimal_quantity, active, plu FROM products WHERE product_id=?1", [&pid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .optional()?;
            let (pname, pdec, _active, plu) = parent.ok_or_else(|| AppError::not_found("Product"))?;
            if pdec == 1 || plu.is_some() {
                return Err(AppError::validation("A bundle is sold in whole units: it cannot be a weighed item or have a PLU."));
            }
            let stock = crate::inventory::current_qty(tx, &pid, &s.branch_id)?;
            for c in &comps {
                if c.product_id == pid {
                    return Err(AppError::validation("A bundle cannot contain itself."));
                }
                let row: Option<(String, i64, i64)> = tx
                    .query_row("SELECT name, allow_decimal_quantity, active FROM products WHERE product_id=?1", [&c.product_id], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })
                    .optional()?;
                let (cname, dec, active) = row.ok_or_else(|| AppError::validation("One of the items no longer exists."))?;
                if active != 1 {
                    return Err(AppError::validation(format!("{cname} is archived.")));
                }
                if dec == 1 {
                    return Err(AppError::validation(format!(
                        "{cname} is weighed: its packs vary, so it cannot be a fixed part of a bundle. Sell it separately."
                    )));
                }
                if c.qty_milli <= 0 || c.qty_milli % 1000 != 0 || c.qty_milli > 1_000_000 {
                    return Err(AppError::validation(format!("{cname}: enter a whole quantity of 1 or more.")));
                }
                if is_bundle(tx, &c.product_id)? {
                    return Err(AppError::validation(format!("{cname} is itself a bundle. Bundles cannot contain bundles.")));
                }
            }
            if !is_bundle(tx, &pid)? {
                // A bundle that is used as a component elsewhere cannot become a bundle.
                let used: bool = tx
                    .query_row("SELECT 1 FROM bundle_components WHERE component_product_id=?1 LIMIT 1", [&pid], |_| Ok(true))
                    .optional()?
                    .unwrap_or(false);
                if used {
                    return Err(AppError::validation(format!("{pname} is an item in another bundle.")));
                }
                if stock != 0 {
                    return Err(AppError::validation(format!(
                        "{pname} has stock recorded. A bundle has no stock of its own: count it to zero first."
                    )));
                }
            }
            let now = time::now_str();
            let before: Option<(i64, i64)> =
                tx.query_row("SELECT version, active FROM bundles WHERE bundle_product_id=?1", [&pid], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let before_comps = match before {
                Some((v, _)) => components(tx, &pid, v)?,
                None => vec![],
            };
            let same = before_comps.len() == comps.len()
                && before_comps.iter().zip(&comps).all(|(a, b)| a.product_id == b.product_id && a.qty_milli == b.qty_milli);
            let version = match before {
                None => {
                    tx.execute(
                        "INSERT INTO bundles(bundle_product_id, version, active, created_by, created_at, updated_by, updated_at)
                         VALUES (?1,1,?2,?3,?4,?3,?4)",
                        params![pid, req.active as i64, s.user_id, now],
                    )?;
                    // No stock and no movements for the parent.
                    tx.execute("UPDATE products SET track_inventory=0, updated_at=?2, version=version+1 WHERE product_id=?1", params![pid, now])?;
                    1
                }
                Some((v, _)) => {
                    if v != req.version {
                        return Err(AppError::conflict("This bundle was changed by someone else. Reload it and try again."));
                    }
                    let nv = if same { v } else { v + 1 };
                    tx.execute(
                        "UPDATE bundles SET version=?2, active=?3, updated_by=?4, updated_at=?5 WHERE bundle_product_id=?1",
                        params![pid, nv, req.active as i64, s.user_id, now],
                    )?;
                    nv
                }
            };
            if before.is_none() || !same {
                for c in &comps {
                    tx.execute(
                        "INSERT INTO bundle_components(bundle_product_id, version, component_product_id, qty_milli) VALUES (?1,?2,?3,?4)",
                        params![pid, version, c.product_id, c.qty_milli],
                    )?;
                }
            }
            audit::record(
                tx,
                &actor,
                if before.is_none() { "bundle.created" } else { "bundle.changed" },
                "bundle",
                Some(&pid),
                before.map(|(v, a)| json!({ "version": v, "active": a == 1, "components": before_comps.iter().map(|c| json!({ "product_id": c.product_id, "qty_milli": c.qty_milli })).collect::<Vec<_>>() })).as_ref(),
                Some(&json!({ "version": version, "active": req.active, "components": comps })),
            )?;
            idempotency::complete(tx, &req.operation_id, "bundles.save", Some(&s.user_id), Some(&s.device_id), &hash, Some(&pid), &json!({}))?;
            Ok(())
        })?;
        self.bundles_get(token, &pid)
    }

    pub fn bundles_list(&self, token: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("bundles.manage") && !s.has("products.view") {
            return Err(AppError::forbidden("bundles.manage"));
        }
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT b.bundle_product_id, p.name, b.version, b.active FROM bundles b JOIN products p ON p.product_id=b.bundle_product_id
                 ORDER BY p.name, b.bundle_product_id",
            )?;
            let rows: Vec<(String, String, i64, i64)> =
                st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<Vec<_>, _>>()?;
            let mut out = vec![];
            for (pid, name, version, active) in rows {
                let avail = availability(c, &pid, version, &s.branch_id)?;
                out.push(json!({ "bundle_product_id": pid, "name": name, "version": version, "active": active == 1,
                    "available": avail.as_ref().map(|a| a.0), "limited_by": avail.map(|a| a.1),
                    "price_minor": crate::catalog::current_price(c, &pid)? }));
            }
            Ok(json!({ "rows": out }))
        })
    }

    /// One bundle: components with stock, the limiting component, how many
    /// can be made, the price, the components' normal value, the saving and
    /// (with cost permission) the margin.
    pub fn bundles_get(&self, token: &str, bundle_product_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("bundles.manage") && !s.has("products.view") {
            return Err(AppError::forbidden("bundles.manage"));
        }
        let pid = validate::id(bundle_product_id, "Product")?;
        let show_cost = s.has("products.view_cost");
        self.db.read(|c| {
            let (version, active): (i64, i64) = c
                .query_row("SELECT version, active FROM bundles WHERE bundle_product_id=?1", [&pid], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| AppError::not_found("Bundle"))?;
            let name: String = c.query_row("SELECT name FROM products WHERE product_id=?1", [&pid], |r| r.get(0))?;
            let comps = components(c, &pid, version)?;
            let price = crate::catalog::current_price(c, &pid)?;
            let normal: i64 = comps.iter().map(|x| x.weight(1000)).sum();
            let mut cost = 0i64;
            let mut items = vec![];
            for x in &comps {
                let stock = crate::inventory::current_qty(c, &x.product_id, &s.branch_id)?;
                let unit_cost = crate::inventory::avg_cost(c, &x.product_id, &s.branch_id)?;
                let line_cost = crate::money::extend(unit_cost, x.qty_milli)?;
                cost += line_cost;
                let mut v = json!({ "product_id": x.product_id, "name": x.name, "qty_milli": x.qty_milli, "unit_price_minor": x.unit_price_minor,
                    "normal_minor": x.weight(1000), "tax_rate_bp": x.tax_rate_bp, "track_inventory": x.track_inventory,
                    "stock_milli": if x.track_inventory { Some(stock) } else { None } });
                if show_cost {
                    v["cost_minor"] = json!(line_cost);
                }
                items.push(v);
            }
            let avail = availability(c, &pid, version, &s.branch_id)?;
            let mut out = json!({ "bundle_product_id": pid, "name": name, "version": version, "active": active == 1,
                "components": items, "price_minor": price, "normal_minor": normal,
                "saving_minor": price.map(|p| (normal - p).max(0)),
                "available": avail.as_ref().map(|a| a.0), "limited_by": avail.map(|a| a.1) });
            if show_cost {
                out["cost_minor"] = json!(cost);
                // Margin on the net of VAT, components at their own rates.
                if let Some(p) = price {
                    let weights: Vec<i64> = comps.iter().map(|x| x.weight(1000)).collect();
                    let weights = if weights.iter().all(|w| *w == 0) { vec![1; weights.len()] } else { weights };
                    let mut net = 0i64;
                    for (x, part) in comps.iter().zip(crate::money::allocate(p, &weights)) {
                        let tax = if x.tax_inclusive {
                            crate::money::tax_from_inclusive(part, x.tax_rate_bp)?
                        } else {
                            0
                        };
                        net += part - tax;
                    }
                    out["margin_minor"] = json!(net - cost);
                    out["margin_bp"] = json!(if net > 0 { Some(((net - cost) as i128 * 10_000 / net as i128) as i64) } else { None });
                }
            }
            Ok(out)
        })
    }
}
