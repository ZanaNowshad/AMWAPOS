mod common;

use amwapos_core::importer::ImportRequest;
use amwapos_core::pricing::TenderInput;
use amwapos_core::reports::ReportParams;
use amwapos_core::sales::FinalizeRequest;
use amwapos_core::ErrorCode;
use common::*;
use serde_json::json;

fn count(e: &Env, sql: &str) -> i64 {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

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
            },
        )
        .unwrap()
}

#[test]
fn purchase_order_partial_receiving() {
    let e = env();
    let t = &e.owner_token;
    let pid = e.product("Sugar 2kg", "5001", 1_200, 800, 0);
    let sup = e.core.supplier_save(t, None, serde_json::from_value(json!({ "name": "Gulf Foods", "phone": "17000000" })).unwrap()).unwrap();
    let po = e
        .core
        .purchase_order_save(
            t,
            None,
            serde_json::from_value(json!({
                "supplier_id": sup.supplier_id, "reference": "Q-77",
                "lines": [{ "product_id": pid, "qty_milli": 24000, "unit_cost_minor": 850 }]
            }))
            .unwrap(),
        )
        .unwrap();
    assert_eq!(po.header.status, "draft");
    assert_eq!(po.header.total_minor, 20_400);
    // Cannot receive a draft.
    let recv = |qty: i64| amwapos_core::purchasing::PoReceiveRequest {
        po_id: po.header.po_id.clone(),
        reference: Some("INV-9".into()),
        lines: vec![amwapos_core::purchasing::PoReceiveLine {
            po_item_id: po.lines[0].po_item_id.clone(),
            qty_milli: qty,
            unit_cost_minor: None,
        }],
        operation_id: op(),
    };
    assert_eq!(e.core.purchase_order_receive(t, recv(1000)).unwrap_err().code, ErrorCode::Conflict);
    e.core.purchase_order_set_status(t, &po.header.po_id, "ordered").unwrap();
    let d = e.core.purchase_order_receive(t, recv(10_000)).unwrap();
    assert_eq!(d.header.status, "partially_received");
    assert_eq!(d.lines[0].qty_remaining_milli, 14_000);
    // Over-receipt is refused.
    assert_eq!(e.core.purchase_order_receive(t, recv(15_000)).unwrap_err().code, ErrorCode::Validation);
    let d = e.core.purchase_order_receive(t, recv(14_000)).unwrap();
    assert_eq!(d.header.status, "received");
    assert_eq!(d.receipts.len(), 2);
    assert_eq!(count(&e, "SELECT qty_milli FROM stock_levels"), 24_000);
    let p = e.core.product_get(t, &pid).unwrap();
    assert_eq!(p.avg_cost_minor, Some(850));
}

#[test]
fn customers_and_deliveries() {
    let e = env();
    let t = &e.owner_token;
    e.product("Water 12pk", "6001", 1_500, 900, 100_000);
    let cust = e
        .core
        .customer_save(
            t,
            None,
            serde_json::from_value(json!({ "name": "Fatima Ali", "phone": "3312 3456", "area": "Juffair", "address": "Bldg 12, Road 40" }))
                .unwrap(),
        )
        .unwrap();
    assert_eq!(cust.info.phone.as_deref(), Some("+97333123456"));
    let dup = e.core.customer_save(t, None, serde_json::from_value(json!({ "name": "Someone", "phone": "+973 33123456" })).unwrap());
    assert_eq!(dup.unwrap_err().code, ErrorCode::Duplicate);
    assert_eq!(e.core.customers_search(t, Some("3312".into()), false, None).unwrap().len(), 1);
    assert_eq!(e.core.customers_search(t, Some("fati".into()), false, None).unwrap().len(), 1);
    e.core.pos_scan(t, "6001", None).unwrap();
    e.core.pos_set_customer(t, Some(cust.customer_id.clone())).unwrap();
    e.open_shift(t, 0);
    let cart = e.core.pos_get_cart(t).unwrap();
    let sale = e
        .core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![TenderInput { method: "cash".into(), amount_minor: 1_500, reference: None }],
                approval_token: None,
                expected_total_minor: None,
            },
        )
        .unwrap();
    let d = e.core.delivery_create(t, serde_json::from_value(json!({ "sale_id": sale.sale_id })).unwrap()).unwrap();
    assert_eq!(d.customer_name.as_deref(), Some("Fatima Ali"));
    assert_eq!(d.payment_status, "paid");
    assert_eq!(d.amount_minor, 1_500);
    assert_eq!(
        e.core.delivery_update(t, &d.delivery_id, Some("delivered".into()), None, None, None).unwrap_err().code,
        ErrorCode::Conflict
    );
    e.core.delivery_update(t, &d.delivery_id, Some("dispatched".into()), None, None, None).unwrap();
    let d2 = e.core.delivery_update(t, &d.delivery_id, Some("delivered".into()), None, None, Some("Left with guard".into())).unwrap();
    assert!(d2.delivered_at.is_some());
    let detail = e.core.customer_get(t, &cust.customer_id).unwrap();
    assert_eq!(detail["customer"]["purchase_count"], 1);
    assert_eq!(detail["deliveries"].as_array().unwrap().len(), 1);
}

