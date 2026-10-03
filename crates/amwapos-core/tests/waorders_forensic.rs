//! Forensic audit of WhatsApp AI orders (platform pass): one test per
//! invariant, through the real ingestion path (`wa_ingest`) and processor.
//!
//! Invariants: a message mutates state at most once; conversations are
//! isolated; completed orders are immutable; staff beat AI; only real
//! catalogue products; prices from AMWAPOS; drafts never move stock; a
//! screenshot never proves payment; nothing is sent by itself; late and
//! out-of-order messages are never applied to the current draft; a new order
//! is never appended to an old one; stock is checked again at confirmation.

mod common;

use amwapos_core::messaging::Inbound;
use amwapos_core::ErrorCode;
use common::*;
use serde_json::{json, Value};

const A: &str = "97333112233@s.whatsapp.net";
const B: &str = "97333445566@s.whatsapp.net";

fn setup() -> Env {
    let e = env();
    let t = &e.owner_token;
    e.core
        .settings_save(
            t,
            "features",
            json!({ "whatsapp.enabled": true, "orders.digital": true, "orders.whatsapp_ai": true, "ocr.enabled": true, "ocr.payment_screenshots": true }),
        )
        .unwrap();
    for (name, bc, price, stock) in [
        ("Coca-Cola Original 1.5 L", "5449000054227", 600, 20_000),
        ("Coca-Cola Zero 1.5 L", "5449000131806", 600, 20_000),
        ("Lay's Cheese 50 g", "6281036000011", 250, 30_000),
        ("Almarai Milk 1 L", "6281007000013", 900, 10_000),
        ("Arabic Bread 6 pcs", "6291100000035", 300, 10_000),
    ] {
        e.product(name, bc, price, price / 2, stock);
    }
    e
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn msg(id: &str, chat: &str, ts: i64, kind: &str, text: Option<&str>) -> Inbound {
    Inbound {
        wa_id: id.into(),
        chat: chat.into(),
        sender_pn: None,
        push_name: Some("Customer".into()),
        ts,
        kind: kind.into(),
        text: if kind == "text" { text.map(|t| t.to_string()) } else { None },
        caption: if kind == "text" { None } else { text.map(|t| t.to_string()) },
        media_mime: (kind == "image").then(|| "image/jpeg".to_string()),
        media_ref: (kind == "image").then(|| "{}".to_string()),
    }
}

/// Deliver and process one text message at a given time.
fn say(e: &Env, id: &str, chat: &str, ts: i64, text: &str) {
    e.core.wa_ingest(&[msg(id, chat, ts, "text", Some(text))]).unwrap();
    e.core.wa_orders_process(50).unwrap();
}

fn time_ago(secs: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(secs)).format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn count(e: &Env, sql: &str) -> i64 {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

/// The newest conversation of a chat.
fn conv(e: &Env, chat: &str) -> Value {
    let sid: String = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row(
                "SELECT session_id FROM wa_order_sessions WHERE chat=?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                [chat],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    e.core.wa_order_get(&e.owner_token, &sid).unwrap()
}

fn sessions(e: &Env, chat: &str) -> i64 {
    count(e, &format!("SELECT COUNT(*) FROM wa_order_sessions WHERE chat='{chat}'"))
}

fn lines(v: &Value) -> Vec<(String, i64)> {
    v["order"]["lines"]
        .as_array()
        .map(|a| a.iter().map(|l| (l["name"].as_str().unwrap_or("").to_string(), l["qty_milli"].as_i64().unwrap_or(0))).collect())
        .unwrap_or_default()
}

fn qty_of(v: &Value, part: &str) -> Option<i64> {
    lines(v).into_iter().find(|(n, _)| n.contains(part)).map(|(_, q)| q)
}

fn seq_of(e: &Env, wa_id: &str) -> i64 {
    e.core.db.read(|c| Ok(c.query_row("SELECT seq FROM wa_inbox WHERE wa_id=?1", [wa_id], |r| r.get(0))?)).unwrap()
}

fn rev(v: &Value) -> i64 {
    v["session"]["revision"].as_i64().unwrap()
}

fn sid(v: &Value) -> String {
    v["session"]["session_id"].as_str().unwrap().to_string()
}

fn pid(e: &Env, name: &str) -> String {
    e.core.db.read(|c| Ok(c.query_row("SELECT product_id FROM products WHERE name=?1", [name], |r| r.get(0))?)).unwrap()
}

#[test]
fn a_message_changes_state_at_most_once_even_when_redelivered() {
    let e = setup();
    let t = now();
    say(&e, "R1", A, t, "2 lays cheese pickup");
    // WhatsApp redelivers after a reconnect; the worker runs again.
    e.core.wa_ingest(&[msg("R1", A, t, "text", Some("2 lays cheese pickup"))]).unwrap();
    for _ in 0..3 {
        e.core.wa_orders_process(50).unwrap();
    }
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_inbox"), 1);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_inbox_processing"), 1);
    assert_eq!(qty_of(&conv(&e, A), "Lay's"), Some(2000));
}

#[test]
fn one_customer_never_changes_another_customers_draft() {
    let e = setup();
    let t = now();
    say(&e, "I1", A, t, "2 lays cheese pickup");
    say(&e, "I2", B, t + 5, "remove lays");
    say(&e, "I3", B, t + 6, "make lays 9");
    assert_eq!(qty_of(&conv(&e, A), "Lay's"), Some(2000));
    assert_eq!(sessions(&e, B), 0, "a change with no open order of its own starts nothing");
}

#[test]
fn negations_questions_and_comments_never_order_anything() {
    let e = setup();
    let t = now();
    // No open draft: nothing here is an order.
    for (i, text) in [
        "don't add coke",
        "coke is too expensive",
        "Last time Coke was 1.5 BD",
        "why did you send 2 coke",
        "stop sending me messages",
        "I was asking the price, not ordering",
    ]
    .iter()
    .enumerate()
    {
        say(&e, &format!("N{i}"), A, t + i as i64, text);
    }
    assert_eq!(count(&e, "SELECT COUNT(*) FROM digital_orders"), 0, "no draft from a negation, question or comment");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM digital_order_lines"), 0);
    // With an open draft: negations remove or do nothing; they never add.
    say(&e, "N10", B, t + 20, "1 lays cheese pickup");
    for (i, text) in
        ["don't send coke", "why did you send 2 coke", "not zero, normal", "milk vendam", "coke venda", "لا ترسل كولا"].iter().enumerate()
    {
        say(&e, &format!("N2{i}"), B, t + 30 + i as i64, text);
    }
    let v = conv(&e, B);
    assert_eq!(lines(&v), vec![("Lay's Cheese 50 g".to_string(), 1000)], "{}", v["order"]);
    assert_eq!(v["order"]["status"], "draft");
}

#[test]
fn corrections_follow_the_customer_and_unclear_ones_wait_for_a_person() {
    let e = setup();
    let t = now();
    say(&e, "C1", A, t, "2 coke 1.5, 1 milk, 1 lays cheese, pickup");
    let v = conv(&e, A);
    assert_eq!(qty_of(&v, "Coca-Cola Original"), Some(2000), "{}", v["order"]);
    say(&e, "C2", A, t + 10, "make coke 3");
    assert_eq!(qty_of(&conv(&e, A), "Coca-Cola Original"), Some(3000));
    // "actually only one": which item? Nothing changes.
    say(&e, "C3", A, t + 20, "actually only one");
    assert_eq!(qty_of(&conv(&e, A), "Coca-Cola Original"), Some(3000));
    say(&e, "C4", A, t + 30, "cancel the milk");
    let v = conv(&e, A);
    assert_eq!(qty_of(&v, "Milk"), None, "the milk only, not the order");
    assert_eq!(v["order"]["status"], "draft");
    // Repeating an item already in the draft does not double it; a new number is asked.
    say(&e, "C5", A, t + 40, "1 lays cheese");
    assert_eq!(qty_of(&conv(&e, A), "Lay's"), Some(1000));
    say(&e, "C6", A, t + 50, "4 lays cheese");
    let v = conv(&e, A);
    assert_eq!(qty_of(&v, "Lay's"), Some(1000));
    assert!(v["session"]["questions"].to_string().contains("in total"), "{}", v["session"]["questions"]);
    // An explicit addition raises it.
    say(&e, "C7", A, t + 60, "add 1 more lays cheese");
    assert_eq!(qty_of(&conv(&e, A), "Lay's"), Some(2000));
    say(&e, "C8", A, t + 70, "send 2 coke not 3");
    assert_eq!(qty_of(&conv(&e, A), "Coca-Cola Original"), Some(2000));
    // "cancel that": a person decides.
    say(&e, "C9", A, t + 80, "cancel that");
    let v = conv(&e, A);
    assert_eq!(v["order"]["status"], "draft");
    assert!(v["session"]["priority_reasons"].to_string().contains("cancel_request_unclear"));
    // "remove the coke and add 1 bread": two changes.
    say(&e, "C10", A, t + 90, "remove the coke and add 1 bread");
    let v = conv(&e, A);
    assert_eq!(qty_of(&v, "Coca-Cola"), None, "{}", v["order"]);
    assert_eq!(qty_of(&v, "Bread"), Some(1000));
    assert_eq!(count(&e, "SELECT COUNT(*) FROM sales"), 0);
}

#[test]
fn a_new_order_never_joins_a_confirmed_cancelled_or_stale_one() {
    let e = setup();
    let t = now() - 3600;
    // 1. Confirmed, then a new order: a new draft; the first is untouched.
    say(&e, "O1", A, t, "2 lays cheese pickup");
    let v = conv(&e, A);
    let first = v["order"]["order_id"].as_str().unwrap().to_string();
    e.core.wa_order_confirm(&e.owner_token, &sid(&v), rev(&v)).unwrap();
    say(&e, "O2", A, t + 600, "1 milk pickup");
    let v2 = conv(&e, A);
    assert_ne!(v2["order"]["order_id"].as_str().unwrap(), first);
    assert_eq!(lines(&v2), vec![("Almarai Milk 1 L".to_string(), 1000)]);
    assert!(v2["session"]["priority_reasons"].to_string().contains("other_open_order"), "the confirmed order is pointed out");
    assert_eq!(e.core.order_get(&e.owner_token, &first).unwrap().lines.len(), 1);
    // A change with no open draft is shown on the confirmed order, never applied.
    say(&e, "O2b", B, t + 700, "3 lays cheese pickup");
    let vb = conv(&e, B);
    e.core.wa_order_confirm(&e.owner_token, &sid(&vb), rev(&vb)).unwrap();
    say(&e, "O2c", B, t + 800, "remove lays");
    assert_eq!(sessions(&e, B), 1, "no new draft for Lay's from \"remove lays\"");
    let vb = conv(&e, B);
    assert!(vb["events"].to_string().contains("message_after_close"), "{}", vb["events"]);
    // 2. Cancelled by the customer, then a new order: a new draft.
    say(&e, "O3", A, t + 900, "cancel my order");
    assert_eq!(conv(&e, A)["order"]["status"], "cancelled");
    say(&e, "O4", A, t + 1200, "1 bread pickup");
    let v3 = conv(&e, A);
    assert_eq!(v3["order"]["status"], "draft");
    assert_eq!(lines(&v3), vec![("Arabic Bread 6 pcs".to_string(), 1000)]);
    // 3. Two days later, an open draft is not reused (the conversation is aged;
    // messages older than the interpretation window are never read at all).
    let old = time_ago(2 * 86_400);
    e.core
        .db
        .write(|c| {
            Ok(c.execute(
                "UPDATE wa_order_sessions SET last_message_at=?2 WHERE chat=?1 AND state IN ('collecting','clarifying','ready')",
                rusqlite::params![A, old],
            )?)
        })
        .unwrap();
    say(&e, "O5", A, now(), "2 coke 1.5 pickup");
    let v4 = conv(&e, A);
    assert_eq!(lines(&v4), vec![("Coca-Cola Original 1.5 L".to_string(), 2000)], "{}", v4["order"]);
    assert_eq!(e.core.order_get(&e.owner_token, v3["order"]["order_id"].as_str().unwrap()).unwrap().lines.len(), 1);
}

#[test]
fn late_and_out_of_order_messages_are_never_applied_to_the_current_draft() {
    let e = setup();
    let t = now();
    say(&e, "L1", A, t, "2 coke 1.5 pickup");
    say(&e, "L3", A, t + 60, "make coke 3");
    // L2 was written before L3 but arrives after it (reconnect): shown, not applied.
    say(&e, "L2", A, t + 30, "make coke 5");
    let v = conv(&e, A);
    assert_eq!(qty_of(&v, "Coca-Cola"), Some(3000));
    assert!(v["events"].to_string().contains("late_message"));
    assert!(v["session"]["priority_reasons"].to_string().contains("late_message"));
    // Several messages delivered together are read in the order they were written.
    e.core.wa_ingest(&[msg("L5", A, t + 200, "text", Some("remove coke")), msg("L4", A, t + 100, "text", Some("add 1 bread"))]).unwrap();
    e.core.wa_orders_process(50).unwrap();
    let v = conv(&e, A);
    assert_eq!(qty_of(&v, "Coca-Cola"), None, "{}", v["order"]);
    assert_eq!(qty_of(&v, "Bread"), Some(1000));
}

#[test]
fn a_staff_edit_beats_a_stale_ai_result() {
    let e = setup();
    let t = now();
    say(&e, "S1", A, t, "1 lays chese and 1 milk pickup");
    let v = conv(&e, A);
    let seq = seq_of(&e, "S1");
    let ai_rev = rev(&v);
    let cheese = pid(&e, "Lay's Cheese 50 g");
    let milk = pid(&e, "Almarai Milk 1 L");
    let lays_line =
        v["order"]["lines"].as_array().unwrap().iter().find(|l| l["requested"].as_str().unwrap_or("").contains("lays")).unwrap().clone();
    if lays_line["product_id"].is_null() {
        // AI can only use a candidate of *that* line: milk is a candidate of another item.
        let allowed = vec![cheese.clone(), milk.clone()];
        assert!(!e
            .core
            .wa_order_apply_ai(seq, ai_rev, &allowed, &json!({ "items": [{ "text": "lays chese", "product_id": milk }] }), "t:m")
            .unwrap());
        let v = conv(&e, A);
        // AI started on revision N; a person edits (N+1); the AI result arrives.
        let staff_rev = rev(&v);
        e.core
            .wa_order_line(
                &e.owner_token,
                &sid(&v),
                staff_rev,
                lays_line["line_no"].as_i64(),
                Some(cheese.clone()),
                Some(3000),
                false,
                false,
            )
            .unwrap();
        assert!(!e
            .core
            .wa_order_apply_ai(seq, staff_rev, &allowed, &json!({ "items": [{ "text": "lays chese", "product_id": cheese }] }), "t:m")
            .unwrap());
        let v = conv(&e, A);
        assert_eq!(qty_of(&v, "Lay's"), Some(3000), "the person's edit stands");
    }
    // A staff edit made against an older revision is refused too.
    let v = conv(&e, A);
    let err = e.core.wa_order_line(&e.owner_token, &sid(&v), rev(&v) - 1, None, Some(milk), Some(1000), false, false).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
}

#[test]
fn ai_never_changes_a_confirmed_order() {
    let e = setup();
    say(&e, "Z1", A, now(), "1 lays chese pickup");
    let v = conv(&e, A);
    let seq = seq_of(&e, "Z1");
    let cheese = pid(&e, "Lay's Cheese 50 g");
    let unresolved = v["order"]["lines"][0]["product_id"].is_null();
    if unresolved {
        // Staff pick the product, confirm; a late AI result must not touch it.
        let r = rev(&v);
        let line_no = v["order"]["lines"][0]["line_no"].as_i64();
        let v = e.core.wa_order_line(&e.owner_token, &sid(&v), r, line_no, Some(cheese.clone()), None, false, false).unwrap();
        e.core.wa_order_confirm(&e.owner_token, &sid(&v), rev(&v)).unwrap();
        let v = conv(&e, A);
        assert!(!e
            .core
            .wa_order_apply_ai(
                seq,
                rev(&v),
                &[cheese],
                &json!({ "items": [{ "text": "lays chese", "product_id": pid(&e, "Almarai Milk 1 L") }] }),
                "t:m"
            )
            .unwrap());
        assert_eq!(conv(&e, A)["order"]["status"], "confirmed");
    }
}

#[test]
fn stock_is_checked_again_at_confirmation_and_reserved() {
    let e = setup();
    let milk = pid(&e, "Almarai Milk 1 L");
    let set_stock = |q: i64| {
        e.core.db.write(|c| Ok(c.execute("UPDATE stock_levels SET qty_milli=?2 WHERE product_id=?1", rusqlite::params![milk, q])?)).unwrap()
    };
    set_stock(2000);
    say(&e, "K1", A, now(), "2 milk pickup");
    let v = conv(&e, A);
    assert_eq!(qty_of(&v, "Milk"), Some(2000));
    // Another till sells one meanwhile.
    set_stock(1000);
    let err = e.core.wa_order_confirm(&e.owner_token, &sid(&v), rev(&v)).unwrap_err();
    assert_eq!(err.details.as_ref().unwrap()["kind"], "stock_shortage", "{}", err.message);
    assert_eq!(conv(&e, A)["order"]["status"], "draft", "nothing confirmed");
    // Acknowledged: confirmed, only the free unit is held.
    let v = e.core.wa_order_confirm_checked(&e.owner_token, &sid(&v), rev(&v), true, false).unwrap();
    assert_eq!(v["order"]["status"], "confirmed");
    let order = v["order"]["order_id"].as_str().unwrap().to_string();
    let o = e.core.order_get(&e.owner_token, &order).unwrap();
    assert_eq!(o.reservations.len(), 1);
    assert_eq!((o.reservations[0]["qty_milli"].as_i64(), o.reservations[0]["wanted_milli"].as_i64()), (Some(1000), Some(2000)));
    // A second customer's order for the same milk: nothing is free now.
    say(&e, "K2", B, now(), "1 milk pickup");
    let vb = conv(&e, B);
    let err = e.core.wa_order_confirm(&e.owner_token, &sid(&vb), rev(&vb)).unwrap_err();
    assert_eq!(err.details.unwrap()["lines"][0]["free_milli"], 0);
    // Cancelling the first order releases its hold.
    e.core.order_cancel(&e.owner_token, &order, Some("customer called".into())).unwrap();
    let vb = conv(&e, B);
    e.core.wa_order_confirm(&e.owner_token, &sid(&vb), rev(&vb)).unwrap();
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_reservations WHERE status='released'"), 1);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_reservations WHERE status='active'"), 1);
    // Reservations never move stock.
    assert_eq!(count(&e, &format!("SELECT qty_milli FROM stock_levels WHERE product_id='{milk}'")), 1000);
}

