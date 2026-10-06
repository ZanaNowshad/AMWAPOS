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

// ------------------------------------------------------------------ duplicates

fn supplier(e: &Env, name: &str) -> String {
    e.core.supplier_save(&e.owner_token, None, serde_json::from_value(json!({ "name": name })).unwrap()).unwrap().supplier_id
}

fn terms(e: &Env, sid: &str, pid: &str, code: Option<&str>, upc: Option<i64>, preferred: bool) {
    e.core
        .supplier_terms_save(
            &e.owner_token,
            serde_json::from_value(
                json!({ "supplier_id": sid, "product_id": pid, "supplier_code": code, "units_per_case": upc, "preferred": preferred }),
            )
            .unwrap(),
        )
        .unwrap();
}

fn pairs(e: &Env, later: bool) -> Vec<serde_json::Value> {
    e.core.duplicates_list(&e.owner_token, later, None).unwrap()["pairs"].as_array().unwrap().clone()
}

fn has_pair(ps: &[serde_json::Value], a: &str, b: &str) -> bool {
    ps.iter().any(|p| {
        let (x, y) = (p["a"]["product_id"].as_str().unwrap(), p["b"]["product_id"].as_str().unwrap());
        (x == a && y == b) || (x == b && y == a)
    })
}

#[test]
fn duplicates_show_evidence_and_respect_what_makes_products_different() {
    let e = env();
    let t = &e.owner_token;
    let cola = e.product("Coca-Cola 330ml", "5449000000996", 250, 150, 10_000);
    let cola2 = e.product("COCA COLA 330 ml", "1000001", 250, 150, 5_000);
    let zero = e.product("Coca-Cola Zero 330ml", "5449000131805", 250, 150, 5_000);
    let big = e.product("Coca-Cola 500ml", "5449000000439", 350, 200, 5_000);
    // The same number written with and without a leading zero.
    let w1 = e.product("Spring water", "629123456789", 100, 50, 0);
    let w2 = e.product("Mineral water bottle", "0629123456789", 100, 50, 0);
    let ps = pairs(&e, false);
    assert!(has_pair(&ps, &cola, &cola2), "same words and size");
    assert!(!has_pair(&ps, &cola, &zero), "'Zero' makes it a different product");
    assert!(!has_pair(&ps, &cola, &big), "a different size is a different product");
    assert!(has_pair(&ps, &w1, &w2), "same barcode number");
    let p = ps.iter().find(|p| has_pair(std::slice::from_ref(p), &cola, &cola2)).unwrap();
    let kinds: Vec<&str> = p["evidence"].as_array().unwrap().iter().map(|x| x["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"same_name") && kinds.contains(&"same_size") && kinds.contains(&"similar_price"), "{kinds:?}");
    assert!(p["score"].as_i64().unwrap() >= 70);
    // "Not duplicates" is remembered; "Review later" is set aside.
    e.core.duplicate_decide(t, &w1, &w2, Some("not_duplicates".into()), None).unwrap();
    e.core.duplicate_decide(t, &cola2, &cola, Some("later".into()), None).unwrap();
    let ps = pairs(&e, false);
    assert!(!has_pair(&ps, &w1, &w2) && !has_pair(&ps, &cola, &cola2));
    let ps = pairs(&e, true);
    assert!(has_pair(&ps, &cola, &cola2) && !has_pair(&ps, &w1, &w2));
    assert_eq!(e.core.duplicates_list(t, false, None).unwrap()["later_count"], 1);
}

#[test]
fn supplier_codes_are_duplicate_evidence() {
    let e = env();
    let sid = supplier(&e, "Gulf Foods");
    let r1 = e.product("Basmati rice 5kg", "2000001", 900, 600, 0);
    let r2 = e.product("Rice India 5kg", "2000002", 950, 600, 0);
    terms(&e, &sid, &r1, Some("GF-RICE-5"), None, false);
    // A different supplier code: names differ, so no suggestion.
    assert!(!has_pair(&pairs(&e, false), &r1, &r2));
    terms(&e, &sid, &r2, Some("gf-rice-5"), None, false);
    // The same supplier item code and the same size: suggested, with the evidence.
    let ps = pairs(&e, false);
    let p = ps.iter().find(|p| has_pair(std::slice::from_ref(p), &r1, &r2)).expect("suggested");
    assert!(p["evidence"].to_string().contains("supplier_code"));
    // The same code on a different size is not suggested.
    let r3 = e.product("Basmati rice 10kg", "2000003", 1700, 1100, 0);
    let sid2 = supplier(&e, "Other");
    terms(&e, &sid2, &r1, Some("X1"), None, false);
    terms(&e, &sid2, &r3, Some("x1"), None, false);
    assert!(!has_pair(&pairs(&e, false), &r1, &r3));
}

// ------------------------------------------------------------------ merge

use amwapos_core::merge::{MergeChoices, MergeRequest};

fn total_stock(e: &Env) -> i64 {
    one(e, "SELECT COALESCE(SUM(qty_milli),0) FROM stock_levels")
}

fn merge_req(e: &Env, src: &str, tgt: &str, choices: MergeChoices) -> MergeRequest {
    let p = e.core.product_merge_preview(&e.owner_token, src, tgt).unwrap();
    MergeRequest {
        source_product_id: src.into(),
        target_product_id: tgt.into(),
        choices,
        preview_hash: p["preview_hash"].as_str().unwrap().into(),
        operation_id: op(),
    }
}

fn sell_code(e: &Env, t: &str, code: &str, qty: i64) -> SaleResult {
    e.core.pos_scan(t, code, Some(qty)).unwrap();
    pay(e, t)
}

#[test]
fn merge_conserves_stock_and_batches_and_keeps_history() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    let keep = e.product("Laban 1L", "6291000000011", 450, 300, 10_000);
    let dup = e.product("Laban 1 litre", "6291000000028", 450, 360, 8_000);
    // A batch on the duplicate: 5 of its 8 counted into batch B-77.
    e.core.product_lot_settings(t, &dup, true, Some("use_by".into())).unwrap();
    e.core
        .lot_count_in(
            t,
            serde_json::from_value(json!({ "product_id": dup, "qty_milli": 5_000, "operation_id": op(),
                "lot": { "supplier_lot_code": "B-77",
                "expires_on": (chrono::Local::now().date_naive() + chrono::Duration::days(90)).to_string() } }))
            .unwrap(),
        )
        .unwrap();
    // History: a sale of the duplicate with its issued receipt.
    let sale = sell_code(&e, t, "6291000000028", 1_000);
    let receipt_hash: String = one(&e, &format!("SELECT sha256 FROM receipt_snapshots WHERE ref_id='{}'", sale.sale_id));
    let sale_items_before: String =
        one(&e, "SELECT group_concat(sale_item_id||product_id||line_total_minor||effective_unit_price_minor, ',') FROM sale_items");
    let total_before = total_stock(&e);
    let movements_before: i64 = one(&e, "SELECT COUNT(*) FROM stock_movements");

    let p = e.core.product_merge_preview(t, &dup, &keep).unwrap();
    assert_eq!(p["can_merge"], true, "{p}");
    assert_eq!(p["moves"]["stock"][0]["source_milli"], 7_000);
    assert_eq!(p["moves"]["stock"][0]["lots"][0]["qty_milli"], 5_000);
    assert_eq!(p["history"]["sale_lines"], 1);
    let r = e.core.product_merge(t, merge_req(&e, &dup, &keep, MergeChoices::default())).unwrap();

    // Invariant 1: the total never changes; all of it is on the kept product.
    assert_eq!(total_stock(&e), total_before);
    let kept: i64 = one(&e, &format!("SELECT qty_milli FROM stock_levels WHERE product_id='{keep}'"));
    let gone: i64 = one(&e, &format!("SELECT qty_milli FROM stock_levels WHERE product_id='{dup}'"));
    assert_eq!((kept, gone), (17_000, 0));
    // Paired movements only (no movement edited or removed).
    let moves: i64 = one(&e, "SELECT COUNT(*) FROM stock_movements");
    let merge_moves: i64 = one(&e, "SELECT COUNT(*) FROM stock_movements WHERE source_type='product_merge'");
    assert_eq!(moves, movements_before + merge_moves);
    let net: i64 = one(&e, "SELECT SUM(qty_delta_milli) FROM stock_movements WHERE source_type='product_merge'");
    assert_eq!(net, 0);
    // The batch moved through an explicit, traceable transition.
    let (prov, from, qty, code): (String, String, i64, String) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row(
                &format!("SELECT provenance, merged_from_lot_id, qty_received_milli, supplier_lot_code FROM stock_lots WHERE product_id='{keep}'"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?)
        })
        .unwrap();
    assert_eq!((prov.as_str(), qty, code.as_str()), ("merge", 5_000, "B-77"));
    let old_lot: String = one(&e, &format!("SELECT lot_id FROM stock_lots WHERE product_id='{dup}'"));
    assert_eq!(from, old_lot);
    let branch: String = one(&e, "SELECT branch_id FROM branches LIMIT 1");
    let lots = e.core.db.read(|c| amwapos_core::lots::replay(c, &keep, &branch)).unwrap();
    assert_eq!(lots.lots.iter().map(|l| l.balance_milli).sum::<i64>(), 5_000);
    assert_eq!(lots.stock_milli, 17_000);
    // Invariants 2 and 3: receipts and sale snapshots are untouched.
    let receipt_after: String = one(&e, &format!("SELECT sha256 FROM receipt_snapshots WHERE ref_id='{}'", sale.sale_id));
    assert_eq!(receipt_after, receipt_hash);
    let sale_items_after: String =
        one(&e, "SELECT group_concat(sale_item_id||product_id||line_total_minor||effective_unit_price_minor, ',') FROM sale_items");
    assert_eq!(sale_items_after, sale_items_before);
    // The duplicate's barcode now sells the kept product.
    let s = e.core.pos_scan(t, "6291000000028", None).unwrap();
    assert_eq!(s.cart.lines[0].product_id.as_deref(), Some(keep.as_str()));
    e.core.pos_cancel_sale(t, None).unwrap();
    // The retired product says where it went.
    let d = e.core.product_get(t, &dup).unwrap();
    assert!(!d.row.active);
    assert_eq!(d.merged_into.as_ref().unwrap()["product_id"], keep.as_str());
    // Weighted average cost: (10 × 0.300 + 7 × 0.360) / 17 = 0.3247 → 325.
    let avg: i64 = one(&e, &format!("SELECT avg_cost_minor FROM product_costs WHERE product_id='{keep}'"));
    assert_eq!(avg, 325);
    // One permanent merge record, audited.
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM product_merges"), 1);
    assert!(e.core.db.write(|c| Ok(c.execute("DELETE FROM product_merges", [])?)).is_err());
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type='product.merged'"), 1);
    assert_eq!(e.core.product_merges_list(t, Some(keep.clone())).unwrap().len(), 1);
    // A merged product cannot be merged again.
    assert_eq!(e.core.product_merge_preview(t, &dup, &keep).unwrap_err().code, ErrorCode::Conflict);
    let _ = r;
}

