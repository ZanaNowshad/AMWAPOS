//! Owner phone view: off by default, token required, revocable, read-only.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::Runtime;
use serde_json::{json, Value};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Result<Value, amwapos_core::AppError> {
    rt.dispatch(cmd, token.map(|t| t.to_string()), args).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phone_view_needs_the_flag_and_a_live_token() {
    let dir = tempfile::tempdir().unwrap();
    let core = Arc::new(AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
    let rt = Runtime::with_bind(core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(200));
    call(
        &rt,
        "setup.initialize",
        None,
        json!({ "business_name": "Test Mart", "branch_name": "Main", "vat_rate_bp": 1000, "owner_name": "Owner",
                "owner_pin": "4826", "device_name": "Hub PC", "device_code": "H01" }),
    )
    .await
    .unwrap();
    let owner = call(&rt, "auth.users", None, json!({})).await.unwrap()[0]["user_id"].as_str().unwrap().to_string();
    let token =
        call(&rt, "auth.login", None, json!({ "user_id": owner, "pin": "4826" })).await.unwrap()["token"].as_str().unwrap().to_string();
    let port = free_port();
    core.db.write(|tx| amwapos_core::settings::put(tx, amwapos_core::sync::KEY_SYNC, &json!({ "port": port }), None)).unwrap();
    call(&rt, "settings.save", Some(&token), json!({ "key": "features", "value": { "hub": true } })).await.unwrap();
    call(&rt, "sync.enable_hub", Some(&token), json!({})).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::builder().no_proxy().build().unwrap();

    // Flag off: the page does not exist and no token can be issued.
    assert_eq!(http.get(format!("{base}/companion/")).send().await.unwrap().status(), 404);
    assert!(call(&rt, "companion.issue", Some(&token), json!({})).await.is_err());

    call(&rt, "settings.save", Some(&token), json!({ "key": "features", "value": { "hub": true, "pwa.companion": true } })).await.unwrap();
    let page = http.get(format!("{base}/companion/")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.headers()["content-security-policy"].to_str().unwrap().contains("script-src 'self'"));
    assert!(page.text().await.unwrap().contains("/companion/app.js"));
    // No token, or a made-up one: refused.
    let snap = |t: &str| http.get(format!("{base}/companion/api/snapshot")).header("authorization", format!("Bearer {t}")).send();
    assert_eq!(http.get(format!("{base}/companion/api/snapshot")).send().await.unwrap().status(), 401);
    assert_eq!(snap(&"ab".repeat(32)).await.unwrap().status(), 401);
    // A live token reads the snapshot; it carries no write endpoints.
    let issued = call(&rt, "companion.issue", Some(&token), json!({ "label": "Owner phone", "hours": 48 })).await.unwrap();
    let secret = issued["token"].as_str().unwrap().to_string();
    let r = snap(&secret).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    let v: Value = r.json().await.unwrap();
    assert!(v["dashboard"]["kpis"].is_object());
    assert!(v["deliveries"].is_array() && v["low_stock"].is_array());
    assert_eq!(http.post(format!("{base}/companion/api/snapshot")).send().await.unwrap().status(), 405);
    // Capped at 24 hours; revocation takes effect at once.
    let listed = call(&rt, "companion.tokens", Some(&token), json!({})).await.unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    let exp = chrono::DateTime::parse_from_rfc3339(issued["expires_at"].as_str().unwrap()).unwrap();
    assert!(exp.signed_duration_since(chrono::Utc::now()) <= chrono::Duration::hours(24));
    call(&rt, "companion.revoke", Some(&token), json!({ "id": issued["id"] })).await.unwrap();
    assert_eq!(snap(&secret).await.unwrap().status(), 401);
    let audit: i64 = core
        .db
        .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM audit_logs WHERE event_type LIKE 'companion.%'", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(audit, 2);
}
