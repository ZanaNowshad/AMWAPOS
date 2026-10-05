//! Wave 3 of the merchant operating system: batches and expiry, waste and
//! days of stock left. Stock stays one truth (the movements); every test
//! checks that batches plus stock in no batch add up to the stock on hand.

mod common;

use amwapos_core::inventory::{ReceiveLine, ReceiveRequest};
use amwapos_core::lots::{replay, LotInput};
use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::{FinalizeRequest, SaleResult};
use amwapos_core::waste::WasteRequest;
use amwapos_core::ErrorCode;
use common::*;
use serde_json::{json, Value};

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

fn lot(exp: &str) -> LotInput {
    LotInput { expires_on: Some(exp.into()), confirm_warnings: true, ..Default::default() }
}

fn receive(e: &Env, pid: &str, qty: i64, cost: i64, l: Option<LotInput>, op_id: &str) -> Value {
    e.core
        .inventory_receive(
            &e.owner_token,
            ReceiveRequest {
                supplier_id: None,
                reference: Some("INV-1".into()),
                lines: vec![ReceiveLine { product_id: pid.into(), qty_milli: qty, unit_cost_minor: cost, po_item_id: None, lot: l }],
                operation_id: op_id.into(),
            },
        )
        .unwrap()
}

fn one<T: rusqlite::types::FromSql>(e: &Env, sql: &str) -> T {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn branch(e: &Env) -> String {
    one(e, "SELECT branch_id FROM branches LIMIT 1")
}

/// Batches by number: (balance, estimated sold, recorded out), and stock in no batch.
fn state(e: &Env, pid: &str) -> (Vec<(String, i64, i64, i64)>, i64) {
    let b = branch(e);
    let r = e.core.db.read(|c| replay(c, pid, &b)).unwrap();
    let total: i64 = r.lots.iter().map(|l| l.balance_milli).sum::<i64>() + r.unlotted_milli;
    assert_eq!(total, r.stock_milli, "batches + stock in no batch = stock on hand");
    assert!(r.lots.iter().all(|l| l.balance_milli >= 0));
    (
        r.lots.iter().map(|l| (l.facts.lot_number.clone(), l.balance_milli, l.estimated_out_milli, l.explicit_out_milli)).collect(),
        r.unlotted_milli,
    )
}

fn today(e: &Env) -> String {
    e.core.db.read(|c| amwapos_core::time::business_date(amwapos_core::time::now(), &amwapos_core::time::day(c)?)).unwrap()
}

fn plus_days(d: &str, n: i64) -> String {
    (chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap() + chrono::Duration::days(n)).to_string()
}

fn waste(pid: &str, lot_id: Option<&str>, qty: i64, reason: &str, op_id: &str) -> WasteRequest {
    WasteRequest {
        product_id: pid.into(),
        lot_id: lot_id.map(String::from),
        qty_milli: qty,
        reason: reason.into(),
        note: None,
        operation_id: op_id.into(),
        approval_token: None,
    }
}

#[test]
fn receiving_makes_one_batch_and_older_stock_stays_outside_batches() {
    let e = env();
    let pid = e.product("Laban 1L", "8001", 500, 300, 10_000); // 10 from before batches
    let d = today(&e);
    let op_id = op();
    let r1 = receive(&e, &pid, 5_000, 320, Some(lot(&plus_days(&d, 20))), &op_id);
    // Retry / double click: same receipt, one batch, stock moved once.
    let r2 = receive(&e, &pid, 5_000, 320, Some(lot(&plus_days(&d, 20))), &op_id);
    assert_eq!(r1["receipt_id"], r2["receipt_id"]);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM stock_lots"), 1);
    let (lots, unlotted) = state(&e, &pid);
    assert_eq!((lots[0].1, unlotted), (5_000, 10_000), "the old 10 are in no batch; nothing was invented");
    let facts: (i64, String, Option<String>) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT qty_received_milli, provenance, receipt_id FROM stock_lots", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?)
        })
        .unwrap();
    assert_eq!((facts.0, facts.1.as_str()), (5_000, "receiving"));
    assert_eq!(facts.2.as_deref(), r1["receipt_id"].as_str());
    // The movement carries the batch; there is no separate quantity to edit.
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM stock_movements WHERE lot_id IS NOT NULL"), 1);
    assert!(e.core.db.write(|tx| Ok(tx.execute("UPDATE stock_lots SET qty_received_milli=1", [])?)).is_err());
}

