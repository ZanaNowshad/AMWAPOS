//! Owner companion view (flag `pwa.companion`, default off).
//!
//! A read-only page the owner opens on a phone on the store LAN. The hub
//! serves it; there is no checkout, no drawer and no admin. Access needs a
//! short-lived token issued from the admin screen (stored here only as a
//! SHA-256 hash, revocable). HTTPS is not used on the LAN, so the token is
//! the only credential and it expires within a day.

use std::collections::HashSet;

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::audit;
use crate::auth::{self, Session};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::service::AppCore;
use crate::setup::clean_opt;
use crate::time;

pub const MAX_HOURS: i64 = 24;
pub const DEFAULT_HOURS: i64 = 12;

impl AppCore {
    fn companion_admin(&self, token: &str) -> AppResult<Session> {
        let s = self.session(token)?;
        self.require_feature("pwa.companion")?;
        s.require("reports.financial")?;
        Ok(s)
    }

    /// Issue a phone token. Returned once; only its hash is kept.
    pub fn companion_issue(&self, token: &str, label: Option<String>, hours: Option<i64>) -> AppResult<Value> {
        let s = self.companion_admin(token)?;
        let d = self.require_device()?;
        if d.mode != "hub" {
            return Err(AppError::conflict(
                "The phone view is served by the hub. Turn this computer into the hub first (Admin → Sync / Hub).",
            ));
        }
        let hours = hours.unwrap_or(DEFAULT_HOURS).clamp(1, MAX_HOURS);
        let label = clean_opt(&label, "Label", 60)?;
        let secret = auth::random_token();
        let hash = auth::sha256_hex(&secret);
        let now = time::now();
        let expires = time::fmt(now + chrono::Duration::hours(hours));
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            tx.execute(
                "INSERT INTO companion_tokens(token_hash, user_id, label, created_at, expires_at) VALUES (?1,?2,?3,?4,?5)",
                params![hash, s.user_id, label, time::fmt(now), expires],
            )?;
            audit::record(
                tx,
                &actor,
                "companion.token_issued",
                "user",
                Some(&s.user_id),
                None,
                Some(&json!({ "expires_at": expires, "label": label })),
            )?;
            Ok(())
        })?;
        Ok(json!({ "token": secret, "id": hash, "expires_at": expires, "path": "/companion/" }))
    }

    pub fn companion_tokens(&self, token: &str) -> AppResult<Vec<Value>> {
        let _s = self.companion_admin(token)?;
        let now = time::now_str();
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT t.token_hash, u.display_name, t.label, t.created_at, t.expires_at, t.last_used_at FROM companion_tokens t
                 LEFT JOIN users u ON u.user_id=t.user_id WHERE t.revoked_at IS NULL AND t.expires_at > ?1 ORDER BY t.created_at DESC",
            )?;
            let rows = st
                .query_map([&now], |r| {
                    Ok(json!({ "id": r.get::<_, String>(0)?, "user_name": r.get::<_, Option<String>>(1)?, "label": r.get::<_, Option<String>>(2)?,
                               "created_at": r.get::<_, String>(3)?, "expires_at": r.get::<_, String>(4)?, "last_used_at": r.get::<_, Option<String>>(5)? }))
                })?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }

    pub fn companion_revoke(&self, token: &str, id: &str) -> AppResult<Vec<Value>> {
        let s = self.companion_admin(token)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let n = tx.execute(
                "UPDATE companion_tokens SET revoked_at=?2 WHERE token_hash=?1 AND revoked_at IS NULL",
                params![id, time::now_str()],
            )?;
            if n == 0 {
                return Err(AppError::not_found("Phone access"));
            }
            audit::record(tx, &actor, "companion.token_revoked", "user", Some(&s.user_id), None, None)?;
            Ok(())
        })?;
        self.companion_tokens(token)
    }

    /// The phone's read-only snapshot, for a valid bearer token.
    pub fn companion_snapshot(&self, bearer: &str) -> AppResult<Value> {
        let denied =
            || AppError::new(ErrorCode::Unauthenticated, "This phone link has expired or was revoked. Issue a new one on the hub.");
        let f = self.features()?;
        if !f.is_on("pwa.companion") {
            return Err(AppError::new(ErrorCode::Forbidden, "The phone view is turned off.")
                .with_details(json!({ "kind": "feature_disabled", "feature": "pwa.companion" })));
        }
        if bearer.len() < 20 || bearer.len() > 200 {
            return Err(denied());
        }
        let d = self.require_device()?;
        let hash = auth::sha256_hex(bearer);
        let now = time::now_str();
        let user_id: String = self
            .db
            .write(|tx| {
                let u: Option<String> = tx
                    .query_row(
                        "SELECT t.user_id FROM companion_tokens t JOIN users u ON u.user_id=t.user_id
                         WHERE t.token_hash=?1 AND t.revoked_at IS NULL AND t.expires_at > ?2 AND u.active=1",
                        params![hash, now],
                        |r| r.get(0),
                    )
                    .optional()?;
                if u.is_some() {
                    tx.execute("UPDATE companion_tokens SET last_used_at=?2 WHERE token_hash=?1", params![hash, now])?;
                }
                Ok(u)
            })?
            .ok_or_else(denied)?;
        let (user, perms): (auth::UserAuthRow, HashSet<String>) = self.db.read(|c| {
            let u = auth::load_user_auth(c, &user_id)?;
            let p = auth::role_permissions(c, &u.role_id)?;
            Ok((u, p))
        })?;
        if !perms.contains("reports.financial") {
            return Err(denied());
        }
        let n = time::now();
        let s = Session {
            token: String::new(),
            user_id: user.user_id,
            display_name: user.display_name.clone(),
            role_id: user.role_id,
            role_name: user.role_name,
            permissions: perms,
            device_id: d.device_id.clone(),
            branch_id: d.branch_id.clone(),
            created_at: n,
            last_activity: n,
            locked: false,
        };
        let scope = self.db.read(|c| crate::branches::list_scope(c, &s))?;
        let dashboard = crate::db::with_report_branch(scope, || self.dashboard_for(&s))?;
        let (deliveries, low_stock, business) = self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT d.delivery_number, d.status, COALESCE(cu.name, d.phone), d.area, d.amount_minor, d.payment_status, d.created_at
                 FROM delivery_orders d LEFT JOIN customers cu ON cu.customer_id=d.customer_id
                 WHERE d.status IN ('pending','preparing','dispatched') ORDER BY d.created_at LIMIT 50",
            )?;
            let del: Vec<Value> = st
                .query_map([], |r| {
                    Ok(json!({ "number": r.get::<_, String>(0)?, "status": r.get::<_, String>(1)?, "customer": r.get::<_, Option<String>>(2)?,
                               "area": r.get::<_, Option<String>>(3)?, "amount_minor": r.get::<_, i64>(4)?, "payment": r.get::<_, String>(5)?,
                               "created_at": r.get::<_, String>(6)? }))
                })?
                .collect::<Result<_, _>>()?;
            let low = crate::eod::low_stock(c, &d.branch_id)?;
            let business: Option<String> =
                c.query_row("SELECT name FROM business LIMIT 1", [], |r| r.get(0)).optional()?;
            Ok((del, low, business))
        })?;
        let (currency, digits) = self.db.read(|c| self.currency(c))?;
        Ok(json!({
            "generated_at": now,
            "business_name": business,
            "viewer": user.display_name,
            "currency": currency,
            "digits": digits,
            "dashboard": dashboard,
            "deliveries": deliveries,
            "low_stock": low_stock.into_iter().take(50).collect::<Vec<_>>(),
        }))
    }
}
