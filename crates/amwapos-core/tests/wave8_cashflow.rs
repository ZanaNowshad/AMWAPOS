//! Wave 8: Cash-flow Radar (docs/INTELLIGENCE_AND_EVIDENCE.md).

mod common;

use amwapos_core::payables::ManualInvoice;
use amwapos_core::ErrorCode;
use common::*;
use serde_json::{json, Value};

fn day(n: i64) -> String {
    (chrono::Utc::now().date_naive() + chrono::Duration::days(n)).to_string()
}

fn supplier(e: &Env, name: &str) -> String {
    e.core.supplier_save(&e.owner_token, None, serde_json::from_value(json!({ "name": name })).unwrap()).unwrap().supplier_id
}

fn posted_invoice(e: &Env, sup: &str, number: &str, due: &str, total: i64, po: Option<&str>) -> String {
    let vat = total / 11;
    let v = e
        .core
        .ap_invoice_create_manual(
            &e.owner_token,
            ManualInvoice {
                supplier_id: sup.into(),
                doc_type: "invoice".into(),
                invoice_number: number.into(),
                invoice_date: day(-30),
                due_date: Some(due.into()),
                subtotal_minor: total - vat,
                vat_minor: vat,
                total_minor: total,
                applies_to_invoice_id: None,
                po_id: po.map(str::to_string),
                notes: None,
                lines: vec![],
            },
        )
        .unwrap();
    let id = v["invoice_id"].as_str().unwrap().to_string();
    e.core.ap_invoice_approve(&e.owner_token, &id).unwrap();
    e.core.ap_invoice_post(&e.owner_token, &id, &op()).unwrap();
    id
}

fn radar(e: &Env, t: &str, h: i64) -> Value {
    e.core.cashflow_radar(t, Some(h)).unwrap()
}

fn horizon(r: &Value, h: i64) -> Value {
    r["horizons"].as_array().unwrap().iter().find(|x| x["days"] == h).unwrap().clone()
}

fn lines_of(r: &Value, kind: &str) -> Vec<Value> {
    r["lines"].as_array().unwrap().iter().filter(|l| l["kind"] == kind).cloned().collect()
}

fn category(e: &Env) -> String {
    e.core.expense_categories(&e.owner_token).unwrap()[0]["category_id"].as_str().unwrap().to_string()
}

fn expense(e: &Env, desc: &str, total: i64) -> String {
    e.core
        .expense_save(
            &e.owner_token,
            None,
            serde_json::from_value(json!({ "category_id": category(e), "description": desc, "total_minor": total })).unwrap(),
        )
        .unwrap()
        .expense_id
}

#[test]
fn an_empty_store_shows_nothing_and_never_claims_a_bank_balance() {
    let e = env();
    let r = radar(&e, &e.owner_token, 30);
    assert!(r["not_a_bank_balance"].as_str().unwrap().contains("not your bank balance"));
    assert_eq!(r["lines"].as_array().unwrap().len(), 0);
    assert_eq!(r["cash_recorded"]["total_minor"], 0);
    assert_eq!(r["scenario"]["available"], false, "no sales history, no scenario");
    for h in [7, 14, 30, 60, 90] {
        assert_eq!(horizon(&r, h)["known_out_minor"], 0);
    }
    assert_eq!(e.core.cashflow_radar(&e.owner_token, Some(45)).unwrap_err().code, ErrorCode::Validation);
}

