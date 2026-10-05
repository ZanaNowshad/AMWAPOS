//! Wave 1 of the merchant operating system: trading day, receipt snapshots,
//! bound approvals, sale void, expenses and petty cash, operating profit and
//! customer statements. Every test checks an invariant, not only a reply.

mod common;

use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::FinalizeRequest;
use amwapos_core::time;
use amwapos_core::ErrorCode;
use common::*;
use serde_json::json;

fn sell(e: &Env, barcode: &str, qty: i64, tender: &str) -> amwapos_core::sales::SaleResult {
    let t = &e.owner_token;
    let cart = e.core.pos_scan(t, barcode, Some(qty)).unwrap().cart;
    let total = cart.totals.total_minor;
    e.core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![TenderInput { method: tender.into(), amount_minor: total, reference: None }],
                approval_token: None,
                expected_total_minor: Some(total),
                fulfilment: None,
            },
        )
        .unwrap()
}

fn one<T: rusqlite::types::FromSql>(e: &Env, sql: &str, p: &str) -> T {
    e.core.db.read(|c| Ok(c.query_row(sql, [p], |r| r.get(0))?)).unwrap()
}

#[test]
fn trading_day_cutoff_decides_the_business_date_of_new_records() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    e.product("Laban 1L", "7001", 450, 300, 50_000);

    // A cutoff after 06:00 is refused; the default stays midnight.
    let mut shift = e.core.settings_get(t, "shift").unwrap();
    shift["day_cutoff_minutes"] = json!(420);
    let err = e.core.settings_save(t, "shift", shift.clone()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Validation);

    // With the latest cutoff (06:00), the sale's stored date is exactly what
    // the one trading-day rule gives for its own timestamp.
    shift["day_cutoff_minutes"] = json!(360);
    e.core.settings_save(t, "shift", shift).unwrap();
    let sale = sell(&e, "7001", 1000, "cash");
    let (stored, at): (String, String) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT business_date, completed_at FROM sales WHERE sale_id=?1", [&sale.sale_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?)
        })
        .unwrap();
    let day = time::Day { tz: "Asia/Bahrain".into(), cutoff_minutes: 360 };
    assert_eq!(stored, time::business_date(time::parse(&at).unwrap(), &day).unwrap());
    // …and the report range for that date contains the sale.
    let (a, b) = time::local_date_range_utc(&stored, &stored, &day).unwrap();
    assert!(a <= at && at < b, "{a} <= {at} < {b}");
    // The setting reaches the stored trading-day helper.
    let read = e.core.db.read(time::day).unwrap();
    assert_eq!(read.cutoff_minutes, 360);
    let _: String = one(&e, "SELECT sale_id FROM sales WHERE sale_id=?1", &sale.sale_id);
}

#[test]
fn every_table_has_a_replication_decision() {
    let e = env();
    let names: Vec<String> = e
        .core
        .db
        .read(|c| {
            let mut st = c.prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '%_fts_%' AND name NOT LIKE 'products_fts%' OR name IN ('products_fts','products_fts_map')",
            )?;
            let v = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            Ok(v)
        })
        .unwrap();
    let mut undecided = vec![];
    for n in &names {
        let replicated = amwapos_core::sync::policy(n).is_some();
        let local = amwapos_core::sync::LOCAL_TABLES.contains(&n.as_str());
        assert!(!(replicated && local), "{n} is listed as both replicated and local");
        if !replicated && !local {
            undecided.push(n.clone());
        }
    }
    assert!(undecided.is_empty(), "tables without a replication decision: {undecided:?}");
}

#[test]
fn issued_receipts_never_change_when_settings_change() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    e.product("Dates 500g", "7101", 1_250, 800, 50_000);
    let mut rcfg = e.core.settings_get(t, "receipt").unwrap();
    rcfg["footer_lines"] = json!(["Thank you — first footer"]);
    e.core.settings_save(t, "receipt", rcfg.clone()).unwrap();
    let sale = sell(&e, "7101", 2000, "cash");

    let before = e.core.db.read(|c| amwapos_core::receipt::issued(c, "sale", &sale.sale_id)).unwrap();
    assert!(before.exact, "a new sale has its receipt frozen at commit");
    assert!(before.doc.to_text().contains("first footer"));

    // The owner later changes the footer and the business name.
    rcfg["footer_lines"] = json!(["A different footer"]);
    e.core.settings_save(t, "receipt", rcfg).unwrap();
    e.core.db.write(|tx| Ok(tx.execute("UPDATE business SET name='Renamed Store'", [])?)).unwrap();

    let after = e.core.db.read(|c| amwapos_core::receipt::issued(c, "sale", &sale.sale_id)).unwrap();
    assert_eq!(after.doc, before.doc, "the issued receipt is unchanged");
    assert_eq!(after.sha256, before.sha256);
    assert!(!after.doc.to_text().contains("Renamed Store"));

    // A reprint says COPY, outside the fingerprinted body.
    let copy = e.core.db.read(|c| amwapos_core::receipt::sale_receipt(c, &sale.sale_id, Some("COPY"))).unwrap();
    assert!(copy.to_text().contains("*** COPY ***"));
    assert_eq!(copy.blocks.len(), before.doc.blocks.len() + 1);

    // The stored receipt cannot be edited or deleted.
    let edit = e.core.db.write(|tx| Ok(tx.execute("UPDATE receipt_snapshots SET doc_json='{}'", [])?));
    assert!(edit.is_err());
    let del = e.core.db.write(|tx| Ok(tx.execute("DELETE FROM receipt_snapshots", [])?));
    assert!(del.is_err());
}

