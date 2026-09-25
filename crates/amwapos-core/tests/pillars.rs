//! Product-brief pillars: flag-off behaviour, transfers (in transit,
//! idempotency, no stock creation), loyalty, digital orders, multi-branch.
mod common;

use amwapos_core::ErrorCode;
use common::*;
use serde_json::json;

fn count(e: &Env, sql: &str) -> i64 {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn features(e: &Env, v: serde_json::Value) {
    e.core.settings_save(&e.owner_token, "features", v).unwrap();
}

fn qty(e: &Env, pid: &str) -> i64 {
    e.core.db.read(|c| amwapos_core::inventory::current_qty(c, pid, &e.core.require_device().unwrap().branch_id)).unwrap()
}

#[test]
fn transfers_between_locations_are_in_transit_until_received() {
    let e = env();
    let t = &e.owner_token;
    let milk = e.product("Milk", "9001", 500, 300, 10_000);
    // Flag off: refused, nothing changes.
    let err = e.core.locations_list(t).unwrap_err();
    assert_eq!(err.details.unwrap()["kind"], "feature_disabled");
    features(&e, json!({ "inventory.locations": true }));
    let locs = e.core.locations_list(t).unwrap();
    assert_eq!(locs.len(), 1);
    assert!(locs[0].is_default);
    let locs = e.core.location_save(t, None, serde_json::from_value(json!({ "code": "shelf", "name": "Front shelf" })).unwrap()).unwrap();
    let shelf = locs.iter().find(|l| l.code == "SHELF").unwrap().location_id.clone();
    let mk = |q: i64| {
        e.core
            .transfer_create(
                t,
                serde_json::from_value(json!({ "to_location_id": shelf, "lines": [{ "product_id": milk, "qty_milli": q }] })).unwrap(),
            )
            .unwrap()
    };
    let tr = mk(3_000);
    assert_eq!(tr.status, "draft");
    assert_eq!(qty(&e, &milk), 10_000, "a draft moves nothing");
    let op1 = op();
    e.core.transfer_ship(t, &tr.transfer_id, &op1).unwrap();
    assert_eq!(qty(&e, &milk), 7_000, "shipped stock has left the stockroom");
    assert_eq!(e.core.transfers_in_transit(t).unwrap()[0]["qty_milli"], 3_000);
    // Replay: same id → no second movement.
    e.core.transfer_ship(t, &tr.transfer_id, &op1).unwrap();
    assert_eq!(qty(&e, &milk), 7_000);
    // The same id on another transfer is refused.
    let tr2 = mk(1_000);
    assert_eq!(e.core.transfer_ship(t, &tr2.transfer_id, &op1).unwrap_err().code, ErrorCode::IdempotencyMismatch);
    // Receiving: exactly what was shipped arrives at the shelf.
    let op2 = op();
    let done = e.core.transfer_receive(t, &tr.transfer_id, &op2).unwrap();
    assert_eq!(done.status, "received");
    e.core.transfer_receive(t, &tr.transfer_id, &op2).unwrap();
    assert_eq!(qty(&e, &milk), 10_000, "branch total restored once");
    assert!(e.core.transfers_in_transit(t).unwrap().is_empty());
    let at_shelf = e.core.location_stock(t, &shelf).unwrap();
    assert_eq!(at_shelf[0]["qty_milli"], 3_000);
    let room = locs.iter().find(|l| l.is_default).unwrap().location_id.clone();
    assert_eq!(e.core.location_stock(t, &room).unwrap()[0]["qty_milli"], 7_000);
    // No stock is created: cannot ship more than the location holds, cannot receive a draft.
    let big = mk(8_000);
    assert_eq!(e.core.transfer_ship(t, &big.transfer_id, &op()).unwrap_err().code, ErrorCode::InsufficientStock);
    assert_eq!(e.core.transfer_receive(t, &big.transfer_id, &op()).unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_movements WHERE type IN ('transfer_in','transfer_out')"), 2);
    // Another branch needs the multi-branch module.
    let err = e
        .core
        .transfer_create(
            t,
            serde_json::from_value(json!({ "to_branch_id": "OTHER", "lines": [{ "product_id": milk, "qty_milli": 1000 }] })).unwrap(),
        )
        .unwrap_err();
    assert_eq!(err.details.unwrap()["feature"], "org.multi_branch");
    assert!(count(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type LIKE 'transfer.%'") >= 4);
}

fn sell(e: &Env, barcode: &str, qty_milli: i64, customer: Option<&str>, points: i64) -> amwapos_core::sales::SaleResult {
    let t = &e.owner_token;
    e.core.pos_scan(t, barcode, Some(qty_milli)).unwrap();
    if let Some(c) = customer {
        e.core.pos_set_customer(t, Some(c.to_string())).unwrap();
    }
    let cart = if points > 0 { e.core.pos_loyalty_redeem(t, points).unwrap() } else { e.core.pos_get_cart(t).unwrap() };
    let total = cart.totals.total_minor;
    e.core
        .pos_finalize(
            t,
            amwapos_core::sales::FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![amwapos_core::pricing::TenderInput { method: "cash".into(), amount_minor: total, reference: None }],
                approval_token: None,
                expected_total_minor: Some(total),
            },
        )
        .unwrap()
}

#[test]
fn loyalty_earns_on_commit_redeems_as_discount_and_reverses_on_refund() {
    let e = env();
    let t = &e.owner_token;
    e.product("Juice", "9101", 1_000, 600, 100_000);
    let cust = e.core.customer_save(t, None, serde_json::from_value(json!({ "name": "Noor", "phone": "33331111" })).unwrap()).unwrap();
    let cid = cust.customer_id.clone();
    e.open_shift(t, 0);
    // Flag off: no points, redemption refused.
    sell(&e, "9101", 1_000, Some(&cid), 0);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM loyalty_ledger"), 0);
    assert!(e.core.pos_loyalty_redeem(t, 10).is_err());
    features(&e, json!({ "loyalty.enabled": true }));
    // 5.000 paid at 1 point per 0.100 → 50 points, written in the sale commit.
    let s1 = sell(&e, "9101", 5_000, Some(&cid), 0);
    assert_eq!(e.core.loyalty_customer(t, &cid).unwrap()["balance"], 50);
    // Redeem 40 points (0.005 each = 0.200) as a discount under the normal pricing engine.
    let mut cfg: amwapos_core::settings::LoyaltySettings = e.core.db.read(|c| amwapos_core::settings::get(c, "loyalty")).unwrap();
    cfg.min_redeem_points = 10;
    e.core.settings_save(t, "loyalty", serde_json::to_value(&cfg).unwrap()).unwrap();
    let s2 = sell(&e, "9101", 2_000, Some(&cid), 40);
    let d = e.core.sale_get(t, &s2.sale_id).unwrap();
    assert_eq!(s2.total_minor, 2_000 - 200, "tax-inclusive price minus the redemption");
    let alloc: i64 = e
        .core
        .db
        .read(|c| Ok(c.query_row("SELECT SUM(loyalty_discount_minor) FROM sale_items WHERE sale_id=?1", [&s2.sale_id], |r| r.get(0))?))
        .unwrap();
    assert_eq!(alloc, 200, "the redemption is a discount snapshot on the lines");
    let (_, expect) = amwapos_core::pricing::price_cart(
        &[amwapos_core::pricing::LineInput {
            unit_price_minor: 1_000,
            qty_milli: 2_000,
            line_discount_minor: 0,
            line_discount_bp: 0,
            tax_rate_bp: 1000,
            tax_inclusive: true,
        }],
        200,
        0,
    )
    .unwrap();
    assert_eq!(d.tax_minor, expect.tax_minor, "VAT from the same engine as any discount");
    // Not cash: the drawer only records what was tendered.
    let cash: i64 = e
        .core
        .db
        .read(|c| Ok(c.query_row("SELECT SUM(amount_minor) FROM payments WHERE sale_id=?1", [&s2.sale_id], |r| r.get(0))?))
        .unwrap();
    assert_eq!(cash, 1_800);
    // 50 − 40 + 18 earned on 1.800 paid.
    assert_eq!(e.core.loyalty_customer(t, &cid).unwrap()["balance"], 28);
    // Refund 2 of 5 from the first sale: 40% of its 50 points is taken back.
    let d1 = e.core.sale_get(t, &s1.sale_id).unwrap();
    let r = e
        .core
        .refund_create(
            t,
            amwapos_core::refunds::RefundRequest {
                sale_id: s1.sale_id.clone(),
                lines: vec![amwapos_core::refunds::RefundLineInput {
                    sale_item_id: d1.items[0].sale_item_id.clone(),
                    qty_milli: 2_000,
                    restock: true,
                }],
                reason: "Returned".into(),
                tenders: vec![],
                operation_id: op(),
                approval_token: None,
            },
        )
        .unwrap();
    assert_eq!(r.total_minor, 2_000);
    assert_eq!(e.core.loyalty_customer(t, &cid).unwrap()["balance"], 8);
    // Refunding the redemption sale gives the redeemed points back in proportion.
    let r2 = e
        .core
        .refund_create(
            t,
            amwapos_core::refunds::RefundRequest {
                sale_id: s2.sale_id.clone(),
                lines: vec![amwapos_core::refunds::RefundLineInput {
                    sale_item_id: d.items[0].sale_item_id.clone(),
                    qty_milli: 2_000,
                    restock: true,
                }],
                reason: "Returned".into(),
                tenders: vec![],
                operation_id: op(),
                approval_token: None,
            },
        )
        .unwrap();
    assert_eq!(r2.total_minor, 1_800);
    assert_eq!(e.core.loyalty_customer(t, &cid).unwrap()["balance"], 8 - 18 + 40);
    assert!(count(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type LIKE 'loyalty.%'") >= 3);
}