#[test]
fn supplier_invoices_count_on_their_due_dates_and_late_ones_count_today() {
    let e = env();
    let t = &e.owner_token;
    let sup = supplier(&e, "Gulf Dairy");
    posted_invoice(&e, &sup, "GD-1", &day(10), 110_000, None);
    posted_invoice(&e, &sup, "GD-2", &day(-5), 55_000, None);
    let r = radar(&e, t, 90);
    assert_eq!(horizon(&r, 7)["known_out_minor"], 55_000, "the overdue one counts today");
    assert_eq!(horizon(&r, 14)["known_out_minor"], 165_000);
    let late = lines_of(&r, "supplier_invoice").into_iter().find(|l| l["overdue"] == true).unwrap();
    assert_eq!(late["date"], r["as_of"]);
    assert_eq!(late["source"]["link"], "/admin/payables");
    assert_eq!(r["overdue_out_minor"], 55_000);
    // A part payment reduces what is open; money not matched to an invoice is shown apart.
    e.core
        .ap_payment_record(
            t,
            amwapos_core::payables::PaymentInput {
                supplier_id: sup.clone(),
                paid_on: day(0),
                amount_minor: 20_000,
                method: "bank_transfer".into(),
                reference: None,
                notes: None,
                allocations: vec![],
                oldest_first: false,
                operation_id: op(),
            },
        )
        .unwrap();
    let r = radar(&e, t, 90);
    assert_eq!(r["unapplied_supplier_balance_minor"], 20_000);
    assert_eq!(horizon(&r, 90)["known_out_minor"], 165_000, "not netted silently into the dated lines");
}

#[test]
fn expenses_fall_in_their_band_and_a_repeating_one_counts_once() {
    let e = env();
    let t = &e.owner_token;
    let approved = expense(&e, "Electricity", 40_000);
    e.core.expense_submit(t, &approved).unwrap();
    let st = e.core.expense_get(t, &approved).unwrap()["expense"]["status"].as_str().unwrap().to_string();
    if st == "submitted" {
        e.core.expense_decide(t, &approved, true, None).unwrap();
    }
    let waiting = expense(&e, "Repairs", 15_000);
    e.core.expense_submit(t, &waiting).unwrap();
    expense(&e, "A draft nobody submitted", 99_000);
    // A weekly repeating expense.
    let rec = e
        .core
        .expense_recurring_save(
            t,
            None,
            serde_json::from_value(json!({ "name": "Cleaning", "category_id": category(&e), "description": "Weekly cleaning",
                "total_minor": 5_000, "cadence": "weekly", "day": 3 }))
            .unwrap(),
        )
        .unwrap();
    let rid = rec.as_array().map(|a| a[0]["recurring_id"].clone()).unwrap_or_else(|| rec["recurring_id"].clone());
    let rid = rid.as_str().unwrap().to_string();
    let r = radar(&e, t, 14);
    // Each expense sits in the band its status gives it.
    for id in [&approved, &waiting] {
        let status = e.core.expense_get(t, id).unwrap()["expense"]["status"].as_str().unwrap().to_string();
        let line = r["lines"].as_array().unwrap().iter().find(|l| l["source"]["id"] == id.as_str()).cloned().unwrap();
        let want = match status.as_str() {
            "approved" => "known",
            "submitted" => "exposure",
            other => panic!("unexpected status {other}"),
        };
        assert_eq!(line["band"], want, "{status}");
    }
    assert!(r["lines"].as_array().unwrap().iter().all(|l| l["amount_minor"] != 99_000), "plain drafts are not counted");
    let weekly = lines_of(&r, "recurring_expense");
    assert_eq!(weekly.len(), 2, "two occurrences in any 14 days");
    assert_eq!(horizon(&r, 14)["scheduled_out_minor"], 10_000);
    // The repeating expense makes its draft for the first date: that counts instead, once.
    let first = weekly.iter().map(|l| l["date"].as_str().unwrap().to_string()).min().unwrap();
    let draft = expense(&e, "Weekly cleaning", 5_000);
    e.core
        .db
        .write(|c| Ok(c.execute("UPDATE expenses SET recurring_id=?1, business_date=?2 WHERE expense_id=?3", [&rid, &first, &draft])?))
        .unwrap();
    let r = radar(&e, t, 14);
    assert_eq!(lines_of(&r, "recurring_expense").len(), 1);
    assert_eq!(lines_of(&r, "expense_recurring_draft").len(), 1);
    assert_eq!(horizon(&r, 14)["scheduled_out_minor"], 10_000, "not 15,000: nothing counted twice");
}

