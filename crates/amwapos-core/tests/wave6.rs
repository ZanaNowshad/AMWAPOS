//! Wave 6 of the merchant operating system: promotions, coupons and virtual
//! bundles (docs/PROMOTIONS_AND_BUNDLES.md).

mod common;

use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::{FinalizeRequest, SaleResult};
use amwapos_core::ErrorCode;
use common::*;
use serde_json::{json, Value};

fn one<T: rusqlite::types::FromSql>(e: &Env, sql: &str) -> T {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn pay(e: &Env, t: &str) -> SaleResult {
    let cart = e.core.pos_get_cart(t).unwrap();
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

/// Save a promotion (created as a draft) from JSON; returns its id.
fn save(e: &Env, p: Value) -> String {
    let mut p = p;
    if p.get("target").is_none() {
        p["target"] = json!("items");
    }
    let r =
        e.core.promotions_save(&e.owner_token, serde_json::from_value(json!({ "promotion": p, "operation_id": op() })).unwrap()).unwrap();
    r["promotion"]["promotion_id"].as_str().unwrap().to_string()
}

fn version(e: &Env, id: &str) -> i64 {
    e.core.promotions_get(&e.owner_token, id).unwrap()["promotion"]["version"].as_i64().unwrap()
}

fn set_status(e: &Env, id: &str, status: &str) -> Value {
    let v = version(e, id);
    e.core
        .promotions_set_status(
            &e.owner_token,
            serde_json::from_value(json!({ "promotion_id": id, "status": status, "version": v, "operation_id": op() })).unwrap(),
        )
        .unwrap()
}

fn activate(e: &Env, p: Value) -> String {
    let id = save(e, p);
    set_status(e, &id, "active");
    id
}

fn ready(e: &Env) {
    e.open_shift(&e.owner_token, 0);
}

// ------------------------------------------------------------------ the pipeline without offers

#[test]
fn without_promotions_pricing_is_exactly_wave5() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let a = e.product("Rice 5kg", "600001", 2_750, 2_000, 50_000);
    let b = e.product("Oil 1L", "600002", 1_235, 900, 50_000);
    e.core.pos_add_product(t, &a, Some(3_000)).unwrap();
    e.core.pos_add_product(t, &b, Some(1_000)).unwrap();
    e.core.pos_cart_discount(t, 0, 500, None).unwrap();
    let cart = e.core.pos_get_cart(t).unwrap();
    // The Wave 5 arithmetic, by hand: gross 8250 + 1235 = 9485; 5% = 474 off.
    let inputs: Vec<amwapos_core::pricing::LineInput> = cart
        .lines
        .iter()
        .map(|l| amwapos_core::pricing::LineInput {
            unit_price_minor: l.unit_price_minor,
            qty_milli: l.qty_milli,
            line_discount_minor: 0,
            line_discount_bp: 0,
            tax_rate_bp: l.tax_rate_bp,
            tax_inclusive: l.tax_inclusive,
            promo_discount_minor: 0,
            parts: vec![],
        })
        .collect();
    let (_, want) = amwapos_core::pricing::price_cart(&inputs, 0, 500).unwrap();
    assert_eq!(cart.totals, want);
    assert_eq!(cart.totals.discount_minor, 474);
    assert!(cart.promotions.applied.is_empty() && cart.promotions.explain.is_empty());
    assert!(cart.lines.iter().all(|l| l.promo_discount_minor == 0 && l.offers.is_empty()));
    let sale = pay(&e, t);
    assert_eq!(sale.total_minor, want.total_minor);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM sale_item_promotions"), 0);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM sale_items WHERE promo_discount_minor <> 0"), 0);
}

// ------------------------------------------------------------------ lifecycle, schedule and scope

#[test]
fn a_promotion_applies_only_when_switched_on_in_schedule_and_scope() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let milk = e.product("Milk 1L", "600010", 500, 300, 50_000);
    let id = save(&e, json!({ "name": "Weekend offer", "kind": "percent", "percent_bp": 1_000, "buy_products": [milk] }));
    e.core.pos_add_product(t, &milk, Some(2_000)).unwrap();
    // A draft never applies (and the till is not told about drafts).
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.totals.total_minor, 1_000);
    assert!(cart.promotions.applied.is_empty() && cart.promotions.explain.is_empty());
    // Switched on: 10% off 1.000.
    set_status(&e, &id, "active");
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.totals.total_minor, 900);
    assert_eq!(cart.lines[0].promo_discount_minor, 100);
    assert_eq!(cart.lines[0].offers, vec!["Weekend offer".to_string()]);
    assert_eq!(cart.promotions.applied[0].amount_minor, 100);
    // Paused, then a schedule in the future: not active yet.
    set_status(&e, &id, "paused");
    assert_eq!(e.core.pos_get_cart(t).unwrap().totals.total_minor, 1_000);
    let mut p = e.core.promotions_get(t, &id).unwrap()["promotion"].clone();
    p["starts_at"] = json!("2099-01-01T00:00");
    e.core.promotions_save(t, serde_json::from_value(json!({ "promotion": p, "operation_id": op() })).unwrap()).unwrap();
    set_status(&e, &id, "active");
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.totals.total_minor, 1_000);
    assert_eq!(cart.promotions.explain[0].reason, "not_started");
    assert!(cart.promotions.explain[0].message.starts_with("Not active yet"), "{}", cart.promotions.explain[0].message);
    assert_eq!(e.core.promotions_get(t, &id).unwrap()["state"], "scheduled");
    // Back in schedule, but only for WhatsApp orders.
    let mut p = e.core.promotions_get(t, &id).unwrap()["promotion"].clone();
    p["starts_at"] = Value::Null;
    p["channels"] = json!(["whatsapp"]);
    e.core.promotions_save(t, serde_json::from_value(json!({ "promotion": p, "operation_id": op() })).unwrap()).unwrap();
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.totals.total_minor, 1_000);
    assert_eq!(cart.promotions.explain[0].message, "Only for WhatsApp sales.");
    e.core.pos_set_channel(t, "whatsapp").unwrap();
    assert_eq!(e.core.pos_get_cart(t).unwrap().totals.total_minor, 900);
    // Ended: never applies again, cannot be edited, cannot be deleted.
    set_status(&e, &id, "ended");
    assert_eq!(e.core.pos_get_cart(t).unwrap().totals.total_minor, 1_000);
    let p = e.core.promotions_get(t, &id).unwrap()["promotion"].clone();
    let err = e.core.promotions_save(t, serde_json::from_value(json!({ "promotion": p, "operation_id": op() })).unwrap()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let del = e.core.db.write(|c| Ok(c.execute("DELETE FROM promotions WHERE promotion_id=?1", [&id])?));
    assert!(del.is_err(), "promotions are archived, never deleted");
    // Ended → active is not a lifecycle move.
    let v = version(&e, &id);
    let err = e
        .core
        .promotions_set_status(
            t,
            serde_json::from_value(json!({ "promotion_id": id, "status": "active", "version": v, "operation_id": op() })).unwrap(),
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    set_status(&e, &id, "archived");
    assert!(one::<i64>(&e, "SELECT COUNT(*) FROM audit_logs WHERE entity_type='promotion'") >= 6);
}

#[test]
fn a_committed_sale_keeps_its_offer_when_the_offer_changes() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let milk = e.product("Milk 1L", "600011", 500, 300, 50_000);
    let id = activate(
        &e,
        json!({ "name": "Milk deal", "name_ar": "عرض الحليب", "kind": "fixed_price", "price_minor": 400, "buy_products": [milk] }),
    );
    e.core.pos_add_product(t, &milk, Some(3_000)).unwrap();
    let sale = pay(&e, t);
    assert_eq!(sale.total_minor, 1_200);
    let frozen: (String, String, i64) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT promotion_id, layer, amount_minor FROM sale_item_promotions", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?)
        })
        .unwrap();
    assert_eq!(frozen, (id.clone(), "item".to_string(), 300));
    assert_eq!(one::<i64>(&e, "SELECT promo_discount_minor FROM sale_items"), 300);
    assert_eq!(one::<i64>(&e, "SELECT discount_minor FROM sales"), 300);
    // Change the offer: the sale's record does not move.
    set_status(&e, &id, "paused");
    let mut p = e.core.promotions_get(t, &id).unwrap()["promotion"].clone();
    p["price_minor"] = json!(100);
    e.core.promotions_save(t, serde_json::from_value(json!({ "promotion": p, "operation_id": op() })).unwrap()).unwrap();
    assert_eq!(one::<i64>(&e, "SELECT amount_minor FROM sale_item_promotions"), 300);
    assert_eq!(one::<i64>(&e, "SELECT total_minor FROM sales"), 1_200);
    let upd = e.core.db.write(|c| Ok(c.execute("UPDATE sale_item_promotions SET amount_minor=0", [])?));
    assert!(upd.is_err(), "sale evidence is immutable");
    let r = e.core.promotions_get(t, &id).unwrap();
    assert_eq!(r["used"], true);
}

