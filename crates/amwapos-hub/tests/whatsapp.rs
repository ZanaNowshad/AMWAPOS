//! WhatsApp service with the fake adapter: separate status flags, pairing,
//! reconnect after restart, sends (idempotent, post-commit, retried),
//! inbound committed before acknowledgement, media → payment review,
//! supervisor restart after a panic while the tills keep selling, logout,
//! the session in its own file, and the owner-only session backup.

use std::net::Ipv4Addr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::messaging::Inbound;
use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::whatsapp::FakeAdapter;
use amwapos_hub::Runtime;
use serde_json::{json, Value};

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Value {
    match rt.dispatch(cmd, token.map(|t| t.to_string()), args).await {
        Ok(v) => v,
        Err(e) => panic!("{cmd} failed: {} ({:?})", e.message, e.code),
    }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn count(core: &AppCore, sql: &str) -> i64 {
    core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

struct Env {
    _dir: tempfile::TempDir,
    data: std::path::PathBuf,
    core: Arc<AppCore>,
    rt: Arc<Runtime>,
    fake: FakeAdapter,
    t: String,
}

async fn env_at(dir: tempfile::TempDir, fresh: bool) -> Env {
    let data = dir.path().to_path_buf();
    let core = Arc::new(AppCore::open(&data, Arc::new(MemorySecretStore::default())).unwrap());
    let rt = Runtime::with_bind(core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(500));
    let fake = FakeAdapter::new();
    rt.set_whatsapp_adapter(Arc::new(fake.clone()));
    if fresh {
        call(
            &rt,
            "setup.initialize",
            None,
            json!({ "business_name": "Test Mart", "branch_name": "Main", "vat_rate_bp": 1000, "owner_name": "Owner",
                    "owner_pin": "4826", "device_name": "Till", "device_code": "T01" }),
        )
        .await;
    }
    let owner = call(&rt, "auth.users", None, json!({})).await[0]["user_id"].as_str().unwrap().to_string();
    let t = call(&rt, "auth.login", None, json!({ "user_id": owner, "pin": "4826" })).await["token"].as_str().unwrap().to_string();
    Env { _dir: dir, data, core, rt, fake, t }
}

fn wa(rt: &Runtime) -> amwapos_hub::whatsapp::WaStatus {
    rt.whatsapp.status()
}

/// Sell one item for cash through the normal command.
async fn sell(e: &Env) -> Value {
    let cart = call(&e.rt, "pos.scan", Some(&e.t), json!({ "barcode": "7001" })).await;
    let cart = &cart["cart"];
    let total = cart["totals"]["total_minor"].as_i64().unwrap();
    call(
        &e.rt,
        "pos.finalize",
        Some(&e.t),
        json!({ "cart_id": cart["cart_id"], "operation_id": ulid(), "expected_total_minor": total,
                "tenders": [{ "method": "cash", "amount_minor": total }] }),
    )
    .await
}

fn ulid() -> String {
    format!("op-{}", amwapos_core::ids::new_id())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn whatsapp_service_end_to_end_with_fake_adapter() {
    let e = env_at(tempfile::tempdir().unwrap(), true).await;
    let (rt, t) = (&e.rt, e.t.as_str());

    // Off by default: nothing runs, commands are refused, the screen still has a status.
    let st = call(rt, "whatsapp.status", Some(t), json!({})).await;
    assert_eq!(st["features"]["whatsapp.enabled"], false);
    assert_eq!(st["whatsapp"]["process"], "disabled");
    assert_eq!(e.fake.state.starts.load(Ordering::SeqCst), 0);
    assert!(rt.dispatch("whatsapp.start", Some(t.to_string()), json!({})).await.is_err());

    // The session lives in its own file under the data folder, never the ledger.
    let session_file = e.data.join("whatsapp").join("session.db");
    assert_eq!(st["whatsapp"]["session_file"], session_file.display().to_string());

    call(rt, "settings.save", Some(t), json!({ "key": "features", "value": { "whatsapp.enabled": true, "whatsapp.send_receipts": true } }))
        .await;
    // Enabled but never linked: stopped until someone presses Link.
    let st = call(rt, "whatsapp.status", Some(t), json!({})).await;
    assert_eq!((st["whatsapp"]["process"].as_str(), st["whatsapp"]["session"].as_str()), (Some("stopped"), Some("none")));

    // Link: QR shown; separate flags say running + pairing, not connected.
    call(rt, "whatsapp.start", Some(t), json!({})).await;
    let s = wa(rt);
    assert_eq!((s.process.as_str(), s.session.as_str(), s.connected, s.ready), ("running", "pairing", false, false));
    assert!(s.qr.as_ref().unwrap()["svg"].as_str().unwrap().starts_with("<?xml"));
    assert!(session_file.exists());
    assert_ne!(session_file, e.data.join(amwapos_core::service::DB_FILE));
    e.fake.scan();
    let s = wa(rt);
    assert_eq!((s.session.as_str(), s.connected, s.ready), ("paired", true, true));
    assert!(s.qr.is_none());

    // A sale with a customer who has WhatsApp: the receipt is queued after the
    // commit and sent by the worker; the sale itself never waited.
    call(rt, "shift.open", Some(t), json!({ "opening_float_minor": 0, "operation_id": ulid() })).await;
    let tax = call(rt, "tax.list", Some(t), json!({})).await[0]["tax_rule_id"].as_str().unwrap().to_string();
    call(
        rt,
        "products.create",
        Some(t),
        json!({ "name": "Laban", "tax_rule_id": tax, "unit": "pcs", "price_minor": 450, "cost_minor": 300,
                "barcodes": ["7001"], "opening_stock_milli": 100_000 }),
    )
    .await;
    let cust =
        call(rt, "customers.save", Some(t), json!({ "customer": { "name": "Ali", "phone": "33337777", "whatsapp": "33337777" } })).await;
    call(rt, "pos.scan", Some(t), json!({ "barcode": "7001" })).await;
    call(rt, "pos.set_customer", Some(t), json!({ "customer_id": cust["customer_id"] })).await;
    let cart = call(rt, "pos.cart", Some(t), json!({})).await;
    let total = cart["totals"]["total_minor"].as_i64().unwrap();
    let sale = call(
        rt,
        "pos.finalize",
        Some(t),
        json!({ "cart_id": cart["cart_id"], "operation_id": ulid(), "expected_total_minor": total, "tenders": [{ "method": "cash", "amount_minor": total }] }),
    )
    .await;
    until("receipt sent", || e.fake.sent().iter().any(|m| m.document.is_some())).await;
    let sent = e.fake.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "97333337777@s.whatsapp.net");
    assert_eq!(count(&e.core, "SELECT COUNT(*) FROM wa_outbox WHERE status='sent' AND wa_message_id IS NOT NULL"), 1);
    assert_eq!(count(&e.core, "SELECT COUNT(*) FROM audit_logs WHERE event_type='whatsapp.sent'"), 1);
    assert!(sale["sale_id"].is_string());

    // Idempotent send: same key + same payload → same row, sent once; different payload → refused.
    let op = ulid();
    let text = json!({ "operation_id": op, "kind": "text", "to_phone": "33334444", "text": "Your order is ready" });
    let a = call(rt, "whatsapp.queue", Some(t), text.clone()).await;
    let b = call(rt, "whatsapp.queue", Some(t), text.clone()).await;
    assert_eq!(a["message_id"], b["message_id"]);
    let mut changed = text.clone();
    changed["text"] = json!("Something else");
    let err = rt.dispatch("whatsapp.queue", Some(t.to_string()), changed).await.unwrap_err();
    assert_eq!(err.code, amwapos_core::ErrorCode::IdempotencyMismatch);
    until("text sent", || e.fake.sent().len() == 2).await;

    // A failed send is queued for retry (not lost, not duplicated).
    e.fake.state.fail_sends.store(1, Ordering::SeqCst);
    call(rt, "whatsapp.queue", Some(t), json!({ "operation_id": ulid(), "kind": "text", "to_phone": "33334444", "text": "retry me" }))
        .await;
    until("failure recorded", || count(&e.core, "SELECT COUNT(*) FROM wa_outbox WHERE status='queued' AND last_error IS NOT NULL") == 1)
        .await;
    e.core.db.write(|tx| Ok(tx.execute("UPDATE wa_outbox SET next_attempt_at=created_at WHERE status='queued'", [])?)).unwrap();
    rt.whatsapp.poke.notify_one();
    until("retried", || e.fake.sent().iter().filter(|m| m.text == "retry me").count() == 1).await;

    // Inbound: committed to AMWAPOS tables before WhatsApp is acknowledged.
    let rev = wa(rt).inbox_rev;
    let img = Inbound {
        wa_id: "IN1".into(),
        chat: "97333337777@s.whatsapp.net".into(),
        sender_pn: Some("97333337777@s.whatsapp.net".into()),
        ts: 1_790_000_000,
        kind: "image".into(),
        caption: Some("paid".into()),
        media_mime: Some("image/png".into()),
        media_ref: Some("media-1".into()),
        ..Default::default()
    };
    e.fake.state.media.lock().unwrap().insert("media-1".into(), b"png bytes".to_vec());
    assert!(e.fake.deliver(vec![img.clone()]).await);
    assert_eq!(count(&e.core, "SELECT COUNT(*) FROM wa_inbox WHERE wa_id='IN1'"), 1, "stored before deliver() returned");
    assert!(wa(rt).inbox_rev > rev);
    // Redelivery is harmless.
    assert!(e.fake.deliver(vec![img]).await);
    assert_eq!(count(&e.core, "SELECT COUNT(*) FROM wa_inbox WHERE wa_id='IN1'"), 1);
    // The worker downloads the media afterwards.
    until("media saved", || count(&e.core, "SELECT COUNT(*) FROM wa_inbox WHERE wa_id='IN1' AND media_state='saved'") == 1).await;

    // Mark read → read receipt pushed by the worker.
    call(rt, "whatsapp.mark_read", Some(t), json!({ "chat": "97333337777@s.whatsapp.net" })).await;
    until("read receipt", || !e.fake.state.read_marks.lock().unwrap().is_empty()).await;

    // Recent references for diagnostics.
    let recent = call(rt, "whatsapp.recent", Some(t), json!({})).await;
    assert!(recent["sent"].as_array().unwrap().len() >= 3);
    assert_eq!(recent["received"][0]["wa_id"], "IN1");

    // The WhatsApp client panics: the supervisor restarts it, and selling never stops.
    let starts = e.fake.state.starts.load(Ordering::SeqCst);
    e.fake.panic_now();
    until("restarting", || wa(rt).process == "restarting" || e.fake.state.starts.load(Ordering::SeqCst) > starts).await;
    let s = wa(rt);
    assert!(s.restarts >= 1);
    assert!(s.last_error.as_deref().unwrap_or_default().contains("panicked") || s.process == "running", "{s:?}");
    let during = sell(&e).await;
    assert!(during["sale_id"].is_string(), "selling works while WhatsApp restarts");
    until("restarted and reconnected", || e.fake.state.starts.load(Ordering::SeqCst) > starts && wa(rt).ready).await;

    // Session backup: owner only, and only with an explicit acknowledgement.
    let err = rt.dispatch("whatsapp.session_backup", Some(t.to_string()), json!({})).await.unwrap_err();
    assert_eq!(err.code, amwapos_core::ErrorCode::Validation);
    // (The fake session file is not SQLite, so the copy itself is covered in the unit test.)

    // Stop: no reconnect; the tills are unaffected.
    call(rt, "whatsapp.stop", Some(t), json!({})).await;
    let s = wa(rt);
    assert_eq!((s.process.as_str(), s.connected, s.ready), ("stopped", false, false));
    sell(&e).await;
    // While stopped, messages stay queued (never lost).
    call(rt, "whatsapp.queue", Some(t), json!({ "operation_id": ulid(), "kind": "text", "to_phone": "33334444", "text": "while stopped" }))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(count(&e.core, "SELECT COUNT(*) FROM wa_outbox WHERE body='while stopped' AND status='queued'"), 1);
    // Start again: it reconnects with the saved session (no QR) and sends the queue.
    call(rt, "whatsapp.start", Some(t), json!({})).await;
    until("reconnected", || wa(rt).ready).await;
    until("queued message sent", || e.fake.sent().iter().any(|m| m.text == "while stopped")).await;

    // Logout from the phone: session removed, no automatic restart.
    e.fake.remote_logout();
    until("logged out", || wa(rt).session == "logged_out" && wa(rt).process == "stopped").await;
    assert!(!session_file.exists(), "a logged-out session is removed so the next link starts fresh");
    let starts = e.fake.state.starts.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(e.fake.state.starts.load(Ordering::SeqCst), starts);

    // Turning the flag off stops the service.
    call(rt, "whatsapp.start", Some(t), json!({})).await;
    e.fake.scan();
    call(rt, "settings.save", Some(t), json!({ "key": "features", "value": { "whatsapp.enabled": false } })).await;
    until("disabled", || wa(rt).process == "disabled").await;
    assert!(!wa(rt).connected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnects_after_app_restart_without_a_new_qr() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    {
        let e = env_at(dir, true).await;
        call(&e.rt, "settings.save", Some(&e.t), json!({ "key": "features", "value": { "whatsapp.enabled": true } })).await;
        call(&e.rt, "whatsapp.start", Some(&e.t), json!({})).await;
        e.fake.scan();
        assert!(wa(&e.rt).ready);
        // Keep the data folder: leak the TempDir guard into the second run.
        std::mem::forget(e._dir);
    }
    let dir2 = tempfile::TempDir::with_prefix("unused").unwrap();
    let core = Arc::new(AppCore::open(&path, Arc::new(MemorySecretStore::default())).unwrap());
    let rt = Runtime::with_bind(core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(500));
    let fake = FakeAdapter::new();
    rt.set_whatsapp_adapter(Arc::new(fake.clone()));
    rt.ensure_services();
    until("autostart after restart", || rt.whatsapp.status().ready).await;
    assert!(rt.whatsapp.status().qr.is_none(), "the saved session is reused");
    drop(dir2);
    let _ = std::fs::remove_dir_all(&path);
}
