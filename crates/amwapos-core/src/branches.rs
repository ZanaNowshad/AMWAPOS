//! Multi-branch (flag `org.multi_branch`, default off).
//!
//! One hub per LAN keeps one database for every branch it serves; there is no
//! cloud mesh. Each device belongs to exactly one branch (chosen when it is
//! paired). The catalogue is shared, with an optional per-branch price.
//! Stock, sales, shifts and orders carry their branch. A user works in their
//! home branch plus any assigned in `user_branches`; `branches.all` (owner)
//! may work in and report on every branch.
//!
//! Isolation rule: a mutation always happens in the session's branch. A user
//! who may work in several branches switches the session's branch first, and
//! the till only sells in the device's own branch. With the flag off none of
//! these checks apply and everything behaves as a single branch.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult};
use crate::service::AppCore;
use crate::setup::{clean, clean_opt, validate_code};
use crate::time;
use crate::validate;

pub fn multi_on(c: &Connection) -> AppResult<bool> {
    Ok(crate::settings::get::<crate::settings::FeatureFlags>(c, crate::settings::KEY_FEATURES)?.is_on("org.multi_branch"))
}

/// May this user work in `branch_id`?
pub fn user_may_work_in(c: &Connection, user_id: &str, all_branches: bool, branch_id: &str) -> AppResult<bool> {
    if all_branches {
        return Ok(true);
    }
    Ok(c.query_row(
        "SELECT 1 FROM users WHERE user_id=?1 AND branch_id=?2
         UNION SELECT 1 FROM user_branches WHERE user_id=?1 AND branch_id=?2",
        params![user_id, branch_id],
        |_| Ok(true),
    )
    .optional()?
    .unwrap_or(false))
}

/// Mutations on another branch's records are refused (flag on only).
pub fn require_branch(c: &Connection, s: &Session, branch_id: &str) -> AppResult<()> {
    if branch_id == s.branch_id || !multi_on(c)? {
        return Ok(());
    }
    Err(AppError::forbidden("branches").with_details(json!({ "kind": "other_branch", "branch_id": branch_id })))
}

/// Branch filter for lists and reports: `None` means every branch (flag off,
/// or the user may see all branches).
pub fn list_scope(c: &Connection, s: &Session) -> AppResult<Option<String>> {
    if !multi_on(c)? || s.has("branches.all") {
        return Ok(None);
    }
    Ok(Some(s.branch_id.clone()))
}

