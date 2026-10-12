//! WAHA gateway connection (docs/WHATSAPP_AUTOPILOT.md, "Connection") against
//! a fake WAHA server that answers with the routes, field names and API-key
//! check of WAHA's own source (sessions, auth/qr, chats/overview,
//! chats/{id}/messages, sendText, sendSeen, sendFile).

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amwapos_core::messaging::Inbound;
use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::whatsapp::adapter::{AdapterEvent, AdapterSink, StartOptions, WaContact};
use amwapos_hub::whatsapp::{WahaAdapter, WahaConfig, WhatsAppAdapter};
use amwapos_hub::Runtime;
use async_trait::async_trait;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use serde_json::{json, Value};

const KEY: &str = "k1-secret";
const CUSTOMER: &str = "97333112233@c.us";

#[derive(Default)]
struct Fake {
    created: bool,
    status: String,
    /// chat id → messages (WAHA's WAMessage shape)
    chats: HashMap<String, Vec<Value>>,
    sent: Vec<Value>,
    /// The next sendText reaches WhatsApp but the answer is lost (HTTP 500).
    lose_next_answer: bool,
    seen: usize,
}

type S = Arc<Mutex<Fake>>;

fn authed(h: &HeaderMap) -> bool {
    h.get("x-api-key").and_then(|v| v.to_str().ok()) == Some(KEY)
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "message": "Unauthorized" }))).into_response()
}

async fn session_get(State(s): State<S>, h: HeaderMap, Path(_n): Path<String>) -> Response {
    if !authed(&h) {
        return unauthorized();
    }
    let f = s.lock().unwrap();
    if !f.created {
        return (StatusCode::NOT_FOUND, Json(json!({ "message": "Session not found" }))).into_response();
    }
    Json(json!({ "name": "default", "status": f.status, "me": if f.status == "WORKING" { json!({ "id": "97300000001@c.us", "pushName": "Shop" }) } else { Value::Null } })).into_response()
}

async fn session_create(State(s): State<S>, h: HeaderMap, Json(b): Json<Value>) -> Response {
    if !authed(&h) {
        return unauthorized();
    }
    assert_eq!(b["name"], "default");
    let mut f = s.lock().unwrap();
    f.created = true;
    f.status = "SCAN_QR_CODE".into();
    Json(json!({ "name": "default", "status": "STARTING" })).into_response()
}

async fn qr(State(_s): State<S>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    if !authed(&h) {
        return unauthorized();
    }
    assert_eq!(q.get("format").map(String::as_str), Some("raw"));
    Json(json!({ "value": "2@QRCODE-FROM-WAHA" })).into_response()
}

async fn overview(State(s): State<S>, h: HeaderMap) -> Response {
    if !authed(&h) {
        return unauthorized();
    }
    let f = s.lock().unwrap();
    let mut out = vec![json!({ "id": "120363000000@g.us", "lastMessage": { "timestamp": chrono::Utc::now().timestamp() } })];
    for (id, msgs) in &f.chats {
        out.push(json!({ "id": id, "name": null, "lastMessage": msgs.last().cloned().unwrap_or(Value::Null) }));
    }
    Json(json!(out)).into_response()
}

async fn messages(
    State(s): State<S>,
    h: HeaderMap,
    Path((_n, chat)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !authed(&h) {
        return unauthorized();
    }
    assert!(!chat.ends_with("@g.us"), "group chats are never read");
    let f = s.lock().unwrap();
    let gte: i64 = q.get("filter.timestamp.gte").and_then(|v| v.parse().ok()).unwrap_or(0);
    let from_me: Option<bool> = q.get("filter.fromMe").and_then(|v| v.parse().ok());
    let list: Vec<Value> = f
        .chats
        .get(&chat)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m["timestamp"].as_i64().unwrap_or(0) >= gte && from_me.is_none_or(|fm| m["fromMe"] == fm))
        .collect();
    Json(json!(list)).into_response()
}

async fn send_text(State(s): State<S>, h: HeaderMap, Json(b): Json<Value>) -> Response {
    if !authed(&h) {
        return unauthorized();
    }
    let mut f = s.lock().unwrap();
    let id = format!("true_{}_{}", b["chatId"].as_str().unwrap(), f.sent.len() + 1);
    f.sent.push(b.clone());
    let chat = b["chatId"].as_str().unwrap().to_string();
    let msg = json!({ "id": id, "timestamp": chrono::Utc::now().timestamp(), "from": "97300000001@c.us", "fromMe": true, "body": b["text"], "hasMedia": false });
    f.chats.entry(chat).or_default().push(msg.clone());
    if f.lose_next_answer {
        f.lose_next_answer = false;
        return (StatusCode::INTERNAL_SERVER_ERROR, "upstream timeout").into_response();
    }
    Json(msg).into_response()
}

