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
        let credit = self.features()?.is_on("customers.credit");
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
        self.require_feature("customers.credit")?;
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
        self.require_feature("customers.credit")?;
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
        self.require_feature("customers.credit")?;
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

// ------------------------------------------------------------ ageing and statements

/// Balance by how late it is. The buckets always add up to the balance:
/// payments, refunds and credits are applied to the oldest charges first
/// (first in, first out), and what is left of each charge is aged from the
/// date of that charge plus the account's terms. Unapplied credit is shown
/// in `current` as a negative amount.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Ageing {
    pub current_minor: i64,
    pub d1_30_minor: i64,
    pub d31_60_minor: i64,
    pub d61_90_minor: i64,
    pub d90_plus_minor: i64,
}

impl Ageing {
    pub fn total(&self) -> i64 {
        self.current_minor + self.d1_30_minor + self.d31_60_minor + self.d61_90_minor + self.d90_plus_minor
    }
    pub fn overdue(&self) -> i64 {
        self.d1_30_minor + self.d31_60_minor + self.d61_90_minor + self.d90_plus_minor
    }
}

/// Pure ageing over (business date, signed amount) entries in date order.
pub fn age(entries: &[(String, i64)], as_of: &str, terms_days: i64) -> Ageing {
    use chrono::NaiveDate;
    let mut charges: std::collections::VecDeque<(String, i64)> = Default::default();
    let mut credit = 0i64;
    for (date, amt) in entries {
        if date.as_str() > as_of {
            continue;
        }
        if *amt > 0 {
            charges.push_back((date.clone(), *amt));
        } else {
            credit += -amt;
        }
        // Apply any credit to the oldest open charges.
        while credit > 0 {
            match charges.front_mut() {
                Some(front) => {
                    let take = credit.min(front.1);
                    front.1 -= take;
                    credit -= take;
                    if front.1 == 0 {
                        charges.pop_front();
                    }
                }
                None => break,
            }
        }
    }
    let as_of_d = NaiveDate::parse_from_str(as_of, "%Y-%m-%d").ok();
    let mut a = Ageing { current_minor: -credit, ..Default::default() };
    for (date, left) in charges {
        let late = match (as_of_d, NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok()) {
            (Some(now), Some(d)) => (now - d).num_days() - terms_days,
            _ => 0,
        };
        match late {
            i64::MIN..=0 => a.current_minor += left,
            1..=30 => a.d1_30_minor += left,
            31..=60 => a.d31_60_minor += left,
            61..=90 => a.d61_90_minor += left,
            _ => a.d90_plus_minor += left,
        }
    }
    a
}

/// A customer's ledger as (business date, amount, kind, reference, note, method).
type LedgerLine = (String, i64, String, Option<String>, Option<String>, Option<String>);