#[test]
fn sales_use_older_stock_then_the_first_expiring_batch() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 1_000);
    let pid = e.product("Yoghurt", "8002", 400, 250, 2_000); // 2 in no batch
    let d = today(&e);
    receive(&e, &pid, 10_000, 250, Some(lot(&plus_days(&d, 40))), &op()); // L-00001, later expiry
    receive(&e, &pid, 10_000, 250, Some(lot(&plus_days(&d, 10))), &op()); // L-00002, first to expire
    receive(&e, &pid, 10_000, 250, Some(LotInput { supplier_lot_code: Some("NODATE".into()), ..Default::default() }), &op()); // no date: last
    sell(&e, t, "8002", 5_000);
    let (lots, unlotted) = state(&e, &pid);
    assert_eq!(unlotted, 0, "the 2 older units went first");
    let by = |n: &str| lots.iter().find(|l| l.0 == n).unwrap().clone();
    assert_eq!(by("L-00002").1, 7_000, "then 3 from the batch expiring first");
    assert_eq!(by("L-00002").2, 3_000, "shown as estimated, not observed");
    assert_eq!((by("L-00001").1, by("L-00003").1), (10_000, 10_000));
    // Order on screen: first expiring first, undated last.
    assert_eq!(lots.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), vec!["L-00002", "L-00001", "L-00003"]);
    sell(&e, t, "8002", 12_000);
    let (lots, _) = state(&e, &pid);
    assert_eq!(lots.iter().map(|l| l.1).collect::<Vec<_>>(), vec![0, 5_000, 10_000]);
}

#[test]
fn equal_expiry_is_broken_by_received_order_and_a_sale_never_fails() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 1_000);
    let pid = e.product("Cheese", "8003", 900, 600, 0);
    let d = today(&e);
    let exp = plus_days(&d, 15);
    receive(&e, &pid, 3_000, 600, Some(lot(&exp)), &op());
    receive(&e, &pid, 3_000, 600, Some(lot(&exp)), &op());
    sell(&e, t, "8003", 4_000);
    let (lots, _) = state(&e, &pid);
    assert_eq!(lots.iter().map(|l| (l.0.as_str(), l.1)).collect::<Vec<_>>(), vec![("L-00001", 0), ("L-00002", 2_000)]);
    // Selling more than the batches hold still sells (batches never block a sale).
    sell(&e, t, "8003", 3_000);
    let (lots, unlotted) = state(&e, &pid);
    assert_eq!((lots[1].1, unlotted), (0, -1_000));
    // A new batch first settles what was sold beyond stock.
    receive(&e, &pid, 5_000, 600, Some(lot(&plus_days(&d, 30))), &op());
    let (lots, unlotted) = state(&e, &pid);
    assert_eq!((lots[2].1, lots[2].2, unlotted), (4_000, 1_000, 0));
}

