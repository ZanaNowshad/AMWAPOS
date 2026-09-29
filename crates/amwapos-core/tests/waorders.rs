//! WhatsApp AI orders through the real ingestion path (`wa_ingest`, the same
//! call the WhatsApp adapter makes) and the processor: conversations become
//! reviewable drafts, clarifications resolve follow-ups, staff overrides win,
//! payment screenshots never settle anything, and nothing sells or moves stock.

mod common;

use amwapos_core::messaging::Inbound;
use amwapos_core::ErrorCode;
use common::*;
use serde_json::{json, Value};

const CHAT: &str = "97333112233@s.whatsapp.net";

fn setup() -> Env {
    let e = env();
    let t = &e.owner_token;
    e.core
        .settings_save(t, "features", json!({ "whatsapp.enabled": true, "orders.digital": true, "orders.whatsapp_ai": true, "ocr.enabled": true, "ocr.payment_screenshots": true }))
        .unwrap();
    let mut d = e.core.settings_get(t, "delivery").unwrap();
    d["zones"] = json!([{ "name": "Zone A", "blocks": [{ "from": 200, "to": 260 }], "areas": [], "fee_minor": 500, "free_over_minor": 20000, "active": true }]);
    e.core.settings_save(t, "delivery", d).unwrap();
    for (name, bc, price, stock) in [
        ("Coca-Cola Original 330 ml", "5449000000996", 150, 50_000),
        ("Coca-Cola Original 1.5 L", "5449000054227", 600, 20_000),
        ("Coca-Cola Original 2.25 L", "5449000133335", 800, 10_000),
        ("Coca-Cola Zero 330 ml", "5449000131805", 150, 40_000),
        ("Lay's Cheese 50 g", "6281036000011", 250, 30_000),
        ("Lay's Salt 50 g", "6281036000028", 250, 30_000),
        ("Almarai Milk 1 L", "6281007000013", 900, 10_000),
        ("Almarai Milk 250 ml", "6281007000020", 300, 10_000),
        ("Water 500 ml", "6291100000011", 100, 0),
        ("Water 1.5 L", "6291100000028", 200, 12_000),
    ] {
        e.product(name, bc, price, price / 2, stock);
    }
    e
}

fn send(e: &Env, id: &str, text: &str) {
    e.core
        .wa_ingest(&[Inbound {
            wa_id: id.into(),
            chat: CHAT.into(),
            sender_pn: None,
            push_name: Some("Ali".into()),
            ts: chrono::Utc::now().timestamp(),
            kind: "text".into(),
            text: Some(text.into()),
            caption: None,
            media_mime: None,
            media_ref: None,
        }])
        .unwrap();
    e.core.wa_orders_process(50).unwrap();
}