#[test]
fn reports_reconcile_with_sales_and_refunds() {
    let e = env();
    let t = &e.owner_token;
    e.product("A", "7001", 1_100, 500, 100_000); // 10% incl -> tax 100 per unit
    e.product("B", "7002", 2_200, 1_000, 100_000);
    e.open_shift(t, 0);
    let s1 = sell(&e, "7001", 2_000, "cash"); // 2.200
    sell(&e, "7002", 1_000, "card"); // 2.200
    let s3 = sell(&e, "7001", 1_000, "benefitpay"); // 1.100
    let _ = s1;
    // Refund s3 fully.
    let d = e.core.sale_get(t, &s3.sale_id).unwrap();
    e.core
        .refund_create(
            t,
            serde_json::from_value(json!({
                "sale_id": s3.sale_id, "reason": "Returned", "operation_id": op(),
                "lines": [{ "sale_item_id": d.items[0].sale_item_id, "qty_milli": 1000 }]
            }))
            .unwrap(),
        )
        .unwrap();
    let rep = e.core.report_run(t, "sales", ReportParams::default()).unwrap();
    let k = |label: &str| rep.kpis.iter().find(|k| k.label == label).unwrap().value;
    assert_eq!(k("Net sales"), 5_500 - 1_100);
    assert_eq!(k("Transactions"), 3);
    assert_eq!(k("VAT (net of refunds)"), 400);
    assert_eq!(k("Gross profit"), (5_500 - 500 - 2_500) - (1_100 - 100 - 500));
    let tax = e.core.report_run(t, "tax", ReportParams::default()).unwrap();
    assert_eq!(tax.kpis[1].value, 400);
    assert_eq!(tax.kpis[0].value + tax.kpis[1].value, tax.kpis[2].value);
    let pay = e.core.report_run(t, "payments", ReportParams::default()).unwrap();
    let net: i64 = pay.rows.iter().map(|r| r["net"].as_i64().unwrap()).sum();
    assert_eq!(net, 4_400);
    let margin = e.core.report_run(t, "margin", ReportParams::default()).unwrap();
    let a = margin.rows.iter().find(|r| r["name"] == "A").unwrap();
    assert_eq!(a["qty"], 2_000);
    assert_eq!(a["total"], 2_200);
    let csv = e.core.report_csv(t, "products", ReportParams::default()).unwrap();
    assert!(csv.lines().next().unwrap().contains("Product"));
    assert!(csv.contains("2.200"));
    let dash = e.core.dashboard(t).unwrap();
    assert_eq!(dash["kpis"]["sales"], 4_400);
    assert_eq!(dash["kpis"]["transactions"], 3);
    let cash = e.core.report_run(t, "cash", ReportParams::default()).unwrap();
    assert_eq!(cash.rows[0]["expected"], 2_200, "the benefitpay refund goes back to benefitpay, not the cash drawer");
}

