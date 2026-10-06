//! Receiving against a purchase order, with every difference recorded
//! (docs/PROCUREMENT.md).
//!
//! For each order line a delivery can show: what was ordered, what was
//! delivered, what was accepted, what was refused (and why), what was kept
//! although damaged, what came instead (a substitute), and what is still
//! missing (a shortage) or came extra (an overage).
//! - Only accepted quantities become stock.
//! - Refused goods never become stock and are never recorded as waste.
//! - A shortage stays visible until a person keeps it on order (backorder)
//!   or cancels it.
//! - An overage beyond the quantity tolerance needs `purchasing.approve`.
//! - A substitute keeps the link ordered A → received B and needs an
//!   explicit acceptance.
//! - Cost differences never stop goods being received; they are reviewed
//!   in the three-way match before the supplier's invoice is posted.

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::new_id;
use crate::inventory::{receive_lines, ReceiveLine};
use crate::money;
use crate::purchasing::{PoDetail, PoReceiveRequest};
use crate::service::AppCore;
use crate::settings::{self, PurchasingSettings};
use crate::setup::clean_opt;
use crate::time;
use crate::validate;

pub const REJECT_REASONS: [&str; 7] = ["damaged", "wrong_item", "short_dated", "expired", "quality", "not_ordered", "other"];

struct PoItem {
    product_id: String,
    name: String,
    ordered: i64,
    received: i64,
    cancelled: i64,
    cost: i64,
}

fn po_items(c: &Connection, po_id: &str) -> AppResult<HashMap<String, PoItem>> {
    let mut st = c.prepare(
        "SELECT i.po_item_id, i.product_id, p.name, i.qty_ordered_milli, i.qty_received_milli, i.qty_cancelled_milli, i.unit_cost_minor
         FROM purchase_order_items i JOIN products p ON p.product_id=i.product_id WHERE i.po_id=?1",
    )?;
    let rows = st.query_map([po_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            PoItem {
                product_id: r.get(1)?,
                name: r.get(2)?,
                ordered: r.get(3)?,
                received: r.get(4)?,
                cancelled: r.get(5)?,
                cost: r.get(6)?,
            },
        ))
    })?;
    let mut out = HashMap::new();
    for r in rows {
        let (k, v) = r?;
        out.insert(k, v);
    }
    Ok(out)
}

/// Overage of a line after accepting `accepted` more: how far the line
/// would then be over its order, and the over-delivery allowed without a
/// manager (the quantity tolerance).
fn over_after(i: &PoItem, accepted: i64, tol_bp: i64) -> (i64, i64, i64) {
    let before = (i.received + i.cancelled - i.ordered).max(0);
    let after = (i.received + i.cancelled + accepted - i.ordered).max(0);
    let allowed = (i.ordered as i128 * tol_bp as i128 / 10_000) as i64;
    (after - before, after, allowed)
}