fn session(e: &Env) -> Value {
    let list = e.core.wa_orders_list(&e.owner_token, Some("all".into())).unwrap();
    let sid = list.iter().find(|s| s["chat"] == CHAT && s["state"] != "cancelled" && s["state"] != "confirmed").or(list.first()).unwrap()
        ["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    e.core.wa_order_get(&e.owner_token, &sid).unwrap()
}

fn count(e: &Env, sql: &str) -> i64 {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn line<'a>(v: &'a Value, name_part: &str) -> &'a Value {
    v["order"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"].as_str().unwrap_or("").contains(name_part) || l["requested"].as_str().unwrap_or("").contains(name_part))
        .unwrap_or_else(|| panic!("no line {name_part}: {}", v["order"]))
}

#[test]
fn the_example_conversation_becomes_a_reviewable_draft() {
    let e = setup();
    send(&e, "M1", "2 coke big, one lays cheese, milk small and deliver to block 221");
    let v = session(&e);
    assert_eq!(v["session"]["intent"], "new_order");
    assert_eq!(v["order"]["status"], "draft");
    // Coke "big": 1.5 L or 2.25 L — asked, not guessed.
    let coke = line(&v, "coke");
    assert!(coke["product_id"].is_null() && coke["resolution"] == "ambiguous", "{coke}");
    let q = v["session"]["questions"].to_string();
    assert!(q.contains("Which Coca-Cola size would you like: 1) 1.5 L, 2) 2.25 L?"), "{q}");
    assert_eq!(line(&v, "Lay's Cheese")["qty_milli"], 1000);
    assert!(line(&v, "Almarai Milk 250 ml")["product_id"].is_string(), "small milk = the 250 ml");
    // Block 221 → Zone A → the configured fee; the building is asked for.
    assert_eq!(v["session"]["delivery_fee_minor"], 500);
    assert!(q.contains("building"), "{q}");
    assert_eq!(v["session"]["state"], "clarifying");

    // The follow-up "2.25" answers the size question; it is not a quantity.
    send(&e, "M2", "2.25");
    let v = session(&e);
    let coke = line(&v, "Coca-Cola Original 2.25 L");
    assert_eq!((coke["qty_milli"].as_i64(), coke["resolution"].as_str()), (Some(2000), Some("resolved")));
    send(&e, "M3", "building 12");
    let v = session(&e);
    assert_eq!(v["session"]["state"], "ready", "{}", v["session"]["questions"]);
    // Totals from POS prices: 2×0.800 + 0.250 + 0.300 = 2.150; delivery 0.500.
    assert_eq!(v["order"]["subtotal_minor"], 2_150);
    assert_eq!(v["order"]["total_minor"], 2_650);
    let reply = v["suggested_reply"].as_str().unwrap();
    for part in [
        "Your order:",
        "2 × Coca-Cola Original 2.25 L",
        "1 × Lay's Cheese 50 g",
        "1 × Almarai Milk 250 ml",
        "Delivery: Bldg 12, Block 221",
        "Subtotal: 2.150 BHD",
        "Delivery: 0.500 BHD",
        "Total: 2.650 BHD",
    ] {
        assert!(reply.contains(part), "{part} missing in {reply}");
    }
    let summary = v["summary"].as_str().unwrap();
    assert!(summary.starts_with("Customer wants delivery to Bldg 12, Block 221."), "{summary}");
    assert!(summary.contains("Customer has not confirmed payment."), "{summary}");
    // Nothing was sold, sent or moved.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM sales"), 0);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_movements WHERE type<>'opening'"), 0);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_outbox"), 0, "no automatic reply");
    // Staff confirm through the normal order confirmation.
    let rev = v["session"]["revision"].as_i64().unwrap();
    assert_eq!(
        e.core.wa_order_confirm(&e.owner_token, v["session"]["session_id"].as_str().unwrap(), rev - 1).unwrap_err().code,
        ErrorCode::Conflict
    );
    let v = e.core.wa_order_confirm(&e.owner_token, v["session"]["session_id"].as_str().unwrap(), rev).unwrap();
    assert_eq!(v["order"]["status"], "confirmed");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM sales"), 0, "confirming is not selling");
}

#[test]
fn questions_greetings_and_spam_make_no_draft() {
    let e = setup();
    send(&e, "Q0", "hello");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_order_sessions"), 0);
    send(&e, "Q1", "how much is coke 1.5L?");
    let v = session(&e);
    assert_eq!(v["session"]["intent"], "price_question");
    assert!(v["order"].is_null(), "no draft for a question");
    assert!(v["suggested_reply"].as_str().unwrap().contains("Coca-Cola Original 1.5 L: 0.600 BHD"), "{}", v["suggested_reply"]);
    send(&e, "Q2", "WIN a free gift!! click https://x.example/promo");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_inbox_processing WHERE intent='spam' AND status='skipped'"), 1);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM digital_orders"), 0);
}

