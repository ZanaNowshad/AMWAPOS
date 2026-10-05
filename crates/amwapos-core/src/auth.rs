//! Authentication, sessions, permissions and manager approvals.
//!
//! * PINs are hashed with Argon2id (per-hash random salt). Plain PINs are
//!   never stored or logged.
//! * Sessions are opaque 256-bit random tokens held in memory only; they
//!   expire, can be locked (idle / manual) without losing the cart, and bind
//!   the user, device and branch.
//! * Every command checks permissions in the backend. Hidden buttons are UX,
//!   not security.
//! * Manager approval produces a single-use, short-lived token bound to one
//!   permission. The approver is recorded on the resulting business record.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::error::{AppError, AppResult, ErrorCode};
use crate::time;

/// (code, domain, description)
pub const PERMISSIONS: &[(&str, &str, &str)] = &[
    ("admin.access", "System", "Enter Admin Mode"),
    ("pos.sell", "POS", "Sell products at the POS"),
    ("pos.discount", "POS", "Apply discounts within the cashier limit"),
    ("pos.discount_override", "POS", "Approve discounts above the cashier limit"),
    ("pos.price_override", "POS", "Override a selling price at the POS"),
    ("pos.void_sale", "POS", "Void a completed sale the same day, before the shift closes"),
    ("pos.custom_item", "POS", "Sell a custom (non-catalogue) item"),
    ("pos.cancel_sale", "POS", "Cancel an in-progress sale"),
    ("pos.remove_line", "POS", "Remove scanned lines from a sale"),
    ("pos.hold", "POS", "Hold and restore sales"),
    ("pos.held_others", "POS", "Resume or delete sales held by other cashiers"),
    ("pos.reprint", "POS", "Reprint historical receipts"),
    ("pos.no_sale", "Cash", "Open the cash drawer without a sale"),
    ("pos.negative_stock", "POS", "Sell tracked items below zero stock"),
    ("refund.create", "Refunds", "Process refunds"),
    ("shift.open", "Shift", "Open a shift"),
    ("shift.close", "Shift", "Close own shift"),
    ("shift.approve_variance", "Shift", "Acknowledge large cash variances"),
    ("shift.view_expected", "Shift", "See expected drawer cash before counting"),
    ("cash.paid_in", "Cash", "Record paid-in cash"),
    ("cash.paid_out", "Cash", "Record paid-out cash"),
    ("cash.safe_drop", "Cash", "Record safe drops"),
    ("sales.view", "Sales", "View sales history"),
    ("products.view", "Catalogue", "View products"),
    ("products.manage", "Catalogue", "Create and edit products, barcodes and categories"),
    ("products.view_cost", "Catalogue", "See product costs and margins"),
    ("prices.manage", "Catalogue", "Change selling prices"),
    ("barcodes.resolve", "Catalogue", "Resolve unknown barcodes"),
    ("inventory.view", "Inventory", "View stock levels and movements"),
    ("inventory.adjust", "Inventory", "Adjust stock"),
    ("inventory.receive", "Inventory", "Receive goods"),
    ("stocktake.manage", "Inventory", "Create, count and finalize stocktakes"),
    ("suppliers.manage", "Purchasing", "Manage suppliers"),
    ("purchasing.manage", "Purchasing", "Manage purchase orders"),
    ("customers.view", "Customers", "Look up customers"),
    ("customers.manage", "Customers", "Create and edit customers"),
    ("customers.credit", "Customers", "Sell on account, take account payments and set credit limits"),
    ("customers.credit_override", "Customers", "Approve a sale above a customer's credit limit"),
    ("deliveries.view", "Deliveries", "View deliveries"),
    ("deliveries.manage", "Deliveries", "Create and update deliveries"),
    ("reports.sales", "Reports", "Sales and operational reports"),
    ("reports.financial", "Reports", "Margin, cost and profit reports"),
    ("reports.tax", "Reports", "VAT reports"),
    ("audit.view", "System", "View the audit trail"),
    ("users.manage", "Staff", "Manage users"),
    ("roles.manage", "Staff", "Manage roles and permissions"),
    ("devices.manage", "System", "Manage terminals"),
    ("sync.manage", "System", "Configure multi-terminal sync"),
    ("settings.manage", "System", "Change store settings"),
    ("backup.manage", "System", "Create backups"),
    ("backup.restore", "System", "Restore backups"),
    ("import.run", "System", "Import data"),
    ("diagnostics.view", "System", "View diagnostics"),
    ("ai.use", "Automation", "Use the AI assistant (read-only)"),
    ("ai.mutate", "Automation", "Approve AI-proposed changes"),
    ("whatsapp.manage", "Automation", "Link WhatsApp, read conversations and send messages"),
    ("whatsapp.send", "Automation", "Send receipts and delivery updates on WhatsApp"),
    ("payments.review", "Automation", "Review payment screenshots"),
    ("ocr.scan", "Automation", "Scan supplier invoices"),
    ("inventory.transfer", "Inventory", "Create, ship and receive stock transfers"),
    ("loyalty.adjust", "Customers", "Adjust loyalty points by hand"),
    ("orders.manage", "Customers", "Record and confirm digital orders"),
    ("branches.manage", "System", "Create branches and assign staff to branches"),
    ("branches.all", "System", "Work in every branch"),
    ("payables.view", "Finance", "View supplier invoices, balances and ageing"),
    ("payables.review", "Finance", "Enter and approve supplier invoices for posting"),
    ("payables.post", "Finance", "Post and reverse supplier invoices and credit notes"),
    ("payables.pay", "Finance", "Record, allocate and reverse supplier payments"),
    ("expenses.view", "Finance", "See expenses and petty cash"),
    ("expenses.create", "Finance", "Enter and submit expenses"),
    ("expenses.approve", "Finance", "Approve, reject and void expenses"),
    ("expenses.pay", "Finance", "Record expense payments"),
    ("petty_cash.manage", "Finance", "Open, top up and count petty cash"),
    ("reports.profit", "Reports", "See operating profit (sales, cost of goods and expenses)"),
    ("day.x_report", "Trading day", "See the current totals (X report), the opening and closing checks"),
    ("day.close", "Trading day", "Close the trading day (Z close)"),
    ("registers.manage", "Trading day", "Add, rename and move registers and cash drawers"),
    ("cases.view", "Trading day", "See cases such as cash differences"),
    ("cases.manage", "Trading day", "Acknowledge, assign and add notes to cases"),
    ("cases.resolve", "Trading day", "Resolve or dismiss cases"),
];