#[test]
fn recorded_evidence_beats_the_estimate() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 1_000);
    let pid = e.product("Bread", "8004", 300, 150, 0);
    let d = today(&e);
    receive(&e, &pid, 10_000, 150, Some(lot(&plus_days(&d, 3))), &op()); // A, first to expire
    receive(&e, &pid, 10_000, 150, Some(lot(&plus_days(&d, 6))), &op()); // B
    sell(&e, t, "8004", 5_000); // estimated from A
    let a = one::<String>(&e, "SELECT lot_id FROM stock_lots WHERE lot_number='L-00001'");
    // A person throws away 8 from A: A still had them, so the estimate moves to B.
    e.core.waste_record(t, waste(&pid, Some(&a), 8_000, "spoiled", &op())).unwrap();
    let (lots, unlotted) = state(&e, &pid);
    assert_eq!(unlotted, 0);
    assert_eq!((lots[0].1, lots[0].2, lots[0].3), (0, 2_000, 8_000), "A: 8 thrown away (recorded), 2 sold (estimated)");
    assert_eq!((lots[1].1, lots[1].2), (7_000, 3_000), "B: 3 of the sale now estimated from B");
    // Evidence can never take more than the batch held.
    let err = e.core.waste_record(t, waste(&pid, Some(&a), 3_000, "spoiled", &op())).unwrap_err();
    assert_eq!(err.code, ErrorCode::Validation);
}

#[test]
fn waste_moves_stock_once_and_a_reversal_compensates() {
    let e = env();
    let t = &e.owner_token;
    let pid = e.product("Eggs 30", "8005", 2_000, 1_400, 20_000);
    let op_id = op();
    let w = e.core.waste_record(t, waste(&pid, None, 2_000, "broken", &op_id)).unwrap();
    let again = e.core.waste_record(t, waste(&pid, None, 2_000, "broken", &op_id)).unwrap();
    assert_eq!(w.waste_id, again.waste_id);
    assert_eq!((w.cost_minor, w.status.as_str()), (Some(2_800), "recorded"));
    let stock = |e: &Env| one::<i64>(e, &format!("SELECT qty_milli FROM stock_levels WHERE product_id='{pid}'"));
    assert_eq!(stock(&e), 18_000);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM stock_movements WHERE type='waste'"), 1);
    // Reverse: a new movement brings it back; the record stays, marked reversed.
    let rop = op();
    let r = e.core.waste_reverse(t, &w.waste_id, "Counted again: not broken", &rop).unwrap();
    e.core.waste_reverse(t, &w.waste_id, "Counted again: not broken", &rop).unwrap();
    assert_eq!(r.status, "reversed");
    assert_eq!(stock(&e), 20_000);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM stock_movements WHERE type='waste'"), 2);
    assert_eq!(e.core.waste_reverse(t, &w.waste_id, "again", &op()).unwrap_err().code, ErrorCode::Conflict);
    assert!(e.core.db.write(|tx| Ok(tx.execute("DELETE FROM waste_records", [])?)).is_err());
    assert!(e.core.db.write(|tx| Ok(tx.execute("UPDATE waste_records SET qty_milli=1", [])?)).is_err());
    // Reversed waste is not counted.
    let sum = e.core.waste_summary(t, None, None).unwrap();
    assert_eq!(sum["cost_minor"], 0);
}

#[test]
fn expired_stock_stays_until_someone_records_what_happened() {
    let e = env();
    let t = &e.owner_token;
    let pid = e.product("Juice", "8006", 600, 350, 0);
    let d = today(&e);
    // A date before today is refused unless the person confirms it.
    let err = e
        .core
        .inventory_receive(
            t,
            ReceiveRequest {
                supplier_id: None,
                reference: None,
                lines: vec![ReceiveLine {
                    product_id: pid.clone(),
                    qty_milli: 4_000,
                    unit_cost_minor: 350,
                    po_item_id: None,
                    lot: Some(LotInput { expires_on: Some(plus_days(&d, -2)), ..Default::default() }),
                }],
                operation_id: op(),
            },
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Validation);
    assert!(err.details.unwrap()["warnings"][0].as_str().unwrap().contains("arrived expired"));
    receive(&e, &pid, 4_000, 350, Some(lot(&plus_days(&d, -2))), &op());
    let ov = e.core.expiry_overview(t, Default::default()).unwrap();
    let row = &ov["rows"][0];
    assert_eq!((row["status"].as_str(), row["balance_milli"].as_i64()), (Some("expired"), Some(4_000)));
    assert_eq!(ov["expired_on_hand_milli"], 4_000);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM waste_records"), 0, "expiry is a condition, not a disposal");
    assert_eq!(one::<i64>(&e, &format!("SELECT qty_milli FROM stock_levels WHERE product_id='{pid}'")), 4_000);
    // Impossible dates are refused outright.
    let bad = LotInput { expires_on: Some("2026-02-30".into()), confirm_warnings: true, ..Default::default() };
    let err = e
        .core
        .inventory_receive(
            t,
            ReceiveRequest {
                supplier_id: None,
                reference: None,
                lines: vec![ReceiveLine {
                    product_id: pid.clone(),
                    qty_milli: 1_000,
                    unit_cost_minor: 350,
                    po_item_id: None,
                    lot: Some(bad),
                }],
                operation_id: op(),
            },
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Validation);
}