// ------------------------------------------------------------------ safety

#[test]
fn offers_are_validated_and_never_go_negative() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let gum = e.product("Gum", "600020", 150, 50, 50_000);
    let bad = |p: Value| {
        e.core.promotions_save(t, serde_json::from_value(json!({ "promotion": p, "operation_id": op() })).unwrap()).unwrap_err().code
    };
    assert_eq!(
        bad(json!({ "name": "x", "kind": "percent", "target": "items", "percent_bp": 0, "buy_products": [gum] })),
        ErrorCode::Validation
    );
    assert_eq!(
        bad(json!({ "name": "x", "kind": "percent", "target": "items", "percent_bp": 10_001, "buy_products": [gum] })),
        ErrorCode::Validation
    );
    assert_eq!(bad(json!({ "name": "x", "kind": "percent", "target": "items", "percent_bp": 500 })), ErrorCode::Validation);
    assert_eq!(
        bad(json!({ "name": "x", "kind": "percent", "target": "items", "percent_bp": 500, "buy_products": ["nope"] })),
        ErrorCode::Validation
    );
    assert_eq!(
        bad(json!({ "name": "x", "kind": "amount", "target": "items", "amount_minor": 100, "buy_products": [gum],
            "starts_at": "2026-05-02T00:00", "ends_at": "2026-05-01T00:00" })),
        ErrorCode::Validation
    );
    assert_eq!(bad(json!({ "name": "x", "kind": "script", "target": "all" })), ErrorCode::Validation);
    // An amount larger than the item makes it free, never negative, never cash out.
    activate(&e, json!({ "name": "Big", "kind": "amount", "amount_minor": 5_000, "buy_products": [gum] }));
    e.core.pos_add_product(t, &gum, Some(2_000)).unwrap();
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.totals.total_minor, 0);
    assert_eq!(cart.lines[0].promo_discount_minor, 300);
}

#[test]
fn only_permitted_people_manage_offers_and_saves_are_idempotent() {
    let e = env();
    let gum = e.product("Gum", "600021", 150, 50, 50_000);
    let (_, cashier) = e.user("Sara", "role_cashier", "1357");
    let req: amwapos_core::promo_admin::PromotionSave = serde_json::from_value(json!({
        "promotion": { "name": "x", "kind": "percent", "target": "items", "percent_bp": 500, "buy_products": [gum] },
        "operation_id": op()
    }))
    .unwrap();
    assert_eq!(e.core.promotions_save(&cashier, req.clone()).unwrap_err().code, ErrorCode::Forbidden);
    let (_, manager) = e.user("Ali", "role_manager", "2468");
    let a = e.core.promotions_save(&manager, req.clone()).unwrap();
    let b = e.core.promotions_save(&manager, req.clone()).unwrap();
    assert_eq!(a["promotion"]["promotion_id"], b["promotion"]["promotion_id"]);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM promotions"), 1);
    // The same operation id with a different request is refused.
    let mut other = req.clone();
    other.promotion.percent_bp = Some(600);
    assert_eq!(e.core.promotions_save(&manager, other).unwrap_err().code, ErrorCode::IdempotencyMismatch);
    // A stale version is refused.
    let mut p: amwapos_core::promotions::Promotion = serde_json::from_value(a["promotion"].clone()).unwrap();
    p.version -= 1;
    let stale = amwapos_core::promo_admin::PromotionSave { promotion: p, operation_id: op() };
    assert_eq!(e.core.promotions_save(&manager, stale).unwrap_err().code, ErrorCode::Conflict);
}

// ------------------------------------------------------------------ manual discounts and exclusions

#[test]
fn manual_discounts_and_overrides_never_double_up_with_offers() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let tea = e.product("Tea", "600030", 1_000, 600, 50_000);
    activate(&e, json!({ "name": "Tea 20%", "kind": "percent", "percent_bp": 2_000, "buy_products": [tea] }));
    let cart = e.core.pos_add_product(t, &tea, Some(1_000)).unwrap();
    assert_eq!(cart.totals.total_minor, 800);
    // A manual line discount replaces the offer on that line (never both).
    let line = cart.lines[0].line_id.clone();
    let cart = e.core.pos_line_discount(t, &line, 50, 0, None).unwrap();
    assert_eq!(cart.lines[0].promo_discount_minor, 0);
    assert_eq!(cart.lines[0].offer_excluded, Some("manual_discount"));
    assert_eq!(cart.totals.total_minor, 950);
    // Removing the manual discount brings the offer back.
    let cart = e.core.pos_line_discount(t, &line, 0, 0, None).unwrap();
    assert_eq!(cart.totals.total_minor, 800);
    // A cart discount applies on top, to what is left after offers.
    let cart = e.core.pos_cart_discount(t, 0, 1_000, None).unwrap();
    assert_eq!(cart.totals.total_minor, 720);
    // A cart discount larger than what is left after offers is refused.
    assert_eq!(e.core.pos_cart_discount(t, 900, 0, None).unwrap_err().code, ErrorCode::Validation);
}