/// Report filter: an explicit branch the user may see, else their scope.
pub fn report_scope(c: &Connection, s: &Session, requested: Option<&str>) -> AppResult<Option<String>> {
    if !multi_on(c)? {
        return Ok(None);
    }
    match requested.filter(|b| !b.is_empty()) {
        Some(b) => {
            let b = validate::id(b, "Branch")?;
            if s.has("branches.all") || user_may_work_in(c, &s.user_id, false, &b)? {
                Ok(Some(b))
            } else {
                Err(AppError::forbidden("branches.all"))
            }
        }
        None => list_scope(c, s),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BranchRow {
    pub branch_id: String,
    pub code: String,
    pub name: String,
    pub address: Option<String>,
    pub phone: Option<String>,
    pub active: bool,
    pub devices: Vec<BranchDevice>,
    pub user_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BranchDevice {
    pub device_id: String,
    pub name: String,
    pub device_code: String,
    pub mode: String,
    pub active: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BranchInput {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default = "yes")]
    pub active: bool,
}
fn yes() -> bool {
    true
}

fn load_branches(c: &Connection, only: Option<&[String]>) -> AppResult<Vec<BranchRow>> {
    let mut st = c.prepare(
        "SELECT b.branch_id, b.code, b.name, b.address, b.phone, b.active,
                (SELECT COUNT(*) FROM users u WHERE u.active=1 AND (u.branch_id=b.branch_id
                   OR EXISTS (SELECT 1 FROM user_branches ub WHERE ub.user_id=u.user_id AND ub.branch_id=b.branch_id)))
         FROM branches b ORDER BY b.code",
    )?;
    let mut rows: Vec<BranchRow> = st
        .query_map([], |r| {
            Ok(BranchRow {
                branch_id: r.get(0)?,
                code: r.get(1)?,
                name: r.get(2)?,
                address: r.get(3)?,
                phone: r.get(4)?,
                active: r.get::<_, i64>(5)? == 1,
                devices: vec![],
                user_count: r.get(6)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    if let Some(ids) = only {
        rows.retain(|b| ids.contains(&b.branch_id));
    }
    let mut ds = c.prepare("SELECT device_id, name, device_code, operating_mode, active FROM devices WHERE branch_id=?1 ORDER BY name")?;
    for b in rows.iter_mut() {
        b.devices = ds
            .query_map([&b.branch_id], |r| {
                Ok(BranchDevice {
                    device_id: r.get(0)?,
                    name: r.get(1)?,
                    device_code: r.get(2)?,
                    mode: r.get(3)?,
                    active: r.get::<_, i64>(4)? == 1,
                })
            })?
            .collect::<Result<_, _>>()?;
    }
    Ok(rows)
}

fn user_branch_ids(c: &Connection, user_id: &str) -> AppResult<Vec<String>> {
    let mut st = c.prepare(
        "SELECT branch_id FROM users WHERE user_id=?1 AND branch_id IS NOT NULL
         UNION SELECT branch_id FROM user_branches WHERE user_id=?1",
    )?;
    let ids = st.query_map([user_id], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
    Ok(ids)
}

impl AppCore {
    /// Branches this session may see (all of them for `branches.manage` /
    /// `branches.all`; otherwise the user's own). Works with the flag off so
    /// the admin can see the single branch.
    pub fn branches_list(&self, token: &str) -> AppResult<Vec<BranchRow>> {
        let s = self.session(token)?;
        self.db.read(|c| {
            if s.has("branches.manage") || s.has("branches.all") {
                load_branches(c, None)
            } else {
                let ids = user_branch_ids(c, &s.user_id)?;
                load_branches(c, Some(&ids))
            }
        })
    }

    pub fn branch_save(&self, token: &str, branch_id: Option<String>, input: BranchInput) -> AppResult<Vec<BranchRow>> {
        let s = self.session(token)?;
        self.require_feature("org.multi_branch")?;
        s.require("branches.manage")?;
        self.require_back_office_writable()?;
        let code = validate_code(&input.code, "Branch code")?;
        let name = clean(&input.name, "Branch name", 80, true)?;
        let address = clean_opt(&input.address, "Address", 300)?;
        let phone = clean_opt(&input.phone, "Phone", 30)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let now = time::now_str();
            let dup: Option<String> = tx.query_row("SELECT branch_id FROM branches WHERE code=?1", [&code], |r| r.get(0)).optional()?;
            match branch_id.as_ref().filter(|x| !x.is_empty()) {
                Some(id) => {
                    let id = validate::id(id, "Branch")?;
                    let before: (String, String, i64) = tx
                        .query_row("SELECT code, name, active FROM branches WHERE branch_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Branch"))?;
                    if dup.as_deref().is_some_and(|d| d != id) {
                        return Err(AppError::duplicate(format!("Branch code {code} is already used.")));
                    }
                    if !input.active {
                        let devices: i64 =
                            tx.query_row("SELECT COUNT(*) FROM devices WHERE branch_id=?1 AND active=1", [&id], |r| r.get(0))?;
                        if devices > 0 {
                            return Err(AppError::conflict("This branch still has active devices. Revoke or move them first."));
                        }
                    }
                    tx.execute(
                        "UPDATE branches SET code=?2, name=?3, address=?4, phone=?5, active=?6, updated_at=?7 WHERE branch_id=?1",
                        params![id, code, name, address, phone, input.active as i64, now],
                    )?;
                    audit::record(
                        tx,
                        &actor,
                        "branch.updated",
                        "branch",
                        Some(&id),
                        Some(&json!({ "code": before.0, "name": before.1, "active": before.2 == 1 })),
                        Some(&json!({ "code": code, "name": name, "active": input.active })),
                    )?;
                }
                None => {
                    if dup.is_some() {
                        return Err(AppError::duplicate(format!("Branch code {code} is already used.")));
                    }
                    let id = crate::ids::new_id();
                    tx.execute(
                        "INSERT INTO branches(branch_id, code, name, address, phone, active, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
                        params![id, code, name, address, phone, input.active as i64, now],
                    )?;
                    crate::transfers::ensure_default_location(tx, &id)?;
                    audit::record(tx, &actor, "branch.created", "branch", Some(&id), None, Some(&json!({ "code": code, "name": name })))?;
                }
            }
            load_branches(tx, None)
        })
    }

    /// Set the extra branches a user may work in (the home branch stays).
    pub fn user_branches_set(&self, token: &str, user_id: &str, branch_ids: Vec<String>) -> AppResult<Vec<String>> {
        let s = self.session(token)?;
        self.require_feature("org.multi_branch")?;
        s.require("branches.manage")?;
        self.require_back_office_writable()?;
        let uid = validate::id(user_id, "User")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let before = user_branch_ids(tx, &uid)?;
            let home: Option<String> = tx
                .query_row("SELECT branch_id FROM users WHERE user_id=?1", [&uid], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("User"))?;
            tx.execute("DELETE FROM user_branches WHERE user_id=?1", [&uid])?;
            for b in &branch_ids {
                let b = validate::id(b, "Branch")?;
                if Some(&b) == home.as_ref() {
                    continue;
                }
                tx.query_row("SELECT 1 FROM branches WHERE branch_id=?1", [&b], |_| Ok(()))
                    .optional()?
                    .ok_or_else(|| AppError::not_found("Branch"))?;
                tx.execute("INSERT OR IGNORE INTO user_branches(user_id, branch_id) VALUES (?1,?2)", params![uid, b])?;
            }
            let after = user_branch_ids(tx, &uid)?;
            audit::record(
                tx,
                &actor,
                "user.branches",
                "user",
                Some(&uid),
                Some(&json!({ "branches": before })),
                Some(&json!({ "branches": after })),
            )?;
            Ok(after)
        })
    }

    pub fn user_branches_get(&self, token: &str, user_id: &str) -> AppResult<Vec<String>> {
        let s = self.session(token)?;
        s.require("users.manage").or_else(|_| s.require("branches.manage"))?;
        let uid = validate::id(user_id, "User")?;
        self.db.read(|c| user_branch_ids(c, &uid))
    }

    /// Work in another branch for back-office tasks (stock, transfers,
    /// orders, reports). The till still sells only in the device's branch.
    pub fn session_switch_branch(&self, token: &str, branch_id: &str) -> AppResult<Session> {
        let s = self.session(token)?;
        self.require_feature("org.multi_branch")?;
        let b = validate::id(branch_id, "Branch")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let active: i64 = tx
                .query_row("SELECT active FROM branches WHERE branch_id=?1", [&b], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Branch"))?;
            if active != 1 {
                return Err(AppError::conflict("This branch is inactive."));
            }
            if !user_may_work_in(tx, &s.user_id, s.has("branches.all"), &b)? {
                return Err(AppError::forbidden("branches"));
            }
            audit::record(tx, &actor, "session.branch_switched", "branch", Some(&b), Some(&json!({ "branch_id": s.branch_id })), None)?;
            Ok(())
        })?;
        self.sessions.set_branch(token, &b)
    }

    /// Per-branch retail price override (None removes it). The shared
    /// catalogue price applies wherever no override exists.
    pub fn branch_price_set(
        &self,
        token: &str,
        product_id: &str,
        branch_id: &str,
        amount_minor: Option<i64>,
    ) -> AppResult<Vec<serde_json::Value>> {
        let s = self.session(token)?;
        self.require_feature("org.multi_branch")?;
        s.require("prices.manage")?;
        self.require_back_office_writable()?;
        let pid = validate::id(product_id, "Product")?;
        let b = validate::id(branch_id, "Branch")?;
        if let Some(a) = amount_minor {
            validate::money_non_negative(a, "Price")?;
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            tx.query_row("SELECT 1 FROM products WHERE product_id=?1", [&pid], |_| Ok(())).optional()?.ok_or_else(|| AppError::not_found("Product"))?;
            tx.query_row("SELECT 1 FROM branches WHERE branch_id=?1", [&b], |_| Ok(())).optional()?.ok_or_else(|| AppError::not_found("Branch"))?;
            let now = time::now_str();
            let before: Option<i64> = tx
                .query_row(
                    "SELECT amount_minor FROM product_prices WHERE product_id=?1 AND branch_id=?2 AND price_type='retail'
                     AND (effective_to IS NULL OR effective_to > ?3) ORDER BY effective_from DESC LIMIT 1",
                    params![pid, b, now],
                    |r| r.get(0),
                )
                .optional()?;
            tx.execute(
                "UPDATE product_prices SET effective_to=?3 WHERE product_id=?1 AND branch_id=?2 AND price_type='retail'
                 AND (effective_to IS NULL OR effective_to > ?3)",
                params![pid, b, now],
            )?;
            if let Some(a) = amount_minor {
                tx.execute(
                    "INSERT INTO product_prices(price_id, product_id, branch_id, price_type, amount_minor, effective_from, reason, created_by, created_at)
                     VALUES (?1,?2,?3,'retail',?4,?5,'Branch price',?6,?5)",
                    params![crate::ids::new_id(), pid, b, a, now, s.user_id],
                )?;
            }
            tx.execute("UPDATE products SET updated_at=?2, version=version+1 WHERE product_id=?1", params![pid, now])?;
            audit::record(
                tx,
                &actor,
                "price.branch_changed",
                "product",
                Some(&pid),
                Some(&json!({ "branch_id": b, "price_minor": before })),
                Some(&json!({ "branch_id": b, "price_minor": amount_minor })),
            )?;
            branch_prices(tx, &pid)
        })
    }

    pub fn branch_prices_get(&self, token: &str, product_id: &str) -> AppResult<Vec<serde_json::Value>> {
        let s = self.session(token)?;
        s.require("products.view").or_else(|_| s.require("prices.manage"))?;
        let pid = validate::id(product_id, "Product")?;
        self.db.read(|c| branch_prices(c, &pid))
    }
}

fn branch_prices(c: &Connection, pid: &str) -> AppResult<Vec<serde_json::Value>> {
    let now = time::now_str();
    let mut st = c.prepare(
        "SELECT b.branch_id, b.code, b.name,
                (SELECT amount_minor FROM product_prices pp WHERE pp.product_id=?1 AND pp.branch_id=b.branch_id AND pp.price_type='retail'
                   AND pp.effective_from <= ?2 AND (pp.effective_to IS NULL OR pp.effective_to > ?2) ORDER BY pp.effective_from DESC LIMIT 1)
         FROM branches b WHERE b.active=1 ORDER BY b.code",
    )?;
    let rows = st
        .query_map(params![pid, now], |r| {
            Ok(json!({ "branch_id": r.get::<_, String>(0)?, "code": r.get::<_, String>(1)?, "name": r.get::<_, String>(2)?,
                       "price_minor": r.get::<_, Option<i64>>(3)? }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}