pub const ROLE_OWNER: &str = "role_owner";
pub const ROLE_MANAGER: &str = "role_manager";
pub const ROLE_CASHIER: &str = "role_cashier";
pub const ROLE_ACCOUNTANT: &str = "role_accountant";
pub const ROLE_INVENTORY: &str = "role_inventory";
pub const ROLE_DELIVERY: &str = "role_delivery";

pub fn default_roles() -> Vec<(&'static str, &'static str, &'static str, Vec<&'static str>)> {
    let all: Vec<&str> = PERMISSIONS.iter().map(|p| p.0).collect();
    let manager: Vec<&str> = all
        .iter()
        .copied()
        .filter(|p| {
            !matches!(
                *p,
                "roles.manage"
                    | "backup.restore"
                    | "sync.manage"
                    | "ai.mutate"
                    | "devices.manage"
                    | "branches.manage"
                    | "branches.all"
                    // Money owed to suppliers is posted and paid by the owner unless granted.
                    | "payables.post"
                    | "payables.pay"
                    | "expenses.pay"
            )
        })
        .collect();
    let cashier = vec![
        "pos.sell",
        "pos.discount",
        "pos.cancel_sale",
        "pos.remove_line",
        "pos.hold",
        "pos.reprint",
        "shift.open",
        "shift.close",
        "customers.view",
        "customers.manage",
        "deliveries.view",
        "whatsapp.send",
    ];
    let accountant = vec![
        "admin.access",
        "sales.view",
        "reports.sales",
        "reports.financial",
        "reports.tax",
        "audit.view",
        "products.view",
        "products.view_cost",
        "inventory.view",
        "customers.view",
        "payables.view",
        "expenses.view",
        "expenses.create",
        "expenses.pay",
        "reports.profit",
        "day.x_report",
        "cases.view",
    ];
    let inventory = vec![
        "admin.access",
        "products.view",
        "inventory.view",
        "inventory.adjust",
        "inventory.receive",
        "stocktake.manage",
        "suppliers.manage",
        "purchasing.manage",
        "barcodes.resolve",
        "ocr.scan",
        "inventory.transfer",
    ];
    let delivery = vec!["deliveries.view", "deliveries.manage", "customers.view", "whatsapp.send", "orders.manage"];
    vec![
        (ROLE_OWNER, "Owner", "Full access", all),
        (ROLE_MANAGER, "Manager", "Store operations and overrides", manager),
        (ROLE_CASHIER, "Cashier", "Checkout only", cashier),
        (ROLE_ACCOUNTANT, "Accountant", "Read-only financial access", accountant),
        (ROLE_INVENTORY, "Inventory", "Receiving and stock control", inventory),
        (ROLE_DELIVERY, "Delivery", "Assigned deliveries", delivery),
    ]
}

