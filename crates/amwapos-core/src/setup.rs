//! First-run setup, login, lock/unlock and manager approval.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::audit::{self, Actor};
use crate::auth::{self, Session};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ids::new_id;
use crate::service::{AppCore, DeviceIdentity};
use crate::settings::{self, BackupSettings, PrinterSettings, ReceiptSettings};
use crate::time;

#[derive(Debug, Clone, Serialize)]
pub struct SetupStatus {
    pub setup_complete: bool,
    pub business_name: Option<String>,
    pub device: Option<DeviceIdentity>,
    pub schema_version: i64,
    pub app_version: String,
    pub data_dir: String,
    pub safety_backup: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetupRequest {
    pub business_name: String,
    #[serde(default)]
    pub business_name_ar: Option<String>,
    #[serde(default)]
    pub cr_number: Option<String>,
    #[serde(default)]
    pub vat_number: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default = "default_currency")]
    pub currency: String,
    #[serde(default = "default_digits")]
    pub currency_digits: i64,
    #[serde(default = "default_tz")]
    pub timezone: String,
    pub branch_name: String,
    #[serde(default = "default_branch_code")]
    pub branch_code: String,
    pub vat_rate_bp: i64,
    #[serde(default = "default_true")]
    pub prices_include_vat: bool,
    pub owner_name: String,
    pub owner_pin: String,
    pub device_name: String,
    #[serde(default = "default_device_code")]
    pub device_code: String,
    /// standalone | hub
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub receipt: Option<ReceiptSettings>,
    #[serde(default)]
    pub printer: Option<PrinterSettings>,
    #[serde(default)]
    pub backup_directory: Option<String>,
}

fn default_currency() -> String {
    "BHD".into()
}
fn default_digits() -> i64 {
    3
}
fn default_tz() -> String {
    "Asia/Bahrain".into()
}
fn default_branch_code() -> String {
    "MAIN".into()
}
fn default_true() -> bool {
    true
}
fn default_device_code() -> String {
    "T01".into()
}
fn default_mode() -> String {
    "standalone".into()
}

pub(crate) fn clean(s: &str, field: &str, max: usize, required: bool) -> AppResult<String> {
    let t = s.trim();
    if required && t.is_empty() {
        return Err(AppError::validation(format!("{field} is required.")));
    }
    if t.chars().count() > max {
        return Err(AppError::validation(format!("{field} must be at most {max} characters.")));
    }
    if t.chars().any(|c| c.is_control() && c != '\n') {
        return Err(AppError::validation(format!("{field} contains invalid characters.")));
    }
    Ok(t.to_string())
}

pub(crate) fn clean_opt(s: &Option<String>, field: &str, max: usize) -> AppResult<Option<String>> {
    match s {
        Some(v) => {
            let c = clean(v, field, max, false)?;
            Ok(if c.is_empty() { None } else { Some(c) })
        }
        None => Ok(None),
    }
}

