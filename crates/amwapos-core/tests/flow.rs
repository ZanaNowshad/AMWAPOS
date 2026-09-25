mod common;

use amwapos_core::auth::ROLE_CASHIER;
use amwapos_core::pricing::TenderInput;
use amwapos_core::refunds::{RefundLineInput, RefundRequest};
use amwapos_core::sales::FinalizeRequest;
use amwapos_core::shifts::{CashEventRequest, ShiftCloseRequest};
use amwapos_core::ErrorCode;
use common::*;

fn cash(a: i64) -> TenderInput {
    TenderInput { method: "cash".into(), amount_minor: a, reference: None }
}
fn card(a: i64) -> TenderInput {
    TenderInput { method: "card".into(), amount_minor: a, reference: Some("4421".into()) }
}

fn count(e: &Env, sql: &str) -> i64 {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

#[test]
fn complete_sale_with_change_and_stock() {
    let e = env();
    let t = &e.owner_token;
    e.product("Coca-Cola 330ml", "06291100001234", 250, 150, 50_000);
    e.product("Milk 1L", "6291100005555", 850, 600, 10_000);
    e.open_shift(t, 20_000);
    let r = e.core.pos_scan(t, "06291100001234\r\n", None).unwrap();
    assert_eq!(r.outcome, "added");
    e.core.pos_scan(t, "06291100001234", None).unwrap();
    e.core.pos_scan(t, "06291100001234", None).unwrap();
    let cart = e.core.pos_scan(t, "6291100005555", None).unwrap().cart;
    assert_eq!(cart.lines.len(), 2, "repeated scans merge into one line");
    assert_eq!(cart.lines[0].qty_milli, 3000);
    assert_eq!(cart.totals.total_minor, 1600);
    assert_eq!(cart.totals.tax_minor, 68 + 77);
    let sale = e
        .core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.clone().unwrap(),
                operation_id: op(),
                tenders: vec![cash(5000)],
                approval_token: None,
                expected_total_minor: Some(1600),
            },
        )
        .unwrap();
    assert_eq!(sale.total_minor, 1600);
    assert_eq!(sale.change_minor, 3400);
    assert!(sale.receipt_number.starts_with("T01-"));
    assert_eq!(sale.print.as_ref().unwrap().status, "disabled");
    // Stock ledger
    assert_eq!(
        count(
            &e,
            "SELECT qty_milli FROM stock_levels s JOIN product_barcodes b ON b.product_id=s.product_id WHERE b.barcode='06291100001234'"
        ),
        47_000
    );
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_movements WHERE type='sale'"), 2);
    // Cart is empty afterwards
    assert!(e.core.pos_get_cart(t).unwrap().lines.is_empty());
    // Receipt renders from the committed sale
    let prev = e.core.receipt_preview(t, "sale", &sale.sale_id).unwrap();
    let text = prev["text"].as_str().unwrap();
    assert!(text.contains("Coca-Cola 330ml"));
    assert!(text.contains("BHD 1.600"));
    assert!(text.contains("Change"));
    assert!(text.contains("VAT No: 200000000000003"));
}

#[test]
fn finalize_is_exactly_once() {
    let e = env();
    let t = &e.owner_token;
    e.product("Water", "111", 100, 50, 10_000);
    e.open_shift(t, 0);
    let cart = e.core.pos_scan(t, "111", None).unwrap().cart;
    let req = FinalizeRequest {
        cart_id: cart.cart_id.clone().unwrap(),
        operation_id: op(),
        tenders: vec![cash(100)],
        approval_token: None,
        expected_total_minor: None,
    };
    let a = e.core.pos_finalize(t, req.clone()).unwrap();
    let b = e.core.pos_finalize(t, req.clone()).unwrap();
    assert!(!a.replayed);
    assert!(b.replayed);
    assert_eq!(a.sale_id, b.sale_id);
    assert_eq!(a.receipt_number, b.receipt_number);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM sales"), 1);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM payments"), 1);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_movements WHERE type='sale'"), 1);
    // Same operation id, different payload -> integrity error, nothing changes.
    let mut bad = req.clone();
    bad.tenders = vec![card(100)];
    let err = e.core.pos_finalize(t, bad).unwrap_err();
    assert_eq!(err.code, ErrorCode::IdempotencyMismatch);
    // New operation id on a completed cart is refused (no double charge).
    let mut again = req;
    again.operation_id = op();
    let err = e.core.pos_finalize(t, again).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM sales"), 1);
}