#[test]
fn quantity_deal_groups_whole_units_in_any_line_order() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let soap = e.product("Soap", "600040", 400, 200, 50_000);
    activate(&e, json!({ "name": "3 for 1.000", "kind": "quantity", "buy_qty": 3, "price_minor": 1_000, "buy_products": [soap] }));
    e.core.pos_add_product(t, &soap, Some(7_000)).unwrap();
    let cart = e.core.pos_get_cart(t).unwrap();
    // Two groups at 1.000 plus one unit at 0.400.
    assert_eq!(cart.totals.total_minor, 2_400);
    assert_eq!(cart.promotions.applied[0].amount_minor, 400);
}

// ------------------------------------------------------------------ Buy-X-Get-Y, basket, conflicts and stacking

fn category(e: &Env, name: &str) -> String {
    e.core.category_save(&e.owner_token, None, name, None, 0).unwrap().category_id
}

fn in_category(e: &Env, pid: &str, cat: &str) {
    e.core.db.write(|c| Ok(c.execute("UPDATE products SET category_id=?2 WHERE product_id=?1", [pid, cat])?)).unwrap();
}

#[test]
fn buy_x_get_y_same_product_and_separate_reward_set() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let juice = e.product("Juice", "600050", 300, 150, 50_000);
    // Buy 2 get 1 free, same product: 7 units = 2 deals (6 units), 1 at full price.
    activate(&e, json!({ "name": "2+1 Juice", "kind": "bxgy", "buy_qty": 2, "get_qty": 1, "buy_products": [juice] }));
    e.core.pos_add_product(t, &juice, Some(7_000)).unwrap();
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.promotions.applied[0].amount_minor, 600);
    assert_eq!(cart.totals.total_minor, 1_500);
    e.core.pos_cancel_sale(t, None).unwrap();
    // Separate reward set at 50%, at most once: buy 2 chips, get a dip half price.
    let chips = e.product("Chips", "600051", 250, 100, 50_000);
    let dip = e.product("Dip", "600052", 400, 200, 50_000);
    activate(
        &e,
        json!({ "name": "Chips & dip", "kind": "bxgy", "buy_qty": 2, "get_qty": 1, "percent_bp": 5_000, "max_uses": 1,
            "buy_products": [chips], "get_products": [dip], "priority": 5 }),
    );
    e.core.pos_add_product(t, &dip, Some(2_000)).unwrap();
    e.core.pos_add_product(t, &chips, Some(1_000)).unwrap();
    let cart = e.core.pos_get_cart(t).unwrap();
    let ex = cart.promotions.explain.iter().find(|x| x.name == "Chips & dip").unwrap();
    assert_eq!(ex.message, "Requires 2 + 1 items.");
    e.core.pos_add_product(t, &chips, Some(1_000)).unwrap();
    let cart = e.core.pos_get_cart(t).unwrap();
    let a = cart.promotions.applied.iter().find(|x| x.name == "Chips & dip").unwrap();
    assert_eq!(a.amount_minor, 200, "one dip at half price, once");
}

#[test]
fn basket_threshold_is_explicit_and_conflicts_resolve_by_priority_benefit_and_id() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let a = e.product("Coffee", "600060", 3_000, 2_000, 50_000);
    let b = e.product("Sugar", "600061", 1_000, 700, 50_000);
    // Basket: 10% off when the eligible lines come to at least 5.000.
    activate(&e, json!({ "name": "Spend 5", "kind": "basket", "target": "all", "threshold_minor": 5_000, "percent_bp": 1_000 }));
    e.core.pos_add_product(t, &a, Some(1_000)).unwrap();
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.promotions.explain[0].message, "Requires a basket of at least 5.000.");
    e.core.pos_add_product(t, &b, Some(2_000)).unwrap();
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.promotions.applied[0].amount_minor, 500);
    // The allocation over the lines sums exactly.
    assert_eq!(cart.lines.iter().map(|l| l.promo_discount_minor).sum::<i64>(), 500);
    e.core.pos_cancel_sale(t, None).unwrap();

    // Two item offers on coffee: the higher priority wins even when smaller.
    let low = activate(&e, json!({ "name": "Coffee 30%", "kind": "percent", "percent_bp": 3_000, "buy_products": [a] }));
    let high = activate(&e, json!({ "name": "Coffee 10%", "kind": "percent", "percent_bp": 1_000, "buy_products": [a], "priority": 10 }));
    e.core.pos_add_product(t, &a, Some(1_000)).unwrap();
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.lines[0].offers, vec!["Coffee 10%".to_string()]);
    let ex = cart.promotions.explain.iter().find(|x| x.promotion_id == low).unwrap();
    assert_eq!(ex.message, "Another higher-priority offer was used.");
    // Equal priority: the better offer for the customer wins.
    set_status(&e, &high, "paused");
    activate(&e, json!({ "name": "Coffee 20%", "kind": "percent", "percent_bp": 2_000, "buy_products": [a] }));
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.lines[0].offers, vec!["Coffee 30%".to_string()]);
    // Non-stackable item offer: the basket offer skips that line (coffee 3.000
    // with 30% = 2.100; sugar ×3 = 3.000 alone reaches 5.000? no: 3.000 < 5.000).
    e.core.pos_add_product(t, &b, Some(3_000)).unwrap();
    let cart = e.core.pos_get_cart(t).unwrap();
    assert!(cart.promotions.applied.iter().all(|x| x.name != "Spend 5"));
    let ex = cart.promotions.explain.iter().find(|x| x.name == "Spend 5").unwrap();
    assert_eq!(ex.reason, "threshold");
}

