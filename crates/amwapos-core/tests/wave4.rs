//! Wave 4 of the merchant operating system: procurement. Supplier
//! catalogue, suggested orders, requisitions, purchase order approval,
//! receiving with differences, the three-way match, supplier returns and
//! their credits (docs/PROCUREMENT.md).

mod common;

use amwapos_core::lots::{replay, LotInput};
use amwapos_core::pricing::TenderInput;
use amwapos_core::purchasing::{PoDetail, PoReceiveLine, PoReceiveRequest, Rejection, ShortageDecision};
use amwapos_core::sales::{FinalizeRequest, SaleResult};
use amwapos_core::ErrorCode;
use common::*;
use serde_json::{json, Value};

fn one<T: rusqlite::types::FromSql>(e: &Env, sql: &str) -> T {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn branch(e: &Env) -> String {
    one(e, "SELECT branch_id FROM branches LIMIT 1")
}

fn today(e: &Env) -> String {
    e.core.db.read(|c| amwapos_core::time::business_date(amwapos_core::time::now(), &amwapos_core::time::day(c)?)).unwrap()
}

fn plus_days(d: &str, n: i64) -> String {
    (chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap() + chrono::Duration::days(n)).to_string()
}

fn stock(e: &Env, pid: &str) -> i64 {
    e.core
        .db
        .read(|c| Ok(c.query_row("SELECT COALESCE(SUM(qty_milli),0) FROM stock_levels WHERE product_id=?1", [pid], |r| r.get(0))?))
        .unwrap()
}

fn supplier(e: &Env, name: &str) -> String {
    e.core.supplier_save(&e.owner_token, None, serde_json::from_value(json!({ "name": name })).unwrap()).unwrap().supplier_id
}

fn terms(e: &Env, sid: &str, pid: &str, upc: Option<i64>, moq: Option<i64>, lead: Option<i64>, preferred: bool) {
    e.core
        .supplier_terms_save(
            &e.owner_token,
            serde_json::from_value(json!({ "supplier_id": sid, "product_id": pid, "units_per_case": upc, "moq_packs": moq,
                "lead_time_days": lead, "preferred": preferred }))
            .unwrap(),
        )
        .unwrap();
}

fn po(e: &Env, sid: &str, lines: Value) -> PoDetail {
    e.core
        .purchase_order_save(&e.owner_token, None, serde_json::from_value(json!({ "supplier_id": sid, "lines": lines })).unwrap())
        .unwrap()
}

fn sell(e: &Env, t: &str, barcode: &str, qty: i64) -> SaleResult {
    let cart = e.core.pos_scan(t, barcode, Some(qty)).unwrap().cart;
    let total = cart.totals.total_minor;
    e.core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![TenderInput { method: "cash".into(), amount_minor: total, reference: None }],
                approval_token: None,
                expected_total_minor: Some(total),
                fulfilment: None,
            },
        )
        .unwrap()
}

/// A copy of a sale as if made on `date` (sales are immutable; this writes
/// a new one with the same lines).
fn backdate(e: &Env, sale_id: &str, date: &str) {
    let at = format!("{date}T09:00:00.000Z");
    let n = ulid::Ulid::new().to_string();
    e.core
        .db
        .write(|tx| {
            let cols: Vec<String> = tx.prepare("SELECT name FROM pragma_table_info('sales')")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            let sel: Vec<String> = cols
                .iter()
                .map(|c| match c.as_str() {
                    "sale_id" => format!("'S{n}'"),
                    "receipt_number" => format!("'B-{}'", &n[16..]),
                    "operation_id" => format!("'o{n}'"),
                    "business_date" => format!("'{date}'"),
                    "completed_at" | "created_at" => format!("'{at}'"),
                    "cart_id" => "NULL".into(),
                    x => format!("\"{x}\""),
                })
                .collect();
            tx.execute(&format!("INSERT INTO sales ({}) SELECT {} FROM sales WHERE sale_id=?1", cols.join(","), sel.join(",")), [sale_id])?;
            tx.execute(
                "INSERT INTO sale_items (sale_item_id, sale_id, line_no, product_id, product_name_snapshot, unit, qty_milli, original_unit_price_minor,
                     effective_unit_price_minor, gross_minor, discount_minor, tax_rate_bp, tax_inclusive, tax_minor, line_total_minor, cost_snapshot_minor)
                 SELECT sale_item_id || ?2, 'S' || ?2, line_no, product_id, product_name_snapshot, unit, qty_milli, original_unit_price_minor,
                     effective_unit_price_minor, gross_minor, discount_minor, tax_rate_bp, tax_inclusive, tax_minor, line_total_minor, cost_snapshot_minor
                 FROM sale_items WHERE sale_id=?1",
                rusqlite::params![sale_id, n],
            )?;
            Ok(())
        })
        .unwrap();
}

/// A product that sold `per_day` units on each of the last 30 days.
fn selling_product(e: &Env, name: &str, barcode: &str, on_hand: i64, per_day: i64) -> String {
    let pid = e.product(name, barcode, 500, 300, on_hand + per_day);
    // No manual reorder point: the engine works from demand.
    e.core.db.write(|tx| Ok(tx.execute("UPDATE products SET reorder_point_milli=0 WHERE product_id=?1", [&pid])?)).unwrap();
    let base = sell(e, &e.owner_token, barcode, per_day);
    let d = today(e);
    for i in 1..=30 {
        backdate(e, &base.sale_id, &plus_days(&d, -i));
    }
    // Its first sale (30 days ago) is its first day: 30 days of history.
    pid
}

fn suggestion(e: &Env, pid: &str) -> Value {
    let v = e.core.replenishment(&e.owner_token, serde_json::from_value(json!({ "product_ids": [pid], "states": ["order","covered","insufficient_history","no_demand","no_supplier","no_lead_time","inactive","invalid_pack"] })).unwrap()).unwrap();
    v["rows"][0].clone()
}

fn receive(e: &Env, t: &str, po_id: &str, lines: Vec<PoReceiveLine>, op_id: &str) -> Result<PoDetail, amwapos_core::AppError> {
    e.core.purchase_order_receive(
        t,
        PoReceiveRequest { po_id: po_id.into(), reference: Some("DN-1".into()), lines, operation_id: op_id.into(), ..Default::default() },
    )
}

fn line(item: &str, qty: i64) -> PoReceiveLine {
    PoReceiveLine { po_item_id: item.into(), qty_milli: qty, ..Default::default() }
}

// ---------------------------------------------------------------- catalogue

