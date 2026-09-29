//! WhatsApp AI orders through the real runtime with the fake WhatsApp adapter:
//! the adapter delivers customer messages, the orders worker reads them into a
//! draft on its own task, staff reply through the normal outbox (nothing is
//! sent automatically), the customer's "yes" never confirms anything, and a
//! person's confirmation creates a confirmed digital order without a sale,
//! a payment or a stock movement. Receipts still go out as before.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::messaging::Inbound;
use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::whatsapp::FakeAdapter;
use amwapos_hub::Runtime;
use serde_json::{json, Value};

const CHAT: &str = "97333112233@s.whatsapp.net";

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Value {
    match rt.dispatch(cmd, token.map(|t| t.to_string()), args).await {
        Ok(v) => v,
        Err(e) => panic!("{cmd} failed: {} ({:?})", e.message, e.code),
    }
}

fn count(core: &AppCore, sql: &str) -> i64 {
    core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn msg(id: &str, text: &str) -> Inbound {
    Inbound {
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
    }
}

/// Wait (up to 20 s) until `f` returns Some.
async fn until<T>(what: &str, mut f: impl AsyncFnMut() -> Option<T>) -> T {
    for _ in 0..400 {
        if let Some(v) = f().await {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn whatsapp_chat_becomes_a_reviewed_order_and_nothing_happens_by_itself() {
    let dir = tempfile::tempdir().unwrap();
    let core = Arc::new(AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
    let rt = Runtime::with_bind(core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(500));
    let fake = FakeAdapter::new();
    rt.set_whatsapp_adapter(Arc::new(fake.clone()));
    call(
        &rt,
        "setup.initialize",
        None,
        json!({ "business_name": "Test Mart", "branch_name": "Main", "vat_rate_bp": 1000, "owner_name": "Owner",
                "owner_pin": "4826", "device_name": "Till", "device_code": "T01" }),
    )
    .await;
    let owner = call(&rt, "auth.users", None, json!({})).await[0]["user_id"].as_str().unwrap().to_string();
    let t = call(&rt, "auth.login", None, json!({ "user_id": owner, "pin": "4826" })).await["token"].as_str().unwrap().to_string();
    let t = t.as_str();

    let tax = call(&rt, "tax.list", Some(t), json!({})).await[0]["tax_rule_id"].as_str().unwrap().to_string();
    for (name, bc, price) in [
        ("Coca-Cola Original 1.5 L", "5449000054227", 600),
        ("Coca-Cola Original 330 ml", "5449000000996", 150),
        ("Lay's Cheese 50 g", "6281036000011", 250),
    ] {
        call(
            &rt,
            "products.create",
            Some(t),
            json!({ "name": name, "tax_rule_id": tax, "unit": "pcs", "price_minor": price, "cost_minor": price / 2,
                    "barcodes": [bc], "opening_stock_milli": 50_000 }),
        )
        .await;
    }
    let mut d = call(&rt, "settings.get", Some(t), json!({ "key": "delivery" })).await;
    d["zones"] = json!([{ "name": "Zone A", "blocks": [{ "from": 200, "to": 260 }], "areas": [], "fee_minor": 500, "free_over_minor": null, "active": true }]);
    call(&rt, "settings.save", Some(t), json!({ "key": "delivery", "value": d })).await;
    call(
        &rt,
        "settings.save",
        Some(t),
        json!({ "key": "features", "value": { "whatsapp.enabled": true, "orders.digital": true, "orders.whatsapp_ai": true } }),
    )
    .await;
    rt.ensure_services();
    until("whatsapp stopped, ready to link", async || (rt.whatsapp.status().process == "stopped").then_some(())).await;
    call(&rt, "whatsapp.start", Some(t), json!({})).await;
    fake.scan();
    let stock_before = count(&core, "SELECT COALESCE(SUM(qty_milli),0) FROM stock_levels");
    let sales_before = count(&core, "SELECT COUNT(*) FROM sales");

    // The customer writes; the adapter delivers; the worker reads it off the receive loop.
    assert!(fake.deliver(vec![msg("w1", "Hi, 2 coke 1.5 and 1 lays cheese please. block 230 road 12 building 5 flat 3")]).await);
    let row = until("draft order", async || {
        let l = rt.dispatch("waorders.list", Some(t.into()), json!({ "filter": "all" })).await.ok()?;
        l.as_array().and_then(|a| a.first().cloned()).filter(|r| r["order_id"].is_string())
    })
    .await;
    let sid = row["session_id"].as_str().unwrap().to_string();
    let v = call(&rt, "waorders.get", Some(t), json!({ "session_id": sid })).await;
    let lines = v["order"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2, "{v}");
    assert!(lines.iter().all(|l| l["resolution"] == "resolved"), "{v}");
    assert_eq!(v["order"]["subtotal_minor"], 2 * 600 + 250);
    assert_eq!(v["session"]["delivery_fee_minor"], 500, "fee from the configured zone: {v}");
    assert_eq!(v["session"]["state"], "ready");
    // Nothing went out on its own.
    assert!(fake.sent().is_empty(), "{:?}", fake.sent());

    // Staff send the summary (edited) through the normal outbox.
    call(
        &rt,
        "waorders.send",
        Some(t),
        json!({ "session_id": sid, "text": "Your order: 2 Coca-Cola 1.5 L, 1 Lay's Cheese. Total 1.950 BHD. Reply yes to confirm." }),
    )
    .await;
    let sent = until("reply sent", async || {
        let s = fake.sent();
        (!s.is_empty()).then_some(s)
    })
    .await;
    assert_eq!(sent[0].to, CHAT);
    assert!(sent[0].text.contains("1.950"));

    // The customer says yes: recorded, but only a person confirms.
    assert!(fake.deliver(vec![msg("w2", "yes confirm")]).await);
    until("customer confirmation recorded", async || {
        let v = rt.dispatch("waorders.get", Some(t.into()), json!({ "session_id": sid })).await.ok()?;
        v["events"].as_array()?.iter().any(|e| e["kind"] == "customer_confirmed").then_some(())
    })
    .await;
    let v = call(&rt, "waorders.get", Some(t), json!({ "session_id": sid })).await;
    assert_eq!(v["order"]["status"], "draft");

    // Redelivery of the same message is harmless.
    assert!(fake.deliver(vec![msg("w2", "yes confirm")]).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(count(&core, "SELECT COUNT(*) FROM wa_inbox_processing"), 2);

    // A person confirms: a confirmed digital order; no sale, payment or stock movement.
    let rev = v["session"]["revision"].as_i64().unwrap();
    let v = call(&rt, "waorders.confirm", Some(t), json!({ "session_id": sid, "revision": rev })).await;
    assert_eq!(v["order"]["status"], "confirmed");
    assert_eq!(v["order"]["payment_state"], "unpaid");
    assert_eq!(count(&core, "SELECT COUNT(*) FROM sales"), sales_before);
    assert_eq!(count(&core, "SELECT COALESCE(SUM(qty_milli),0) FROM stock_levels"), stock_before);
    assert_eq!(fake.sent().len(), 1, "confirming sends nothing by itself");

    // Audit trail of the staff actions.
    assert_eq!(count(&core, "SELECT COUNT(*) FROM audit_logs WHERE event_type='order.confirmed'"), 1);
}