fn ledger_lines(c: &Connection, customer_id: &str) -> AppResult<Vec<LedgerLine>> {
    let day = time::day(c)?;
    let mut st = c.prepare(
        "SELECT l.created_at, l.amount_minor, l.kind,
                COALESCE(s.receipt_number, r.refund_receipt_number), l.note, l.method
         FROM customer_ledger l
         LEFT JOIN sales s ON l.ref_type='sale' AND s.sale_id=l.ref_id
         LEFT JOIN refunds r ON l.ref_type IN ('refund','void') AND r.refund_id=l.ref_id
         WHERE l.customer_id=?1 ORDER BY l.created_at, l.rowid",
    )?;
    let rows = st
        .query_map([customer_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(at, amt, kind, rf, note, method)| Ok((time::business_date(time::parse(&at)?, &day)?, amt, kind, rf, note, method)))
        .collect()
}

pub fn terms_days(c: &Connection, customer_id: &str) -> AppResult<i64> {
    Ok(c.query_row("SELECT terms_days FROM customer_accounts WHERE customer_id=?1", [customer_id], |r| r.get(0)).optional()?.unwrap_or(30))
}

pub fn customer_ageing(c: &Connection, customer_id: &str, as_of: &str) -> AppResult<Ageing> {
    let lines = ledger_lines(c, customer_id)?;
    let entries: Vec<(String, i64)> = lines.iter().map(|l| (l.0.clone(), l.1)).collect();
    Ok(age(&entries, as_of, terms_days(c, customer_id)?))
}

#[derive(Debug, Clone, Serialize)]
pub struct StatementLine {
    pub date: String,
    pub kind: String,
    pub reference: Option<String>,
    pub note: Option<String>,
    pub method: Option<String>,
    /// Charges (positive ledger amounts).
    pub charge_minor: i64,
    /// Payments, refunds and credits (negative ledger amounts, shown positive).
    pub credit_minor: i64,
    pub balance_minor: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Statement {
    pub customer_id: String,
    pub customer_name: String,
    pub phone: Option<String>,
    pub from: String,
    pub to: String,
    pub opening_minor: i64,
    pub lines: Vec<StatementLine>,
    pub closing_minor: i64,
    pub terms_days: i64,
    pub credit_limit_minor: i64,
    pub ageing: Ageing,
}

pub fn statement(c: &Connection, customer_id: &str, from: &str, to: &str) -> AppResult<Statement> {
    time::validate_date(from)?;
    time::validate_date(to)?;
    if to < from {
        return Err(AppError::validation("The end date is before the start date."));
    }
    let (name, phone): (String, Option<String>) = c
        .query_row("SELECT name, phone FROM customers WHERE customer_id=?1", [customer_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?
        .ok_or_else(|| AppError::not_found("Customer"))?;
    let lines = ledger_lines(c, customer_id)?;
    let opening: i64 = lines.iter().filter(|l| l.0.as_str() < from).map(|l| l.1).sum();
    let mut bal = opening;
    let mut out = vec![];
    for (date, amt, kind, rf, note, method) in lines.iter().filter(|l| l.0.as_str() >= from && l.0.as_str() <= to) {
        bal += amt;
        out.push(StatementLine {
            date: date.clone(),
            kind: kind.clone(),
            reference: rf.clone(),
            note: note.clone(),
            method: method.clone(),
            charge_minor: (*amt).max(0),
            credit_minor: (-amt).max(0),
            balance_minor: bal,
        });
    }
    let entries: Vec<(String, i64)> = lines.iter().map(|l| (l.0.clone(), l.1)).collect();
    let terms = terms_days(c, customer_id)?;
    let acc = account(c, customer_id)?;
    Ok(Statement {
        customer_id: customer_id.into(),
        customer_name: name,
        phone,
        from: from.into(),
        to: to.into(),
        opening_minor: opening,
        lines: out,
        closing_minor: bal,
        terms_days: terms,
        credit_limit_minor: acc.credit_limit_minor,
        ageing: age(&entries, to, terms),
    })
}

impl AppCore {
    fn statement_range(&self, c: &Connection, from: Option<String>, to: Option<String>) -> AppResult<(String, String)> {
        let today = time::business_date(time::now(), &time::day(c)?)?;
        let to = to.filter(|x| !x.is_empty()).unwrap_or(today);
        let from = from.filter(|x| !x.is_empty()).unwrap_or_else(|| format!("{}-01", &to[..7]));
        Ok((from, to))
    }

    pub fn customer_statement(&self, token: &str, customer_id: &str, from: Option<String>, to: Option<String>) -> AppResult<Statement> {
        let s = self.session(token)?;
        s.require("customers.view")?;
        let cid = validate::id(customer_id, "Customer")?;
        self.db.read(|c| {
            let (from, to) = self.statement_range(c, from, to)?;
            statement(c, &cid, &from, &to)
        })
    }

    /// The statement as a bilingual PDF (for printing or sending).
    pub fn customer_statement_pdf(&self, token: &str, customer_id: &str, from: Option<String>, to: Option<String>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("customers.view")?;
        let cid = validate::id(customer_id, "Customer")?;
        let (doc, st) = self.db.read(|c| {
            let (from, to) = self.statement_range(c, from, to)?;
            let st = statement(c, &cid, &from, &to)?;
            Ok((crate::receipt::statement_doc(c, &s.branch_id, &st)?, st))
        })?;
        let bytes = crate::pdf::bitmap_pdf(&doc.to_bitmap(), &format!("Statement {}", st.customer_name));
        let safe: String = st.customer_name.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' }).collect();
        Ok(json!({ "file_name": format!("Statement-{safe}-{}.pdf", st.to), "base64": crate::ids::b64(&bytes), "text": doc.to_text() }))
    }

    /// Every customer who owes, by how late it is (receivables ageing).
    pub fn receivables(&self, token: &str, as_of: Option<String>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("customers.view")?;
        self.db.read(|c| {
            let today = time::business_date(time::now(), &time::day(c)?)?;
            let as_of = as_of.filter(|x| !x.is_empty()).unwrap_or(today);
            time::validate_date(&as_of)?;
            let mut st = c.prepare(
                "SELECT cu.customer_id, cu.name, cu.phone, COALESCE(a.credit_limit_minor,0)
                 FROM customers cu LEFT JOIN customer_accounts a ON a.customer_id=cu.customer_id
                 WHERE EXISTS (SELECT 1 FROM customer_ledger l WHERE l.customer_id=cu.customer_id) ORDER BY cu.name",
            )?;
            let custs = st
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, i64>(3)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut rows = vec![];
            let mut total = Ageing::default();
            for (id, name, phone, limit) in custs {
                let a = customer_ageing(c, &id, &as_of)?;
                if a.total() == 0 {
                    continue;
                }
                total.current_minor += a.current_minor;
                total.d1_30_minor += a.d1_30_minor;
                total.d31_60_minor += a.d31_60_minor;
                total.d61_90_minor += a.d61_90_minor;
                total.d90_plus_minor += a.d90_plus_minor;
                rows.push(json!({ "customer_id": id, "name": name, "phone": phone, "limit_minor": limit, "balance_minor": a.total(),
                    "overdue_minor": a.overdue(), "ageing": a }));
            }
            rows.sort_by_key(|r| -r["overdue_minor"].as_i64().unwrap_or(0));
            Ok(json!({ "as_of": as_of, "rows": rows, "total": total, "balance_minor": total.total(), "overdue_minor": total.overdue() }))
        })
    }

    pub fn customer_terms_set(&self, token: &str, customer_id: &str, terms_days: i64) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("customers.credit")?;
        let cid = validate::id(customer_id, "Customer")?;
        if !(0..=365).contains(&terms_days) {
            return Err(AppError::validation("Terms must be between 0 and 365 days."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let n = tx.execute("UPDATE customer_accounts SET terms_days=?2 WHERE customer_id=?1", params![cid, terms_days])?;
            if n == 0 {
                return Err(AppError::conflict("This customer does not have an account yet."));
            }
            audit::record(tx, &actor, "customer.terms_set", "customer", Some(&cid), None, Some(&json!({ "terms_days": terms_days })))?;
            Ok(json!({ "terms_days": terms_days }))
        })
    }
}

#[cfg(test)]
mod ageing_tests {
    use super::*;
    fn e(d: &str, a: i64) -> (String, i64) {
        (d.to_string(), a)
    }
    #[test]
    fn payments_clear_the_oldest_charges_first() {
        // 100 on 1 Jul, 50 on 20 Aug, paid 120 on 1 Sep; as of 30 Sep, 30-day terms.
        let a = age(&[e("2026-07-01", 100), e("2026-08-20", 50), e("2026-09-01", -120)], "2026-09-30", 30);
        // 1 Jul is fully paid; 30 of 20 Aug remains: 41 days old, 11 days late.
        assert_eq!(a, Ageing { d1_30_minor: 30, ..Default::default() });
        assert_eq!(a.total(), 30);
    }
    #[test]
    fn buckets_always_add_up_to_the_balance() {
        let entries = vec![e("2026-01-10", 500), e("2026-04-01", 300), e("2026-06-15", -100), e("2026-08-01", 200), e("2026-09-20", 50)];
        let a = age(&entries, "2026-09-30", 30);
        assert_eq!(a.total(), 950);
        assert_eq!(a.d90_plus_minor, 700, "Jan 400 left + Apr 300");
        // 1 Aug is 60 days old: 30 days past 30-day terms.
        assert_eq!(a.d1_30_minor, 200);
        assert_eq!(a.current_minor, 50);
    }
    #[test]
    fn an_overpayment_is_a_credit_not_a_late_amount() {
        let a = age(&[e("2026-09-01", 100), e("2026-09-02", -150)], "2026-12-31", 30);
        assert_eq!(a, Ageing { current_minor: -50, ..Default::default() });
    }
    #[test]
    fn future_entries_do_not_count() {
        let a = age(&[e("2026-09-01", 100), e("2026-10-15", -100)], "2026-09-30", 30);
        assert_eq!(a.total(), 100);
    }
}