#[test]
fn supplier_terms_are_one_model_and_a_person_wins_over_documents() {
    let e = env();
    let sid = supplier(&e, "Awal Dairy");
    let pid = e.product("Laban", "9001", 500, 300, 0);
    // A purchase order makes the supplier-product row, with no terms invented.
    po(&e, &sid, json!([{ "product_id": pid, "qty_milli": 1000, "unit_cost_minor": 300 }]));
    let row: (Option<i64>, Option<i64>, Option<i64>, i64) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT units_per_case, moq_packs, lead_time_days, preferred FROM supplier_products", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?)
        })
        .unwrap();
    assert_eq!(row, (None, None, None, 0));
    // Document learning fills an empty pack size (as document evidence)...
    e.core.db.write(|tx| amwapos_core::catalogue::learn_from_document(tx, &sid, &pid, Some(12))).unwrap();
    assert_eq!(one::<String>(&e, "SELECT pack_source FROM supplier_products"), "document");
    // ...a person's confirmed terms replace it, and later documents do not.
    terms(&e, &sid, &pid, Some(24), Some(2), Some(3), true);
    e.core.db.write(|tx| amwapos_core::catalogue::learn_from_document(tx, &sid, &pid, Some(6))).unwrap();
    let (upc, src): (i64, String) = e
        .core
        .db
        .read(|c| Ok(c.query_row("SELECT units_per_case, pack_source FROM supplier_products", [], |r| Ok((r.get(0)?, r.get(1)?)))?))
        .unwrap();
    assert_eq!((upc, src.as_str()), (24, "person"));
    assert_eq!(e.core.db.read(|c| amwapos_core::catalogue::confirmed_pack(c, &sid, &pid)).unwrap(), Some(24));
    // Zero or negative pack sizes are refused, by the API and by the schema.
    for bad in [0, -6] {
        let err = e
            .core
            .supplier_terms_save(
                &e.owner_token,
                serde_json::from_value(json!({ "supplier_id": sid, "product_id": pid, "units_per_case": bad })).unwrap(),
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Validation);
    }
    assert!(e.core.db.write(|tx| Ok(tx.execute("UPDATE supplier_products SET units_per_case=0", [])?)).is_err());
    // One preferred supplier per product.
    let s2 = supplier(&e, "Gulf Foods");
    terms(&e, &s2, &pid, None, None, Some(5), true);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM supplier_products WHERE preferred=1"), 1);
    assert_eq!(one::<String>(&e, "SELECT supplier_id FROM supplier_products WHERE preferred=1"), s2);
    // The pack arithmetic: a case of 24, a need of 37, a minimum of 2 cases → 48.
    assert_eq!(amwapos_core::catalogue::order_quantity(37_000, Some(24), Some(2), false).unwrap(), (Some(2), 48_000));
}

// ---------------------------------------------------------------- replenishment

#[test]
fn suggested_orders_count_stock_once_and_never_order_by_themselves() {
    let e = env();
    e.open_shift(&e.owner_token, 1_000);
    let sid = supplier(&e, "Awal Dairy");
    // 2 a day for 30 days; 10 usable on hand.
    let pid = selling_product(&e, "Fresh milk", "9101", 10_000, 2_000);
    terms(&e, &sid, &pid, Some(6), None, Some(4), true);
    let pos_before = one::<i64>(&e, "SELECT COUNT(*) FROM purchase_orders");
    let r = suggestion(&e, &pid);
    assert_eq!(r["state"], "order", "{r}");
    // 2/day × (lead 4 + safety 3) = 14 reorder point; × (4+3+7) = 28 up to.
    assert_eq!(r["reorder_point_milli"], 14_000);
    assert_eq!(r["order_up_to_milli"], 28_000);
    assert_eq!(r["position_milli"], 10_000);
    assert_eq!(r["need_milli"], 18_000);
    assert_eq!((r["packs"].as_i64(), r["suggested_milli"].as_i64()), (Some(3), Some(18_000)));
    assert!(r["reasons"].as_array().unwrap().iter().any(|x| x == "rounded_to_packs"));
    assert_eq!(r["supplier"]["supplier_id"], sid);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM purchase_orders"), pos_before, "a suggestion never orders");

    // A requisition from the suggestion: quantities recomputed on the server.
    let req = e
        .core
        .requisition_from_suggestions(
            &e.owner_token,
            serde_json::from_value(json!({ "product_ids": [pid], "operation_id": op() })).unwrap(),
        )
        .unwrap();
    assert_eq!(req["lines"][0]["qty_milli"], 18_000);
    assert_eq!(req["lines"][0]["source"], "replenishment");
    assert_eq!(req["lines"][0]["evidence"]["reorder_point_milli"], 14_000);
    // Now requested: no second suggestion (no double ordering).
    let r = suggestion(&e, &pid);
    assert_eq!(r["state"], "covered", "{r}");
    assert_eq!(r["facts"]["requisitioned_milli"], 18_000);
    // After conversion the order counts instead of the requisition, once.
    let rid = req["requisition_id"].as_str().unwrap();
    // No confirmed cost from this supplier yet: conversion would refuse, so
    // the person enters the expected cost. The line keeps its provenance.
    let l0 = &req["lines"][0];
    let saved = e
        .core
        .requisition_save(
            &e.owner_token,
            rid,
            serde_json::from_value(json!({ "lines": [{ "line_id": l0["line_id"], "product_id": pid,
            "supplier_id": sid, "qty_milli": 18_000, "unit_cost_minor": 310 }] }))
            .unwrap(),
        )
        .unwrap();
    assert_eq!(saved["lines"][0]["source"], "replenishment");
    assert_eq!(saved["lines"][0]["evidence"]["edited"], Value::Null, "same quantity and supplier: not edited");
    e.core.requisition_set_status(&e.owner_token, rid, "submit", None).unwrap();
    e.core.requisition_set_status(&e.owner_token, rid, "approve", None).unwrap();
    e.core.requisition_convert(&e.owner_token, rid, &op()).unwrap();
    let r = suggestion(&e, &pid);
    assert_eq!((r["facts"]["requisitioned_milli"].as_i64(), r["facts"]["draft_po_milli"].as_i64()), (Some(0), Some(18_000)));
    assert_eq!(r["position_milli"], 28_000);
    assert_eq!(r["state"], "covered");

    // Expired use-by stock is not usable; holds are not usable.
    let pid2 = selling_product(&e, "Yoghurt", "9102", 0, 1_000);
    terms(&e, &sid, &pid2, None, None, Some(2), true);
    e.core.product_lot_settings(&e.owner_token, &pid2, true, Some("use_by".into())).unwrap();
    let d = today(&e);
    e.core
        .inventory_receive(
            &e.owner_token,
            serde_json::from_value(json!({ "supplier_id": sid, "operation_id": op(), "lines": [{ "product_id": pid2, "qty_milli": 30_000, "unit_cost_minor": 200,
                "lot": { "expires_on": plus_days(&d, -1), "confirm_warnings": true } }] }))
            .unwrap(),
        )
        .unwrap();
    let r = suggestion(&e, &pid2);
    assert_eq!(r["facts"]["expired_use_by_milli"], 30_000, "{r}");
    assert_eq!(r["usable_milli"], 0);
    assert_eq!(r["state"], "order");
    assert!(r["reasons"].as_array().unwrap().iter().any(|x| x == "expired_stock_not_counted"));
}

#[test]
fn the_engine_says_why_when_it_cannot_suggest() {
    let e = env();
    e.open_shift(&e.owner_token, 1_000);
    let sid = supplier(&e, "Awal Dairy");
    let no_sup = selling_product(&e, "No supplier", "9201", 0, 1_000);
    assert_eq!(suggestion(&e, &no_sup)["state"], "no_supplier");
    let no_lead = selling_product(&e, "No lead time", "9202", 0, 1_000);
    terms(&e, &sid, &no_lead, None, None, None, false);
    let r = suggestion(&e, &no_lead);
    assert_eq!(r["state"], "no_lead_time");
    assert_eq!(r["supplier"]["supplier_id"], sid, "the supplier is still shown");
    let fresh = e.product("Brand new", "9203", 100, 50, 0);
    e.core.db.write(|tx| Ok(tx.execute("UPDATE products SET reorder_point_milli=0 WHERE product_id=?1", [&fresh])?)).unwrap();
    assert_eq!(suggestion(&e, &fresh)["state"], "insufficient_history");
    let inactive = selling_product(&e, "Retired", "9204", 0, 1_000);
    e.core.db.write(|tx| Ok(tx.execute("UPDATE products SET active=0 WHERE product_id=?1", [&inactive])?)).unwrap();
    assert_eq!(suggestion(&e, &inactive)["state"], "inactive");
    // Inactive supplier terms are not used.
    let p = selling_product(&e, "Inactive terms", "9205", 0, 1_000);
    terms(&e, &sid, &p, None, None, Some(2), true);
    e.core
        .supplier_terms_save(
            &e.owner_token,
            serde_json::from_value(json!({ "supplier_id": sid, "product_id": p, "lead_time_days": 2, "active": false })).unwrap(),
        )
        .unwrap();
    assert_eq!(suggestion(&e, &p)["state"], "no_supplier");
    // A shipped transfer on its way in counts as stock on the way.
    let r = suggestion(&e, &no_lead);
    assert_eq!(r["facts"]["transfers_in_milli"], 0);
}