#[test]
fn stacking_requires_both_sides_to_allow_it() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let a = e.product("Honey", "600070", 5_000, 3_000, 50_000);
    let item = activate(&e, json!({ "name": "Honey 10%", "kind": "percent", "percent_bp": 1_000, "buy_products": [a], "stackable": true }));
    let basket =
        activate(&e, json!({ "name": "Spend 4", "kind": "basket", "target": "all", "threshold_minor": 4_000, "amount_minor": 500 }));
    e.core.pos_add_product(t, &a, Some(1_000)).unwrap();
    // The basket offer does not stack: the honey line already has an offer.
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.totals.total_minor, 4_500);
    // Both stackable: 5.000 − 0.500 (10%) = 4.500 ≥ 4.000, then 0.500 off.
    set_status(&e, &basket, "paused");
    let mut p = e.core.promotions_get(t, &basket).unwrap()["promotion"].clone();
    p["stackable"] = json!(true);
    e.core.promotions_save(t, serde_json::from_value(json!({ "promotion": p, "operation_id": op() })).unwrap()).unwrap();
    set_status(&e, &basket, "active");
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.totals.total_minor, 4_000);
    assert_eq!(cart.lines[0].offers, vec!["Honey 10%".to_string(), "Spend 4".to_string()]);
    // The item offer stops stacking: the basket offer leaves the line again.
    set_status(&e, &item, "paused");
    let mut p = e.core.promotions_get(t, &item).unwrap()["promotion"].clone();
    p["stackable"] = json!(false);
    e.core.promotions_save(t, serde_json::from_value(json!({ "promotion": p, "operation_id": op() })).unwrap()).unwrap();
    set_status(&e, &item, "active");
    assert_eq!(e.core.pos_get_cart(t).unwrap().totals.total_minor, 4_500);
}

#[test]
fn categories_scale_labels_and_determinism() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let dairy = category(&e, "Dairy");
    let yog = e.product("Yoghurt", "600080", 350, 200, 50_000);
    let lab = e.product("Laban", "600081", 250, 150, 50_000);
    in_category(&e, &yog, &dairy);
    in_category(&e, &lab, &dairy);
    activate(&e, json!({ "name": "Dairy 3 for 0.900", "kind": "quantity", "buy_qty": 3, "price_minor": 900, "buy_categories": [dairy] }));
    // Order A: laban ×2 then yoghurt ×2; order B: the reverse. Same result.
    e.core.pos_add_product(t, &lab, Some(2_000)).unwrap();
    e.core.pos_add_product(t, &yog, Some(2_000)).unwrap();
    let a = e.core.pos_get_cart(t).unwrap();
    e.core.pos_cancel_sale(t, None).unwrap();
    e.core.pos_add_product(t, &yog, Some(2_000)).unwrap();
    e.core.pos_add_product(t, &lab, Some(2_000)).unwrap();
    let b = e.core.pos_get_cart(t).unwrap();
    assert_eq!(a.totals, b.totals);
    assert_eq!(a.promotions.applied[0].amount_minor, b.promotions.applied[0].amount_minor);
    // The dearest three units form the group: 0.350+0.350+0.250 = 0.950 → 0.900.
    assert_eq!(a.promotions.applied[0].amount_minor, 50);
    e.core.pos_cancel_sale(t, None).unwrap();

    // Scale labels: a price label never takes an ordinary offer; a weight label does.
    let lamb = {
        let req: amwapos_core::catalog::ProductCreate = serde_json::from_value(json!({
            "name": "Lamb", "tax_rule_id": e.tax_rule(), "unit": "kg", "track_inventory": true,
            "allow_decimal_quantity": true, "reorder_point_milli": 0, "is_favorite": false,
            "price_minor": 5_000, "cost_minor": 3_000, "barcodes": [], "opening_stock_milli": 20_000, "plu": "77"
        }))
        .unwrap();
        e.core.product_create(t, req).unwrap().row.product_id
    };
    activate(&e, json!({ "name": "Lamb 10%", "kind": "percent", "percent_bp": 1_000, "buy_products": [lamb] }));
    let rule = |name: &str, prefix: &str, kind: &str| -> amwapos_core::barcodes::ScaleRule {
        serde_json::from_value(json!({
            "name": name, "prefix": prefix, "length": 13, "item_start": 3, "item_length": 5,
            "value_kind": kind, "value_start": 8, "value_length": 5, "decimals": 3, "check_digit": "ean", "priority": 0
        }))
        .unwrap()
    };
    e.core.scale_rule_save(t, rule("Price", "22", "price")).unwrap();
    e.core.scale_rule_save(t, rule("Weight", "21", "weight")).unwrap();
    let ean = |body: &str| (0..10).map(|d| format!("{body}{d}")).find(|c| amwapos_core::barcodes::gs1_check_ok(c)).unwrap();
    let cart = e.core.pos_scan(t, &ean("220007702345"), None).unwrap().cart;
    assert_eq!(cart.lines[0].offer_excluded, Some("scale_price"));
    assert_eq!(cart.totals.total_minor, 2_345, "the printed price is the price");
    let ex = cart.promotions.explain.iter().find(|x| x.name == "Lamb 10%").unwrap();
    assert_eq!(ex.message, "This item came from a fixed-price scale label.");
    let cart = e.core.pos_scan(t, &ean("210007701000"), None).unwrap().cart;
    let w = cart.lines.iter().find(|l| l.offer_excluded.is_none()).unwrap();
    assert_eq!(w.promo_discount_minor, 500, "1.000 kg at 5.000 with 10% off");
}

#[test]
fn mixed_vat_basket_allocation_sums_exactly_and_vat_is_per_line() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let zero = e
        .core
        .db
        .write(|c| {
            let id = amwapos_core::ids::new_id();
            c.execute(
                "INSERT INTO tax_rules(tax_rule_id, name, rate_bp, inclusive, active, effective_from, created_at) VALUES (?1,'Zero',0,1,1,'2020-01-01','2020-01-01')",
                [&id],
            )?;
            Ok(id)
        })
        .unwrap();
    let std_ = e.product("Shampoo", "600090", 1_333, 800, 50_000);
    let bread = e.product("Bread", "600091", 777, 500, 50_000);
    e.core.db.write(|c| Ok(c.execute("UPDATE products SET tax_rule_id=?2 WHERE product_id=?1", [&bread, &zero])?)).unwrap();
    activate(&e, json!({ "name": "Spend 2", "kind": "basket", "target": "all", "threshold_minor": 2_000, "amount_minor": 1_001 }));
    e.core.pos_add_product(t, &std_, Some(1_000)).unwrap();
    e.core.pos_add_product(t, &bread, Some(1_000)).unwrap();
    let cart = e.core.pos_get_cart(t).unwrap();
    let promo: i64 = cart.lines.iter().map(|l| l.promo_discount_minor).sum();
    assert_eq!(promo, 1_001);
    let bread_line = cart.lines.iter().find(|l| l.name == "Bread").unwrap();
    assert_eq!(bread_line.tax_minor, 0, "zero-rated stays zero after the offer");
    let sale = pay(&e, t);
    let (disc, tax, total): (i64, i64, i64) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT discount_minor, tax_minor, total_minor FROM sales WHERE sale_id=?1", [&sale.sale_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?)
        })
        .unwrap();
    assert_eq!(disc, 1_001);
    assert_eq!(total, 1_333 + 777 - 1_001);
    let line_tax: i64 = one(&e, &format!("SELECT SUM(tax_minor) FROM sale_items WHERE sale_id='{}'", sale.sale_id));
    assert_eq!(tax, line_tax);
    let sip: i64 = one(&e, &format!("SELECT SUM(amount_minor) FROM sale_item_promotions WHERE sale_id='{}'", sale.sale_id));
    assert_eq!(sip, 1_001);
}

