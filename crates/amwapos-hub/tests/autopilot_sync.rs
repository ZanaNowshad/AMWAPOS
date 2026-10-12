//! WhatsApp autopilot and order sync through the real runtime
//! (docs/WHATSAPP_AUTOPILOT.md): a customer orders and confirms over
//! WhatsApp with nobody touching AMWAPOS, the replies really go out through
//! the WhatsApp adapter, and every order event reaches a signed webhook in
//! order, after the receiver's temporary failure, and a CSV file once.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amwapos_core::messaging::Inbound;
use amwapos_core::order_sync::{hmac_sha256, SyncOutcome};
use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::order_sync::classify_status;
use amwapos_hub::whatsapp::FakeAdapter;
use amwapos_hub::Runtime;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use serde_json::{json, Value};

const CHAT: &str = "97333112233@s.whatsapp.net";

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Value {
    match rt.dispatch(cmd, token.map(|t| t.to_string()), args).await {
        Ok(v) => v,
        Err(e) => panic!("{cmd} failed: {} ({:?})", e.message, e.code),
    }
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

async fn until<T>(what: &str, mut f: impl AsyncFnMut() -> Option<T>) -> T {
    for _ in 0..600 {
        if let Some(v) = f().await {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {what}");
}

#[derive(Clone, Default)]
struct Receiver {
    got: Arc<Mutex<Vec<(HeaderMap, String)>>>,
    fail_first: Arc<Mutex<bool>>,
}

async fn hook(State(r): State<Receiver>, headers: HeaderMap, body: String) -> StatusCode {
    let mut f = r.fail_first.lock().unwrap();
    if *f {
        *f = false;
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    r.got.lock().unwrap().push((headers, body));
    StatusCode::OK
}

async fn receiver() -> (String, Receiver) {
    let r = Receiver { fail_first: Arc::new(Mutex::new(true)), ..Default::default() };
    let app = axum::Router::new().route("/hook", post(hook)).with_state(r.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}/hook"), r)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_customer_orders_over_whatsapp_and_every_event_reaches_the_webhook_in_order() {
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
    call(
        &rt,
        "products.create",
        Some(t),
        json!({ "name": "Almarai Milk 1 L", "tax_rule_id": tax, "unit": "pcs", "price_minor": 900, "cost_minor": 450,
                "barcodes": ["6281007000013"], "opening_stock_milli": 50_000 }),
    )
    .await;
    call(
        &rt,
        "settings.save",
        Some(t),
        json!({ "key": "features", "value": { "whatsapp.enabled": true, "orders.digital": true, "orders.whatsapp_ai": true,
                                               "orders.whatsapp_autopilot": true } }),
    )
    .await;
    // A webhook target that fails once before it accepts.
    let (url, rx) = receiver().await;
    let created = call(&rt, "order_sync.create", Some(t), json!({ "target": { "name": "ERP", "kind": "webhook", "url": url } })).await;
    let secret = created["secret"].as_str().unwrap().to_string();
    let sink = created["target"]["sink_id"].as_str().unwrap().to_string();
    rt.ensure_services();
    until("whatsapp ready", async || (rt.whatsapp.status().process == "stopped").then_some(())).await;
    call(&rt, "whatsapp.start", Some(t), json!({})).await;
    fake.scan();

    // The customer orders: the summary goes out by itself.
    assert!(fake.deliver(vec![msg("w1", "2 almarai milk 1 l, pickup")]).await);
    let sent = until("automatic summary", async || {
        let s = fake.sent();
        (!s.is_empty()).then_some(s)
    })
    .await;
    assert_eq!(sent[0].to, CHAT);
    assert!(sent[0].text.contains("Total: 1.800 BHD") && sent[0].text.contains("Reply YES"), "{}", sent[0].text);
    // "Yes": confirmed, and the confirmation goes out by itself.
    assert!(fake.deliver(vec![msg("w2", "yes")]).await);
    let sent = until("confirmation", async || {
        let s = fake.sent();
        (s.len() >= 2).then_some(s)
    })
    .await;
    assert!(sent[1].text.contains("is confirmed. Total: 1.800 BHD"), "{}", sent[1].text);
    let status: String = core.db.read(|c| Ok(c.query_row("SELECT status FROM digital_orders", [], |r| r.get(0))?)).unwrap();
    assert_eq!(status, "confirmed");

    // The webhook: the first attempt failed; the retry waits, so skip the wait.
    until("first failure recorded", async || {
        core.db
            .read(|c| Ok(c.query_row("SELECT last_error FROM sync_sinks WHERE sink_id=?1", [&sink], |r| r.get::<_, Option<String>>(0))?))
            .unwrap()
    })
    .await;
    core.db.write(|c| Ok(c.execute("UPDATE sync_sinks SET next_at=NULL", [])?)).unwrap();
    rt.order_sync.poke.notify_one();
    let got = until("both events delivered", async || {
        let g = rx.got.lock().unwrap().clone();
        (g.len() >= 2).then_some(g)
    })
    .await;
    let kinds: Vec<String> =
        got.iter().map(|(_, b)| serde_json::from_str::<Value>(b).unwrap()["kind"].as_str().unwrap().to_string()).collect();
    assert_eq!(kinds, vec!["order.draft", "order.confirmed"], "in order, the failed one first");
    for (h, body) in &got {
        let sig = h.get("x-amwapos-signature").unwrap().to_str().unwrap();
        assert_eq!(
            sig,
            format!("sha256={}", hex::encode(hmac_sha256(secret.as_bytes(), body.as_bytes()))),
            "signed with the target's secret"
        );
        let v: Value = serde_json::from_str(body).unwrap();
        assert_eq!(h.get("x-amwapos-event-id").unwrap().to_str().unwrap(), v["event_id"].as_str().unwrap());
        assert_eq!(v["order"]["channel"], "whatsapp");
        assert_eq!(v["order"]["lines"][0]["qty_milli"], 2000);
    }
    let versions: Vec<i64> = got.iter().map(|(_, b)| serde_json::from_str::<Value>(b).unwrap()["version"].as_i64().unwrap()).collect();
    assert_eq!(versions, vec![1, 2]);
    // "Send a test" reaches the same receiver and changes nothing.
    let r = call(&rt, "order_sync.test", Some(t), json!({ "sink_id": sink })).await;
    assert_eq!(r["ok"], true, "{r}");
    let list = call(&rt, "order_sync.list", Some(t), json!({})).await;
    assert_eq!(list["targets"][0]["delivered"], 2, "{list}");
    assert_eq!(list["targets"][0]["parked"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_csv_target_writes_each_event_once() {
    let dir = tempfile::tempdir().unwrap();
    let core = Arc::new(AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
    let rt = Runtime::with_bind(core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(500));
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
    call(&rt, "settings.save", Some(t), json!({ "key": "features", "value": { "orders.digital": true } })).await;
    let path = dir.path().join("shared").join("orders.csv");
    let created =
        call(&rt, "order_sync.create", Some(t), json!({ "target": { "name": "Sheet", "kind": "csv", "path": path.to_string_lossy() } }))
            .await;
    assert!(created["secret"].is_null());
    let tax = call(&rt, "tax.list", Some(t), json!({})).await[0]["tax_rule_id"].as_str().unwrap().to_string();
    let p = call(
        &rt,
        "products.create",
        Some(t),
        json!({ "name": "=SUM(1)", "tax_rule_id": tax, "unit": "pcs", "price_minor": 300, "barcodes": ["9302"], "opening_stock_milli": 10_000 }),
    )
    .await;
    let pid = p["product_id"].as_str().unwrap();
    let o = call(
        &rt,
        "orders.save",
        Some(t),
        json!({ "order": { "channel": "phone", "phone": "33334444", "lines": [{ "product_id": pid, "qty_milli": 1000 }] } }),
    )
    .await;
    call(&rt, "orders.confirm", Some(t), json!({ "order_id": o["order_id"] })).await;
    // The background worker (started by setup) and this pass share the work.
    let caught_up = async || {
        rt.order_sync.tick().await.unwrap();
        let l = call(&rt, "order_sync.list", Some(t), json!({})).await;
        (l["targets"][0]["behind"] == 0).then_some(())
    };
    until("both events written", caught_up).await;
    // The cursor is moved back as after a crash: the file is not written twice.
    core.db.write(|c| Ok(c.execute("UPDATE sync_sinks SET cursor_seq=0", [])?)).unwrap();
    until("caught up again", caught_up).await;
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[0].starts_with("event_id,seq,occurred_at,kind"));
    assert!(lines[1].contains("order.draft") && lines[2].contains("order.confirmed"), "{text}");
    assert!(!text.contains(",=SUM"), "a product name never becomes a spreadsheet formula: {text}");
}

#[test]
fn http_answers_are_classified_for_retry_or_refusal() {
    assert_eq!(classify_status(200, None), SyncOutcome::Delivered);
    assert_eq!(classify_status(204, None), SyncOutcome::Delivered);
    assert_eq!(classify_status(409, None), SyncOutcome::Delivered, "the receiver already has this or a newer version");
    assert!(matches!(classify_status(503, None), SyncOutcome::Retry { .. }));
    assert_eq!(classify_status(429, Some(30)), SyncOutcome::Retry { error: "The receiver answered HTTP 429.".into(), after_s: Some(30) });
    assert!(matches!(classify_status(408, None), SyncOutcome::Retry { .. }));
    assert!(matches!(classify_status(400, None), SyncOutcome::Refused { .. }));
    assert!(matches!(classify_status(401, None), SyncOutcome::Refused { .. }));
    assert!(matches!(classify_status(301, None), SyncOutcome::Refused { .. }), "redirects are not followed");
}
