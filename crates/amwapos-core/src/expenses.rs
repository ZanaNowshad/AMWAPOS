//! Operating expenses and petty cash (docs/MERCHANT_OS_PLAN.md, Wave 1).
//!
//! Expense lifecycle
//! -----------------
//! draft → submitted → approved → paid, or submitted → rejected; an approved
//! or paid expense is corrected by voiding it (with a reason) and entering it
//! again. Drafts are the only editable or deletable state; the database
//! refuses changes to amounts, category, date or branch after submission.
//! Someone who may approve, or an expense at or below the store's
//! auto-approve amount, is approved on entry (recorded as such).
//!
//! Money: integer fils; total = net + VAT, checked by the database. Operating
//! profit counts `net_minor` of approved and paid expenses by business date.
//!
//! Petty cash is a fund apart from the till drawer. Its balance is the sum of
//! its append-only entries; paying an expense from it writes an entry; a
//! void writes a compensating one; a count records what was found.
//!
//! Everything here is back office: written on the hub, not replicated.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::{new_id, next_seq};
use crate::service::AppCore;
use crate::settings::{self, ExpenseSettings};
use crate::setup::clean;
use crate::time;
use crate::validate;

pub const PAY_METHODS: [&str; 6] = ["petty_cash", "till_paid_out", "bank_transfer", "card", "cheque", "other"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseInput {
    pub category_id: String,
    #[serde(default)]
    pub business_date: Option<String>,
    #[serde(default)]
    pub supplier_id: Option<String>,
    #[serde(default)]
    pub payee: Option<String>,
    pub description: String,
    /// What was paid, VAT included.
    pub total_minor: i64,
    /// The VAT in that total (0 when there is none).
    #[serde(default)]
    pub vat_minor: i64,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseRow {
    pub expense_id: String,
    pub number: String,
    pub business_date: String,
    pub category_id: String,
    pub category: String,
    pub category_ar: Option<String>,
    pub supplier_id: Option<String>,
    pub payee: Option<String>,
    pub description: String,
    pub net_minor: i64,
    pub vat_minor: i64,
    pub total_minor: i64,
    pub status: String,
    pub payment_method: Option<String>,
    pub fund_id: Option<String>,
    pub cash_event_id: Option<String>,
    pub reference: Option<String>,
    pub notes: Option<String>,
    pub recurring_id: Option<String>,
    pub created_by_name: Option<String>,
    pub decided_by_name: Option<String>,
    pub decision_note: Option<String>,
    pub paid_at: Option<String>,
    pub void_reason: Option<String>,
    pub attachments: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayInput {
    pub method: String,
    #[serde(default)]
    pub fund_id: Option<String>,
    #[serde(default)]
    pub cash_event_id: Option<String>,
    #[serde(default)]
    pub reference: Option<String>,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurringInput {
    pub name: String,
    pub category_id: String,
    #[serde(default)]
    pub supplier_id: Option<String>,
    #[serde(default)]
    pub payee: Option<String>,
    pub description: String,
    pub total_minor: i64,
    #[serde(default)]
    pub vat_minor: i64,
    /// monthly | weekly
    pub cadence: String,
    /// Day of month (1–28) or weekday (1 = Monday … 7 = Sunday).
    pub day: i64,
    #[serde(default = "yes")]
    pub active: bool,
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct FundRow {
    pub fund_id: String,
    pub name: String,
    pub custodian_user_id: Option<String>,
    pub custodian_name: Option<String>,
    pub balance_minor: i64,
    pub last_count_at: Option<String>,
    pub active: bool,
}

const ROW_SQL: &str = "SELECT e.expense_id, e.number, e.business_date, e.category_id, c.name, c.name_ar, e.supplier_id,
        COALESCE(e.payee, s.name), e.description, e.net_minor, e.vat_minor, e.total_minor, e.status, e.payment_method, e.fund_id,
        e.cash_event_id, e.reference, e.notes, e.recurring_id, cu.display_name, du.display_name, e.decision_note, e.paid_at,
        e.void_reason, (SELECT COUNT(*) FROM expense_attachments a WHERE a.expense_id=e.expense_id), e.created_at
     FROM expenses e JOIN expense_categories c ON c.category_id=e.category_id
     LEFT JOIN suppliers s ON s.supplier_id=e.supplier_id
     LEFT JOIN users cu ON cu.user_id=e.created_by LEFT JOIN users du ON du.user_id=e.decided_by";

fn map_row(r: &rusqlite::Row) -> rusqlite::Result<ExpenseRow> {
    Ok(ExpenseRow {
        expense_id: r.get(0)?,
        number: r.get(1)?,
        business_date: r.get(2)?,
        category_id: r.get(3)?,
        category: r.get(4)?,
        category_ar: r.get(5)?,
        supplier_id: r.get(6)?,
        payee: r.get(7)?,
        description: r.get(8)?,
        net_minor: r.get(9)?,
        vat_minor: r.get(10)?,
        total_minor: r.get(11)?,
        status: r.get(12)?,
        payment_method: r.get(13)?,
        fund_id: r.get(14)?,
        cash_event_id: r.get(15)?,
        reference: r.get(16)?,
        notes: r.get(17)?,
        recurring_id: r.get(18)?,
        created_by_name: r.get(19)?,
        decided_by_name: r.get(20)?,
        decision_note: r.get(21)?,
        paid_at: r.get(22)?,
        void_reason: r.get(23)?,
        attachments: r.get(24)?,
        created_at: r.get(25)?,
    })
}

pub fn get_row(c: &Connection, id: &str) -> AppResult<ExpenseRow> {
    c.query_row(&format!("{ROW_SQL} WHERE e.expense_id=?1"), [id], map_row).optional()?.ok_or_else(|| AppError::not_found("Expense"))
}

/// Petty-cash balance: the sum of the fund's entries.
pub fn fund_balance(c: &Connection, fund_id: &str) -> AppResult<i64> {
    Ok(c.query_row("SELECT COALESCE(SUM(amount_minor),0) FROM petty_cash_entries WHERE fund_id=?1", [fund_id], |r| r.get(0))?)
}

/// Approved and paid expenses (net of VAT) between two business dates,
/// in total and by category: the expense side of operating profit.
/// (category id, name, Arabic name, net amount)
pub type CategoryTotal = (String, String, Option<String>, i64);

pub fn operating_expenses(c: &Connection, from: &str, to: &str, branch: Option<&str>) -> AppResult<(i64, Vec<CategoryTotal>)> {
    let mut st = c.prepare(
        "SELECT e.category_id, c.name, c.name_ar, SUM(e.net_minor) FROM expenses e JOIN expense_categories c ON c.category_id=e.category_id
         WHERE e.status IN ('approved','paid') AND e.business_date>=?1 AND e.business_date<=?2 AND (?3 IS NULL OR e.branch_id=?3)
         GROUP BY e.category_id ORDER BY SUM(e.net_minor) DESC",
    )?;
    let rows = st
        .query_map(params![from, to, branch], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<Vec<CategoryTotal>, _>>()?;
    Ok((rows.iter().map(|r| r.3).sum(), rows))
}

fn amounts(total: i64, vat: i64) -> AppResult<(i64, i64, i64)> {
    if total <= 0 {
        return Err(AppError::validation("Enter the amount paid."));
    }
    if vat < 0 || vat > total {
        return Err(AppError::validation("The VAT must be between zero and the amount paid."));
    }
    Ok((total - vat, vat, total))
}

fn check_date(c: &Connection, d: Option<&str>) -> AppResult<String> {
    let today = time::business_date(time::now(), &time::day(c)?)?;
    match d.filter(|x| !x.is_empty()) {
        None => Ok(today),
        Some(d) => {
            time::validate_date(d)?;
            if d > today.as_str() {
                return Err(AppError::validation("An expense cannot be dated in the future."));
            }
            Ok(d.to_string())
        }
    }
}

fn check_category(c: &Connection, id: &str) -> AppResult<String> {
    let id = validate::id(id, "Category")?;
    let ok: Option<i64> = c.query_row("SELECT active FROM expense_categories WHERE category_id=?1", [&id], |r| r.get(0)).optional()?;
    match ok {
        Some(1) => Ok(id),
        _ => Err(AppError::validation("Choose an expense category.")),
    }
}

fn opt_text(v: &Option<String>, label: &str, max: usize) -> AppResult<Option<String>> {
    match v.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
        Some(x) => Ok(Some(clean(x, label, max, false)?)),
        None => Ok(None),
    }
}

/// Next date on or after `from` for a recurring template.
pub fn next_due(cadence: &str, day: i64, from: &str) -> AppResult<String> {
    use chrono::{Datelike, NaiveDate};
    let f = NaiveDate::parse_from_str(from, "%Y-%m-%d").map_err(|_| AppError::validation("Invalid date."))?;
    match cadence {
        "monthly" => {
            let d = day.clamp(1, 28) as u32;
            let this = NaiveDate::from_ymd_opt(f.year(), f.month(), d).unwrap();
            if this >= f {
                Ok(this.to_string())
            } else {
                let (y, m) = if f.month() == 12 { (f.year() + 1, 1) } else { (f.year(), f.month() + 1) };
                Ok(NaiveDate::from_ymd_opt(y, m, d).unwrap().to_string())
            }
        }
        "weekly" => {
            let want = day.clamp(1, 7) as u32; // 1 = Monday
            let have = f.weekday().number_from_monday();
            let add = (want + 7 - have) % 7;
            Ok((f + chrono::Duration::days(add as i64)).to_string())
        }
        _ => Err(AppError::validation("Repeat monthly or weekly.")),
    }
}

impl AppCore {
    fn expenses_session(&self, token: &str, perm: &str) -> AppResult<Session> {
        let s = self.session(token)?;
        s.require(perm)?;
        Ok(s)
    }

    fn expenses_write(&self, token: &str, perm: &str) -> AppResult<Session> {
        let s = self.expenses_session(token, perm)?;
        self.require_back_office_writable()?;
        Ok(s)
    }

    /// Drafts for every recurring template that has come due (once per date).
    pub(crate) fn expenses_generate_due(&self, s: &Session) -> AppResult<usize> {
        if !s.has("expenses.create") || self.require_back_office_writable().is_err() {
            return Ok(0);
        }
        let actor = self.actor(s, None);
        self.db.write(|tx| {
            let today = time::business_date(time::now(), &time::day(tx)?)?;
            let mut st = tx.prepare(
                "SELECT recurring_id, category_id, supplier_id, payee, description, net_minor, vat_minor, cadence, day, next_date, branch_id
                 FROM expense_recurring WHERE active=1 AND next_date<=?1",
            )?;
            #[allow(clippy::type_complexity)]
            let due: Vec<(String, String, Option<String>, Option<String>, String, i64, i64, String, i64, String, String)> = st
                .query_map([&today], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?))
                })?
                .collect::<Result<_, _>>()?;
            drop(st);
            let mut made = 0;
            for (rid, cat, sup, payee, desc, net, vat, cadence, day, mut date, branch) in due {
                // Catch up one draft per missed date, never more than 12 at once.
                for _ in 0..12 {
                    if date > today {
                        break;
                    }
                    let exists: bool =
                        tx.query_row("SELECT 1 FROM expenses WHERE recurring_id=?1 AND business_date=?2", params![rid, date], |_| Ok(true)).optional()?.unwrap_or(false);
                    if !exists {
                        let id = new_id();
                        let number = format!("EXP-{:06}", next_seq(tx, "expense")?);
                        let now = time::now_str();
                        tx.execute(
                            "INSERT INTO expenses(expense_id, number, branch_id, business_date, category_id, supplier_id, payee, description,
                                net_minor, vat_minor, total_minor, status, recurring_id, created_by, created_at, updated_at)
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'draft',?12,?13,?14,?14)",
                            params![id, number, branch, date, cat, sup, payee, desc, net, vat, net + vat, rid, s.user_id, now],
                        )?;
                        audit::record(tx, &actor, "expense.drafted_from_recurring", "expense", Some(&id), None, Some(&json!({ "recurring_id": rid, "date": date })))?;
                        made += 1;
                    }
                    let d = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(|_| AppError::validation("Invalid date."))?;
                    date = next_due(&cadence, day, &(d + chrono::Duration::days(1)).to_string())?;
                }
                tx.execute("UPDATE expense_recurring SET next_date=?2, updated_at=?3 WHERE recurring_id=?1", params![rid, date, time::now_str()])?;
            }
            Ok(made)
        })
    }

    pub fn expenses_list(&self, token: &str, from: Option<String>, to: Option<String>, status: Option<String>) -> AppResult<Value> {
        let s = self.expenses_session(token, "expenses.view")?;
        self.expenses_generate_due(&s)?;
        self.db.read(|c| {
            let today = time::business_date(time::now(), &time::day(c)?)?;
            let from = from.filter(|x| !x.is_empty()).unwrap_or_else(|| format!("{}-01", &today[..7]));
            let to = to.filter(|x| !x.is_empty()).unwrap_or(today);
            time::validate_date(&from)?;
            time::validate_date(&to)?;
            let mut st = c.prepare(&format!(
                "{ROW_SQL} WHERE ((e.business_date>=?1 AND e.business_date<=?2) OR e.status IN ('draft','submitted') OR (e.status='approved' AND e.payment_method IS NULL))
                 AND (?3 IS NULL OR e.status=?3) ORDER BY e.business_date DESC, e.created_at DESC LIMIT 2000"
            ))?;
            let rows = st.query_map(params![from, to, status.as_deref().filter(|x| !x.is_empty())], map_row)?.collect::<Result<Vec<_>, _>>()?;
            let (spent, by_category) = operating_expenses(c, &from, &to, None)?;
            let waiting = rows.iter().filter(|r| r.status == "submitted").count();
            let to_pay = rows.iter().filter(|r| r.status == "approved").count();
            Ok(json!({
                "from": from, "to": to, "rows": rows, "spent_minor": spent,
                "by_category": by_category.into_iter().map(|(id, n, ar, v)| json!({ "category_id": id, "name": n, "name_ar": ar, "net_minor": v })).collect::<Vec<_>>(),
                "waiting_approval": waiting, "approved_unpaid": to_pay,
            }))
        })
    }

    pub fn expense_get(&self, token: &str, id: &str) -> AppResult<Value> {
        self.expenses_session(token, "expenses.view")?;
        let id = validate::id(id, "Expense")?;
        self.db.read(|c| {
            let row = get_row(c, &id)?;
            let mut st = c.prepare("SELECT attachment_id, file_name, mime, added_at FROM expense_attachments WHERE expense_id=?1 ORDER BY added_at")?;
            let files = st
                .query_map([&id], |r| Ok(json!({ "attachment_id": r.get::<_, String>(0)?, "file_name": r.get::<_, String>(1)?, "mime": r.get::<_, String>(2)?, "added_at": r.get::<_, String>(3)? })))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({ "expense": row, "attachments": files }))
        })
    }

    /// Create or edit a draft.
    pub fn expense_save(&self, token: &str, id: Option<String>, input: ExpenseInput) -> AppResult<ExpenseRow> {
        let s = self.expenses_write(token, "expenses.create")?;
        let actor = self.actor(&s, None);
        let description = clean(&input.description, "Description", 200, true)?;
        let payee = opt_text(&input.payee, "Paid to", 120)?;
        let reference = opt_text(&input.reference, "Reference", 60)?;
        let notes = opt_text(&input.notes, "Notes", 500)?;
        let (net, vat, total) = amounts(input.total_minor, input.vat_minor)?;
        self.db.write(|tx| {
            let cat = check_category(tx, &input.category_id)?;
            let date = check_date(tx, input.business_date.as_deref())?;
            let supplier = match input.supplier_id.as_deref().filter(|x| !x.is_empty()) {
                Some(sid) => {
                    let sid = validate::id(sid, "Supplier")?;
                    tx.query_row("SELECT 1 FROM suppliers WHERE supplier_id=?1", [&sid], |_| Ok(())).optional()?.ok_or_else(|| AppError::not_found("Supplier"))?;
                    Some(sid)
                }
                None => None,
            };
            let now = time::now_str();
            let eid = match id.as_deref() {
                None => {
                    let eid = new_id();
                    let number = format!("EXP-{:06}", next_seq(tx, "expense")?);
                    tx.execute(
                        "INSERT INTO expenses(expense_id, number, branch_id, business_date, category_id, supplier_id, payee, description,
                            net_minor, vat_minor, total_minor, status, reference, notes, created_by, created_at, updated_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'draft',?12,?13,?14,?15,?15)",
                        params![eid, number, s.branch_id, date, cat, supplier, payee, description, net, vat, total, reference, notes, s.user_id, now],
                    )?;
                    audit::record(tx, &actor, "expense.drafted", "expense", Some(&eid), None, Some(&json!({ "number": number, "total_minor": total })))?;
                    eid
                }
                Some(eid) => {
                    let eid = validate::id(eid, "Expense")?;
                    let before = get_row(tx, &eid)?;
                    if before.status != "draft" {
                        return Err(AppError::conflict("Only drafts can be changed. Void the expense and enter it again."));
                    }
                    tx.execute(
                        "UPDATE expenses SET business_date=?2, category_id=?3, supplier_id=?4, payee=?5, description=?6, net_minor=?7, vat_minor=?8,
                            total_minor=?9, reference=?10, notes=?11, revision=revision+1, updated_at=?12 WHERE expense_id=?1",
                        params![eid, date, cat, supplier, payee, description, net, vat, total, reference, notes, now],
                    )?;
                    audit::record(tx, &actor, "expense.draft_changed", "expense", Some(&eid), Some(&json!({ "total_minor": before.total_minor })), Some(&json!({ "total_minor": total })))?;
                    eid
                }
            };
            get_row(tx, &eid)
        })
    }

    pub fn expense_delete_draft(&self, token: &str, id: &str) -> AppResult<()> {
        let s = self.expenses_write(token, "expenses.create")?;
        let id = validate::id(id, "Expense")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let row = get_row(tx, &id)?;
            if row.status != "draft" {
                return Err(AppError::conflict("Only drafts can be deleted."));
            }
            tx.execute("DELETE FROM expense_attachments WHERE expense_id=?1", [&id])?;
            tx.execute("DELETE FROM expenses WHERE expense_id=?1", [&id])?;
            audit::record(tx, &actor, "expense.draft_deleted", "expense", Some(&id), Some(&json!({ "number": row.number })), None)?;
            Ok(())
        })
    }

    /// Submit a draft. Approved on entry when the submitter may approve, or
    /// the amount is within the store's auto-approve limit.
    pub fn expense_submit(&self, token: &str, id: &str) -> AppResult<ExpenseRow> {
        let s = self.expenses_write(token, "expenses.create")?;
        let id = validate::id(id, "Expense")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let row = get_row(tx, &id)?;
            if row.status != "draft" {
                // Submitting twice is harmless: report the current state.
                return Ok(row);
            }
            let cfg: ExpenseSettings = settings::get(tx, settings::KEY_EXPENSES)?;
            let auto = s.has("expenses.approve") || (cfg.auto_approve_up_to_minor > 0 && row.total_minor <= cfg.auto_approve_up_to_minor);
            let now = time::now_str();
            if auto {
                tx.execute(
                    "UPDATE expenses SET status='approved', submitted_by=?2, submitted_at=?3, decided_by=?2, decided_at=?3,
                        decision_note='Approved on entry', updated_at=?3 WHERE expense_id=?1",
                    params![id, s.user_id, now],
                )?;
            } else {
                tx.execute(
                    "UPDATE expenses SET status='submitted', submitted_by=?2, submitted_at=?3, updated_at=?3 WHERE expense_id=?1",
                    params![id, s.user_id, now],
                )?;
            }
            audit::record(
                tx,
                &actor,
                if auto { "expense.approved_on_entry" } else { "expense.submitted" },
                "expense",
                Some(&id),
                None,
                Some(&json!({ "total_minor": row.total_minor })),
            )?;
            get_row(tx, &id)
        })
    }

    pub fn expense_decide(&self, token: &str, id: &str, approve: bool, note: Option<String>) -> AppResult<ExpenseRow> {
        let s = self.expenses_write(token, "expenses.approve")?;
        let id = validate::id(id, "Expense")?;
        let note = opt_text(&note, "Note", 300)?;
        if !approve && note.is_none() {
            return Err(AppError::validation("Say why the expense is rejected."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let row = get_row(tx, &id)?;
            let target = if approve { "approved" } else { "rejected" };
            if row.status == target {
                return Ok(row);
            }
            if row.status != "submitted" {
                return Err(AppError::conflict("Only an expense waiting for approval can be approved or rejected."));
            }
            tx.execute(
                "UPDATE expenses SET status=?2, decided_by=?3, decided_at=?4, decision_note=?5, updated_at=?4 WHERE expense_id=?1",
                params![id, target, s.user_id, time::now_str(), note],
            )?;
            audit::record(
                tx,
                &actor,
                &format!("expense.{target}"),
                "expense",
                Some(&id),
                None,
                Some(&json!({ "note": note, "total_minor": row.total_minor })),
            )?;
            get_row(tx, &id)
        })
    }

    /// Record how an approved expense was paid. Petty cash writes a fund
    /// entry; a till paid-out links the cash event already recorded at the
    /// till (same amount, used once). Idempotent on the operation id.
    pub fn expense_pay(&self, token: &str, id: &str, input: PayInput) -> AppResult<ExpenseRow> {
        let s = self.expenses_write(token, "expenses.pay")?;
        let id = validate::id(id, "Expense")?;
        idempotency::validate_operation_id(&input.operation_id)?;
        if !PAY_METHODS.contains(&input.method.as_str()) {
            return Err(AppError::validation("Choose how the expense was paid."));
        }
        let reference = opt_text(&input.reference, "Reference", 60)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &input.operation_id, "expense.pay", &(&id, &input))? {
                Check::Replay { .. } => return get_row(tx, &id),
                Check::New { payload_hash } => payload_hash,
            };
            let row = get_row(tx, &id)?;
            if row.status != "approved" {
                return Err(AppError::conflict(match row.status.as_str() {
                    "paid" => "This expense is already paid.",
                    "submitted" => "This expense is waiting for approval.",
                    _ => "Only an approved expense can be paid.",
                }));
            }
            let now = time::now_str();
            let mut fund = None;
            let mut cash_event = None;
            match input.method.as_str() {
                "petty_cash" => {
                    let f = validate::id(input.fund_id.as_deref().unwrap_or(""), "Petty cash fund")?;
                    let active: Option<i64> = tx.query_row("SELECT active FROM petty_cash_funds WHERE fund_id=?1", [&f], |r| r.get(0)).optional()?;
                    if active != Some(1) {
                        return Err(AppError::validation("Choose an open petty cash fund."));
                    }
                    let bal = fund_balance(tx, &f)?;
                    if bal < row.total_minor {
                        return Err(AppError::conflict("The petty cash fund does not hold enough. Top it up first."));
                    }
                    tx.execute(
                        "INSERT INTO petty_cash_entries(entry_id, fund_id, kind, amount_minor, expense_id, note, operation_id, user_id, business_date, created_at)
                         VALUES (?1,?2,'expense',?3,?4,?5,?6,?7,?8,?9)",
                        params![new_id(), f, -row.total_minor, id, row.number, input.operation_id, s.user_id, row.business_date, now],
                    )?;
                    fund = Some(f);
                }
                "till_paid_out" => {
                    let ce = validate::id(input.cash_event_id.as_deref().unwrap_or(""), "Till paid-out")?;
                    let (kind, amount): (String, i64) = tx
                        .query_row("SELECT type, amount_minor FROM cash_events WHERE cash_event_id=?1", [&ce], |r| Ok((r.get(0)?, r.get(1)?)))
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Till paid-out"))?;
                    if kind != "paid_out" || amount != row.total_minor {
                        return Err(AppError::validation("Choose the till paid-out of exactly this amount."));
                    }
                    let used: bool = tx.query_row("SELECT 1 FROM expenses WHERE cash_event_id=?1", [&ce], |_| Ok(true)).optional()?.unwrap_or(false);
                    if used {
                        return Err(AppError::conflict("That till paid-out is already linked to another expense."));
                    }
                    cash_event = Some(ce);
                }
                _ => {}
            }
            tx.execute(
                "UPDATE expenses SET status='paid', payment_method=?2, fund_id=?3, cash_event_id=?4, reference=COALESCE(?5, reference),
                    paid_by=?6, paid_at=?7, updated_at=?7 WHERE expense_id=?1",
                params![id, input.method, fund, cash_event, reference, s.user_id, now],
            )?;
            audit::record(tx, &actor, "expense.paid", "expense", Some(&id), None, Some(&json!({ "method": input.method, "total_minor": row.total_minor, "fund_id": fund })))?;
            let out = get_row(tx, &id)?;
            idempotency::complete(tx, &input.operation_id, "expense.pay", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &serde_json::to_value(&out)?)?;
            Ok(out)
        })
    }

    /// Void a submitted, approved or paid expense. A petty-cash payment is
    /// returned to the fund by a compensating entry; nothing is deleted.
    pub fn expense_void(&self, token: &str, id: &str, reason: &str, operation_id: &str) -> AppResult<ExpenseRow> {
        let s = self.expenses_write(token, "expenses.approve")?;
        let id = validate::id(id, "Expense")?;
        let reason = clean(reason, "Reason", 200, true)?;
        idempotency::validate_operation_id(operation_id)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, operation_id, "expense.void", &(&id, &reason))? {
                Check::Replay { .. } => return get_row(tx, &id),
                Check::New { payload_hash } => payload_hash,
            };
            let row = get_row(tx, &id)?;
            if !["submitted", "approved", "paid"].contains(&row.status.as_str()) {
                return Err(AppError::conflict(if row.status == "void" { "This expense is already void." } else { "Delete the draft instead." }));
            }
            let now = time::now_str();
            if row.status == "paid" && row.payment_method.as_deref() == Some("petty_cash") {
                if let Some(f) = &row.fund_id {
                    tx.execute(
                        "INSERT INTO petty_cash_entries(entry_id, fund_id, kind, amount_minor, expense_id, note, operation_id, user_id, business_date, created_at)
                         VALUES (?1,?2,'void',?3,?4,?5,?6,?7,?8,?9)",
                        params![new_id(), f, row.total_minor, id, format!("{} void: {reason}", row.number), operation_id, s.user_id,
                            time::business_date(time::now(), &time::day(tx)?)?, now],
                    )?;
                }
            }
            tx.execute(
                "UPDATE expenses SET status='void', void_by=?2, void_at=?3, void_reason=?4, cash_event_id=NULL, updated_at=?3 WHERE expense_id=?1",
                params![id, s.user_id, now, reason],
            )?;
            audit::record(tx, &actor, "expense.voided", "expense", Some(&id), Some(&json!({ "status": row.status })), Some(&json!({ "reason": reason, "total_minor": row.total_minor })))?;
            let out = get_row(tx, &id)?;
            idempotency::complete(tx, operation_id, "expense.void", Some(&s.user_id), Some(&s.device_id), &hash, Some(&id), &serde_json::to_value(&out)?)?;
            Ok(out)
        })
    }

    /// Keep a photo or PDF of the bill with the expense.
    pub fn expense_attach(&self, token: &str, id: &str, file_name: &str, data_b64: &str) -> AppResult<Value> {
        let s = self.expenses_write(token, "expenses.create")?;
        let id = validate::id(id, "Expense")?;
        let name = clean(file_name, "File name", 120, true)?;
        let bytes = crate::ids::b64_decode(data_b64).ok_or_else(|| AppError::validation("The file could not be read."))?;
        let aid = new_id();
        let (path, sha, mime) = crate::docintel::service::store_original(&self.data_dir.join("expenses"), &name, &bytes, &aid)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            get_row(tx, &id)?;
            tx.execute(
                "INSERT INTO expense_attachments(attachment_id, expense_id, file_name, path, mime, sha256, added_by, added_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![aid, id, name, path.to_string_lossy(), mime, sha, s.user_id, time::now_str()],
            )?;
            audit::record(tx, &actor, "expense.attachment_added", "expense", Some(&id), None, Some(&json!({ "file": name, "sha256": sha })))?;
            Ok(json!({ "attachment_id": aid }))
        })
    }

    pub fn expense_attachment(&self, token: &str, attachment_id: &str) -> AppResult<Value> {
        self.expenses_session(token, "expenses.view")?;
        let aid = validate::id(attachment_id, "Attachment")?;
        let (name, path, mime): (String, String, String) = self.db.read(|c| {
            c.query_row("SELECT file_name, path, mime FROM expense_attachments WHERE attachment_id=?1", [&aid], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?
            .ok_or_else(|| AppError::not_found("Attachment"))
        })?;
        let bytes = std::fs::read(&path).map_err(|_| AppError::not_found("Attachment file"))?;
        Ok(json!({ "file_name": name, "mime": mime, "base64": crate::ids::b64(&bytes) }))
    }

    pub fn expense_categories(&self, token: &str) -> AppResult<Value> {
        self.expenses_session(token, "expenses.view")?;
        self.db.read(|c| {
            let mut st = c.prepare("SELECT category_id, name, name_ar, active FROM expense_categories ORDER BY sort, name")?;
            let rows = st
                .query_map([], |r| Ok(json!({ "category_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "name_ar": r.get::<_, Option<String>>(2)?, "active": r.get::<_, i64>(3)? == 1 })))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!(rows))
        })
    }

    pub fn expense_category_save(
        &self,
        token: &str,
        id: Option<String>,
        name: &str,
        name_ar: Option<String>,
        active: bool,
    ) -> AppResult<Value> {
        let s = self.expenses_write(token, "expenses.approve")?;
        let name = clean(name, "Category name", 60, true)?;
        let name_ar = opt_text(&name_ar, "Arabic name", 60)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let cid = match id.as_deref().filter(|x| !x.is_empty()) {
                Some(cid) => {
                    let cid = validate::id(cid, "Category")?;
                    let n = tx.execute(
                        "UPDATE expense_categories SET name=?2, name_ar=?3, active=?4 WHERE category_id=?1",
                        params![cid, name, name_ar, active as i64],
                    )?;
                    if n == 0 {
                        return Err(AppError::not_found("Category"));
                    }
                    cid
                }
                None => {
                    let cid = new_id();
                    tx.execute(
                        "INSERT INTO expense_categories(category_id, name, name_ar, active, sort, created_at) VALUES (?1,?2,?3,?4,500,?5)",
                        params![cid, name, name_ar, active as i64, time::now_str()],
                    )?;
                    cid
                }
            };
            audit::record(
                tx,
                &actor,
                "expense.category_saved",
                "expense_category",
                Some(&cid),
                None,
                Some(&json!({ "name": name, "active": active })),
            )?;
            Ok(json!({ "category_id": cid }))
        })
    }

    pub fn expense_recurring_list(&self, token: &str) -> AppResult<Value> {
        self.expenses_session(token, "expenses.view")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT r.recurring_id, r.name, r.category_id, c.name, c.name_ar, r.net_minor + r.vat_minor, r.vat_minor, r.cadence, r.day, r.next_date, r.active,
                        r.payee, r.description, r.supplier_id
                 FROM expense_recurring r JOIN expense_categories c ON c.category_id=r.category_id ORDER BY r.active DESC, r.next_date",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(json!({ "recurring_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "category_id": r.get::<_, String>(2)?,
                        "category": r.get::<_, String>(3)?, "category_ar": r.get::<_, Option<String>>(4)?, "total_minor": r.get::<_, i64>(5)?,
                        "vat_minor": r.get::<_, i64>(6)?, "cadence": r.get::<_, String>(7)?, "day": r.get::<_, i64>(8)?, "next_date": r.get::<_, String>(9)?,
                        "active": r.get::<_, i64>(10)? == 1, "payee": r.get::<_, Option<String>>(11)?, "description": r.get::<_, String>(12)?,
                        "supplier_id": r.get::<_, Option<String>>(13)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!(rows))
        })
    }

    pub fn expense_recurring_save(&self, token: &str, id: Option<String>, input: RecurringInput) -> AppResult<Value> {
        let s = self.expenses_write(token, "expenses.approve")?;
        let name = clean(&input.name, "Name", 80, true)?;
        let description = clean(&input.description, "Description", 200, true)?;
        let payee = opt_text(&input.payee, "Paid to", 120)?;
        let (net, vat, _) = amounts(input.total_minor, input.vat_minor)?;
        match input.cadence.as_str() {
            "monthly" if (1..=28).contains(&input.day) => {}
            "weekly" if (1..=7).contains(&input.day) => {}
            _ => return Err(AppError::validation("Choose a day of the month (1–28) or a weekday.")),
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let cat = check_category(tx, &input.category_id)?;
            let today = time::business_date(time::now(), &time::day(tx)?)?;
            let next = next_due(&input.cadence, input.day, &today)?;
            let now = time::now_str();
            let rid = match id.as_deref().filter(|x| !x.is_empty()) {
                Some(rid) => {
                    let rid = validate::id(rid, "Recurring expense")?;
                    let n = tx.execute(
                        "UPDATE expense_recurring SET name=?2, category_id=?3, supplier_id=?4, payee=?5, description=?6, net_minor=?7, vat_minor=?8,
                            cadence=?9, day=?10, next_date=?11, active=?12, updated_at=?13 WHERE recurring_id=?1",
                        params![rid, name, cat, input.supplier_id, payee, description, net, vat, input.cadence, input.day, next, input.active as i64, now],
                    )?;
                    if n == 0 {
                        return Err(AppError::not_found("Recurring expense"));
                    }
                    rid
                }
                None => {
                    let rid = new_id();
                    tx.execute(
                        "INSERT INTO expense_recurring(recurring_id, name, category_id, supplier_id, payee, description, net_minor, vat_minor, cadence, day,
                            next_date, branch_id, active, created_by, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15)",
                        params![rid, name, cat, input.supplier_id, payee, description, net, vat, input.cadence, input.day, next, s.branch_id, input.active as i64, s.user_id, now],
                    )?;
                    rid
                }
            };
            audit::record(tx, &actor, "expense.recurring_saved", "expense_recurring", Some(&rid), None, Some(&json!({ "name": name, "next_date": next, "active": input.active })))?;
            Ok(json!({ "recurring_id": rid, "next_date": next }))
        })
    }

    // ------------------------------------------------------------ petty cash

    pub fn petty_funds(&self, token: &str) -> AppResult<Vec<FundRow>> {
        self.expenses_session(token, "expenses.view")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT f.fund_id, f.name, f.custodian_user_id, u.display_name, f.active,
                        (SELECT COALESCE(SUM(amount_minor),0) FROM petty_cash_entries e WHERE e.fund_id=f.fund_id),
                        (SELECT MAX(created_at) FROM petty_cash_entries e WHERE e.fund_id=f.fund_id AND e.kind='count')
                 FROM petty_cash_funds f LEFT JOIN users u ON u.user_id=f.custodian_user_id ORDER BY f.active DESC, f.name",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(FundRow {
                        fund_id: r.get(0)?,
                        name: r.get(1)?,
                        custodian_user_id: r.get(2)?,
                        custodian_name: r.get(3)?,
                        active: r.get::<_, i64>(4)? == 1,
                        balance_minor: r.get(5)?,
                        last_count_at: r.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn petty_fund_save(
        &self,
        token: &str,
        id: Option<String>,
        name: &str,
        custodian_user_id: Option<String>,
        active: bool,
    ) -> AppResult<Value> {
        let s = self.expenses_write(token, "petty_cash.manage")?;
        let name = clean(name, "Fund name", 60, true)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            if let Some(u) = custodian_user_id.as_deref().filter(|x| !x.is_empty()) {
                tx.query_row("SELECT 1 FROM users WHERE user_id=?1", [u], |_| Ok(())).optional()?.ok_or_else(|| AppError::not_found("User"))?;
            }
            let custodian = custodian_user_id.filter(|x| !x.is_empty());
            let fid = match id.as_deref().filter(|x| !x.is_empty()) {
                Some(fid) => {
                    let fid = validate::id(fid, "Petty cash fund")?;
                    if !active && fund_balance(tx, &fid)? != 0 {
                        return Err(AppError::conflict("Hand back or count the cash before closing the fund (its balance must be zero)."));
                    }
                    tx.execute("UPDATE petty_cash_funds SET name=?2, custodian_user_id=?3, active=?4 WHERE fund_id=?1", params![fid, name, custodian, active as i64])?;
                    fid
                }
                None => {
                    let fid = new_id();
                    tx.execute(
                        "INSERT INTO petty_cash_funds(fund_id, branch_id, name, custodian_user_id, active, created_by, created_at) VALUES (?1,?2,?3,?4,1,?5,?6)",
                        params![fid, s.branch_id, name, custodian, s.user_id, time::now_str()],
                    )?;
                    fid
                }
            };
            audit::record(tx, &actor, "petty_cash.fund_saved", "petty_cash_fund", Some(&fid), None, Some(&json!({ "name": name, "active": active })))?;
            Ok(json!({ "fund_id": fid }))
        })
    }

    /// Money into or out of the fund: open, top_up (positive), reimburse
    /// (handed back, negative), adjust (signed, with a reason).
    pub fn petty_entry(
        &self,
        token: &str,
        fund_id: &str,
        kind: &str,
        amount_minor: i64,
        note: Option<String>,
        operation_id: &str,
    ) -> AppResult<Value> {
        let s = self.expenses_write(token, "petty_cash.manage")?;
        let fid = validate::id(fund_id, "Petty cash fund")?;
        idempotency::validate_operation_id(operation_id)?;
        let note = opt_text(&note, "Note", 200)?;
        let signed = match kind {
            "open" | "top_up" if amount_minor > 0 => amount_minor,
            "reimburse" if amount_minor > 0 => -amount_minor,
            "adjust" if amount_minor != 0 => {
                if note.is_none() {
                    return Err(AppError::validation("Say why the fund is adjusted."));
                }
                amount_minor
            }
            "open" | "top_up" | "reimburse" | "adjust" => return Err(AppError::validation("Enter an amount.")),
            _ => return Err(AppError::validation("Unknown petty cash entry.")),
        };
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, operation_id, "petty.entry", &(&fid, kind, amount_minor, &note))? {
                Check::Replay { result } => return Ok(result),
                Check::New { payload_hash } => payload_hash,
            };
            let active: Option<i64> = tx.query_row("SELECT active FROM petty_cash_funds WHERE fund_id=?1", [&fid], |r| r.get(0)).optional()?;
            if active != Some(1) {
                return Err(AppError::validation("Choose an open petty cash fund."));
            }
            if fund_balance(tx, &fid)? + signed < 0 {
                return Err(AppError::conflict("The fund does not hold that much."));
            }
            let eid = new_id();
            tx.execute(
                "INSERT INTO petty_cash_entries(entry_id, fund_id, kind, amount_minor, note, operation_id, user_id, business_date, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![eid, fid, kind, signed, note, operation_id, s.user_id, time::business_date(time::now(), &time::day(tx)?)?, time::now_str()],
            )?;
            let bal = fund_balance(tx, &fid)?;
            audit::record(tx, &actor, &format!("petty_cash.{kind}"), "petty_cash_fund", Some(&fid), None, Some(&json!({ "amount_minor": signed, "balance_minor": bal, "note": note })))?;
            let out = json!({ "entry_id": eid, "balance_minor": bal });
            idempotency::complete(tx, operation_id, "petty.entry", Some(&s.user_id), Some(&s.device_id), &hash, Some(&eid), &out)?;
            Ok(out)
        })
    }

    /// Count the fund. The difference from the expected balance is recorded
    /// as part of the count, so the balance then equals what was counted.
    pub fn petty_count(
        &self,
        token: &str,
        fund_id: &str,
        counted_minor: i64,
        note: Option<String>,
        operation_id: &str,
    ) -> AppResult<Value> {
        let s = self.expenses_write(token, "petty_cash.manage")?;
        let fid = validate::id(fund_id, "Petty cash fund")?;
        idempotency::validate_operation_id(operation_id)?;
        if counted_minor < 0 {
            return Err(AppError::validation("The counted amount cannot be negative."));
        }
        let note = opt_text(&note, "Note", 200)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, operation_id, "petty.count", &(&fid, counted_minor, &note))? {
                Check::Replay { result } => return Ok(result),
                Check::New { payload_hash } => payload_hash,
            };
            let expected = fund_balance(tx, &fid)?;
            let diff = counted_minor - expected;
            let eid = new_id();
            tx.execute(
                "INSERT INTO petty_cash_entries(entry_id, fund_id, kind, amount_minor, counted_minor, note, operation_id, user_id, business_date, created_at)
                 VALUES (?1,?2,'count',?3,?4,?5,?6,?7,?8,?9)",
                params![eid, fid, diff, counted_minor, note, operation_id, s.user_id, time::business_date(time::now(), &time::day(tx)?)?, time::now_str()],
            )?;
            audit::record(tx, &actor, "petty_cash.counted", "petty_cash_fund", Some(&fid), None, Some(&json!({ "expected_minor": expected, "counted_minor": counted_minor, "difference_minor": diff, "note": note })))?;
            let out = json!({ "entry_id": eid, "expected_minor": expected, "counted_minor": counted_minor, "difference_minor": diff });
            idempotency::complete(tx, operation_id, "petty.count", Some(&s.user_id), Some(&s.device_id), &hash, Some(&eid), &out)?;
            Ok(out)
        })
    }

    pub fn petty_entries(&self, token: &str, fund_id: &str) -> AppResult<Value> {
        self.expenses_session(token, "expenses.view")?;
        let fid = validate::id(fund_id, "Petty cash fund")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT e.entry_id, e.kind, e.amount_minor, e.counted_minor, e.note, ex.number, u.display_name, e.business_date, e.created_at
                 FROM petty_cash_entries e LEFT JOIN expenses ex ON ex.expense_id=e.expense_id LEFT JOIN users u ON u.user_id=e.user_id
                 WHERE e.fund_id=?1 ORDER BY e.created_at, e.rowid",
            )?;
            let mut running = 0i64;
            let rows = st
                .query_map([&fid], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<i64>>(3)?, r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?, r.get::<_, Option<String>>(6)?, r.get::<_, String>(7)?, r.get::<_, String>(8)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|(id, kind, amt, counted, note, number, user, date, at)| {
                    running += amt;
                    json!({ "entry_id": id, "kind": kind, "amount_minor": amt, "counted_minor": counted, "note": note, "expense_number": number,
                        "user": user, "business_date": date, "created_at": at, "balance_minor": running })
                })
                .collect::<Vec<_>>();
            Ok(json!(rows))
        })
    }

    /// Till paid-outs not yet linked to an expense (to pick from when paying).
    pub fn expense_unlinked_paid_outs(&self, token: &str) -> AppResult<Value> {
        self.expenses_session(token, "expenses.pay")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT ce.cash_event_id, ce.amount_minor, ce.reason, ce.created_at, u.display_name FROM cash_events ce
                 LEFT JOIN users u ON u.user_id=ce.actor_user_id
                 WHERE ce.type='paid_out' AND NOT EXISTS (SELECT 1 FROM expenses e WHERE e.cash_event_id=ce.cash_event_id)
                 ORDER BY ce.created_at DESC LIMIT 200",
            )?;
            let rows = st
                .query_map([], |r| Ok(json!({ "cash_event_id": r.get::<_, String>(0)?, "amount_minor": r.get::<_, i64>(1)?, "reason": r.get::<_, String>(2)?, "created_at": r.get::<_, String>(3)?, "user": r.get::<_, Option<String>>(4)? })))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!(rows))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recurring_dates() {
        assert_eq!(next_due("monthly", 5, "2026-10-01").unwrap(), "2026-10-05");
        assert_eq!(next_due("monthly", 5, "2026-10-06").unwrap(), "2026-11-05");
        assert_eq!(next_due("monthly", 28, "2026-12-29").unwrap(), "2027-01-28");
        // 2026-10-05 is a Monday.
        assert_eq!(next_due("weekly", 1, "2026-10-05").unwrap(), "2026-10-05");
        assert_eq!(next_due("weekly", 3, "2026-10-05").unwrap(), "2026-10-07");
        assert_eq!(next_due("weekly", 1, "2026-10-06").unwrap(), "2026-10-12");
    }
    #[test]
    fn amounts_are_checked() {
        assert_eq!(amounts(10_500, 500).unwrap(), (10_000, 500, 10_500));
        assert!(amounts(0, 0).is_err());
        assert!(amounts(100, 200).is_err());
    }
}