#[test]
fn large_waste_and_shrinkage_need_a_manager_and_cashiers_cannot_waste() {
    let e = env();
    let pid = e.product("Saffron", "8007", 15_000, 12_000, 10_000);
    let (_i, it) = e.user("Inventory clerk", amwapos_core::auth::ROLE_INVENTORY, "5281");
    let (_c, ct) = e.user("Cashier W", amwapos_core::auth::ROLE_CASHIER, "4682");
    assert_eq!(e.core.waste_record(&ct, waste(&pid, None, 1_000, "damaged", &op())).unwrap_err().code, ErrorCode::Forbidden);
    // Small waste: no friction.
    e.core.waste_record(&it, waste(&pid, None, 1_000, "damaged", &op())).unwrap();
    // 2 × 12.000 = 24.000 > 20.000: a manager approves this exact request.
    let big = || waste(&pid, None, 2_000, "damaged", &op());
    let asked = e.core.waste_record(&it, big()).unwrap_err();
    assert_eq!(asked.code, ErrorCode::ApprovalRequired);
    let binding = asked.details.unwrap()["binding"].as_str().unwrap().to_string();
    let tok = e.core.approve(&it, &e.owner_id, OWNER_PIN, "waste.approve", "x", Some(&binding)).unwrap()["approval_token"]
        .as_str()
        .unwrap()
        .to_string();
    let mut req = big();
    req.approval_token = Some(tok);
    let w = e.core.waste_record(&it, req).unwrap();
    assert_eq!(w.approved_by_name.as_deref(), Some("Owner"));
    // Shrinkage (unexplained) always asks, whatever the amount.
    assert_eq!(e.core.waste_record(&it, waste(&pid, None, 1, "shrinkage", &op())).unwrap_err().code, ErrorCode::ApprovalRequired);
    // Reversing needs waste.approve.
    assert_eq!(e.core.waste_reverse(&it, &w.waste_id, "x", &op()).unwrap_err().code, ErrorCode::Forbidden);
}