#[test]
fn split_tender_and_validation() {
    let e = env();
    let t = &e.owner_token;
    e.product("Rice 5kg", "222", 18_450, 12_000, 10_000);
    e.open_shift(t, 0);
    let cart = e.core.pos_scan(t, "222", None).unwrap().cart;
    let cid = cart.cart_id.unwrap();
    let over = e.core.pos_finalize(
        t,
        FinalizeRequest {
            cart_id: cid.clone(),
            operation_id: op(),
            tenders: vec![card(20_000)],
            approval_token: None,
            expected_total_minor: None,
        },
    );
    assert_eq!(over.unwrap_err().code, ErrorCode::Validation, "card cannot be over-tendered");
    let short = e.core.pos_finalize(
        t,
        FinalizeRequest {
            cart_id: cid.clone(),
            operation_id: op(),
            tenders: vec![cash(10_000)],
            approval_token: None,
            expected_total_minor: None,
        },
    );
    assert_eq!(short.unwrap_err().code, ErrorCode::Validation);
    let stale = e.core.pos_finalize(
        t,
        FinalizeRequest {
            cart_id: cid.clone(),
            operation_id: op(),
            tenders: vec![cash(20_000)],
            approval_token: None,
            expected_total_minor: Some(1),
        },
    );
    assert_eq!(stale.unwrap_err().code, ErrorCode::Conflict);
    let s = e
        .core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cid,
                operation_id: op(),
                tenders: vec![cash(10_000), card(8_450)],
                approval_token: None,
                expected_total_minor: None,
            },
        )
        .unwrap();
    assert_eq!(s.change_minor, 0);
    assert_eq!(s.payments.len(), 2);
    assert_eq!(count(&e, "SELECT SUM(amount_minor) FROM payments"), 18_450);
}

#[test]
fn unknown_barcode_is_recorded_not_guessed() {
    let e = env();
    let t = &e.owner_token;
    e.product("Bread", "0001", 300, 100, 10_000);
    let r = e.core.pos_scan(t, "001", None).unwrap();
    assert_eq!(r.outcome, "unknown", "no fuzzy match on leading zeros");
    e.core.pos_scan(t, "001", None).unwrap();
    let list = e.core.unknown_barcodes_list(t, None).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].barcode, "001");
    assert_eq!(list[0].scan_count, 2);
    // Assigning the barcode resolves it.
    let pid = e.core.products_search(t, Default::default()).unwrap().rows[0].product_id.clone();
    e.core.barcode_add(t, &pid, "001", false).unwrap();
    assert!(e.core.unknown_barcodes_list(t, None).unwrap().is_empty());
    assert_eq!(e.core.pos_scan(t, "001", None).unwrap().outcome, "added");
    // A barcode cannot belong to two products.
    let other = e.product("Cake", "999", 500, 200, 0);
    let err = e.core.barcode_add(t, &other, "0001", false).unwrap_err();
    assert_eq!(err.code, ErrorCode::Duplicate);
    assert!(err.message.contains("Bread"));
}