// ---------------------------------------------------------------- requisitions

#[test]
fn requisitions_move_through_their_states_and_convert_exactly_once() {
    let e = env();
    let a = supplier(&e, "Awal Dairy");
    let b = supplier(&e, "Gulf Foods");
    let p1 = e.product("Laban", "9301", 500, 300, 0);
    let p2 = e.product("Rice", "9302", 900, 600, 0);
    let p3 = e.product("Oil", "9303", 1500, 1000, 0);
    let (_, inv_t) = e.user("Store", "role_inventory", "5813");
    let req = e
        .core
        .requisition_create(
            &inv_t,
            serde_json::from_value(json!({ "operation_id": op(), "lines": [
            { "product_id": p1, "supplier_id": a, "qty_milli": 10_000, "unit_cost_minor": 300 },
            { "product_id": p2, "supplier_id": b, "qty_milli": 5_000, "unit_cost_minor": 600 },
            { "product_id": p3, "supplier_id": a, "qty_milli": 2_000, "unit_cost_minor": 1000 }] }))
            .unwrap(),
        )
        .unwrap();
    let rid = req["requisition_id"].as_str().unwrap().to_string();
    assert_eq!(req["status"], "draft");
    assert!(req["lines"].as_array().unwrap().iter().all(|l| l["source"] == "manual"));
    // Approval needs purchasing.approve; the store person cannot approve.
    e.core.requisition_set_status(&inv_t, &rid, "submit", None).unwrap();
    assert_eq!(e.core.requisition_set_status(&inv_t, &rid, "approve", None).unwrap_err().code, ErrorCode::Forbidden);
    // Converting before approval is refused.
    assert_eq!(e.core.requisition_convert(&e.owner_token, &rid, &op()).unwrap_err().code, ErrorCode::Conflict);
    e.core.requisition_set_status(&e.owner_token, &rid, "approve", None).unwrap();
    // One draft PO per supplier.
    let conv_op = op();
    let v = e.core.requisition_convert(&e.owner_token, &rid, &conv_op).unwrap();
    assert_eq!(v["status"], "converted");
    assert_eq!(v["purchase_orders"].as_array().unwrap().len(), 2);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM purchase_orders"), 2);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM purchase_order_items WHERE requisition_line_id IS NOT NULL"), 3);
    assert!(v["purchase_orders"].as_array().unwrap().iter().all(|p| p["status"] == "draft"), "orders start as drafts");
    // A retry returns the same result; a second conversion is refused.
    e.core.requisition_convert(&e.owner_token, &rid, &conv_op).unwrap();
    let err = e.core.requisition_convert(&e.owner_token, &rid, &op()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM purchase_orders"), 2);
    // Rejecting needs a reason; cancelling a converted one is refused.
    let r2 = e
        .core
        .requisition_create(
            &inv_t,
            serde_json::from_value(json!({ "operation_id": op(), "lines": [{ "product_id": p1, "supplier_id": a, "qty_milli": 1_000 }] }))
                .unwrap(),
        )
        .unwrap();
    let r2id = r2["requisition_id"].as_str().unwrap();
    e.core.requisition_set_status(&inv_t, r2id, "submit", None).unwrap();
    assert_eq!(e.core.requisition_set_status(&e.owner_token, r2id, "reject", None).unwrap_err().code, ErrorCode::Validation);
    assert_eq!(e.core.requisition_set_status(&e.owner_token, r2id, "reject", Some("Not this week".into())).unwrap()["status"], "rejected");
    assert_eq!(e.core.requisition_set_status(&e.owner_token, &rid, "cancel", None).unwrap_err().code, ErrorCode::Conflict);
    // Submitting needs a supplier on every line.
    let r3 = e
        .core
        .requisition_create(
            &inv_t,
            serde_json::from_value(json!({ "operation_id": op(), "lines": [{ "product_id": p1, "qty_milli": 1_000 }] })).unwrap(),
        )
        .unwrap();
    assert_eq!(
        e.core.requisition_set_status(&inv_t, r3["requisition_id"].as_str().unwrap(), "submit", None).unwrap_err().code,
        ErrorCode::Validation
    );
}

#[test]
fn concurrent_conversions_make_one_set_of_orders() {
    let e = std::sync::Arc::new(env());
    let a = supplier(&e, "Awal Dairy");
    let p1 = e.product("Laban", "9311", 500, 300, 0);
    let req = e
        .core
        .requisition_create(&e.owner_token, serde_json::from_value(json!({ "operation_id": op(), "lines": [{ "product_id": p1, "supplier_id": a, "qty_milli": 1_000, "unit_cost_minor": 300 }] })).unwrap())
        .unwrap();
    let rid = req["requisition_id"].as_str().unwrap().to_string();
    e.core.requisition_set_status(&e.owner_token, &rid, "submit", None).unwrap();
    e.core.requisition_set_status(&e.owner_token, &rid, "approve", None).unwrap();
    let hs: Vec<_> = (0..4)
        .map(|_| {
            let e = e.clone();
            let rid = rid.clone();
            std::thread::spawn(move || e.core.requisition_convert(&e.owner_token, &rid, &op()).is_ok())
        })
        .collect();
    let ok = hs.into_iter().map(|h| h.join().unwrap()).filter(|x| *x).count();
    assert_eq!(ok, 1);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM purchase_orders"), 1);
}

#[test]
fn a_failed_conversion_changes_nothing() {
    let e = env();
    let a = supplier(&e, "Awal Dairy");
    let b = supplier(&e, "Closed Supplier");
    let p1 = e.product("Laban", "9321", 500, 300, 0);
    let p2 = e.product("Rice", "9322", 900, 600, 0);
    let req = e
        .core
        .requisition_create(
            &e.owner_token,
            serde_json::from_value(json!({ "operation_id": op(), "lines": [
            { "product_id": p1, "supplier_id": a, "qty_milli": 1_000, "unit_cost_minor": 300 },
            { "product_id": p2, "supplier_id": b, "qty_milli": 1_000, "unit_cost_minor": 600 }] }))
            .unwrap(),
        )
        .unwrap();
    let rid = req["requisition_id"].as_str().unwrap();
    e.core.requisition_set_status(&e.owner_token, rid, "submit", None).unwrap();
    e.core.requisition_set_status(&e.owner_token, rid, "approve", None).unwrap();
    e.core
        .supplier_save(
            &e.owner_token,
            Some(b.clone()),
            serde_json::from_value(json!({ "name": "Closed Supplier", "active": false })).unwrap(),
        )
        .unwrap();
    assert_eq!(e.core.requisition_convert(&e.owner_token, rid, &op()).unwrap_err().code, ErrorCode::Validation);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM purchase_orders"), 0, "the first supplier's order was rolled back");
    assert_eq!(e.core.requisition_get(&e.owner_token, rid).unwrap()["status"], "approved");
}

// ---------------------------------------------------------------- PO approval