#[test]
fn old_stock_can_be_counted_into_a_batch_and_batch_details_corrected() {
    let e = env();
    let t = &e.owner_token;
    let pid = e.product("Rice 5kg", "8008", 3_500, 2_400, 10_000);
    let d = today(&e);
    let op_id = op();
    let req = |q: i64, o: &str| {
        serde_json::from_value::<amwapos_core::lots::CountInRequest>(json!({
            "product_id": pid, "qty_milli": q, "operation_id": o,
            "lot": { "expires_on": plus_days(&d, 60), "supplier_lot_code": "R-77" }
        }))
        .unwrap()
    };
    let a = e.core.lot_count_in(t, req(4_000, &op_id)).unwrap();
    assert_eq!(e.core.lot_count_in(t, req(4_000, &op_id)).unwrap(), a, "retry counts once");
    assert_eq!(e.core.lot_count_in(t, req(7_000, &op())).unwrap_err().code, ErrorCode::Validation, "only 6 are in no batch");
    let (lots, unlotted) = state(&e, &pid);
    assert_eq!((lots[0].1, unlotted), (4_000, 6_000));
    assert_eq!(one::<i64>(&e, &format!("SELECT qty_milli FROM stock_levels WHERE product_id='{pid}'")), 10_000, "stock unchanged");
    // A correction is recorded beside the original, which stays.
    let lid = a["lot_id"].as_str().unwrap().to_string();
    let fixed = e
        .core
        .lot_correct(
            t,
            serde_json::from_value(
                json!({ "lot_id": lid, "expires_on": plus_days(&d, 45), "reason": "Misread the pack", "operation_id": op() }),
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(fixed["expires_on"], plus_days(&d, 45));
    assert_eq!(fixed["corrected"], true);
    assert_eq!(one::<String>(&e, "SELECT expires_on FROM stock_lots"), plus_days(&d, 60));
    assert_eq!(fixed["corrections"].as_array().unwrap().len(), 1);
}

#[test]
fn a_date_read_from_a_document_needs_a_person() {
    let e = env();
    let t = &e.owner_token;
    let pid = e.product("Dates", "8009", 2_000, 1_200, 0);
    let sid = e.core.supplier_save(t, None, serde_json::from_value(json!({ "name": "Gulf Foods" })).unwrap()).unwrap().supplier_id;
    let did = amwapos_core::ids::new_id();
    let d = today(&e);
    e.core
        .db
        .write(|tx| {
            tx.execute(
                "INSERT INTO receiving_drafts(draft_id, number, supplier_id, status, created_by, created_at, updated_at) VALUES (?1,'RD-T',?2,'draft','x','x','x')",
                rusqlite::params![did, sid],
            )?;
            tx.execute(
                "INSERT INTO receiving_draft_lines(draft_id, line_no, product_id, description, qty_milli, unit_cost_minor, expires_on, expiry_source, expiry_confirmed)
                 VALUES (?1,1,?2,'Dates 1kg EXP',5000,1200,?3,'document',0)",
                rusqlite::params![did, pid, plus_days(&d, 90)],
            )?;
            Ok(())
        })
        .unwrap();
    let err = e.core.receiving_draft_post(t, &did, &op()).unwrap_err();
    assert!(err.message.contains("Confirm it or clear it"), "{}", err.message);
    let set = |confirm: bool| {
        e.core.receiving_draft_set_lot(
            t,
            serde_json::from_value(
                json!({ "draft_id": did, "line_no": 1, "expires_on": plus_days(&d, 90), "confirm_document_date": confirm }),
            )
            .unwrap(),
        )
    };
    let v = set(true).unwrap();
    assert_eq!((v["lines"][0]["expiry_source"].as_str(), v["lines"][0]["expiry_confirmed"].as_bool()), (Some("document"), Some(true)));
    e.core.receiving_draft_post(t, &did, &op()).unwrap();
    assert_eq!(one::<String>(&e, "SELECT expiry_source FROM stock_lots"), "document");
    assert_eq!(one::<String>(&e, "SELECT expires_on FROM stock_lots"), plus_days(&d, 90));
}

/// A sale as it would have been written on `date` (business date and time).
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

#[test]
fn days_of_stock_left_say_why_when_they_cannot_be_counted() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 1_000);
    let pid = e.product("Water 1.5L", "8010", 150, 90, 100_000);
    e.product("Brand new", "8011", 150, 90, 5_000);
    e.product("Nothing left", "8012", 150, 90, 0);
    let d = today(&e);
    // 2 a day for the last 30 days (sales written on those days).
    let base = sell(&e, t, "8010", 2_000);
    for i in 1..=30 {
        backdate(&e, &base.sale_id, &plus_days(&d, -i));
    }
    let cover = e.core.stock_cover(t, serde_json::from_value(json!({ "window_days": 30 })).unwrap()).unwrap();
    let get = |name: &str| cover["rows"].as_array().unwrap().iter().find(|r| r["name"] == name).unwrap().clone();
    let w = get("Water 1.5L");
    assert_eq!((w["state"].as_str(), w["per_day_milli"].as_i64()), (Some("ok"), Some(2_000)));
    // 98 on hand (100 − today's 2) ÷ 2 a day = 49 days.
    assert_eq!(w["cover_tenths"], 490);
    assert_eq!(w["stockout_on"], plus_days(&d, 49));
    assert_eq!(get("Brand new")["state"], "not_enough_history");
    assert_eq!(get("Nothing left")["state"], "no_stock");
    // Refunds and voids lower demand like in the sales reports: refund 2 of
    // an old sale today; over a range that includes today, 62 sold − 2 back.
    refund_two(&e);
    let br = branch(&e);
    let net = e.core.db.read(|c| amwapos_core::lots::net_units_sold(c, &pid, &br, &plus_days(&d, -30), &d)).unwrap();
    assert_eq!(net, 60_000);
    // Old sales only (nothing in the window): no demand, not "infinity".
    let old = e.product("Slow seller", "8013", 150, 90, 5_000);
    let s = sell(&e, t, "8013", 1_000);
    backdate(&e, &s.sale_id, &plus_days(&d, -45));
    let cover = e.core.stock_cover(t, serde_json::from_value(json!({ "window_days": 30, "product_id": old })).unwrap()).unwrap();
    // Today's sale is outside the window (it ends yesterday); the old one is before it.
    assert_eq!(cover["rows"][0]["state"], "no_demand");
    // Another branch's sales are not this branch's demand.
    e.core.settings_save(t, "features", json!({ "org.multi_branch": true })).unwrap();
    let rows = e.core.branch_save(t, None, serde_json::from_value(json!({ "code": "B2", "name": "Riffa" })).unwrap()).unwrap();
    let b2 = rows.iter().find(|b| b.code == "B2").unwrap().branch_id.clone();
    let other = e.core.stock_cover(t, serde_json::from_value(json!({ "window_days": 30, "branch_id": b2 })).unwrap()).unwrap();
    let w2 = other["rows"].as_array().unwrap().iter().find(|r| r["name"] == "Water 1.5L").unwrap().clone();
    assert_eq!((w2["net_sold_milli"].as_i64(), w2["state"].as_str()), (Some(0), Some("no_stock")));
}