#[test]
fn records_before_snapshots_reprint_as_reconstructed() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    e.product("Tea 100", "7102", 900, 500, 50_000);
    let sale = sell(&e, "7102", 1000, "cash");
    // Simulate a sale made before migration 27: no snapshot row.
    e.core
        .db
        .write(|tx| {
            tx.execute_batch("DROP TRIGGER trg_receipt_snapshots_no_delete")?;
            Ok(tx.execute("DELETE FROM receipt_snapshots", [])?)
        })
        .unwrap();
    let r = e.core.db.read(|c| amwapos_core::receipt::issued(c, "sale", &sale.sale_id)).unwrap();
    assert!(!r.exact);
    assert!(r.doc.to_text().contains("Tea 100"));
}

fn sell_as(e: &Env, t: &str, barcode: &str, qty: i64, tender: &str) -> amwapos_core::sales::SaleResult {
    let cart = e.core.pos_scan(t, barcode, Some(qty)).unwrap().cart;
    let total = cart.totals.total_minor;
    e.core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![TenderInput { method: tender.into(), amount_minor: total, reference: None }],
                approval_token: None,
                expected_total_minor: Some(total),
                fulfilment: None,
            },
        )
        .unwrap()
}

fn void_req(sale_id: &str, token: Option<String>) -> amwapos_core::voids::VoidRequest {
    amwapos_core::voids::VoidRequest { sale_id: sale_id.into(), reason: "Rang up twice".into(), operation_id: op(), approval_token: token }
}

