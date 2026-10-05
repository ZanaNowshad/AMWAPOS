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

fn expense(e: &Env, t: &str, total: i64, vat: i64, cat: &str) -> amwapos_core::expenses::ExpenseRow {
    e.core
        .expense_save(
            t,
            None,
            serde_json::from_value(
                json!({ "category_id": cat, "description": "October", "total_minor": total, "vat_minor": vat, "payee": "Landlord" }),
            )
            .unwrap(),
        )
        .unwrap()
}

#[test]
fn expense_lifecycle_and_operating_profit() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    // A sale: 2 × 1.100 incl. 10% VAT (net 1.000 each), cost 0.600 each.
    e.product("Juice", "7301", 1_100, 600, 50_000);
    sell(&e, "7301", 2000, "cash");

    // Draft → (owner may approve) approved on entry → paid by bank.
    let x = expense(&e, t, 105_000, 5_000, "exp_rent");
    assert_eq!((x.status.as_str(), x.net_minor, x.vat_minor), ("draft", 100_000, 5_000));
    let x = e.core.expense_submit(t, &x.expense_id).unwrap();
    assert_eq!(x.status, "approved");
    // Amounts are frozen once submitted (by the database itself).
    let frozen = e.core.db.write(|tx| Ok(tx.execute("UPDATE expenses SET total_minor=1, net_minor=1, vat_minor=0", [])?));
    assert!(frozen.is_err());
    let pay = |method: &str| -> amwapos_core::expenses::PayInput {
        serde_json::from_value(json!({ "method": method, "operation_id": op() })).unwrap()
    };
    let p = pay("bank_transfer");
    let x = e.core.expense_pay(t, &x.expense_id, p.clone()).unwrap();
    assert_eq!(x.status, "paid");
    // A lost reply retried returns the same payment.
    assert_eq!(e.core.expense_pay(t, &x.expense_id, p).unwrap().status, "paid");

    // A drafted expense is not counted; a voided one stops counting.
    expense(&e, t, 9_000, 0, "exp_cleaning");
    let y = expense(&e, t, 20_000, 0, "exp_transport");
    let y = e.core.expense_submit(t, &y.expense_id).unwrap();
    let rep = e.core.report_run(t, "operating_profit", Default::default()).unwrap();
    let k = |label: &str| rep.kpis.iter().find(|k| k.label == label).unwrap().value;
    assert_eq!(k("Revenue"), 2_000);
    assert_eq!(k("Gross profit"), 2_000 - 1_200);
    assert_eq!(k("Operating expenses"), 100_000 + 20_000);
    assert_eq!(k("Operating profit"), 800 - 120_000);
    e.core.expense_void(t, &y.expense_id, "Entered twice", &op()).unwrap();
    let rep = e.core.report_run(t, "operating_profit", Default::default()).unwrap();
    assert_eq!(rep.kpis.iter().find(|k| k.label == "Operating expenses").unwrap().value, 100_000);
    assert!(rep.notes.iter().any(|n| n.contains("not net profit")));
}

#[test]
fn expenses_need_an_approver_unless_within_the_limit() {
    let e = env();
    let t = &e.owner_token;
    let (_a, at) = e.user("Accounts", amwapos_core::auth::ROLE_ACCOUNTANT, "7531");
    // The accountant enters and pays but does not approve.
    let x = expense(&e, &at, 30_000, 0, "exp_repairs");
    let x = e.core.expense_submit(&at, &x.expense_id).unwrap();
    assert_eq!(x.status, "submitted");
    assert_eq!(e.core.expense_decide(&at, &x.expense_id, true, None).unwrap_err().code, ErrorCode::Forbidden);
    let pay: amwapos_core::expenses::PayInput = serde_json::from_value(json!({ "method": "card", "operation_id": op() })).unwrap();
    assert_eq!(e.core.expense_pay(&at, &x.expense_id, pay.clone()).unwrap_err().code, ErrorCode::Conflict, "not approved yet");
    // A rejection needs a reason.
    assert_eq!(e.core.expense_decide(t, &x.expense_id, false, None).unwrap_err().code, ErrorCode::Validation);
    e.core.expense_decide(t, &x.expense_id, true, None).unwrap();
    assert_eq!(e.core.expense_pay(&at, &x.expense_id, pay).unwrap().status, "paid");

    // Small amounts can be approved on entry by store policy.
    e.core.settings_save(t, "expenses", json!({ "auto_approve_up_to_minor": 5_000 })).unwrap();
    let small = expense(&e, &at, 4_000, 0, "exp_supplies");
    assert_eq!(e.core.expense_submit(&at, &small.expense_id).unwrap().status, "approved");
    let big = expense(&e, &at, 6_000, 0, "exp_supplies");
    assert_eq!(e.core.expense_submit(&at, &big.expense_id).unwrap().status, "submitted");

    // A cashier cannot see expenses at all.
    let (_c, ct) = e.user("Cashier X", amwapos_core::auth::ROLE_CASHIER, "8642");
    assert_eq!(e.core.expenses_list(&ct, None, None, None).unwrap_err().code, ErrorCode::Forbidden);
}