#[test]
fn modifications_substitutions_and_cancellation() {
    let e = setup();
    send(&e, "A1", "3 coke 330ml and 1 milk 1L, pickup");
    let v = session(&e);
    assert_eq!(line(&v, "Coca-Cola Original 330 ml")["qty_milli"], 3000);
    assert_eq!(v["session"]["delivery_mode"], "pickup");
    assert_eq!(v["session"]["fee_state"], "not_applicable");
    send(&e, "A2", "make coke 5");
    assert_eq!(line(&session(&e), "Coca-Cola Original 330 ml")["qty_milli"], 5000);
    send(&e, "A3", "remove milk");
    assert!(session(&e)["order"]["lines"].as_array().unwrap().iter().all(|l| !l["name"].as_str().unwrap().contains("Milk")));
    send(&e, "A4", "same but coke zero");
    let v = session(&e);
    let z = line(&v, "Coca-Cola Zero");
    assert_eq!(z["qty_milli"], 5000, "the replaced line keeps its quantity: {}", v["order"]);
    assert!(v["order"]["lines"].as_array().unwrap().iter().all(|l| l["name"] != "Coca-Cola Original 330 ml"));
    // Out of stock: real in-stock alternatives, and confirming is refused until replaced.
    send(&e, "A5", "add 2 water 500ml");
    let v = session(&e);
    let w = line(&v, "Water 500 ml");
    assert_eq!(w["resolution"], "unavailable");
    assert!(w["alternatives"].to_string().contains("Water 1.5 L"), "{w}");
    let sid = v["session"]["session_id"].as_str().unwrap().to_string();
    assert!(e
        .core
        .wa_order_confirm(&e.owner_token, &sid, v["session"]["revision"].as_i64().unwrap())
        .unwrap_err()
        .message
        .contains("out of stock"));
    send(&e, "A6", "cancel it");
    let v = e.core.wa_order_get(&e.owner_token, &sid).unwrap();
    assert_eq!(v["session"]["state"], "cancelled");
    assert_eq!(v["order"]["status"], "cancelled");
}

#[test]
fn arabic_mixed_and_unknown_products() {
    let e = setup();
    send(&e, "R1", "ابغى ٢ بيبسي و ٣ حليب ٢٥٠ مل");
    let v = session(&e);
    assert_eq!(v["session"]["intent"], "new_order");
    let milk = line(&v, "Almarai Milk 250 ml");
    assert_eq!(milk["qty_milli"], 3000);
    // "Pepsi" is not in the catalogue: never invented, the customer is asked.
    let p = v["order"]["lines"].as_array().unwrap().iter().find(|l| l["product_id"].is_null()).unwrap();
    assert_eq!(p["resolution"], "unmatched");
    assert!(v["session"]["questions"].to_string().contains("We couldn't find"), "{}", v["session"]["questions"]);
    send(&e, "R2", "randu paal 1L");
    let v = session(&e);
    assert_eq!(line(&v, "Almarai Milk 1 L")["qty_milli"], 2000, "Manglish: randu = 2, paal = milk");
}

#[test]
fn staff_overrides_are_kept_and_learned() {
    let e = setup();
    send(&e, "S1", "2 coke big");
    let v = session(&e);
    let sid = v["session"]["session_id"].as_str().unwrap().to_string();
    let no = line(&v, "coke")["line_no"].as_i64().unwrap();
    let big = e
        .core
        .db
        .read(|c| Ok(c.query_row("SELECT product_id FROM products WHERE name='Coca-Cola Original 2.25 L'", [], |r| r.get::<_, String>(0))?))
        .unwrap();
    let (_, cashier) = e.user("Cash", "role_cashier", "1357");
    let denied =
        e.core.wa_order_line(&cashier, &sid, v["session"]["revision"].as_i64().unwrap(), Some(no), Some(big.clone()), None, false, true);
    assert_eq!(denied.unwrap_err().code, ErrorCode::Forbidden, "cashiers cannot edit drafts");
    let v = e
        .core
        .wa_order_line(&e.owner_token, &sid, v["session"]["revision"].as_i64().unwrap(), Some(no), Some(big.clone()), None, false, true)
        .unwrap();
    assert_eq!(line(&v, "2.25 L")["locked"], true);
    // A later customer message does not change the staff's line.
    send(&e, "S2", "make coke 7");
    let v = session(&e);
    assert_eq!(line(&v, "2.25 L")["qty_milli"], 2000);
    assert!(v["events"].to_string().contains("skipped_locked_line"));
    // The phrase is learned: next time "coke big" is the 2.25 L directly.
    e.core.wa_order_cancel(&e.owner_token, &sid).unwrap();
    send(&e, "S3", "1 coke big");
    let v = session(&e);
    assert_eq!(line(&v, "2.25 L")["resolution"], "resolved", "{}", v["order"]);
    // Takeover: the interpreter stops changing this conversation.
    let sid = v["session"]["session_id"].as_str().unwrap().to_string();
    e.core.wa_order_flags(&e.owner_token, &sid, Some(true), None, None).unwrap();
    send(&e, "S4", "add 3 lays cheese");
    let v = session(&e);
    assert_eq!(v["order"]["lines"].as_array().unwrap().len(), 1);
    assert!(v["events"].to_string().contains("message_during_takeover"));
}

