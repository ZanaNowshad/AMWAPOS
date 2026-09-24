//! Staff users, roles and the permission matrix.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::auth::{self, PERMISSIONS, ROLE_OWNER};
use crate::error::{AppError, AppResult};
use crate::ids::new_id;
use crate::service::AppCore;
use crate::settings;
use crate::setup::clean;
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Serialize)]
pub struct UserRow {
    pub user_id: String,
    pub display_name: String,
    pub role_id: String,
    pub role_name: String,
    pub active: bool,
    pub last_login_at: Option<String>,
    pub locked_until: Option<String>,
    pub failed_attempts: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserInput {
    pub display_name: String,
    pub role_id: String,
    #[serde(default)]
    pub pin: Option<String>,
    #[serde(default = "yes")]
    pub active: bool,
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleRow {
    pub role_id: String,
    pub name: String,
    pub description: Option<String>,
    pub is_system: bool,
    pub user_count: i64,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionRow {
    pub code: String,
    pub domain: String,
    pub description: String,
}

fn load_user(c: &rusqlite::Connection, id: &str) -> AppResult<UserRow> {
    c.query_row(
        "SELECT u.user_id, u.display_name, u.role_id, r.name, u.active, s.last_login_at, s.locked_until, COALESCE(s.failed_attempts,0), u.created_at
         FROM users u JOIN roles r ON r.role_id=u.role_id LEFT JOIN user_login_state s ON s.user_id=u.user_id WHERE u.user_id=?1",
        [id],
        |r| {
            Ok(UserRow {
                user_id: r.get(0)?,
                display_name: r.get(1)?,
                role_id: r.get(2)?,
                role_name: r.get(3)?,
                active: r.get::<_, i64>(4)? == 1,
                last_login_at: r.get(5)?,
                locked_until: r.get(6)?,
                failed_attempts: r.get(7)?,
                created_at: r.get(8)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("User"))
}

fn owner_count(c: &rusqlite::Connection, except: &str) -> AppResult<i64> {
    Ok(c.query_row(
        "SELECT COUNT(*) FROM users WHERE role_id=?1 AND active=1 AND user_id<>?2",
        params![ROLE_OWNER, except],
        |r| r.get(0),
    )?)
}

impl AppCore {
    pub fn users_list(&self, token: &str) -> AppResult<Vec<UserRow>> {
        let s = self.session(token)?;
        s.require("users.manage")?;
        self.db.read(|c| {
            let mut st = c.prepare("SELECT user_id FROM users ORDER BY active DESC, display_name COLLATE NOCASE")?;
            let ids = st.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            ids.iter().map(|id| load_user(c, id)).collect()
        })
    }

    pub fn user_create(&self, token: &str, input: UserInput) -> AppResult<UserRow> {
        let s = self.session(token)?;
        s.require("users.manage")?;
        self.require_back_office_writable()?;
        let name = clean(&input.display_name, "Name", 80, true)?;
        let role = validate::id(&input.role_id, "Role")?;
        if role == ROLE_OWNER && s.role_id != ROLE_OWNER {
            return Err(AppError::forbidden("roles.manage"));
        }
        let pin = input.pin.clone().ok_or_else(|| AppError::validation("A PIN is required for a new user."))?;
        let sec: settings::SecuritySettings = self.db.read(|c| settings::get(c, settings::KEY_SECURITY))?;
        auth::validate_pin(&pin, sec.pin_min_length, sec.pin_max_length)?;
        let hash = auth::hash_pin(&pin)?;
        let actor = self.actor(&s, None);
        let id = self.db.write(|tx| {
            let ok: bool = tx.query_row("SELECT 1 FROM roles WHERE role_id=?1", [&role], |_| Ok(true)).optional()?.unwrap_or(false);
            if !ok {
                return Err(AppError::validation("The selected role does not exist."));
            }
            let dup: bool = tx
                .query_row("SELECT 1 FROM users WHERE display_name=?1 COLLATE NOCASE AND active=1", [&name], |_| Ok(true))
                .optional()?
                .unwrap_or(false);
            if dup {
                return Err(AppError::duplicate(format!("An active user named {name} already exists.")));
            }
            let id = new_id();
            let now = time::now_str();
            tx.execute(
                "INSERT INTO users(user_id, branch_id, display_name, pin_hash, role_id, active, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
                params![id, s.branch_id, name, hash, role, input.active as i64, now],
            )?;
            audit::record(tx, &actor, "user.created", "user", Some(&id), None, Some(&json!({ "name": name, "role_id": role })))?;
            Ok(id)
        })?;
        self.db.read(|c| load_user(c, &id))
    }

    pub fn user_update(&self, token: &str, user_id: &str, input: UserInput) -> AppResult<UserRow> {
        let s = self.session(token)?;
        s.require("users.manage")?;
        self.require_back_office_writable()?;
        let id = validate::id(user_id, "User")?;
        let name = clean(&input.display_name, "Name", 80, true)?;
        let role = validate::id(&input.role_id, "Role")?;
        let actor = self.actor(&s, None);
        let hash = match &input.pin {
            Some(p) if !p.is_empty() => {
                let sec: settings::SecuritySettings = self.db.read(|c| settings::get(c, settings::KEY_SECURITY))?;
                auth::validate_pin(p, sec.pin_min_length, sec.pin_max_length)?;
                Some(auth::hash_pin(p)?)
            }
            _ => None,
        };
        self.db.write(|tx| {
            let before = load_user(tx, &id)?;
            if (before.role_id == ROLE_OWNER || role == ROLE_OWNER) && s.role_id != ROLE_OWNER {
                return Err(AppError::forbidden("roles.manage"));
            }
            if before.role_id == ROLE_OWNER && (role != ROLE_OWNER || !input.active) && owner_count(tx, &id)? == 0 {
                return Err(AppError::conflict("At least one active owner must remain."));
            }
            if id == s.user_id && !input.active {
                return Err(AppError::conflict("You cannot deactivate your own account."));
            }
            let ok: bool = tx.query_row("SELECT 1 FROM roles WHERE role_id=?1", [&role], |_| Ok(true)).optional()?.unwrap_or(false);
            if !ok {
                return Err(AppError::validation("The selected role does not exist."));
            }
            tx.execute(
                "UPDATE users SET display_name=?2, role_id=?3, active=?4, pin_hash=COALESCE(?5, pin_hash), updated_at=?6 WHERE user_id=?1",
                params![id, name, role, input.active as i64, hash, time::now_str()],
            )?;
            if hash.is_some() {
                tx.execute("UPDATE user_login_state SET failed_attempts=0, locked_until=NULL WHERE user_id=?1", [&id])?;
            }
            audit::record(
                tx,
                &actor,
                "user.updated",
                "user",
                Some(&id),
                Some(&json!({ "name": before.display_name, "role_id": before.role_id, "active": before.active })),
                Some(&json!({ "name": name, "role_id": role, "active": input.active, "pin_reset": hash.is_some() })),
            )?;
            Ok(())
        })?;
        // Permission or credential changes end existing sessions (except our own name edits).
        if id != s.user_id {
            self.sessions.remove_user(&id);
        }
        self.db.read(|c| load_user(c, &id))
    }

    pub fn user_unlock(&self, token: &str, user_id: &str) -> AppResult<UserRow> {
        let s = self.session(token)?;
        s.require("users.manage")?;
        let id = validate::id(user_id, "User")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            tx.execute("UPDATE user_login_state SET failed_attempts=0, locked_until=NULL WHERE user_id=?1", [&id])?;
            audit::record(tx, &actor, "user.unlocked", "user", Some(&id), None, None)?;
            Ok(())
        })?;
        self.db.read(|c| load_user(c, &id))
    }

    /// Change your own PIN (requires the current PIN).
    pub fn user_change_own_pin(&self, token: &str, current_pin: &str, new_pin: &str) -> AppResult<()> {
        let s = self.session(token)?;
        let sec: settings::SecuritySettings = self.db.read(|c| settings::get(c, settings::KEY_SECURITY))?;
        let u = self.db.read(|c| auth::load_user_auth(c, &s.user_id))?;
        if !auth::verify_pin(current_pin, &u.pin_hash) {
            return Err(AppError::new(crate::ErrorCode::InvalidCredentials, "The current PIN is incorrect."));
        }
        auth::validate_pin(new_pin, sec.pin_min_length, sec.pin_max_length)?;
        let hash = auth::hash_pin(new_pin)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            tx.execute("UPDATE users SET pin_hash=?2, updated_at=?3 WHERE user_id=?1", params![s.user_id, hash, time::now_str()])?;
            audit::record(tx, &actor, "user.pin_changed", "user", Some(&s.user_id), None, None)?;
            Ok(())
        })
    }

    pub fn roles_list(&self, token: &str) -> AppResult<Vec<RoleRow>> {
        let s = self.session(token)?;
        if !s.has("users.manage") && !s.has("roles.manage") {
            return Err(AppError::forbidden("users.manage"));
        }
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT r.role_id, r.name, r.description, r.is_system, (SELECT COUNT(*) FROM users u WHERE u.role_id=r.role_id AND u.active=1)
                 FROM roles r ORDER BY r.is_system DESC, r.name",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(RoleRow {
                        role_id: r.get(0)?,
                        name: r.get(1)?,
                        description: r.get(2)?,
                        is_system: r.get::<_, i64>(3)? == 1,
                        user_count: r.get(4)?,
                        permissions: vec![],
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut out = vec![];
            for mut r in rows {
                let mut p: Vec<String> = auth::role_permissions(c, &r.role_id)?.into_iter().collect();
                p.sort();
                r.permissions = p;
                out.push(r);
            }
            Ok(out)
        })
    }

