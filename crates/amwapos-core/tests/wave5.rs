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
