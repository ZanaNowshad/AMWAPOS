//! Accounts Payable foundation (supplier invoices → liabilities → credits,
//! payments, allocations → balances and ageing).
//!
//! Lifecycle of a supplier invoice / credit note record:
//!
//! ```text
//! draft ──review──► approved ──post (payables.post)──► posted ──► (payment status: unpaid → partially paid → paid)
//!   │                  │                                  │
//!   └──void────────────┘                                  └──reverse──► reversed (the record and its liability stay, marked)
//! ```
//!
//! * A liability exists only after an explicit, authorized posting of an
//!   approved record; OCR / Document Intelligence output never posts.
//! * Posting is idempotent on the operation id and impossible twice per
//!   invoice (unique liability per invoice, checked states).
//! * Posted records are immutable (database triggers); corrections are
//!   reversals (recorded with actor, time and reason) or credit notes.
//! * Balances are derived, never stored: supplier balance = open liabilities −
//!   open credits − posted payments; an invoice's outstanding amount = its
//!   liability − its active allocations.
//! * Payments are allocated explicitly (or "oldest first" only when the person
//!   chooses it). Money is exact minor units (fils); no floating point.

#![allow(clippy::type_complexity)]

use std::collections::HashMap;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ids::{new_id, next_seq};
use crate::service::AppCore;
use crate::time;
use crate::validate;

pub const METHODS: [&str; 6] = ["cash", "bank_transfer", "cheque", "card", "benefitpay", "other"];

/// Ageing buckets on outstanding amounts, by days past the due date.
pub const BUCKETS: [(&str, i64, i64); 5] =
    [("current", i64::MIN, 0), ("1_30", 1, 30), ("31_60", 31, 60), ("61_90", 61, 90), ("90_plus", 91, i64::MAX)];

fn today() -> NaiveDate {
    time::now().date_naive()
}

fn parse_date(s: &str, field: &str) -> AppResult<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10).unwrap_or(s), "%Y-%m-%d")
        .map_err(|_| AppError::validation(format!("{field} must be a date (YYYY-MM-DD).")))
}

/// Days of credit written in a supplier's payment terms: "30 days", "Net 30",
/// "30", "٣٠ يوم". None when the terms say nothing countable ("cash", "COD").
pub fn terms_days(terms: Option<&str>) -> Option<i64> {
    let t = crate::ocrflow::normalize_digits(terms?).to_lowercase();
    if ["cash", "cod", "immediate", "on delivery", "نقدا", "كاش"].iter().any(|w| t.contains(w)) {
        return Some(0);
    }
    let digits: String = t.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<i64>().ok().filter(|d| (0..=365).contains(d))
}

/// The due date and the rule that produced it: the invoice's own due date,
/// else the invoice date plus the supplier's payment terms, else the invoice
/// date itself (due on receipt).
pub fn due_date(invoice_date: &str, invoice_due: Option<&str>, terms: Option<&str>) -> AppResult<(String, &'static str)> {
    let d = parse_date(invoice_date, "Invoice date")?;
    if let Some(due) = invoice_due.filter(|x| !x.trim().is_empty()) {
        let due = parse_date(due, "Due date")?;
        if due < d {
            return Err(AppError::validation("The due date is before the invoice date."));
        }
        return Ok((due.to_string(), "invoice"));
    }
    if let Some(n) = terms_days(terms) {
        return Ok(((d + chrono::Duration::days(n)).to_string(), "terms"));
    }
    Ok((d.to_string(), "invoice_date"))
}

/// The ageing bucket of an amount due on `due` as of `as_of`.
pub fn bucket(due: &str, as_of: NaiveDate) -> &'static str {
    let Ok(d) = NaiveDate::parse_from_str(due.get(..10).unwrap_or(due), "%Y-%m-%d") else { return "current" };
    let late = (as_of - d).num_days();
    BUCKETS.iter().find(|(_, lo, hi)| late >= *lo && late <= *hi).map(|b| b.0).unwrap_or("current")
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManualLine {
    #[serde(default)]
    pub product_id: Option<String>,
    pub description: String,
    pub qty_milli: i64,
    pub unit_cost_minor: i64,
    #[serde(default)]
    pub vat_minor: Option<i64>,
}