#[test]
fn idempotent_ingestion_and_reprocessing() {
    let e = setup();
    send(&e, "D1", "2 lays cheese");
    send(&e, "D1", "2 lays cheese");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_inbox"), 1);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_inbox_processing"), 1);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM digital_order_lines"), 1);
    // A second pass (a restart) interprets nothing again.
    assert!(e.core.wa_orders_process(50).unwrap().is_empty());
    assert_eq!(line(&session(&e), "Lay's Cheese")["qty_milli"], 2000);
}

#[test]
fn customers_are_matched_not_duplicated() {
    let e = setup();
    let t = &e.owner_token;
    let c = e.core.customer_save(t, None, serde_json::from_value(json!({ "name": "Ali Hasan", "phone": "33112233" })).unwrap()).unwrap();
    send(&e, "C1", "1 lays salt");
    let v = session(&e);
    assert_eq!(v["session"]["customer_id"], json!(c.customer_id), "{}", v["session"]);
    let before = count(&e, "SELECT COUNT(*) FROM customers");
    // A number with no customer stays provisional; no record is created.
    e.core
        .wa_ingest(&[Inbound {
            wa_id: "C2".into(),
            chat: "97339998877@s.whatsapp.net".into(),
            sender_pn: None,
            push_name: None,
            ts: chrono::Utc::now().timestamp(),
            kind: "text".into(),
            text: Some("2 lays salt".into()),
            caption: None,
            media_mime: None,
            media_ref: None,
        }])
        .unwrap();
    e.core.wa_orders_process(50).unwrap();
    let list = e.core.wa_orders_list(t, Some("all".into())).unwrap();
    let other = list.iter().find(|s| s["chat"] == "97339998877@s.whatsapp.net").unwrap();
    assert_eq!(other["customer_state"], "provisional");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM customers"), before);
}

#[test]
fn payment_screenshots_attach_but_never_verify() {
    let e = setup();
    send(&e, "P1", "1 lays cheese pickup");
    e.core
        .wa_ingest(&[Inbound {
            wa_id: "P2".into(),
            chat: CHAT.into(),
            sender_pn: None,
            push_name: None,
            ts: chrono::Utc::now().timestamp(),
            kind: "image".into(),
            text: None,
            caption: Some("paid".into()),
            media_mime: Some("image/jpeg".into()),
            media_ref: Some("{}".into()),
        }])
        .unwrap();
    e.core.wa_orders_process(50).unwrap();
    let seq = e.core.db.read(|c| Ok(c.query_row("SELECT seq FROM wa_inbox WHERE wa_id='P2'", [], |r| r.get::<_, i64>(0))?)).unwrap();
    // The media arrives later (the existing download path opens a payment review).
    e.core.wa_media_saved(seq, b"\xff\xd8jpeg", Some("image/jpeg")).unwrap();
    let v = session(&e);
    assert_eq!(v["order"]["payment_state"], "screenshot_pending");
    let ev = v["payment_evidence"].as_array().unwrap();
    assert_eq!(ev.len(), 1, "{v}");
    assert_eq!(ev[0]["verified"], false);
    assert!(v["summary"].as_str().unwrap().contains("needs staff verification"));
    let sid = v["session"]["session_id"].as_str().unwrap().to_string();
    let rid = ev[0]["review_id"].as_str().unwrap().to_string();
    // Verifying needs payments.review.
    let (_, delivery) = e.user("Rider", "role_delivery", "1470");
    assert_eq!(e.core.wa_order_payment(&delivery, &sid, &rid, "verified", None).unwrap_err().code, ErrorCode::Forbidden);
    let v = e.core.wa_order_payment(&e.owner_token, &sid, &rid, "verified", None).unwrap();
    assert_eq!(v["order"]["payment_state"], "recorded");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM payments"), 0, "no payment is taken outside the till");
}