// ------------------------------------------------------------------ coupons

fn coupon(e: &Env, promotion_id: &str, code: &str, kind: &str, max: Option<i64>) -> Result<Value, amwapos_core::AppError> {
    e.core.coupons_save(
        &e.owner_token,
        serde_json::from_value(
            json!({ "promotion_id": promotion_id, "code": code, "kind": kind, "max_redemptions": max, "operation_id": op() }),
        )
        .unwrap(),
    )
}

fn as_terminal(e: &Env) {
    e.core
        .db
        .write(|c| Ok(c.execute("UPDATE settings SET value_json=json_set(value_json,'$.mode','terminal') WHERE key='local.device'", [])?))
        .unwrap();
}

#[test]
fn a_reusable_coupon_previews_holds_and_redeems_only_at_commit() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let tea = e.product("Tea", "600100", 2_500, 1_500, 50_000);
    let pid = activate(
        &e,
        json!({ "name": "Coupon 10%", "kind": "basket", "buy_products": [tea], "threshold_minor": 1_000, "percent_bp": 1_000,
        "requires_coupon": true }),
    );
    // Codes only for coupon offers; codes are unique after normalisation.
    let plain = activate(&e, json!({ "name": "Plain", "kind": "percent", "percent_bp": 100, "buy_products": [tea] }));
    assert_eq!(coupon(&e, &plain, "X1", "reusable", None).unwrap_err().code, ErrorCode::Validation);
    set_status(&e, &plain, "paused");
    coupon(&e, &pid, "Save10", "reusable", None).unwrap();
    assert_eq!(coupon(&e, &pid, " save10", "reusable", None).unwrap_err().code, ErrorCode::Duplicate);
    let (_, cashier) = e.user("Sara", "role_cashier", "1357");
    let req = serde_json::from_value(json!({ "promotion_id": pid, "code": "C2", "kind": "reusable", "operation_id": op() })).unwrap();
    assert_eq!(e.core.coupons_save(&cashier, req).unwrap_err().code, ErrorCode::Forbidden);

    e.core.pos_add_product(t, &tea, Some(1_000)).unwrap();
    // Without the coupon: the offer explains itself.
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.promotions.explain.iter().find(|x| x.promotion_id == pid).unwrap().message, "Coupon is required.");
    // An unknown code never blocks the sale.
    let cart = e.core.pos_set_coupon(t, Some("nope".into())).unwrap();
    assert_eq!(cart.promotions.coupon.as_ref().unwrap().status, "invalid");
    assert_eq!(cart.totals.total_minor, 2_500);
    let cart = e.core.pos_set_coupon(t, Some(" save 10 ".into())).unwrap();
    let cs = cart.promotions.coupon.clone().unwrap();
    assert_eq!((cs.status.as_str(), cs.code.as_str(), cs.saved_minor), ("applied", "SAVE10", 250));
    assert_eq!(cart.totals.total_minor, 2_250);
    assert!(cart.promotions.explain.iter().all(|x| x.promotion_id != pid));
    // A preview and a held sale redeem nothing; the held sale keeps the coupon.
    e.core.pos_hold(t, None).unwrap();
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM coupon_redemptions"), 0);
    let held = e.core.pos_held_list(t).unwrap();
    assert_eq!(held[0].total_minor, 2_250);
    let back = e.core.pos_restore(t, &held[0].cart_id).unwrap();
    assert_eq!(back.coupon_code.as_deref(), Some("SAVE10"));
    assert_eq!(back.totals.total_minor, 2_250);
    let sale = pay(&e, t);
    let (code, amount, sale_id, opid): (String, i64, String, String) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT code, amount_minor, sale_id, operation_id FROM coupon_redemptions", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?)
        })
        .unwrap();
    assert_eq!((code.as_str(), amount, sale_id.as_str()), ("SAVE10", 250, sale.sale_id.as_str()));
    assert!(!opid.is_empty());
    assert_eq!(one::<String>(&e, "SELECT coupon_code FROM sale_item_promotions"), "SAVE10");
    // Reusable: works again, and offline on a till.
    as_terminal(&e);
    e.core.pos_add_product(t, &tea, Some(1_000)).unwrap();
    let cart = e.core.pos_set_coupon(t, Some("SAVE10".into())).unwrap();
    assert_eq!(cart.promotions.coupon.unwrap().status, "applied");
}

#[test]
fn a_limited_coupon_needs_the_main_computer_and_is_used_once() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let tea = e.product("Tea", "600110", 2_000, 1_000, 50_000);
    let pid =
        activate(&e, json!({ "name": "Welcome", "kind": "percent", "percent_bp": 5_000, "buy_products": [tea], "requires_coupon": true }));
    assert_eq!(coupon(&e, &pid, "ONE", "limited", None).unwrap_err().code, ErrorCode::Validation);
    coupon(&e, &pid, "ONE", "limited", Some(1)).unwrap();
    // A sale previews the code, then waits on hold.
    e.core.pos_add_product(t, &tea, Some(1_000)).unwrap();
    let preview = e.core.pos_set_coupon(t, Some("one".into())).unwrap();
    assert_eq!(preview.totals.total_minor, 1_000);
    let held_cart = preview.cart_id.clone().unwrap();
    e.core.pos_hold(t, None).unwrap();
    // Another sale takes the only use.
    e.core.pos_add_product(t, &tea, Some(1_000)).unwrap();
    e.core.pos_set_coupon(t, Some("ONE".into())).unwrap();
    pay(&e, t);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM coupon_redemptions"), 1);
    // The held sale is checked again on restore; paying the previewed total
    // is refused, paying the full price works (the coupon never blocks).
    let cart = e.core.pos_restore(t, &held_cart).unwrap();
    assert!(cart.notices.iter().any(|n| n == "Coupon ONE: This coupon has already been used."), "{:?}", cart.notices);
    let cs = cart.promotions.coupon.clone().unwrap();
    assert_eq!(cs.status, "already_used");
    let stale = e.core.pos_finalize(
        t,
        FinalizeRequest {
            cart_id: held_cart.clone(),
            operation_id: op(),
            tenders: vec![TenderInput { method: "cash".into(), amount_minor: 1_000, reference: None }],
            approval_token: None,
            expected_total_minor: Some(1_000),
            fulfilment: None,
        },
    );
    assert_eq!(stale.unwrap_err().code, ErrorCode::Conflict);
    let sale = pay(&e, t);
    assert_eq!(sale.total_minor, 2_000);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM coupon_redemptions"), 1);
    // On a till (offline from the main computer) a limited code is never accepted.
    as_terminal(&e);
    e.core.pos_add_product(t, &tea, Some(1_000)).unwrap();
    let cs = e.core.pos_set_coupon(t, Some("ONE".into())).unwrap().promotions.coupon.unwrap();
    assert_eq!((cs.status.as_str(), cs.message.as_str()), ("needs_main", "This coupon needs the main computer to verify it."));
}