#[test]
fn purchase_order_approval_is_durable_and_lost_on_a_material_edit() {
    let e = env();
    let sid = supplier(&e, "Awal Dairy");
    let pid = e.product("Laban", "9401", 500, 300, 0);
    // Off (the default): ordering works as before.
    let p0 = po(&e, &sid, json!([{ "product_id": pid, "qty_milli": 100_000, "unit_cost_minor": 300 }]));
    assert_eq!(e.core.purchase_order_set_status(&e.owner_token, &p0.header.po_id, "ordered").unwrap().header.status, "ordered");
    e.core
        .settings_save(
            &e.owner_token,
            "purchasing",
            json!({ "po_approval_mode": "above_threshold", "po_approval_threshold_minor": 10_000 }),
        )
        .unwrap();
    let p = po(&e, &sid, json!([{ "product_id": pid, "qty_milli": 50_000, "unit_cost_minor": 300 }]));
    assert_eq!(p.header.approval_state.as_deref(), Some("needs_approval"));
    let err = e.core.purchase_order_set_status(&e.owner_token, &p.header.po_id, "ordered").unwrap_err();
    assert_eq!((err.code, err.details.as_ref().unwrap()["kind"].as_str()), (ErrorCode::Conflict, Some("approval_required")));
    // Only purchasing.approve can approve.
    let (_, inv_t) = e.user("Store", "role_inventory", "5813");
    assert_eq!(e.core.purchase_order_approve(&inv_t, &p.header.po_id, None, &op()).unwrap_err().code, ErrorCode::Forbidden);
    let ap_op = op();
    let a = e.core.purchase_order_approve(&e.owner_token, &p.header.po_id, Some("Weekly order".into()), &ap_op).unwrap();
    assert_eq!(a.header.approval_state.as_deref(), Some("approved"));
    assert_eq!(a.approval["history"][0]["total_minor"], a.header.total_minor);
    e.core.purchase_order_approve(&e.owner_token, &p.header.po_id, None, &ap_op).unwrap(); // retry: nothing new
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM purchase_order_approvals"), 1);
    // A note is not material: still approved.
    let edit = |notes: &str, qty: i64| {
        e.core
            .purchase_order_save(
                &e.owner_token,
                Some(p.header.po_id.clone()),
                serde_json::from_value(json!({ "supplier_id": sid, "notes": notes,
                "lines": [{ "product_id": pid, "qty_milli": qty, "unit_cost_minor": 300 }] }))
                .unwrap(),
            )
            .unwrap()
    };
    assert_eq!(edit("Call first", 50_000).header.approval_state.as_deref(), Some("approved"));
    // A quantity is: the approval no longer covers it, and stays in the history.
    let d = edit("Call first", 60_000);
    assert_eq!(d.header.approval_state.as_deref(), Some("needs_approval"));
    assert_eq!(d.approval["history"][0]["invalidated_reason"], "edited");
    assert_eq!(e.core.purchase_order_set_status(&e.owner_token, &p.header.po_id, "ordered").unwrap_err().code, ErrorCode::Conflict);
    e.core.purchase_order_approve(&e.owner_token, &p.header.po_id, None, &op()).unwrap();
    assert_eq!(e.core.purchase_order_set_status(&e.owner_token, &p.header.po_id, "ordered").unwrap().header.status, "ordered");
    // Below the threshold no approval is needed.
    let small = po(&e, &sid, json!([{ "product_id": pid, "qty_milli": 10_000, "unit_cost_minor": 300 }]));
    assert_eq!(small.header.approval_state, None);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type IN ('po.approved','po.approval_invalidated')"), 3);
}

// ---------------------------------------------------------------- receiving

#[test]
fn receiving_keeps_every_difference_and_only_accepted_goods_become_stock() {
    let e = env();
    let sid = supplier(&e, "Awal Dairy");
    let a = e.product("Laban", "9501", 500, 300, 0);
    let b = e.product("Laban 2L", "9502", 900, 550, 0);
    let c = e.product("Laban 2L (other brand)", "9503", 900, 560, 0);
    let p = po(
        &e,
        &sid,
        json!([{ "product_id": a, "qty_milli": 24_000, "unit_cost_minor": 300 }, { "product_id": b, "qty_milli": 10_000, "unit_cost_minor": 550 }]),
    );
    e.core.purchase_order_set_status(&e.owner_token, &p.header.po_id, "ordered").unwrap();
    let (ia, ib) = (p.lines[0].po_item_id.clone(), p.lines[1].po_item_id.clone());
    let waste_before = one::<i64>(&e, "SELECT COUNT(*) FROM waste_records");
    // A: 20 delivered, 2 refused damaged, 1 kept although dented → 18 accepted.
    // B: 10 of product C came instead, accepted explicitly.
    let d = receive(
        &e,
        &e.owner_token,
        &p.header.po_id,
        vec![
            PoReceiveLine {
                po_item_id: ia.clone(),
                qty_milli: 18_000,
                delivered_milli: Some(20_000),
                rejected: vec![Rejection { qty_milli: 2_000, reason: "damaged".into(), note: None }],
                damaged_kept_milli: 1_000,
                ..Default::default()
            },
            PoReceiveLine {
                po_item_id: ib.clone(),
                qty_milli: 10_000,
                substitute_product_id: Some(c.clone()),
                accept_substitution: true,
                ..Default::default()
            },
        ],
        &op(),
    )
    .unwrap();
    assert_eq!((stock(&e, &a), stock(&e, &b), stock(&e, &c)), (18_000, 0, 10_000));
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM waste_records"), waste_before, "refused goods are not waste");
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM stock_movements WHERE type='receive' AND source_type='goods_receipt'"), 2);
    let kinds: Vec<(String, i64, String)> = d
        .discrepancies
        .iter()
        .map(|x| (x["kind"].as_str().unwrap().to_string(), x["qty_milli"].as_i64().unwrap(), x["resolution"].as_str().unwrap().to_string()))
        .collect();
    assert!(kinds.contains(&("rejected".into(), 2_000, "rejected".into())), "{kinds:?}");
    assert!(kinds.contains(&("damaged".into(), 1_000, "accepted".into())));
    assert!(kinds.contains(&("shortage".into(), 6_000, "open".into())), "24 ordered − 18 accepted, still visible");
    let sub = d.discrepancies.iter().find(|x| x["kind"] == "substitution").unwrap();
    assert_eq!(
        (sub["product_id"].as_str(), sub["substitute_product_id"].as_str()),
        (Some(b.as_str()), Some(c.as_str())),
        "ordered B → received C"
    );
    let delivered: i64 = e
        .core
        .db
        .read(|x| Ok(x.query_row("SELECT qty_delivered_milli FROM goods_receipt_items WHERE product_id=?1", [&a], |r| r.get(0))?))
        .unwrap();
    assert_eq!(delivered, 20_000);
    let sub_for: String = e
        .core
        .db
        .read(|x| Ok(x.query_row("SELECT substitute_for_product_id FROM goods_receipt_items WHERE product_id=?1", [&c], |r| r.get(0))?))
        .unwrap();
    assert_eq!(sub_for, b);
    assert_eq!(d.header.status, "partially_received");
    // A substitute must be accepted explicitly.
    let p2 = po(&e, &sid, json!([{ "product_id": b, "qty_milli": 1_000, "unit_cost_minor": 550 }]));
    e.core.purchase_order_set_status(&e.owner_token, &p2.header.po_id, "ordered").unwrap();
    let err = receive(
        &e,
        &e.owner_token,
        &p2.header.po_id,
        vec![PoReceiveLine {
            po_item_id: p2.lines[0].po_item_id.clone(),
            qty_milli: 1_000,
            substitute_product_id: Some(c.clone()),
            ..Default::default()
        }],
        &op(),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Validation);
    // The shortage: cancelled explicitly; the order then closes.
    let short = d.discrepancies.iter().find(|x| x["kind"] == "shortage").unwrap()["discrepancy_id"].as_str().unwrap().to_string();
    let after = e.core.receipt_shortage_decide(&e.owner_token, &short, "cancel").unwrap();
    assert_eq!(after.header.status, "received");
    assert_eq!(after.lines[0].qty_cancelled_milli, 6_000);
    assert_eq!(after.lines[0].qty_remaining_milli, 0);
    // Every refusal at the door and nothing else: no supplier return was made.
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM supplier_returns"), 0);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM stock_movements WHERE type='supplier_return'"), 0);
}