/// Refund 2 of one of the backdated sales (today).
fn refund_two(e: &Env) {
    let (sid, item): (String, String) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row(
                "SELECT s.sale_id, i.sale_item_id FROM sales s JOIN sale_items i ON i.sale_id=s.sale_id WHERE s.receipt_number LIKE 'B-%' LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    e.core
        .refund_create(
            &e.owner_token,
            serde_json::from_value(json!({ "sale_id": sid, "reason": "Leaking", "operation_id": op(),
                "lines": [{ "sale_item_id": item, "qty_milli": 2_000, "restock": false }] }))
            .unwrap(),
        )
        .unwrap();
}

#[test]
fn upgrading_keeps_stock_and_invents_no_batches_or_waste() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("amwapos.db");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        amwapos_core::db::migrate_until(&c, &path, 28).unwrap();
        c.execute_batch(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO products(product_id, sku, name, tax_rule_id, created_at, updated_at) VALUES ('p1','S1','Old milk','t','x','x');
             INSERT INTO stock_levels(product_id, branch_id, qty_milli, updated_at) VALUES ('p1','br',7000,'x');
             INSERT INTO stock_movements(movement_id, product_id, branch_id, type, qty_delta_milli, balance_after_milli, source_type, created_at)
               VALUES ('m1','p1','br','receive',10000,10000,'goods_receipt','2026-01-01T00:00:00Z'),
                      ('m2','p1','br','adjust',-3000,7000,'adjustment','2026-02-01T00:00:00Z');",
        )
        .unwrap();
    }
    let core =
        amwapos_core::service::AppCore::open(dir.path(), std::sync::Arc::new(amwapos_core::service::MemorySecretStore::default())).unwrap();
    let n = |sql: &str| core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get::<_, i64>(0))?)).unwrap();
    assert_eq!(n("SELECT qty_milli FROM stock_levels WHERE product_id='p1'"), 7_000);
    assert_eq!(n("SELECT COUNT(*) FROM stock_lots") + n("SELECT COUNT(*) FROM waste_records"), 0);
    assert_eq!(n("SELECT COUNT(*) FROM stock_movements WHERE type='waste' OR lot_id IS NOT NULL"), 0, "old adjustments stay adjustments");
    assert_eq!(n("SELECT track_lots FROM products"), 0);
    let r = core.db.read(|c| replay(c, "p1", "br")).unwrap();
    assert_eq!((r.unlotted_milli, r.lots.len()), (7_000, 0), "existing stock shows as in no batch");
}