#[test]
fn merge_is_idempotent_and_refuses_a_stale_preview() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    let keep = e.product("Tea 100", "7000001", 900, 500, 4_000);
    let dup = e.product("Tea 100 bags", "7000002", 900, 500, 3_000);
    let req = merge_req(&e, &dup, &keep, MergeChoices::default());
    // Something changes after the preview: refused, nothing moved.
    sell_code(&e, t, "7000002", 1_000);
    let err = e.core.product_merge(t, req).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM product_merges"), 0);
    let req = merge_req(&e, &dup, &keep, MergeChoices::default());
    let first = e.core.product_merge(t, req.clone()).unwrap();
    let again = e.core.product_merge(t, req).unwrap();
    assert_eq!(first, again, "a retry returns the same result");
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM product_merges"), 1);
    assert_eq!(total_stock(&e), 6_000);
}

#[test]
fn merge_needs_explicit_choices_for_conflicts() {
    let e = env();
    let t = &e.owner_token;
    let keep = weighed(&e, "Tomatoes", "11", 600);
    let dup = weighed(&e, "Tomato", "12", 650);
    let sid = supplier(&e, "Farm");
    terms(&e, &sid, &keep, Some("T-1"), Some(10), true);
    terms(&e, &sid, &dup, Some("T-9"), Some(12), false);
    let p = e.core.product_merge_preview(t, &dup, &keep).unwrap();
    assert_eq!(p["conflicts"]["price"]["source"], 650);
    assert_eq!(p["conflicts"]["plu"]["target"], "11");
    assert_eq!(p["conflicts"]["supplier_terms"].as_array().unwrap().len(), 1);
    // Without choices: refused, naming what to choose.
    let err = e.core.product_merge(t, merge_req(&e, &dup, &keep, MergeChoices::default())).unwrap_err();
    assert_eq!(err.code, ErrorCode::Validation);
    let mut ch = MergeChoices { price: Some("source".into()), plu: Some("target".into()), ..Default::default() };
    let err = e.core.product_merge(t, merge_req(&e, &dup, &keep, ch.clone())).unwrap_err();
    assert!(err.message.contains("Farm"), "{}", err.message);
    ch.supplier_terms.insert(sid.clone(), "source".into());
    e.core.product_merge(t, merge_req(&e, &dup, &keep, ch)).unwrap();
    let d = e.core.product_get(t, &keep).unwrap();
    assert_eq!(d.row.price_minor, Some(650), "the chosen price");
    assert_eq!(d.plu.as_deref(), Some("11"), "the chosen PLU");
    let (code, upc, pref): (String, i64, i64) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT supplier_code, units_per_case, preferred FROM supplier_products WHERE product_id=?1", [&keep], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?)
        })
        .unwrap();
    assert_eq!((code.as_str(), upc, pref), ("T-9", 12, 1), "the chosen terms; still the preferred supplier");
    // Price history records only the applied change.
    let n: i64 = one(&e, &format!("SELECT COUNT(*) FROM product_prices WHERE product_id='{keep}'"));
    assert_eq!(n, 2);
    // The retired product's PLU is freed, not duplicated.
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM products WHERE plu IS NOT NULL"), 1);
}

