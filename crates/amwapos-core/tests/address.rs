//! Bahrain addresses: Flat / Building / Road / Block compose the one-line
//! address; the block fills the area (the shop's own drops first).
mod common;

use amwapos_core::address::AddressParts;
use amwapos_core::auth::ROLE_CASHIER;
use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::{FinalizeRequest, Fulfilment};
use amwapos_core::tickets::TicketFilter;
use amwapos_core::ErrorCode;
use common::*;
use serde_json::json;

fn parts(flat: &str, bldg: &str, road: &str, block: &str) -> AddressParts {
    let o = |s: &str| (!s.is_empty()).then(|| s.to_string());
    AddressParts { flat: o(flat), building: o(bldg), road: o(road), block: o(block), landmark: None, ..Default::default() }
}

fn send_sale(e: &Env, t: &str, cu: &str, f: Fulfilment) -> String {
    e.core.pos_scan(t, "7001", Some(1000)).unwrap();
    let cart = e.core.pos_set_customer(t, Some(cu.into())).unwrap();
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
                fulfilment: Some(f),
            },
        )
        .unwrap()
        .delivery_id
        .unwrap()
}

#[test]
fn customer_parts_compose_the_address_and_the_block_fills_the_area() {
    let e = env();
    let c = e
        .core
        .customer_save(
            &e.owner_token,
            None,
            serde_json::from_value(json!({ "name": "Ahmed", "phone": "33338888",
                "address_parts": { "flat": "12", "building": "1203", "road": "2515", "block": "256", "landmark": "near the gate" } }))
            .unwrap(),
        )
        .unwrap();
    assert_eq!(c.info.address.as_deref(), Some("Flat 12, Bldg 1203, Road 2515, Block 256, near the gate"));
    assert_eq!(c.info.area.as_deref(), Some("Amwaj"), "block 256 is Amwaj");
    assert_eq!(c.info.address_parts.block.as_deref(), Some("256"));
    // A building is needed once any part is given.
    let bad = e.core.customer_save(
        &e.owner_token,
        None,
        serde_json::from_value(json!({ "name": "X", "address_parts": { "road": "1" } })).unwrap(),
    );
    assert_eq!(bad.unwrap_err().code, ErrorCode::Validation);
    // Free text still works (the UI sends address_parts: null for it).
    let f = e
        .core
        .customer_save(
            &e.owner_token,
            None,
            serde_json::from_value(json!({ "name": "Free", "address": "Villa 7, Riffa", "address_parts": null })).unwrap(),
        )
        .unwrap();
    let fu: Fulfilment = serde_json::from_value(json!({ "mode": "send", "address": "x", "address_parts": null })).unwrap();
    assert!(!fu.address_parts.is_structured());
    assert_eq!((f.info.address.as_deref(), f.info.area.as_deref()), (Some("Villa 7, Riffa"), Some("Riffa")));
    assert_eq!(e.core.block_area(&e.owner_token, "1205").unwrap().as_deref(), Some("Hamad Town"));
    assert_eq!(e.core.block_area(&e.owner_token, "999").unwrap(), None);
}

#[test]
fn a_drop_uses_its_own_parts_or_the_customers_and_the_shop_learns_blocks() {
    let e = env();
    e.product("Laban 1L", "7001", 450, 300, 100_000);
    let (_u, ct) = e.user("Cashier", ROLE_CASHIER, "2580");
    e.open_shift(&ct, 0);
    let cu = e
        .core
        .customer_save(
            &e.owner_token,
            None,
            serde_json::from_value(json!({ "name": "Sara", "phone": "33339999",
                "address_parts": { "building": "55", "road": "12", "block": "905" } }))
            .unwrap(),
        )
        .unwrap()
        .customer_id;
    // No address typed on Send: the customer's saved parts are used.
    let d1 = send_sale(&e, &ct, &cu, Fulfilment { mode: "send".into(), area: Some("Riffa East".into()), ..Default::default() });
    let t1 = e.core.ticket_get(&ct, &d1).unwrap();
    assert_eq!(t1["ticket"]["address"], "Bldg 55, Road 12, Block 905");
    // Block 905 is not on the known list; now the shop has used it for Riffa East.
    assert_eq!(e.core.block_area(&ct, "905").unwrap().as_deref(), Some("Riffa East"));
    // Parts typed for this drop win and fill the area from the block; saved on the customer when asked.
    let d2 = send_sale(
        &e,
        &ct,
        &cu,
        Fulfilment { mode: "send".into(), address_parts: parts("3", "7", "4518", "257"), save_on_customer: true, ..Default::default() },
    );
    let rows = e.core.tickets_list(&ct, TicketFilter::default()).unwrap();
    let t2 = rows.iter().find(|r| r.delivery_id.as_deref() == Some(d2.as_str())).unwrap();
    assert_eq!((t2.address.as_deref(), t2.area.as_deref()), (Some("Flat 3, Bldg 7, Road 4518, Block 257"), Some("Amwaj")));
    let c = e.core.customer_get(&e.owner_token, &cu).unwrap();
    assert_eq!(c["customer"]["address_parts"]["block"], "257");
    assert_eq!(c["customer"]["address"], "Flat 3, Bldg 7, Road 4518, Block 257");
}

#[test]
fn the_block_list_is_an_editable_setting_and_shop_history_still_wins() {
    let e = env();
    let t = &e.owner_token;
    // Starter rows ship as data, not code.
    let d = e.core.settings_get(t, "delivery").unwrap();
    assert!(d["blocks"].as_array().unwrap().iter().any(|r| r["area"] == "Amwaj" && r["from"] == 256));
    // The owner corrects it: 256 is now "Amwaj Islands"; a new range is added.
    e.core
        .settings_save(
            t,
            "delivery",
            json!({ "blocks": [ { "from": 256, "to": 258, "area": "Amwaj Islands" }, { "from": 901, "to": 939, "area": "Riffa East" } ] }),
        )
        .unwrap();
    assert_eq!(e.core.block_area(t, "256").unwrap().as_deref(), Some("Amwaj Islands"));
    assert_eq!(e.core.block_area(t, "905").unwrap().as_deref(), Some("Riffa East"));
    assert_eq!(e.core.block_area(t, "1205").unwrap(), None, "removed rows are gone");
    // Overlapping or empty rows are refused.
    let bad = e.core.settings_save(
        t,
        "delivery",
        json!({ "blocks": [ { "from": 1, "to": 10, "area": "A" }, { "from": 5, "to": 20, "area": "B" } ] }),
    );
    assert_eq!(bad.unwrap_err().code, ErrorCode::Validation);
    let bad = e.core.settings_save(t, "delivery", json!({ "blocks": [ { "from": 1, "to": 10, "area": " " } ] }));
    assert_eq!(bad.unwrap_err().code, ErrorCode::Validation);
    // The shop's own history for a block beats the list.
    e.core
        .customer_save(
            t,
            None,
            serde_json::from_value(json!({ "name": "H", "area": "Galali", "address_parts": { "building": "1", "block": "256" } })).unwrap(),
        )
        .unwrap();
    assert_eq!(e.core.block_area(t, "256").unwrap().as_deref(), Some("Galali"));
}