#[test]
fn shortage_kept_on_order_stays_expected_and_a_later_delivery_settles_it() {
    let e = env();
    let sid = supplier(&e, "Awal Dairy");
    let a = e.product("Laban", "9511", 500, 300, 0);
    let p = po(&e, &sid, json!([{ "product_id": a, "qty_milli": 10_000, "unit_cost_minor": 300 }]));
    e.core.purchase_order_set_status(&e.owner_token, &p.header.po_id, "ordered").unwrap();
    let item = p.lines[0].po_item_id.clone();
    let d = e
        .core
        .purchase_order_receive(
            &e.owner_token,
            PoReceiveRequest {
                po_id: p.header.po_id.clone(),
                lines: vec![line(&item, 6_000)],
                operation_id: op(),
                shortages: vec![ShortageDecision { po_item_id: item.clone(), decision: "backorder".into() }],
                ..Default::default()
            },
        )
        .unwrap();
    assert!(d.discrepancies.iter().any(|x| x["kind"] == "shortage" && x["resolution"] == "backorder"));
    assert_eq!(d.lines[0].qty_remaining_milli, 4_000);
    let d = receive(&e, &e.owner_token, &p.header.po_id, vec![line(&item, 4_000)], &op()).unwrap();
    assert_eq!(d.header.status, "received");
    assert_eq!(stock(&e, &a), 10_000);
}

#[test]
fn over_delivery_needs_confirmation_and_an_approver_beyond_tolerance() {
    let e = env();
    let sid = supplier(&e, "Awal Dairy");
    let a = e.product("Laban", "9521", 500, 300, 0);
    let (_, inv_t) = e.user("Store", "role_inventory", "5813");
    let mk = || {
        let p = po(&e, &sid, json!([{ "product_id": a, "qty_milli": 10_000, "unit_cost_minor": 300 }]));
        e.core.purchase_order_set_status(&e.owner_token, &p.header.po_id, "ordered").unwrap();
        p
    };
    let p = mk();
    let item = p.lines[0].po_item_id.clone();
    // Typing 12 by mistake is refused until the extra is confirmed.
    assert_eq!(receive(&e, &inv_t, &p.header.po_id, vec![line(&item, 12_000)], &op()).unwrap_err().code, ErrorCode::Validation);
    // Confirmed, but beyond the (zero) tolerance: a manager must approve.
    let over = PoReceiveLine { accept_overage: true, ..line(&item, 12_000) };
    let err = receive(&e, &inv_t, &p.header.po_id, vec![over.clone()], &op()).unwrap_err();
    assert_eq!(err.code, ErrorCode::ApprovalRequired);
    assert_eq!(stock(&e, &a), 0);
    // An approver accepts it: recorded with who approved.
    let d = receive(&e, &e.owner_token, &p.header.po_id, vec![over], &op()).unwrap();
    let o = d.discrepancies.iter().find(|x| x["kind"] == "overage").unwrap();
    assert_eq!((o["qty_milli"].as_i64(), o["resolution"].as_str()), (Some(2_000), Some("accepted")));
    assert_eq!(o["approved_by"], e.owner_id);
    assert_eq!(stock(&e, &a), 12_000);
    // Within the tolerance (20%), confirmed extra needs no manager.
    e.core.settings_save(&e.owner_token, "purchasing", json!({ "qty_tolerance_bp": 2000 })).unwrap();
    let p = mk();
    let d =
        receive(&e, &inv_t, &p.header.po_id, vec![PoReceiveLine { accept_overage: true, ..line(&p.lines[0].po_item_id, 12_000) }], &op())
            .unwrap();
    assert!(d.discrepancies.iter().any(|x| x["kind"] == "overage" && x["note"] == "Within the quantity tolerance"));
}

#[test]
fn receiving_is_safe_to_retry_and_refuses_a_changed_or_partial_retry() {
    let e = env();
    let sid = supplier(&e, "Awal Dairy");
    let a = e.product("Laban", "9531", 500, 300, 0);
    let b = e.product("Rice", "9532", 900, 600, 0);
    let p = po(
        &e,
        &sid,
        json!([{ "product_id": a, "qty_milli": 10_000, "unit_cost_minor": 300 }, { "product_id": b, "qty_milli": 5_000, "unit_cost_minor": 600 }]),
    );
    e.core.purchase_order_set_status(&e.owner_token, &p.header.po_id, "ordered").unwrap();
    let (ia, ib) = (p.lines[0].po_item_id.clone(), p.lines[1].po_item_id.clone());
    let op1 = op();
    receive(&e, &e.owner_token, &p.header.po_id, vec![line(&ia, 4_000), line(&ib, 5_000)], &op1).unwrap();
    // The reply was lost (committed already): the retry changes nothing.
    receive(&e, &e.owner_token, &p.header.po_id, vec![line(&ia, 4_000), line(&ib, 5_000)], &op1).unwrap();
    assert_eq!((stock(&e, &a), stock(&e, &b)), (4_000, 5_000));
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM goods_receipts"), 1);
    // The same id with a different or partial request is refused.
    assert_eq!(
        receive(&e, &e.owner_token, &p.header.po_id, vec![line(&ia, 5_000), line(&ib, 5_000)], &op1).unwrap_err().code,
        ErrorCode::IdempotencyMismatch
    );
    assert_eq!(
        receive(&e, &e.owner_token, &p.header.po_id, vec![line(&ia, 4_000)], &op1).unwrap_err().code,
        ErrorCode::IdempotencyMismatch
    );
    assert_eq!((stock(&e, &a), stock(&e, &b)), (4_000, 5_000));
    // A failure part way (an impossible batch date on the second line)
    // writes nothing at all.
    let bad = PoReceiveLine { lot: Some(LotInput { expires_on: Some("2026-02-30".into()), ..Default::default() }), ..line(&ia, 1_000) };
    e.core.product_lot_settings(&e.owner_token, &a, true, None).unwrap();
    let receipts = one::<i64>(&e, "SELECT COUNT(*) FROM goods_receipts");
    assert!(receive(&e, &e.owner_token, &p.header.po_id, vec![bad], &op()).is_err());
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM goods_receipts"), receipts);
    assert_eq!(stock(&e, &a), 4_000);
}

