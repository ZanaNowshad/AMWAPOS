//! Sale finalization (exactly-once) and sales history.

use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::auth::Session;
use crate::catalog::Page;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::idempotency::{self, Check};
use crate::ids::{new_id, next_seq};
use crate::inventory::{self, Movement};
use crate::money;
use crate::pos::load_lines;
use crate::pricing::{self, TenderInput};
use crate::printing::PrintOutcome;
use crate::service::AppCore;
use crate::settings;
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FinalizeRequest {
    pub cart_id: String,
    pub operation_id: String,
    pub tenders: Vec<TenderInput>,
    #[serde(default, skip_serializing)]
    pub approval_token: Option<String>,
    /// Expected total shown to the cashier. If present and different from the
    /// authoritative total, finalization is refused (nothing is charged
    /// against a stale screen).
    #[serde(default)]
    pub expected_total_minor: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentView {
    pub method: String,
    pub amount_minor: i64,
    pub tendered_minor: i64,
    pub change_minor: i64,
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaleResult {
    pub sale_id: String,
    pub receipt_number: String,
    pub total_minor: i64,
    pub paid_minor: i64,
    pub change_minor: i64,
    pub payments: Vec<PaymentView>,
    pub completed_at: String,
    /// True when this response is a replay of an already-committed sale.
    #[serde(default)]
    pub replayed: bool,
    #[serde(default)]
    pub print: Option<PrintOutcome>,
    /// Items sold below zero recorded stock (allowed by the store setting).
    #[serde(default)]
    pub stock_warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SaleRow {
    pub sale_id: String,
    pub receipt_number: String,
    pub completed_at: String,
    pub cashier_name: String,
    pub device_name: Option<String>,
    pub customer_name: Option<String>,
    pub item_count_milli: i64,
    pub total_minor: i64,
    pub methods: String,
    pub refunded_minor: i64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SaleItemView {
    pub sale_item_id: String,
    pub line_no: i64,
    pub product_id: Option<String>,
    pub name: String,
    pub sku: Option<String>,
    pub barcode: Option<String>,
    pub unit: String,
    pub qty_milli: i64,
    pub original_unit_price_minor: i64,
    pub unit_price_minor: i64,
    pub gross_minor: i64,
    pub discount_minor: i64,
    pub tax_rate_bp: i64,
    pub tax_inclusive: bool,
    pub tax_minor: i64,
    pub line_total_minor: i64,
    pub cost_minor: Option<i64>,
    pub refunded_qty_milli: i64,
    pub is_custom: bool,
    /// Arabic product name at the time of sale (if the product had one).
    pub name_ar: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SaleDetail {
    pub sale_id: String,
    pub receipt_number: String,
    pub status: String,
    pub completed_at: String,
    pub business_date: String,
    pub cashier_user_id: String,
    pub cashier_name: String,
    pub device_id: String,
    pub device_name: Option<String>,
    pub shift_id: String,
    pub customer_id: Option<String>,
    pub customer_name: Option<String>,
    pub customer_phone: Option<String>,
    pub subtotal_minor: i64,
    pub discount_minor: i64,
    pub tax_minor: i64,
    pub total_minor: i64,
    pub paid_minor: i64,
    pub change_minor: i64,
    pub cost_total_minor: Option<i64>,
    pub items: Vec<SaleItemView>,
    pub payments: Vec<PaymentView>,
    pub refunds: Vec<RefundRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefundRef {
    pub refund_id: String,
    pub refund_receipt_number: String,
    pub total_minor: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SalesQuery {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub receipt: Option<String>,
    #[serde(default)]
    pub cashier_id: Option<String>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub customer_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub shift_id: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

pub(crate) fn open_shift_for(c: &Connection, s: &Session) -> AppResult<Option<String>> {
    Ok(c.query_row(
        "SELECT shift_id FROM shifts WHERE device_id=?1 AND user_id=?2 AND status='open'",
        params![s.device_id, s.user_id],
        |r| r.get(0),
    )
    .optional()?)
}

pub(crate) fn shift_required() -> AppError {
    AppError::new(ErrorCode::ShiftRequired, "Open a shift before selling or handling cash.")
}

fn load_payments(c: &Connection, sale_id: &str) -> AppResult<Vec<PaymentView>> {
    let mut st = c.prepare_cached(
        "SELECT method, amount_minor, tendered_minor, change_minor, reference FROM payments WHERE sale_id=?1 ORDER BY created_at, payment_id",
    )?;
    let rows = st
        .query_map([sale_id], |r| {
            Ok(PaymentView {
                method: r.get(0)?,
                amount_minor: r.get(1)?,
                tendered_minor: r.get(2)?,
                change_minor: r.get(3)?,
                reference: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn load_sale_detail(c: &Connection, sale_id: &str, show_cost: bool) -> AppResult<SaleDetail> {
    let mut d = c
        .query_row(
            "SELECT s.sale_id, s.receipt_number, s.status, s.completed_at, s.business_date, s.cashier_user_id, u.display_name,
                    s.device_id, d.name, s.shift_id, s.customer_id, cu.name, cu.phone, s.subtotal_minor, s.discount_minor,
                    s.tax_minor, s.total_minor, s.paid_minor, s.change_minor, s.cost_total_minor
             FROM sales s LEFT JOIN users u ON u.user_id=s.cashier_user_id LEFT JOIN devices d ON d.device_id=s.device_id
             LEFT JOIN customers cu ON cu.customer_id=s.customer_id WHERE s.sale_id=?1",
            [sale_id],
            |r| {
                Ok(SaleDetail {
                    sale_id: r.get(0)?,
                    receipt_number: r.get(1)?,
                    status: r.get(2)?,
                    completed_at: r.get(3)?,
                    business_date: r.get(4)?,
                    cashier_user_id: r.get(5)?,
                    cashier_name: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    device_id: r.get(7)?,
                    device_name: r.get(8)?,
                    shift_id: r.get(9)?,
                    customer_id: r.get(10)?,
                    customer_name: r.get(11)?,
                    customer_phone: r.get(12)?,
                    subtotal_minor: r.get(13)?,
                    discount_minor: r.get(14)?,
                    tax_minor: r.get(15)?,
                    total_minor: r.get(16)?,
                    paid_minor: r.get(17)?,
                    change_minor: r.get(18)?,
                    cost_total_minor: if show_cost { Some(r.get(19)?) } else { None },
                    items: vec![],
                    payments: vec![],
                    refunds: vec![],
                })
            },
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("Sale"))?;
    let mut st = c.prepare_cached(
        "SELECT i.sale_item_id, i.line_no, i.product_id, i.product_name_snapshot, i.sku_snapshot, i.barcode_snapshot, i.unit, i.qty_milli,
                i.original_unit_price_minor, i.effective_unit_price_minor, i.gross_minor, i.discount_minor, i.tax_rate_bp, i.tax_inclusive,
                i.tax_minor, i.line_total_minor, i.cost_snapshot_minor,
                COALESCE((SELECT SUM(ri.qty_milli) FROM refund_items ri WHERE ri.original_sale_item_id=i.sale_item_id),0), i.is_custom,
                i.product_name_ar_snapshot
         FROM sale_items i WHERE i.sale_id=?1 ORDER BY i.line_no",
    )?;
    d.items = st
        .query_map([sale_id], |r| {
            Ok(SaleItemView {
                sale_item_id: r.get(0)?,
                line_no: r.get(1)?,
                product_id: r.get(2)?,
                name: r.get(3)?,
                sku: r.get(4)?,
                barcode: r.get(5)?,
                unit: r.get(6)?,
                qty_milli: r.get(7)?,
                original_unit_price_minor: r.get(8)?,
                unit_price_minor: r.get(9)?,
                gross_minor: r.get(10)?,
                discount_minor: r.get(11)?,
                tax_rate_bp: r.get(12)?,
                tax_inclusive: r.get::<_, i64>(13)? == 1,
                tax_minor: r.get(14)?,
                line_total_minor: r.get(15)?,
                cost_minor: if show_cost { Some(r.get(16)?) } else { None },
                refunded_qty_milli: r.get(17)?,
                is_custom: r.get::<_, i64>(18)? == 1,
                name_ar: r.get(19)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    d.payments = load_payments(c, sale_id)?;
    let mut st = c.prepare_cached(
        "SELECT refund_id, refund_receipt_number, total_minor, created_at FROM refunds WHERE original_sale_id=?1 ORDER BY created_at",
    )?;
    d.refunds = st
        .query_map([sale_id], |r| {
            Ok(RefundRef { refund_id: r.get(0)?, refund_receipt_number: r.get(1)?, total_minor: r.get(2)?, created_at: r.get(3)? })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(d)
}

impl AppCore {
    /// Commit a sale exactly once. The idempotency check, stock movements,
    /// payments, audit entry and receipt print job are one transaction.
    /// Printing happens after commit and can never undo the sale.
    pub fn pos_finalize(&self, token: &str, req: FinalizeRequest) -> AppResult<SaleResult> {
        let s = self.session(token)?;
        s.require("pos.sell")?;
        let cart_id = validate::id(&req.cart_id, "Cart")?;
        idempotency::validate_operation_id(&req.operation_id)?;
        if req.tenders.len() > 10 {
            return Err(AppError::validation("A sale can have at most 10 payments."));
        }
        // Fast replay path (no locks needed).
        let replay = self.db.read(|c| idempotency::check(c, &req.operation_id, "sale.finalize", &(&cart_id, &req.tenders)))?;
        if let Check::Replay { result } = replay {
            let mut r: SaleResult = serde_json::from_value(result)?;
            r.replayed = true;
            r.print = self.db.read(|c| crate::printing::latest_job_outcome(c, "sale", &r.sale_id))?;
            return Ok(r);
        }
        let pos_cfg: settings::PosSettings = self.db.read(|c| settings::get(c, settings::KEY_POS))?;
        let pay_cfg: settings::PaymentSettings = self.db.read(settings::payments)?;
        for t in &req.tenders {
            let cfg = pay_cfg
                .tender(&t.method)
                .filter(|c| c.enabled)
                .ok_or_else(|| AppError::validation(format!("Payment method '{}' is not enabled.", t.method)))?;
            if cfg.requires_reference && t.reference.as_deref().unwrap_or("").trim().is_empty() {
                return Err(AppError::validation(format!("{} requires a reference.", cfg.label)));
            }
            if let Some(r) = &t.reference {
                crate::setup::clean(r, "Payment reference", 60, false)?;
            }
        }
        let change_methods: Vec<&str> =
            pay_cfg.tenders.iter().filter(|t| t.allows_change && t.enabled).map(|t| t.method.as_str()).collect();

        // Negative-stock check happens before the write so that an approval can be requested.
        let shortfalls: Vec<String> = self.db.read(|c| {
            let lines = load_lines(c, &cart_id)?;
            let mut need: std::collections::BTreeMap<String, (String, i64)> = Default::default();
            for l in lines.iter().filter(|l| l.track_inventory) {
                if let Some(pid) = &l.product_id {
                    let e = need.entry(pid.clone()).or_insert((l.name.clone(), 0));
                    e.1 += l.qty_milli;
                }
            }
            let mut out = vec![];
            for (pid, (name, q)) in need {
                let on_hand = inventory::current_qty(c, &pid, &s.branch_id)?;
                if on_hand - q < 0 {
                    out.push(format!("{name} (in stock: {})", money::format_qty(on_hand)));
                }
            }
            Ok(out)
        })?;
        let negative_approved_by = if !shortfalls.is_empty() && !pos_cfg.allow_negative_stock {
            let summary = format!("Sell below zero stock: {}", shortfalls.join(", "));
            match self.authorize(&s, "pos.negative_stock", req.approval_token.as_deref(), &summary) {
                Ok(a) => a,
                Err(e) if e.code == ErrorCode::ApprovalRequired => {
                    return Err(AppError::new(
                        ErrorCode::InsufficientStock,
                        format!("Not enough stock recorded for: {}. A manager can approve selling anyway.", shortfalls.join(", ")),
                    )
                    .with_details(json!({ "permission": "pos.negative_stock", "summary": summary, "items": shortfalls })))
                }
                Err(e) => return Err(e),
            }
        } else {
            None
        };

        // Customer account tender: module, permission and credit limit.
        let account_amount: i64 = req.tenders.iter().filter(|t| t.method == "account").map(|t| t.amount_minor).sum();
        let mut account_over_ok = false;
        let mut account_approved_by = None;
        if account_amount > 0 {
            // Any cashier may charge an enabled account within its limit;
            // going over the limit needs a manager (below).
            self.require_feature("customers.credit")?;
            let (customer, over) = self.db.read(|c| {
                let cid: Option<String> =
                    c.query_row("SELECT customer_id FROM carts WHERE cart_id=?1", [&cart_id], |r| r.get(0)).optional()?.flatten();
                let over = match &cid {
                    Some(id) => {
                        let a = crate::credit::account(c, id)?;
                        a.balance_minor + account_amount > a.credit_limit_minor
                    }
                    None => false,
                };
                Ok((cid, over))
            })?;
            if customer.is_some() && over {
                account_approved_by = self.authorize(
                    &s,
                    "customers.credit_override",
                    req.approval_token.as_deref(),
                    "Sale above the customer's credit limit",
                )?;
                account_over_ok = true;
            }
        }
        let negative_approved_by = negative_approved_by.or(account_approved_by);
        let tz_currency = self.db.read(|c| Ok((self.store_timezone(c)?, pos_cfg.receipt_auto_print)))?;
        let device = self.require_device()?;
        let actor = self.actor(&s, negative_approved_by.clone());
        let result = self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "sale.finalize", &(&cart_id, &req.tenders))? {
                Check::Replay { result } => {
                    let mut r: SaleResult = serde_json::from_value(result)?;
                    r.replayed = true;
                    return Ok(r);
                }
                Check::New { payload_hash } => payload_hash,
            };
            type Head = (String, String, String, Option<String>, i64, i64, i64);
            let (status, dev, user, customer_id, cd_minor, cd_bp, loyalty_points): Head = tx
                .query_row(
                    "SELECT status, device_id, user_id, customer_id, cart_discount_minor, cart_discount_bp, loyalty_points FROM carts WHERE cart_id=?1",
                    [&cart_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Sale"))?;
            if dev != s.device_id || user != s.user_id {
                return Err(AppError::conflict("This sale belongs to another cashier or terminal."));
            }
            if status != "active" {
                return Err(AppError::conflict("This sale is no longer open. It may have been completed already; check Sales History before charging again."));
            }
            let shift_id = open_shift_for(tx, &s)?.ok_or_else(shift_required)?;
            let lines = load_lines(tx, &cart_id)?;
            if lines.is_empty() {
                return Err(AppError::validation("The sale is empty."));
            }
            let lp = crate::loyalty::price(tx, &lines, cd_minor, cd_bp, customer_id.as_deref(), loyalty_points)?;
            let (priced, totals) = (lp.lines.clone(), lp.totals.clone());
            if let Some(exp) = req.expected_total_minor {
                if exp != totals.total_minor {
                    return Err(AppError::conflict("The sale total changed. Review the basket and take payment again.")
                        .with_details(json!({ "total_minor": totals.total_minor })));
                }
            }
            let (applied, change) = pricing::apply_tenders(totals.total_minor, &req.tenders, &change_methods)?;
            let now = time::now();
            let now_s = time::fmt(now);
            let business_date = time::business_date(now, &tz_currency.0)?;
            let seq = next_seq(tx, &format!("receipt:{}", device.device_id))?;
            let receipt_number = format!("{}-{:07}", device.device_code, seq);
            let sale_id = new_id();
            let mut cost_total = 0i64;
            let mut costs = Vec::with_capacity(lines.len());
            for l in &lines {
                let unit_cost = match &l.product_id {
                    Some(pid) => inventory::avg_cost(tx, pid, &s.branch_id)?,
                    None => 0,
                };
                let c = money::extend(unit_cost, l.qty_milli)?;
                cost_total += c;
                costs.push(c);
            }
            let paid: i64 = applied.iter().map(|a| a.amount_minor).sum::<i64>() + change;
            tx.execute(
                "INSERT INTO sales(sale_id, receipt_number, branch_id, device_id, shift_id, cashier_user_id, customer_id, status,
                    subtotal_minor, discount_minor, tax_minor, total_minor, paid_minor, change_minor, cost_total_minor, item_count_milli,
                    cart_id, operation_id, business_date, completed_at, created_at, loyalty_discount_minor)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,'completed',?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?19,?20)",
                params![
                    sale_id, receipt_number, s.branch_id, s.device_id, shift_id, s.user_id, customer_id,
                    totals.subtotal_minor, totals.discount_minor, totals.tax_minor, totals.total_minor, paid, change,
                    cost_total, totals.item_count_milli, cart_id, req.operation_id, business_date, now_s, lp.loyalty_minor
                ],
            )?;
            for (((l, p), cost), loyalty_share) in lines.iter().zip(&priced).zip(&costs).zip(&lp.loyalty_alloc) {
                let item_id = new_id();
                let approver = l.discount_approved_by.clone();
                tx.execute(
                    "INSERT INTO sale_items(sale_item_id, sale_id, line_no, product_id, product_name_snapshot, sku_snapshot, barcode_snapshot,
                        category_id_snapshot, unit, qty_milli, original_unit_price_minor, effective_unit_price_minor, gross_minor, discount_minor,
                        tax_rule_id, tax_rate_bp, tax_inclusive, tax_minor, line_total_minor, cost_snapshot_minor, is_custom,
                        price_override_by, discount_approved_by, product_name_ar_snapshot, loyalty_discount_minor)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,
                        (SELECT NULLIF(TRIM(name_ar),'') FROM products WHERE product_id=?4),?24)",
                    params![
                        item_id, sale_id, l.line_no, l.product_id, l.name, l.sku, l.barcode, l.category_id, l.unit, l.qty_milli,
                        l.catalog_unit_price_minor, l.unit_price_minor, p.gross_minor, p.discount_minor, l.tax_rule_id, l.tax_rate_bp,
                        l.tax_inclusive as i64, p.tax_minor, p.line_total_minor, cost, l.is_custom as i64, l.price_override_by, approver,
                        loyalty_share
                    ],
                )?;
                if l.track_inventory {
                    if let Some(pid) = &l.product_id {
                        let unit_cost = if l.qty_milli > 0 { money::div_round(*cost as i128 * 1000, l.qty_milli as i128) as i64 } else { 0 };
                        inventory::apply_movement(
                            tx,
                            &Movement {
                                product_id: pid,
                                branch_id: &s.branch_id,
                                kind: "sale",
                                qty_delta_milli: -l.qty_milli,
                                unit_cost_minor: Some(unit_cost),
                                source_type: "sale",
                                source_id: Some(&sale_id),
                                reason: None,
                                user_id: Some(&s.user_id),
                                device_id: Some(&s.device_id),
                            },
                        )?;
                    }
                }
            }
            for a in &applied {
                tx.execute(
                    "INSERT INTO payments(payment_id, sale_id, method, amount_minor, tendered_minor, change_minor, reference, metadata_json, created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        new_id(), sale_id, a.method, a.amount_minor, a.tendered_minor, a.change_minor, a.reference,
                        json!({ "verification": "recorded_tender" }).to_string(), now_s
                    ],
                )?;
            }
            let on_account: i64 = applied.iter().filter(|a| a.method == "account").map(|a| a.amount_minor).sum();
            if on_account > 0 {
                let cid = crate::credit::check_sale(tx, customer_id.as_deref(), on_account, account_over_ok)?;
                crate::credit::post(
                    tx,
                    &s,
                    &cid,
                    "sale",
                    on_account,
                    Some("sale"),
                    Some(&sale_id),
                    Some("account"),
                    None,
                    &crate::credit::sale_op(&req.operation_id),
                )?;
            }
            tx.execute(
                "UPDATE carts SET status='completed', sale_id=?2, shift_id=?3, updated_at=?4, version=version+1 WHERE cart_id=?1",
                params![cart_id, sale_id, shift_id, now_s],
            )?;
            // Loyalty ledger entries, inside this commit.
            crate::loyalty::record_sale(tx, &s, &self.actor(&s, None), customer_id.as_deref(), &sale_id, &lp)?;
            let print_job = if tz_currency.1 {
                Some(crate::printing::enqueue(tx, "sale", &sale_id, None, Some(&s.user_id))?)
            } else {
                None
            };
            let has_cash = applied.iter().any(|a| a.method == "cash") || change > 0;
            if has_cash {
                crate::printing::enqueue_drawer_pulse(tx, Some(&s.user_id), &sale_id)?;
            }
            let result = SaleResult {
                sale_id: sale_id.clone(),
                receipt_number: receipt_number.clone(),
                total_minor: totals.total_minor,
                paid_minor: paid,
                change_minor: change,
                payments: applied
                    .iter()
                    .map(|a| PaymentView {
                        method: a.method.clone(),
                        amount_minor: a.amount_minor,
                        tendered_minor: a.tendered_minor,
                        change_minor: a.change_minor,
                        reference: a.reference.clone(),
                    })
                    .collect(),
                completed_at: now_s.clone(),
                replayed: false,
                stock_warnings: if pos_cfg.allow_negative_stock { shortfalls.clone() } else { vec![] },
                print: print_job.as_ref().map(|_| PrintOutcome::queued()),
            };
            audit::record(
                tx,
                &actor,
                "sale.completed",
                "sale",
                Some(&sale_id),
                None,
                Some(&json!({
                    "receipt_number": receipt_number, "total_minor": totals.total_minor, "tax_minor": totals.tax_minor,
                    "discount_minor": totals.discount_minor, "lines": lines.len(), "change_minor": change,
                    "negative_stock_approved_by": negative_approved_by
                })),
            )?;
            let mut stored = serde_json::to_value(&result)?;
            stored["print"] = serde_json::Value::Null;
            idempotency::complete(tx, &req.operation_id, "sale.finalize", Some(&s.user_id), Some(&s.device_id), &hash, Some(&sale_id), &stored)?;
            Ok(result)
        })?;
        let mut result = result;
        if !result.replayed {
            // Post-commit hardware I/O. Failures are reported, never raised.
            self.print_pending_for(&result.sale_id);
            self.receipt_pdf_after_commit("sale", &result.sale_id);
            // Only queues a row; WhatsApp sending happens elsewhere.
            self.wa_after_sale(&s, &result.sale_id);
            result.print = self.db.read(|c| crate::printing::latest_job_outcome(c, "sale", &result.sale_id)).unwrap_or(None);
        } else {
            result.print = self.db.read(|c| crate::printing::latest_job_outcome(c, "sale", &result.sale_id)).unwrap_or(None);
        }
        Ok(result)
    }

    pub fn sales_list(&self, token: &str, q: SalesQuery) -> AppResult<Page<SaleRow>> {
        let s = self.session(token)?;
        let own_only = !s.has("sales.view");
        if own_only && !s.has("pos.reprint") && !s.has("refund.create") && !s.has("pos.sell") {
            return Err(AppError::forbidden("sales.view"));
        }
        let limit = validate::limit(q.limit, 50, 500);
        let offset = validate::offset(q.offset);
        self.db.read(|c| {
            let tz = self.store_timezone(c)?;
            let mut wheres: Vec<String> = vec!["1=1".into()];
            let mut args: Vec<rusqlite::types::Value> = vec![];
            if own_only {
                // Cashiers only see their own recent sales on this terminal.
                args.push(s.user_id.clone().into());
                wheres.push(format!("s.cashier_user_id=?{}", args.len()));
                args.push(s.device_id.clone().into());
                wheres.push(format!("s.device_id=?{}", args.len()));
                args.push(time::fmt(time::now() - chrono::Duration::days(2)).into());
                wheres.push(format!("s.completed_at>=?{}", args.len()));
            }
            if let Some(r) = q.receipt.as_ref().map(|r| r.trim()).filter(|r| !r.is_empty()) {
                args.push(format!("%{}", r.replace('%', "")).into());
                wheres.push(format!("s.receipt_number LIKE ?{}", args.len()));
            }
            if q.from.is_some() || q.to.is_some() {
                let from = q.from.clone().unwrap_or_else(|| "2000-01-01".into());
                let to = q.to.clone().unwrap_or_else(|| "2999-12-31".into());
                let (a, b) = time::local_date_range_utc(&from, &to, &tz)?;
                args.push(a.into());
                wheres.push(format!("s.completed_at>=?{}", args.len()));
                args.push(b.into());
                wheres.push(format!("s.completed_at<?{}", args.len()));
            }
            for (v, col) in [(&q.cashier_id, "s.cashier_user_id"), (&q.customer_id, "s.customer_id"), (&q.device_id, "s.device_id"), (&q.shift_id, "s.shift_id")] {
                if let Some(v) = v.as_ref().filter(|x| !x.is_empty()) {
                    args.push(v.clone().into());
                    wheres.push(format!("{col}=?{}", args.len()));
                }
            }
            if let Some(m) = q.method.as_ref().filter(|x| !x.is_empty()) {
                args.push(m.clone().into());
                wheres.push(format!("EXISTS (SELECT 1 FROM payments p WHERE p.sale_id=s.sale_id AND p.method=?{})", args.len()));
            }
            let w = wheres.join(" AND ");
            let total: i64 = c.query_row(&format!("SELECT COUNT(*) FROM sales s WHERE {w}"), params_from_iter(args.iter()), |r| r.get(0))?;
            let sql = format!(
                "SELECT s.sale_id, s.receipt_number, s.completed_at, COALESCE(u.display_name,''), d.name, cu.name, s.item_count_milli, s.total_minor,
                    COALESCE((SELECT group_concat(DISTINCT method) FROM payments p WHERE p.sale_id=s.sale_id),''),
                    COALESCE((SELECT SUM(total_minor) FROM refunds r WHERE r.original_sale_id=s.sale_id),0)
                 FROM sales s LEFT JOIN users u ON u.user_id=s.cashier_user_id LEFT JOIN devices d ON d.device_id=s.device_id
                 LEFT JOIN customers cu ON cu.customer_id=s.customer_id
                 WHERE {w} ORDER BY s.completed_at DESC LIMIT {limit} OFFSET {offset}"
            );
            let mut st = c.prepare(&sql)?;
            let rows = st
                .query_map(params_from_iter(args.iter()), |r| {
                    let total: i64 = r.get(7)?;
                    let refunded: i64 = r.get(9)?;
                    Ok(SaleRow {
                        sale_id: r.get(0)?,
                        receipt_number: r.get(1)?,
                        completed_at: r.get(2)?,
                        cashier_name: r.get(3)?,
                        device_name: r.get(4)?,
                        customer_name: r.get(5)?,
                        item_count_milli: r.get(6)?,
                        total_minor: total,
                        methods: r.get(8)?,
                        refunded_minor: refunded,
                        status: if refunded == 0 {
                            "completed".into()
                        } else if refunded >= total {
                            "refunded".into()
                        } else {
                            "partially_refunded".into()
                        },
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Page { rows, total, limit, offset })
        })
    }

    pub fn sale_get(&self, token: &str, sale_id: &str) -> AppResult<SaleDetail> {
        let s = self.session(token)?;
        let sid = validate::id(sale_id, "Sale")?;
        let d = self.db.read(|c| load_sale_detail(c, &sid, s.has("products.view_cost")))?;
        let own = d.cashier_user_id == s.user_id && d.device_id == s.device_id;
        if !(s.has("sales.view") || s.has("refund.create") || own) {
            return Err(AppError::forbidden("sales.view"));
        }
        Ok(d)
    }

    pub fn sale_find_by_receipt(&self, token: &str, receipt_number: &str) -> AppResult<SaleDetail> {
        let s = self.session(token)?;
        let rn = crate::setup::clean(receipt_number, "Receipt number", 40, true)?;
        let sid: String = self.db.read(|c| {
            c.query_row("SELECT sale_id FROM sales WHERE receipt_number=?1 COLLATE NOCASE", [&rn], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::new(ErrorCode::NotFound, format!("No sale with receipt number {rn} was found on this terminal.")))
        })?;
        drop(s);
        self.sale_get(token, &sid)
    }

    /// Queue a reprint (marked COPY). Reprinting never touches the sale.
    pub fn sale_reprint(&self, token: &str, sale_id: &str) -> AppResult<PrintOutcome> {
        let s = self.session(token)?;
        s.require("pos.reprint")?;
        let sid = validate::id(sale_id, "Sale")?;
        let actor = self.actor(&s, None);
        let job = self.db.write(|tx| {
            let rn: String = tx
                .query_row("SELECT receipt_number FROM sales WHERE sale_id=?1", [&sid], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Sale"))?;
            let job = crate::printing::enqueue(tx, "sale", &sid, Some("COPY"), Some(&s.user_id))?;
            audit::record(tx, &actor, "receipt.reprinted", "sale", Some(&sid), None, Some(&json!({ "receipt_number": rn })))?;
            Ok(job)
        })?;
        Ok(self.print_job_run(&job))
    }
}
