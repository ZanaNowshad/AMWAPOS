//! Promotion administration: save, switch on/pause/end/archive, list and
//! inspect (coverage, margin, conflicts). The money is computed by
//! `promotions`; nothing here changes a product's normal price.
//!
//! Lifecycle: draft → active ⇄ paused → ended; draft, paused and ended can be
//! archived. Reaching the start date does not switch an offer on: someone
//! with `promotions.manage` does. Nothing is ever deleted (a used offer is
//! sale evidence), and editing an offer never changes a committed sale: what
//! each sale took is frozen in `sale_item_promotions`.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::new_id;
use crate::money::percent_of;
use crate::promotions::{self, Promotion};
use crate::service::AppCore;
use crate::time;
use crate::validate;

pub const CHANNELS: [&str; 5] = ["pos", "whatsapp", "phone", "web", "other"];

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PromotionSave {
    pub promotion: Promotion,
    pub operation_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PromotionStatus {
    pub promotion_id: String,
    pub status: String,
    pub version: i64,
    pub operation_id: String,
}

/// Allowed lifecycle moves.
pub fn can_move(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("draft", "active")
            | ("active", "paused")
            | ("paused", "active")
            | ("active", "ended")
            | ("paused", "ended")
            | ("draft", "archived")
            | ("paused", "archived")
            | ("ended", "archived")
    )
}

/// Where an offer stands at `local_now` (derived; never stored).
pub fn state(p: &Promotion, local_now: &str) -> &'static str {
    match p.status.as_str() {
        "active" => {
            if p.starts_at.as_deref().is_some_and(|s| s > local_now) {
                "scheduled"
            } else if p.ends_at.as_deref().is_some_and(|e| e <= local_now) {
                "finished"
            } else {
                "running"
            }
        }
        "paused" => "paused",
        "draft" => "draft",
        "ended" => "ended",
        _ => "archived",
    }
}

fn check_refs(c: &Connection, p: &Promotion) -> AppResult<()> {
    for id in p.buy_products.iter().chain(&p.get_products) {
        let ok: bool = c.query_row("SELECT 1 FROM products WHERE product_id=?1", [id], |_| Ok(true)).optional()?.unwrap_or(false);
        if !ok {
            return Err(AppError::validation("One of the chosen products no longer exists."));
        }
    }
    for id in p.buy_categories.iter().chain(&p.get_categories) {
        let ok: bool = c.query_row("SELECT 1 FROM categories WHERE category_id=?1", [id], |_| Ok(true)).optional()?.unwrap_or(false);
        if !ok {
            return Err(AppError::validation("One of the chosen categories no longer exists."));
        }
    }
    if let Some(b) = &p.branches {
        for id in b {
            let ok: bool = c.query_row("SELECT 1 FROM branches WHERE branch_id=?1", [id], |_| Ok(true)).optional()?.unwrap_or(false);
            if !ok {
                return Err(AppError::validation("One of the chosen branches no longer exists."));
            }
        }
    }
    if let Some(ch) = &p.channels {
        if ch.iter().any(|x| !CHANNELS.contains(&x.as_str())) {
            return Err(AppError::validation("Choose channels from the list."));
        }
    }
    Ok(())
}

fn clean(mut p: Promotion) -> Promotion {
    let t = |v: Option<String>| v.map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
    p.name = p.name.trim().to_string();
    p.name_ar = t(p.name_ar);
    p.description = t(p.description);
    p.starts_at = t(p.starts_at).map(|x| x.replace(' ', "T"));
    p.ends_at = t(p.ends_at).map(|x| x.replace(' ', "T"));
    for v in [&mut p.buy_products, &mut p.buy_categories, &mut p.get_products, &mut p.get_categories] {
        v.sort();
        v.dedup();
    }
    if p.target == "all" {
        p.buy_products.clear();
        p.buy_categories.clear();
    }
    if p.kind != "bxgy" {
        p.get_products.clear();
        p.get_categories.clear();
    }
    p.branches = p.branches.map(|mut b| {
        b.sort();
        b.dedup();
        b
    });
    p.channels = p.channels.map(|mut b| {
        b.sort();
        b.dedup();
        b
    });
    p
}

