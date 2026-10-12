//! WhatsApp order autopilot (docs/WHATSAPP_AUTOPILOT.md): customers order,
//! confirm, ask for the status and cancel over WhatsApp without a person;
//! every reply comes from POS data; anything unclear goes to a person; the
//! customer hears each status change once, whoever made it.

mod common;

use amwapos_core::messaging::Inbound;
use common::*;
use serde_json::{json, Value};

const CHAT: &str = "97333112233@s.whatsapp.net";

fn setup() -> Env {
    let e = env();
    let t = &e.owner_token;
    e.core
        .settings_save(
            t,
            "features",
            json!({ "whatsapp.enabled": true, "orders.digital": true, "orders.whatsapp_ai": true, "orders.whatsapp_autopilot": true,
                    "whatsapp.delivery_notices": true }),
        )
        .unwrap();
    let mut d = e.core.settings_get(t, "delivery").unwrap();
    d["zones"] = json!([{ "name": "Zone A", "blocks": [{ "from": 200, "to": 260 }], "areas": [], "fee_minor": 500, "free_over_minor": 20000, "active": true }]);
    e.core.settings_save(t, "delivery", d).unwrap();
    // The older automatic delivery notice is on too: it must not send a second copy.
    let mut w = e.core.settings_get(t, "whatsapp").unwrap();
    w["auto_delivery_notice"] = json!(true);
    e.core.settings_save(t, "whatsapp", w).unwrap();
    e.product("Almarai Milk 1 L", "6281007000013", 900, 450, 10_000);
    e.product("Lay's Salt 50 g", "6281036000028", 250, 120, 30_000);
    e.open_shift(t, 0);
    e
}

fn set_flag(e: &Env, name: &str, on: bool) {
    let mut f = e.core.settings_get(&e.owner_token, "features").unwrap();
    f[name] = json!(on);
    e.core.settings_save(&e.owner_token, "features", f).unwrap();
}

fn ingest(e: &Env, chat: &str, id: &str, text: &str, ts: i64) {
    e.core
        .wa_ingest(&[Inbound {
            wa_id: id.into(),
            chat: chat.into(),
            sender_pn: None,
            push_name: Some("Ali".into()),
            ts,
            kind: "text".into(),
            text: Some(text.into()),
            caption: None,
            media_mime: None,
            media_ref: None,
        }])
        .unwrap();
}