#[test]
fn coupon_states_are_truthful() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let tea = e.product("Tea", "600120", 2_000, 1_000, 50_000);
    let gum = e.product("Gum", "600121", 100, 50, 50_000);
    let soon = activate(
        &e,
        json!({ "name": "Soon", "kind": "percent", "percent_bp": 1_000, "buy_products": [tea],
        "requires_coupon": true, "starts_at": "2099-01-01T00:00" }),
    );
    let past = activate(
        &e,
        json!({ "name": "Past", "kind": "percent", "percent_bp": 1_000, "buy_products": [tea],
        "requires_coupon": true, "starts_at": "2020-01-01T00:00", "ends_at": "2020-02-01T00:00" }),
    );
    let wa = activate(
        &e,
        json!({ "name": "WA", "kind": "percent", "percent_bp": 1_000, "buy_products": [tea],
        "requires_coupon": true, "channels": ["whatsapp"] }),
    );
    let off =
        activate(&e, json!({ "name": "Off", "kind": "percent", "percent_bp": 1_000, "buy_products": [tea], "requires_coupon": true }));
    for (p, code) in [(&soon, "SOON"), (&past, "PAST"), (&wa, "WA"), (&off, "OFF")] {
        coupon(&e, p, code, "reusable", None).unwrap();
    }
    let off_coupon: Value = e.core.promotions_get(t, &off).unwrap()["coupons"][0].clone();
    e.core
        .coupons_save(
            t,
            serde_json::from_value(json!({ "coupon_id": off_coupon["coupon_id"], "promotion_id": off, "code": "OFF", "kind": "reusable",
                "active": false, "version": off_coupon["version"], "operation_id": op() }))
            .unwrap(),
        )
        .unwrap();
    e.core.pos_add_product(t, &gum, Some(1_000)).unwrap();
    let state = |code: &str| e.core.pos_set_coupon(t, Some(code.into())).unwrap().promotions.coupon.unwrap().status;
    assert_eq!(state("SOON"), "not_started");
    assert_eq!(state("PAST"), "expired");
    assert_eq!(state("WA"), "wrong_channel");
    assert_eq!(state("OFF"), "inactive");
    e.core.pos_set_channel(t, "whatsapp").unwrap();
    assert_eq!(state("WA"), "not_eligible", "only gum in the basket");
    let cart = e.core.pos_set_coupon(t, None).unwrap();
    assert!(cart.promotions.coupon.is_none() && cart.coupon_code.is_none());
}

// ------------------------------------------------------------------ bundles

fn zero_rate(e: &Env) -> String {
    e.core
        .db
        .write(|c| {
            let id = amwapos_core::ids::new_id();
            c.execute(
                "INSERT INTO tax_rules(tax_rule_id, name, rate_bp, inclusive, active, effective_from, created_at) VALUES (?1,'Zero',0,1,1,'2020-01-01','2020-01-01')",
                [&id],
            )?;
            Ok(id)
        })
        .unwrap()
}

fn bundle(e: &Env, parent: &str, comps: &[(&str, i64)], version: i64) -> Result<Value, amwapos_core::AppError> {
    let comps: Vec<Value> = comps.iter().map(|(p, q)| json!({ "product_id": p, "qty_milli": q })).collect();
    e.core.bundles_save(
        &e.owner_token,
        serde_json::from_value(json!({ "bundle_product_id": parent, "components": comps, "version": version, "operation_id": op() }))
            .unwrap(),
    )
}

fn stock(e: &Env, pid: &str) -> i64 {
    one(e, &format!("SELECT COALESCE((SELECT qty_milli FROM stock_levels WHERE product_id='{pid}'),0)"))
}

/// Family hamper: 2 rice (2.750, standard VAT) + 3 dates (1.000, zero rate), sold at 7.500.
fn hamper(e: &Env) -> (String, String, String) {
    let rice = e.product("Rice 5kg", "600200", 2_750, 2_000, 10_000);
    let dates = e.product("Dates 1kg", "600201", 1_000, 600, 9_000);
    let z = zero_rate(e);
    e.core.db.write(|c| Ok(c.execute("UPDATE products SET tax_rule_id=?2 WHERE product_id=?1", [&dates, &z])?)).unwrap();
    let parent = e.product("Family Hamper", "600209", 7_500, 0, 0);
    bundle(e, &parent, &[(&rice, 2_000), (&dates, 3_000)], 0).unwrap();
    (parent, rice, dates)
}

#[test]
fn a_bundle_sells_from_its_components_with_exact_money_vat_and_cost() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let (parent, rice, dates) = hamper(&e);
    let b = e.core.bundles_get(t, &parent).unwrap();
    assert_eq!(b["normal_minor"], 8_500);
    assert_eq!(b["saving_minor"], 1_000);
    assert_eq!(b["available"], 3, "dates limit it: 9 / 3");
    assert_eq!(b["limited_by"], "Dates 1kg");
    assert_eq!(b["cost_minor"], 2 * 2_000 + 3 * 600);
    // The parent has no stock of its own.
    assert_eq!(one::<i64>(&e, &format!("SELECT track_inventory FROM products WHERE product_id='{parent}'")), 0);

    e.core.pos_add_product(t, &parent, Some(2_000)).unwrap();
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.totals.total_minor, 15_000);
    // VAT: only the rice share (11.000 of 17.000 normal value) carries 10%.
    let rice_net = 15_000 * 11_000 / 17_000; // 9705.88 → allocator gives 9706
    assert!((cart.totals.tax_minor - (rice_net * 1_000 / 11_000)).abs() <= 1, "tax {}", cart.totals.tax_minor);
    let sale = pay(&e, t);
    // Two component lines, summing exactly to the bundle line.
    let (n, total, tax, cost): (i64, i64, i64, i64) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*), SUM(line_total_minor), SUM(tax_minor), SUM(cost_snapshot_minor) FROM sale_items WHERE sale_id=?1",
                [&sale.sale_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?)
        })
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(total, 15_000);
    assert_eq!(tax, cart.totals.tax_minor);
    assert_eq!(cost, 2 * (2 * 2_000 + 3 * 600));
    let zero_tax: i64 = one(&e, &format!("SELECT tax_minor FROM sale_items WHERE product_id='{dates}'"));
    assert_eq!(zero_tax, 0);
    assert_eq!(one::<String>(&e, "SELECT bundle_name FROM sale_items LIMIT 1"), "Family Hamper");
    // Stock: components out, parent untouched, no double movement.
    assert_eq!(stock(&e, &rice), 10_000 - 4_000);
    assert_eq!(stock(&e, &dates), 9_000 - 6_000);
    assert_eq!(one::<i64>(&e, &format!("SELECT COUNT(*) FROM stock_movements WHERE product_id='{parent}'")), 0);
    assert_eq!(one::<i64>(&e, &format!("SELECT COUNT(*) FROM stock_movements WHERE source_id='{}'", sale.sale_id)), 2);
    assert_eq!(e.core.bundles_get(t, &parent).unwrap()["available"], 1);
}

