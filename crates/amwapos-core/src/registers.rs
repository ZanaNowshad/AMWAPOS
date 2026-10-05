//! Registers and cash drawers.
//!
//! A register is the checkout station as a business concept ("Till 1"); a
//! drawer is the cash a person is responsible for. A register points at the
//! computer that currently stands for it, so a replaced computer can take over
//! an existing register. Every computer gets a register and a drawer when it
//! is added (migration 0028). Shifts opened from then on record both; older
//! shifts have neither, because nobody recorded it at the time.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::ids::new_id;
use crate::service::AppCore;
use crate::setup::clean;
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Serialize)]
pub struct DrawerRow {
    pub drawer_id: String,
    pub name: String,
    pub active: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegisterRow {
    pub register_id: String,
    pub branch_id: String,
    pub branch_name: String,
    pub code: String,
    pub name: String,
    pub active: bool,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub drawers: Vec<DrawerRow>,
    /// The shift open on this register now, if any: number and cashier.
    pub open_shift: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegisterInput {
    pub name: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub branch_id: Option<String>,
    /// The computer that stands for this register now (None = none).
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default = "yes")]
    pub active: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DrawerInput {
    pub register_id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub active: bool,
    #[serde(default)]
    pub make_default: bool,
}

fn yes() -> bool {
    true
}

/// The register this computer stands for, and its default drawer.
pub(crate) fn for_device(c: &Connection, device_id: &str) -> AppResult<(Option<String>, Option<String>)> {
    Ok(c.query_row(
        "SELECT r.register_id, d.drawer_id FROM registers r
             LEFT JOIN cash_drawers d ON d.drawer_id=r.default_drawer_id AND d.active=1
             WHERE r.device_id=?1 AND r.active=1",
        [device_id],
        |r| Ok((Some(r.get::<_, String>(0)?), r.get::<_, Option<String>>(1)?)),
    )
    .optional()?
    .unwrap_or((None, None)))
}

pub(crate) fn list(c: &Connection, branch: Option<&str>) -> AppResult<Vec<RegisterRow>> {
    let mut st = c.prepare(
        "SELECT r.register_id, r.branch_id, COALESCE(b.name,''), r.code, r.name, r.active, r.device_id, dv.name, r.default_drawer_id
         FROM registers r LEFT JOIN branches b ON b.branch_id=r.branch_id LEFT JOIN devices dv ON dv.device_id=r.device_id
         WHERE (?1 IS NULL OR r.branch_id=?1) ORDER BY r.active DESC, r.code",
    )?;
    let rows = st
        .query_map(params![branch], |r| {
            Ok((
                RegisterRow {
                    register_id: r.get(0)?,
                    branch_id: r.get(1)?,
                    branch_name: r.get(2)?,
                    code: r.get(3)?,
                    name: r.get(4)?,
                    active: r.get::<_, i64>(5)? == 1,
                    device_id: r.get(6)?,
                    device_name: r.get(7)?,
                    drawers: vec![],
                    open_shift: None,
                },
                r.get::<_, Option<String>>(8)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = vec![];
    for (mut reg, default) in rows {
        let mut st = c.prepare("SELECT drawer_id, name, active FROM cash_drawers WHERE register_id=?1 ORDER BY name")?;
        reg.drawers = st
            .query_map([&reg.register_id], |r| {
                let id: String = r.get(0)?;
                Ok(DrawerRow {
                    is_default: Some(&id) == default.as_ref(),
                    drawer_id: id,
                    name: r.get(1)?,
                    active: r.get::<_, i64>(2)? == 1,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        reg.open_shift = c
            .query_row(
                "SELECT s.shift_id, s.shift_number, COALESCE(u.display_name,''), s.opened_at FROM shifts s LEFT JOIN users u ON u.user_id=s.user_id
                 WHERE s.register_id=?1 AND s.status='open' ORDER BY s.opened_at DESC LIMIT 1",
                [&reg.register_id],
                |r| {
                    Ok(json!({ "shift_id": r.get::<_, String>(0)?, "shift_number": r.get::<_, String>(1)?,
                               "cashier_name": r.get::<_, String>(2)?, "opened_at": r.get::<_, String>(3)? }))
                },
            )
            .optional()?;
        out.push(reg);
    }
    Ok(out)
}

impl AppCore {
    pub fn registers_list(&self, token: &str) -> AppResult<Vec<RegisterRow>> {
        let s = self.session(token)?;
        if !s.has("registers.manage") && !s.has("day.x_report") {
            return Err(AppError::forbidden("registers.manage"));
        }
        self.db.read(|c| {
            let scope = crate::branches::list_scope(c, &s)?;
            list(c, scope.as_deref())
        })
    }

    /// The active computers a register can stand for.
    pub fn register_devices(&self, token: &str) -> AppResult<Vec<serde_json::Value>> {
        let s = self.session(token)?;
        s.require("registers.manage")?;
        self.db.read(|c| {
            let mut st = c.prepare("SELECT device_id, name, device_code FROM devices WHERE active=1 ORDER BY device_code")?;
            let rows = st
                .query_map([], |r| {
                    Ok(json!({ "device_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "device_code": r.get::<_, String>(2)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Add or change a register. Pointing it at a computer releases that
    /// computer's previous register (one computer stands for one register).
    pub fn register_save(&self, token: &str, register_id: Option<String>, input: RegisterInput) -> AppResult<Vec<RegisterRow>> {
        let s = self.session(token)?;
        s.require("registers.manage")?;
        self.require_back_office_writable()?;
        let name = clean(&input.name, "Name", 40, true)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let now = time::now_str();
            let device = match input.device_id.as_deref().filter(|d| !d.is_empty()) {
                Some(d) => {
                    let d = validate::id(d, "Computer")?;
                    let ok: Option<i64> = tx.query_row("SELECT active FROM devices WHERE device_id=?1", [&d], |r| r.get(0)).optional()?;
                    if ok != Some(1) {
                        return Err(AppError::validation("That computer is not active."));
                    }
                    Some(d)
                }
                None => None,
            };
            let (id, before) = match register_id.as_deref().filter(|x| !x.is_empty()) {
                Some(rid) => {
                    let rid = validate::id(rid, "Register")?;
                    let before: Option<(String, String, Option<String>, i64)> = tx
                        .query_row("SELECT name, code, device_id, active FROM registers WHERE register_id=?1", [&rid], |r| {
                            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                        })
                        .optional()?;
                    let before = before.ok_or_else(|| AppError::not_found("Register"))?;
                    if !input.active {
                        let open: bool =
                            tx.query_row("SELECT 1 FROM shifts WHERE register_id=?1 AND status='open'", [&rid], |_| Ok(true)).optional()?.is_some();
                        if open {
                            return Err(AppError::conflict("Close the shift on this register before switching it off."));
                        }
                    }
                    (rid, Some(json!({ "name": before.0, "code": before.1, "device_id": before.2, "active": before.3 == 1 })))
                }
                None => (new_id(), None),
            };
            let code = match input.code.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
                Some(c) => clean(c, "Code", 12, true)?.to_uppercase(),
                None => match &before {
                    Some(b) => b["code"].as_str().unwrap_or_default().to_string(),
                    None => {
                        let n: i64 = tx.query_row("SELECT COUNT(*) FROM registers", [], |r| r.get(0))?;
                        format!("R{:02}", n + 1)
                    }
                },
            };
            let taken: bool = tx
                .query_row("SELECT 1 FROM registers WHERE code=?1 AND register_id<>?2", params![code, id], |_| Ok(true))
                .optional()?
                .is_some();
            if taken {
                return Err(AppError::conflict("Another register already uses that code."));
            }
            let device = if input.active { device } else { None };
            if let Some(d) = &device {
                // The computer leaves the register it stood for until now.
                tx.execute("UPDATE registers SET device_id=NULL, updated_at=?3 WHERE device_id=?1 AND register_id<>?2", params![d, id, now])?;
            }
            match &before {
                Some(_) => {
                    tx.execute(
                        "UPDATE registers SET name=?2, code=?3, device_id=?4, active=?5, updated_at=?6 WHERE register_id=?1",
                        params![id, name, code, device, input.active as i64, now],
                    )?;
                }
                None => {
                    let branch = match input.branch_id.as_deref().filter(|b| !b.is_empty()) {
                        Some(b) => validate::id(b, "Branch")?,
                        None => s.branch_id.clone(),
                    };
                    let drawer = new_id();
                    tx.execute(
                        "INSERT INTO cash_drawers(drawer_id, branch_id, register_id, name, active, created_at, updated_at) VALUES (?1,?2,?3,?4,1,?5,?5)",
                        params![drawer, branch, id, format!("Drawer {code}"), now],
                    )?;
                    tx.execute(
                        "INSERT INTO registers(register_id, branch_id, code, name, active, device_id, default_drawer_id, created_at, updated_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
                        params![id, branch, code, name, input.active as i64, device, drawer, now],
                    )?;
                }
            }
            let after = json!({ "name": name, "code": code, "device_id": device, "active": input.active });
            audit::record(tx, &actor, if before.is_some() { "register.updated" } else { "register.created" }, "register", Some(&id), before.as_ref(), Some(&after))?;
            Ok(())
        })?;
        self.registers_list(token)
    }

    /// Add or change a drawer on a register.
    pub fn drawer_save(&self, token: &str, drawer_id: Option<String>, input: DrawerInput) -> AppResult<Vec<RegisterRow>> {
        let s = self.session(token)?;
        s.require("registers.manage")?;
        self.require_back_office_writable()?;
        let name = clean(&input.name, "Name", 40, true)?;
        let rid = validate::id(&input.register_id, "Register")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let now = time::now_str();
            let (branch, default): (String, Option<String>) = tx
                .query_row("SELECT branch_id, default_drawer_id FROM registers WHERE register_id=?1", [&rid], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| AppError::not_found("Register"))?;
            let id = match drawer_id.as_deref().filter(|x| !x.is_empty()) {
                Some(d) => {
                    let d = validate::id(d, "Drawer")?;
                    let n = tx.execute(
                        "UPDATE cash_drawers SET name=?2, active=?3, updated_at=?4 WHERE drawer_id=?1 AND register_id=?5",
                        params![d, name, input.active as i64, now, rid],
                    )?;
                    if n == 0 {
                        return Err(AppError::not_found("Drawer"));
                    }
                    d
                }
                None => {
                    let d = new_id();
                    tx.execute(
                        "INSERT INTO cash_drawers(drawer_id, branch_id, register_id, name, active, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?6)",
                        params![d, branch, rid, name, input.active as i64, now],
                    )?;
                    d
                }
            };
            if input.make_default && input.active {
                tx.execute("UPDATE registers SET default_drawer_id=?2, updated_at=?3 WHERE register_id=?1", params![rid, id, now])?;
            } else if !input.active && default.as_deref() == Some(id.as_str()) {
                return Err(AppError::conflict("Choose another drawer for this register before switching this one off."));
            }
            audit::record(tx, &actor, "drawer.saved", "cash_drawer", Some(&id), None, Some(&json!({ "register_id": rid, "name": name, "active": input.active, "default": input.make_default })))?;
            Ok(())
        })?;
        self.registers_list(token)
    }
}