#[test]
fn priority_and_confirmed_orders_are_not_changed() {
    let e = setup();
    send(&e, "U1", "2 lays cheese, pickup");
    let v = session(&e);
    let sid = v["session"]["session_id"].as_str().unwrap().to_string();
    e.core.wa_order_confirm(&e.owner_token, &sid, v["session"]["revision"].as_i64().unwrap()).unwrap();
    let order = v["order"]["order_id"].as_str().unwrap().to_string();
    // After confirmation a change request never mutates the confirmed order.
    send(&e, "U2", "where is my order?? still waiting, urgent");
    let lines = e.core.order_get(&e.owner_token, &order).unwrap().lines;
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].qty_milli, 2000);
    let v = session(&e);
    assert_eq!(v["session"]["priority"], "high");
    assert!(v["session"]["priority_reasons"].to_string().contains("customer_waiting"));
    assert_eq!(v["session"]["intent"], "support_issue");
}

#[test]
fn ai_help_is_validated_and_never_stale() {
    let e = setup();
    // No real provider (the default test model): rules only, nothing breaks.
    send(&e, "X1", "1 lays chese");
    assert!(e.core.wa_order_ai_turn(1).unwrap().is_none());
    let v = session(&e);
    let seq = e.core.db.read(|c| Ok(c.query_row("SELECT seq FROM wa_inbox WHERE wa_id='X1'", [], |r| r.get::<_, i64>(0))?)).unwrap();
    let rev = v["session"]["revision"].as_i64().unwrap();
    let lays: Vec<String> = e
        .core
        .db
        .read(|c| {
            let mut st = c.prepare("SELECT product_id FROM products WHERE name LIKE 'Lay''s%' ORDER BY name")?;
            let r = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            Ok(r)
        })
        .unwrap();
    let unresolved = v["order"]["lines"].as_array().unwrap().iter().any(|l| l["product_id"].is_null());
    if unresolved {
        // A fabricated id is ignored; a real candidate resolves the line.
        assert!(!e
            .core
            .wa_order_apply_ai(
                seq,
                rev,
                &lays,
                &json!({ "intent": "new_order", "items": [{ "text": "lays chese", "qty": 1, "product_id": "01FAKE" }] }),
                "t:m"
            )
            .unwrap());
        let rev = session(&e)["session"]["revision"].as_i64().unwrap();
        // A stale result (the conversation changed) is dropped.
        assert!(!e
            .core
            .wa_order_apply_ai(seq, rev - 1, &lays, &json!({ "items": [{ "text": "lays chese", "product_id": lays[0] }] }), "t:m")
            .unwrap());
        assert!(e
            .core
            .wa_order_apply_ai(seq, rev, &lays, &json!({ "items": [{ "text": "lays chese", "product_id": lays[0] }] }), "t:m")
            .unwrap());
        assert!(session(&e)["events"].to_string().contains("\"source\":\"ai\""));
    }
    assert_eq!(count(&e, "SELECT COUNT(*) FROM sales"), 0);
}