/// A supplier invoice or credit note entered by hand (no scanned document).
#[derive(Debug, Clone, Deserialize)]
pub struct ManualInvoice {
    pub supplier_id: String,
    /// invoice | credit_note
    pub doc_type: String,
    pub invoice_number: String,
    pub invoice_date: String,
    #[serde(default)]
    pub due_date: Option<String>,
    pub subtotal_minor: i64,
    pub vat_minor: i64,
    pub total_minor: i64,
    #[serde(default)]
    pub applies_to_invoice_id: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub lines: Vec<ManualLine>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AllocationInput {
    /// The supplier invoice (its liability) to settle.
    pub invoice_id: String,
    pub amount_minor: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaymentInput {
    pub supplier_id: String,
    pub paid_on: String,
    pub amount_minor: i64,
    pub method: String,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub allocations: Vec<AllocationInput>,
    /// Allocate what is not allocated explicitly to the oldest due invoices.
    /// Only when the person chose it; never assumed.
    #[serde(default)]
    pub oldest_first: bool,
    pub operation_id: String,
}

// ---------------------------------------------------------------- derived amounts

fn allocated_to_liability(c: &Connection, liability: &str) -> AppResult<i64> {
    Ok(c.query_row(
        "SELECT COALESCE(SUM(amount_minor),0) FROM ap_allocations WHERE liability_id=?1 AND status='active'",
        [liability],
        |r| r.get(0),
    )?)
}

fn allocated_from_payment(c: &Connection, payment: &str) -> AppResult<i64> {
    Ok(c.query_row("SELECT COALESCE(SUM(amount_minor),0) FROM ap_allocations WHERE payment_id=?1 AND status='active'", [payment], |r| {
        r.get(0)
    })?)
}

fn allocated_from_credit(c: &Connection, credit: &str) -> AppResult<i64> {
    Ok(c.query_row("SELECT COALESCE(SUM(amount_minor),0) FROM ap_allocations WHERE credit_id=?1 AND status='active'", [credit], |r| {
        r.get(0)
    })?)
}

/// What a supplier is owed: posted invoices − posted credits − posted payments.
pub fn supplier_balance(c: &Connection, supplier: &str) -> AppResult<i64> {
    let (inv, cr, pay): (i64, i64, i64) = c.query_row(
        "SELECT (SELECT COALESCE(SUM(amount_minor),0) FROM ap_liabilities WHERE supplier_id=?1 AND status='open'),
                (SELECT COALESCE(SUM(amount_minor),0) FROM ap_credits WHERE supplier_id=?1 AND status='open'),
                (SELECT COALESCE(SUM(amount_minor),0) FROM ap_payments WHERE supplier_id=?1 AND status='posted')",
        [supplier],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(inv - cr - pay)
}

/// Payment status of a posted invoice from its liability.
fn payment_status(amount: i64, allocated: i64) -> &'static str {
    if allocated <= 0 {
        "unpaid"
    } else if allocated < amount {
        "partially_paid"
    } else {
        "paid"
    }
}

/// The lifecycle shown for a supplier invoice record.
pub fn lifecycle(c: &Connection, invoice_id: &str) -> AppResult<(String, Option<i64>)> {
    let (status, posting, doc_type): (String, String, String) =
        c.query_row("SELECT status, posting, doc_type FROM supplier_invoices WHERE invoice_id=?1", [invoice_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    Ok(match (status.as_str(), posting.as_str()) {
        ("void", _) => ("void".into(), None),
        (_, "reversed") => ("reversed".into(), None),
        (_, "posted") if doc_type == "invoice" => {
            let (lid, amount): (String, i64) =
                c.query_row("SELECT liability_id, amount_minor FROM ap_liabilities WHERE invoice_id=?1", [invoice_id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            let alloc = allocated_to_liability(c, &lid)?;
            (payment_status(amount, alloc).into(), Some(amount - alloc))
        }
        (_, "posted") => {
            let (cid, amount): (String, i64) =
                c.query_row("SELECT credit_id, amount_minor FROM ap_credits WHERE invoice_id=?1", [invoice_id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            let used = allocated_from_credit(c, &cid)?;
            (if used >= amount { "credit_applied".into() } else { "credit_open".into() }, Some(amount - used))
        }
        ("approved", _) => ("approved".into(), None),
        _ => ("draft".into(), None),
    })
}

fn system_note() -> &'static str {
    "Posted supplier records change only by reversal. Nothing here moves stock."
}

impl AppCore {
    fn ap_session(&self, token: &str, perm: &str) -> AppResult<Session> {
        let s = self.session(token)?;
        // Purchasing managers keep read access to the invoice records they review.
        if perm == "payables.view" && (s.has("payables.view") || s.has("purchasing.manage")) {
            return Ok(s);
        }
        s.require(perm)?;
        // Every payables write: the ledger lives on the hub (its tables do not
        // sync), so a terminal must not start a second, private one.
        self.require_back_office_writable()?;
        Ok(s)
    }

    /// A supplier invoice / credit note typed in by hand (a draft for review).
    pub fn ap_invoice_create_manual(&self, token: &str, input: ManualInvoice) -> AppResult<Value> {
        let s = self.ap_session(token, "payables.review")?;
        let supplier = validate::id(&input.supplier_id, "Supplier")?;
        if !matches!(input.doc_type.as_str(), "invoice" | "credit_note") {
            return Err(AppError::validation("Document type must be invoice or credit note."));
        }
        let number = input.invoice_number.trim().chars().take(60).collect::<String>();
        if number.is_empty() {
            return Err(AppError::validation("Enter the supplier's invoice number."));
        }
        parse_date(&input.invoice_date, "Invoice date")?;
        for (v, f) in [(input.subtotal_minor, "Subtotal"), (input.vat_minor, "VAT"), (input.total_minor, "Total")] {
            validate::money_non_negative(v, f)?;
        }
        if input.subtotal_minor + input.vat_minor != input.total_minor {
            return Err(AppError::validation("Subtotal plus VAT must equal the total exactly."));
        }
        if input.total_minor == 0 {
            return Err(AppError::validation("The total must be more than zero."));
        }
        let actor = self.actor(&s, None);
        let id = self.db.write(|tx| {
            tx.query_row("SELECT 1 FROM suppliers WHERE supplier_id=?1", [&supplier], |_| Ok(())).optional()?.ok_or_else(|| AppError::not_found("Supplier"))?;
            if let Some(orig) = &input.applies_to_invoice_id {
                let sup: Option<String> = tx.query_row("SELECT supplier_id FROM supplier_invoices WHERE invoice_id=?1", [orig], |r| r.get(0)).optional()?;
                if sup.as_deref() != Some(supplier.as_str()) {
                    return Err(AppError::validation("The credit note must refer to an invoice of the same supplier."));
                }
            }
            let id = new_id();
            let seq = format!("SI-{:05}", next_seq(tx, "supplier_invoice")?);
            let now = time::now_str();
            tx.execute(
                "INSERT INTO supplier_invoices(invoice_id, number, doc_type, supplier_id, scan_id, invoice_number, invoice_date, due_date, subtotal_minor, vat_minor,
                    total_minor, status, posting, notes, created_by, created_at, updated_at, source, applies_to_invoice_id)
                 VALUES (?1,?2,?3,?4,NULL,?5,?6,?7,?8,?9,?10,'draft','not_posted',?11,?12,?13,?13,'manual',?14)",
                params![
                    id,
                    seq,
                    input.doc_type,
                    supplier,
                    number,
                    input.invoice_date.get(..10).unwrap_or(&input.invoice_date),
                    input.due_date.as_deref().filter(|d| !d.is_empty()),
                    input.subtotal_minor,
                    input.vat_minor,
                    input.total_minor,
                    input.notes.as_deref().map(|n| n.chars().take(500).collect::<String>()),
                    s.user_id,
                    now,
                    input.applies_to_invoice_id
                ],
            )?;
            for (i, l) in input.lines.iter().enumerate() {
                validate::money_non_negative(l.unit_cost_minor, "Unit cost")?;
                let total = crate::money::extend(l.unit_cost_minor, l.qty_milli)?;
                tx.execute(
                    "INSERT INTO supplier_invoice_lines(invoice_id, line_no, product_id, description, qty_milli, unit_cost_minor, vat_minor, line_total_minor)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![id, i as i64 + 1, l.product_id, l.description.chars().take(200).collect::<String>(), l.qty_milli, l.unit_cost_minor, l.vat_minor, total],
                )?;
            }
            audit::record(tx, &actor, "supplier_invoice.drafted", "supplier_invoice", Some(&id), None, Some(&json!({ "number": seq, "source": "manual", "origin": "admin" })))?;
            Ok(id)
        })?;
        self.ap_invoice_get(token, &id)
    }

    /// Mark a reviewed record ready for posting.
    pub fn ap_invoice_approve(&self, token: &str, invoice_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("payables.review") {
            s.require("purchasing.manage")?;
        }
        self.supplier_invoice_set_status(token, invoice_id, "approved")
    }

    /// Post an approved supplier invoice (→ liability) or credit note (→ credit).
    /// Repeating the same operation id returns the same result; a second
    /// posting of the same record is refused.
    pub fn ap_invoice_post(&self, token: &str, invoice_id: &str, operation_id: &str) -> AppResult<Value> {
        let s = self.ap_session(token, "payables.post")?;
        crate::idempotency::validate_operation_id(operation_id)?;
        let id = validate::id(invoice_id, "Supplier invoice")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            // Replay of the same request.
            let replay: Option<String> = tx
                .query_row(
                    "SELECT invoice_id FROM ap_liabilities WHERE operation_id=?1 UNION ALL SELECT invoice_id FROM ap_credits WHERE operation_id=?1",
                    [operation_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(done) = replay {
                if done != id {
                    return Err(AppError::new(ErrorCode::IdempotencyMismatch, "This operation id was used for another posting. Nothing was changed."));
                }
                return Ok(());
            }
            type Row = (String, String, String, String, Option<String>, Option<String>, Option<String>, i64, String, Option<String>, Option<String>);
            let (status, posting, doc_type, supplier, number, date, due, total, currency, applies, terms): Row = tx
                .query_row(
                    "SELECT i.status, i.posting, i.doc_type, i.supplier_id, i.invoice_number, i.invoice_date, i.due_date, i.total_minor, i.currency,
                            i.applies_to_invoice_id, s.payment_terms
                     FROM supplier_invoices i JOIN suppliers s ON s.supplier_id=i.supplier_id WHERE i.invoice_id=?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?)),
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Supplier invoice"))?;
            if posting == "posted" || posting == "reversed" {
                return Err(AppError::conflict("This supplier invoice was already posted.").with_details(json!({ "kind": "already_posted" })));
            }
            if status != "approved" {
                return Err(AppError::validation("Only an approved (reviewed) supplier invoice can be posted.").with_details(json!({ "kind": "not_approved" })));
            }
            if total <= 0 {
                return Err(AppError::validation("The total must be more than zero."));
            }
            let date = date.filter(|d| !d.is_empty()).ok_or_else(|| AppError::validation("Enter the invoice date before posting."))?;
            if number.as_deref().is_none_or(|n| n.trim().is_empty()) {
                return Err(AppError::validation("Enter the supplier's invoice number before posting."));
            }
            // Another posted record with the same supplier number and type.
            let dup: Option<String> = tx
                .query_row(
                    // Suppliers reuse numbers after a while: only within a year is it the same invoice.
                    "SELECT number FROM supplier_invoices WHERE supplier_id=?1 AND doc_type=?2 AND lower(invoice_number)=lower(?3) AND posting='posted'
                       AND invoice_id<>?4 AND abs(julianday(invoice_date) - julianday(?5)) <= 365",
                    params![supplier, doc_type, number, id, date],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(d) = dup {
                return Err(AppError::conflict(format!("Invoice number {} of this supplier is already posted ({d}).", number.unwrap_or_default()))
                    .with_details(json!({ "kind": "duplicate_number", "other": d })));
            }
            let now = time::now_str();
            if doc_type == "invoice" {
                let (due, rule) = due_date(&date, due.as_deref(), terms.as_deref())?;
                tx.execute(
                    "INSERT INTO ap_liabilities(liability_id, supplier_id, invoice_id, doc_date, due_date, due_rule, amount_minor, currency, status, operation_id, posted_by, posted_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'open',?9,?10,?11)",
                    params![new_id(), supplier, id, date, due, rule, total, currency, operation_id, s.user_id, now],
                )?;
                // The supplier's costs join the canonical purchase-cost history.
                let lines: Vec<(String, i64)> = {
                    let mut st = tx.prepare(
                        "SELECT l.product_id, l.unit_cost_minor FROM supplier_invoice_lines l JOIN products p ON p.product_id=l.product_id
                         WHERE l.invoice_id=?1 AND l.qty_milli > 0",
                    )?;
                    let r = st.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
                    r
                };
                for (pid, cost) in lines {
                    tx.execute(
                        "INSERT INTO product_cost_history(cost_id, product_id, supplier_id, cost_minor, source, source_id, effective_at, created_by)
                         VALUES (?1,?2,?3,?4,'supplier_invoice',?5,?6,?7)",
                        params![new_id(), pid, supplier, cost, id, format!("{date}T00:00:00Z"), s.user_id],
                    )?;
                }
            } else {
                if let Some(orig) = &applies {
                    let ok: Option<String> = tx.query_row("SELECT supplier_id FROM supplier_invoices WHERE invoice_id=?1", [orig], |r| r.get(0)).optional()?;
                    if ok.as_deref() != Some(supplier.as_str()) {
                        return Err(AppError::validation("The credit note refers to an invoice of another supplier."));
                    }
                }
                tx.execute(
                    "INSERT INTO ap_credits(credit_id, supplier_id, invoice_id, applies_to_invoice_id, doc_date, amount_minor, currency, status, operation_id, posted_by, posted_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,'open',?8,?9,?10)",
                    params![new_id(), supplier, id, applies, date, total, currency, operation_id, s.user_id, now],
                )?;
            }
            tx.execute(
                "UPDATE supplier_invoices SET posting='posted', posted_at=?2, posted_by=?3, updated_at=?2, revision=revision+1 WHERE invoice_id=?1",
                params![id, now, s.user_id],
            )?;
            audit::record(
                tx,
                &actor,
                if doc_type == "invoice" { "ap.invoice_posted" } else { "ap.credit_posted" },
                "supplier_invoice",
                Some(&id),
                Some(&json!({ "posting": "not_posted" })),
                Some(&json!({ "posting": "posted", "amount_minor": total, "supplier_id": supplier, "operation_id": operation_id, "origin": "admin" })),
            )?;
            Ok(())
        })?;
        self.ap_invoice_get(token, &id)
    }

    /// Reverse a posted invoice or credit note. Its payments / credit
    /// applications must be undone first, so no settled amount is orphaned.
    pub fn ap_invoice_reverse(&self, token: &str, invoice_id: &str, reason: &str) -> AppResult<Value> {
        let s = self.ap_session(token, "payables.post")?;
        let id = validate::id(invoice_id, "Supplier invoice")?;
        let reason = reason.trim().chars().take(300).collect::<String>();
        if reason.is_empty() {
            return Err(AppError::validation("Enter the reason for the reversal."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let (posting, doc_type): (String, String) = tx
                .query_row("SELECT posting, doc_type FROM supplier_invoices WHERE invoice_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| AppError::not_found("Supplier invoice"))?;
            if posting != "posted" {
                return Err(AppError::conflict("Only a posted supplier invoice can be reversed."));
            }
            let now = time::now_str();
            let (table, key) = if doc_type == "invoice" { ("ap_liabilities", "liability_id") } else { ("ap_credits", "credit_id") };
            let (rid, amount): (String, i64) =
                tx.query_row(&format!("SELECT {key}, amount_minor FROM {table} WHERE invoice_id=?1"), [&id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let used = if doc_type == "invoice" { allocated_to_liability(tx, &rid)? } else { allocated_from_credit(tx, &rid)? };
            if used > 0 {
                return Err(AppError::conflict("Payments or credits are applied to it. Remove those allocations first.")
                    .with_details(json!({ "kind": "allocated", "amount_minor": used })));
            }
            tx.execute(
                &format!("UPDATE {table} SET status='reversed', reversed_by=?2, reversed_at=?3, reversal_reason=?4 WHERE {key}=?1"),
                params![rid, s.user_id, now, reason],
            )?;
            tx.execute(
                "UPDATE supplier_invoices SET posting='reversed', reversed_at=?2, reversed_by=?3, reversal_reason=?4, updated_at=?2, revision=revision+1 WHERE invoice_id=?1",
                params![id, now, s.user_id, reason],
            )?;
            audit::record(
                tx,
                &actor,
                "ap.reversed",
                "supplier_invoice",
                Some(&id),
                Some(&json!({ "posting": "posted", "amount_minor": amount })),
                Some(&json!({ "posting": "reversed", "reason": reason, "origin": "admin" })),
            )?;
            Ok(())
        })?;
        self.ap_invoice_get(token, &id)
    }

    /// Record a supplier payment and (optionally) allocate it. Idempotent on
    /// the operation id: a double-submit returns the same payment.
    pub fn ap_payment_record(&self, token: &str, input: PaymentInput) -> AppResult<Value> {
        let s = self.ap_session(token, "payables.pay")?;
        crate::idempotency::validate_operation_id(&input.operation_id)?;
        let supplier = validate::id(&input.supplier_id, "Supplier")?;
        if !METHODS.contains(&input.method.as_str()) {
            return Err(AppError::validation("Unknown payment method."));
        }
        if input.amount_minor <= 0 {
            return Err(AppError::validation("The amount must be more than zero."));
        }
        let paid_on = parse_date(&input.paid_on, "Payment date")?;
        if paid_on > today() + chrono::Duration::days(1) {
            return Err(AppError::validation("The payment date is in the future."));
        }
        let actor = self.actor(&s, None);
        let pid = self.db.write(|tx| {
            if let Some((existing, sup, amount)) = tx
                .query_row("SELECT payment_id, supplier_id, amount_minor FROM ap_payments WHERE operation_id=?1", [&input.operation_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
                })
                .optional()?
            {
                if sup != supplier || amount != input.amount_minor {
                    return Err(AppError::new(ErrorCode::IdempotencyMismatch, "This operation id was used for a different payment. Nothing was changed."));
                }
                return Ok(existing);
            }
            tx.query_row("SELECT 1 FROM suppliers WHERE supplier_id=?1", [&supplier], |_| Ok(())).optional()?.ok_or_else(|| AppError::not_found("Supplier"))?;
            let id = new_id();
            let number = format!("SP-{:05}", next_seq(tx, "supplier_payment")?);
            let now = time::now_str();
            tx.execute(
                "INSERT INTO ap_payments(payment_id, number, supplier_id, paid_on, amount_minor, method, reference, notes, status, operation_id, created_by, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'posted',?9,?10,?11)",
                params![
                    id,
                    number,
                    supplier,
                    paid_on.to_string(),
                    input.amount_minor,
                    input.method,
                    input.reference.as_deref().map(|r| r.trim().chars().take(80).collect::<String>()),
                    input.notes.as_deref().map(|r| r.trim().chars().take(300).collect::<String>()),
                    input.operation_id,
                    s.user_id,
                    now
                ],
            )?;
            audit::record(
                tx,
                &actor,
                "ap.payment_recorded",
                "supplier_payment",
                Some(&id),
                None,
                Some(&json!({ "number": number, "supplier_id": supplier, "amount_minor": input.amount_minor, "method": input.method, "origin": "admin" })),
            )?;
            let mut plan: Vec<(String, i64)> = input.allocations.iter().map(|a| (a.invoice_id.clone(), a.amount_minor)).collect();
            if input.oldest_first {
                let explicit: i64 = plan.iter().map(|p| p.1).sum();
                let mut left = input.amount_minor - explicit;
                for (inv, out) in open_items(tx, &supplier)? {
                    if left <= 0 {
                        break;
                    }
                    let already: i64 = plan.iter().filter(|p| p.0 == inv).map(|p| p.1).sum();
                    let take = (out - already).min(left);
                    if take > 0 {
                        plan.push((inv, take));
                        left -= take;
                    }
                }
            }
            allocate(tx, &actor, &s, Source::Payment(&id), &supplier, input.amount_minor, &plan)?;
            Ok(id)
        })?;
        self.ap_payment_get(token, &pid)
    }

    /// Allocate a payment's or credit's unallocated remainder.
    pub fn ap_allocate(
        &self,
        token: &str,
        payment_id: Option<String>,
        credit_id: Option<String>,
        allocations: Vec<AllocationInput>,
    ) -> AppResult<Value> {
        let s = self.ap_session(token, "payables.pay")?;
        let actor = self.actor(&s, None);
        let plan: Vec<(String, i64)> = allocations.iter().map(|a| (a.invoice_id.clone(), a.amount_minor)).collect();
        self.db.write(|tx| match (&payment_id, &credit_id) {
            (Some(p), None) => {
                let (sup, amount, status): (String, i64, String) = tx
                    .query_row("SELECT supplier_id, amount_minor, status FROM ap_payments WHERE payment_id=?1", [p], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })
                    .optional()?
                    .ok_or_else(|| AppError::not_found("Supplier payment"))?;
                if status != "posted" {
                    return Err(AppError::conflict("This payment was reversed."));
                }
                let left = amount - allocated_from_payment(tx, p)?;
                allocate(tx, &actor, &s, Source::Payment(p), &sup, left, &plan)
            }
            (None, Some(c)) => {
                let (sup, amount, status): (String, i64, String) = tx
                    .query_row("SELECT supplier_id, amount_minor, status FROM ap_credits WHERE credit_id=?1", [c], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })
                    .optional()?
                    .ok_or_else(|| AppError::not_found("Supplier credit"))?;
                if status != "open" {
                    return Err(AppError::conflict("This credit was reversed."));
                }
                let left = amount - allocated_from_credit(tx, c)?;
                allocate(tx, &actor, &s, Source::Credit(c), &sup, left, &plan)
            }
            _ => Err(AppError::validation("Choose a payment or a credit to allocate.")),
        })?;
        Ok(json!({ "ok": true }))
    }

    /// Undo one allocation (the payment / credit becomes available again).
    pub fn ap_allocation_reverse(&self, token: &str, allocation_id: &str) -> AppResult<Value> {
        let s = self.ap_session(token, "payables.pay")?;
        let id = validate::id(allocation_id, "Allocation")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let n = tx.execute(
                "UPDATE ap_allocations SET status='reversed', reversed_by=?2, reversed_at=?3 WHERE allocation_id=?1 AND status='active'",
                params![id, s.user_id, time::now_str()],
            )?;
            if n == 0 {
                return Err(AppError::conflict("This allocation is not active."));
            }
            audit::record(tx, &actor, "ap.allocation_reversed", "ap_allocation", Some(&id), None, Some(&json!({ "origin": "admin" })))?;
            Ok(())
        })?;
        Ok(json!({ "ok": true }))
    }

    /// Reverse a payment (and its allocations): the invoices it settled are open again.
    pub fn ap_payment_reverse(&self, token: &str, payment_id: &str, reason: &str) -> AppResult<Value> {
        let s = self.ap_session(token, "payables.pay")?;
        let id = validate::id(payment_id, "Supplier payment")?;
        let reason = reason.trim().chars().take(300).collect::<String>();
        if reason.is_empty() {
            return Err(AppError::validation("Enter the reason for the reversal."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let now = time::now_str();
            let n = tx.execute(
                "UPDATE ap_payments SET status='reversed', reversed_by=?2, reversed_at=?3, reversal_reason=?4 WHERE payment_id=?1 AND status='posted'",
                params![id, s.user_id, now, reason],
            )?;
            if n == 0 {
                return Err(AppError::conflict("This payment is not posted."));
            }
            let undone = tx.execute(
                "UPDATE ap_allocations SET status='reversed', reversed_by=?2, reversed_at=?3 WHERE payment_id=?1 AND status='active'",
                params![id, s.user_id, now],
            )?;
            audit::record(tx, &actor, "ap.payment_reversed", "supplier_payment", Some(&id), None, Some(&json!({ "reason": reason, "allocations": undone, "origin": "admin" })))?;
            Ok(())
        })?;
        self.ap_payment_get(token, &id)
    }

    pub fn ap_payment_get(&self, token: &str, payment_id: &str) -> AppResult<Value> {
        let _ = self.ap_session(token, "payables.view")?;
        let id = validate::id(payment_id, "Supplier payment")?;
        self.db.read(|c| {
            let mut v = c
                .query_row(
                    "SELECT p.payment_id, p.number, p.supplier_id, s.name, p.paid_on, p.amount_minor, p.method, p.reference, p.notes, p.status, p.created_at,
                            u.display_name, p.reversed_at, p.reversal_reason
                     FROM ap_payments p JOIN suppliers s ON s.supplier_id=p.supplier_id LEFT JOIN users u ON u.user_id=p.created_by WHERE p.payment_id=?1",
                    [&id],
                    |r| {
                        Ok(json!({ "payment_id": r.get::<_, String>(0)?, "number": r.get::<_, String>(1)?, "supplier_id": r.get::<_, String>(2)?,
                                   "supplier_name": r.get::<_, String>(3)?, "paid_on": r.get::<_, String>(4)?, "amount_minor": r.get::<_, i64>(5)?,
                                   "method": r.get::<_, String>(6)?, "reference": r.get::<_, Option<String>>(7)?, "notes": r.get::<_, Option<String>>(8)?,
                                   "status": r.get::<_, String>(9)?, "created_at": r.get::<_, String>(10)?, "created_by_name": r.get::<_, Option<String>>(11)?,
                                   "reversed_at": r.get::<_, Option<String>>(12)?, "reversal_reason": r.get::<_, Option<String>>(13)? }))
                    },
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Supplier payment"))?;
            let alloc = allocated_from_payment(c, &id)?;
            v["allocated_minor"] = json!(alloc);
            v["unallocated_minor"] = json!(if v["status"] == "posted" { v["amount_minor"].as_i64().unwrap_or(0) - alloc } else { 0 });
            v["allocations"] = json!(allocations_of(c, "payment_id", &id)?);
            Ok(v)
        })
    }

    /// One supplier invoice / credit note record with its AP state.
    pub fn ap_invoice_get(&self, token: &str, invoice_id: &str) -> AppResult<Value> {
        let s = self.ap_session(token, "payables.view")?;
        let id = validate::id(invoice_id, "Supplier invoice")?;
        let mut v = self.supplier_invoice_get_as(&s, &id)?;
        self.db.read(|c| {
            let (life, outstanding) = lifecycle(c, &id)?;
            v["lifecycle"] = json!(life);
            v["outstanding_minor"] = json!(outstanding);
            let liab: Option<Value> = c
                .query_row(
                    "SELECT liability_id, due_date, due_rule, amount_minor, status, posted_at, reversed_at, reversal_reason FROM ap_liabilities WHERE invoice_id=?1",
                    [&id],
                    |r| {
                        Ok(json!({ "liability_id": r.get::<_, String>(0)?, "due_date": r.get::<_, String>(1)?, "due_rule": r.get::<_, String>(2)?,
                                   "amount_minor": r.get::<_, i64>(3)?, "status": r.get::<_, String>(4)?, "posted_at": r.get::<_, String>(5)?,
                                   "reversed_at": r.get::<_, Option<String>>(6)?, "reversal_reason": r.get::<_, Option<String>>(7)? }))
                    },
                )
                .optional()?;
            if let Some(l) = &liab {
                v["allocations"] = json!(allocations_of(c, "liability_id", l["liability_id"].as_str().unwrap_or_default())?);
            }
            v["liability"] = liab.unwrap_or(Value::Null);
            v["credit"] = c
                .query_row("SELECT credit_id, amount_minor, status, applies_to_invoice_id FROM ap_credits WHERE invoice_id=?1", [&id], |r| {
                    Ok(json!({ "credit_id": r.get::<_, String>(0)?, "amount_minor": r.get::<_, i64>(1)?, "status": r.get::<_, String>(2)?,
                               "applies_to_invoice_id": r.get::<_, Option<String>>(3)? }))
                })
                .optional()?
                .unwrap_or(Value::Null);
            v["posting_note"] = json!(system_note());
            Ok(())
        })?;
        Ok(v)
    }

    /// The Payables overview: what is owed, what is due, per supplier.
    pub fn ap_overview(&self, token: &str) -> AppResult<Value> {
        let _ = self.ap_session(token, "payables.view")?;
        let as_of = today();
        self.db.read(|c| {
            let open = all_open_items(c)?;
            let mut per: HashMap<String, Value> = HashMap::new();
            let mut ageing: HashMap<&str, i64> = BUCKETS.iter().map(|b| (b.0, 0)).collect();
            let (mut overdue, mut due_soon) = (0i64, 0i64);
            for it in &open {
                let b = bucket(&it.due_date, as_of);
                *ageing.get_mut(b).unwrap() += it.outstanding;
                if b != "current" {
                    overdue += it.outstanding;
                } else if parse_date(&it.due_date, "due").map(|d| (d - as_of).num_days() <= 7).unwrap_or(false) {
                    due_soon += it.outstanding;
                }
                let e = per.entry(it.supplier_id.clone()).or_insert_with(|| json!({ "supplier_id": it.supplier_id, "supplier_name": it.supplier_name,
                    "open_invoices": 0, "outstanding_minor": 0, "overdue_minor": 0 }));
                e["open_invoices"] = json!(e["open_invoices"].as_i64().unwrap_or(0) + 1);
                e["outstanding_minor"] = json!(e["outstanding_minor"].as_i64().unwrap_or(0) + it.outstanding);
                if b != "current" {
                    e["overdue_minor"] = json!(e["overdue_minor"].as_i64().unwrap_or(0) + it.outstanding);
                }
            }
            // Balances also count unapplied credits and unallocated payments.
            let mut suppliers: Vec<Value> = vec![];
            let ids: Vec<String> = {
                let mut st = c.prepare(
                    "SELECT supplier_id FROM ap_liabilities UNION SELECT supplier_id FROM ap_credits UNION SELECT supplier_id FROM ap_payments",
                )?;
                let r = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
                r
            };
            for sid in ids {
                let bal = supplier_balance(c, &sid)?;
                let mut e = per.remove(&sid).unwrap_or_else(|| json!({ "supplier_id": sid, "open_invoices": 0, "outstanding_minor": 0, "overdue_minor": 0 }));
                if e["supplier_name"].is_null() {
                    e["supplier_name"] = json!(c.query_row("SELECT name FROM suppliers WHERE supplier_id=?1", [&sid], |r| r.get::<_, String>(0))?);
                }
                e["balance_minor"] = json!(bal);
                if bal != 0 || e["open_invoices"].as_i64().unwrap_or(0) > 0 {
                    suppliers.push(e);
                }
            }
            suppliers.sort_by_key(|e| -e["balance_minor"].as_i64().unwrap_or(0));
            let n = |sql: &str| -> AppResult<i64> { Ok(c.query_row(sql, [], |r| r.get(0))?) };
            Ok(json!({
                "as_of": as_of.to_string(),
                "outstanding_minor": open.iter().map(|i| i.outstanding).sum::<i64>(),
                "overdue_minor": overdue,
                "due_within_7_days_minor": due_soon,
                "ageing": BUCKETS.iter().map(|b| json!({ "bucket": b.0, "amount_minor": ageing[b.0] })).collect::<Vec<_>>(),
                "suppliers": suppliers,
                "to_review": n("SELECT COUNT(*) FROM supplier_invoices WHERE status='draft'")?,
                "to_post": n("SELECT COUNT(*) FROM supplier_invoices WHERE status='approved' AND posting='not_posted'")?,
                "overdue_invoices": open.iter().filter(|i| bucket(&i.due_date, as_of) != "current").count(),
                "note": "Only posted records count here. Draft and approved-but-not-posted invoices are not owed yet.",
            }))
        })
    }

    /// One supplier's account: balance, open invoices, credits, payments,
    /// ageing and a dated statement with a running balance.
    pub fn ap_supplier(&self, token: &str, supplier_id: &str) -> AppResult<Value> {
        let _ = self.ap_session(token, "payables.view")?;
        let sid = validate::id(supplier_id, "Supplier")?;
        let as_of = today();
        self.db.read(|c| {
            let name: String = c.query_row("SELECT name FROM suppliers WHERE supplier_id=?1", [&sid], |r| r.get(0)).optional()?.ok_or_else(|| AppError::not_found("Supplier"))?;
            let balance = supplier_balance(c, &sid)?;
            let open: Vec<OpenItem> = all_open_items(c)?.into_iter().filter(|i| i.supplier_id == sid).collect();
            let mut ageing: HashMap<&str, i64> = BUCKETS.iter().map(|b| (b.0, 0)).collect();
            for it in &open {
                *ageing.get_mut(bucket(&it.due_date, as_of)).unwrap() += it.outstanding;
            }
            let open_json: Vec<Value> = open
                .iter()
                .map(|i| {
                    let late = parse_date(&i.due_date, "due").map(|d| (as_of - d).num_days()).unwrap_or(0);
                    json!({ "invoice_id": i.invoice_id, "number": i.number, "invoice_number": i.invoice_number, "doc_date": i.doc_date, "due_date": i.due_date,
                            "amount_minor": i.amount, "outstanding_minor": i.outstanding, "days_overdue": late.max(0),
                            "payment_status": payment_status(i.amount, i.amount - i.outstanding), "bucket": bucket(&i.due_date, as_of) })
                })
                .collect();
            let mut st = c.prepare(
                "SELECT c.credit_id, i.number, i.invoice_number, c.doc_date, c.amount_minor, c.status, c.invoice_id FROM ap_credits c
                 JOIN supplier_invoices i ON i.invoice_id=c.invoice_id WHERE c.supplier_id=?1 ORDER BY c.doc_date DESC",
            )?;
            let credits: Vec<Value> = st
                .query_map([&sid], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?, r.get::<_, String>(5)?, r.get::<_, String>(6)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|(id, number, inv_no, date, amount, status, inv)| {
                    let used = allocated_from_credit(c, &id).unwrap_or(0);
                    json!({ "credit_id": id, "invoice_id": inv, "number": number, "invoice_number": inv_no, "doc_date": date, "amount_minor": amount,
                            "status": status, "unapplied_minor": if status == "open" { amount - used } else { 0 } })
                })
                .collect();
            let mut st = c.prepare(
                "SELECT payment_id, number, paid_on, amount_minor, method, reference, status FROM ap_payments WHERE supplier_id=?1 ORDER BY paid_on DESC, created_at DESC",
            )?;
            let payments: Vec<Value> = st
                .query_map([&sid], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, String>(4)?, r.get::<_, Option<String>>(5)?, r.get::<_, String>(6)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|(id, number, date, amount, method, reference, status)| {
                    let used = allocated_from_payment(c, &id).unwrap_or(0);
                    json!({ "payment_id": id, "number": number, "paid_on": date, "amount_minor": amount, "method": method, "reference": reference,
                            "status": status, "unallocated_minor": if status == "posted" { amount - used } else { 0 } })
                })
                .collect();
            // Statement: every posted document and payment, oldest first, with a running balance.
            let mut st = c.prepare(
                "SELECT d, kind, ref, amt FROM (
                   SELECT l.doc_date AS d, 'invoice' AS kind, COALESCE(i.invoice_number, i.number) AS ref, l.amount_minor AS amt, l.posted_at AS t
                     FROM ap_liabilities l JOIN supplier_invoices i ON i.invoice_id=l.invoice_id WHERE l.supplier_id=?1 AND l.status='open'
                   UNION ALL SELECT c.doc_date, 'credit_note', COALESCE(i.invoice_number, i.number), -c.amount_minor, c.posted_at
                     FROM ap_credits c JOIN supplier_invoices i ON i.invoice_id=c.invoice_id WHERE c.supplier_id=?1 AND c.status='open'
                   UNION ALL SELECT p.paid_on, 'payment', COALESCE(p.reference, p.number), -p.amount_minor, p.created_at
                     FROM ap_payments p WHERE p.supplier_id=?1 AND p.status='posted')
                 ORDER BY d, t",
            )?;
            let mut running = 0i64;
            let statement: Vec<Value> = st
                .query_map([&sid], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, i64>(3)?)))?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|(d, kind, reference, amt)| {
                    running += amt;
                    json!({ "date": d, "kind": kind, "reference": reference, "amount_minor": amt, "balance_minor": running })
                })
                .collect();
            Ok(json!({
                "supplier_id": sid, "supplier_name": name, "balance_minor": balance, "as_of": as_of.to_string(),
                "open_invoices": open_json, "credits": credits, "payments": payments,
                "ageing": BUCKETS.iter().map(|b| json!({ "bucket": b.0, "amount_minor": ageing[b.0] })).collect::<Vec<_>>(),
                "statement": statement,
            }))
        })
    }

    /// Supplier purchase history for a product from the canonical cost
    /// history: latest and previous cost, change, cheapest recent supplier,
    /// and the gross-margin effect at today's selling price. Nothing changes a price.
    pub fn purchase_costs(&self, token: &str, product_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("products.view_cost") && !s.has("payables.view") && !s.has("purchasing.manage") {
            return Err(AppError::forbidden("products.view_cost"));
        }
        let pid = validate::id(product_id, "Product")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT h.doc_date, h.supplier_id, s.name, h.source, h.source_id, h.unit_cost_minor, h.qty_milli FROM purchase_cost_history h
                 LEFT JOIN suppliers s ON s.supplier_id=h.supplier_id WHERE h.product_id=?1 ORDER BY h.effective_at DESC, h.cost_id DESC LIMIT 100",
            )?;
            let rows: Vec<(String, Option<String>, Option<String>, String, Option<String>, i64, Option<i64>)> =
                st.query_map([&pid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?.collect::<Result<_, _>>()?;
            let price: Option<i64> = c
                .query_row(&format!("SELECT {} FROM products p WHERE p.product_id=?1", crate::catalog::PRICE_SQL), [&pid], |r| r.get(0))
                .optional()?
                .flatten();
            let latest = rows.first().map(|r| r.5);
            let previous = rows.iter().skip(1).map(|r| r.5).find(|c| Some(*c) != latest).or_else(|| rows.get(1).map(|r| r.5));
            let change = latest.zip(previous).map(|(a, b)| a - b);
            // Percentage in basis points (integer, exact).
            let change_bp = latest.zip(previous).and_then(|(a, b)| (b > 0).then(|| (a - b) * 10_000 / b));
            let since = time::fmt(time::now() - chrono::Duration::days(90)).get(..10).unwrap_or("").to_string();
            let mut best: HashMap<String, (i64, String)> = HashMap::new();
            for r in rows.iter().filter(|r| r.0 >= since) {
                if let Some(sid) = &r.1 {
                    let e = best.entry(sid.clone()).or_insert((r.5, r.2.clone().unwrap_or_default()));
                    // Newest cost per supplier (rows are newest first).
                    let _ = e;
                }
            }
            let cheapest = best.iter().min_by_key(|(_, v)| v.0).map(|(k, v)| json!({ "supplier_id": k, "supplier_name": v.1, "unit_cost_minor": v.0 }));
            let margin = |cost: Option<i64>| -> Value {
                match (price, cost) {
                    (Some(p), Some(c)) if p > 0 => json!({ "margin_minor": p - c, "margin_bp": (p - c) * 10_000 / p }),
                    _ => Value::Null,
                }
            };
            Ok(json!({
                "product_id": pid,
                "selling_price_minor": price,
                "latest_cost_minor": latest,
                "previous_cost_minor": previous,
                "change_minor": change,
                "change_bp": change_bp,
                "margin_now": margin(latest),
                "margin_before": margin(previous),
                "cheapest_recent_supplier": cheapest,
                "history": rows.iter().map(|r| json!({ "date": r.0, "supplier_id": r.1, "supplier_name": r.2, "source": r.3, "source_id": r.4,
                                                        "unit_cost_minor": r.5, "qty_milli": r.6 })).collect::<Vec<_>>(),
                "note": "Costs from posted receiving and posted supplier invoices. Selling prices change only when a person changes them.",
            }))
        })
    }
}

enum Source<'a> {
    Payment(&'a str),
    Credit(&'a str),
}

/// Write allocations after checking each one: the invoice belongs to the
/// supplier, is posted and open, and neither it nor the source is over-used.
fn allocate(
    tx: &Connection,
    actor: &audit::Actor,
    s: &Session,
    src: Source,
    supplier: &str,
    available: i64,
    plan: &[(String, i64)],
) -> AppResult<()> {
    let total: i64 = plan.iter().map(|p| p.1).sum();
    if total > available {
        return Err(AppError::validation("The allocations are more than the amount available to allocate.")
            .with_details(json!({ "kind": "over_allocation", "available_minor": available, "requested_minor": total })));
    }
    let mut used: HashMap<String, i64> = HashMap::new();
    for (inv, amount) in plan {
        if *amount <= 0 {
            return Err(AppError::validation("Each allocation must be more than zero."));
        }
        let (lid, sup, lamount, status): (String, String, i64, String) = tx
            .query_row("SELECT liability_id, supplier_id, amount_minor, status FROM ap_liabilities WHERE invoice_id=?1", [inv], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .optional()?
            .ok_or_else(|| AppError::validation("Payments can be allocated only to posted invoices."))?;
        if sup != supplier {
            return Err(AppError::validation("That invoice belongs to another supplier."));
        }
        if status != "open" {
            return Err(AppError::validation("That invoice was reversed."));
        }
        let outstanding = lamount - allocated_to_liability(tx, &lid)? - used.get(&lid).copied().unwrap_or(0);
        if *amount > outstanding {
            return Err(AppError::validation("An allocation is more than the invoice's outstanding amount.")
                .with_details(json!({ "kind": "over_invoice", "invoice_id": inv, "outstanding_minor": outstanding })));
        }
        *used.entry(lid.clone()).or_default() += *amount;
        let id = new_id();
        let (kind, pid, cid) = match &src {
            Source::Payment(p) => ("payment", Some(*p), None),
            Source::Credit(c) => ("credit", None, Some(*c)),
        };
        tx.execute(
            "INSERT INTO ap_allocations(allocation_id, kind, payment_id, credit_id, liability_id, amount_minor, status, created_by, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,'active',?7,?8)",
            params![id, kind, pid, cid, lid, amount, s.user_id, time::now_str()],
        )?;
        audit::record(
            tx,
            actor,
            "ap.allocated",
            "ap_allocation",
            Some(&id),
            None,
            Some(&json!({ "kind": kind, "invoice_id": inv, "amount_minor": amount, "origin": "admin" })),
        )?;
    }
    Ok(())
}

struct OpenItem {
    supplier_id: String,
    supplier_name: String,
    invoice_id: String,
    number: String,
    invoice_number: Option<String>,
    doc_date: String,
    due_date: String,
    amount: i64,
    outstanding: i64,
}

/// Every posted, unreversed invoice with something still outstanding.
fn all_open_items(c: &Connection) -> AppResult<Vec<OpenItem>> {
    let mut st = c.prepare(
        "SELECT l.supplier_id, s.name, l.invoice_id, i.number, i.invoice_number, l.doc_date, l.due_date, l.amount_minor,
                l.amount_minor - COALESCE((SELECT SUM(a.amount_minor) FROM ap_allocations a WHERE a.liability_id=l.liability_id AND a.status='active'),0)
         FROM ap_liabilities l JOIN supplier_invoices i ON i.invoice_id=l.invoice_id JOIN suppliers s ON s.supplier_id=l.supplier_id
         WHERE l.status='open' ORDER BY l.due_date, l.doc_date",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(OpenItem {
                supplier_id: r.get(0)?,
                supplier_name: r.get(1)?,
                invoice_id: r.get(2)?,
                number: r.get(3)?,
                invoice_number: r.get(4)?,
                doc_date: r.get(5)?,
                due_date: r.get(6)?,
                amount: r.get(7)?,
                outstanding: r.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().filter(|i| i.outstanding > 0).collect())
}

/// (invoice id, outstanding) of a supplier's open invoices, oldest due first.
fn open_items(c: &Connection, supplier: &str) -> AppResult<Vec<(String, i64)>> {
    Ok(all_open_items(c)?.into_iter().filter(|i| i.supplier_id == supplier).map(|i| (i.invoice_id, i.outstanding)).collect())
}

fn allocations_of(c: &Connection, col: &str, id: &str) -> AppResult<Vec<Value>> {
    let mut st = c.prepare(&format!(
        "SELECT a.allocation_id, a.kind, a.payment_id, p.number, a.credit_id, a.liability_id, i.invoice_id, i.number, i.invoice_number, a.amount_minor, a.status, a.created_at
         FROM ap_allocations a JOIN ap_liabilities l ON l.liability_id=a.liability_id JOIN supplier_invoices i ON i.invoice_id=l.invoice_id
         LEFT JOIN ap_payments p ON p.payment_id=a.payment_id WHERE a.{col}=?1 ORDER BY a.created_at"
    ))?;
    let rows = st
        .query_map([id], |r| {
            Ok(json!({ "allocation_id": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?, "payment_id": r.get::<_, Option<String>>(2)?,
                       "payment_number": r.get::<_, Option<String>>(3)?, "credit_id": r.get::<_, Option<String>>(4)?, "liability_id": r.get::<_, String>(5)?,
                       "invoice_id": r.get::<_, String>(6)?, "invoice_record": r.get::<_, String>(7)?, "invoice_number": r.get::<_, Option<String>>(8)?,
                       "amount_minor": r.get::<_, i64>(9)?, "status": r.get::<_, String>(10)?, "created_at": r.get::<_, String>(11)? }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_dates_follow_a_stated_rule() {
        assert_eq!(due_date("2026-09-01", Some("2026-10-15"), Some("30 days")).unwrap(), ("2026-10-15".into(), "invoice"));
        assert_eq!(due_date("2026-09-01", None, Some("Net 30")).unwrap(), ("2026-10-01".into(), "terms"));
        assert_eq!(due_date("2026-09-01", None, Some("٤٥ يوم")).unwrap(), ("2026-10-16".into(), "terms"));
        assert_eq!(due_date("2026-09-01", None, Some("Cash")).unwrap(), ("2026-09-01".into(), "terms"));
        assert_eq!(due_date("2026-09-01", None, Some("monthly statement")).unwrap(), ("2026-09-01".into(), "invoice_date"));
        assert_eq!(due_date("2026-09-01", None, None).unwrap(), ("2026-09-01".into(), "invoice_date"));
        assert!(due_date("2026-09-01", Some("2026-08-01"), None).is_err(), "due before the invoice date");
        assert!(due_date("01/09/2026", None, None).is_err());
    }

    #[test]
    fn ageing_buckets() {
        let d = NaiveDate::from_ymd_opt(2026, 9, 29).unwrap();
        assert_eq!(bucket("2026-10-10", d), "current");
        assert_eq!(bucket("2026-09-29", d), "current", "due today is not overdue");
        assert_eq!(bucket("2026-09-28", d), "1_30");
        assert_eq!(bucket("2026-08-30", d), "1_30");
        assert_eq!(bucket("2026-08-29", d), "31_60");
        assert_eq!(bucket("2026-07-31", d), "31_60", "60 days late");
        assert_eq!(bucket("2026-07-30", d), "61_90");
        assert_eq!(bucket("2026-06-30", d), "90_plus");
    }

    #[test]
    fn payment_states() {
        assert_eq!(payment_status(1_250, 0), "unpaid");
        assert_eq!(payment_status(1_250, 1), "partially_paid");
        assert_eq!(payment_status(1_250, 1_250), "paid");
    }
}