/// Every difference recorded for a purchase order, newest first.
pub fn discrepancies_for_po(c: &Connection, po_id: &str) -> AppResult<Vec<Value>> {
    let mut st = c.prepare(
        "SELECT d.discrepancy_id, d.receipt_id, d.po_item_id, d.product_id, p.name, p.name_ar, d.substitute_product_id, sp.name, d.kind, d.qty_milli,
                d.reason, d.resolution, d.approved_by, d.resolved_by, d.resolved_at, d.note, d.created_at, u.display_name
         FROM receipt_discrepancies d JOIN products p ON p.product_id=d.product_id LEFT JOIN products sp ON sp.product_id=d.substitute_product_id
         LEFT JOIN users u ON u.user_id=d.created_by
         WHERE d.po_id=?1 ORDER BY d.created_at DESC, d.discrepancy_id",
    )?;
    let rows = st
        .query_map([po_id], |r| {
            Ok(json!({ "discrepancy_id": r.get::<_, String>(0)?, "receipt_id": r.get::<_, String>(1)?, "po_item_id": r.get::<_, Option<String>>(2)?,
                "product_id": r.get::<_, String>(3)?, "product_name": r.get::<_, String>(4)?, "product_name_ar": r.get::<_, Option<String>>(5)?,
                "substitute_product_id": r.get::<_, Option<String>>(6)?, "substitute_name": r.get::<_, Option<String>>(7)?,
                "kind": r.get::<_, String>(8)?, "qty_milli": r.get::<_, i64>(9)?, "reason": r.get::<_, Option<String>>(10)?,
                "resolution": r.get::<_, String>(11)?, "approved_by": r.get::<_, Option<String>>(12)?, "resolved_by": r.get::<_, Option<String>>(13)?,
                "resolved_at": r.get::<_, Option<String>>(14)?, "note": r.get::<_, Option<String>>(15)?, "created_at": r.get::<_, String>(16)?,
                "created_by_name": r.get::<_, Option<String>>(17)? }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Remaining, received and status of a purchase order after a change.
pub(crate) fn refresh_po_status(c: &Connection, po_id: &str) -> AppResult<String> {
    let (remaining, received): (i64, i64) = c.query_row(
        "SELECT COALESCE(SUM(MAX(qty_ordered_milli - qty_received_milli - qty_cancelled_milli, 0)),0), COALESCE(SUM(qty_received_milli),0)
         FROM purchase_order_items WHERE po_id=?1",
        [po_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let status = if remaining == 0 {
        if received > 0 {
            "received"
        } else {
            "cancelled"
        }
    } else if received > 0 {
        "partially_received"
    } else {
        "ordered"
    };
    c.execute(
        "UPDATE purchase_orders SET status=?2, updated_at=?3, version=version+1 WHERE po_id=?1 AND status<>?2",
        params![po_id, status, time::now_str()],
    )?;
    Ok(status.to_string())
}

#[allow(clippy::too_many_arguments)]
fn add_discrepancy(
    c: &Connection,
    receipt: &str,
    po: &str,
    po_item: Option<&str>,
    supplier: &str,
    product: &str,
    substitute: Option<&str>,
    kind: &str,
    qty: i64,
    reason: Option<&str>,
    resolution: &str,
    approved_by: Option<&str>,
    note: Option<&str>,
    user: &str,
) -> AppResult<String> {
    let id = new_id();
    let now = time::now_str();
    let resolved = resolution != "open";
    c.execute(
        "INSERT INTO receipt_discrepancies(discrepancy_id, receipt_id, po_id, po_item_id, supplier_id, product_id, substitute_product_id, kind, qty_milli,
            reason, resolution, approved_by, resolved_by, resolved_at, note, created_by, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![
            id,
            receipt,
            po,
            po_item,
            supplier,
            product,
            substitute,
            kind,
            qty,
            reason,
            resolution,
            approved_by,
            resolved.then_some(user),
            resolved.then_some(now.as_str()),
            note,
            user,
            now
        ],
    )?;
    Ok(id)
}

impl AppCore {
    /// Receive (part of) a purchase order with what was delivered, accepted
    /// and refused. Repeating the same operation id changes nothing; the same
    /// id with a different request is refused.
    pub fn purchase_order_receive(&self, token: &str, req: PoReceiveRequest) -> AppResult<PoDetail> {
        let s = self.session(token)?;
        s.require("inventory.receive")?;
        self.require_back_office_writable()?;
        let id = validate::id(&req.po_id, "Purchase order")?;
        let reference = clean_opt(&req.reference, "Reference", 80)?;
        idempotency::validate_operation_id(&req.operation_id)?;
        if let Check::Replay { .. } = self.db.read(|c| idempotency::check(c, &req.operation_id, "po.receive", &req))? {
            return self.purchase_order_get(token, &id);
        }
        // Shape checks (no database needed).
        let mut seen = std::collections::HashSet::new();
        for l in &req.lines {
            validate::id(&l.po_item_id, "Order line")?;
            if !seen.insert(l.po_item_id.clone()) {
                return Err(AppError::validation("Each order line can appear only once in a delivery."));
            }
            if l.qty_milli < 0 || l.damaged_kept_milli < 0 {
                return Err(AppError::validation("Received quantity cannot be negative."));
            }
            if l.damaged_kept_milli > l.qty_milli {
                return Err(AppError::validation("Damaged goods kept cannot be more than the quantity accepted."));
            }
            for r in &l.rejected {
                if r.qty_milli <= 0 {
                    return Err(AppError::validation("A refused quantity must be more than zero."));
                }
                if !REJECT_REASONS.contains(&r.reason.as_str()) {
                    return Err(AppError::validation("Choose why the goods were refused."));
                }
            }
            let rejected: i64 = l.rejected.iter().map(|r| r.qty_milli).sum();
            if let Some(d) = l.delivered_milli {
                if d != l.qty_milli + rejected {
                    return Err(AppError::validation("Delivered must equal accepted plus refused."));
                }
            }
            if l.substitute_product_id.is_some() && !l.accept_substitution {
                return Err(AppError::validation("A substitute product must be accepted explicitly."));
            }
        }
        for d in &req.shortages {
            if !matches!(d.decision.as_str(), "backorder" | "cancel") {
                return Err(AppError::validation("Choose to keep a missing quantity on order or to cancel it."));
            }
        }
        // Over-delivery beyond the tolerance needs a manager, bound to this
        // exact delivery.
        let (ps, overs, po_number) = self.db.read(|c| {
            let ps: PurchasingSettings = settings::get(c, settings::KEY_PURCHASING)?;
            let items = po_items(c, &id)?;
            let number: String = c
                .query_row("SELECT po_number FROM purchase_orders WHERE po_id=?1", [&id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Purchase order"))?;
            let mut overs = vec![];
            for l in &req.lines {
                if let Some(i) = items.get(&l.po_item_id) {
                    let (extra, after, allowed) = over_after(i, l.qty_milli, ps.qty_tolerance_bp);
                    if l.accept_overage && extra > 0 && after > allowed {
                        overs.push(json!({ "po_item_id": l.po_item_id, "product": i.name, "over_milli": after }));
                    }
                }
            }
            Ok((ps, overs, number))
        })?;
        let approved = if overs.is_empty() {
            None
        } else {
            let summary = format!(
                "Accept more than ordered on {po_number}: {}",
                overs
                    .iter()
                    .map(|o| format!(
                        "{} +{}",
                        o["product"].as_str().unwrap_or(""),
                        money::format_qty(o["over_milli"].as_i64().unwrap_or(0))
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            Some(
                self.authorize_bound(
                    &s,
                    "purchasing.approve",
                    req.approval_token.as_deref(),
                    &summary,
                    "po.receive.overage",
                    &id,
                    &json!({ "overs": overs }),
                )?
                .unwrap_or_else(|| s.user_id.clone()),
            )
        };
        let actor = self.actor(&s, approved.clone());
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
            let mut items = po_items(tx, &id)?;
            let rid = new_id();
            tx.execute(
                "INSERT INTO goods_receipts(receipt_id, po_id, supplier_id, branch_id, reference, total_cost_minor, operation_id, user_id, device_id, created_at)
                 VALUES (?1,?2,?3,?4,?5,0,?6,?7,?8,?9)",
                params![rid, id, supplier, s.branch_id, reference, req.operation_id, s.user_id, s.device_id, time::now_str()],
            )?;
            let mut accepted_lines: Vec<ReceiveLine> = vec![];
            let mut delivered_by_item: Vec<(String, i64, Option<String>)> = vec![];
            let mut any = false;
            for l in &req.lines {
                let item = items.get_mut(&l.po_item_id).ok_or_else(|| AppError::validation("An order line does not belong to this purchase order."))?;
                let rejected: i64 = l.rejected.iter().map(|r| r.qty_milli).sum();
                if l.qty_milli == 0 && rejected == 0 {
                    continue;
                }
                any = true;
                // Over-delivery: within tolerance, or approved above.
                let (extra, after, allowed) = over_after(item, l.qty_milli, ps.qty_tolerance_bp);
                if extra > 0 {
                    if !l.accept_overage {
                        return Err(AppError::validation(format!(
                            "{}: receiving {} would exceed the ordered quantity ({} remaining). Confirm the extra to keep it, or refuse it.",
                            item.name,
                            money::format_qty(l.qty_milli),
                            money::format_qty((item.ordered - item.received - item.cancelled).max(0))
                        )));
                    }
                    if after > allowed && approved.is_none() {
                        return Err(AppError::conflict(format!(
                            "{}: receiving {} would exceed the ordered quantity ({} remaining). A manager must approve the extra.",
                            item.name,
                            money::format_qty(l.qty_milli),
                            money::format_qty((item.ordered - item.received - item.cancelled).max(0))
                        )));
                    }
                    let note = if after <= allowed { Some("Within the quantity tolerance") } else { None };
                    add_discrepancy(tx, &rid, &id, Some(&l.po_item_id), &supplier, &item.product_id, None, "overage", extra, None, "accepted",
                        approved.as_deref(), note, &s.user_id)?;
                }
                // The product actually received (B for a substitution).
                let received_product = match &l.substitute_product_id {
                    Some(b) if *b != item.product_id => {
                        let b = validate::id(b, "Substitute product")?;
                        tx.query_row("SELECT 1 FROM products WHERE product_id=?1", [&b], |_| Ok(()))
                            .optional()?
                            .ok_or_else(|| AppError::not_found("Substitute product"))?;
                        add_discrepancy(tx, &rid, &id, Some(&l.po_item_id), &supplier, &item.product_id, Some(&b), "substitution",
                            l.qty_milli.max(1), None, "accepted", None, None, &s.user_id)?;
                        Some(b)
                    }
                    _ => None,
                };
                for r in &l.rejected {
                    let note = clean_opt(&r.note, "Note", 300)?;
                    add_discrepancy(tx, &rid, &id, Some(&l.po_item_id), &supplier, &item.product_id, received_product.as_deref(), "rejected",
                        r.qty_milli, Some(&r.reason), "rejected", None, note.as_deref(), &s.user_id)?;
                }
                if l.damaged_kept_milli > 0 {
                    add_discrepancy(tx, &rid, &id, Some(&l.po_item_id), &supplier, received_product.as_deref().unwrap_or(&item.product_id), None,
                        "damaged", l.damaged_kept_milli, Some("damaged"), "accepted", None, None, &s.user_id)?;
                }
                if l.qty_milli > 0 {
                    tx.execute("UPDATE purchase_order_items SET qty_received_milli=qty_received_milli+?2 WHERE po_item_id=?1", params![l.po_item_id, l.qty_milli])?;
                    item.received += l.qty_milli;
                    accepted_lines.push(ReceiveLine {
                        product_id: received_product.clone().unwrap_or_else(|| item.product_id.clone()),
                        qty_milli: l.qty_milli,
                        unit_cost_minor: l.unit_cost_minor.unwrap_or(item.cost),
                        po_item_id: Some(l.po_item_id.clone()),
                        lot: l.lot.clone(),
                    });
                    delivered_by_item.push((l.po_item_id.clone(), l.qty_milli + rejected, received_product.as_ref().map(|_| item.product_id.clone())));
                }
            }
            if !any {
                return Err(AppError::validation("Enter at least one received or refused quantity."));
            }
            let total = if accepted_lines.is_empty() { 0 } else { receive_lines(tx, &s, &rid, Some(&supplier), &accepted_lines)? };
            for (item, delivered, sub_for) in delivered_by_item {
                tx.execute(
                    "UPDATE goods_receipt_items SET qty_delivered_milli=?3, substitute_for_product_id=?4 WHERE receipt_id=?1 AND po_item_id=?2",
                    params![rid, item, delivered, sub_for],
                )?;
            }
            for l in &accepted_lines {
                crate::catalogue::note_supplier_product(tx, &supplier, &l.product_id)?;
            }
            tx.execute("UPDATE goods_receipts SET total_cost_minor=?2 WHERE receipt_id=?1", params![rid, total])?;
            // Earlier open shortages are overtaken by this delivery.
            tx.execute(
                "UPDATE receipt_discrepancies SET resolution='backorder', resolved_by=?2, resolved_at=?3, note=COALESCE(note,'A later delivery was received')
                 WHERE po_id=?1 AND kind='shortage' AND resolution='open'",
                params![id, s.user_id, time::now_str()],
            )?;
            // What is still missing: kept on order or cancelled as the person
            // chose; otherwise open, and visible, until someone decides.
            let decisions: HashMap<&str, &str> = req.shortages.iter().map(|d| (d.po_item_id.as_str(), d.decision.as_str())).collect();
            let mut keys: Vec<&String> = items.keys().collect();
            keys.sort();
            for k in keys {
                let i = &items[k];
                let missing = i.ordered - i.received - i.cancelled;
                if missing <= 0 {
                    continue;
                }
                let resolution = match decisions.get(k.as_str()) {
                    Some(&"cancel") => {
                        tx.execute("UPDATE purchase_order_items SET qty_cancelled_milli=qty_cancelled_milli+?2 WHERE po_item_id=?1", params![k, missing])?;
                        "cancelled"
                    }
                    Some(&"backorder") => "backorder",
                    _ => "open",
                };
                add_discrepancy(tx, &rid, &id, Some(k), &supplier, &i.product_id, None, "shortage", missing, None, resolution, None, None, &s.user_id)?;
            }
            let new_status = crate::procurement::refresh_po_status(tx, &id)?;
            let result = json!({ "receipt_id": rid, "po_id": id, "total_cost_minor": total, "status": new_status });
            audit::record(tx, &actor, "po.received", "purchase_order", Some(&id), None, Some(&result))?;
            idempotency::complete(tx, &req.operation_id, "po.receive", Some(&s.user_id), Some(&s.device_id), &hash, Some(&rid), &result)?;
            Ok(())
        })?;
        self.purchase_order_get(token, &id)
    }

    /// Decide a shortage: keep it on order (backorder) or cancel it. A
    /// cancelled quantity is no longer expected; the order closes when
    /// nothing else is outstanding.
    pub fn receipt_shortage_decide(&self, token: &str, discrepancy_id: &str, decision: &str) -> AppResult<PoDetail> {
        let s = self.session(token)?;
        s.require("purchasing.manage")?;
        self.require_back_office_writable()?;
        let did = validate::id(discrepancy_id, "Difference")?;
        if !matches!(decision, "backorder" | "cancel") {
            return Err(AppError::validation("Choose to keep the missing quantity on order or to cancel it."));
        }
        let actor = self.actor(&s, None);
        let po = self.db.write(|tx| {
            let (kind, resolution, po, item, qty): (String, String, Option<String>, Option<String>, i64) = tx
                .query_row(
                    "SELECT kind, resolution, po_id, po_item_id, qty_milli FROM receipt_discrepancies WHERE discrepancy_id=?1",
                    [&did],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Difference"))?;
            let (Some(po), Some(item)) = (po, item) else { return Err(AppError::validation("This difference has no order line.")) };
            if kind != "shortage" {
                return Err(AppError::validation("Only a missing quantity can be kept on order or cancelled."));
            }
            let target = if decision == "cancel" { "cancelled" } else { "backorder" };
            if resolution == target {
                return Ok(po); // already decided this way
            }
            if resolution == "cancelled" {
                return Err(AppError::conflict("This missing quantity was already cancelled."));
            }
            if decision == "cancel" {
                let (o, r, cx): (i64, i64, i64) = tx.query_row(
                    "SELECT qty_ordered_milli, qty_received_milli, qty_cancelled_milli FROM purchase_order_items WHERE po_item_id=?1",
                    [&item],
                    |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)),
                )?;
                let cancel = qty.min((o - r - cx).max(0));
                tx.execute(
                    "UPDATE purchase_order_items SET qty_cancelled_milli=qty_cancelled_milli+?2 WHERE po_item_id=?1",
                    params![item, cancel],
                )?;
            }
            tx.execute(
                "UPDATE receipt_discrepancies SET resolution=?2, resolved_by=?3, resolved_at=?4 WHERE discrepancy_id=?1",
                params![did, target, s.user_id, time::now_str()],
            )?;
            refresh_po_status(tx, &po)?;
            audit::record(
                tx,
                &actor,
                "po.shortage_decided",
                "purchase_order",
                Some(&po),
                Some(&json!({ "resolution": resolution })),
                Some(&json!({ "discrepancy_id": did, "resolution": target })),
            )?;
            Ok(po)
        })?;
        self.purchase_order_get(token, &po)
    }

    /// Open differences that need someone: shortages not decided.
    pub fn receiving_open_shortages(&self, token: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("purchasing.manage") && !s.has("inventory.receive") {
            return Err(AppError::forbidden("purchasing.manage"));
        }
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT d.discrepancy_id, d.po_id, o.po_number, sp.name, p.name, p.name_ar, d.qty_milli, d.created_at
                 FROM receipt_discrepancies d JOIN purchase_orders o ON o.po_id=d.po_id JOIN suppliers sp ON sp.supplier_id=o.supplier_id
                 JOIN products p ON p.product_id=d.product_id
                 WHERE d.kind='shortage' AND d.resolution='open' AND o.branch_id=?1 ORDER BY d.created_at LIMIT 500",
            )?;
            let rows: Vec<Value> = st
                .query_map([&s.branch_id], |r| {
                    Ok(json!({ "discrepancy_id": r.get::<_, String>(0)?, "po_id": r.get::<_, String>(1)?, "po_number": r.get::<_, String>(2)?,
                        "supplier": r.get::<_, String>(3)?, "product_name": r.get::<_, String>(4)?, "product_name_ar": r.get::<_, Option<String>>(5)?,
                        "qty_milli": r.get::<_, i64>(6)?, "created_at": r.get::<_, String>(7)? }))
                })?
                .collect::<Result<_, _>>()?;
            Ok(json!({ "rows": rows }))
        })
    }
}

impl AppCore {
    /// The three-way match of a supplier invoice record: order ↔ goods
    /// accepted ↔ invoice, with the evidence for every line.
    pub fn supplier_invoice_match(&self, token: &str, invoice_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("payables.view") && !s.has("purchasing.manage") && !s.has("purchasing.approve") {
            return Err(AppError::forbidden("payables.view"));
        }
        let id = validate::id(invoice_id, "Supplier invoice")?;
        self.db.read(|c| {
            let (fp, by, at, note, posting): (Option<String>, Option<String>, Option<String>, Option<String>, String) = c
                .query_row(
                    "SELECT i.match_accepted_fingerprint, u.display_name, i.match_accepted_at, i.match_accepted_note, i.posting
                     FROM supplier_invoices i LEFT JOIN users u ON u.user_id=i.match_accepted_by WHERE i.invoice_id=?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Supplier invoice"))?;
            let m = crate::payables::invoice_match(c, &id, &s.branch_id)?;
            let valid = matches!((&m, &fp), (Some(m), Some(f)) if &m.fingerprint == f);
            Ok(json!({ "invoice_id": id, "posting": posting, "match": m,
                "acceptance": fp.map(|_| json!({ "by": by, "at": at, "note": note, "still_applies": valid })) }))
        })
    }

    /// A person who approves purchasing accepts a match that needs review
    /// (a cost beyond tolerance, a line not on the order, a VAT difference).
    /// A blocked match (invoiced beyond what was received) cannot be accepted.
    pub fn supplier_invoice_accept_match(&self, token: &str, invoice_id: &str, note: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("purchasing.approve")?;
        self.require_back_office_writable()?;
        let id = validate::id(invoice_id, "Supplier invoice")?;
        let note = note.trim().chars().take(500).collect::<String>();
        if note.is_empty() {
            return Err(AppError::validation("Say why the differences are accepted."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let posting: String = tx
                .query_row("SELECT posting FROM supplier_invoices WHERE invoice_id=?1", [&id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Supplier invoice"))?;
            if posting != "not_posted" {
                return Err(AppError::conflict("This supplier invoice was already posted."));
            }
            let m = crate::payables::invoice_match(tx, &id, &s.branch_id)?.ok_or_else(|| AppError::validation("This invoice is not for a purchase order."))?;
            match m.outcome.as_str() {
                "blocked" => return Err(AppError::conflict("A blocked match cannot be accepted: the invoice charges for more than was received.")),
                "matched" | "within_tolerance" => return Err(AppError::validation("This match needs no review.")),
                _ => {}
            }
            tx.execute(
                "UPDATE supplier_invoices SET match_accepted_by=?2, match_accepted_at=?3, match_accepted_fingerprint=?4, match_accepted_note=?5 WHERE invoice_id=?1",
                params![id, s.user_id, time::now_str(), m.fingerprint, note],
            )?;
            audit::record(tx, &actor, "supplier_invoice.match_accepted", "supplier_invoice", Some(&id), None,
                Some(&json!({ "outcome": m.outcome, "fingerprint": m.fingerprint, "note": note, "summary": m.summary })))?;
            Ok(())
        })?;
        self.supplier_invoice_match(token, &id)
    }
}