#[test]
fn petty_cash_balance_follows_its_entries() {
    let e = env();
    let t = &e.owner_token;
    let f = e.core.petty_fund_save(t, None, "Front desk", None, true).unwrap();
    let fid = f["fund_id"].as_str().unwrap().to_string();
    e.core.petty_entry(t, &fid, "open", 50_000, None, &op()).unwrap();
    let bal = |e: &Env| e.core.petty_funds(t).unwrap().into_iter().find(|x| x.fund_id == fid).unwrap().balance_minor;
    assert_eq!(bal(&e), 50_000);

    // Paying more than the fund holds is refused.
    let x = expense(&e, t, 60_000, 0, "exp_supplies");
    e.core.expense_submit(t, &x.expense_id).unwrap();
    let pay = |fid: &str| -> amwapos_core::expenses::PayInput {
        serde_json::from_value(json!({ "method": "petty_cash", "fund_id": fid, "operation_id": op() })).unwrap()
    };
    assert_eq!(e.core.expense_pay(t, &x.expense_id, pay(&fid)).unwrap_err().code, ErrorCode::Conflict);
    e.core.expense_void(t, &x.expense_id, "Wrong amount", &op()).unwrap();

    // Paid from the fund, then voided: the money goes back, nothing deleted.
    let y = expense(&e, t, 12_500, 0, "exp_cleaning");
    e.core.expense_submit(t, &y.expense_id).unwrap();
    e.core.expense_pay(t, &y.expense_id, pay(&fid)).unwrap();
    assert_eq!(bal(&e), 37_500);
    e.core.expense_void(t, &y.expense_id, "Supplier refunded", &op()).unwrap();
    assert_eq!(bal(&e), 50_000);

    // A count records the difference; the balance is then what was counted.
    let c = e.core.petty_count(t, &fid, 49_000, Some("1 BD short".into()), &op()).unwrap();
    assert_eq!(c["difference_minor"], -1_000);
    assert_eq!(bal(&e), 49_000);
    let entries = e.core.petty_entries(t, &fid).unwrap();
    assert_eq!(entries.as_array().unwrap().last().unwrap()["balance_minor"], 49_000);
    // Entries can never be edited.
    assert!(e.core.db.write(|tx| Ok(tx.execute("UPDATE petty_cash_entries SET amount_minor=0", [])?)).is_err());
}

#[test]
fn a_till_paid_out_backs_one_expense_only() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 50_000);
    let ce = e
        .core
        .cash_event(
            t,
            serde_json::from_value(json!({ "kind": "paid_out", "amount_minor": 3_000, "reason": "Water delivery", "operation_id": op() }))
                .unwrap(),
        )
        .unwrap();
    let ce_id =
        ce["cash_event_id"].as_str().map(str::to_string).unwrap_or_else(|| ce["cash_event"]["cash_event_id"].as_str().unwrap().to_string());
    let link = |x: &amwapos_core::expenses::ExpenseRow| {
        e.core.expense_submit(t, &x.expense_id).unwrap();
        e.core.expense_pay(
            t,
            &x.expense_id,
            serde_json::from_value(json!({ "method": "till_paid_out", "cash_event_id": ce_id, "operation_id": op() })).unwrap(),
        )
    };
    let wrong = expense(&e, t, 2_500, 0, "exp_water");
    assert_eq!(link(&wrong).unwrap_err().code, ErrorCode::Validation, "amount must match");
    let a = expense(&e, t, 3_000, 0, "exp_water");
    assert_eq!(link(&a).unwrap().status, "paid");
    let b = expense(&e, t, 3_000, 0, "exp_water");
    assert_eq!(link(&b).unwrap_err().code, ErrorCode::Conflict, "a paid-out backs one expense");
}

#[test]
fn recurring_expenses_make_drafts_never_payments() {
    let e = env();
    let t = &e.owner_token;
    let today = e.core.db.read(|c| amwapos_core::time::business_date(amwapos_core::time::now(), &amwapos_core::time::day(c)?)).unwrap();
    let r = e
        .core
        .expense_recurring_save(
            t,
            None,
            serde_json::from_value(json!({ "name": "Shop rent", "category_id": "exp_rent", "description": "Monthly rent", "total_minor": 400_000, "cadence": "monthly", "day": 1 })).unwrap(),
        )
        .unwrap();
    // Make it due today.
    e.core.db.write(|tx| Ok(tx.execute("UPDATE expense_recurring SET next_date=?1", [&today])?)).unwrap();
    let l = e.core.expenses_list(t, None, None, None).unwrap();
    let drafts: Vec<_> = l["rows"].as_array().unwrap().iter().filter(|x| x["recurring_id"] == r["recurring_id"]).collect();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0]["status"], "draft");
    // Opening the list again does not make a second one.
    let l = e.core.expenses_list(t, None, None, None).unwrap();
    assert_eq!(l["rows"].as_array().unwrap().iter().filter(|x| x["recurring_id"] == r["recurring_id"]).count(), 1);
    // Drafts are not expenses yet.
    assert_eq!(l["spent_minor"], 0);
}