#[test]
fn merge_is_blocked_by_open_documents_and_is_owner_only() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    let keep = e.product("Bread", "8000001", 300, 200, 1_000);
    let dup = e.product("Bread loaf", "8000002", 300, 200, 1_000);
    let sid = supplier(&e, "Bakery");
    e.core
        .purchase_order_save(
            t,
            None,
            serde_json::from_value(
                json!({ "supplier_id": sid, "lines": [{ "product_id": dup, "qty_milli": 5_000, "unit_cost_minor": 200 }] }),
            )
            .unwrap(),
        )
        .unwrap();
    // A sale in progress with the product.
    e.core.pos_scan(t, "8000002", None).unwrap();
    let p = e.core.product_merge_preview(t, &dup, &keep).unwrap();
    let kinds: Vec<&str> = p["blockers"].as_array().unwrap().iter().map(|b| b["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["purchase_order", "cart"]);
    assert_eq!(p["can_merge"], false);
    let err = e.core.product_merge(t, merge_req(&e, &dup, &keep, MergeChoices::default())).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.message.contains("Open purchase orders"), "{}", err.message);
    // The manager can preview but not merge; the cashier cannot do either.
    let (_, manager) = e.user("Mgr", "role_manager", "5932");
    let (_, cashier) = e.user("Cash", "role_cashier", "5931");
    assert!(e.core.product_merge_preview(&manager, &dup, &keep).is_ok());
    assert_eq!(e.core.product_merge(&manager, merge_req(&e, &dup, &keep, MergeChoices::default())).unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(e.core.product_merge_preview(&cashier, &dup, &keep).unwrap_err().code, ErrorCode::Forbidden);
}

#[test]
fn a_failed_merge_changes_nothing() {
    let e = env();
    let t = &e.owner_token;
    let keep = e.product("Oil 1L", "8100001", 1200, 800, 3_000);
    let dup = e.product("Oil 1 L", "8100002", 1200, 800, 2_000);
    let before: String = one(&e, "SELECT group_concat(product_id||':'||qty_milli) FROM (SELECT * FROM stock_levels ORDER BY product_id)");
    let moves: i64 = one(&e, "SELECT COUNT(*) FROM stock_movements");
    // Failure injection: the merge record cannot be written (last step).
    e.core
        .db
        .write(|c| {
            Ok(c.execute_batch("CREATE TRIGGER t_fail BEFORE INSERT ON product_merges BEGIN SELECT RAISE(ABORT, 'disk full'); END;")?)
        })
        .unwrap();
    assert!(e.core.product_merge(t, merge_req(&e, &dup, &keep, MergeChoices::default())).is_err());
    let after: String = one(&e, "SELECT group_concat(product_id||':'||qty_milli) FROM (SELECT * FROM stock_levels ORDER BY product_id)");
    assert_eq!(after, before);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM stock_movements"), moves);
    assert_eq!(one::<i64>(&e, &format!("SELECT COUNT(*) FROM product_barcodes WHERE product_id='{dup}'")), 1);
    assert!(e.core.product_get(t, &dup).unwrap().row.active);
}

// ------------------------------------------------------------------ addresses

#[test]
fn bahrain_address_keeps_governorate_directions_and_a_normalized_area() {
    let e = env();
    let t = &e.owner_token;
    let c = e
        .core
        .customer_save(
            t,
            None,
            serde_json::from_value(json!({ "name": "Maryam", "phone": "33445566", "area": "  JUFAIR ",
                "address_parts": { "flat": "12", "building": "1203", "road": "4518", "block": "324",
                    "governorate": "Capital", "directions": "Second gate, ring twice" } }))
            .unwrap(),
        )
        .unwrap();
    assert_eq!(c.info.area.as_deref(), Some("Juffair"));
    let p = &c.info.address_parts;
    assert_eq!(p.governorate.as_deref(), Some("capital"));
    assert_eq!(p.directions.as_deref(), Some("Second gate, ring twice"));
    assert_eq!(c.info.address.as_deref(), Some("Flat 12, Bldg 1203, Road 4518, Block 324"));
    // Optional: nothing is invented when not given.
    let c2 = e.core.customer_save(t, None, serde_json::from_value(json!({ "name": "Ali", "phone": "33445577" })).unwrap()).unwrap();
    assert_eq!(c2.info.address_parts.governorate, None);
    // An unknown governorate is refused.
    let err = e
        .core
        .customer_save(
            t,
            None,
            serde_json::from_value(json!({ "name": "X", "address_parts": { "building": "1", "governorate": "west" } })).unwrap(),
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Validation);
    // A drop for the customer carries the saved governorate and directions.
    e.open_shift(t, 0);
    let d = e
        .core
        .delivery_create(
            t,
            serde_json::from_value(json!({ "customer_id": c.customer_id, "payment_status": "cod", "amount_minor": 500 })).unwrap(),
        )
        .unwrap();
    let (g, dir): (Option<String>, Option<String>) = e
        .core
        .db
        .read(|x| {
            Ok(x.query_row("SELECT governorate, directions FROM delivery_orders WHERE delivery_id=?1", [&d.delivery_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?)
        })
        .unwrap();
    assert_eq!((g.as_deref(), dir.as_deref()), (Some("capital"), Some("Second gate, ring twice")));
}

// ------------------------------------------------------------------ sales channel

#[test]
fn channel_is_explicit_kept_through_hold_and_fixed_after_sale() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    e.product("Juice", "9100001", 500, 300, 10_000);
    // Sales made before Wave 5 have no channel: "not recorded", never guessed.
    let before: i64 = one(&e, "SELECT COUNT(*) FROM sales WHERE channel IS NOT NULL");
    assert_eq!(before, 0);
    // A till sale is a 'pos' sale from the first scan.
    let c = e.core.pos_scan(t, "9100001", None).unwrap().cart;
    assert_eq!(c.channel.as_deref(), Some("pos"));
    // The cashier records a phone order; it survives hold and restore.
    let c = e.core.pos_set_channel(t, "phone").unwrap();
    assert_eq!(c.channel.as_deref(), Some("phone"));
    e.core.pos_hold(t, None).unwrap();
    let held = e.core.pos_held_list(t).unwrap();
    let back = e.core.pos_restore(t, &held[0].cart_id).unwrap();
    assert_eq!(back.channel.as_deref(), Some("phone"));
    assert_eq!(e.core.pos_set_channel(t, "telepathy").unwrap_err().code, ErrorCode::Validation);
    let sale = pay(&e, t);
    let d = e.core.sale_get(t, &sale.sale_id).unwrap();
    assert_eq!(d.channel.as_deref(), Some("phone"));
    // Completed sales never change.
    let r = e.core.db.write(|c| Ok(c.execute("UPDATE sales SET channel='web' WHERE sale_id=?1", [&sale.sale_id])?));
    assert!(r.is_err());
    // Fulfilment stays separate: the next till sale is a 'pos' sale again.
    let c = e.core.pos_scan(t, "9100001", None).unwrap().cart;
    assert_eq!(c.channel.as_deref(), Some("pos"));
}

#[test]
fn an_order_rung_up_carries_its_channel_before_pricing() {
    let e = env();
    let t = &e.owner_token;
    e.core.settings_save(t, "features", json!({ "orders.digital": true })).unwrap();
    e.open_shift(t, 0);
    let pid = e.product("Dates box", "9200001", 2_000, 1_200, 10_000);
    let o = e
        .core
        .order_save(
            t,
            None,
            serde_json::from_value(
                json!({ "channel": "whatsapp", "phone": "33334444", "lines": [{ "product_id": pid, "qty_milli": 1000 }] }),
            )
            .unwrap(),
        )
        .unwrap();
    e.core.order_confirm(t, &o.order_id).unwrap();
    let cart = e.core.order_convert(t, &o.order_id, &op()).unwrap();
    assert_eq!(cart.channel.as_deref(), Some("whatsapp"));
    assert_eq!(cart.lines[0].price_type.as_deref(), Some("retail"));
    assert!(cart.lines[0].using_retail, "no WhatsApp price yet: retail is used and shown");
    // The order's channel cannot be changed at the till.
    assert_eq!(e.core.pos_set_channel(t, "pos").unwrap_err().code, ErrorCode::Conflict);
    let sale = pay(&e, t);
    assert_eq!(e.core.sale_get(t, &sale.sale_id).unwrap().channel.as_deref(), Some("whatsapp"));
}

// ------------------------------------------------------------------ channel prices

fn set_ch(e: &Env, pid: &str, ty: &str, amount: Option<i64>, branch: Option<&str>) {
    e.core.product_channel_price_set(&e.owner_token, pid, ty, amount, branch.map(String::from), None).unwrap();
}

#[test]
fn channel_price_fallback_is_deterministic_and_documented() {
    let e = env();
    let t = &e.owner_token;
    e.core.settings_save(t, "features", json!({ "org.multi_branch": true })).unwrap();
    let branch = e.core.require_device().unwrap().branch_id;
    let pid = e.product("Cake", "9300001", 1_000, 500, 10_000);
    e.open_shift(t, 0);
    let price_on = |ch: &str| -> i64 {
        e.core.pos_set_channel(t, ch).unwrap();
        let c = e.core.pos_scan(t, "9300001", None).unwrap().cart;
        let p = c.lines[0].unit_price_minor;
        e.core.pos_cancel_sale(t, None).unwrap();
        p
    };
    // retail only
    assert_eq!(price_on("whatsapp"), 1_000);
    // branch retail beats retail
    e.core.branch_price_set(t, &pid, &branch, Some(950)).unwrap();
    assert_eq!(price_on("whatsapp"), 950);
    assert_eq!(price_on("pos"), 950);
    // channel price beats branch retail
    set_ch(&e, &pid, "whatsapp", Some(900), None);
    assert_eq!(price_on("whatsapp"), 900);
    assert_eq!(price_on("pos"), 950, "the till keeps retail");
    // branch + channel beats channel
    set_ch(&e, &pid, "whatsapp", Some(880), Some(&branch));
    assert_eq!(price_on("whatsapp"), 880);
    // removing them falls back step by step
    set_ch(&e, &pid, "whatsapp", None, Some(&branch));
    assert_eq!(price_on("whatsapp"), 900);
    set_ch(&e, &pid, "whatsapp", None, None);
    assert_eq!(price_on("whatsapp"), 950);
    let rows = e.core.product_channel_prices(t, &pid).unwrap();
    let wa = rows.iter().find(|r| r.price_type == "whatsapp").unwrap();
    assert!(wa.using_retail && wa.own_price_minor.is_none());
    // Unknown lists are refused; the retail price cannot be removed this way.
    assert_eq!(e.core.product_channel_price_set(t, &pid, "retail", None, None, None).unwrap_err().code, ErrorCode::Validation);
}

#[test]
fn retail_is_unchanged_without_channel_prices() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    let pid = e.product("Soap", "9300002", 700, 300, 10_000);
    for ch in ["pos", "whatsapp", "phone", "web", "other"] {
        e.core.pos_set_channel(t, ch).unwrap();
        let c = e.core.pos_scan(t, "9300002", None).unwrap().cart;
        assert_eq!(c.lines[0].unit_price_minor, 700, "{ch}");
        assert_eq!(c.lines[0].price_type.as_deref(), Some("retail"));
        e.core.pos_cancel_sale(t, None).unwrap();
    }
    for ty in ["whatsapp", "phone", "web"] {
        let sql = amwapos_core::catalog::price_sql_for(ty, "amount_minor");
        let v: i64 = e
            .core
            .db
            .read(|c| Ok(c.query_row(&format!("SELECT {sql} FROM products p WHERE p.product_id=?1"), [&pid], |r| r.get(0))?))
            .unwrap();
        assert_eq!(v, 700);
    }
    assert_eq!(amwapos_core::catalog::price_sql_for("retail", "amount_minor"), amwapos_core::catalog::PRICE_SQL);
}

#[test]
fn channel_price_is_snapshotted_on_the_sale_and_matches_the_order() {
    let e = env();
    let t = &e.owner_token;
    e.core.settings_save(t, "features", json!({ "orders.digital": true })).unwrap();
    e.open_shift(t, 0);
    let pid = e.product("Honey jar", "9300003", 3_000, 1_800, 10_000);
    set_ch(&e, &pid, "whatsapp", Some(2_800), None);
    let o = e
        .core
        .order_save(
            t,
            None,
            serde_json::from_value(
                json!({ "channel": "whatsapp", "phone": "33334444", "lines": [{ "product_id": pid, "qty_milli": 2000 }] }),
            )
            .unwrap(),
        )
        .unwrap();
    // The order's estimate uses the same prices the till will charge.
    assert_eq!(o.estimate_minor, 5_600);
    e.core.order_confirm(t, &o.order_id).unwrap();
    let cart = e.core.order_convert(t, &o.order_id, &op()).unwrap();
    assert_eq!(cart.totals.total_minor, 5_600);
    assert!(!cart.lines[0].using_retail);
    let sale = pay(&e, t);
    // The price is kept on the sale: a later change does not touch it.
    set_ch(&e, &pid, "whatsapp", Some(2_500), None);
    let d = e.core.sale_get(t, &sale.sale_id).unwrap();
    assert_eq!((d.items[0].unit_price_minor, d.total_minor), (2_800, 5_600));
    let pt: String = one(&e, &format!("SELECT price_type FROM sale_items WHERE sale_id='{}'", sale.sale_id));
    assert_eq!(pt, "whatsapp");
    // Same cart, same channel, same configuration: the same price every time.
    let mut seen = std::collections::HashSet::new();
    for _ in 0..3 {
        e.core.pos_set_channel(t, "whatsapp").unwrap();
        seen.insert(e.core.pos_scan(t, "9300003", Some(2_000)).unwrap().cart.totals.total_minor);
        e.core.pos_cancel_sale(t, None).unwrap();
    }
    assert_eq!(seen.len(), 1);
    // Price history records each change with its list.
    let n: i64 = one(&e, &format!("SELECT COUNT(*) FROM product_prices WHERE product_id='{pid}' AND price_type='whatsapp'"));
    assert_eq!(n, 2);
}

#[test]
fn switching_channel_reprices_catalogue_lines_only() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    let pid = e.product("Nuts", "9300004", 1_000, 600, 10_000);
    set_ch(&e, &pid, "phone", Some(1_100), None);
    e.core.pos_scan(t, "9300004", None).unwrap();
    let c = e.core.pos_set_channel(t, "phone").unwrap();
    assert_eq!(c.lines[0].unit_price_minor, 1_100);
    assert_eq!(c.lines[0].price_type.as_deref(), Some("phone"));
    let c = e.core.pos_set_channel(t, "pos").unwrap();
    assert_eq!(c.lines[0].unit_price_minor, 1_000);
    // Cashiers cannot set channel prices.
    let (_, cashier) = e.user("Cash", "role_cashier", "5931");
    assert_eq!(e.core.product_channel_price_set(&cashier, &pid, "web", Some(1), None, None).unwrap_err().code, ErrorCode::Forbidden);
}