#[test]
fn cashier_permissions_and_manager_approval() {
    let e = env();
    let _t = &e.owner_token;
    e.product("Chocolate", "333", 1_000, 400, 10_000);
    let (_cid, ct) = e.user("Cashier One", ROLE_CASHIER, "2580");
    // Cashier cannot manage catalogue or see costs.
    let err = e.core.products_search(&ct, Default::default()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);
    e.open_shift(&ct, 5_000);
    let cart = e.core.pos_scan(&ct, "333", None).unwrap().cart;
    let line = cart.lines[0].line_id.clone();
    // 5% is within the cashier limit (10%).
    e.core.pos_line_discount(&ct, &line, 0, 500, None).unwrap();
    // 20% requires approval.
    let err = e.core.pos_line_discount(&ct, &line, 0, 2000, None).unwrap_err();
    assert_eq!(err.code, ErrorCode::ApprovalRequired);
    // Wrong PIN approval fails; correct owner PIN issues a token.
    assert!(e.core.approve(&ct, &e.owner_id, "0000", "pos.discount_override", "20%").is_err());
    let appr = e.core.approve(&ct, &e.owner_id, OWNER_PIN, "pos.discount_override", "20%").unwrap();
    let tok = appr["approval_token"].as_str().unwrap().to_string();
    let cart = e.core.pos_line_discount(&ct, &line, 0, 2000, Some(tok.clone())).unwrap();
    assert_eq!(cart.totals.total_minor, 800);
    // Token is single use.
    let err = e.core.pos_line_discount(&ct, &line, 0, 3000, Some(tok)).unwrap_err();
    assert_eq!(err.code, ErrorCode::ApprovalRequired);
    // Removing a line needs no approval for the cashier role (has pos.remove_line).
    e.core.pos_remove_line(&ct, &line, None).unwrap();
    // Approver recorded in audit.
    assert!(count(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type='pos.line_discount' AND approved_by IS NOT NULL") >= 1);
}

#[test]
fn negative_stock_blocked_unless_approved() {
    let e = env();
    let (_cid, ct) = e.user("Cashier Two", ROLE_CASHIER, "3690");
    e.product("Eggs", "444", 1_200, 800, 1_000);
    e.product("Milk", "445", 500, 300, 0);
    e.open_shift(&ct, 0);
    // Default: allowed, with a warning on the result.
    let mut pos: amwapos_core::settings::PosSettings = e.core.db.read(|c| amwapos_core::settings::get(c, "pos")).unwrap();
    assert!(pos.allow_negative_stock);
    let c0 = e.core.pos_scan(&ct, "445", None).unwrap().cart;
    let warned = e
        .core
        .pos_finalize(
            &ct,
            FinalizeRequest {
                cart_id: c0.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![cash(500)],
                approval_token: None,
                expected_total_minor: None,
            },
        )
        .unwrap();
    assert_eq!(warned.stock_warnings.len(), 1, "{:?}", warned.stock_warnings);
    // Store setting off: a manager approves each shortfall.
    pos.allow_negative_stock = false;
    e.core.settings_save(&e.owner_token, "pos", serde_json::to_value(&pos).unwrap()).unwrap();
    e.core.pos_scan(&ct, "444", None).unwrap();
    let cart = e.core.pos_scan(&ct, "444", None).unwrap().cart;
    let req = FinalizeRequest {
        cart_id: cart.cart_id.clone().unwrap(),
        operation_id: op(),
        tenders: vec![cash(2_400)],
        approval_token: None,
        expected_total_minor: None,
    };
    let err = e.core.pos_finalize(&ct, req.clone()).unwrap_err();
    assert_eq!(err.code, ErrorCode::InsufficientStock);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM sales"), 1, "only the earlier allowed sale");
    let appr = e.core.approve(&ct, &e.owner_id, OWNER_PIN, "pos.negative_stock", "sell").unwrap();
    let mut ok = req;
    ok.approval_token = Some(appr["approval_token"].as_str().unwrap().to_string());
    e.core.pos_finalize(&ct, ok).unwrap();
    let eggs: i64 = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row(
                "SELECT s.qty_milli FROM stock_levels s JOIN product_barcodes b ON b.product_id=s.product_id WHERE b.barcode='444'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(eggs, -1_000);
}

#[test]
fn held_cart_reprices_on_restore() {
    let e = env();
    let t = &e.owner_token;
    let pid = e.product("Juice", "555", 500, 300, 10_000);
    e.open_shift(t, 0);
    e.core.pos_scan(t, "555", None).unwrap();
    e.core.pos_hold(t, Some("Customer went to get wallet".into())).unwrap();
    assert!(e.core.pos_get_cart(t).unwrap().lines.is_empty());
    let held = e.core.pos_held_list(t).unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].total_minor, 500);
    e.core.product_price_update(t, &pid, 550, Some("Supplier increase".into()), None).unwrap();
    let restored = e.core.pos_restore(t, &held[0].cart_id).unwrap();
    assert_eq!(restored.totals.total_minor, 550);
    assert_eq!(restored.notices.len(), 1);
    assert!(restored.notices[0].contains("0.500") && restored.notices[0].contains("0.550"));
    // Price history is retained.
    let d = e.core.product_get(t, &pid).unwrap();
    assert_eq!(d.price_history.len(), 2);
    assert!(d.price_history[1].effective_to.is_some());
}

#[test]
fn refunds_are_bounded_exact_and_idempotent() {
    let e = env();
    let t = &e.owner_token;
    e.product("Soap", "666", 1_000, 400, 10_000);
    e.open_shift(t, 10_000);
    e.core.pos_scan(t, "666", Some(3_000)).unwrap();
    let cart = e.core.pos_cart_discount(t, 100, 0, None).unwrap();
    assert_eq!(cart.totals.total_minor, 2_900);
    let sale = e
        .core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![cash(3_000)],
                approval_token: None,
                expected_total_minor: None,
            },
        )
        .unwrap();
    let detail = e.core.sale_get(t, &sale.sale_id).unwrap();
    let item = detail.items[0].sale_item_id.clone();
    let mk = |q: i64| RefundRequest {
        sale_id: sale.sale_id.clone(),
        lines: vec![RefundLineInput { sale_item_id: item.clone(), qty_milli: q, restock: true }],
        reason: "Damaged".into(),
        tenders: vec![],
        operation_id: op(),
        approval_token: None,
    };
    let r1 = e.core.refund_create(t, mk(1_000)).unwrap();
    assert_eq!(r1.total_minor, 967); // 2900 * 1/3 rounded
    let req2 = mk(2_000);
    let r2 = e.core.refund_create(t, req2.clone()).unwrap();
    assert_eq!(r1.total_minor + r2.total_minor, 2_900, "full refund equals the original exactly");
    let r2b = e.core.refund_create(t, req2).unwrap();
    assert!(r2b.replayed);
    assert_eq!(r2b.refund_id, r2.refund_id);
    let err = e.core.refund_create(t, mk(1)).unwrap_err();
    assert_eq!(err.code, ErrorCode::Validation);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM refunds"), 2);
    assert_eq!(count(&e, "SELECT qty_milli FROM stock_levels"), 10_000, "stock fully restored");
    let tax: i64 = count(&e, "SELECT SUM(tax_minor) FROM refunds");
    assert_eq!(tax, count(&e, "SELECT tax_minor FROM sales"), "VAT fully reversed");
    // Shift expected cash: float 10.000 + 2.900 cash sale - 2.900 refunded
    let sh = e.core.shift_current(t).unwrap().unwrap();
    assert_eq!(sh.expected_cash_minor, 10_000);
}