#[test]
fn deleted_or_repriced_products_are_caught_before_confirmation() {
    let e = setup();
    say(&e, "D1", A, now(), "1 lays cheese and 1 bread pickup");
    let v = conv(&e, A);
    // The reply with the total goes out, then the price changes.
    e.core.wa_order_send(&e.owner_token, &sid(&v), None).unwrap();
    let bread = pid(&e, "Arabic Bread 6 pcs");
    e.core.product_price_update(&e.owner_token, &bread, 350, None, None).unwrap();
    let v = conv(&e, A);
    let err = e.core.wa_order_confirm(&e.owner_token, &sid(&v), rev(&v)).unwrap_err();
    assert_eq!(err.details.as_ref().unwrap()["kind"], "price_changed_since_quote", "{}", err.message);
    // A product switched off meanwhile: no confirmation until it is replaced.
    e.core.product_set_active(&e.owner_token, &bread, false).unwrap();
    let v = conv(&e, A);
    let err = e.core.wa_order_confirm_checked(&e.owner_token, &sid(&v), rev(&v), false, true).unwrap_err();
    assert!(err.message.contains("stock") || err.message.contains("out of stock"), "{}", err.message);
    assert_eq!(conv(&e, A)["order"]["status"], "draft");
}

fn image(e: &Env, id: &str, chat: &str, ts: i64, bytes: &[u8]) -> i64 {
    e.core.wa_ingest(&[msg(id, chat, ts, "image", Some("paid"))]).unwrap();
    e.core.wa_orders_process(50).unwrap();
    let seq = seq_of(e, id);
    e.core.wa_media_saved(seq, bytes, Some("image/jpeg")).unwrap();
    seq
}

