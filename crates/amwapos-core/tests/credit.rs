//! Customer accounts: gated by module, limit with manager override, ledger
//! entries in the same transaction as the sale/refund, cash payments counted
//! in the drawer, append-only ledger.
mod common;

use amwapos_core::pricing::TenderInput;
use amwapos_core::refunds::RefundRequest;
use amwapos_core::sales::FinalizeRequest;
use amwapos_core::shifts::ShiftCloseRequest;
use amwapos_core::ErrorCode;
use common::*;
use serde_json::json;

fn count(e: &Env, sql: &str) -> i64 {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

#[test]
fn account_sales_payments_and_refunds() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    e.product("Rice 5kg", "8001", 4_000, 3_000, 50_000);
    let mut pay = e.core.settings_get(t, "payments").unwrap();
    for x in pay["tenders"].as_array_mut().unwrap() {
        if x["method"] == "account" {
            x["enabled"] = json!(true);
        }
    }
    e.core.settings_save(t, "payments", pay).unwrap();
    let cust = e.core.customer_save(t, None, serde_json::from_value(json!({ "name": "Fatima", "phone": "33334444" })).unwrap()).unwrap();
    let cid = cust.customer_id.clone();

    let sell = |token: &str, qty: i64, approval: Option<String>| {
        let cart = e.core.pos_scan(token, "8001", Some(qty)).unwrap().cart;
        let cart_id = cart.cart_id.clone().unwrap();
        e.core.pos_set_customer(token, Some(cid.clone())).unwrap();
        let total = cart.totals.total_minor;
        e.core.pos_finalize(
            token,
            FinalizeRequest {
                cart_id,
                operation_id: op(),
                tenders: vec![TenderInput { method: "account".into(), amount_minor: total, reference: None }],
                approval_token: approval,
                expected_total_minor: Some(total),
            },
        )
    };
    // Module off.
    assert_eq!(sell(t, 1000, None).unwrap_err().details.unwrap()["kind"], "feature_disabled");
    e.core.pos_cancel_sale(t, None).ok();
    e.core.settings_save(t, "features", json!({ "customer_credit": true })).unwrap();
    // No account yet.
    assert_eq!(sell(t, 1000, None).unwrap_err().code, ErrorCode::Conflict);
    e.core.pos_cancel_sale(t, None).ok();
    e.core.customer_account_set(t, &cid, true, 10_000).unwrap();
    // Within the limit (4.000 of 10.000).
    let sale = sell(t, 1000, None).unwrap();
    assert_eq!(count(&e, "SELECT SUM(amount_minor) FROM customer_ledger"), 4_000);
    // Cash payment towards the balance goes into the drawer.
    let before = e.core.shift_current(t).unwrap().unwrap().expected_cash_minor;
    let p = e
        .core
        .customer_account_payment(
            t,
            serde_json::from_value(json!({ "customer_id": cid, "amount_minor": 1_500, "method": "cash", "operation_id": op() })).unwrap(),
        )
        .unwrap();
    assert_eq!(p["balance_minor"], 2_500);
    assert_eq!(e.core.shift_current(t).unwrap().unwrap().expected_cash_minor, before + 1_500);
    // Over-payment refused.
    assert!(e
        .core
        .customer_account_payment(
            t,
            serde_json::from_value(json!({ "customer_id": cid, "amount_minor": 9_000, "method": "card", "operation_id": op() })).unwrap()
        )
        .is_err());
    // Refund to the account reduces the balance.
    let lookup = e.core.refund_lookup(t, &sale.receipt_number).unwrap();
    let line = lookup.items[0].sale_item_id.clone();
    let r: RefundRequest = serde_json::from_value(json!({
        "sale_id": sale.sale_id, "operation_id": op(), "reason": "Damaged",
        "lines": [{ "sale_item_id": line, "qty_milli": 1000, "restock": true }],
        "tenders": [{ "method": "account", "amount_minor": 4_000 }]
    }))
    .unwrap();
    e.core.refund_create(t, r).unwrap();
    let acc = e.core.customer_account(t, &cid).unwrap();
    assert_eq!(acc["account"]["balance_minor"], -1_500, "customer now in credit");
    // A cashier going over the limit needs a manager.
    let cur = e.core.shift_current(t).unwrap().unwrap();
    let close = ShiftCloseRequest { counted_cash_minor: cur.expected_cash_minor, note: None, operation_id: op(), approval_token: None };
    e.core.shift_close(t, &cur.shift_id, close).unwrap();
    let (_, cashier) = e.user("Sara", "role_cashier", "2468");
    e.open_shift(&cashier, 0);
    let err = sell(&cashier, 3000, None).unwrap_err(); // 12.000 against 10.000 limit (balance -1.500)
    assert_eq!(err.code, ErrorCode::ApprovalRequired);
    assert_eq!(count(&e, "SELECT SUM(amount_minor) FROM customer_ledger"), -1_500, "nothing posted");
    let appr = e.core.approve(&cashier, &e.owner_id, OWNER_PIN, "customers.credit_override", "over limit").unwrap();
    e.core.pos_cancel_sale(&cashier, None).ok();
    sell(&cashier, 3000, Some(appr["approval_token"].as_str().unwrap().into())).unwrap();
    assert_eq!(count(&e, "SELECT SUM(amount_minor) FROM customer_ledger"), 10_500);
    // The ledger cannot be edited.
    assert!(e.core.db.write(|tx| Ok(tx.execute("UPDATE customer_ledger SET amount_minor=1", [])?)).is_err());
    assert!(e.core.db.write(|tx| Ok(tx.execute("DELETE FROM customer_ledger", [])?)).is_err());
}