/// Permissions added to the catalogue after the first release. Built-in
/// roles created before an upgrade get the ones their defaults include, once.
pub const UPGRADE_PERMISSIONS: &[&str] = &[
    "day.x_report",
    "day.close",
    "registers.manage",
    "cases.view",
    "cases.manage",
    "cases.resolve",
    "pos.void_sale",
    "expenses.view",
    "expenses.create",
    "expenses.approve",
    "expenses.pay",
    "petty_cash.manage",
    "reports.profit",
    "customers.credit",
    "customers.credit_override",
    "whatsapp.send",
    "payments.review",
    "ocr.scan",
    "inventory.transfer",
    "loyalty.adjust",
    "orders.manage",
    "branches.manage",
    "branches.all",
    "payables.view",
    "payables.review",
    "payables.post",
    "payables.pay",
];

fn seed_mark(conn: &Connection, role: &str, perm: &str, now: &str) -> AppResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO role_permission_seeds(role_id, permission_code, seeded_at) VALUES (?1,?2,?3)",
        params![role, perm, now],
    )?;
    Ok(())
}

/// Insert the permission catalogue and system roles (idempotent).
pub fn seed_roles(conn: &Connection) -> AppResult<()> {
    let now = time::now_str();
    for (code, domain, desc) in PERMISSIONS {
        conn.execute(
            "INSERT INTO permissions(code, domain, description) VALUES (?1,?2,?3)
             ON CONFLICT(code) DO UPDATE SET domain=excluded.domain, description=excluded.description
             WHERE permissions.domain IS NOT excluded.domain OR permissions.description IS NOT excluded.description",
            params![code, domain, desc],
        )?;
    }
    for (id, name, desc, perms) in default_roles() {
        let existed: bool = conn.query_row("SELECT 1 FROM roles WHERE role_id=?1", [id], |_| Ok(true)).optional()?.unwrap_or(false);
        if !existed {
            conn.execute(
                "INSERT INTO roles(role_id,name,description,is_system,created_at,updated_at) VALUES (?1,?2,?3,1,?4,?4)",
                params![id, name, desc, now],
            )?;
            for p in &perms {
                conn.execute("INSERT OR IGNORE INTO role_permissions(role_id, permission_code) VALUES (?1,?2)", params![id, p])?;
                seed_mark(conn, id, p, &now)?;
            }
        } else if id == ROLE_OWNER {
            // Owner always holds every permission, including ones added by upgrades.
            for p in perms {
                conn.execute("INSERT OR IGNORE INTO role_permissions(role_id, permission_code) VALUES (?1,?2)", params![id, p])?;
            }
        } else {
            // Other built-in roles: a permission added to the catalogue after the
            // first release is granted once, if that role's defaults include it.
            // A permission the owner removes later is not added back. The cashier
            // defaults contain neither ai.use nor orders.manage.
            for p in perms.iter().filter(|p| UPGRADE_PERMISSIONS.contains(p)) {
                let seeded: bool = conn
                    .query_row("SELECT 1 FROM role_permission_seeds WHERE role_id=?1 AND permission_code=?2", params![id, p], |_| Ok(true))
                    .optional()?
                    .unwrap_or(false);
                if !seeded {
                    conn.execute("INSERT OR IGNORE INTO role_permissions(role_id, permission_code) VALUES (?1,?2)", params![id, p])?;
                    seed_mark(conn, id, p, &now)?;
                }
            }
        }
    }
    Ok(())
}

fn argon() -> Argon2<'static> {
    // OWASP-recommended Argon2id parameters (19 MiB, 2 iterations, 1 lane).
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::new(19 * 1024, 2, 1, None).expect("valid argon2 params"))
}

pub fn validate_pin(pin: &str, min_len: usize, max_len: usize) -> AppResult<()> {
    if pin.len() < min_len || pin.len() > max_len || !pin.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::validation(format!("The PIN must be {min_len}–{max_len} digits.")));
    }
    if pin.chars().all(|c| c == pin.chars().next().unwrap()) {
        return Err(AppError::validation("The PIN cannot be a single repeated digit."));
    }
    const WEAK: &[&str] = &["1234", "12345", "123456", "1234567", "12345678", "4321", "654321", "0123", "012345"];
    if WEAK.contains(&pin) {
        return Err(AppError::validation("This PIN is too easy to guess. Choose another."));
    }
    Ok(())
}