// ------------------------------------------------------------------ policies, rounding, margin

use amwapos_core::policies::Policy;

fn policy(e: &Env, v: serde_json::Value) -> Policy {
    e.core.pricing_policy_save(&e.owner_token, serde_json::from_value(v).unwrap()).unwrap()
}

fn review(e: &Env, group: Option<&str>) -> serde_json::Value {
    e.core.pricing_review(&e.owner_token, group.map(String::from), Some(500), None).unwrap()
}

fn row<'a>(r: &'a serde_json::Value, pid: &str, ty: &str) -> Option<&'a serde_json::Value> {
    r["rows"].as_array().unwrap().iter().find(|x| x["product_id"] == pid && x["price_type"] == ty)
}

fn prices(e: &Env) -> String {
    one(e, "SELECT group_concat(price_id||':'||amount_minor||':'||COALESCE(effective_to,''), ',') FROM (SELECT * FROM product_prices ORDER BY price_id)")
}

#[test]
fn policies_recommend_and_never_rewrite_a_price() {
    let e = env();
    let t = &e.owner_token;
    // VAT 10% included (setup default). Cost 1.000; price 1.200 → margin 8.3% on net.
    let pid = e.product("Olive oil", "9400001", 1_200, 1_000, 10_000);
    let before = prices(&e);
    let p = policy(
        &e,
        json!({ "name": "Store 25% margin", "scope": "global", "target_margin_bp": 2_500, "min_margin_bp": 1_500,
                 "rounding_step_minor": 50, "cost_basis": "average" }),
    );
    assert_eq!(p.version, 1);
    // Invariant 7: saving a policy changes no price.
    assert_eq!(prices(&e), before);
    let r = review(&e, None);
    let x = row(&r, &pid, "retail").expect("listed");
    // 1.000 × 1.10 ÷ 0.75 = 1.4667 → step 0.050 → 1.450 is below 25% target but above the
    // 15% floor (1.2941): nearest step wins; the floor is never breached.
    assert_eq!(x["recommended_minor"], 1_450);
    assert_eq!(x["floor_price_minor"], 1_295);
    let groups: Vec<&str> = x["groups"].as_array().unwrap().iter().map(|g| g.as_str().unwrap()).collect();
    assert!(groups.contains(&"below_min_margin") && groups.contains(&"recommendation"), "{groups:?}");
    assert_eq!(r["counts"]["below_min_margin"], 1);
    assert_eq!(x["policy_name"], "Store 25% margin");
    assert_eq!(x["cost_basis"], "average");
    // Still nothing changed: recommendations only.
    assert_eq!(prices(&e), before);
    assert_eq!(e.core.product_get(t, &pid).unwrap().row.price_minor, Some(1_200));
}