#[test]
fn the_replay_does_not_depend_on_arrival_order() {
    // The same movements written in two different orders give the same batches.
    let mk = || env();
    let (a, b) = (mk(), mk());
    let mut results = vec![];
    for (e, reverse) in [(&a, false), (&b, true)] {
        let pid = "PFIX";
        let br = branch(e);
        let tax = e.tax_rule();
        e.core
            .db
            .write(|tx| {
                tx.execute(
                    "INSERT INTO products(product_id, sku, name, tax_rule_id, created_at, updated_at) VALUES ('PFIX','PFIX','Fixed',?1,'x','x')",
                    [&tax],
                )?;
                tx.execute(
                    "INSERT INTO stock_lots(lot_id, lot_number, product_id, branch_id, received_at, expires_on, qty_received_milli, unit_cost_minor, provenance, created_at)
                     VALUES ('LA','L-A','PFIX',?1,'2026-09-01T00:00:00Z','2026-12-01',10000,100,'receiving','x'),
                            ('LB','L-B','PFIX',?1,'2026-09-02T00:00:00Z','2026-11-01',10000,100,'receiving','x')",
                    [&br],
                )?;
                let mut rows = vec![
                    ("m1", 10_000, Some("LA"), "2026-09-01T00:00:00.000Z"),
                    ("m2", 10_000, Some("LB"), "2026-09-02T00:00:00.000Z"),
                    ("m3", -4_000, None, "2026-09-03T00:00:00.000Z"), // till sale, offline
                    ("m4", -3_000, Some("LB"), "2026-09-04T00:00:00.000Z"), // hub waste from B
                    ("m5", -6_000, None, "2026-09-05T00:00:00.000Z"),
                ];
                if reverse {
                    rows.reverse();
                }
                for (id, q, lot, at) in rows {
                    tx.execute(
                        "INSERT INTO stock_movements(movement_id, product_id, branch_id, type, qty_delta_milli, balance_after_milli, source_type, created_at, lot_id)
                         VALUES (?1,'PFIX',?2,'sale',?3,0,'x',?4,?5)",
                        rusqlite::params![id, br, q, at, lot],
                    )?;
                }
                tx.execute("INSERT INTO stock_levels(product_id, branch_id, qty_milli, updated_at) VALUES ('PFIX',?1,7000,'x')", [&br])?;
                Ok(())
            })
            .unwrap();
        results.push(state(e, pid));
        let _ = pid;
    }
    assert_eq!(results[0], results[1]);
    // B expires first: the till sale of 4 is estimated from B (6 left), the
    // hub's recorded waste of 3 takes B to 3, the last sale of 6 empties B
    // and takes 3 from A. Stock 7 = A 7 + B 0.
    assert_eq!(
        results[0].0.iter().map(|l| (l.0.as_str(), l.1, l.2, l.3)).collect::<Vec<_>>(),
        vec![("L-B", 0, 7_000, 3_000), ("L-A", 7_000, 3_000, 0)]
    );
    assert_eq!(results[0].0.iter().map(|l| l.1).sum::<i64>(), 7_000);
}