fn q<T: rusqlite::types::FromSql>(e: &Env, sql: &str) -> T {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

/// What the worker does for new messages: interpret, then answer each.
fn work(e: &Env) -> Vec<String> {
    e.core.wa_orders_process(50).unwrap();
    e.core.wa_autopilot_pending(50).unwrap()
}

/// A customer message; returns the autopilot's action and its reply text.
fn say_in(e: &Env, chat: &str, text: &str) -> (String, Option<String>) {
    let before: i64 = q(e, "SELECT COUNT(*) FROM wa_outbox");
    ingest(e, chat, &ulid::Ulid::new().to_string(), text, chrono::Utc::now().timestamp());
    let actions = work(e);
    let after: i64 = q(e, "SELECT COUNT(*) FROM wa_outbox");
    let reply = (after > before).then(|| q::<String>(e, "SELECT body FROM wa_outbox ORDER BY created_at DESC, rowid DESC LIMIT 1"));
    (actions.last().cloned().unwrap_or_default(), reply)
}

fn say(e: &Env, text: &str) -> (String, Option<String>) {
    say_in(e, CHAT, text)
}

fn notify(e: &Env) -> Vec<(String, String)> {
    let before: i64 = q(e, "SELECT COUNT(*) FROM wa_outbox");
    e.core.wa_autopilot_notify(100).unwrap();
    e.core
        .db
        .read(|c| {
            let mut st = c.prepare("SELECT kind, COALESCE(body,'') FROM wa_outbox ORDER BY created_at, rowid LIMIT -1 OFFSET ?1")?;
            let r = st.query_map([before], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<(String, String)>, _>>()?;
            Ok(r)
        })
        .unwrap()
}

fn order_of(e: &Env) -> Value {
    let id: String = q(e, "SELECT order_id FROM digital_orders WHERE channel='whatsapp' ORDER BY created_at DESC LIMIT 1");
    serde_json::to_value(e.core.order_get(&e.owner_token, &id).unwrap()).unwrap()
}

fn sell_order(e: &Env, order_id: &str) -> String {
    let t = &e.owner_token;
    let cart = e.core.order_convert(t, order_id, &op()).unwrap();
    let total = cart.totals.total_minor;
    e.core
        .pos_finalize(
            t,
            amwapos_core::sales::FinalizeRequest {
                cart_id: cart.cart_id.clone().unwrap(),
                operation_id: op(),
                tenders: vec![amwapos_core::pricing::TenderInput { method: "cash".into(), amount_minor: total, reference: None }],
                approval_token: None,
                expected_total_minor: Some(total),
                fulfilment: None,
            },
        )
        .unwrap();
    e.core.order_get(t, order_id).unwrap().delivery_id.unwrap_or_default()
}

#[test]
fn a_customer_orders_confirms_and_asks_for_the_status_without_a_person() {
    let e = setup();
    // Start: a greeting gets the help text once.
    let (a, r) = say(&e, "hi");
    assert_eq!(a, "help");
    assert!(r.unwrap().contains("STATUS"));
    // The order: real products, POS prices, pickup.
    let (a, r) = say(&e, "2 almarai milk 1 l and 1 lays salt, pickup");
    assert_eq!(a, "summary", "{r:?}");
    let r = r.unwrap();
    assert!(r.contains("Almarai Milk 1 L") && r.contains("1.800") && r.contains("Total: 2.050 BHD"), "{r}");
    assert!(r.contains("Reply YES") && r.contains("cash on delivery"), "{r}");
    assert_eq!(order_of(&e)["status"], "draft");
    // Yes: confirmed through the normal checks, stock held, by the autopilot.
    let (a, r) = say(&e, "yes");
    assert_eq!(a, "confirmed");
    assert!(r.is_none(), "the confirmation message comes from the order event");
    let o = order_of(&e);
    assert_eq!(o["status"], "confirmed");
    assert_eq!(q::<String>(&e, "SELECT confirmed_by FROM digital_orders WHERE channel='whatsapp'"), "system:autopilot");
    assert_eq!(q::<i64>(&e, "SELECT COUNT(*) FROM stock_reservations WHERE status='active'"), 2);
    assert_eq!(q::<i64>(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type='order.confirmed' AND user_id='system:autopilot'"), 1);
    // The customer is told once.
    let sent = notify(&e);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].1.contains("is confirmed. Total: 2.050 BHD") && sent[0].1.contains("collect it from the shop"), "{sent:?}");
    assert!(notify(&e).is_empty(), "no second copy");
    // Status in plain words.
    let (a, r) = say(&e, "status");
    assert_eq!(a, "status");
    assert!(r.unwrap().contains("confirmed and waiting to be prepared"));
    // Nothing sold and no stock moved by WhatsApp: that happens at the till.
    assert_eq!(q::<i64>(&e, "SELECT COUNT(*) FROM sales"), 0);
    // A message is answered once, even if the worker sees it again.
    let seq: i64 = q(&e, "SELECT MAX(seq) FROM wa_inbox");
    assert_eq!(e.core.wa_autopilot(seq).unwrap(), None);
}

#[test]
fn a_delivery_order_reports_each_status_change_once_whoever_makes_it() {
    let e = setup();
    let (a, r) = say(&e, "1 almarai milk 1 l deliver to flat 3 building 120 road 2201 block 221");
    assert_eq!(a, "summary", "{r:?}");
    let r = r.unwrap();
    assert!(r.contains("Delivery fee: 0.500 BHD") && r.contains("Total: 1.400 BHD"), "{r}");
    assert_eq!(say(&e, "yes").0, "confirmed");
    notify(&e);
    // Staff ring it up at the till: a delivery opens. No extra message for that.
    let order_id = order_of(&e)["order_id"].as_str().unwrap().to_string();
    let delivery = sell_order(&e, &order_id);
    assert!(notify(&e).is_empty(), "being prepared is not announced");
    // The rider takes it: one "on the way" (the shop's own template), not two.
    e.core.delivery_update(&e.owner_token, &delivery, Some("dispatched".into()), None, None, None).unwrap();
    let sent = notify(&e);
    assert_eq!(sent.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(), vec!["dispatch"], "{sent:?}");
    e.core.delivery_update(&e.owner_token, &delivery, Some("delivered".into()), None, None, None).unwrap();
    let sent = notify(&e);
    assert_eq!(sent.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(), vec!["delivered"], "{sent:?}");
    assert_eq!(q::<i64>(&e, "SELECT COUNT(*) FROM wa_outbox WHERE kind='dispatch'"), 1, "the older automatic notice stayed quiet");
    let (_, r) = say(&e, "where is my order?");
    assert!(r.unwrap().contains("was delivered"));
}

#[test]
fn a_yes_confirms_only_the_order_the_customer_was_shown() {
    let e = setup();
    assert_eq!(say(&e, "1 almarai milk 1 l, pickup").0, "summary");
    // The price changes after the summary was sent.
    let milk: String = q(&e, "SELECT product_id FROM products WHERE name='Almarai Milk 1 L'");
    e.core.product_price_update(&e.owner_token, &milk, 950, None, None).unwrap();
    let (a, r) = say(&e, "yes");
    assert_eq!(a, "summary", "a new summary, not a confirmation");
    assert!(r.unwrap().contains("Total: 0.950 BHD"));
    assert_eq!(order_of(&e)["status"], "draft");
    assert_eq!(say(&e, "yes").0, "confirmed");
}

#[test]
fn the_customer_cancels_until_the_order_is_rung_up_then_a_person_takes_over() {
    let e = setup();
    say(&e, "1 almarai milk 1 l, pickup");
    say(&e, "yes");
    notify(&e);
    let (a, _) = say(&e, "cancel my order");
    assert_eq!(a, "cancelled");
    assert_eq!(order_of(&e)["status"], "cancelled");
    assert_eq!(q::<i64>(&e, "SELECT COUNT(*) FROM stock_reservations WHERE status='active'"), 0, "the held stock is free again");
    let sent = notify(&e);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].1.contains("has been cancelled"));
    // A second order, already rung up: cancelling it is a person's job.
    say(&e, "1 lays salt, pickup");
    say(&e, "yes");
    let order_id = order_of(&e)["order_id"].as_str().unwrap().to_string();
    sell_order(&e, &order_id);
    let (a, r) = say(&e, "cancel my order");
    assert_eq!(a, "handoff:already_preparing");
    assert!(r.unwrap().contains("already being prepared"));
    assert_eq!(order_of(&e)["status"], "converted");
}