#[test]
fn policy_validation_and_specific_scope_wins() {
    let e = env();
    let t = &e.owner_token;
    let cat = e.core.category_save(t, None, "Dairy", None, 0).unwrap().category_id;
    let bad = |v: serde_json::Value| e.core.pricing_policy_save(t, serde_json::from_value(v).unwrap()).unwrap_err().code;
    assert_eq!(bad(json!({ "name": "x", "scope": "global", "markup_bp": 100, "target_margin_bp": 100 })), ErrorCode::Validation);
    assert_eq!(bad(json!({ "name": "x", "scope": "category", "target_margin_bp": 100 })), ErrorCode::Validation);
    assert_eq!(bad(json!({ "name": "x", "scope": "global", "target_margin_bp": 100, "rounding_step_minor": 7 })), ErrorCode::Validation);
    assert_eq!(bad(json!({ "name": "x", "scope": "global", "target_margin_bp": 1_000, "min_margin_bp": 2_000 })), ErrorCode::Validation);
    assert_eq!(bad(json!({ "name": "x", "scope": "global" })), ErrorCode::Validation);
    policy(&e, json!({ "name": "All 20% markup", "scope": "global", "markup_bp": 2_000 }));
    policy(
        &e,
        json!({ "name": "Dairy 30% margin", "scope": "category", "scope_id": cat, "target_margin_bp": 3_000, "rounding_step_minor": 5 }),
    );
    let milk = e.product("Milk", "9400002", 500, 400, 1_000);
    let tax = e.tax_rule();
    let mut d = e.core.product_get(t, &milk).unwrap();
    e.core
        .product_update(
            t,
            serde_json::from_value(json!({ "product_id": milk, "expected_version": d.version, "name": "Milk", "category_id": cat, "tax_rule_id": tax, "unit": "pcs" }))
                .unwrap(),
        )
        .unwrap();
    d = e.core.product_get(t, &milk).unwrap();
    assert_eq!(d.row.category_id.as_deref(), Some(cat.as_str()));
    let r = review(&e, None);
    let x = row(&r, &milk, "retail").unwrap();
    assert_eq!(x["policy_name"], "Dairy 30% margin", "the category is more specific than global");
    // 0.400 × 1.10 ÷ 0.70 = 0.62857 → 0.630
    assert_eq!(x["recommended_minor"], 630);
    // Two policies with the same scope and priority are surfaced as ambiguous.
    policy(&e, json!({ "name": "Dairy 35% margin", "scope": "category", "scope_id": cat, "target_margin_bp": 3_500 }));
    let r = review(&e, None);
    assert_eq!(r["ambiguous"].as_array().unwrap().len(), 1, "{}", r["ambiguous"]);
    // Cashiers cannot see or set policies.
    let (_, cashier) = e.user("Cash", "role_cashier", "5931");
    assert_eq!(e.core.pricing_policies_list(&cashier).unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(e.core.pricing_review(&cashier, None, None, None).unwrap_err().code, ErrorCode::Forbidden);
}

#[test]
fn review_groups_dismiss_postpone_and_channel_gaps() {
    let e = env();
    let t = &e.owner_token;
    let a = e.product("Rice 5kg", "9400010", 3_000, 2_000, 1_000);
    let b = e.product("Sugar 1kg", "9400011", 800, 500, 1_000);
    // No policy yet: listed as "no policy".
    let r = review(&e, Some("no_policy"));
    assert_eq!(r["total"], 2);
    policy(&e, json!({ "name": "Global 30%", "scope": "global", "target_margin_bp": 3_000, "rounding_step_minor": 25 }));
    policy(
        &e,
        json!({ "name": "WhatsApp 35%", "scope": "channel", "scope_id": "whatsapp", "target_margin_bp": 3_500, "rounding_step_minor": 25 }),
    );
    let r = review(&e, None);
    assert_eq!(r["counts"]["no_policy"], 0);
    // A channel with a policy but no price of its own: "Retail price will be used".
    let wa = row(&r, &a, "whatsapp").unwrap();
    assert!(wa["groups"].to_string().contains("channel_price_missing"));
    assert_eq!(wa["policy_name"], "WhatsApp 35%");
    // Dismiss hides it while cost and suggestion stay the same.
    e.core.pricing_decide(t, &a, "retail", Some("dismissed".into()), None).unwrap();
    assert!(row(&review(&e, None), &a, "retail").is_none());
    // Postpone hides it until the date.
    e.core.pricing_decide(t, &b, "retail", Some("postponed".into()), Some("2099-01-01".into())).unwrap();
    assert!(row(&review(&e, None), &b, "retail").is_none());
    assert_eq!(e.core.pricing_decide(t, &b, "retail", Some("postponed".into()), None).unwrap_err().code, ErrorCode::Validation);
    // A cost change brings the dismissed one back, in "cost changed".
    e.core.product_cost_update(t, &a, 2_400, Some("New supplier price".into())).unwrap();
    let r = review(&e, None);
    let x = row(&r, &a, "retail").expect("back after a cost change");
    assert!(x["groups"].to_string().contains("cost_changed"), "{}", x["groups"]);
}

#[test]
fn bulk_apply_is_atomic_idempotent_audited_and_guards_the_margin() {
    let e = env();
    let t = &e.owner_token;
    let a = e.product("Tea A", "9400020", 1_000, 600, 1_000);
    let b = e.product("Tea B", "9400021", 1_000, 600, 1_000);
    policy(
        &e,
        json!({ "name": "Min 20%", "scope": "global", "min_margin_bp": 2_000, "target_margin_bp": 3_000, "rounding_step_minor": 5 }),
    );
    // Preview changes nothing and flags the price below the floor.
    let items = json!([{ "product_id": a, "amount_minor": 950 }, { "product_id": b, "amount_minor": 700 }]);
    let before = prices(&e);
    let pv = e.core.pricing_apply_preview(t, serde_json::from_value(items.clone()).unwrap()).unwrap();
    assert_eq!(pv["below_min_margin"], 1, "0.700 is below the 0.825 floor (0.600 × 1.10 ÷ 0.80)");
    assert_eq!(prices(&e), before);
    // A manager without pricing.policy needs an approval bound to these prices.
    e.core
        .db
        .write(|c| Ok(c.execute("DELETE FROM role_permissions WHERE role_id='role_manager' AND permission_code='pricing.policy'", [])?))
        .unwrap();
    let (_, mgr) = e.user("Mgr", "role_manager", "5932");
    let req = |op: &str, tok: Option<String>| -> amwapos_core::policies::ApplyRequest {
        serde_json::from_value(json!({ "items": items, "operation_id": op, "reason": "Review", "approval_token": tok })).unwrap()
    };
    let opid = op();
    let asked = e.core.pricing_apply(&mgr, req(&opid, None)).unwrap_err();
    assert_eq!(asked.code, ErrorCode::ApprovalRequired);
    assert_eq!(prices(&e), before, "nothing applied");
    let binding = asked.details.as_ref().unwrap()["binding"].as_str().unwrap().to_string();
    let appr = e.core.approve(&mgr, &e.owner_id, OWNER_PIN, "pricing.policy", "x", Some(&binding)).unwrap();
    let tok = appr["approval_token"].as_str().unwrap().to_string();
    let r = e.core.pricing_apply(&mgr, req(&opid, Some(tok))).unwrap();
    assert_eq!(r["applied"], 2);
    assert_eq!(e.core.product_get(t, &a).unwrap().row.price_minor, Some(950));
    assert_eq!(e.core.product_get(t, &b).unwrap().row.price_minor, Some(700));
    // Idempotent: the same operation returns the same result, nothing twice.
    let again = e.core.pricing_apply(&mgr, req(&opid, None)).unwrap();
    assert_eq!(again["batch_id"], r["batch_id"]);
    let n: i64 = one(&e, "SELECT COUNT(*) FROM product_prices WHERE batch_id IS NOT NULL");
    assert_eq!(n, 2, "price history: only the applied changes");
    let (approved, count): (Option<String>, i64) = e
        .core
        .db
        .read(|c| Ok(c.query_row("SELECT approved_by, item_count FROM price_change_batches", [], |r| Ok((r.get(0)?, r.get(1)?)))?))
        .unwrap();
    assert_eq!((approved.as_deref(), count), (Some(e.owner_id.as_str()), 2));
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type='pricing.applied'"), 1);
    // Unchanged prices are skipped (no history row).
    let r = e
        .core
        .pricing_apply(
            t,
            serde_json::from_value(json!({ "items": [{ "product_id": a, "amount_minor": 950 }], "operation_id": op() })).unwrap(),
        )
        .unwrap();
    assert_eq!((r["applied"].as_i64(), r["skipped_unchanged"].as_i64()), (Some(0), Some(1)));
    // Atomic: one bad item (archived product) and nothing is applied.
    e.core.product_set_active(t, &b, false).unwrap();
    let before = prices(&e);
    let bad =
        json!({ "items": [{ "product_id": a, "amount_minor": 990 }, { "product_id": b, "amount_minor": 990 }], "operation_id": op() });
    assert!(e.core.pricing_apply(t, serde_json::from_value(bad).unwrap()).is_err());
    assert_eq!(prices(&e), before);
    // Cashiers cannot apply prices.
    let (_, cashier) = e.user("Cash", "role_cashier", "5931");
    let c = json!({ "items": [{ "product_id": a, "amount_minor": 999 }], "operation_id": op() });
    assert_eq!(e.core.pricing_apply(&cashier, serde_json::from_value(c).unwrap()).unwrap_err().code, ErrorCode::Forbidden);
}

#[test]
fn the_assistant_cannot_price_below_a_minimum_margin() {
    let e = env();
    let a = e.product("Coffee", "9400030", 2_000, 1_000, 1_000);
    policy(&e, json!({ "name": "Min 25%", "scope": "global", "min_margin_bp": 2_500 }));
    // Floor: 1.000 × 1.10 ÷ 0.75 = 1.4667 → 1.467
    let err = e.core.ai_margin_guard(&[(a.clone(), 1_400)]).unwrap_err();
    assert_eq!(err.code, ErrorCode::Validation);
    assert!(err.message.contains("minimum margin"), "{}", err.message);
    assert!(e.core.ai_margin_guard(&[(a, 1_500)]).is_ok());
}

// ------------------------------------------------------------------ reports and summary

#[test]
fn channel_report_separates_channels_and_never_invents_one() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    e.product("Bread", "9500001", 1_000, 600, 50_000);
    // An older sale with no channel (as before Wave 5): written directly.
    e.core.pos_scan(t, "9500001", None).unwrap();
    let old = pay(&e, t);
    e.core
        .db
        .write(|c| {
            c.execute_batch(&format!(
                "DROP TRIGGER trg_sales_no_update; UPDATE sales SET channel=NULL WHERE sale_id='{}';
                 CREATE TRIGGER trg_sales_no_update BEFORE UPDATE ON sales BEGIN SELECT RAISE(ABORT, 'sales are immutable'); END;",
                old.sale_id
            ))?;
            Ok(())
        })
        .unwrap();
    e.core.pos_scan(t, "9500001", Some(2_000)).unwrap();
    pay(&e, t);
    e.core.pos_set_channel(t, "phone").unwrap();
    e.core.pos_scan(t, "9500001", Some(3_000)).unwrap();
    pay(&e, t);
    let r = e.core.report_run(t, "channels", Default::default()).unwrap();
    let by = |k: &str| r.rows.iter().find(|x| x["channel"] == k).cloned().unwrap();
    assert_eq!(by("pos")["sales"], 2_000);
    assert_eq!(by("phone")["sales"], 3_000);
    assert_eq!(by("")["key"], "Not recorded");
    assert_eq!(by("")["sales"], 1_000);
    assert!(r.notes.iter().any(|n| n.contains("not a profit per channel")));
    // Gross margin only with financial reports.
    assert!(by("pos")["gross_margin"].is_i64());
    let (_, inv) = e.user("Inv", "role_cashier", "5931");
    assert_eq!(e.core.report_run(&inv, "channels", Default::default()).unwrap_err().code, ErrorCode::Forbidden);
    // Price changes report lists applied changes only.
    let pid: String = one(&e, "SELECT product_id FROM products LIMIT 1");
    e.core.product_price_update(t, &pid, 1_100, Some("Supplier".into()), None).unwrap();
    let r = e.core.report_run(t, "price_changes", Default::default()).unwrap();
    let row = r.rows.iter().find(|x| x["new"] == 1_100).unwrap();
    assert_eq!((row["old"].as_i64(), row["change"].as_i64()), (Some(1_000), Some(100)));
}

