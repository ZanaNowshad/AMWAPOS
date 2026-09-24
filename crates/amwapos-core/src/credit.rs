//! Customer accounts (module `customer_credit`) and saved addresses.
//!
//! Money rules:
//! - The balance is the sum of append-only ledger entries (positive = owed to
//!   the store). Nothing is ever updated or deleted; a correction is a new
//!   `adjustment` entry with a note.
//! - A sale paid with the `account` tender writes a `sale` entry in the same
//!   transaction as the sale. Going over the credit limit needs a manager's
//!   `customers.credit_override` approval.
//! - A refund to the `account` tender writes a negative `refund` entry.
//! - A cash account payment is also a `paid_in` cash event in the open shift,
//!   so the drawer's expected cash stays right.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::idempotency::{self, Check};
use crate::ids::new_id;
use crate::service::AppCore;
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Serialize)]
pub struct Account {
    pub customer_id: String,
    pub enabled: bool,
    pub credit_limit_minor: i64,
    pub balance_minor: i64,
    pub available_minor: i64,
}

pub fn balance(c: &Connection, customer_id: &str) -> AppResult<i64> {
    Ok(c.query_row("SELECT COALESCE(SUM(amount_minor),0) FROM customer_ledger WHERE customer_id=?1", [customer_id], |r| r.get(0))?)
}

pub fn account(c: &Connection, customer_id: &str) -> AppResult<Account> {
    let (enabled, limit): (bool, i64) = c
        .query_row("SELECT enabled, credit_limit_minor FROM customer_accounts WHERE customer_id=?1", [customer_id], |r| {
            Ok((r.get::<_, i64>(0)? != 0, r.get(1)?))
        })
        .optional()?
        .unwrap_or((false, 0));
    let bal = balance(c, customer_id)?;
    Ok(Account {
        customer_id: customer_id.into(),
        enabled,
        credit_limit_minor: limit,
        balance_minor: bal,
        available_minor: (limit - bal).max(0),
    })
}