#[test]
fn a_purchase_order_counts_only_what_is_not_invoiced_yet() {
    let e = env();
    let t = &e.owner_token;
    let sup = supplier(&e, "Alpha Foods");
    let branch = e.core.session_info(t).unwrap().branch_id;
    let po = amwapos_core::ids::new_id();
    e.core
        .db
        .write(|c| {
            Ok(c.execute(
                "INSERT INTO purchase_orders(po_id, po_number, supplier_id, branch_id, status, expected_at, subtotal_minor, tax_minor, total_minor,
                   created_by, created_at, updated_at, version)
                 VALUES (?1,'PO-9001',?2,?3,'ordered',?4,300000,0,300000,'x',?4,?4,1)",
                rusqlite::params![po, sup, branch, day(5)],
            )?)
        })
        .unwrap();
    let r = radar(&e, t, 30);
    assert_eq!(horizon(&r, 7)["exposure_out_minor"], 300_000);
    // Part of it is invoiced and posted: that part is known, the rest stays exposure.
    posted_invoice(&e, &sup, "AF-77", &day(20), 120_000, Some(&po));
    let r = radar(&e, t, 30);
    assert_eq!(horizon(&r, 30)["exposure_out_minor"], 180_000);
    assert_eq!(horizon(&r, 30)["known_out_minor"], 120_000);
    assert_eq!(
        horizon(&r, 30)["exposure_out_minor"].as_i64().unwrap() + horizon(&r, 30)["known_out_minor"].as_i64().unwrap(),
        300_000,
        "the order is counted once"
    );
    assert_eq!(lines_of(&r, "purchase_order")[0]["source"]["link"], format!("/admin/purchase-orders/{po}"));
}

#[test]
fn cash_recorded_in_the_store_and_customers_owing_have_no_invented_dates() {
    let e = env();
    let t = &e.owner_token;
    e.product("Tea", "8101", 800, 400, 10_000);
    e.open_shift(t, 5_000);
    let cart = e.core.pos_scan(t, "8101", Some(1000)).unwrap().cart;
    e.core
        .pos_finalize(
            t,
            amwapos_core::sales::FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![amwapos_core::pricing::TenderInput { method: "cash".into(), amount_minor: 800, reference: None }],
                approval_token: None,
                expected_total_minor: None,
                fulfilment: None,
            },
        )
        .unwrap();
    let r = radar(&e, t, 7);
    assert_eq!(r["cash_recorded"]["drawers_expected_minor"], 5_800);
    assert_eq!(r["cash_recorded"]["total_minor"], 5_800);
    assert!(r["cash_recorded"]["note"].as_str().unwrap().contains("Not the bank"));
    assert!(r["receivables"]["note"].as_str().unwrap().contains("no date"));
    assert_eq!(r["scenario"]["available"], false, "one day of sales is not enough for a scenario");
}

#[test]
fn a_heavy_week_is_a_pressure_week_and_the_figures_add_up() {
    let e = env();
    let t = &e.owner_token;
    let sup = supplier(&e, "Gulf Dairy");
    posted_invoice(&e, &sup, "BIG-1", &day(2), 900_000, None);
    posted_invoice(&e, &sup, "SMALL-1", &day(20), 100_000, None);
    let r = radar(&e, t, 30);
    let weeks = r["weeks"].as_array().unwrap();
    assert_eq!(weeks.len(), 5);
    assert_eq!(weeks[0]["pressure"], true);
    assert!(weeks.iter().skip(1).all(|w| w["pressure"] == false));
    let total: i64 = weeks.iter().map(|w| w["known_out_minor"].as_i64().unwrap()).sum();
    assert_eq!(total, horizon(&r, 30)["known_out_minor"].as_i64().unwrap());
    // Horizons only grow.
    let k: Vec<i64> = [7, 14, 30, 60, 90].iter().map(|h| horizon(&r, *h)["known_out_minor"].as_i64().unwrap()).collect();
    assert!(k.windows(2).all(|w| w[0] <= w[1]), "{k:?}");
    assert!(r["formulas"].as_array().unwrap().len() >= 5);
}