fn write_targets(tx: &Connection, p: &Promotion) -> AppResult<()> {
    tx.execute("DELETE FROM promotion_targets WHERE promotion_id=?1", [&p.promotion_id])?;
    let sets: [(&str, &str, &Vec<String>); 4] = [
        ("buy", "product", &p.buy_products),
        ("buy", "category", &p.buy_categories),
        ("get", "product", &p.get_products),
        ("get", "category", &p.get_categories),
    ];
    for (role, kind, ids) in sets {
        for id in ids {
            tx.execute(
                "INSERT INTO promotion_targets(promotion_id, role, ref_kind, ref_id) VALUES (?1,?2,?3,?4)",
                params![p.promotion_id, role, kind, id],
            )?;
        }
    }
    Ok(())
}

/// The offer price of one unit of a product with normal price `normal`, for
/// the margin preview. None when it depends on the rest of the basket.
pub fn offer_unit_price(p: &Promotion, normal: i64) -> Option<i64> {
    let after = match p.kind.as_str() {
        "percent" => normal - percent_of(normal, p.percent_bp?).ok()?,
        "amount" => normal - p.amount_minor?,
        "fixed_price" => p.price_minor?.min(normal),
        "quantity" => {
            let q = p.buy_qty?.max(1);
            (p.price_minor? + q / 2) / q
        }
        "bxgy" => {
            let (x, y) = (p.buy_qty?.max(1), p.get_qty?.max(1));
            let reward = normal - percent_of(normal, p.percent_bp.unwrap_or(10_000)).ok()?;
            (x * normal + y * reward + (x + y) / 2) / (x + y)
        }
        "basket" => normal - percent_of(normal, p.percent_bp?).ok()?,
        _ => return None,
    };
    Some(after.clamp(0, normal))
}

impl AppCore {
    pub fn promotions_list(&self, token: &str, status: Option<String>) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("promotions.manage") && !s.has("reports.sales") {
            return Err(AppError::forbidden("promotions.manage"));
        }
        self.db.read(|c| {
            let now = self.local_minute(c)?;
            let ids: Vec<String> = {
                let mut st =
                    c.prepare("SELECT promotion_id FROM promotions ORDER BY status='archived', priority DESC, name, promotion_id")?;
                let v = st.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                v
            };
            let mut rows = vec![];
            for id in ids {
                let Some(p) = promotions::load(c, &id)? else { continue };
                if status.as_deref().is_some_and(|f| f != p.status && f != state(&p, &now)) {
                    continue;
                }
                let (uses, amount): (i64, i64) = c.query_row(
                    "SELECT COUNT(DISTINCT sp.sale_id), COALESCE(SUM(sp.amount_minor),0) FROM sale_item_promotions sp
                     JOIN sales sa ON sa.sale_id=sp.sale_id WHERE sp.promotion_id=?1 AND sa.status='completed'",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let coupons: i64 = c.query_row("SELECT COUNT(*) FROM coupons WHERE promotion_id=?1", [&id], |r| r.get(0))?;
                rows.push(json!({ "promotion": p, "state": state(&p, &now), "sales": uses, "discount_minor": amount, "coupons": coupons }));
            }
            Ok(json!({ "rows": rows, "local_now": now }))
        })
    }