pub fn hash_pin(pin: &str) -> AppResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    argon().hash_password(pin.as_bytes(), &salt).map(|h| h.to_string()).map_err(|e| AppError::internal(format!("PIN hashing failed: {e}")))
}

pub fn verify_pin(pin: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => argon().verify_password(pin.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

pub fn random_token() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

pub fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(s.as_bytes()))
}

#[derive(Debug, Clone, Serialize)]
pub struct Session {
    #[serde(skip_serializing)]
    pub token: String,
    pub user_id: String,
    pub display_name: String,
    pub role_id: String,
    pub role_name: String,
    pub permissions: HashSet<String>,
    pub device_id: String,
    pub branch_id: String,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub locked: bool,
}

impl Session {
    pub fn has(&self, perm: &str) -> bool {
        self.permissions.contains(perm)
    }
    pub fn require(&self, perm: &str) -> AppResult<()> {
        if self.has(perm) {
            Ok(())
        } else {
            Err(AppError::forbidden(perm))
        }
    }
}

#[derive(Debug, Clone)]
struct Approval {
    approver_id: String,
    approver_name: String,
    permission: String,
    expires: DateTime<Utc>,
    /// High-risk approvals are bound to one exact request (see `binding`):
    /// the action, the record, a digest of the request and the device. An
    /// unbound approval cannot be used for a bound action, or the reverse.
    binding: Option<String>,
}

/// A high-risk request waiting for a manager: what the manager is shown is
/// kept on the server, so the approval and its audit record describe the
/// request the backend saw, not text supplied by the client.
#[derive(Debug, Clone)]
pub struct PendingBinding {
    pub permission: String,
    pub summary: String,
    expires: DateTime<Utc>,
}

/// Fingerprint of one exact high-risk request.
pub fn binding(action: &str, entity: &str, payload: &serde_json::Value, device_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let body = serde_json::json!([action, entity, payload, device_id]).to_string();
    hex::encode(Sha256::digest(body.as_bytes()))
}

pub const SESSION_MAX_HOURS: i64 = 16;
pub const APPROVAL_TTL_SECONDS: i64 = 120;

#[derive(Default)]
pub struct SessionStore {
    sessions: Mutex<HashMap<String, Session>>,
    approvals: Mutex<HashMap<String, Approval>>,
    pending: Mutex<HashMap<String, PendingBinding>>,
}

impl SessionStore {
    pub fn insert(&self, s: Session) {
        if let Ok(mut m) = self.sessions.lock() {
            m.insert(s.token.clone(), s);
        }
    }

    /// Resolve a token to an active (unlocked, unexpired) session and touch it.
    /// `idle_lock_minutes` = 0 disables idle locking.
    pub fn get_active(&self, token: &str, idle_lock_minutes: i64) -> AppResult<Session> {
        let mut m = self.sessions.lock().map_err(|_| AppError::internal("session store poisoned"))?;
        let now = time::now();
        let s =
            m.get_mut(token).ok_or_else(|| AppError::new(ErrorCode::Unauthenticated, "Your session has ended. Please log in again."))?;
        if now - s.created_at > Duration::hours(SESSION_MAX_HOURS) {
            m.remove(token);
            return Err(AppError::new(ErrorCode::Unauthenticated, "Your session expired. Please log in again."));
        }
        if !s.locked && idle_lock_minutes > 0 && now - s.last_activity > Duration::minutes(idle_lock_minutes) {
            s.locked = true;
        }
        if s.locked {
            return Err(AppError::new(ErrorCode::Unauthenticated, "The terminal is locked. Enter your PIN to continue.")
                .with_details(serde_json::json!({ "locked": true, "user_id": s.user_id })));
        }
        s.last_activity = now;
        Ok(s.clone())
    }

    /// Get a session regardless of lock state (for unlock / status).
    pub fn peek(&self, token: &str) -> Option<Session> {
        self.sessions.lock().ok().and_then(|m| m.get(token).cloned())
    }

    pub fn set_locked(&self, token: &str, locked: bool) -> AppResult<()> {
        let mut m = self.sessions.lock().map_err(|_| AppError::internal("session store poisoned"))?;
        let s = m.get_mut(token).ok_or_else(|| AppError::new(ErrorCode::Unauthenticated, "Session not found."))?;
        s.locked = locked;
        s.last_activity = time::now();
        Ok(())
    }