#[test]
fn payment_evidence_is_linked_whenever_it_arrives_and_never_proves_payment() {
    let e = setup();
    let t = now();
    // A screenshot before any item: linked to the draft once it exists.
    image(&e, "P0", A, t, b"\xff\xd8shot-1");
    say(&e, "P1", A, t + 10, "1 lays cheese pickup");
    let v = conv(&e, A);
    assert_eq!(v["payment_evidence"].as_array().unwrap().len(), 1, "{}", v["payment_evidence"]);
    assert_eq!(v["order"]["payment_state"], "screenshot_pending");
    // After confirmation a new screenshot belongs to the confirmed order (no new conversation).
    e.core.wa_order_confirm(&e.owner_token, &sid(&v), rev(&v)).unwrap();
    image(&e, "P2", A, t + 60, b"\xff\xd8shot-2");
    assert_eq!(sessions(&e, A), 1);
    let v = conv(&e, A);
    assert_eq!(v["payment_evidence"].as_array().unwrap().len(), 2);
    // The same image again is flagged; verifying it needs a written reason.
    image(&e, "P3", A, t + 90, b"\xff\xd8shot-2");
    let v = conv(&e, A);
    let ev = v["payment_evidence"].as_array().unwrap().clone();
    assert_eq!(ev.len(), 3);
    let dup = ev[2]["review_id"].as_str().unwrap();
    let err = e.core.wa_order_payment(&e.owner_token, &sid(&v), dup, "verified", None).unwrap_err();
    assert_eq!(err.details.unwrap()["reason"], "duplicate_image");
    // A different amount also needs a reason.
    let first = ev[0]["review_id"].as_str().unwrap().to_string();
    e.core.db.write(|c| Ok(c.execute("UPDATE payment_reviews SET detected_minor=100 WHERE review_id=?1", [&first])?)).unwrap();
    let err = e.core.wa_order_payment(&e.owner_token, &sid(&v), &first, "verified", None).unwrap_err();
    assert_eq!(err.details.unwrap()["reason"], "amount_mismatch");
    let v = e.core.wa_order_payment(&e.owner_token, &sid(&v), &first, "verified", Some("Customer paid the rest in cash".into())).unwrap();
    assert_eq!(v["order"]["payment_state"], "recorded");
    let (by, at): (Option<String>, Option<String>) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT decided_by, decided_at FROM payment_reviews WHERE review_id=?1", [&first], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?)
        })
        .unwrap();
    assert!(by.is_some() && at.is_some(), "who and when are recorded");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM payments"), 0);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM sales"), 0);
}

