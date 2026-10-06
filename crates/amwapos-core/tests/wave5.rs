//! Wave 5 of the merchant operating system: the retail commercial
//! foundation. Barcode kinds, PLUs, scale barcodes, duplicate review and
//! product merge, structured addresses, the sales channel, channel prices,
//! pricing policies, rounding and margin protection
//! (docs/PRICING_AND_CATALOGUE.md).

mod common;

use amwapos_core::barcodes::ScaleRule;
use amwapos_core::catalog::{ProductCreate, ProductInput};
use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::{FinalizeRequest, SaleResult};
use amwapos_core::ErrorCode;
use common::*;
use serde_json::json;

fn one<T: rusqlite::types::FromSql>(e: &Env, sql: &str) -> T {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn weighed(e: &Env, name: &str, plu: &str, price_per_kg: i64) -> String {
    let req = ProductCreate {
        product: ProductInput {
            sku: None,
            name: name.into(),
            name_ar: None,
            description: None,
            category_id: None,
            tax_rule_id: e.tax_rule(),
            unit: "kg".into(),
            track_inventory: true,
            allow_decimal_quantity: true,
            reorder_point_milli: 0,
            is_favorite: false,
        },
        price_minor: price_per_kg,
        cost_minor: Some(price_per_kg / 2),
        barcodes: vec![],
        opening_stock_milli: Some(20_000),
        image_b64: None,
        plu: Some(plu.into()),
    };
    e.core.product_create(&e.owner_token, req).unwrap().row.product_id
}

pub fn rule(name: &str, prefix: &str, kind: &str, priority: i64) -> ScaleRule {
    serde_json::from_value(json!({
        "name": name, "prefix": prefix, "length": 13, "item_start": 3, "item_length": 5,
        "value_kind": kind, "value_start": 8, "value_length": 5, "decimals": 3, "check_digit": "ean", "priority": priority
    }))
    .unwrap()
}

/// `body` (12 digits) with its EAN check digit.
fn ean(body: &str) -> String {
    (0..10).map(|d| format!("{body}{d}")).find(|c| amwapos_core::barcodes::gs1_check_ok(c)).unwrap()
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

// ------------------------------------------------------------------ barcodes and PLU

#[test]
fn barcode_kinds_are_chosen_never_guessed() {
    let e = env();
    let t = &e.owner_token;
    let pid = e.product("Water 500ml", "4006381333931", 100, 50, 10_000);
    let d = e.core.product_get(t, &pid).unwrap();
    let b = &d.barcodes[0];
    // Not recorded until a person chooses; the digits only suggest.
    assert_eq!(b.kind, None);
    assert_eq!(b.suggested_kind.as_deref(), Some("ean13"));
    // A kind that does not fit the code is refused.
    let err = e.core.barcode_set_kind(t, &b.barcode_id, Some("ean8".into())).unwrap_err();
    assert_eq!(err.code, ErrorCode::Validation);
    let d = e.core.barcode_set_kind(t, &b.barcode_id, Some("ean13".into())).unwrap();
    assert_eq!(d.barcodes[0].kind.as_deref(), Some("ean13"));
    let audited: i64 = one(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type='barcode.kind_set'");
    assert_eq!(audited, 1);
}

#[test]
fn plu_is_unique_normalized_and_conflicts_are_actionable() {
    let e = env();
    let t = &e.owner_token;
    let apples = weighed(&e, "Apples", "0042", 800);
    assert_eq!(e.core.product_get(t, &apples).unwrap().plu.as_deref(), Some("42"));
    let pears = weighed(&e, "Pears", "43", 900);
    // The same PLU (even written with zeros) on another product is refused, naming the owner.
    let err = e.core.product_set_plu(t, &pears, Some("042".into())).unwrap_err();
    assert_eq!(err.code, ErrorCode::Duplicate);
    assert!(err.message.contains("Apples"), "{}", err.message);
    // A barcode that reads as another product's PLU is refused, and the reverse.
    let water = e.product("Water", "777001", 100, 50, 1_000);
    let err = e.core.barcode_add(t, &water, "42", false).unwrap_err();
    assert_eq!(err.code, ErrorCode::Duplicate);
    let err = e.core.product_set_plu(t, &pears, Some("777001".into())).unwrap_err();
    assert_eq!(err.code, ErrorCode::Duplicate);
    // Arabic digits are read as digits.
    let d = e.core.product_set_plu(t, &pears, Some("٤٤".into())).unwrap();
    assert_eq!(d.plu.as_deref(), Some("44"));
    // Clearing works; non-digits are refused.
    assert_eq!(e.core.product_set_plu(t, &pears, None).unwrap().plu, None);
    assert_eq!(e.core.product_set_plu(t, &pears, Some("4a".into())).unwrap_err().code, ErrorCode::Validation);
}

#[test]
fn plu_sells_through_scan_and_search() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    let apples = weighed(&e, "Apples", "42", 800);
    let r = e.core.pos_scan(t, "42", None).unwrap();
    assert_eq!(r.outcome, "added");
    assert_eq!(r.cart.lines[0].product_id.as_deref(), Some(apples.as_str()));
    assert_eq!(r.cart.lines[0].qty_milli, 1000);
    // Leading zeros and Arabic digits typed on the till.
    assert_eq!(e.core.pos_scan(t, "0042", Some(500)).unwrap().outcome, "added");
    assert_eq!(e.core.pos_scan(t, "٤٢", Some(250)).unwrap().outcome, "added");
}

// ------------------------------------------------------------------ scale barcodes

#[test]
fn scale_rules_are_validated_when_saved() {
    let e = env();
    let t = &e.owner_token;
    let mut r = rule("Deli", "21", "weight", 0);
    r.value_start = 6; // overlaps the item code
    assert_eq!(e.core.scale_rule_save(t, r).unwrap_err().code, ErrorCode::Validation);
    let mut r = rule("Deli", "21", "weight", 0);
    r.value_length = 6; // runs into the check digit
    assert_eq!(e.core.scale_rule_save(t, r).unwrap_err().code, ErrorCode::Validation);
    let saved = e.core.scale_rule_save(t, rule("Deli", "21", "weight", 0)).unwrap();
    assert_eq!(saved.version, 1);
    // A stale edit is refused.
    let mut stale = saved.clone();
    stale.version = 0;
    assert_eq!(e.core.scale_rule_save(t, stale).unwrap_err().code, ErrorCode::Conflict);
    // Rules are switched off, never deleted.
    let deleted = e.core.db.write(|c| Ok(c.execute("DELETE FROM scale_barcode_rules", [])?));
    assert!(deleted.is_err());
}

#[test]
fn weight_label_sells_the_weighed_quantity_with_integer_math() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    let pid = weighed(&e, "Halloumi", "42", 3_200); // 3.200 BHD/kg
    let r = e.core.scale_rule_save(t, rule("Deli weight", "21", "weight", 0)).unwrap();
    let code = ean("210004201250"); // PLU 42, 1.250 kg
    let s = e.core.pos_scan(t, &code, None).unwrap();
    assert_eq!(s.outcome, "added");
    let l = &s.cart.lines[0];
    assert_eq!(l.qty_milli, 1_250);
    assert_eq!(l.line_total_minor, 4_000); // 3.200 × 1.250
                                           // Two labels of the same item never merge: each keeps its own weight.
    let s = e.core.pos_scan(t, &ean("210004200500"), None).unwrap();
    assert_eq!(s.cart.lines.len(), 2);
    let sale = pay(&e, t);
    let (rid, kind, value): (String, String, i64) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row(
                "SELECT scale_rule_id, scale_value_kind, scale_value FROM sale_items WHERE sale_id=?1 ORDER BY line_no LIMIT 1",
                [&sale.sale_id],
                |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!((rid.as_str(), kind.as_str(), value), (r.rule_id.as_str(), "weight", 1_250));
    let stock: i64 = one(&e, &format!("SELECT qty_milli FROM stock_levels WHERE product_id='{pid}'"));
    assert_eq!(stock, 20_000 - 1_250 - 500);
}

#[test]
fn price_label_charges_the_printed_price_and_keeps_it_on_restore() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    let pid = weighed(&e, "Lamb cuts", "77", 5_000);
    let mut r = rule("Butcher price", "22", "price", 0);
    r.decimals = 3;
    e.core.scale_rule_save(t, r).unwrap();
    let code = ean("220007702345"); // PLU 77, 2.345 BHD
    let s = e.core.pos_scan(t, &code, None).unwrap();
    assert_eq!(s.cart.lines[0].line_total_minor, 2_345);
    assert_eq!(s.cart.lines[0].qty_milli, 1_000);
    // The catalogue price changes while the sale is held: the label's price stays.
    e.core.pos_hold(t, None).unwrap();
    e.core.product_price_update(t, &pid, 6_000, None, None).unwrap();
    let held = e.core.pos_held_list(t).unwrap();
    let back = e.core.pos_restore(t, &held[0].cart_id).unwrap();
    assert_eq!(back.lines[0].line_total_minor, 2_345);
    assert!(back.notices.is_empty(), "{:?}", back.notices);
}

#[test]
fn ambiguous_rules_fail_closed_and_charge_nothing() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    weighed(&e, "Cheese", "42", 4_000);
    e.core.scale_rule_save(t, rule("A", "2", "weight", 5)).unwrap();
    e.core.scale_rule_save(t, rule("B", "21", "price", 5)).unwrap();
    let code = ean("210004201250");
    let err = e.core.pos_scan(t, &code, None).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(err.message, "More than one scale-barcode rule matches this code.");
    assert!(e.core.pos_get_cart(t).unwrap().lines.is_empty(), "nothing was charged");
    let test = e.core.scale_rule_test(t, &code).unwrap();
    assert_eq!(test.outcome, "ambiguous");
    assert_eq!(test.rules.len(), 2);
    // A higher priority settles it, deliberately.
    let rules = e.core.scale_rules_list(t).unwrap();
    let mut a = rules.into_iter().find(|r| r.name == "A").unwrap();
    a.priority = 10;
    e.core.scale_rule_save(t, a).unwrap();
    assert_eq!(e.core.pos_scan(t, &code, None).unwrap().cart.lines[0].qty_milli, 1_250);
}

