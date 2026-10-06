//! Suppliers, purchase orders and PO receiving.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::idempotency;
use crate::ids::{new_id, next_seq};
use crate::money;
use crate::service::AppCore;
use crate::setup::{clean, clean_opt};
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupplierInput {
    pub name: String,
    #[serde(default)]
    pub cr_number: Option<String>,
    #[serde(default)]
    pub vat_number: Option<String>,
    #[serde(default)]
    pub contact_name: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub whatsapp: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub payment_terms: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default = "yes")]
    pub active: bool,
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct SupplierRow {
    pub supplier_id: String,
    #[serde(flatten)]
    pub info: SupplierInput,
    pub open_po_count: i64,
    pub last_purchase_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoLineInput {
    pub product_id: String,
    pub qty_milli: i64,
    pub unit_cost_minor: i64,
    #[serde(default)]
    pub tax_rate_bp: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PoInput {
    pub supplier_id: String,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub expected_at: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    pub lines: Vec<PoLineInput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PoRow {
    pub po_id: String,
    pub po_number: String,
    pub supplier_id: String,
    pub supplier_name: String,
    pub status: String,
    pub reference: Option<String>,
    pub ordered_at: Option<String>,
    pub expected_at: Option<String>,
    pub created_at: String,
    pub line_count: i64,
    pub total_minor: i64,
    pub received_pct: i64,
    /// For a draft under the approval policy: needs_approval | approved.
    pub approval_state: Option<String>,
    pub requisition_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PoLineView {
    pub po_item_id: String,
    pub line_no: i64,
    pub product_id: String,
    pub product_name: String,
    pub sku: String,
    pub primary_barcode: Option<String>,
    pub allow_decimal_quantity: bool,
    pub qty_ordered_milli: i64,
    pub qty_received_milli: i64,
    /// Quantity a person decided will not come (a shortage cancelled).
    pub qty_cancelled_milli: i64,
    pub qty_remaining_milli: i64,
    pub unit_cost_minor: i64,
    pub tax_rate_bp: i64,
    pub total_minor: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PoDetail {
    #[serde(flatten)]
    pub header: PoRow,
    pub notes: Option<String>,
    pub subtotal_minor: i64,
    pub tax_minor: i64,
    pub version: i64,
    pub lines: Vec<PoLineView>,
    pub receipts: Vec<serde_json::Value>,
    /// The approval policy as it applies to this order, and its approvals.
    pub approval: serde_json::Value,
    /// Delivery differences recorded at receiving.
    pub discrepancies: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PoReceiveLine {
    pub po_item_id: String,
    /// Accepted into stock. Only this quantity creates stock.
    pub qty_milli: i64,
    #[serde(default)]
    pub unit_cost_minor: Option<i64>,
    #[serde(default)]
    pub lot: Option<crate::lots::LotInput>,
    /// Delivered (accepted + rejected). When given it must add up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_milli: Option<i64>,
    /// Refused at the door: never stock, never waste.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<Rejection>,
    /// Of the accepted quantity, how much is damaged but kept.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub damaged_kept_milli: i64,
    /// Product B delivered in place of the ordered product A.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub substitute_product_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accept_substitution: bool,
    /// The person confirms more than ordered arrived and is to be kept.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accept_overage: bool,
}
fn is_zero(v: &i64) -> bool {
    *v == 0
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Rejection {
    pub qty_milli: i64,
    /// damaged | wrong_item | short_dated | expired | quality | not_ordered | other
    pub reason: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// What happens to a quantity that did not come: kept on order or cancelled.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ShortageDecision {
    pub po_item_id: String,
    /// backorder | cancel
    pub decision: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PoReceiveRequest {
    pub po_id: String,
    #[serde(default)]
    pub reference: Option<String>,
    pub lines: Vec<PoReceiveLine>,
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shortages: Vec<ShortageDecision>,
    #[serde(default, skip_serializing)]
    pub approval_token: Option<String>,
}

fn validate_supplier(s: &SupplierInput) -> AppResult<SupplierInput> {
    let email = clean_opt(&s.email, "Email", 120)?;
    if let Some(e) = &email {
        if !e.contains('@') || e.contains(' ') {
            return Err(AppError::validation("Enter a valid email address."));
        }
    }
    Ok(SupplierInput {
        name: clean(&s.name, "Supplier name", 120, true)?,
        cr_number: clean_opt(&s.cr_number, "CR number", 40)?,
        vat_number: clean_opt(&s.vat_number, "VAT number", 40)?,
        contact_name: clean_opt(&s.contact_name, "Contact name", 80)?,
        phone: clean_opt(&s.phone, "Phone", 40)?,
        whatsapp: clean_opt(&s.whatsapp, "WhatsApp", 40)?,
        email,
        address: clean_opt(&s.address, "Address", 300)?,
        payment_terms: clean_opt(&s.payment_terms, "Payment terms", 80)?,
        notes: clean_opt(&s.notes, "Notes", 2000)?,
        active: s.active,
    })
}

fn load_supplier(c: &Connection, id: &str) -> AppResult<SupplierRow> {
    c.query_row(
        "SELECT s.supplier_id, s.name, s.cr_number, s.vat_number, s.contact_name, s.phone, s.whatsapp, s.email, s.address, s.payment_terms,
                s.notes, s.active, s.created_at,
                (SELECT COUNT(*) FROM purchase_orders p WHERE p.supplier_id=s.supplier_id AND p.status IN ('draft','ordered','partially_received')),
                (SELECT MAX(created_at) FROM goods_receipts g WHERE g.supplier_id=s.supplier_id)
         FROM suppliers s WHERE s.supplier_id=?1",
        [id],
        |r| {
            Ok(SupplierRow {
                supplier_id: r.get(0)?,
                info: SupplierInput {
                    name: r.get(1)?,
                    cr_number: r.get(2)?,
                    vat_number: r.get(3)?,
                    contact_name: r.get(4)?,
                    phone: r.get(5)?,
                    whatsapp: r.get(6)?,
                    email: r.get(7)?,
                    address: r.get(8)?,
                    payment_terms: r.get(9)?,
                    notes: r.get(10)?,
                    active: r.get::<_, i64>(11)? == 1,
                },
                created_at: r.get(12)?,
                open_po_count: r.get(13)?,
                last_purchase_at: r.get(14)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("Supplier"))
}

fn po_header(c: &Connection, id: &str) -> AppResult<PoRow> {
    c.query_row(
        "SELECT p.po_id, p.po_number, p.supplier_id, s.name, p.status, p.reference, p.ordered_at, p.expected_at, p.created_at,
                (SELECT COUNT(*) FROM purchase_order_items i WHERE i.po_id=p.po_id), p.total_minor,
                COALESCE((SELECT CAST(SUM(MIN(qty_received_milli, qty_ordered_milli)) * 100 / SUM(qty_ordered_milli) AS INTEGER) FROM purchase_order_items i WHERE i.po_id=p.po_id), 0),
                p.requisition_id
         FROM purchase_orders p JOIN suppliers s ON s.supplier_id=p.supplier_id WHERE p.po_id=?1",
        [id],
        |r| {
            Ok(PoRow {
                approval_state: None,
                requisition_id: r.get(12)?,
                po_id: r.get(0)?,
                po_number: r.get(1)?,
                supplier_id: r.get(2)?,
                supplier_name: r.get(3)?,
                status: r.get(4)?,
                reference: r.get(5)?,
                ordered_at: r.get(6)?,
                expected_at: r.get(7)?,
                created_at: r.get(8)?,
                line_count: r.get(9)?,
                total_minor: r.get(10)?,
                received_pct: r.get(11)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("Purchase order"))
    .and_then(|mut row| {
        if row.status == "draft" {
            let a = approval_status(c, &row.po_id)?;
            row.approval_state = match (a.required, a.valid) {
                (_, true) => Some("approved".into()),
                (true, false) => Some("needs_approval".into()),
                _ => None,
            };
        }
        Ok(row)
    })
}

/// The fingerprint of what an approval covers: supplier, every line
/// (product, quantity, cost, tax, total) and the order totals. Notes, the
/// reference and the expected date are not material.
pub(crate) fn po_fingerprint(c: &Connection, po_id: &str) -> AppResult<String> {
    use sha2::{Digest, Sha256};
    let (sup, sub, tax, total): (String, i64, i64, i64) =
        c.query_row("SELECT supplier_id, subtotal_minor, tax_minor, total_minor FROM purchase_orders WHERE po_id=?1", [po_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
    let mut h = Sha256::new();
    h.update(format!("{sup}|{sub}|{tax}|{total}\n"));
    let mut st = c.prepare_cached(
        "SELECT product_id, qty_ordered_milli, unit_cost_minor, tax_rate_bp, total_minor FROM purchase_order_items WHERE po_id=?1 ORDER BY line_no",
    )?;
    let rows = st.query_map([po_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?))
    })?;
    for r in rows {
        let (p, q, u, t, tot) = r?;
        h.update(format!("{p}|{q}|{u}|{t}|{tot}\n"));
    }
    Ok(hex::encode(h.finalize()))
}

/// Whether the approval policy asks for approval of an order of this total.
pub(crate) fn approval_needed(ps: &crate::settings::PurchasingSettings, total_minor: i64) -> bool {
    match ps.po_approval_mode.as_str() {
        "always" => true,
        "above_threshold" => total_minor > ps.po_approval_threshold_minor,
        _ => false,
    }
}

pub(crate) struct ApprovalStatus {
    pub required: bool,
    /// A recorded approval covers the order exactly as it is now.
    pub valid: bool,
    pub current: Option<String>,
}

pub(crate) fn approval_status(c: &Connection, po_id: &str) -> AppResult<ApprovalStatus> {
    let ps: crate::settings::PurchasingSettings = crate::settings::get(c, crate::settings::KEY_PURCHASING)?;
    let total: i64 = c.query_row("SELECT total_minor FROM purchase_orders WHERE po_id=?1", [po_id], |r| r.get(0))?;
    let current: Option<(String, String)> = c
        .query_row(
            "SELECT approval_id, fingerprint FROM purchase_order_approvals WHERE po_id=?1 AND invalidated_at IS NULL ORDER BY approved_at DESC, approval_id DESC LIMIT 1",
            [po_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let valid = match &current {
        Some((_, fp)) => *fp == po_fingerprint(c, po_id)?,
        None => false,
    };
    Ok(ApprovalStatus { required: approval_needed(&ps, total), valid, current: current.map(|x| x.0) })
}

/// Invalidate approvals that no longer cover the order (after a material edit).
fn invalidate_stale_approvals(c: &Connection, actor: &audit::Actor, po_id: &str) -> AppResult<()> {
    let fp = po_fingerprint(c, po_id)?;
    let mut st =
        c.prepare("SELECT approval_id FROM purchase_order_approvals WHERE po_id=?1 AND invalidated_at IS NULL AND fingerprint<>?2")?;
    let stale: Vec<String> = st.query_map(params![po_id, fp], |r| r.get(0))?.collect::<Result<_, _>>()?;
    for a in stale {
        c.execute(
            "UPDATE purchase_order_approvals SET invalidated_at=?2, invalidated_reason='edited' WHERE approval_id=?1",
            params![a, time::now_str()],
        )?;
        audit::record(
            c,
            actor,
            "po.approval_invalidated",
            "purchase_order",
            Some(po_id),
            Some(&json!({ "approval_id": a })),
            Some(&json!({ "reason": "edited" })),
        )?;
    }
    Ok(())
}

fn approval_view(c: &Connection, po_id: &str) -> AppResult<serde_json::Value> {
    let ps: crate::settings::PurchasingSettings = crate::settings::get(c, crate::settings::KEY_PURCHASING)?;
    let st = approval_status(c, po_id)?;
    let mut q = c.prepare(
        "SELECT a.approval_id, a.po_version, a.total_minor, a.approved_by, u.display_name, a.approved_at, a.note, a.invalidated_at, a.invalidated_reason, a.policy_mode
         FROM purchase_order_approvals a LEFT JOIN users u ON u.user_id=a.approved_by WHERE a.po_id=?1 ORDER BY a.approved_at DESC LIMIT 50",
    )?;
    let history: Vec<serde_json::Value> = q
        .query_map([po_id], |r| {
            Ok(json!({ "approval_id": r.get::<_, String>(0)?, "po_version": r.get::<_, i64>(1)?, "total_minor": r.get::<_, i64>(2)?,
                "approved_by": r.get::<_, String>(3)?, "approved_by_name": r.get::<_, Option<String>>(4)?, "approved_at": r.get::<_, String>(5)?,
                "note": r.get::<_, Option<String>>(6)?, "invalidated_at": r.get::<_, Option<String>>(7)?, "invalidated_reason": r.get::<_, Option<String>>(8)?,
                "policy_mode": r.get::<_, String>(9)? }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(json!({ "mode": ps.po_approval_mode, "threshold_minor": ps.po_approval_threshold_minor, "required": st.required, "valid": st.valid,
        "current_approval_id": st.current, "history": history }))
}

/// Insert a draft purchase order with its lines (shared by the purchase
/// order form and requisition conversion). Returns the new id.
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_draft_po(
    tx: &Connection,
    s: &crate::auth::Session,
    supplier_id: &str,
    reference: Option<&str>,
    notes: Option<&str>,
    expected: Option<&str>,
    lines: &[PoLineInput],
    requisition: Option<(&str, &[String])>,
) -> AppResult<String> {
    let id = new_id();
    let number = format!("PO-{:05}", next_seq(tx, "po")?);
    let now = time::now_str();
    tx.execute(
        "INSERT INTO purchase_orders(po_id, po_number, supplier_id, branch_id, status, reference, notes, expected_at, created_by, created_at, updated_at, requisition_id)
         VALUES (?1,?2,?3,?4,'draft',?5,?6,?7,?8,?9,?9,?10)",
        params![id, number, supplier_id, s.branch_id, reference, notes, expected, s.user_id, now, requisition.map(|r| r.0)],
    )?;
    let (sub, tax) = write_po_lines(tx, &id, lines)?;
    tx.execute(
        "UPDATE purchase_orders SET subtotal_minor=?2, tax_minor=?3, total_minor=?4 WHERE po_id=?1",
        params![id, sub, tax, sub + tax],
    )?;
    if let Some((_, line_ids)) = requisition {
        let mut st = tx.prepare("SELECT po_item_id FROM purchase_order_items WHERE po_id=?1 ORDER BY line_no")?;
        let items: Vec<String> = st.query_map([&id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        for (item, rl) in items.iter().zip(line_ids) {
            tx.execute("UPDATE purchase_order_items SET requisition_line_id=?2 WHERE po_item_id=?1", params![item, rl])?;
            tx.execute("UPDATE requisition_lines SET po_id=?2, po_item_id=?3 WHERE line_id=?1", params![rl, id, item])?;
        }
    }
    for l in lines {
        crate::catalogue::note_supplier_product(tx, supplier_id, &l.product_id)?;
    }
    Ok(id)
}

fn write_po_lines(c: &Connection, po_id: &str, lines: &[PoLineInput]) -> AppResult<(i64, i64)> {
    if lines.is_empty() {
        return Err(AppError::validation("Add at least one product to the purchase order."));
    }
    if lines.len() > 1000 {
        return Err(AppError::validation("A purchase order can have at most 1,000 lines."));
    }
    c.execute("DELETE FROM purchase_order_items WHERE po_id=?1", [po_id])?;
    let mut sub = 0i64;
    let mut tax = 0i64;
    let mut seen = std::collections::HashSet::new();
    for (i, l) in lines.iter().enumerate() {
        let pid = validate::id(&l.product_id, "Product")?;
        if !seen.insert(pid.clone()) {
            return Err(AppError::validation("Each product can appear only once on a purchase order."));
        }
        let (dec, name): (i64, String) = c
            .query_row("SELECT allow_decimal_quantity, name FROM products WHERE product_id=?1", [&pid], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
            .ok_or_else(|| AppError::not_found("Product"))?;
        validate::qty_positive(l.qty_milli, dec == 1, &format!("Quantity for {name}"))?;
        validate::money_non_negative(l.unit_cost_minor, &format!("Cost for {name}"))?;
        if !(0..=10000).contains(&l.tax_rate_bp) {
            return Err(AppError::validation("Invalid tax rate."));
        }
        let net = money::extend(l.unit_cost_minor, l.qty_milli)?;
        let t = money::tax_on_exclusive(net, l.tax_rate_bp)?;
        sub += net;
        tax += t;
        c.execute(
            "INSERT INTO purchase_order_items(po_item_id, po_id, line_no, product_id, qty_ordered_milli, qty_received_milli, unit_cost_minor, tax_rate_bp, total_minor)
             VALUES (?1,?2,?3,?4,?5,0,?6,?7,?8)",
            params![new_id(), po_id, i as i64 + 1, pid, l.qty_milli, l.unit_cost_minor, l.tax_rate_bp, net + t],
        )?;
    }
    Ok((sub, tax))
}

impl AppCore {
    pub fn suppliers_list(&self, token: &str, q: Option<String>, include_inactive: bool) -> AppResult<Vec<SupplierRow>> {
        let s = self.session(token)?;
        if !s.has("suppliers.manage") && !s.has("purchasing.manage") && !s.has("inventory.receive") {
            return Err(AppError::forbidden("suppliers.manage"));
        }
        self.db.read(|c| {
            let like = format!("%{}%", q.unwrap_or_default().trim().replace('%', ""));
            let mut st = c.prepare(
                "SELECT supplier_id FROM suppliers WHERE (?1 OR active=1) AND (name LIKE ?2 OR COALESCE(phone,'') LIKE ?2 OR COALESCE(contact_name,'') LIKE ?2)
                 ORDER BY active DESC, name COLLATE NOCASE LIMIT 1000",
            )?;
            let ids = st.query_map(params![include_inactive, like], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            ids.iter().map(|id| load_supplier(c, id)).collect()
        })
    }

    pub fn supplier_get(&self, token: &str, supplier_id: &str) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        if !s.has("suppliers.manage") && !s.has("purchasing.manage") {
            return Err(AppError::forbidden("suppliers.manage"));
        }
        let id = validate::id(supplier_id, "Supplier")?;
        self.db.read(|c| {
            let sup = load_supplier(c, &id)?;
            let mut st = c.prepare("SELECT po_id FROM purchase_orders WHERE supplier_id=?1 ORDER BY created_at DESC LIMIT 100")?;
            let pos = st.query_map([&id], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            let pos: Vec<PoRow> = pos.iter().map(|p| po_header(c, p)).collect::<AppResult<_>>()?;
            let mut st = c.prepare(
                "SELECT p.product_id, p.name, p.sku, h.cost_minor, h.effective_at FROM product_cost_history h JOIN products p ON p.product_id=h.product_id
                 WHERE h.supplier_id=?1 AND h.effective_at = (SELECT MAX(effective_at) FROM product_cost_history h2 WHERE h2.product_id=h.product_id AND h2.supplier_id=?1)
                 ORDER BY p.name LIMIT 500",
            )?;
            let products = st
                .query_map([&id], |r| {
                    Ok(json!({ "product_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "sku": r.get::<_, String>(2)?,
                        "last_cost_minor": if s.has("products.view_cost") { Some(r.get::<_, i64>(3)?) } else { None }, "last_received_at": r.get::<_, String>(4)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({ "supplier": sup, "purchase_orders": pos, "products": products }))
        })
    }

    pub fn supplier_save(&self, token: &str, supplier_id: Option<String>, input: SupplierInput) -> AppResult<SupplierRow> {
        let s = self.session(token)?;
        s.require("suppliers.manage")?;
        self.require_back_office_writable()?;
        let v = validate_supplier(&input)?;
        let actor = self.actor(&s, None);
        let id = self.db.write(|tx| {
            let now = time::now_str();
            let dup: Option<String> = tx
                .query_row("SELECT supplier_id FROM suppliers WHERE name=?1 COLLATE NOCASE AND supplier_id IS NOT ?2", params![v.name, supplier_id], |r| r.get(0))
                .optional()?;
            if dup.is_some() {
                return Err(AppError::duplicate(format!("A supplier named {} already exists.", v.name)));
            }
            let id = match &supplier_id {
                Some(id) => {
                    let id = validate::id(id, "Supplier")?;
                    let before = serde_json::to_value(load_supplier(tx, &id)?.info)?;
                    tx.execute(
                        "UPDATE suppliers SET name=?2, cr_number=?3, vat_number=?4, contact_name=?5, phone=?6, whatsapp=?7, email=?8, address=?9,
                            payment_terms=?10, notes=?11, active=?12, updated_at=?13 WHERE supplier_id=?1",
                        params![id, v.name, v.cr_number, v.vat_number, v.contact_name, v.phone, v.whatsapp, v.email, v.address, v.payment_terms, v.notes, v.active as i64, now],
                    )?;
                    audit::record(tx, &actor, "supplier.updated", "supplier", Some(&id), Some(&before), Some(&serde_json::to_value(&v)?))?;
                    id
                }
                None => {
                    let id = new_id();
                    tx.execute(
                        "INSERT INTO suppliers(supplier_id, name, cr_number, vat_number, contact_name, phone, whatsapp, email, address, payment_terms, notes, active, created_at, updated_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?13)",
                        params![id, v.name, v.cr_number, v.vat_number, v.contact_name, v.phone, v.whatsapp, v.email, v.address, v.payment_terms, v.notes, v.active as i64, now],
                    )?;
                    audit::record(tx, &actor, "supplier.created", "supplier", Some(&id), None, Some(&serde_json::to_value(&v)?))?;
                    id
                }
            };
            Ok(id)
        })?;
        self.db.read(|c| load_supplier(c, &id))
    }

    pub fn purchase_orders_list(&self, token: &str, status: Option<String>, supplier_id: Option<String>) -> AppResult<Vec<PoRow>> {
        let s = self.session(token)?;
        if !s.has("purchasing.manage") && !s.has("inventory.receive") {
            return Err(AppError::forbidden("purchasing.manage"));
        }
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT po_id FROM purchase_orders WHERE (?1 IS NULL OR status=?1) AND (?2 IS NULL OR supplier_id=?2) ORDER BY created_at DESC LIMIT 500",
            )?;
            let ids = st
                .query_map(params![status.filter(|x| !x.is_empty()), supplier_id.filter(|x| !x.is_empty())], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids.iter().map(|id| po_header(c, id)).collect()
        })
    }

    pub fn purchase_order_get(&self, token: &str, po_id: &str) -> AppResult<PoDetail> {
        let s = self.session(token)?;
        if !s.has("purchasing.manage") && !s.has("inventory.receive") {
            return Err(AppError::forbidden("purchasing.manage"));
        }
        let id = validate::id(po_id, "Purchase order")?;
        self.db.read(|c| {
            let header = po_header(c, &id)?;
            let (notes, sub, tax, version): (Option<String>, i64, i64, i64) =
                c.query_row("SELECT notes, subtotal_minor, tax_minor, version FROM purchase_orders WHERE po_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            let mut st = c.prepare(
                "SELECT i.po_item_id, i.line_no, i.product_id, p.name, p.sku,
                        (SELECT barcode FROM product_barcodes b WHERE b.product_id=p.product_id ORDER BY is_primary DESC LIMIT 1),
                        p.allow_decimal_quantity, i.qty_ordered_milli, i.qty_received_milli, i.unit_cost_minor, i.tax_rate_bp, i.total_minor, i.qty_cancelled_milli
                 FROM purchase_order_items i JOIN products p ON p.product_id=i.product_id WHERE i.po_id=?1 ORDER BY i.line_no",
            )?;
            let lines = st
                .query_map([&id], |r| {
                    let o: i64 = r.get(7)?;
                    let rc: i64 = r.get(8)?;
                    let cx: i64 = r.get(12)?;
                    Ok(PoLineView {
                        po_item_id: r.get(0)?,
                        line_no: r.get(1)?,
                        product_id: r.get(2)?,
                        product_name: r.get(3)?,
                        sku: r.get(4)?,
                        primary_barcode: r.get(5)?,
                        allow_decimal_quantity: r.get::<_, i64>(6)? == 1,
                        qty_ordered_milli: o,
                        qty_received_milli: rc,
                        qty_cancelled_milli: cx,
                        qty_remaining_milli: (o - rc - cx).max(0),
                        unit_cost_minor: r.get(9)?,
                        tax_rate_bp: r.get(10)?,
                        total_minor: r.get(11)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut st = c.prepare(
                "SELECT g.receipt_id, g.reference, g.total_cost_minor, g.created_at, u.display_name FROM goods_receipts g LEFT JOIN users u ON u.user_id=g.user_id
                 WHERE g.po_id=?1 ORDER BY g.created_at",
            )?;
            let receipts = st
                .query_map([&id], |r| {
                    Ok(json!({ "receipt_id": r.get::<_, String>(0)?, "reference": r.get::<_, Option<String>>(1)?, "total_cost_minor": r.get::<_, i64>(2)?,
                        "created_at": r.get::<_, String>(3)?, "user_name": r.get::<_, Option<String>>(4)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let approval = approval_view(c, &id)?;
            let discrepancies = crate::procurement::discrepancies_for_po(c, &id)?;
            Ok(PoDetail { header, notes, subtotal_minor: sub, tax_minor: tax, version, lines, receipts, approval, discrepancies })
        })
    }

    /// Create (po_id None) or update a draft purchase order.
    pub fn purchase_order_save(&self, token: &str, po_id: Option<String>, input: PoInput) -> AppResult<PoDetail> {
        let s = self.session(token)?;
        s.require("purchasing.manage")?;
        self.require_back_office_writable()?;
        let sid = validate::id(&input.supplier_id, "Supplier")?;
        let reference = clean_opt(&input.reference, "Reference", 80)?;
        let notes = clean_opt(&input.notes, "Notes", 2000)?;
        let expected = match input.expected_at.as_ref().filter(|x| !x.is_empty()) {
            Some(d) => {
                time::validate_date(d)?;
                Some(d.clone())
            }
            None => None,
        };
        let actor = self.actor(&s, None);
        let id = self.db.write(|tx| {
            let ok: bool = tx.query_row("SELECT active FROM suppliers WHERE supplier_id=?1", [&sid], |r| r.get::<_, i64>(0)).optional()?.map(|a| a == 1).unwrap_or(false);
            if !ok {
                return Err(AppError::validation("Choose an active supplier."));
            }
            let now = time::now_str();
            let id = match &po_id {
                Some(id) => {
                    let id = validate::id(id, "Purchase order")?;
                    let status: String = tx
                        .query_row("SELECT status FROM purchase_orders WHERE po_id=?1", [&id], |r| r.get(0))
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Purchase order"))?;
                    if status != "draft" {
                        return Err(AppError::conflict("Only draft purchase orders can be edited."));
                    }
                    tx.execute(
                        "UPDATE purchase_orders SET supplier_id=?2, reference=?3, expected_at=?4, notes=?5, updated_at=?6, version=version+1 WHERE po_id=?1",
                        params![id, sid, reference, expected, notes, now],
                    )?;
                    id
                }
                None => insert_draft_po(tx, &s, &sid, reference.as_deref(), notes.as_deref(), expected.as_deref(), &input.lines, None)?,
            };
            if po_id.is_some() {
                let (sub, tax) = write_po_lines(tx, &id, &input.lines)?;
                tx.execute("UPDATE purchase_orders SET subtotal_minor=?2, tax_minor=?3, total_minor=?4 WHERE po_id=?1", params![id, sub, tax, sub + tax])?;
                for l in &input.lines {
                    crate::catalogue::note_supplier_product(tx, &sid, &l.product_id)?;
                }
                // A material change (supplier, lines, quantities, costs, taxes,
                // totals) voids an earlier approval.
                invalidate_stale_approvals(tx, &actor, &id)?;
            }
            let total: i64 = tx.query_row("SELECT total_minor FROM purchase_orders WHERE po_id=?1", [&id], |r| r.get(0))?;
            audit::record(tx, &actor, if po_id.is_some() { "po.updated" } else { "po.created" }, "purchase_order", Some(&id), None,
                Some(&json!({ "supplier_id": sid, "lines": input.lines.len(), "total_minor": total })))?;
            Ok(id)
        })?;
        self.purchase_order_get(token, &id)
    }

    /// draft → ordered, or cancel (draft/ordered with nothing received).
    pub fn purchase_order_set_status(&self, token: &str, po_id: &str, status: &str) -> AppResult<PoDetail> {
        let s = self.session(token)?;
        s.require("purchasing.manage")?;
        self.require_back_office_writable()?;
        let id = validate::id(po_id, "Purchase order")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let cur: String = tx
                .query_row("SELECT status FROM purchase_orders WHERE po_id=?1", [&id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Purchase order"))?;
            let received: i64 =
                tx.query_row("SELECT COALESCE(SUM(qty_received_milli),0) FROM purchase_order_items WHERE po_id=?1", [&id], |r| r.get(0))?;
            match (cur.as_str(), status) {
                ("draft", "ordered") => {
                    let a = approval_status(tx, &id)?;
                    if a.required && !a.valid {
                        return Err(AppError::conflict("This purchase order needs approval before it is placed.")
                            .with_details(json!({ "kind": "approval_required" })));
                    }
                    tx.execute(
                        "UPDATE purchase_orders SET status='ordered', ordered_at=?2, updated_at=?2, version=version+1 WHERE po_id=?1",
                        params![id, time::now_str()],
                    )?;
                }
                ("draft", "cancelled") | ("ordered", "cancelled") if received == 0 => {
                    tx.execute(
                        "UPDATE purchase_orders SET status='cancelled', updated_at=?2, version=version+1 WHERE po_id=?1",
                        params![id, time::now_str()],
                    )?;
                }
                ("partially_received", "received") => {
                    // Close a PO short: a person decided the remaining
                    // quantities will not arrive; they are cancelled, visibly.
                    tx.execute(
                        "UPDATE purchase_order_items SET qty_cancelled_milli = qty_cancelled_milli + MAX(qty_ordered_milli - qty_received_milli - qty_cancelled_milli, 0) WHERE po_id=?1",
                        [&id],
                    )?;
                    tx.execute(
                        "UPDATE receipt_discrepancies SET resolution='cancelled', resolved_by=?2, resolved_at=?3 WHERE po_id=?1 AND kind='shortage' AND resolution IN ('open','backorder')",
                        params![id, s.user_id, time::now_str()],
                    )?;
                    tx.execute(
                        "UPDATE purchase_orders SET status='received', updated_at=?2, version=version+1 WHERE po_id=?1",
                        params![id, time::now_str()],
                    )?;
                }
                _ => return Err(AppError::conflict(format!("A purchase order that is {cur} cannot become {status}."))),
            }
            audit::record(
                tx,
                &actor,
                "po.status",
                "purchase_order",
                Some(&id),
                Some(&json!({ "status": cur })),
                Some(&json!({ "status": status })),
            )?;
            Ok(())
        })?;
        self.purchase_order_get(token, &id)
    }

    /// Approve a draft purchase order exactly as it is now. The approval is
    /// durable (who, when, which revision and total) and stops covering the
    /// order as soon as a material detail changes.
    pub fn purchase_order_approve(&self, token: &str, po_id: &str, note: Option<String>, operation_id: &str) -> AppResult<PoDetail> {
        let s = self.session(token)?;
        s.require("purchasing.approve")?;
        self.require_back_office_writable()?;
        idempotency::validate_operation_id(operation_id)?;
        let id = validate::id(po_id, "Purchase order")?;
        let note = clean_opt(&note, "Note", 500)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let done: Option<String> =
                tx.query_row("SELECT po_id FROM purchase_order_approvals WHERE operation_id=?1", [operation_id], |r| r.get(0)).optional()?;
            if let Some(done) = done {
                if done != id {
                    return Err(AppError::new(crate::error::ErrorCode::IdempotencyMismatch, "This operation id was used for another approval."));
                }
                return Ok(());
            }
            let (status, version, total): (String, i64, i64) = tx
                .query_row("SELECT status, version, total_minor FROM purchase_orders WHERE po_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?
                .ok_or_else(|| AppError::not_found("Purchase order"))?;
            if status != "draft" {
                return Err(AppError::conflict("Only a draft purchase order can be approved."));
            }
            if approval_status(tx, &id)?.valid {
                return Err(AppError::conflict("This purchase order is already approved as it is."));
            }
            let ps: crate::settings::PurchasingSettings = crate::settings::get(tx, crate::settings::KEY_PURCHASING)?;
            let fp = po_fingerprint(tx, &id)?;
            let aid = new_id();
            tx.execute(
                "INSERT INTO purchase_order_approvals(approval_id, po_id, po_version, fingerprint, total_minor, policy_mode, threshold_minor, approved_by, approved_at, note, operation_id)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![aid, id, version, fp, total, ps.po_approval_mode, ps.po_approval_threshold_minor, s.user_id, time::now_str(), note, operation_id],
            )?;
            audit::record(tx, &actor, "po.approved", "purchase_order", Some(&id), None,
                Some(&json!({ "approval_id": aid, "po_version": version, "total_minor": total, "policy_mode": ps.po_approval_mode })))?;
            Ok(())
        })?;
        self.purchase_order_get(token, &id)
    }
}