/// Derived id for a ledger/cash row that belongs to another operation.
fn derived_op(op: &str, suffix: &str) -> String {
    hex::encode(&Sha256::digest(format!("{op}:{suffix}"))[..13]).to_uppercase()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn post(
    tx: &Connection,
    s: &Session,
    customer_id: &str,
    kind: &str,
    amount_minor: i64,
    ref_type: Option<&str>,
    ref_id: Option<&str>,
    method: Option<&str>,
    note: Option<&str>,
    operation_id: &str,
) -> AppResult<String> {
    let id = new_id();
    tx.execute(
        "INSERT INTO customer_ledger(entry_id, customer_id, kind, amount_minor, ref_type, ref_id, method, note, operation_id, user_id, device_id, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![id, customer_id, kind, amount_minor, ref_type, ref_id, method, note, operation_id, s.user_id, s.device_id, time::now_str()],
    )?;
    Ok(id)
}

/// Checks before a sale with an `account` tender. Returns the customer id.
/// `over_limit_ok` is true when a manager approved going over the limit.
pub(crate) fn check_sale(tx: &Connection, customer_id: Option<&str>, amount_minor: i64, over_limit_ok: bool) -> AppResult<String> {
    let cid = customer_id.ok_or_else(|| AppError::validation("Choose the customer before charging to an account."))?;
    let a = account(tx, cid)?;
    if !a.enabled {
        return Err(AppError::conflict("This customer does not have an account. Enable it on the customer's page first."));
    }
    if a.balance_minor + amount_minor > a.credit_limit_minor && !over_limit_ok {
        return Err(AppError::approval_required("customers.credit_override", "Sale above the customer's credit limit")
            .with_details(json!({ "permission": "customers.credit_override", "summary": "Sale above the customer's credit limit",
                                  "balance_minor": a.balance_minor, "limit_minor": a.credit_limit_minor })));
    }
    Ok(cid.to_string())
}

pub(crate) fn sale_op(op: &str) -> String {
    derived_op(op, "account-sale")
}

pub(crate) fn refund_op(op: &str) -> String {
    derived_op(op, "account-refund")
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccountPayment {
    pub customer_id: String,
    pub amount_minor: i64,
    /// cash | card | benefitpay | bank_transfer | wallet
    pub method: String,
    #[serde(default)]
    pub reference: Option<String>,
    pub operation_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AddressInput {
    #[serde(default)]
    pub address_id: Option<String>,
    pub customer_id: String,
    pub label: String,
    #[serde(default)]
    pub area: Option<String>,
    pub address: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub is_default: bool,
}

impl AppCore {
    /// Account summary, recent ledger and saved addresses for a customer.
    pub fn customer_account(&self, token: &str, customer_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("customers.view")?;
        let cid = validate::id(customer_id, "Customer")?;
        let credit = self.features()?.is_on("customer_credit");
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT address_id, label, area, address, notes, is_default FROM customer_addresses WHERE customer_id=?1 ORDER BY is_default DESC, label",
            )?;
            let addresses: Vec<Value> = st
                .query_map([&cid], |r| {
                    Ok(json!({ "address_id": r.get::<_, String>(0)?, "label": r.get::<_, String>(1)?, "area": r.get::<_, Option<String>>(2)?,
                               "address": r.get::<_, String>(3)?, "notes": r.get::<_, Option<String>>(4)?, "is_default": r.get::<_, i64>(5)? != 0 }))
                })?
                .collect::<Result<_, _>>()?;
            let (account, ledger) = if credit {
                let mut st = c.prepare(
                    "SELECT l.entry_id, l.kind, l.amount_minor, l.ref_type, l.ref_id, l.method, l.note, u.display_name, l.created_at,
                            CASE WHEN l.ref_type='sale' THEN (SELECT receipt_number FROM sales WHERE sale_id=l.ref_id)
                                 WHEN l.ref_type='refund' THEN (SELECT refund_receipt_number FROM refunds WHERE refund_id=l.ref_id) END
                     FROM customer_ledger l LEFT JOIN users u ON u.user_id=l.user_id WHERE l.customer_id=?1 ORDER BY l.created_at DESC LIMIT 200",
                )?;
                let rows: Vec<Value> = st
                    .query_map([&cid], |r| {
                        Ok(json!({ "entry_id": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?, "amount_minor": r.get::<_, i64>(2)?,
                                   "ref_type": r.get::<_, Option<String>>(3)?, "ref_id": r.get::<_, Option<String>>(4)?, "method": r.get::<_, Option<String>>(5)?,
                                   "note": r.get::<_, Option<String>>(6)?, "user": r.get::<_, Option<String>>(7)?, "created_at": r.get::<_, String>(8)?,
                                   "reference": r.get::<_, Option<String>>(9)? }))
                    })?
                    .collect::<Result<_, _>>()?;
                (Some(account(c, &cid)?), rows)
            } else {
                (None, vec![])
            };
            Ok(json!({ "credit_enabled": credit, "account": account, "ledger": ledger, "addresses": addresses }))
        })
    }

    /// Enable/disable an account and set its limit.
    pub fn customer_account_set(&self, token: &str, customer_id: &str, enabled: bool, credit_limit_minor: i64) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("customers.credit")?;
        self.require_feature("customer_credit")?;
        let cid = validate::id(customer_id, "Customer")?;
        validate::money_non_negative(credit_limit_minor, "Credit limit")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            tx.query_row("SELECT 1 FROM customers WHERE customer_id=?1", [&cid], |_| Ok(()))
                .optional()?
                .ok_or_else(|| AppError::not_found("Customer"))?;
            let before = account(tx, &cid)?;
            tx.execute(
                "INSERT INTO customer_accounts(customer_id, enabled, credit_limit_minor, updated_by, updated_at) VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(customer_id) DO UPDATE SET enabled=excluded.enabled, credit_limit_minor=excluded.credit_limit_minor,
                   updated_by=excluded.updated_by, updated_at=excluded.updated_at",
                params![cid, enabled as i64, credit_limit_minor, s.user_id, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "customer.account_changed",
                "customer",
                Some(&cid),
                Some(&json!({ "enabled": before.enabled, "limit": before.credit_limit_minor })),
                Some(&json!({ "enabled": enabled, "limit": credit_limit_minor })),
            )?;
            Ok(())
        })?;
        self.customer_account(token, &cid)
    }

    /// Record a payment towards an account balance.
    pub fn customer_account_payment(&self, token: &str, req: AccountPayment) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("customers.credit")?;
        self.require_feature("customer_credit")?;
        let cid = validate::id(&req.customer_id, "Customer")?;
        validate::money_non_negative(req.amount_minor, "Amount")?;
        if req.amount_minor == 0 {
            return Err(AppError::validation("Enter an amount greater than zero."));
        }
        let pay = self.db.read(crate::settings::payments)?;
        let cfg = pay
            .tender(&req.method)
            .filter(|t| t.enabled && t.method != "account")
            .ok_or_else(|| AppError::validation("Choose an enabled payment method."))?;
        if cfg.requires_reference && req.reference.as_deref().unwrap_or("").trim().is_empty() {
            return Err(AppError::validation(format!("{} requires a reference.", cfg.label)));
        }
        idempotency::validate_operation_id(&req.operation_id)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "account.payment", &req)? {
                Check::Replay { result } => return Ok(result),
                Check::New { payload_hash } => payload_hash,
            };
            let a = account(tx, &cid)?;
            if req.amount_minor > a.balance_minor {
                return Err(AppError::validation("The payment is more than the amount owed."));
            }
            let mut cash_event = None;
            if req.method == "cash" {
                let shift_id = crate::sales::open_shift_for(tx, &s)?.ok_or_else(crate::sales::shift_required)?;
                let id = new_id();
                let name: String = tx.query_row("SELECT name FROM customers WHERE customer_id=?1", [&cid], |r| r.get(0))?;
                tx.execute(
                    "INSERT INTO cash_events(cash_event_id, shift_id, type, amount_minor, reason, actor_user_id, approved_by, operation_id, device_id, created_at)
                     VALUES (?1,?2,'paid_in',?3,?4,?5,NULL,?6,?7,?8)",
                    params![id, shift_id, req.amount_minor, format!("Account payment: {name}"), s.user_id, derived_op(&req.operation_id, "cash"), s.device_id, time::now_str()],
                )?;
                crate::printing::enqueue_drawer_pulse(tx, Some(&s.user_id), &id)?;
                cash_event = Some(id);
            }
            let entry = post(
                tx,
                &s,
                &cid,
                "payment",
                -req.amount_minor,
                cash_event.as_ref().map(|_| "cash_event"),
                cash_event.as_deref(),
                Some(&req.method),
                req.reference.as_deref(),
                &req.operation_id,
            )?;
            let result = json!({ "entry_id": entry, "balance_minor": balance(tx, &cid)?, "cash_event_id": cash_event });
            audit::record(tx, &actor, "customer.account_payment", "customer", Some(&cid), None, Some(&json!({ "amount_minor": req.amount_minor, "method": req.method })))?;
            idempotency::complete(tx, &req.operation_id, "account.payment", Some(&s.user_id), Some(&s.device_id), &hash, Some(&entry), &result)?;
            Ok(result)
        })
    }

    /// A correcting entry (positive adds to what is owed). A note is required.
    pub fn customer_account_adjust(
        &self,
        token: &str,
        customer_id: &str,
        amount_minor: i64,
        note: &str,
        operation_id: &str,
    ) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("customers.credit")?;
        s.require("customers.credit_override")?;
        self.require_feature("customer_credit")?;
        let cid = validate::id(customer_id, "Customer")?;
        if amount_minor == 0 {
            return Err(AppError::validation("Enter a non-zero amount."));
        }
        let note = note.trim();
        if note.is_empty() {
            return Err(AppError::validation("Explain the adjustment."));
        }
        idempotency::validate_operation_id(operation_id)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let payload = json!({ "c": cid, "a": amount_minor, "n": note });
            let hash = match idempotency::check(tx, operation_id, "account.adjust", &payload)? {
                Check::Replay { result } => return Ok(result),
                Check::New { payload_hash } => payload_hash,
            };
            account(tx, &cid)?;
            let entry = post(tx, &s, &cid, "adjustment", amount_minor, None, None, None, Some(note), operation_id)?;
            let result = json!({ "entry_id": entry, "balance_minor": balance(tx, &cid)? });
            audit::record(
                tx,
                &actor,
                "customer.account_adjusted",
                "customer",
                Some(&cid),
                None,
                Some(&json!({ "amount_minor": amount_minor, "note": note })),
            )?;
            idempotency::complete(tx, operation_id, "account.adjust", Some(&s.user_id), Some(&s.device_id), &hash, Some(&entry), &result)?;
            Ok(result)
        })
    }

    pub fn customer_address_save(&self, token: &str, a: AddressInput) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("customers.manage")?;
        let cid = validate::id(&a.customer_id, "Customer")?;
        let label = a.label.trim();
        let address = a.address.trim();
        if label.is_empty() || address.is_empty() || label.len() > 40 || address.len() > 300 {
            return Err(AppError::validation("Enter a label (up to 40 characters) and an address (up to 300)."));
        }
        let now = time::now_str();
        self.db.write(|tx| {
            tx.query_row("SELECT 1 FROM customers WHERE customer_id=?1", [&cid], |_| Ok(())).optional()?.ok_or_else(|| AppError::not_found("Customer"))?;
            if a.is_default {
                tx.execute("UPDATE customer_addresses SET is_default=0, updated_at=?2 WHERE customer_id=?1 AND is_default=1", params![cid, now])?;
            }
            match a.address_id.as_deref().filter(|x| !x.is_empty()) {
                Some(id) => {
                    let n = tx.execute(
                        "UPDATE customer_addresses SET label=?3, area=?4, address=?5, notes=?6, is_default=?7, updated_at=?8 WHERE address_id=?1 AND customer_id=?2",
                        params![id, cid, label, a.area, address, a.notes, a.is_default as i64, now],
                    )?;
                    if n == 0 {
                        return Err(AppError::not_found("Address"));
                    }
                }
                None => {
                    tx.execute(
                        "INSERT INTO customer_addresses(address_id, customer_id, label, area, address, notes, is_default, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
                        params![new_id(), cid, label, a.area, address, a.notes, a.is_default as i64, now],
                    )?;
                }
            }
            Ok(())
        })?;
        self.customer_account(token, &cid)
    }

    pub fn customer_address_delete(&self, token: &str, address_id: &str) -> AppResult<()> {
        let s = self.session(token)?;
        s.require("customers.manage")?;
        let id = validate::id(address_id, "Address")?;
        self.db.write(|tx| {
            let n = tx.execute("DELETE FROM customer_addresses WHERE address_id=?1", [&id])?;
            if n == 0 {
                return Err(AppError::new(ErrorCode::NotFound, "Address was not found."));
            }
            Ok(())
        })
    }
}