#[test]
fn takeover_release_and_takeover_again() {
    let e = setup();
    let t = now();
    say(&e, "T1", A, t, "1 lays cheese pickup");
    let v = conv(&e, A);
    e.core.wa_order_flags(&e.owner_token, &sid(&v), Some(true), None, None).unwrap();
    say(&e, "T2", A, t + 10, "add 2 milk");
    let v = conv(&e, A);
    assert_eq!(qty_of(&v, "Milk"), None, "the interpreter does not change a conversation staff took over");
    assert!(v["events"].to_string().contains("message_during_takeover"));
    assert!(v["messages"].to_string().contains("add 2 milk"), "the message stays visible");
    e.core.wa_order_flags(&e.owner_token, &sid(&v), Some(false), None, None).unwrap();
    say(&e, "T3", A, t + 20, "add 1 bread");
    assert_eq!(qty_of(&conv(&e, A), "Bread"), Some(1000));
    let v = conv(&e, A);
    let before = rev(&v);
    e.core.wa_order_flags(&e.owner_token, &sid(&v), Some(true), None, None).unwrap();
    let seq = seq_of(&e, "T3");
    assert!(!e.core.wa_order_apply_ai(seq, before, &[], &json!({ "items": [] }), "t:m").unwrap(), "AI results are dropped during takeover");
}