#[test]
fn commercial_summary_counts_only_what_the_user_may_act_on() {
    let e = env();
    let t = &e.owner_token;
    e.product("Cola 330ml", "9600001", 250, 150, 1_000);
    e.product("COLA 330 ml", "9600002", 250, 150, 1_000);
    e.product("Loss leader", "9600003", 100, 200, 1_000);
    policy(&e, json!({ "name": "Min 10%", "scope": "global", "min_margin_bp": 1_000 }));
    let s = e.core.commercial_summary(t).unwrap();
    assert_eq!(s["likely_duplicates"], 1);
    assert_eq!(s["below_min_margin"], 1);
    let (_, cashier) = e.user("Cash", "role_cashier", "5931");
    assert_eq!(e.core.commercial_summary(&cashier).unwrap(), json!({}));
}

#[test]
fn the_assistant_cannot_set_wave5_fields_through_older_tools() {
    use amwapos_core::ai_tools::forbidden_key_in;
    assert_eq!(forbidden_key_in("products.update", &json!({ "product_id": "x", "plu": "42" })), Some("plu"));
    assert_eq!(
        forbidden_key_in("customers.save", &json!({ "customer": { "address_parts": null, "governorate": "capital" } })),
        Some("governorate")
    );
    assert_eq!(forbidden_key_in("orders.save", &json!({ "order": { "channel": "web" } })), Some("channel"));
    assert_eq!(forbidden_key_in("products.update", &json!({ "product_id": "x", "name": "y" })), None);
    // Every Wave 5 write has no AI tool.
    for cmd in [
        "products.merge",
        "scale_rules.save",
        "products.set_plu",
        "barcodes.set_kind",
        "pos.set_channel",
        "products.channel_price_set",
        "pricing.policy_save",
        "pricing.apply",
        "pricing.decide",
        "duplicates.decide",
    ] {
        assert!(amwapos_core::ai_tools::NO_TOOL.iter().any(|(c, _)| *c == cmd), "{cmd}");
    }
}