#[test]
fn cashier_refund_needs_manager() {
    let e = env();
    let (_c, ct) = e.user("Cashier Three", ROLE_CASHIER, "7410");
    e.product("Tea", "777", 700, 300, 10_000);
    e.open_shift(&ct, 1_000);
    let cart = e.core.pos_scan(&ct, "777", None).unwrap().cart;
    let sale = e
        .core
        .pos_finalize(
            &ct,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![cash(700)],
                approval_token: None,
                expected_total_minor: None,
            },
        )
        .unwrap();
    let d = e.core.refund_lookup(&ct, &sale.receipt_number).unwrap();
    let req = RefundRequest {
        sale_id: sale.sale_id.clone(),
        lines: vec![RefundLineInput { sale_item_id: d.items[0].sale_item_id.clone(), qty_milli: 1_000, restock: true }],
        reason: "Wrong item".into(),
        tenders: vec![],
        operation_id: op(),
        approval_token: None,
    };
    assert_eq!(e.core.refund_create(&ct, req.clone()).unwrap_err().code, ErrorCode::ApprovalRequired);
    let appr = e.core.approve(&ct, &e.owner_id, OWNER_PIN, "refund.create", "refund").unwrap();
    let mut ok = req;
    ok.approval_token = Some(appr["approval_token"].as_str().unwrap().into());
    let r = e.core.refund_create(&ct, ok).unwrap();
    assert_eq!(r.total_minor, 700);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM refunds WHERE approved_by IS NOT NULL"), 1);
}