pub(crate) fn validate_code(code: &str, field: &str) -> AppResult<String> {
    let c = code.trim().to_ascii_uppercase();
    if c.is_empty() || c.len() > 8 || !c.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return Err(AppError::validation(format!("{field} must be 1–8 letters or digits.")));
    }
    Ok(c)
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginUser {
    pub user_id: String,
    pub display_name: String,
    pub role_name: String,
    pub locked: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginResult {
    pub token: String,
    pub session: Session,
}

impl AppCore {
    pub fn setup_status(&self) -> AppResult<SetupStatus> {
        let (complete, name) = self.db.read(|c| {
            let complete = settings::get_raw(c, settings::KEY_SETUP_COMPLETE)?.is_some();
            let name: Option<String> = c
                .query_row("SELECT name FROM business LIMIT 1", [], |r| r.get(0))
                .optional()?;
            Ok((complete, name))
        })?;
        Ok(SetupStatus {
            setup_complete: complete,
            business_name: name,
            device: self.device(),
            schema_version: self.db.schema_version()?,
            app_version: audit::APP_VERSION.to_string(),
            data_dir: self.data_dir.to_string_lossy().to_string(),
            safety_backup: self.migration.safety_backup.clone(),
        })
    }

    /// Initialize a standalone store or a hub in one atomic transaction. If
    /// anything fails nothing is written, so the wizard can simply be re-run.
    pub fn setup_initialize(&self, req: SetupRequest) -> AppResult<SetupStatus> {
        if self.device().is_some() {
            return Err(AppError::conflict("This installation is already set up."));
        }
        let business_name = clean(&req.business_name, "Business name", 120, true)?;
        let branch_name = clean(&req.branch_name, "Branch name", 120, true)?;
        let owner_name = clean(&req.owner_name, "Owner name", 80, true)?;
        let device_name = clean(&req.device_name, "Terminal name", 60, true)?;
        let branch_code = validate_code(&req.branch_code, "Branch code")?;
        let device_code = validate_code(&req.device_code, "Terminal code")?;
        let currency = req.currency.trim().to_ascii_uppercase();
        if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(AppError::validation("Currency must be a 3-letter ISO code."));
        }
        if !(0..=4).contains(&req.currency_digits) {
            return Err(AppError::validation("Currency decimals must be between 0 and 4."));
        }
        time::tz(&req.timezone)?;
        if !(0..=10000).contains(&req.vat_rate_bp) {
            return Err(AppError::validation("VAT rate must be between 0% and 100%."));
        }
        if req.mode != "standalone" && req.mode != "hub" {
            return Err(AppError::validation("Mode must be standalone or hub. Terminals are set up by pairing with a hub."));
        }
        let sec = settings::SecuritySettings::default();
        auth::validate_pin(&req.owner_pin, sec.pin_min_length, sec.pin_max_length)?;
        let pin_hash = auth::hash_pin(&req.owner_pin)?;
        let receipt = req.receipt.clone().unwrap_or_default();
        settings::validate(settings::KEY_RECEIPT, serde_json::to_value(&receipt)?)?;
        let printer = req.printer.clone().unwrap_or_default();
        settings::validate(settings::KEY_PRINTER, serde_json::to_value(&printer)?)?;
        let backup_dir = match &req.backup_directory {
            Some(d) if !d.trim().is_empty() => d.trim().to_string(),
            _ => self.data_dir.join("backups").to_string_lossy().to_string(),
        };

        let now = time::now_str();
        let business_id = new_id();
        let branch_id = new_id();
        let device_id = new_id();
        let owner_id = new_id();
        let tax_id = new_id();
        let identity = DeviceIdentity {
            device_id: device_id.clone(),
            device_code: device_code.clone(),
            name: device_name.clone(),
            branch_id: branch_id.clone(),
            mode: req.mode.clone(),
        };
        self.db.write(|tx| {
            let existing: i64 = tx.query_row("SELECT COUNT(*) FROM business", [], |r| r.get(0))?;
            if existing > 0 {
                return Err(AppError::conflict("This installation is already set up."));
            }
            tx.execute(
                "INSERT INTO business(business_id,name,name_ar,cr_number,vat_number,phone,address,currency,currency_digits,timezone,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11)",
                params![
                    business_id,
                    business_name,
                    clean_opt(&req.business_name_ar, "Arabic name", 120)?,
                    clean_opt(&req.cr_number, "CR number", 40)?,
                    clean_opt(&req.vat_number, "VAT number", 40)?,
                    clean_opt(&req.phone, "Phone", 40)?,
                    clean_opt(&req.address, "Address", 300)?,
                    currency,
                    req.currency_digits,
                    req.timezone,
                    now
                ],
            )?;
            tx.execute(
                "INSERT INTO branches(branch_id,code,name,address,phone,cr_number,vat_number,currency,timezone,active,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10,?10)",
                params![
                    branch_id,
                    branch_code,
                    branch_name,
                    clean_opt(&req.address, "Address", 300)?,
                    clean_opt(&req.phone, "Phone", 40)?,
                    clean_opt(&req.cr_number, "CR number", 40)?,
                    clean_opt(&req.vat_number, "VAT number", 40)?,
                    currency,
                    req.timezone,
                    now
                ],
            )?;
            tx.execute(
                "INSERT INTO devices(device_id,branch_id,name,device_code,operating_mode,active,activated_at,app_version)
                 VALUES (?1,?2,?3,?4,?5,1,?6,?7)",
                params![device_id, branch_id, device_name, device_code, req.mode, now, audit::APP_VERSION],
            )?;
            let rate_name = if req.vat_rate_bp == 0 {
                "Zero-rated".to_string()
            } else {
                format!("VAT {}%", crate::money::format_decimal(req.vat_rate_bp, 2).trim_end_matches('0').trim_end_matches('.'))
            };
            tx.execute(
                "INSERT INTO tax_rules(tax_rule_id,name,rate_bp,inclusive,active,effective_from,created_at) VALUES (?1,?2,?3,?4,1,?5,?5)",
                params![tax_id, rate_name, req.vat_rate_bp, req.prices_include_vat as i64, now],
            )?;
            if req.vat_rate_bp != 0 {
                tx.execute(
                    "INSERT INTO tax_rules(tax_rule_id,name,rate_bp,inclusive,active,effective_from,created_at) VALUES (?1,'Zero-rated',0,?2,1,?3,?3)",
                    params![new_id(), req.prices_include_vat as i64, now],
                )?;
            }
            tx.execute(
                "INSERT INTO categories(category_id,parent_id,name,sort_order,active,created_at,updated_at) VALUES (?1,NULL,'General',0,1,?2,?2)",
                params![new_id(), now],
            )?;
            tx.execute(
                "INSERT INTO users(user_id,branch_id,display_name,pin_hash,role_id,active,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,1,?6,?6)",
                params![owner_id, branch_id, owner_name, pin_hash, auth::ROLE_OWNER, now],
            )?;
            settings::put(tx, settings::KEY_RECEIPT, &receipt, Some(&owner_id))?;
            settings::put(tx, settings::KEY_PRINTER, &printer, Some(&owner_id))?;
            settings::put(
                tx,
                settings::KEY_BACKUP,
                &BackupSettings { directory: backup_dir.clone(), ..Default::default() },
                Some(&owner_id),
            )?;
            settings::put(tx, settings::KEY_POS, &settings::PosSettings::default(), Some(&owner_id))?;
            settings::put(tx, settings::KEY_PAYMENTS, &settings::PaymentSettings::default(), Some(&owner_id))?;
            settings::put(tx, settings::KEY_DEVICE, &identity, Some(&owner_id))?;
            let actor = Actor {
                user_id: Some(owner_id.clone()),
                device_id: Some(device_id.clone()),
                branch_id: Some(branch_id.clone()),
                approved_by: None,
            };
            audit::record(
                tx,
                &actor,
                "setup.completed",
                "business",
                Some(&business_id),
                None,
                Some(&serde_json::json!({
                    "business": business_name, "branch": branch_name, "device": device_name,
                    "mode": req.mode, "currency": currency, "vat_rate_bp": req.vat_rate_bp
                })),
            )?;
            settings::put(tx, settings::KEY_SETUP_COMPLETE, &serde_json::json!({ "at": now }), Some(&owner_id))?;
            Ok(())
        })?;
        std::fs::create_dir_all(&backup_dir).ok();
        self.write_marker()?;
        self.set_device(Some(identity));
        self.setup_status()
    }

    pub fn login_users(&self) -> AppResult<Vec<LoginUser>> {
        self.require_device()?;
        let now = time::now_str();
        self.db.read(|c| {
            let mut stmt = c.prepare(
                "SELECT u.user_id, u.display_name, r.name, s.locked_until
                 FROM users u JOIN roles r ON r.role_id=u.role_id
                 LEFT JOIN user_login_state s ON s.user_id=u.user_id
                 WHERE u.active=1 ORDER BY u.display_name COLLATE NOCASE",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    let lu: Option<String> = r.get(3)?;
                    Ok(LoginUser {
                        user_id: r.get(0)?,
                        display_name: r.get(1)?,
                        role_name: r.get(2)?,
                        locked: lu.map(|l| l > now).unwrap_or(false),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Verify a PIN with lockout accounting. Returns the user on success.
    fn verify_user_pin(&self, user_id: &str, pin: &str, purpose: &str) -> AppResult<auth::UserAuthRow> {
        let device = self.require_device()?;
        let sec: settings::SecuritySettings = self.db.read(|c| settings::get(c, settings::KEY_SECURITY))?;
        let user = self.db.read(|c| auth::load_user_auth(c, user_id))?;
        let now = time::now();
        let now_s = time::fmt(now);
        if !user.active {
            return Err(AppError::new(ErrorCode::AccountLocked, "This account is deactivated. Ask a manager."));
        }
        if let Some(lu) = &user.locked_until {
            if *lu > now_s {
                let until = time::parse(lu).map(|t| (t - now).num_minutes() + 1).unwrap_or(0);
                return Err(AppError::new(
                    ErrorCode::AccountLocked,
                    format!("Too many incorrect PINs. This account is locked for {until} more minute(s)."),
                )
                .with_details(serde_json::json!({ "locked_until": lu })));
            }
        }
        // Argon2 verification happens outside the write lock.
        let ok = auth::verify_pin(pin, &user.pin_hash);
        let actor = Actor {
            user_id: Some(user.user_id.clone()),
            device_id: Some(device.device_id.clone()),
            branch_id: Some(device.branch_id.clone()),
            approved_by: None,
        };
        if ok {
            self.db.write(|tx| {
                tx.execute(
                    "INSERT INTO user_login_state(user_id, failed_attempts, locked_until, last_login_at) VALUES (?1,0,NULL,?2)
                     ON CONFLICT(user_id) DO UPDATE SET failed_attempts=0, locked_until=NULL, last_login_at=?2",
                    params![user.user_id, now_s],
                )?;
                if purpose == "login" {
                    audit::record(tx, &actor, "auth.login", "user", Some(&user.user_id), None, None)?;
                }
                Ok(())
            })?;
            Ok(user)
        } else {
            let attempts = user.failed_attempts + 1;
            let lock = attempts >= sec.max_failed_attempts;
            let locked_until = if lock {
                Some(time::fmt(now + chrono::Duration::minutes(sec.lockout_minutes)))
            } else {
                None
            };
            self.db.write(|tx| {
                tx.execute(
                    "INSERT INTO user_login_state(user_id, failed_attempts, locked_until, last_failed_at) VALUES (?1,?2,?3,?4)
                     ON CONFLICT(user_id) DO UPDATE SET failed_attempts=?2, locked_until=?3, last_failed_at=?4",
                    params![user.user_id, if lock { 0 } else { attempts }, locked_until, now_s],
                )?;
                audit::record(
                    tx,
                    &actor,
                    if lock { "auth.locked_out" } else { "auth.failed_pin" },
                    "user",
                    Some(&user.user_id),
                    None,
                    Some(&serde_json::json!({ "purpose": purpose, "attempt": attempts })),
                )?;
                Ok(())
            })?;
            if lock {
                return Err(AppError::new(
                    ErrorCode::AccountLocked,
                    format!("Too many incorrect PINs. This account is locked for {} minutes.", sec.lockout_minutes),
                ));
            }
            let left = sec.max_failed_attempts - attempts;
            Err(AppError::new(
                ErrorCode::InvalidCredentials,
                format!("Incorrect PIN. {left} attempt(s) left before the account is locked."),
            ))
        }
    }

    pub fn login(&self, user_id: &str, pin: &str) -> AppResult<LoginResult> {
        let device = self.require_device()?;
        let user = self.verify_user_pin(user_id, pin, "login")?;
        let perms = self.db.read(|c| auth::role_permissions(c, &user.role_id))?;
        let now = time::now();
        let token = auth::random_token();
        let session = Session {
            token: token.clone(),
            user_id: user.user_id,
            display_name: user.display_name,
            role_id: user.role_id,
            role_name: user.role_name,
            permissions: perms,
            device_id: device.device_id,
            branch_id: device.branch_id,
            created_at: now,
            last_activity: now,
            locked: false,
        };
        self.sessions.insert(session.clone());
        Ok(LoginResult { token, session })
    }

    pub fn logout(&self, token: &str) -> AppResult<()> {
        if let Some(s) = self.sessions.peek(token) {
            let actor = self.actor(&s, None);
            self.db.write(|tx| {
                audit::record(tx, &actor, "auth.logout", "user", Some(&s.user_id), None, None)?;
                Ok(())
            })?;
        }
        self.sessions.remove(token);
        Ok(())
    }

    pub fn lock(&self, token: &str) -> AppResult<()> {
        self.sessions.set_locked(token, true)
    }

    /// Unlock a locked session with the session owner's PIN.
    pub fn unlock(&self, token: &str, pin: &str) -> AppResult<Session> {
        let s = self
            .sessions
            .peek(token)
            .ok_or_else(|| AppError::new(ErrorCode::Unauthenticated, "Session not found. Please log in."))?;
        self.verify_user_pin(&s.user_id, pin, "unlock")?;
        self.sessions.set_locked(token, false)?;
        self.sessions
            .peek(token)
            .ok_or_else(|| AppError::new(ErrorCode::Unauthenticated, "Session not found."))
    }

    /// Session info without touching idle timers (works while locked).
    pub fn session_info(&self, token: &str) -> AppResult<Session> {
        self.sessions
            .peek(token)
            .ok_or_else(|| AppError::new(ErrorCode::Unauthenticated, "Session not found. Please log in."))
    }

    /// A manager enters their PIN to approve one action for the current
    /// session. Returns a single-use token bound to `permission`.
    pub fn approve(
        &self,
        requester_token: &str,
        approver_user_id: &str,
        pin: &str,
        permission: &str,
        summary: &str,
    ) -> AppResult<serde_json::Value> {
        let requester = self.session(requester_token)?;
        if !auth::PERMISSIONS.iter().any(|p| p.0 == permission) {
            return Err(AppError::validation("Unknown permission."));
        }
        let approver = self.verify_user_pin(approver_user_id, pin, "approval")?;
        let perms = self.db.read(|c| auth::role_permissions(c, &approver.role_id))?;
        if !perms.contains(permission) {
            return Err(AppError::new(
                ErrorCode::Forbidden,
                format!("{} is not allowed to approve this action.", approver.display_name),
            ));
        }
        let token = self
            .sessions
            .issue_approval(&approver.user_id, &approver.display_name, permission);
        let actor = self.actor(&requester, Some(approver.user_id.clone()));
        let summary = crate::setup::clean(summary, "Summary", 300, false)?;
        self.db.write(|tx| {
            audit::record(
                tx,
                &actor,
                "auth.approval_granted",
                "permission",
                Some(permission),
                None,
                Some(&serde_json::json!({ "summary": summary, "approver": approver.display_name })),
            )?;
            Ok(())
        })?;
        Ok(serde_json::json!({
            "approval_token": token,
            "approver_id": approver.user_id,
            "approver_name": approver.display_name,
            "expires_in_seconds": auth::APPROVAL_TTL_SECONDS
        }))
    }

    /// Users who can approve `permission` (for the approval dialog).
    pub fn approvers(&self, token: &str, permission: &str) -> AppResult<Vec<LoginUser>> {
        self.session(token)?;
        self.db.read(|c| {
            let mut stmt = c.prepare(
                "SELECT u.user_id, u.display_name, r.name FROM users u
                 JOIN roles r ON r.role_id=u.role_id
                 JOIN role_permissions rp ON rp.role_id=u.role_id AND rp.permission_code=?1
                 WHERE u.active=1 ORDER BY u.display_name",
            )?;
            let rows = stmt
                .query_map([permission], |r| {
                    Ok(LoginUser { user_id: r.get(0)?, display_name: r.get(1)?, role_name: r.get(2)?, locked: false })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }
}