#[test]
fn two_people_receiving_the_same_order_at_once_cannot_double_receive() {
    let e = std::sync::Arc::new(env());
    let sid = supplier(&e, "Awal Dairy");
    let a = e.product("Laban", "9541", 500, 300, 0);
    let p = po(&e, &sid, json!([{ "product_id": a, "qty_milli": 10_000, "unit_cost_minor": 300 }]));
    e.core.purchase_order_set_status(&e.owner_token, &p.header.po_id, "ordered").unwrap();
    let item = p.lines[0].po_item_id.clone();
    let hs: Vec<_> = (0..4)
        .map(|_| {
            let (e, po_id, item) = (e.clone(), p.header.po_id.clone(), item.clone());
            std::thread::spawn(move || receive(&e, &e.owner_token, &po_id, vec![line(&item, 10_000)], &op()).is_ok())
        })
        .collect();
    let ok = hs.into_iter().map(|h| h.join().unwrap()).filter(|x| *x).count();
    assert_eq!(ok, 1, "the others would be over-deliveries nobody confirmed");
    assert_eq!(stock(&e, &a), 10_000);
}

#[test]
fn receiving_into_batches_keeps_the_batch_truth() {
    let e = env();
    let sid = supplier(&e, "Awal Dairy");
    let a = e.product("Laban", "9551", 500, 300, 0);
    e.core.product_lot_settings(&e.owner_token, &a, true, Some("use_by".into())).unwrap();
    let p = po(&e, &sid, json!([{ "product_id": a, "qty_milli": 10_000, "unit_cost_minor": 300 }]));
    e.core.purchase_order_set_status(&e.owner_token, &p.header.po_id, "ordered").unwrap();
    let d = today(&e);
    let l = PoReceiveLine {
        lot: Some(LotInput { supplier_lot_code: Some("B-77".into()), expires_on: Some(plus_days(&d, 20)), ..Default::default() }),
        rejected: vec![Rejection { qty_milli: 1_000, reason: "short_dated".into(), note: None }],
        ..line(&p.lines[0].po_item_id, 9_000)
    };
    receive(&e, &e.owner_token, &p.header.po_id, vec![l], &op()).unwrap();
    let br = branch(&e);
    let r = e.core.db.read(|c| replay(c, &a, &br)).unwrap();
    assert_eq!(
        (r.lots.len(), r.lots[0].balance_milli, r.lots[0].facts.qty_received_milli),
        (1, 9_000, 9_000),
        "only the accepted 9 are in the batch"
    );
}

// ---------------------------------------------------------------- three-way match

fn invoice(e: &Env, sid: &str, po_id: &str, number: &str, lines: Value) -> String {
    let (mut sub, mut vat) = (0i64, 0i64);
    for l in lines.as_array().unwrap() {
        let net = l["qty_milli"].as_i64().unwrap() * l["unit_cost_minor"].as_i64().unwrap() / 1000;
        sub += net;
        vat += net * l["vat_rate_bp"].as_i64().unwrap_or(1000) / 10_000;
    }
    let v = e
        .core
        .ap_invoice_create_manual(
            &e.owner_token,
            serde_json::from_value(json!({ "supplier_id": sid, "doc_type": "invoice", "invoice_number": number, "invoice_date": today(e),
                "subtotal_minor": sub, "vat_minor": vat, "total_minor": sub + vat, "po_id": po_id,
                "lines": lines.as_array().unwrap().iter().map(|l| json!({ "product_id": l["product_id"], "description": "x", "qty_milli": l["qty_milli"],
                    "unit_cost_minor": l["unit_cost_minor"], "vat_rate_bp": l.get("vat_rate_bp").cloned().unwrap_or(json!(1000)) })).collect::<Vec<_>>() }))
            .unwrap(),
        )
        .unwrap();
    let id = v["invoice"]["invoice_id"].as_str().or(v["invoice_id"].as_str()).unwrap().to_string();
    e.core.ap_invoice_approve(&e.owner_token, &id).unwrap();
    id
}