    /// Move a session to another branch (multi-branch; checked by the caller).
    pub fn set_branch(&self, token: &str, branch_id: &str) -> AppResult<Session> {
        let mut m = self.sessions.lock().map_err(|_| AppError::internal("session store poisoned"))?;
        let s = m.get_mut(token).ok_or_else(|| AppError::new(ErrorCode::Unauthenticated, "Session not found."))?;
        s.branch_id = branch_id.to_string();
        Ok(s.clone())
    }

    pub fn remove(&self, token: &str) {
        if let Ok(mut m) = self.sessions.lock() {
            m.remove(token);
        }
    }

    /// Remove every session of a user (deactivation, PIN reset, role change).
    pub fn remove_user(&self, user_id: &str) {
        if let Ok(mut m) = self.sessions.lock() {
            m.retain(|_, s| s.user_id != user_id);
        }
    }

    pub fn active_users(&self) -> Vec<(String, String)> {
        self.sessions.lock().map(|m| m.values().map(|s| (s.user_id.clone(), s.display_name.clone())).collect()).unwrap_or_default()
    }

    pub fn issue_approval(&self, approver_id: &str, approver_name: &str, permission: &str) -> String {
        self.issue_approval_bound(approver_id, approver_name, permission, None)
    }

    pub fn issue_approval_bound(&self, approver_id: &str, approver_name: &str, permission: &str, binding: Option<&str>) -> String {
        let token = random_token();
        if let Ok(mut m) = self.approvals.lock() {
            let now = time::now();
            m.retain(|_, a| a.expires > now);
            m.insert(
                token.clone(),
                Approval {
                    approver_id: approver_id.to_string(),
                    approver_name: approver_name.to_string(),
                    permission: permission.to_string(),
                    expires: now + Duration::seconds(APPROVAL_TTL_SECONDS),
                    binding: binding.map(str::to_string),
                },
            );
        }
        token
    }

    /// Remember a high-risk request the backend has asked a manager to approve.
    pub fn register_pending(&self, binding: &str, permission: &str, summary: &str) {
        if let Ok(mut m) = self.pending.lock() {
            let now = time::now();
            m.retain(|_, p| p.expires > now);
            m.insert(
                binding.to_string(),
                PendingBinding {
                    permission: permission.to_string(),
                    summary: summary.to_string(),
                    expires: now + Duration::seconds(APPROVAL_TTL_SECONDS * 5),
                },
            );
        }
    }

    pub fn pending(&self, binding: &str) -> Option<PendingBinding> {
        let m = self.pending.lock().ok()?;
        m.get(binding).filter(|p| p.expires > time::now()).cloned()
    }

    /// Consume an unbound approval token for `permission`. Returns (approver_id, name).
    pub fn consume_approval(&self, token: &str, permission: &str) -> Option<(String, String)> {
        self.consume_inner(token, permission, None)
    }

    /// Consume an approval bound to exactly this request.
    pub fn consume_bound(&self, token: &str, permission: &str, binding: &str) -> Option<(String, String)> {
        let r = self.consume_inner(token, permission, Some(binding));
        if r.is_some() {
            if let Ok(mut m) = self.pending.lock() {
                m.remove(binding);
            }
        }
        r
    }

    fn consume_inner(&self, token: &str, permission: &str, binding: Option<&str>) -> Option<(String, String)> {
        let mut m = self.approvals.lock().ok()?;
        let a = m.get(token)?.clone();
        if a.permission != permission || a.expires < time::now() || a.binding.as_deref() != binding {
            return None;
        }
        m.remove(token);
        Some((a.approver_id, a.approver_name))
    }

    /// Check an approval token without consuming it.
    pub fn peek_approval(&self, token: &str, permission: &str) -> Option<(String, String)> {
        let m = self.approvals.lock().ok()?;
        let a = m.get(token)?;
        if a.permission != permission || a.expires < time::now() {
            return None;
        }
        Some((a.approver_id.clone(), a.approver_name.clone()))
    }
}

#[derive(Debug, Clone)]
pub struct UserAuthRow {
    pub user_id: String,
    pub display_name: String,
    pub pin_hash: String,
    pub role_id: String,
    pub role_name: String,
    pub active: bool,
    pub failed_attempts: i64,
    pub locked_until: Option<String>,
}