    pub fn permissions_catalog(&self, token: &str) -> AppResult<Vec<PermissionRow>> {
        self.session(token)?;
        Ok(PERMISSIONS
            .iter()
            .map(|(c, d, desc)| PermissionRow { code: c.to_string(), domain: d.to_string(), description: desc.to_string() })
            .collect())
    }

    /// Create or update a role's permissions. The Owner role is protected.
    pub fn role_save(&self, token: &str, role_id: Option<String>, name: &str, description: Option<String>, permissions: Vec<String>) -> AppResult<Vec<RoleRow>> {
        let s = self.session(token)?;
        s.require("roles.manage")?;
        self.require_back_office_writable()?;
        let name = clean(name, "Role name", 40, true)?;
        let desc = crate::setup::clean_opt(&description, "Description", 200)?;
        for p in &permissions {
            if !PERMISSIONS.iter().any(|x| x.0 == p) {
                return Err(AppError::validation(format!("Unknown permission {p}.")));
            }
        }
        let actor = self.actor(&s, None);
        let affected = self.db.write(|tx| {
            let now = time::now_str();
            let id = match role_id.filter(|r| !r.is_empty()) {
                Some(id) => {
                    let id = validate::id(&id, "Role")?;
                    if id == ROLE_OWNER {
                        return Err(AppError::conflict("The Owner role always has every permission and cannot be edited."));
                    }
                    let before: Vec<String> = auth::role_permissions(tx, &id)?.into_iter().collect();
                    let n = tx.execute("UPDATE roles SET name=?2, description=?3, updated_at=?4 WHERE role_id=?1", params![id, name, desc, now])?;
                    if n == 0 {
                        return Err(AppError::not_found("Role"));
                    }
                    tx.execute("DELETE FROM role_permissions WHERE role_id=?1", [&id])?;
                    audit::record(tx, &actor, "role.updated", "role", Some(&id), Some(&json!({ "permissions": before })), Some(&json!({ "name": name, "permissions": permissions })))?;
                    id
                }
                None => {
                    let id = new_id();
                    tx.execute(
                        "INSERT INTO roles(role_id, name, description, is_system, created_at, updated_at) VALUES (?1,?2,?3,0,?4,?4)",
                        params![id, name, desc, now],
                    )?;
                    audit::record(tx, &actor, "role.created", "role", Some(&id), None, Some(&json!({ "name": name, "permissions": permissions })))?;
                    id
                }
            };
            for p in &permissions {
                tx.execute("INSERT OR IGNORE INTO role_permissions(role_id, permission_code) VALUES (?1,?2)", params![id, p])?;
            }
            let mut st = tx.prepare("SELECT user_id FROM users WHERE role_id=?1")?;
            let users = st.query_map([&id], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            Ok(users)
        })?;
        for u in affected {
            if u != s.user_id {
                self.sessions.remove_user(&u);
            }
        }
        self.roles_list(token)
    }
}