#[test]
fn only_owners_managers_and_accountants_see_it_and_the_assistant_only_reads_it() {
    let e = env();
    e.core.settings_save(&e.owner_token, "features", json!({ "ai.enabled": true, "ai.mutations": true })).unwrap();
    let (_, acc) = e.user("Sara", "role_accountant", "3691");
    assert!(e.core.cashflow_radar(&acc, None).is_ok());
    let (_, mgr) = e.user("Mani", "role_manager", "3692");
    assert!(e.core.cashflow_radar(&mgr, None).is_ok());
    for (n, role, pin) in [("Omar", "role_cashier", "1470"), ("Ali", "role_inventory", "2580")] {
        let (_, tk) = e.user(n, role, pin);
        assert_eq!(e.core.cashflow_radar(&tk, None).unwrap_err().code, ErrorCode::Forbidden, "{role}");
    }
    let (r, err) = e.core.ai_tool(&e.owner_token, "01CASHFLOWTEST000000000000", "cashflow_radar", &json!({ "horizon": 14 }));
    assert!(!err, "{r}");
    assert_eq!(r["evidence"]["basis"], "derived");
    assert!(!amwapos_core::ai_tools::TOOLS
        .iter()
        .any(|s| s.cmd.starts_with("cashflow.") && s.kind == amwapos_core::ai_tools::Kind::Propose));
}

#[test]
fn with_28_days_of_sales_the_scenario_is_shown_and_labelled_as_an_estimate() {
    let e = env();
    let t = &e.owner_token;
    e.product("Tea", "8101", 2_800, 400, 100_000);
    e.open_shift(t, 0);
    for _ in 0..2 {
        let cart = e.core.pos_scan(t, "8101", Some(1000)).unwrap().cart;
        e.core
            .pos_finalize(
                t,
                amwapos_core::sales::FinalizeRequest {
                    cart_id: cart.cart_id.unwrap(),
                    operation_id: op(),
                    tenders: vec![amwapos_core::pricing::TenderInput { method: "cash".into(), amount_minor: 2_800, reference: None }],
                    approval_token: None,
                    expected_total_minor: None,
                    fulfilment: None,
                },
            )
            .unwrap();
    }
    // Test database only: the two sales happened 40 and 10 days ago.
    let ids: Vec<String> = e
        .core
        .db
        .read(|c| {
            let mut st = c.prepare("SELECT sale_id FROM sales ORDER BY created_at")?;
            let r = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            Ok(r)
        })
        .unwrap();
    e.core
        .db
        .write(|c| {
            let guards: Vec<String> = {
                let mut st = c.prepare("SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name='sales'")?;
                let r = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
                r
            };
            for g in guards {
                c.execute_batch(&format!("DROP TRIGGER {g}"))?;
            }
            c.execute("UPDATE sales SET business_date=?1 WHERE sale_id=?2", rusqlite::params![day(-40), ids[0]])?;
            c.execute("UPDATE sales SET business_date=?1 WHERE sale_id=?2", rusqlite::params![day(-10), ids[1]])?;
            Ok(())
        })
        .unwrap();
    let r = radar(&e, t, 14);
    let s = &r["scenario"];
    assert_eq!(s["available"], true, "{s}");
    assert_eq!(s["basis"], "estimate");
    assert_eq!(s["net_sales_minor"], 2_800, "only the last 28 days");
    assert_eq!(s["daily_average_minor"], 100);
    assert_eq!(horizon(&r, 14)["scenario_in_minor"], 1_400);
    assert!(s["assumption"].as_str().unwrap().starts_with("If sales"));
}