#[test]
fn drafts_never_move_stock_sell_or_send() {
    let e = setup();
    let stock = count(&e, "SELECT COALESCE(SUM(qty_milli),0) FROM stock_levels");
    let t = now();
    say(&e, "Q1", A, t, "2 coke 1.5 and 1 milk, deliver to block 221 road 12 building 5");
    say(&e, "Q2", A, t + 5, "yes confirm");
    assert_eq!(count(&e, "SELECT COALESCE(SUM(qty_milli),0) FROM stock_levels"), stock);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM sales"), 0);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_outbox"), 0, "nothing is sent without a person");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_reservations"), 0, "no hold before a person confirms");
    assert_eq!(conv(&e, A)["order"]["status"], "draft", "the customer's yes does not confirm the order");
}

#[test]
fn a_hold_that_is_not_sold_in_time_expires_and_frees_the_stock() {
    let e = setup();
    say(&e, "H1", A, now(), "2 milk pickup");
    let v = conv(&e, A);
    e.core.wa_order_confirm(&e.owner_token, &sid(&v), rev(&v)).unwrap();
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_reservations WHERE status='active'"), 1);
    assert_eq!(e.core.orders_expire_reservations().unwrap(), 0, "not due yet");
    e.core.db.write(|c| Ok(c.execute("UPDATE stock_reservations SET expires_at='2000-01-01T00:00:00.000Z'", [])?)).unwrap();
    assert_eq!(e.core.orders_expire_reservations().unwrap(), 1);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_reservations WHERE status='expired'"), 1);
    assert_eq!(conv(&e, A)["order"]["status"], "confirmed", "the order itself stays for a person to decide");
}