#[test]
fn the_three_way_match_blocks_reviews_and_lets_matched_invoices_post() {
    let e = env();
    let sid = supplier(&e, "Awal Dairy");
    let a = e.product("Laban", "9601", 500, 300, 0);
    let p = po(&e, &sid, json!([{ "product_id": a, "qty_milli": 10_000, "unit_cost_minor": 1000, "tax_rate_bp": 1000 }]));
    e.core.purchase_order_set_status(&e.owner_token, &p.header.po_id, "ordered").unwrap();
    let item = p.lines[0].po_item_id.clone();
    receive(&e, &e.owner_token, &p.header.po_id, vec![line(&item, 6_000)], &op()).unwrap();
    // Invoiced 10 but only 6 accepted: blocked, cannot post, cannot be accepted.
    let i1 = invoice(&e, &sid, &p.header.po_id, "INV-1", json!([{ "product_id": a, "qty_milli": 10_000, "unit_cost_minor": 1000 }]));
    let m = e.core.supplier_invoice_match(&e.owner_token, &i1).unwrap();
    assert_eq!(m["match"]["outcome"], "blocked");
    assert!(m["match"]["lines"][0]["states"].as_array().unwrap().iter().any(|s| s == "invoice_exceeds_received"));
    let err = e.core.ap_invoice_post(&e.owner_token, &i1, &op()).unwrap_err();
    assert_eq!(err.details.as_ref().unwrap()["kind"], "match_blocked");
    assert_eq!(e.core.supplier_invoice_accept_match(&e.owner_token, &i1, "ok").unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM ap_liabilities"), 0);
    e.core.supplier_invoice_set_status(&e.owner_token, &i1, "void").unwrap();
    // 6 at 1.020: 2% and 0.120 BHD — within both tolerances: posts.
    let i2 = invoice(&e, &sid, &p.header.po_id, "INV-2", json!([{ "product_id": a, "qty_milli": 6_000, "unit_cost_minor": 1020 }]));
    let m = e.core.supplier_invoice_match(&e.owner_token, &i2).unwrap();
    assert_eq!(m["match"]["outcome"], "within_tolerance", "{m}");
    e.core.ap_invoice_post(&e.owner_token, &i2, &op()).unwrap();
    // Cumulative: the other 4 arrive and a second invoice for them matches.
    receive(&e, &e.owner_token, &p.header.po_id, vec![line(&item, 4_000)], &op()).unwrap();
    let i3 = invoice(&e, &sid, &p.header.po_id, "INV-3", json!([{ "product_id": a, "qty_milli": 4_000, "unit_cost_minor": 1000 }]));
    let m = e.core.supplier_invoice_match(&e.owner_token, &i3).unwrap();
    assert_eq!(m["match"]["outcome"], "matched", "{m}");
    assert_eq!(m["match"]["lines"][0]["invoiced_elsewhere_milli"], 6_000);
    // Invoicing them a second time is now beyond what was received.
    let i4 = invoice(&e, &sid, &p.header.po_id, "INV-4", json!([{ "product_id": a, "qty_milli": 4_000, "unit_cost_minor": 1000 }]));
    e.core.ap_invoice_post(&e.owner_token, &i3, &op()).unwrap();
    assert_eq!(e.core.supplier_invoice_match(&e.owner_token, &i4).unwrap()["match"]["outcome"], "blocked", "no duplicate liability");
}

#[test]
fn a_cost_beyond_tolerance_needs_a_person_to_accept_the_match() {
    let e = env();
    let sid = supplier(&e, "Awal Dairy");
    let a = e.product("Laban", "9611", 500, 300, 0);
    let x = e.product("Not ordered", "9612", 500, 300, 0);
    let p = po(&e, &sid, json!([{ "product_id": a, "qty_milli": 10_000, "unit_cost_minor": 1000, "tax_rate_bp": 1000 }]));
    e.core.purchase_order_set_status(&e.owner_token, &p.header.po_id, "ordered").unwrap();
    receive(&e, &e.owner_token, &p.header.po_id, vec![line(&p.lines[0].po_item_id, 10_000)], &op()).unwrap();
    // 1.200 instead of 1.000: 20% — review.
    let i = invoice(&e, &sid, &p.header.po_id, "INV-9", json!([{ "product_id": a, "qty_milli": 10_000, "unit_cost_minor": 1200 }]));
    let m = e.core.supplier_invoice_match(&e.owner_token, &i).unwrap();
    assert_eq!(m["match"]["outcome"], "review");
    assert_eq!(m["match"]["lines"][0]["value_variance_minor"], 2_000);
    assert_eq!(m["match"]["lines"][0]["last_cost_minor"], 1000, "the last confirmed cost from this supplier (the receipt)");
    assert_eq!(e.core.ap_invoice_post(&e.owner_token, &i, &op()).unwrap_err().details.unwrap()["kind"], "match_review");
    let (_, inv_t) = e.user("Store", "role_inventory", "5813");
    assert_eq!(e.core.supplier_invoice_accept_match(&inv_t, &i, "fine").unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(e.core.supplier_invoice_accept_match(&e.owner_token, &i, "  ").unwrap_err().code, ErrorCode::Validation);
    let v = e.core.supplier_invoice_accept_match(&e.owner_token, &i, "New price list from October").unwrap();
    assert_eq!(v["acceptance"]["still_applies"], true);
    e.core.ap_invoice_post(&e.owner_token, &i, &op()).unwrap();
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM ap_liabilities"), 1);
    // A line not on the order, and a different VAT rate, also need review.
    let p2 = po(&e, &sid, json!([{ "product_id": a, "qty_milli": 1_000, "unit_cost_minor": 1000, "tax_rate_bp": 1000 }]));
    e.core.purchase_order_set_status(&e.owner_token, &p2.header.po_id, "ordered").unwrap();
    receive(&e, &e.owner_token, &p2.header.po_id, vec![line(&p2.lines[0].po_item_id, 1_000)], &op()).unwrap();
    let j = invoice(
        &e,
        &sid,
        &p2.header.po_id,
        "INV-10",
        json!([{ "product_id": a, "qty_milli": 1_000, "unit_cost_minor": 1000, "vat_rate_bp": 0 },
        { "product_id": x, "qty_milli": 1_000, "unit_cost_minor": 100 }]),
    );
    let m = e.core.supplier_invoice_match(&e.owner_token, &j).unwrap();
    assert_eq!(m["match"]["outcome"], "review");
    let states: Vec<String> = m["match"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|l| l["states"].as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()))
        .collect();
    assert!(states.contains(&"vat_differs".to_string()) && states.contains(&"not_on_po".to_string()), "{states:?}");
}

// ---------------------------------------------------------------- supplier returns

#[test]
fn a_supplier_return_moves_stock_once_and_its_credit_moves_none() {
    let e = env();
    let sid = supplier(&e, "Awal Dairy");
    let a = e.product("Laban", "9701", 500, 300, 0);
    e.core.product_lot_settings(&e.owner_token, &a, true, Some("use_by".into())).unwrap();
    let p = po(&e, &sid, json!([{ "product_id": a, "qty_milli": 10_000, "unit_cost_minor": 400 }]));
    e.core.purchase_order_set_status(&e.owner_token, &p.header.po_id, "ordered").unwrap();
    let d = today(&e);
    let l = PoReceiveLine {
        lot: Some(LotInput { expires_on: Some(plus_days(&d, 30)), ..Default::default() }),
        ..line(&p.lines[0].po_item_id, 10_000)
    };
    receive(&e, &e.owner_token, &p.header.po_id, vec![l], &op()).unwrap();
    let rid: String = one(&e, "SELECT receipt_id FROM goods_receipts");
    let lot: String = one(&e, "SELECT lot_id FROM stock_lots");
    let draft = |qty: i64| {
        e.core
            .supplier_return_save(
                &e.owner_token,
                None,
                serde_json::from_value(json!({ "supplier_id": sid, "receipt_id": rid, "operation_id": op(),
                "lines": [{ "product_id": a, "lot_id": lot, "qty_milli": qty, "reason": "quality" }] }))
                .unwrap(),
            )
            .unwrap()
    };
    let r = draft(3_000);
    let ret = r["return_id"].as_str().unwrap().to_string();
    assert_eq!(r["expected_credit_minor"], 1_200, "3 × 0.400, the batch's cost");
    assert_eq!(stock(&e, &a), 10_000, "a draft moves nothing");
    let cop = op();
    e.core.supplier_return_confirm(&e.owner_token, &ret, &cop).unwrap();
    e.core.supplier_return_confirm(&e.owner_token, &ret, &cop).unwrap(); // retry
    assert_eq!(e.core.supplier_return_confirm(&e.owner_token, &ret, &op()).unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM stock_movements WHERE type='supplier_return'"), 1);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM stock_movements WHERE type='supplier_return' AND lot_id IS NOT NULL"), 1);
    assert_eq!(stock(&e, &a), 7_000);
    let br = branch(&e);
    let rp = e.core.db.read(|c| replay(c, &a, &br)).unwrap();
    assert_eq!((rp.lots[0].balance_milli, rp.lots[0].explicit_out_milli), (7_000, 3_000), "evidence from the batch");
    // More than the batch or the receipt has left is refused.
    let big = draft(8_000);
    assert_eq!(
        e.core.supplier_return_confirm(&e.owner_token, big["return_id"].as_str().unwrap(), &op()).unwrap_err().code,
        ErrorCode::Validation
    );
    e.core.supplier_return_cancel(&e.owner_token, big["return_id"].as_str().unwrap()).unwrap();
    // The credit note: drafted from the return, posted in Payables.
    let stock_before = stock(&e, &a);
    let v = e.core.supplier_return_draft_credit(&e.owner_token, &ret, "CN-55", &d).unwrap();
    let inv = v["credit_invoice_id"].as_str().unwrap().to_string();
    assert_eq!(v["status"], "confirmed");
    e.core.ap_invoice_approve(&e.owner_token, &inv).unwrap();
    e.core.ap_invoice_post(&e.owner_token, &inv, &op()).unwrap();
    let v = e.core.supplier_return_get(&e.owner_token, &ret).unwrap();
    assert_eq!(
        (v["status"].as_str(), v["actual_credit_minor"].as_i64(), v["credit_difference_minor"].as_i64()),
        (Some("credited"), Some(1_200), Some(0))
    );
    assert_eq!(stock(&e, &a), stock_before, "the credit moves no stock");
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM ap_credits"), 1);
    // A second credit note cannot be linked; a credited return is not reversed.
    assert_eq!(e.core.supplier_return_draft_credit(&e.owner_token, &ret, "CN-56", &d).unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(e.core.supplier_return_reverse(&e.owner_token, &ret, "Mistake", &op()).unwrap_err().code, ErrorCode::Conflict);
}

#[test]
fn a_confirmed_return_is_reversed_by_compensation() {
    let e = env();
    let sid = supplier(&e, "Awal Dairy");
    let a = e.product("Laban", "9711", 500, 300, 10_000);
    let r = e
        .core
        .supplier_return_save(
            &e.owner_token,
            None,
            serde_json::from_value(json!({ "supplier_id": sid, "lines": [{ "product_id": a, "qty_milli": 2_000, "reason": "damaged" }] }))
                .unwrap(),
        )
        .unwrap();
    let ret = r["return_id"].as_str().unwrap();
    e.core.supplier_return_confirm(&e.owner_token, ret, &op()).unwrap();
    assert_eq!(stock(&e, &a), 8_000);
    assert_eq!(e.core.supplier_return_cancel(&e.owner_token, ret).unwrap_err().code, ErrorCode::Conflict, "only drafts are cancelled");
    let rev = op();
    e.core.supplier_return_reverse(&e.owner_token, ret, "Supplier refused it", &rev).unwrap();
    e.core.supplier_return_reverse(&e.owner_token, ret, "Supplier refused it", &rev).unwrap();
    assert_eq!(stock(&e, &a), 10_000);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM stock_movements WHERE type='supplier_return'"), 2, "the return and its compensation");
    assert_eq!(e.core.supplier_return_get(&e.owner_token, ret).unwrap()["status"], "reversed");
    // A cashier cannot return goods.
    let (_, cash_t) = e.user("Cashier", "role_cashier", "5821");
    assert_eq!(e.core.supplier_returns_list(&cash_t, None, None).unwrap_err().code, ErrorCode::Forbidden);
}

#[test]
fn a_return_and_sales_at_the_same_moment_never_return_missing_stock() {
    let e = std::sync::Arc::new(env());
    e.open_shift(&e.owner_token, 1_000);
    let sid = supplier(&e, "Awal Dairy");
    let a = e.product("Laban", "9721", 500, 300, 5_000);
    let r = e
        .core
        .supplier_return_save(
            &e.owner_token,
            None,
            serde_json::from_value(json!({ "supplier_id": sid, "lines": [{ "product_id": a, "qty_milli": 5_000, "reason": "recalled" }] }))
                .unwrap(),
        )
        .unwrap();
    let ret = r["return_id"].as_str().unwrap().to_string();
    let seller = {
        let e = e.clone();
        std::thread::spawn(move || {
            for _ in 0..3 {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sell(&e, &e.owner_token, "9721", 1_000)));
            }
        })
    };
    let returned = e.core.supplier_return_confirm(&e.owner_token, &ret, &op()).is_ok();
    seller.join().unwrap();
    let sold: i64 = one(&e, "SELECT COALESCE(-SUM(qty_delta_milli),0) FROM stock_movements WHERE type='sale'");
    let back: i64 = if returned { 5_000 } else { 0 };
    assert_eq!(stock(&e, &a), 5_000 - sold - back, "one stock truth");
    // The return was checked against the stock at its own moment: it never
    // took stock that was not there (tills may still sell below zero).
    if returned {
        let after: i64 = one(&e, "SELECT balance_after_milli FROM stock_movements WHERE type='supplier_return'");
        assert!(after >= 0, "returned only what was on hand: {after}");
    } else {
        assert!(sold > 0, "refused only because a sale came first");
    }
}