#[test]
fn bundles_refund_whole_from_the_frozen_split_and_restore_component_stock() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let (parent, rice, dates) = hamper(&e);
    e.core.pos_add_product(t, &parent, Some(2_000)).unwrap();
    let sale = pay(&e, t);
    // The prices change afterwards; the refund uses what was sold.
    e.core.product_price_update(t, &rice, 9_000, None, None).unwrap();
    let items: Vec<(String, String, i64)> = e
        .core
        .db
        .read(|c| {
            let mut st =
                c.prepare("SELECT sale_item_id, product_id, line_total_minor FROM sale_items WHERE sale_id=?1 ORDER BY product_id")?;
            let v = st.query_map([&sale.sale_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(v)
        })
        .unwrap();
    let qty_of = |pid: &str, bundles: i64| if pid == rice { 2 * bundles } else { 3 * bundles };
    let req = |lines: Vec<Value>| -> amwapos_core::refunds::RefundRequest {
        serde_json::from_value(json!({ "sale_id": sale.sale_id, "lines": lines, "reason": "Returned", "operation_id": op() })).unwrap()
    };
    // Only the rice of a hamper: refused.
    let rice_item = items.iter().find(|x| x.1 == rice).unwrap();
    let err = e.core.refund_create(t, req(vec![json!({ "sale_item_id": rice_item.0, "qty_milli": 2_000 })])).unwrap_err();
    assert_eq!(err.code, ErrorCode::Validation);
    assert!(err.message.contains("Family Hamper"), "{}", err.message);
    // Uneven: refused.
    let lines: Vec<Value> =
        items.iter().map(|x| json!({ "sale_item_id": x.0, "qty_milli": if x.1 == rice { 2_000 } else { 6_000 } })).collect();
    assert_eq!(e.core.refund_create(t, req(lines)).unwrap_err().code, ErrorCode::Validation);
    // One whole hamper, then the other: exact, and stock comes back per component.
    let one_hamper: Vec<Value> = items.iter().map(|x| json!({ "sale_item_id": x.0, "qty_milli": qty_of(&x.1, 1) * 1_000 })).collect();
    let r1 = e.core.refund_create(t, req(one_hamper.clone())).unwrap();
    let r2 = e.core.refund_create(t, req(one_hamper)).unwrap();
    assert_eq!(r1.total_minor + r2.total_minor, 15_000);
    assert_eq!(stock(&e, &rice), 10_000);
    assert_eq!(stock(&e, &dates), 9_000);
}

#[test]
fn bundle_versions_holds_offers_and_safety() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let (parent, rice, dates) = hamper(&e);
    // A bundle can take a promotion; it splits over the components.
    activate(&e, json!({ "name": "Hamper week", "kind": "amount", "amount_minor": 500, "buy_products": [parent] }));
    e.core.pos_add_product(t, &parent, Some(1_000)).unwrap();
    assert_eq!(e.core.pos_get_cart(t).unwrap().totals.total_minor, 7_000);
    e.core.pos_hold(t, None).unwrap();
    // The bundle changes while the sale is held: restore applies the new contents, with a notice.
    let oil = e.product("Oil", "600210", 1_500, 1_000, 10_000);
    bundle(&e, &parent, &[(&rice, 2_000), (&dates, 3_000), (&oil, 1_000)], 1).unwrap();
    let held = e.core.pos_held_list(t).unwrap();
    let back = e.core.pos_restore(t, &held[0].cart_id).unwrap();
    assert!(back.notices.iter().any(|n| n.contains("contents changed")), "{:?}", back.notices);
    let sale = pay(&e, t);
    assert_eq!(one::<i64>(&e, &format!("SELECT COUNT(*) FROM sale_items WHERE sale_id='{}'", sale.sale_id)), 3);
    assert_eq!(one::<i64>(&e, &format!("SELECT MAX(bundle_version) FROM sale_items WHERE sale_id='{}'", sale.sale_id)), 2);
    let sip: i64 = one(&e, &format!("SELECT SUM(amount_minor) FROM sale_item_promotions WHERE sale_id='{}'", sale.sale_id));
    assert_eq!(sip, 500);
    // Version 1 is still there, unchanged.
    assert_eq!(one::<i64>(&e, &format!("SELECT COUNT(*) FROM bundle_components WHERE bundle_product_id='{parent}' AND version=1")), 2);
    // Unsafe set-ups are refused.
    let kg: amwapos_core::catalog::ProductCreate = serde_json::from_value(json!({
        "name": "Cheese", "tax_rule_id": e.tax_rule(), "unit": "kg", "track_inventory": true,
        "allow_decimal_quantity": true, "reorder_point_milli": 0, "is_favorite": false,
        "price_minor": 4_000, "cost_minor": 2_000, "barcodes": [], "opening_stock_milli": 5_000
    }))
    .unwrap();
    let cheese = e.core.product_create(t, kg).unwrap().row.product_id;
    let other = e.product("Gift box", "600211", 5_000, 0, 0);
    assert_eq!(bundle(&e, &other, &[(&cheese, 1_000)], 0).unwrap_err().code, ErrorCode::Validation, "weighed component");
    assert_eq!(bundle(&e, &other, &[(&parent, 1_000)], 0).unwrap_err().code, ErrorCode::Validation, "nested bundle");
    assert_eq!(bundle(&e, &other, &[(&rice, 1_500)], 0).unwrap_err().code, ErrorCode::Validation, "part unit");
    assert_eq!(bundle(&e, &rice, &[(&dates, 1_000)], 0).unwrap_err().code, ErrorCode::Validation, "a stocked product");
    let (_, cashier) = e.user("Sara", "role_cashier", "1357");
    let req = serde_json::from_value(json!({ "bundle_product_id": other, "components": [{ "product_id": rice, "qty_milli": 1_000 }],
        "operation_id": op() }))
    .unwrap();
    assert_eq!(e.core.bundles_save(&cashier, req).unwrap_err().code, ErrorCode::Forbidden);
    // Not enough components: the negative-stock rule applies to them (this
    // store allows selling below zero, with a warning naming the component).
    e.core.pos_add_product(t, &parent, Some(5_000)).unwrap();
    let sale = pay(&e, t);
    assert!(sale.stock_warnings.iter().any(|w| w.contains("Dates 1kg")), "{:?}", sale.stock_warnings);
}