// ------------------------------------------------------------------ upgrade, permissions, invariants

#[test]
fn upgrading_fabricates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("amwapos.db");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        amwapos_core::db::migrate_until(&c, &path, 30).unwrap();
        c.execute_batch(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO products(product_id, sku, name, tax_rule_id, created_at, updated_at) VALUES ('p1','S1','Milk','t','x','x'), ('p2','S2','milk','t','x','x');
             INSERT INTO product_barcodes(barcode_id, product_id, barcode, is_primary, source, created_at) VALUES ('b1','p1','4006381333931',1,'manual','x');
             INSERT INTO product_prices(price_id, product_id, price_type, amount_minor, effective_from, created_at) VALUES ('pr1','p1','retail',500,'2026-01-01','x');
             INSERT INTO stock_lots(lot_id, lot_number, product_id, branch_id, received_at, qty_received_milli, unit_cost_minor, provenance, created_at)
               VALUES ('l1','L-00001','p1','br','x',5000,300,'receiving','x');
             INSERT INTO customers(customer_id, name, created_at, updated_at, building, block) VALUES ('c1','Ali','x','x','12','221');",
        )
        .unwrap();
    }
    let core =
        amwapos_core::service::AppCore::open(dir.path(), std::sync::Arc::new(amwapos_core::service::MemorySecretStore::default())).unwrap();
    let n = |sql: &str| core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get::<_, i64>(0))?)).unwrap();
    assert_eq!(n("SELECT COUNT(*) FROM product_barcodes WHERE kind IS NOT NULL"), 0, "no barcode kind guessed");
    assert_eq!(n("SELECT COUNT(*) FROM products WHERE plu IS NOT NULL OR merged_into_product_id IS NOT NULL"), 0);
    assert_eq!(
        n("SELECT (SELECT COUNT(*) FROM scale_barcode_rules) + (SELECT COUNT(*) FROM pricing_policies) + (SELECT COUNT(*) FROM product_merges)
            + (SELECT COUNT(*) FROM product_duplicate_decisions) + (SELECT COUNT(*) FROM price_recommendation_decisions) + (SELECT COUNT(*) FROM price_change_batches)"),
        0
    );
    assert_eq!(n("SELECT COUNT(*) FROM product_prices WHERE price_type<>'retail' OR policy_id IS NOT NULL OR batch_id IS NOT NULL"), 0);
    assert_eq!(n("SELECT COUNT(*) FROM customers WHERE governorate IS NOT NULL OR directions IS NOT NULL"), 0, "no governorate guessed");
    assert_eq!(
        n("SELECT COUNT(*) FROM stock_lots WHERE lot_id='l1' AND qty_received_milli=5000 AND provenance='receiving'"),
        1,
        "batches kept exactly"
    );
    assert_eq!(n("SELECT COUNT(*) FROM sales WHERE channel IS NOT NULL"), 0);
    // Batches stay immutable after the rebuild.
    assert!(core.db.write(|c| Ok(c.execute("UPDATE stock_lots SET qty_received_milli=1", [])?)).is_err());
}