// ---------------------------------------------------------------- upgrade + permissions

#[test]
fn upgrading_invents_no_terms_approvals_or_differences() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("amwapos.db");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        amwapos_core::db::migrate_until(&c, &path, 29).unwrap();
        c.execute_batch(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO suppliers(supplier_id, name, active, created_at, updated_at) VALUES ('s1','Old supplier',1,'x','x');
             INSERT INTO products(product_id, sku, name, tax_rule_id, created_at, updated_at) VALUES ('p1','S1','Milk','t','x','x'), ('p2','S2','Rice','t','x','x');
             INSERT INTO supplier_product_map(supplier_id, key_kind, key_norm, product_id, units_per_case, uses, confirmed_at) VALUES
               ('s1','code','A1','p1',12,3,'x'), ('s1','desc','milk 1l','p1',12,1,'x'), ('s1','code','B2','p2',6,1,'x'), ('s1','desc','rice','p2',10,1,'x');
             INSERT INTO purchase_orders(po_id, po_number, supplier_id, branch_id, status, created_by, created_at, updated_at, total_minor) VALUES
               ('po1','PO-00001','s1','br','draft','u','x','x',999999), ('po2','PO-00002','s1','br','partially_received','u','x','x',1000);
             INSERT INTO purchase_order_items(po_item_id, po_id, line_no, product_id, qty_ordered_milli, qty_received_milli, unit_cost_minor, total_minor) VALUES
               ('i1','po1',1,'p1',1000,0,300,300), ('i2','po2',1,'p2',2000,1000,500,1000);
             INSERT INTO goods_receipts(receipt_id, po_id, supplier_id, branch_id, total_cost_minor, operation_id, user_id, device_id, created_at) VALUES
               ('g1','po2','s1','br',500,'op-old','u','d','x');
             INSERT INTO goods_receipt_items(receipt_item_id, receipt_id, po_item_id, product_id, qty_milli, unit_cost_minor) VALUES ('gi1','g1','i2','p2',1000,500);",
        )
        .unwrap();
    }
    let core =
        amwapos_core::service::AppCore::open(dir.path(), std::sync::Arc::new(amwapos_core::service::MemorySecretStore::default())).unwrap();
    let n = |sql: &str| core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get::<_, i64>(0))?)).unwrap();
    assert_eq!(n("SELECT COUNT(*) FROM supplier_products"), 2, "who supplied what: a fact");
    assert_eq!(
        n("SELECT units_per_case FROM supplier_products WHERE product_id='p1'"),
        12,
        "one confirmed pack size: kept as document evidence"
    );
    assert_eq!(
        n("SELECT COUNT(*) FROM supplier_products WHERE product_id='p2' AND units_per_case IS NULL"),
        1,
        "two different ones: not guessed"
    );
    assert_eq!(n("SELECT COUNT(*) FROM supplier_products WHERE moq_packs IS NOT NULL OR lead_time_days IS NOT NULL OR preferred=1"), 0);
    assert_eq!(
        n("SELECT COUNT(*) FROM purchase_order_approvals")
            + n("SELECT COUNT(*) FROM requisitions")
            + n("SELECT COUNT(*) FROM receipt_discrepancies")
            + n("SELECT COUNT(*) FROM supplier_returns"),
        0
    );
    assert_eq!(n("SELECT COUNT(*) FROM purchase_orders WHERE status='draft'"), 1, "the draft stays a draft, not approved");
    assert_eq!(n("SELECT qty_cancelled_milli FROM purchase_order_items WHERE po_item_id='i2'"), 0);
    assert_eq!(n("SELECT COUNT(*) FROM goods_receipt_items WHERE qty_delivered_milli IS NULL"), 1, "old receipts keep only what they knew");
    assert_eq!(n("SELECT COUNT(*) FROM supplier_product_map"), 4, "the document evidence is untouched");
}

#[test]
fn new_permissions_are_narrow_and_an_owner_removal_sticks() {
    let e = env();
    let perms = |role: &str| -> Vec<String> {
        e.core
            .db
            .read(|c| {
                let mut st = c.prepare("SELECT permission_code FROM role_permissions WHERE role_id=?1")?;
                let v = st.query_map([role], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
                Ok(v)
            })
            .unwrap()
    };
    let inv = perms("role_inventory");
    assert!(inv.contains(&"requisitions.create".to_string()) && inv.contains(&"supplier_returns.manage".to_string()));
    assert!(!inv.contains(&"purchasing.approve".to_string()), "the store person does not approve");
    assert!(perms("role_manager").contains(&"purchasing.approve".to_string()));
    assert!(!perms("role_cashier")
        .iter()
        .any(|p| p.starts_with("requisitions") || p.starts_with("purchasing") || p.starts_with("supplier_returns")));
    // The owner removes one; an upgrade does not add it back.
    e.core
        .db
        .write(|tx| {
            Ok(tx.execute("DELETE FROM role_permissions WHERE role_id='role_inventory' AND permission_code='requisitions.create'", [])?)
        })
        .unwrap();
    e.core.db.write(|tx| amwapos_core::auth::seed_roles(tx)).unwrap();
    assert!(!perms("role_inventory").contains(&"requisitions.create".to_string()));
}