// ------------------------------------------------------------------ receipts

#[test]
fn receipts_show_savings_by_name_and_bundles_as_one_line() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let (parent, _, _) = hamper(&e);
    let milk = e.product("Milk", "600300", 2_500, 1_500, 50_000);
    let weekend = activate(&e, json!({ "name": "Weekend offer", "kind": "amount", "amount_minor": 500, "buy_products": [milk] }));
    let cp = activate(
        &e,
        json!({ "name": "Ten percent", "kind": "basket", "target": "items", "buy_products": [milk], "threshold_minor": 1_000,
        "percent_bp": 1_250, "requires_coupon": true, "stackable": true }),
    );
    // The coupon reaches the milk line only if the weekend offer stacks too.
    let mut p = e.core.promotions_get(t, &weekend).unwrap()["promotion"].clone();
    set_status(&e, &weekend, "paused");
    p["stackable"] = json!(true);
    p["version"] = json!(version(&e, &weekend));
    e.core.promotions_save(t, serde_json::from_value(json!({ "promotion": p, "operation_id": op() })).unwrap()).unwrap();
    set_status(&e, &weekend, "active");
    coupon(&e, &cp, "SAVE10", "reusable", None).unwrap();
    e.core.pos_add_product(t, &milk, Some(1_000)).unwrap();
    e.core.pos_add_product(t, &parent, Some(1_000)).unwrap();
    e.core.pos_set_coupon(t, Some("save10".into())).unwrap();
    let sale = pay(&e, t);
    // 2.500 − 0.500 = 2.000; 12.5% of 2.000 = 0.250; hamper 7.500.
    assert_eq!(sale.total_minor, 2_000 - 250 + 7_500);
    let text = e.core.db.read(|c| amwapos_core::receipt::sale_receipt(c, &sale.sale_id, None)).unwrap().to_text();
    assert!(text.contains("Weekend offer"), "{text}");
    assert!(text.contains("-0.500"), "{text}");
    assert!(text.contains("Coupon SAVE10"), "{text}");
    assert!(text.contains("-0.250"), "{text}");
    assert!(text.contains("Family Hamper"), "{text}");
    assert!(text.contains("7.500"), "{text}");
    assert!(text.contains("You saved"), "{text}");
    assert!(!text.contains(&weekend) && !text.contains(&cp), "no internal ids on a receipt");
    // The issued receipt is frozen.
    let issued = e.core.db.read(|c| amwapos_core::receipt::issued(c, "sale", &sale.sale_id)).unwrap();
    assert!(issued.exact && issued.doc.to_text().contains("Coupon SAVE10"));
}

// ------------------------------------------------------------------ reports, dashboard

#[test]
fn reports_and_attention_are_truthful() {
    let e = env();
    let t = &e.owner_token;
    ready(&e);
    let (parent, _, dates) = hamper(&e);
    let milk = e.product("Milk", "600400", 1_000, 1_200, 50_000); // cost above price
    let below = activate(&e, json!({ "name": "Milk 10%", "kind": "percent", "percent_bp": 1_000, "buy_products": [milk] }));
    let cp = activate(
        &e,
        json!({ "name": "Once", "kind": "percent", "percent_bp": 500, "buy_products": [milk], "requires_coupon": true,
        "stackable": true }),
    );
    let mut p = e.core.promotions_get(t, &below).unwrap()["promotion"].clone();
    set_status(&e, &below, "paused");
    p["stackable"] = json!(true);
    p["version"] = json!(version(&e, &below));
    e.core.promotions_save(t, serde_json::from_value(json!({ "promotion": p, "operation_id": op() })).unwrap()).unwrap();
    set_status(&e, &below, "active");
    coupon(&e, &cp, "ONCE", "limited", Some(1)).unwrap();
    e.core.pos_add_product(t, &milk, Some(2_000)).unwrap();
    e.core.pos_add_product(t, &parent, Some(3_000)).unwrap();
    e.core.pos_set_coupon(t, Some("ONCE".into())).unwrap();
    pay(&e, t);
    let run = |k: &str| e.core.report_run(t, k, Default::default()).unwrap();
    let promos = run("promotions");
    let milk_row = promos.rows.iter().find(|r| r["name"] == "Milk 10%").unwrap();
    assert_eq!(milk_row["discount"], 200);
    assert_eq!(milk_row["sales"], 1);
    assert!(promos.notes.iter().any(|n| n.contains("do not say what would have sold")));
    let coupons = run("coupons");
    assert_eq!(coupons.rows[0]["code"], "ONCE");
    assert_eq!(coupons.rows[0]["left"], 0);
    let bundles = run("bundles");
    assert_eq!(bundles.rows[0]["units"], 3_000);
    assert_eq!(bundles.rows[0]["cost"], 3 * (2 * 2_000 + 3 * 600));
    assert!(bundles.rows[0]["items_used"].as_str().unwrap().contains("Dates 1kg × 9"));
    // The dashboard: below cost, coupon used up, bundle that stock cannot make (dates: 9 − 9 = 0).
    assert_eq!(stock(&e, &dates), 0);
    let att = e.core.promotions_attention(t).unwrap();
    let kinds: Vec<&str> = att["items"].as_array().unwrap().iter().map(|x| x["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"below_cost"), "{kinds:?}");
    assert!(kinds.contains(&"coupon_used_up"), "{kinds:?}");
    assert!(kinds.contains(&"bundle_unavailable"), "{kinds:?}");
    assert!(att["items"].as_array().unwrap().iter().all(|x| x["link"].as_str().unwrap().starts_with("/admin/")));
    // A cashier sees none of it.
    let (_, cashier) = e.user("Sara", "role_cashier", "1357");
    assert_eq!(e.core.promotions_attention(&cashier).unwrap()["items"].as_array().unwrap().len(), 0);
}