#[test]
fn new_permissions_are_narrow_and_an_owner_removal_sticks() {
    let e = env();
    let perms = |role: &str| -> Vec<String> {
        e.core
            .db
            .read(|c| {
                let mut st = c.prepare("SELECT permission_code FROM role_permissions WHERE role_id=?1")?;
                let v = st.query_map([role], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
                Ok(v)
            })
            .unwrap()
    };
    let has = |role: &str, p: &str| perms(role).iter().any(|x| x == p);
    assert!(has("role_owner", "catalog.merge"));
    assert!(!has("role_manager", "catalog.merge"), "merging is the owner's");
    assert!(has("role_manager", "pricing.policy") && has("role_manager", "barcode_rules.manage"));
    for p in ["catalog.merge", "pricing.policy", "barcode_rules.manage", "prices.manage"] {
        assert!(!has("role_cashier", p), "{p}");
    }
    e.core
        .db
        .write(|tx| Ok(tx.execute("DELETE FROM role_permissions WHERE role_id='role_manager' AND permission_code='pricing.policy'", [])?))
        .unwrap();
    e.core.db.write(|tx| amwapos_core::auth::seed_roles(tx)).unwrap();
    assert!(!has("role_manager", "pricing.policy"), "an owner's removal is never re-granted");
}

/// The ten invariants of docs/PRICING_AND_CATALOGUE.md, checked together on
/// one store after a mixed day of work.
#[test]
fn wave5_invariants_hold_together() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 0);
    let a = e.product("Juice 1L", "9700001", 1_000, 600, 10_000);
    let b = e.product("Juice 1 L", "9700002", 1_000, 600, 5_000);
    let w = weighed(&e, "Olives", "42", 2_000);
    e.core.scale_rule_save(t, rule("W", "21", "weight", 0)).unwrap();
    e.core.scale_rule_save(t, rule("A", "2", "price", 0)).unwrap();
    // Sales: a normal one, a scale one.
    e.core.pos_scan(t, "9700002", Some(2_000)).unwrap();
    let s1 = pay(&e, t);
    let snap = |e: &Env| -> String {
        one(e, "SELECT group_concat(sale_item_id||':'||product_id||':'||line_total_minor||':'||effective_unit_price_minor, ',') FROM sale_items")
    };
    let receipts = |e: &Env| -> String { one(e, "SELECT group_concat(sha256, ',') FROM receipt_snapshots") };
    // 5. Two rules at the same priority: nothing is charged.
    let code = ean("210004201250");
    assert!(e.core.pos_scan(t, &code, None).is_err());
    assert!(e.core.pos_get_cart(t).unwrap().lines.is_empty());
    let before_items = snap(&e);
    let before_receipts = receipts(&e);
    let total = total_stock(&e);
    // 7. A policy never rewrites a price.
    let p_before = prices(&e);
    policy(
        &e,
        json!({ "name": "Global 40%", "scope": "global", "target_margin_bp": 4_000, "min_margin_bp": 2_000, "rounding_step_minor": 50 }),
    );
    review(&e, None);
    assert_eq!(prices(&e), p_before);
    // 1, 2, 3. Merge: total stock, receipts and sale snapshots unchanged.
    e.core.product_merge(t, merge_req(&e, &b, &a, MergeChoices::default())).unwrap();
    assert_eq!(total_stock(&e), total);
    assert_eq!(receipts(&e), before_receipts);
    assert_eq!(snap(&e), before_items);
    let _ = s1;
    // 4. One code, one product: the merged barcode answers only to the kept product.
    let owners: i64 = one(&e, "SELECT COUNT(DISTINCT product_id) FROM product_barcodes WHERE barcode='9700002'");
    assert_eq!(owners, 1);
    assert_eq!(e.core.barcode_add(t, &w, "9700002", false).unwrap_err().code, ErrorCode::Duplicate);
    // 8. Every recommendation meets its floor.
    for r in review(&e, None)["rows"].as_array().unwrap() {
        if let (Some(rec), Some(fp)) = (r["recommended_minor"].as_i64(), r["floor_price_minor"].as_i64()) {
            assert!(rec >= fp, "{r}");
        }
    }
    // 9. Retail unchanged without channel prices; 6. same cart + channel = same price.
    let mut seen = std::collections::HashSet::new();
    for ch in ["pos", "whatsapp", "pos"] {
        e.core.pos_set_channel(t, ch).unwrap();
        seen.insert(e.core.pos_scan(t, "9700001", Some(1_000)).unwrap().cart.totals.total_minor);
        e.core.pos_cancel_sale(t, None).unwrap();
    }
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec![1_000]);
    // 10. The till prices and sells with no hub, internet or AI: everything
    // above ran on a standalone store with the AI switched off.
    let ai: i64 = one(&e, "SELECT COUNT(*) FROM ai_messages");
    assert_eq!(ai, 0);
}