async fn send_file(h: HeaderMap) -> Response {
    if !authed(&h) {
        return unauthorized();
    }
    (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "message": "Sending files is available only in WAHA Plus" }))).into_response()
}

async fn send_seen(State(s): State<S>, h: HeaderMap) -> Response {
    if !authed(&h) {
        return unauthorized();
    }
    s.lock().unwrap().seen += 1;
    Json(json!({})).into_response()
}

async fn fake_waha() -> (String, S) {
    let s: S = Arc::new(Mutex::new(Fake::default()));
    let app = axum::Router::new()
        .route("/api/sessions", post(session_create))
        .route("/api/sessions/{n}", get(session_get))
        .route("/api/{n}/auth/qr", get(qr))
        .route("/api/{n}/chats/overview", get(overview))
        .route("/api/{n}/chats/{chat}/messages", get(messages))
        .route("/api/sendText", post(send_text))
        .route("/api/sendFile", post(send_file))
        .route("/api/sendSeen", post(send_seen))
        .with_state(s.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}"), s)
}

fn customer_says(s: &S, id: &str, text: &str) {
    let msg = json!({ "id": id, "timestamp": chrono::Utc::now().timestamp(), "from": CUSTOMER, "fromMe": false, "body": text, "hasMedia": false,
                      "_data": { "notifyName": "Ali" } });
    s.lock().unwrap().chats.entry(CUSTOMER.into()).or_default().push(msg);
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

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Value {
    match rt.dispatch(cmd, token.map(|t| t.to_string()), args).await {
        Ok(v) => v,
        Err(e) => panic!("{cmd} failed: {} ({:?})", e.message, e.code),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_shop_links_through_waha_and_customers_get_automatic_answers() {
    let (url, waha) = fake_waha().await;
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
    let tax = call(&rt, "tax.list", Some(t), json!({})).await[0]["tax_rule_id"].as_str().unwrap().to_string();
    call(
        &rt,
        "products.create",
        Some(t),
        json!({ "name": "Almarai Milk 1 L", "tax_rule_id": tax, "unit": "pcs", "price_minor": 900, "cost_minor": 450,
                "barcodes": ["6281007000013"], "opening_stock_milli": 50_000 }),
    )
    .await;
    // Settings → WhatsApp: connect through WAHA; the key goes to the credential store.
    let mut w = call(&rt, "settings.get", Some(t), json!({ "key": "whatsapp" })).await;
    w["connection"] = json!("waha");
    w["waha_url"] = json!(url);
    w["waha_session"] = json!("default");
    call(&rt, "settings.save", Some(t), json!({ "key": "whatsapp", "value": w })).await;
    call(&rt, "whatsapp.waha_key_set", Some(t), json!({ "api_key": KEY })).await;
    assert_eq!(call(&rt, "whatsapp.waha_key_status", Some(t), json!({})).await["configured"], true);
    let saved = call(&rt, "settings.get", Some(t), json!({ "key": "whatsapp" })).await;
    assert!(!saved.to_string().contains(KEY), "the key is not in the settings");
    call(
        &rt,
        "settings.save",
        Some(t),
        json!({ "key": "features", "value": { "whatsapp.enabled": true, "orders.digital": true, "orders.whatsapp_ai": true,
                                               "orders.whatsapp_autopilot": true } }),
    )
    .await;
    rt.ensure_services();
    until("whatsapp ready to link", async || (rt.whatsapp.status().process == "stopped").then_some(())).await;
    call(&rt, "whatsapp.start", Some(t), json!({})).await;
    // The session is created on WAHA and its QR code shown in AMWAPOS.
    let st = until("QR from WAHA", async || {
        let s = rt.whatsapp.status();
        s.qr.is_some().then_some(s)
    })
    .await;
    assert_eq!(st.adapter, "waha");
    assert!(st.qr.unwrap()["svg"].as_str().unwrap().contains("<svg"), "WAHA's QR value drawn as the usual QR card");
    assert_eq!(st.session, "pairing");
    // The phone scans: WAHA reports WORKING.
    waha.lock().unwrap().status = "WORKING".into();
    until("connected", async || rt.whatsapp.status().connected.then_some(())).await;
    // A customer writes; AMWAPOS pulls it, answers it, and sends through WAHA.
    customer_says(&waha, "false_97333112233@c.us_A1", "2 almarai milk 1 l, pickup");
    let sent = until("automatic answer through WAHA", async || {
        let f = waha.lock().unwrap();
        (!f.sent.is_empty()).then(|| f.sent.clone())
    })
    .await;
    assert_eq!(sent[0]["chatId"], CUSTOMER);
    assert_eq!(sent[0]["session"], "default");
    assert!(sent[0]["text"].as_str().unwrap().contains("Total: 1.800 BHD"), "{}", sent[0]);
    // Polling again and again never stores the message twice; the shop's own reply is not an inbound message.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let n: i64 = core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM wa_inbox", [], |r| r.get(0))?)).unwrap();
    assert_eq!(n, 1);
    let chat: String = core.db.read(|c| Ok(c.query_row("SELECT chat FROM wa_inbox", [], |r| r.get(0))?)).unwrap();
    assert_eq!(chat, "97333112233@s.whatsapp.net", "the same chat id as the built-in link");
    customer_says(&waha, "false_97333112233@c.us_A2", "yes");
    until("confirmation through WAHA", async || {
        let f = waha.lock().unwrap();
        f.sent.iter().any(|m| m["text"].as_str().unwrap_or("").contains("is confirmed")).then_some(())
    })
    .await;
    let status: String = core.db.read(|c| Ok(c.query_row("SELECT status FROM digital_orders", [], |r| r.get(0))?)).unwrap();
    assert_eq!(status, "confirmed");
}

#[derive(Default)]
struct Sink {
    events: Mutex<Vec<AdapterEvent>>,
}

#[async_trait]
impl AdapterSink for Sink {
    fn event(&self, e: AdapterEvent) {
        self.events.lock().unwrap().push(e);
    }
    async fn inbound(&self, _batch: Vec<Inbound>) -> Result<(), String> {
        Ok(())
    }
    async fn contacts(&self, _batch: Vec<WaContact>) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_whose_answer_was_lost_is_not_sent_twice_and_files_fall_back_to_text() {
    let (url, waha) = fake_waha().await;
    {
        let mut f = waha.lock().unwrap();
        f.created = true;
        f.status = "WORKING".into();
    }
    let dir = tempfile::tempdir().unwrap();
    let opts = || StartOptions { session_db: dir.path().join("session.db"), pair_phone: None };
    // A wrong key is refused with a message that says what to fix.
    let bad = WahaAdapter::new(WahaConfig { base_url: url.clone(), session: "default".into(), api_key: Some("nope".into()) });
    let err = bad.start(opts(), Arc::new(Sink::default())).await.err().unwrap();
    assert!(err.message.contains("API key"), "{}", err.message);

    let a = WahaAdapter::new(WahaConfig { base_url: url.clone(), session: "default".into(), api_key: Some(KEY.into()) });
    let (session, _exit) = a.start(opts(), Arc::new(Sink::default())).await.unwrap();
    waha.lock().unwrap().lose_next_answer = true;
    let first = session.send_text("MSG-1", "+97333112233", "Your order is confirmed.").await;
    assert!(first.is_err() && !first.as_ref().unwrap_err().permanent, "a lost answer is a temporary failure");
    let again = session.send_text("MSG-1", "+97333112233", "Your order is confirmed.").await.unwrap();
    assert!(again.starts_with("true_97333112233@c.us_"), "{again}");
    assert_eq!(waha.lock().unwrap().sent.len(), 1, "the retry found the message already sent");
    // WAHA Core cannot send files: the text still reaches the customer.
    session
        .send_document("MSG-2", "+97333112233", b"%PDF-1.4".to_vec(), "receipt.pdf", "application/pdf", Some("Receipt R-1, total 1.800"))
        .await
        .unwrap();
    let sent = waha.lock().unwrap().sent.clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["text"], "Receipt R-1, total 1.800");
    // The API key is never sent to another host.
    let e = session.download("http://example.com/api/files/x.jpeg").await.unwrap_err();
    assert!(e.permanent);
    session.mark_read("97333112233@s.whatsapp.net", &[]).await.unwrap();
    assert_eq!(waha.lock().unwrap().seen, 1);
    session.stop().await;
}
