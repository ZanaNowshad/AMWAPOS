//! Suppliers, purchase orders and PO receiving.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::{new_id, next_seq};
use crate::inventory::{receive_lines, ReceiveLine};
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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoReceiveLine {
    pub po_item_id: String,
    pub qty_milli: i64,
    #[serde(default)]
    pub unit_cost_minor: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoReceiveRequest {
    pub po_id: String,
    #[serde(default)]
    pub reference: Option<String>,
    pub lines: Vec<PoReceiveLine>,
    pub operation_id: String,
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
                COALESCE((SELECT CAST(SUM(MIN(qty_received_milli, qty_ordered_milli)) * 100 / SUM(qty_ordered_milli) AS INTEGER) FROM purchase_order_items i WHERE i.po_id=p.po_id), 0)
         FROM purchase_orders p JOIN suppliers s ON s.supplier_id=p.supplier_id WHERE p.po_id=?1",
        [id],
        |r| {
            Ok(PoRow {
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
                        p.allow_decimal_quantity, i.qty_ordered_milli, i.qty_received_milli, i.unit_cost_minor, i.tax_rate_bp, i.total_minor
                 FROM purchase_order_items i JOIN products p ON p.product_id=i.product_id WHERE i.po_id=?1 ORDER BY i.line_no",
            )?;
            let lines = st
                .query_map([&id], |r| {
                    let o: i64 = r.get(7)?;
                    let rc: i64 = r.get(8)?;
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
                        qty_remaining_milli: (o - rc).max(0),
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
            Ok(PoDetail { header, notes, subtotal_minor: sub, tax_minor: tax, version, lines, receipts })
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
                None => {
                    let id = new_id();
                    let number = format!("PO-{:05}", next_seq(tx, "po")?);
                    tx.execute(
                        "INSERT INTO purchase_orders(po_id, po_number, supplier_id, branch_id, status, reference, notes, expected_at, created_by, created_at, updated_at)
                         VALUES (?1,?2,?3,?4,'draft',?5,?6,?7,?8,?9,?9)",
                        params![id, number, sid, s.branch_id, reference, notes, expected, s.user_id, now],
                    )?;
                    id
                }
            };
            let (sub, tax) = write_po_lines(tx, &id, &input.lines)?;
            tx.execute("UPDATE purchase_orders SET subtotal_minor=?2, tax_minor=?3, total_minor=?4 WHERE po_id=?1", params![id, sub, tax, sub + tax])?;
            audit::record(tx, &actor, if po_id.is_some() { "po.updated" } else { "po.created" }, "purchase_order", Some(&id), None,
                Some(&json!({ "supplier_id": sid, "lines": input.lines.len(), "total_minor": sub + tax })))?;
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
                    // Close a PO short: remaining quantities will not arrive.
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

    /// Receive (part of) a purchase order. Over-receiving is refused.
    pub fn purchase_order_receive(&self, token: &str, req: PoReceiveRequest) -> AppResult<PoDetail> {
        let s = self.session(token)?;
        s.require("inventory.receive")?;
        self.require_back_office_writable()?;
        let id = validate::id(&req.po_id, "Purchase order")?;
        let reference = clean_opt(&req.reference, "Reference", 80)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "po.receive", &req)? {
                Check::Replay { .. } => return Ok(()),
                Check::New { payload_hash } => payload_hash,
            };
            let (status, supplier): (String, String) = tx
                .query_row("SELECT status, supplier_id FROM purchase_orders WHERE po_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| AppError::not_found("Purchase order"))?;
            if !matches!(status.as_str(), "ordered" | "partially_received") {
                return Err(AppError::conflict("Only ordered purchase orders can be received. Place the order first."));
            }
            let mut lines = vec![];
            for l in req.lines.iter().filter(|l| l.qty_milli != 0) {
                let iid = validate::id(&l.po_item_id, "Order line")?;
                let (po, pid, ordered, received, cost, name): (String, String, i64, i64, i64, String) = tx
                    .query_row(
                        "SELECT i.po_id, i.product_id, i.qty_ordered_milli, i.qty_received_milli, i.unit_cost_minor, p.name
                         FROM purchase_order_items i JOIN products p ON p.product_id=i.product_id WHERE i.po_item_id=?1",
                        [&iid],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                    )
                    .optional()?
                    .ok_or_else(|| AppError::not_found("Order line"))?;
                if po != id {
                    return Err(AppError::validation("An order line does not belong to this purchase order."));
                }
                if l.qty_milli < 0 {
                    return Err(AppError::validation("Received quantity cannot be negative."));
                }
                if received + l.qty_milli > ordered {
                    return Err(AppError::validation(format!(
                        "{name}: receiving {} would exceed the ordered quantity ({} remaining).",
                        money::format_qty(l.qty_milli),
                        money::format_qty(ordered - received)
                    )));
                }
                tx.execute("UPDATE purchase_order_items SET qty_received_milli=qty_received_milli+?2 WHERE po_item_id=?1", params![iid, l.qty_milli])?;
                lines.push(ReceiveLine { product_id: pid, qty_milli: l.qty_milli, unit_cost_minor: l.unit_cost_minor.unwrap_or(cost), po_item_id: Some(iid) });
            }
            if lines.is_empty() {
                return Err(AppError::validation("Enter at least one received quantity."));
            }
            let rid = new_id();
            tx.execute(
                "INSERT INTO goods_receipts(receipt_id, po_id, supplier_id, branch_id, reference, total_cost_minor, operation_id, user_id, device_id, created_at)
                 VALUES (?1,?2,?3,?4,?5,0,?6,?7,?8,?9)",
                params![rid, id, supplier, s.branch_id, reference, req.operation_id, s.user_id, s.device_id, time::now_str()],
            )?;
            let total = receive_lines(tx, &s, &rid, Some(&supplier), &lines)?;
            tx.execute("UPDATE goods_receipts SET total_cost_minor=?2 WHERE receipt_id=?1", params![rid, total])?;
            let remaining: i64 = tx.query_row(
                "SELECT COALESCE(SUM(qty_ordered_milli - qty_received_milli),0) FROM purchase_order_items WHERE po_id=?1",
                [&id],
                |r| r.get(0),
            )?;
            let new_status = if remaining == 0 { "received" } else { "partially_received" };
            tx.execute("UPDATE purchase_orders SET status=?2, updated_at=?3, version=version+1 WHERE po_id=?1", params![id, new_status, time::now_str()])?;
            let result = json!({ "receipt_id": rid, "po_id": id, "total_cost_minor": total, "status": new_status });
            audit::record(tx, &actor, "po.received", "purchase_order", Some(&id), None, Some(&result))?;
            idempotency::complete(tx, &req.operation_id, "po.receive", Some(&s.user_id), Some(&s.device_id), &hash, Some(&rid), &result)?;
            Ok(())
        })?;
        self.purchase_order_get(token, &id)
    }
}