#[test]
fn exact_barcode_wins_over_scale_rules_and_unknowns_stay_unknown() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    weighed(&e, "Cheese", "42", 4_000);
    e.core.scale_rule_save(t, rule("Deli", "21", "weight", 0)).unwrap();
    let code = ean("210004201250");
    let pid = e.product("Boxed cheese", &code, 1_500, 900, 5_000);
    let s = e.core.pos_scan(t, &code, None).unwrap();
    assert_eq!(s.cart.lines[0].product_id.as_deref(), Some(pid.as_str()));
    assert_eq!(e.core.scale_rule_test(t, &code).unwrap().outcome, "barcode");
    // A label whose item has no PLU is refused, not guessed.
    let err = e.core.pos_scan(t, &ean("210009901000"), None).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    // A code no rule fits is recorded as unknown, as before.
    assert_eq!(e.core.pos_scan(t, "99887766", None).unwrap().outcome, "unknown");
    // A bad check digit is not read by an EAN rule.
    let good = ean("210004201250");
    let bad = format!("{}{}", &good[..12], (good.as_bytes()[12] - b'0' + 1) % 10);
    assert_eq!(e.core.pos_scan(t, &bad, None).unwrap().outcome, "unknown");
}

#[test]
fn scale_labels_have_sanity_limits() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    weighed(&e, "Rice sack", "42", 500);
    weighed(&e, "Whole lamb", "43", 5_000);
    e.core.scale_rule_save(t, rule("W", "21", "weight", 0)).unwrap();
    e.core.scale_rule_save(t, rule("P", "22", "price", 0)).unwrap();
    let mut pos: serde_json::Value = e.core.settings_get(t, "pos").unwrap();
    pos["scale_max_weight_milli"] = json!(30_000);
    pos["scale_max_price_minor"] = json!(50_000);
    e.core.settings_save(t, "pos", pos).unwrap();
    // 45.000 kg is above the 30 kg limit; 0 kg is refused too.
    assert_eq!(e.core.pos_scan(t, &ean("210004245000"), None).unwrap_err().code, ErrorCode::Validation);
    assert_eq!(e.core.pos_scan(t, &ean("210004200000"), None).unwrap_err().code, ErrorCode::Validation);
    // 60.000 BHD is above the 50 BHD limit.
    assert_eq!(e.core.pos_scan(t, &ean("220004360000"), None).unwrap_err().code, ErrorCode::Validation);
    assert!(e.core.pos_get_cart(t).unwrap().lines.is_empty());
}

#[test]
fn weight_label_on_a_piece_product_is_refused() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    let pid = e.product("Soap", "5550001", 300, 100, 1_000);
    e.core.product_set_plu(t, &pid, Some("42".into())).unwrap();
    e.core.scale_rule_save(t, rule("W", "21", "weight", 0)).unwrap();
    assert_eq!(e.core.pos_scan(t, &ean("210004201250"), None).unwrap_err().code, ErrorCode::Validation);
}

#[test]
fn barcode_rules_and_plu_need_their_permissions() {
    let e = env();
    let (_, cashier) = e.user("Cash", "role_cashier", "5931");
    let (_, manager) = e.user("Mgr", "role_manager", "5932");
    assert_eq!(e.core.scale_rule_save(&cashier, rule("X", "21", "weight", 0)).unwrap_err().code, ErrorCode::Forbidden);
    assert!(e.core.scale_rule_save(&manager, rule("X", "21", "weight", 0)).is_ok());
    let pid = e.product("Tea", "5550002", 300, 100, 1_000);
    assert_eq!(e.core.product_set_plu(&cashier, &pid, Some("9".into())).unwrap_err().code, ErrorCode::Forbidden);
}