    /// One offer with what the editor shows: coverage, margin per covered
    /// product (with cost permission), conflicts and coupons.
    pub fn promotions_get(&self, token: &str, promotion_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("promotions.manage") && !s.has("reports.sales") {
            return Err(AppError::forbidden("promotions.manage"));
        }
        let id = validate::id(promotion_id, "Promotion")?;
        let show_cost = s.has("products.view_cost");
        self.db.read(|c| {
            let p = promotions::load(c, &id)?.ok_or_else(|| AppError::not_found("Promotion"))?;
            let now = self.local_minute(c)?;
            let insight = insight(c, &p, &s.branch_id, show_cost)?;
            let coupons: Vec<Value> = {
                let mut st = c.prepare(
                    "SELECT coupon_id, code, kind, max_redemptions, active,
                        (SELECT COUNT(*) FROM coupon_redemptions r WHERE r.coupon_id=c.coupon_id), version
                     FROM coupons c WHERE promotion_id=?1 ORDER BY code_norm",
                )?;
                let v = st
                    .query_map([&id], |r| {
                        Ok(json!({ "coupon_id": r.get::<_, String>(0)?, "code": r.get::<_, String>(1)?, "kind": r.get::<_, String>(2)?,
                            "max_redemptions": r.get::<_, Option<i64>>(3)?, "active": r.get::<_, i64>(4)? == 1,
                            "redemptions": r.get::<_, i64>(5)?, "version": r.get::<_, i64>(6)? }))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                v
            };
            let used: i64 = c.query_row("SELECT COUNT(*) FROM sale_item_promotions WHERE promotion_id=?1", [&id], |r| r.get(0))?;
            Ok(json!({ "promotion": p, "state": state(&p, &now), "local_now": now, "used": used > 0,
                "coupons": coupons, "insight": insight }))
        })
    }

    /// Coverage, margin and conflicts for an offer being edited (not saved).
    pub fn promotions_preview(&self, token: &str, promotion: Promotion) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("promotions.manage")?;
        let p = clean(promotion);
        let show_cost = s.has("products.view_cost");
        self.db.read(|c| {
            let problem = promotions::validate(&p).err();
            Ok(json!({ "problem": problem, "insight": insight(c, &p, &s.branch_id, show_cost)? }))
        })
    }

    pub fn promotions_save(&self, token: &str, req: PromotionSave) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("promotions.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let p = clean(req.promotion.clone());
        promotions::validate(&p).map_err(AppError::validation)?;
        let id = self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "promotions.save", &req)? {
                Check::Replay { result } => return Ok(result["promotion_id"].as_str().unwrap_or_default().to_string()),
                Check::New { payload_hash } => payload_hash,
            };
            check_refs(tx, &p)?;
            let now = time::now_str();
            let list = |v: &Option<Vec<String>>| v.as_ref().filter(|x| !x.is_empty()).map(|x| serde_json::to_string(x).unwrap_or_default());
            let mut p = p.clone();
            let before = if p.promotion_id.is_empty() {
                p.promotion_id = new_id();
                tx.execute(
                    "INSERT INTO promotions(promotion_id, name, status, kind, created_by, created_at, updated_at, version)
                     VALUES (?1,?2,'draft',?3,?4,?5,?5,0)",
                    params![p.promotion_id, p.name, p.kind, s.user_id, now],
                )?;
                None
            } else {
                let id = validate::id(&p.promotion_id, "Promotion")?;
                let b = promotions::load(tx, &id)?.ok_or_else(|| AppError::not_found("Promotion"))?;
                if b.version != p.version {
                    return Err(AppError::conflict("This offer was changed by someone else. Reload it and try again."));
                }
                if b.status == "archived" || b.status == "ended" {
                    return Err(AppError::conflict("An ended or archived offer cannot be changed. Duplicate it instead."));
                }
                Some(b)
            };
            tx.execute(
                "UPDATE promotions SET name=?2, name_ar=?3, description=?4, kind=?5, target=?6, starts_at=?7, ends_at=?8,
                    branches_json=?9, channels_json=?10, priority=?11, stackable=?12, requires_coupon=?13, percent_bp=?14,
                    amount_minor=?15, price_minor=?16, buy_qty=?17, get_qty=?18, max_uses=?19, threshold_minor=?20,
                    updated_at=?21, version=version+1
                 WHERE promotion_id=?1",
                params![
                    p.promotion_id,
                    p.name,
                    p.name_ar,
                    p.description,
                    p.kind,
                    p.target,
                    p.starts_at,
                    p.ends_at,
                    list(&p.branches),
                    list(&p.channels),
                    p.priority,
                    p.stackable as i64,
                    p.requires_coupon as i64,
                    p.percent_bp,
                    p.amount_minor,
                    p.price_minor,
                    p.buy_qty,
                    p.get_qty,
                    p.max_uses,
                    p.threshold_minor,
                    now
                ],
            )?;
            write_targets(tx, &p)?;
            let after = promotions::load(tx, &p.promotion_id)?;
            let event = if before.is_some() { "promotion.changed" } else { "promotion.created" };
            audit::record(
                tx,
                &actor,
                event,
                "promotion",
                Some(&p.promotion_id),
                before.as_ref().map(|b| json!(b)).as_ref(),
                Some(&json!(after)),
            )?;
            idempotency::complete(
                tx,
                &req.operation_id,
                "promotions.save",
                Some(&s.user_id),
                Some(&s.device_id),
                &hash,
                Some(&p.promotion_id),
                &json!({ "promotion_id": p.promotion_id }),
            )?;
            Ok(p.promotion_id.clone())
        })?;
        self.promotions_get(token, &id)
    }

    pub fn promotions_set_status(&self, token: &str, req: PromotionStatus) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("promotions.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let id = validate::id(&req.promotion_id, "Promotion")?;
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "promotions.set_status", &req)? {
                Check::Replay { .. } => return Ok(()),
                Check::New { payload_hash } => payload_hash,
            };
            let p = promotions::load(tx, &id)?.ok_or_else(|| AppError::not_found("Promotion"))?;
            if p.version != req.version {
                return Err(AppError::conflict("This offer was changed by someone else. Reload it and try again."));
            }
            if !can_move(&p.status, &req.status) {
                return Err(AppError::conflict(format!("An offer that is {} cannot be moved to {}.", p.status, req.status)));
            }
            if req.status == "active" {
                promotions::validate(&p).map_err(AppError::validation)?;
            }
            tx.execute(
                "UPDATE promotions SET status=?2, updated_at=?3, version=version+1 WHERE promotion_id=?1",
                params![id, req.status, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "promotion.status",
                "promotion",
                Some(&id),
                Some(&json!({ "status": p.status })),
                Some(&json!({ "status": req.status })),
            )?;
            idempotency::complete(
                tx,
                &req.operation_id,
                "promotions.set_status",
                Some(&s.user_id),
                Some(&s.device_id),
                &hash,
                Some(&id),
                &json!({}),
            )?;
            Ok(())
        })?;
        self.promotions_get(token, &id)
    }

    /// What needs a person's attention, each with the screen to fix it:
    /// offers that sell below cost, offers starting soon that cover nothing,
    /// offers ending soon, limited coupons used up, bundles that stock cannot
    /// make. Nothing else (no "insight" without an action).
    pub fn promotions_attention(&self, token: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("promotions.manage") && !s.has("bundles.manage") {
            return Ok(json!({ "items": [] }));
        }
        let show_cost = s.has("products.view_cost");
        self.db.read(|c| {
            let now = self.local_minute(c)?;
            let soon = chrono::NaiveDateTime::parse_from_str(&now, "%Y-%m-%dT%H:%M")
                .map(|t| (t + chrono::Duration::days(3)).format("%Y-%m-%dT%H:%M").to_string())
                .unwrap_or_else(|_| now.clone());
            let mut items = vec![];
            if s.has("promotions.manage") {
                let ids: Vec<String> = {
                    let mut st = c.prepare("SELECT promotion_id FROM promotions WHERE status IN ('active','draft') ORDER BY name, promotion_id")?;
                    let v = st.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                    v
                };
                for id in ids {
                    let Some(p) = promotions::load(c, &id)? else { continue };
                    let link = format!("/admin/promotions?open={id}");
                    let st = state(&p, &now);
                    if p.status == "active" && (st == "running" || st == "scheduled") && show_cost {
                        // Up to 50 covered products (named ones first): enough to flag it.
                        let (_, below, _) = margins(c, &p, &s.branch_id, true, 50)?;
                        if below > 0 {
                            items.push(json!({ "kind": "below_cost", "name": p.name, "count": below, "link": link }));
                        }
                    }
                    let starts_soon = p.starts_at.as_deref().is_some_and(|x| x > now.as_str() && x <= soon.as_str());
                    if starts_soon && promotions::coverage(c, &p)? == 0 {
                        items.push(json!({ "kind": "no_products", "name": p.name, "link": link }));
                    }
                    if p.status == "active" && p.ends_at.as_deref().is_some_and(|x| x > now.as_str() && x <= soon.as_str()) {
                        items.push(json!({ "kind": "ending_soon", "name": p.name, "at": p.ends_at, "link": link }));
                    }
                    if p.status == "draft" && starts_soon {
                        items.push(json!({ "kind": "not_switched_on", "name": p.name, "at": p.starts_at, "link": link }));
                    }
                }
                let mut st = c.prepare(
                    "SELECT c.code, c.promotion_id FROM coupons c WHERE c.active=1 AND c.kind='limited'
                       AND (SELECT COUNT(*) FROM coupon_redemptions r WHERE r.coupon_id=c.coupon_id) >= c.max_redemptions ORDER BY c.code_norm",
                )?;
                for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
                    let (code, pid) = row?;
                    items.push(json!({ "kind": "coupon_used_up", "name": code, "link": format!("/admin/promotions?open={pid}") }));
                }
            }
            if s.has("bundles.manage") {
                let mut st = c.prepare(
                    "SELECT b.bundle_product_id, p.name, b.version FROM bundles b JOIN products p ON p.product_id=b.bundle_product_id
                     WHERE b.active=1 ORDER BY p.name",
                )?;
                let rows: Vec<(String, String, i64)> =
                    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
                for (pid, name, v) in rows {
                    if let Some((0, limit)) = crate::bundles::availability(c, &pid, v, &s.branch_id)? {
                        items.push(json!({ "kind": "bundle_unavailable", "name": name, "limited_by": limit,
                            "link": format!("/admin/bundles?open={pid}") }));
                    }
                }
            }
            Ok(json!({ "items": items }))
        })
    }

    /// 'YYYY-MM-DDTHH:MM' now, in the store's timezone.
    pub(crate) fn local_minute(&self, c: &Connection) -> AppResult<String> {
        Ok(promotions::local_minute(time::now(), &self.store_timezone(c)?))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CouponSave {
    /// Empty to create.
    #[serde(default)]
    pub coupon_id: String,
    pub promotion_id: String,
    pub code: String,
    /// reusable | limited
    pub kind: String,
    #[serde(default)]
    pub max_redemptions: Option<i64>,
    #[serde(default = "yes")]
    pub active: bool,
    #[serde(default)]
    pub version: i64,
    pub operation_id: String,
}

fn yes() -> bool {
    true
}

impl AppCore {
    /// Create a coupon code for a coupon promotion, or switch one on/off.
    /// A code, its kind and its limit never change once created (they are
    /// on receipts and redemptions); switch it off and create another.
    pub fn coupons_save(&self, token: &str, req: CouponSave) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("coupons.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let norm = promotions::normalize_code(&req.code);
        let promotion_id = self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "coupons.save", &req)? {
                Check::Replay { result } => return Ok(result["promotion_id"].as_str().unwrap_or_default().to_string()),
                Check::New { payload_hash } => payload_hash,
            };
            let now = time::now_str();
            let id = if req.coupon_id.is_empty() {
                if norm.is_empty() || norm.chars().count() > 32 || !norm.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                    return Err(AppError::validation("Use 1–32 letters, digits, '-' or '_' for the code."));
                }
                let p = promotions::load(tx, &validate::id(&req.promotion_id, "Promotion")?)?
                    .ok_or_else(|| AppError::not_found("Promotion"))?;
                if !p.requires_coupon {
                    return Err(AppError::validation("Codes are for offers that need a coupon. Turn on 'Needs a coupon' for this offer first."));
                }
                if p.status == "archived" || p.status == "ended" {
                    return Err(AppError::conflict("This offer has ended."));
                }
                let max = match (req.kind.as_str(), req.max_redemptions) {
                    ("reusable", _) => None,
                    ("limited", Some(m)) if (1..=1_000_000).contains(&m) => Some(m),
                    ("limited", _) => return Err(AppError::validation("Enter how many times the code can be used.")),
                    _ => return Err(AppError::validation("Choose reusable or limited.")),
                };
                let taken: bool =
                    tx.query_row("SELECT 1 FROM coupons WHERE code_norm=?1", [&norm], |_| Ok(true)).optional()?.unwrap_or(false);
                if taken {
                    return Err(AppError::new(crate::error::ErrorCode::Duplicate, "This code is already used by another coupon."));
                }
                let id = new_id();
                tx.execute(
                    "INSERT INTO coupons(coupon_id, promotion_id, code, code_norm, kind, max_redemptions, active, created_by, created_at, updated_at, version)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,1)",
                    params![id, p.promotion_id, req.code.trim(), norm, req.kind, max, req.active as i64, s.user_id, now],
                )?;
                audit::record(tx, &actor, "coupon.created", "coupon", Some(&id), None,
                    Some(&json!({ "code": norm, "kind": req.kind, "max_redemptions": max, "promotion_id": p.promotion_id })))?;
                (id, p.promotion_id)
            } else {
                let id = validate::id(&req.coupon_id, "Coupon")?;
                let (pid, active, version): (String, i64, i64) = tx
                    .query_row("SELECT promotion_id, active, version FROM coupons WHERE coupon_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .optional()?
                    .ok_or_else(|| AppError::not_found("Coupon"))?;
                if version != req.version {
                    return Err(AppError::conflict("This coupon was changed by someone else. Reload it and try again."));
                }
                tx.execute(
                    "UPDATE coupons SET active=?2, updated_at=?3, version=version+1 WHERE coupon_id=?1",
                    params![id, req.active as i64, now],
                )?;
                audit::record(tx, &actor, "coupon.changed", "coupon", Some(&id), Some(&json!({ "active": active == 1 })),
                    Some(&json!({ "active": req.active })))?;
                (id, pid)
            };
            idempotency::complete(tx, &req.operation_id, "coupons.save", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id.0),
                &json!({ "coupon_id": id.0, "promotion_id": id.1 }))?;
            Ok(id.1)
        })?;
        self.promotions_get(token, &promotion_id)
    }
}