#[test]
fn shift_reconciliation_and_cash_idempotency() {
    let e = env();
    let (_c, ct) = e.user("Cashier Four", ROLE_CASHIER, "8520");
    e.product("Bag", "888", 2_000, 1_000, 100_000);
    e.open_shift(&ct, 10_000);
    for _ in 0..3 {
        let cart = e.core.pos_scan(&ct, "888", None).unwrap().cart;
        e.core
            .pos_finalize(
                &ct,
                FinalizeRequest {
                    cart_id: cart.cart_id.unwrap(),
                    operation_id: op(),
                    tenders: vec![cash(5_000)],
                    approval_token: None,
                    expected_total_minor: None,
                },
            )
            .unwrap();
    }
    let cart = e.core.pos_scan(&ct, "888", None).unwrap().cart;
    e.core
        .pos_finalize(
            &ct,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![card(2_000)],
                approval_token: None,
                expected_total_minor: None,
            },
        )
        .unwrap();
    // Cashier lacks paid-out permission -> approval required.
    let po = CashEventRequest {
        kind: "paid_out".into(),
        amount_minor: 1_500,
        reason: "Cleaning supplies".into(),
        operation_id: op(),
        approval_token: None,
    };
    assert_eq!(e.core.cash_event(&ct, po.clone()).unwrap_err().code, ErrorCode::ApprovalRequired);
    let appr = e.core.approve(&ct, &e.owner_id, OWNER_PIN, "cash.paid_out", "paid out").unwrap();
    let mut po_ok = po;
    po_ok.approval_token = Some(appr["approval_token"].as_str().unwrap().into());
    let a = e.core.cash_event(&ct, po_ok.clone()).unwrap();
    let b = e.core.cash_event(&ct, po_ok).unwrap();
    assert_eq!(a["cash_event_id"], b["cash_event_id"], "retry returns the original event");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM cash_events"), 1);
    // Blind close: cashier does not see the expected amount.
    let cur = e.core.shift_current(&ct).unwrap().unwrap();
    assert!(!cur.expected_visible);
    // expected = 10.000 + 6.000 cash - 1.500 = 14.500. Counting 12.000 is a 2.500 shortage > 1.000 threshold.
    let close = ShiftCloseRequest { counted_cash_minor: 12_000, note: None, operation_id: op(), approval_token: None };
    assert_eq!(e.core.shift_close(&ct, &cur.shift_id, close.clone()).unwrap_err().code, ErrorCode::ApprovalRequired);
    let appr = e.core.approve(&ct, &e.owner_id, OWNER_PIN, "shift.approve_variance", "variance").unwrap();
    let mut close_ok = close;
    close_ok.approval_token = Some(appr["approval_token"].as_str().unwrap().into());
    let (sum, _print) = e.core.shift_close(&ct, &cur.shift_id, close_ok.clone()).unwrap();
    assert_eq!(sum.expected_cash_minor, 14_500);
    assert_eq!(sum.variance_minor, Some(-2_500));
    assert_eq!(sum.sale_count, 4);
    assert_eq!(sum.sales_total_minor, 8_000);
    // Retrying the close with the same operation id is harmless.
    let (again, _) = e.core.shift_close(&ct, &cur.shift_id, close_ok).unwrap();
    assert_eq!(again.variance_minor, Some(-2_500));
    // Selling now requires a shift.
    let cart = e.core.pos_scan(&ct, "888", None).unwrap().cart;
    let err = e
        .core
        .pos_finalize(
            &ct,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![cash(2_000)],
                approval_token: None,
                expected_total_minor: None,
            },
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ShiftRequired);
}

#[test]
fn printer_failure_never_undoes_a_sale() {
    let e = env();
    let t = &e.owner_token;
    let mut p: serde_json::Value = serde_json::to_value(amwapos_core::settings::PrinterSettings::default()).unwrap();
    p["mode"] = "network".into();
    p["target"] = "127.0.0.1:1".into(); // closed port
    e.core.settings_save(t, "local.printer", p).unwrap();
    e.product("Candle", "999", 300, 100, 10_000);
    e.open_shift(t, 0);
    let cart = e.core.pos_scan(t, "999", None).unwrap().cart;
    let sale = e
        .core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![cash(300)],
                approval_token: None,
                expected_total_minor: None,
            },
        )
        .unwrap();
    let pr = sale.print.unwrap();
    assert_eq!(pr.status, "failed");
    assert!(pr.message.unwrap().contains("not reachable"));
    assert_eq!(count(&e, "SELECT COUNT(*) FROM sales"), 1);
    // Switch to a file printer and retry: the same job prints, the sale is untouched.
    let path = e.dir.path().join("printer.txt");
    let mut p: serde_json::Value = serde_json::to_value(amwapos_core::settings::PrinterSettings::default()).unwrap();
    p["mode"] = "file".into();
    p["target"] = path.to_string_lossy().to_string().into();
    e.core.settings_save(t, "local.printer", p).unwrap();
    let r = e.core.print_retry(t, &pr.job_id.unwrap()).unwrap();
    assert_eq!(r.status, "printed");
    let printed = std::fs::read_to_string(&path).unwrap();
    assert!(printed.contains(&sale.receipt_number));
    e.core.sale_reprint(t, &sale.sale_id).unwrap();
    let printed = std::fs::read_to_string(&path).unwrap();
    assert!(printed.contains("*** COPY ***"));
    assert_eq!(count(&e, "SELECT COUNT(*) FROM sales"), 1);
}

