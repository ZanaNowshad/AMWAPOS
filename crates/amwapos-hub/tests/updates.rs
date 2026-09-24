//! Signed updates: a valid signature and checksum are accepted; a tampered
//! manifest, a wrong key, a wrong checksum or a build without a key are refused.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::Runtime;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Value {
    match rt.dispatch(cmd, token.map(|t| t.to_string()), args).await {
        Ok(v) => v,
        Err(e) => panic!("{cmd} failed: {} ({:?})", e.message, e.code),
    }
}

async fn kind(rt: &Arc<Runtime>, cmd: &str, t: &str) -> String {
    let e = rt.dispatch(cmd, Some(t.to_string()), json!({})).await.unwrap_err();
    e.details.and_then(|d| d["kind"].as_str().map(|s| s.to_string())).unwrap_or_else(|| format!("{:?}", e.code))
}

#[derive(Clone)]
struct Feed {
    doc: Arc<std::sync::Mutex<Value>>,
    bin: Arc<Vec<u8>>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_protocol_refuses_anything_unverified() {
    let b64 = base64::engine::general_purpose::STANDARD;
    let key = SigningKey::generate(&mut rand::rngs::OsRng);
    let other = SigningKey::generate(&mut rand::rngs::OsRng);
    let installer = b"MZ fake installer bytes".repeat(100);
    let sha = hex::encode(Sha256::digest(&installer));

    let feed = Feed { doc: Arc::new(std::sync::Mutex::new(Value::Null)), bin: Arc::new(installer.clone()) };
    let app = Router::new()
        .route("/latest.json", get(|State(f): State<Feed>| async move { Json(f.doc.lock().unwrap().clone()) }))
        .route("/AMWAPOS_9.9.9_x64-setup.exe", get(|State(f): State<Feed>| async move { (*f.bin).clone() }))
        .with_state(feed.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let sign =
        |k: &SigningKey, payload: &str| json!({ "payload": payload, "signature": b64.encode(k.sign(payload.as_bytes()).to_bytes()) });
    let payload = |sha: &str| {
        json!({ "version": "9.9.9", "notes": "Test", "published_at": "2026-09-24",
                "installer": { "url": format!("http://127.0.0.1:{port}/AMWAPOS_9.9.9_x64-setup.exe"), "sha256": sha, "size": installer.len(), "file_name": "AMWAPOS_9.9.9_x64-setup.exe" } })
        .to_string()
    };

    let dir = tempfile::tempdir().unwrap();
    let core = Arc::new(AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
    let rt = Runtime::with_bind(core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(500));
    call(&rt, "setup.initialize", None, json!({ "business_name": "T", "branch_name": "M", "vat_rate_bp": 1000, "owner_name": "O", "owner_pin": "4826", "device_name": "D", "device_code": "T01" })).await;
    let owner = call(&rt, "auth.users", None, json!({})).await[0]["user_id"].as_str().unwrap().to_string();
    let t = call(&rt, "auth.login", None, json!({ "user_id": owner, "pin": "4826" })).await["token"].as_str().unwrap().to_string();

    // Module off.
    assert_eq!(kind(&rt, "updates.check", &t).await, "feature_disabled");
    call(&rt, "settings.save", Some(&t), json!({ "key": "features", "value": { "updates": true } })).await;
    call(
        &rt,
        "settings.save",
        Some(&t),
        json!({ "key": "updates", "value": { "feed_url": format!("http://127.0.0.1:{port}/latest.json") } }),
    )
    .await;

    // A build without a key never checks or installs.
    *rt.updater.pubkey.lock().unwrap() = None;
    *feed.doc.lock().unwrap() = sign(&key, &payload(&sha));
    assert_eq!(kind(&rt, "updates.check", &t).await, "update_unsigned_build");

    *rt.updater.pubkey.lock().unwrap() = Some(b64.encode(key.verifying_key().to_bytes()));
    // Signed by another key.
    *feed.doc.lock().unwrap() = sign(&other, &payload(&sha));
    assert_eq!(kind(&rt, "updates.check", &t).await, "update_bad_signature");
    // Payload changed after signing.
    let mut doc = sign(&key, &payload(&sha));
    doc["payload"] = json!(payload(&sha).replace("9.9.9\"", "9.9.8\""));
    *feed.doc.lock().unwrap() = doc;
    assert_eq!(kind(&rt, "updates.check", &t).await, "update_bad_signature");
    // Unsigned manifest.
    *feed.doc.lock().unwrap() = json!({ "version": "9.9.9" });
    assert_eq!(kind(&rt, "updates.check", &t).await, "update_unsigned");
    // Correctly signed but the checksum does not match the file.
    *feed.doc.lock().unwrap() = sign(&key, &payload(&"0".repeat(64)));
    let c = call(&rt, "updates.check", Some(&t), json!({})).await;
    assert_eq!(c["newer"], true);
    assert_eq!(kind(&rt, "updates.download", &t).await, "update_hash_mismatch");
    assert!(!dir.path().join("updates/AMWAPOS_9.9.9_x64-setup.exe").exists());
    assert_eq!(kind(&rt, "updates.install", &t).await, "update_none");

    // Everything correct: verified download is ready.
    *feed.doc.lock().unwrap() = sign(&key, &payload(&sha));
    call(&rt, "updates.check", Some(&t), json!({})).await;
    let st = call(&rt, "updates.download", Some(&t), json!({})).await;
    assert_eq!(st["downloaded"]["version"], "9.9.9");
    // Tampering with the file on disk is caught at install time.
    std::fs::write(dir.path().join("updates/AMWAPOS_9.9.9_x64-setup.exe"), b"evil").unwrap();
    assert_eq!(kind(&rt, "updates.install", &t).await, "update_hash_mismatch");
}