pub fn load_user_auth(conn: &Connection, user_id: &str) -> AppResult<UserAuthRow> {
    conn.query_row(
        "SELECT u.user_id, u.display_name, u.pin_hash, u.role_id, r.name, u.active,
                COALESCE(s.failed_attempts,0), s.locked_until
         FROM users u JOIN roles r ON r.role_id = u.role_id
         LEFT JOIN user_login_state s ON s.user_id = u.user_id
         WHERE u.user_id = ?1",
        [user_id],
        |r| {
            Ok(UserAuthRow {
                user_id: r.get(0)?,
                display_name: r.get(1)?,
                pin_hash: r.get(2)?,
                role_id: r.get(3)?,
                role_name: r.get(4)?,
                active: r.get::<_, i64>(5)? == 1,
                failed_attempts: r.get(6)?,
                locked_until: r.get(7)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::new(ErrorCode::InvalidCredentials, "Unknown user or incorrect PIN."))
}

pub fn role_permissions(conn: &Connection, role_id: &str) -> AppResult<HashSet<String>> {
    let mut stmt = conn.prepare_cached("SELECT permission_code FROM role_permissions WHERE role_id = ?1")?;
    let rows = stmt.query_map([role_id], |r| r.get::<_, String>(0))?.collect::<Result<HashSet<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_hash_roundtrip() {
        let h = hash_pin("4826").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(!h.contains("4826"));
        assert!(verify_pin("4826", &h));
        assert!(!verify_pin("4827", &h));
        assert!(!verify_pin("4826", "garbage"));
    }

    #[test]
    fn pin_rules() {
        assert!(validate_pin("4826", 4, 8).is_ok());
        assert!(validate_pin("482", 4, 8).is_err());
        assert!(validate_pin("48a6", 4, 8).is_err());
        assert!(validate_pin("1111", 4, 8).is_err());
        assert!(validate_pin("1234", 4, 8).is_err());
        assert!(validate_pin("123456789", 4, 8).is_err());
    }

    #[test]
    fn approvals_are_single_use_and_bound() {
        let s = SessionStore::default();
        let t = s.issue_approval("u1", "Manager", "refund.create");
        assert!(s.consume_approval(&t, "pos.discount_override").is_none());
        assert_eq!(s.consume_approval(&t, "refund.create").unwrap().0, "u1");
        assert!(s.consume_approval(&t, "refund.create").is_none());
    }

    #[test]
    fn bound_approval_covers_one_exact_request_once() {
        let s = SessionStore::default();
        let body = serde_json::json!({ "qty": 1 });
        let b = binding("sale.void", "S1", &body, "dev-1");
        // Anything that differs gives a different fingerprint.
        assert_ne!(b, binding("sale.void", "S2", &body, "dev-1"));
        assert_ne!(b, binding("sale.void", "S1", &serde_json::json!({ "qty": 2 }), "dev-1"));
        assert_ne!(b, binding("sale.void", "S1", &body, "dev-2"));
        assert_ne!(b, binding("refund.create", "S1", &body, "dev-1"));
        let t = s.issue_approval_bound("m1", "Mona", "pos.void_sale", Some(&b));
        // Not usable unbound, nor for another request.
        assert!(s.consume_approval(&t, "pos.void_sale").is_none());
        assert!(s.consume_bound(&t, "pos.void_sale", &binding("sale.void", "S2", &body, "dev-1")).is_none());
        assert_eq!(s.consume_bound(&t, "pos.void_sale", &b).unwrap().0, "m1");
        assert!(s.consume_bound(&t, "pos.void_sale", &b).is_none(), "single use");
        // An unbound approval cannot be used for a bound action.
        let u = s.issue_approval("m1", "Mona", "pos.void_sale");
        assert!(s.consume_bound(&u, "pos.void_sale", &b).is_none());
    }

    #[test]
    fn cashier_has_no_admin_permissions() {
        let roles = default_roles();
        let cashier = &roles.iter().find(|r| r.0 == ROLE_CASHIER).unwrap().3;
        for p in ["admin.access", "products.view_cost", "reports.financial", "settings.manage", "refund.create"] {
            assert!(!cashier.contains(&p), "cashier must not have {p}");
        }
        let acct = &roles.iter().find(|r| r.0 == ROLE_ACCOUNTANT).unwrap().3;
        for p in ["pos.sell", "inventory.adjust", "products.manage"] {
            assert!(!acct.contains(&p), "accountant must not have {p}");
        }
    }
}