#[test]
fn a_person_takes_over_on_request_and_the_autopilot_stays_quiet() {
    let e = setup();
    let (a, r) = say(&e, "agent");
    assert_eq!(a, "handoff:customer_asked_for_person");
    assert!(r.unwrap().contains("team will reply"));
    assert_eq!(q::<i64>(&e, "SELECT COUNT(*) FROM wa_order_sessions WHERE staff_takeover=1 AND priority='high'"), 1);
    let (a, r) = say(&e, "2 almarai milk 1 l");
    assert!(a.starts_with("none"), "{a}");
    assert!(r.is_none());
}

#[test]
fn an_unknown_delivery_fee_or_stock_that_ran_out_goes_to_a_person() {
    let e = setup();
    let (a, r) = say(&e, "1 almarai milk 1 l deliver to flat 1 building 10 road 901 block 905");
    assert_eq!(a, "handoff:fee_unknown", "{r:?}");
    assert!(r.unwrap().contains("delivery fee"));
    // Another customer: the stock runs out between the summary and the yes.
    let other = "97339998877@s.whatsapp.net";
    assert_eq!(say_in(&e, other, "2 lays salt, pickup").0, "summary");
    e.core.db.write(|c| Ok(c.execute("UPDATE stock_levels SET qty_milli=1000", [])?)).unwrap();
    let (a, r) = say_in(&e, other, "yes");
    assert_eq!(a, "handoff:stock_changed");
    assert!(r.unwrap().contains("ran out"));
    assert_eq!(q::<i64>(&e, "SELECT COUNT(*) FROM digital_orders WHERE status='confirmed'"), 0);
}

#[test]
fn chats_that_go_back_and_forth_are_handed_over_and_help_is_not_repeated() {
    let e = setup();
    assert_eq!(say(&e, "hello").0, "help");
    assert_eq!(say(&e, "hello again").0, "none:help_sent_recently");
    let other = "97335554433@s.whatsapp.net";
    let mut last = String::new();
    for _ in 0..8 {
        last = say_in(&e, other, "status").0;
        if last.starts_with("handoff") {
            break;
        }
    }
    assert_eq!(last, "handoff:too_many_messages");
    assert!(say_in(&e, other, "status").0.starts_with("none"), "quiet once a person has it");
}

#[test]
fn arabic_customers_get_arabic_and_old_messages_are_left_to_staff() {
    let e = setup();
    let (a, r) = say(&e, "مساعدة");
    assert_eq!(a, "help");
    assert!(r.unwrap().contains("يمكنك الطلب"));
    let (a, r) = say(&e, "وين طلبي");
    assert!(r.as_deref().unwrap_or_default().contains("ليس لديك طلبات"), "{a} {r:?}");
    // AMWAPOS was off for three hours: that message is not answered by itself.
    let other = "97336667788@s.whatsapp.net";
    ingest(&e, other, "OLD1", "status", chrono::Utc::now().timestamp() - 3 * 3600);
    assert_eq!(work(&e), vec!["none:too_old".to_string()]);
}

#[test]
fn nothing_is_sent_while_the_autopilot_is_off_and_turning_it_on_sends_no_old_news() {
    let e = setup();
    set_flag(&e, "orders.whatsapp_autopilot", false);
    let (a, r) = say(&e, "1 almarai milk 1 l, pickup");
    assert_eq!(a, "");
    assert!(r.is_none());
    // Staff confirm it by hand while the autopilot is off.
    let order_id = order_of(&e)["order_id"].as_str().unwrap().to_string();
    e.core.order_confirm(&e.owner_token, &order_id).unwrap();
    assert!(notify(&e).is_empty());
    set_flag(&e, "orders.whatsapp_autopilot", true);
    assert!(notify(&e).is_empty(), "the confirmation made while off is not sent later");
}