#[test]
fn pin_lockout() {
    let e = env();
    let (uid, _t) = e.user("Cashier Five", ROLE_CASHIER, "9631");
    for i in 0..4 {
        let err = e.core.login(&uid, "0000").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidCredentials, "attempt {i}");
    }
    let err = e.core.login(&uid, "0000").unwrap_err();
    assert_eq!(err.code, ErrorCode::AccountLocked);
    let err = e.core.login(&uid, "9631").unwrap_err();
    assert_eq!(err.code, ErrorCode::AccountLocked, "correct PIN refused while locked");
    e.core.user_unlock(&e.owner_token, &uid).unwrap();
    e.core.login(&uid, "9631").unwrap();
    // PIN never stored in plaintext
    assert_eq!(count(&e, "SELECT COUNT(*) FROM users WHERE pin_hash LIKE '%9631%'"), 0);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM audit_logs WHERE after_json LIKE '%9631%' OR before_json LIKE '%9631%'"), 0);
}

#[test]
fn audit_chain_verifies() {
    let e = env();
    let t = &e.owner_token;
    e.product("Item", "121", 100, 10, 1_000);
    let rep = e.core.audit_verify(t).unwrap();
    assert!(rep.valid, "{}", rep.message);
    assert!(rep.entries >= 2);
}

#[test]
fn stocktake_counts_relative_to_count_time() {
    let e = env();
    let t = &e.owner_token;
    let pid = e.product("Flour", "131", 900, 500, 10_000);
    e.open_shift(t, 0);
    let st = e
        .core
        .stocktake_create(
            t,
            serde_json::from_value(serde_json::json!({ "name": "Weekly", "scope_type": "products", "product_ids": [pid] })).unwrap(),
        )
        .unwrap();
    // A sale before counting: system 9, physical 9 -> counted 8 means 1 missing.
    let cart = e.core.pos_scan(t, "131", None).unwrap().cart;
    e.core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![cash(900)],
                approval_token: None,
                expected_total_minor: None,
            },
        )
        .unwrap();
    e.core.stocktake_count(t, &st.header.stocktake_id, None, Some("131".into()), 8_000, "set").unwrap();
    // A sale after counting must not be double counted.
    let cart = e.core.pos_scan(t, "131", None).unwrap().cart;
    e.core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![cash(900)],
                approval_token: None,
                expected_total_minor: None,
            },
        )
        .unwrap();
    e.core.stocktake_set_status(t, &st.header.stocktake_id, "review").unwrap();
    let r = e.core.stocktake_finalize(t, &st.header.stocktake_id, &op()).unwrap();
    assert_eq!(r["adjusted_lines"], 1);
    // 10 - 1 sale - 1 missing - 1 sale = 7
    assert_eq!(count(&e, "SELECT qty_milli FROM stock_levels"), 7_000);
}

#[test]
fn receiving_updates_weighted_average_cost() {
    let e = env();
    let t = &e.owner_token;
    let pid = e.product("Oil", "141", 2_000, 1_000, 10_000); // 10 @ 1.000
    let req: amwapos_core::inventory::ReceiveRequest = serde_json::from_value(serde_json::json!({
        "reference": "INV-1", "operation_id": op(),
        "lines": [{ "product_id": pid, "qty_milli": 10000, "unit_cost_minor": 1400 }]
    }))
    .unwrap();
    e.core.inventory_receive(t, req.clone()).unwrap();
    e.core.inventory_receive(t, req).unwrap(); // replay
    let d = e.core.product_get(t, &pid).unwrap();
    assert_eq!(d.avg_cost_minor, Some(1_200));
    assert_eq!(d.last_cost_minor, Some(1_400));
    assert_eq!(d.row.stock_milli, 20_000);
}