#[test]
fn csv_import_preview_and_apply() {
    let e = env();
    let t = &e.owner_token;
    e.product("Existing", "8000000000001", 500, 200, 0);
    let csv = "Item Code,Product Name,Barcode,Price,Cost,Category,Stock\n\
               A1,Coke 330,06291100001234|6291100001235,0.250,0.150,Drinks,48\n\
               A2,Pepsi 330,6291100002222,0.240,0.140,Drinks,24\n\
               A3,Bad price,6291100003333,abc,,Drinks,\n\
               A4,Dup barcode,6291100002222,1.000,,Snacks,\n\
               A5,Sci,6.2911E+12,1.000,,Snacks,\n\
               A6,Clash,8000000000001,1.000,,Snacks,\n";
    let req = ImportRequest { csv: csv.into(), mapping: None, update_existing: false, skip_errors: false, operation_id: Some(op()) };
    let p = e.core.products_import_preview(t, req.clone()).unwrap();
    assert_eq!(p.mapping.get("sku").unwrap(), "Item Code");
    assert_eq!(p.mapping.get("barcodes").unwrap(), "Barcode");
    assert_eq!(p.total_rows, 6);
    assert_eq!(p.creates, 1, "{:#?}", p.rows);
    assert_eq!(p.errors, 5);
    assert!(p.spreadsheet_warning.is_some());
    assert_eq!(p.new_categories, vec!["Drinks".to_string(), "Snacks".to_string()]);
    // Apply refuses while errors exist.
    assert_eq!(e.core.products_import_apply(t, req.clone()).unwrap_err().code, ErrorCode::Validation);
    let mut skip = req;
    skip.skip_errors = true;
    let r = e.core.products_import_apply(t, skip.clone()).unwrap();
    assert_eq!(r["created"], 1);
    assert_eq!(r["skipped"], 5);
    let again = e.core.products_import_apply(t, skip).unwrap();
    assert_eq!(again, r, "replay returns the same result");
    // Leading zeros preserved.
    assert_eq!(e.core.pos_scan(t, "06291100001234", None).unwrap().outcome, "added");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM product_barcodes WHERE barcode='06291100001234'"), 1);
    assert_eq!(count(&e, "SELECT qty_milli FROM stock_levels s JOIN products p ON p.product_id=s.product_id WHERE p.sku='A1'"), 48_000);
}

#[test]
fn backup_and_restore_roundtrip() {
    let e = env();
    let t = &e.owner_token;
    e.product("Before", "9001", 100, 50, 10_000);
    e.open_shift(t, 0);
    sell(&e, "9001", 1_000, "cash");
    let b = e.core.backup_create(t, None).unwrap();
    assert_eq!(b.status, "completed");
    let insp = e.core.backup_inspect(t, &b.path).unwrap();
    assert!(insp.ok, "{:?}", insp.problems);
    assert_eq!(insp.checksum_matches, Some(true));
    assert_eq!(insp.record_counts["sales"], 1);
    // Changes after the backup...
    sell(&e, "9001", 1_000, "cash");
    e.product("After", "9002", 100, 50, 0);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM sales"), 2);
    let res = e.core.backup_restore(t, &b.path, false).unwrap();
    assert_eq!(res["counts_verified"], true);
    assert!(std::path::Path::new(res["safety_backup"].as_str().unwrap()).exists());
    // ...are gone, backup content is back, sessions were cleared.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM sales"), 1);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM products WHERE name='After'"), 0);
    assert_eq!(e.core.session(t).unwrap_err().code, ErrorCode::Unauthenticated);
    let t2 = e.core.login(&e.owner_id, OWNER_PIN).unwrap().token;
    assert!(e.core.audit_verify(&t2).unwrap().valid);
    // Tampered backup is rejected.
    let mut bytes = std::fs::read(&b.path).unwrap();
    let n = bytes.len();
    bytes[n / 2] ^= 0xFF;
    let bad = e.dir.path().join("tampered.amwbak");
    std::fs::write(&bad, &bytes).unwrap();
    std::fs::copy(format!("{}.json", b.path.trim_end_matches(".amwbak").to_string() + ".amwbak"), bad.with_extension("amwbak.json")).ok();
    let insp = e.core.backup_inspect(&t2, bad.to_str().unwrap()).unwrap();
    assert!(!insp.ok);
}

#[test]
fn missing_database_is_not_silently_recreated() {
    let e = env();
    let dir = e.dir.path().to_path_buf();
    drop(e.core);
    std::fs::remove_file(dir.join("amwapos.db")).unwrap();
    let _ = std::fs::remove_file(dir.join("amwapos.db-wal"));
    let _ = std::fs::remove_file(dir.join("amwapos.db-shm"));
    let err = amwapos_core::AppCore::open(&dir, std::sync::Arc::new(amwapos_core::service::MemorySecretStore::default())).err().unwrap();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(!dir.join("amwapos.db").exists());
}

#[test]
fn stocktake_blind_hides_expected() {
    let e = env();
    let t = &e.owner_token;
    let pid = e.product("Nuts", "9101", 900, 500, 5_000);
    let st = e
        .core
        .stocktake_create(t, serde_json::from_value(json!({ "name": "Blind", "scope_type": "all", "blind": true })).unwrap())
        .unwrap();
    assert!(st.lines.iter().all(|l| l.expected_qty_milli.is_none()));
    let _ = pid;
    e.core.stocktake_set_status(t, &st.header.stocktake_id, "review").unwrap();
    let st = e.core.stocktake_get(t, &st.header.stocktake_id).unwrap();
    assert_eq!(st.lines[0].expected_qty_milli, Some(5_000));
}