/// Coverage, the margin preview and conflicts for the editor.
fn insight(c: &Connection, p: &Promotion, branch_id: &str, show_cost: bool) -> AppResult<Value> {
    let coverage = promotions::coverage(c, p)?;
    let (margins, below_cost, negative) = margins(c, p, branch_id, show_cost, 200)?;
    let conflicts = conflicts(c, p)?;
    Ok(json!({ "coverage": coverage, "margins": margins, "below_cost": below_cost, "negative_margin": negative,
        "show_cost": show_cost, "conflicts": conflicts }))
}

/// The offer price and margin of up to `limit` covered products (named
/// ones first), with how many sell below cost or at a negative margin.
fn margins(c: &Connection, p: &Promotion, branch_id: &str, show_cost: bool, limit: usize) -> AppResult<(Vec<Value>, i64, i64)> {
    let mut ids: Vec<String> = p.buy_products.clone();
    if ids.len() < limit {
        for cat in &p.buy_categories {
            let mut st = c.prepare_cached("SELECT product_id FROM products WHERE active=1 AND category_id=?1 ORDER BY name LIMIT ?2")?;
            for r in st.query_map(params![cat, limit as i64], |r| r.get::<_, String>(0))? {
                ids.push(r?);
            }
        }
        if p.target == "all" {
            let mut st = c.prepare_cached("SELECT product_id FROM products WHERE active=1 ORDER BY name LIMIT ?1")?;
            for r in st.query_map([limit as i64], |r| r.get::<_, String>(0))? {
                ids.push(r?);
            }
        }
    }
    ids.dedup();
    ids.truncate(limit);
    let mut margins = vec![];
    let (mut below_cost, mut negative) = (0, 0);
    for pid in &ids {
        let row: Option<(String, i64, i64)> = c
            .query_row(
                "SELECT p.name, t.rate_bp, t.inclusive FROM products p JOIN tax_rules t ON t.tax_rule_id=p.tax_rule_id WHERE p.product_id=?1",
                [pid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((name, rate, incl)) = row else { continue };
        let Some(normal) = crate::catalog::current_price(c, pid)? else { continue };
        let Some(offer) = offer_unit_price(p, normal) else { continue };
        let mut m = json!({ "product_id": pid, "name": name, "normal_minor": normal, "offer_minor": offer });
        if show_cost {
            let cost = crate::inventory::avg_cost(c, pid, branch_id)?;
            let mb = crate::policies::margin_bp(offer, cost, rate, incl == 1);
            let below = cost > 0 && !crate::policies::meets_floor(offer, cost, 0, rate, incl == 1);
            if below {
                below_cost += 1;
            }
            if mb.is_some_and(|x| x < 0) {
                negative += 1;
            }
            m["cost_minor"] = json!(cost);
            m["margin_bp"] = json!(mb);
            m["below_cost"] = json!(below);
        }
        margins.push(m);
    }
    Ok((margins, below_cost, negative))
}

/// Other live or draft offers in the same layer over the same products or
/// categories (or the whole basket), whose schedules overlap.
fn conflicts(c: &Connection, p: &Promotion) -> AppResult<Vec<Value>> {
    let mut conflicts = vec![];
    let mut st =
        c.prepare_cached("SELECT promotion_id FROM promotions WHERE status IN ('draft','active','paused') AND promotion_id<>?1")?;
    let others: Vec<String> = st.query_map([&p.promotion_id], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    for oid in others {
        let Some(o) = promotions::load(c, &oid)? else { continue };
        if o.layer() != p.layer() {
            continue;
        }
        let overlap_targets = p.target == "all"
            || o.target == "all"
            || o.buy_products.iter().any(|x| p.buy_products.contains(x))
            || o.buy_categories.iter().any(|x| p.buy_categories.contains(x));
        let overlap_time = !(o.ends_at.as_deref().zip(p.starts_at.as_deref()).is_some_and(|(e, s)| e <= s)
            || p.ends_at.as_deref().zip(o.starts_at.as_deref()).is_some_and(|(e, s)| e <= s));
        if overlap_targets && overlap_time {
            let rel = match o.priority.cmp(&p.priority) {
                std::cmp::Ordering::Greater => "higher",
                std::cmp::Ordering::Less => "lower",
                std::cmp::Ordering::Equal => "same",
            };
            conflicts.push(
                json!({ "promotion_id": o.promotion_id, "name": o.name, "status": o.status, "priority": o.priority, "relation": rel }),
            );
        }
    }
    Ok(conflicts)
}