fn qty(e: &Env, barcode: &str) -> i64 {
    e.core
        .db
        .read(|c| {
            Ok(c.query_row(
                "SELECT COALESCE(SUM(m.qty_delta_milli),0) FROM stock_movements m JOIN product_barcodes b ON b.product_id=m.product_id WHERE b.barcode=?1",
                [barcode],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

#[test]
fn a_void_reverses_the_whole_sale_once_and_leaves_it_untouched() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    e.product("Rice 5kg", "7201", 3_500, 2_400, 20_000);
    let before_stock = qty(&e, "7201");
    let expected_before = e.core.shift_current(t).unwrap().unwrap().expected_cash_minor;
    let sale = sell(&e, "7201", 2000, "cash");
    assert_eq!(qty(&e, "7201"), before_stock - 2000);

    let check = e.core.sale_void_check(t, &sale.sale_id).unwrap();
    assert!(check.allowed && !check.requires_approval);
    let req = void_req(&sale.sale_id, None);
    let v = e.core.sale_void(t, req.clone()).unwrap();
    assert_eq!(v.reversal.total_minor, sale.total_minor);
    assert!(v.reversal.refund_receipt_number.contains("-V"));

    // Stock back with `void` movements; the drawer expects what it did before.
    assert_eq!(qty(&e, "7201"), before_stock);
    let n: i64 = one(&e, "SELECT COUNT(*) FROM stock_movements WHERE type='void' AND source_id=?1", &v.reversal.refund_id);
    assert_eq!(n, 1);
    assert_eq!(e.core.shift_current(t).unwrap().unwrap().expected_cash_minor, expected_before);

    // The original sale is unchanged; the void and its reversal are recorded.
    let status: String = one(&e, "SELECT status FROM sales WHERE sale_id=?1", &sale.sale_id);
    assert_eq!(status, "completed");
    let kind: String = one(&e, "SELECT kind FROM refunds WHERE refund_id=?1", &v.reversal.refund_id);
    assert_eq!(kind, "void");

    // Retrying the same request returns the same void (lost reply); a new
    // request is refused (already voided). Nothing is reversed twice.
    let again = e.core.sale_void(t, req).unwrap();
    assert!(again.reversal.replayed);
    assert_eq!(again.void_id, v.void_id);
    let err = e.core.sale_void(t, void_req(&sale.sale_id, None)).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(qty(&e, "7201"), before_stock);

    // The void's receipt says so.
    let doc = e.core.db.read(|c| amwapos_core::receipt::refund_receipt(c, &v.reversal.refund_id, None)).unwrap();
    assert!(doc.to_text().contains("SALE VOIDED"));
}

#[test]
fn void_rules_send_other_cases_to_a_refund() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    e.product("Milk", "7202", 600, 400, 50_000);
    // Partly refunded → refund the rest instead.
    let sale = sell(&e, "7202", 2000, "cash");
    let d = e.core.refund_lookup(t, &sale.receipt_number).unwrap();
    e.core
        .refund_create(
            t,
            amwapos_core::refunds::RefundRequest {
                sale_id: sale.sale_id.clone(),
                lines: vec![amwapos_core::refunds::RefundLineInput {
                    sale_item_id: d.items[0].sale_item_id.clone(),
                    qty_milli: 1000,
                    restock: true,
                }],
                reason: "Damaged".into(),
                tenders: vec![],
                operation_id: op(),
                approval_token: None,
            },
        )
        .unwrap();
    let c = e.core.sale_void_check(t, &sale.sale_id).unwrap();
    assert!(!c.allowed);
    assert!(c.reason.unwrap().contains("refund"));
    assert_eq!(e.core.sale_void(t, void_req(&sale.sale_id, None)).unwrap_err().code, ErrorCode::Conflict);

    // Shift closed → refund instead.
    let sale2 = sell(&e, "7202", 1000, "cash");
    let sh = e.core.shift_current(t).unwrap().unwrap();
    e.core
        .shift_close(
            t,
            &sh.shift_id,
            serde_json::from_value(json!({ "counted_cash_minor": sh.expected_cash_minor, "operation_id": op() })).unwrap(),
        )
        .unwrap();
    e.open_shift(t, 10_000);
    let c = e.core.sale_void_check(t, &sale2.sale_id).unwrap();
    assert!(!c.allowed);
    assert!(c.reason.unwrap().contains("shift"));
}

#[test]
fn a_cashier_void_needs_a_manager_approval_for_that_sale_only() {
    let e = env();
    e.product("Bread", "7203", 300, 150, 50_000);
    let (_c, ct) = e.user("Cashier V", amwapos_core::auth::ROLE_CASHIER, "4682");
    e.open_shift(&ct, 10_000);
    let a = sell_as(&e, &ct, "7203", 1000, "cash");
    let b = sell_as(&e, &ct, "7203", 1000, "cash");

    let asked = e.core.sale_void(&ct, void_req(&a.sale_id, None)).unwrap_err();
    assert_eq!(asked.code, ErrorCode::ApprovalRequired);
    let binding = asked.details.as_ref().unwrap()["binding"].as_str().unwrap().to_string();
    assert!(asked.details.as_ref().unwrap()["summary"].as_str().unwrap().contains(&a.receipt_number));
    let appr = e.core.approve(&ct, &e.owner_id, OWNER_PIN, "pos.void_sale", "ignored", Some(&binding)).unwrap();
    let tok = appr["approval_token"].as_str().unwrap().to_string();

    // The approval for sale A does not void sale B.
    assert_eq!(e.core.sale_void(&ct, void_req(&b.sale_id, Some(tok.clone()))).unwrap_err().code, ErrorCode::ApprovalRequired);
    // It voids A, records the approver, and only once.
    let v = e.core.sale_void(&ct, void_req(&a.sale_id, Some(tok.clone()))).unwrap();
    let approver: String = one(&e, "SELECT approved_by FROM sale_voids WHERE void_id=?1", &v.void_id);
    assert_eq!(approver, e.owner_id);
    let n: i64 = one(&e, "SELECT COUNT(*) FROM sale_voids WHERE sale_id=?1", &b.sale_id);
    assert_eq!(n, 0);
}

#[test]
fn voiding_an_account_sale_returns_the_customer_balance() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    e.product("Oil", "7204", 2_000, 1_500, 50_000);
    e.core.settings_save(t, "features", json!({ "customer_credit": true })).unwrap();
    let mut pay = e.core.settings_get(t, "payments").unwrap();
    for x in pay["tenders"].as_array_mut().unwrap() {
        if x["method"] == "account" {
            x["enabled"] = json!(true);
        }
    }
    e.core.settings_save(t, "payments", pay).unwrap();
    let cu = e.core.customer_save(t, None, serde_json::from_value(json!({ "name": "Hassan", "phone": "36000001" })).unwrap()).unwrap();
    e.core.customer_account_set(t, &cu.customer_id, true, 50_000).unwrap();
    e.core.pos_scan(t, "7204", Some(1000)).unwrap();
    let cart = e.core.pos_set_customer(t, Some(cu.customer_id.clone())).unwrap();
    let total = cart.totals.total_minor;
    let sale = e
        .core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![TenderInput { method: "account".into(), amount_minor: total, reference: None }],
                approval_token: None,
                expected_total_minor: Some(total),
                fulfilment: None,
            },
        )
        .unwrap();
    let bal =
        |e: &Env| -> i64 { one(e, "SELECT COALESCE(SUM(amount_minor),0) FROM customer_ledger WHERE customer_id=?1", &cu.customer_id) };
    assert_eq!(bal(&e), total);
    e.core.sale_void(t, void_req(&sale.sale_id, None)).unwrap();
    assert_eq!(bal(&e), 0, "the account charge is reversed, the ledger is not edited");
}
