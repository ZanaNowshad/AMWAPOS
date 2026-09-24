//! Protocol 2: nothing sensitive crosses the LAN in the clear, protocol 1 is
//! refused, and a pairing code cannot be guessed.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::Runtime;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Value {
    match rt.dispatch(cmd, token.map(|t| t.to_string()), args).await {
        Ok(v) => v,
        Err(e) => panic!("{cmd} failed: {} ({:?})", e.message, e.code),
    }
}

struct Hub {
    _dir: tempfile::TempDir,
    core: Arc<AppCore>,
    rt: Arc<Runtime>,
    token: String,
    owner: String,
    port: u16,
}

const PRODUCT: &str = "Secret Saffron 50g";

async fn hub() -> Hub {
    let dir = tempfile::tempdir().unwrap();
    let core = Arc::new(AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
    let rt = Runtime::with_bind(core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(200));
    call(
        &rt,
        "setup.initialize",
        None,
        json!({
            "business_name": "Test Mart", "branch_name": "Main", "vat_rate_bp": 1000, "owner_name": "Owner",
            "owner_pin": "4826", "device_name": "Hub PC", "device_code": "H01"
        }),
    )
    .await;
    let owner = call(&rt, "auth.users", None, json!({})).await[0]["user_id"].as_str().unwrap().to_string();
    let token = call(&rt, "auth.login", None, json!({ "user_id": owner, "pin": "4826" })).await["token"].as_str().unwrap().to_string();
    let port = free_port();
    core.db.write(|tx| amwapos_core::settings::put(tx, amwapos_core::sync::KEY_SYNC, &json!({ "port": port }), None)).unwrap();
    call(&rt, "sync.enable_hub", Some(&token), json!({})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let tax = call(&rt, "tax.list", Some(&token), json!({})).await[0]["tax_rule_id"].as_str().unwrap().to_string();
    call(
        &rt,
        "products.create",
        Some(&token),
        json!({ "name": PRODUCT, "tax_rule_id": tax, "price_minor": 4750, "cost_minor": 3000, "barcodes": ["0099887766554"], "opening_stock_milli": 5000 }),
    )
    .await;
    Hub { _dir: dir, core, rt, token, owner, port }
}

/// Forward TCP traffic to `target`, recording every byte in both directions.
async fn recording_proxy(target: u16) -> (u16, Arc<Mutex<Vec<u8>>>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else { return };
            let Ok(server) = TcpStream::connect((Ipv4Addr::LOCALHOST, target)).await else { continue };
            let (cr, cw) = client.into_split();
            let (sr, sw) = server.into_split();
            for (mut r, mut w) in [
                (
                    Box::new(cr) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                    Box::new(sw) as Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
                ),
                (Box::new(sr), Box::new(cw)),
            ] {
                let log = log2.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 16 * 1024];
                    loop {
                        match r.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                log.lock().unwrap().extend_from_slice(&buf[..n]);
                                if w.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    let _ = w.shutdown().await;
                });
            }
        }
    });
    (port, log)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_sensitive_crosses_the_lan_in_clear() {
    let h = hub().await;
    let (proxy, wire) = recording_proxy(h.port).await;
    let code = call(&h.rt, "sync.pairing_code", Some(&h.token), json!({})).await["code"].as_str().unwrap().to_string();

    let dir = tempfile::tempdir().unwrap();
    let core = Arc::new(AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
    let term = Runtime::with_bind(core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(200));
    let url = format!("127.0.0.1:{proxy}");
    call(&term, "sync.join", None, json!({ "hub_url": url, "code": code, "device_name": "Till 2", "device_code": "T02" })).await;
    let tt = call(&term, "auth.login", None, json!({ "user_id": h.owner, "pin": "4826" })).await["token"].as_str().unwrap().to_string();
    call(&term, "shift.open", Some(&tt), json!({ "opening_float_minor": 0, "operation_id": ulid::Ulid::new().to_string() })).await;
    let scan = call(&term, "pos.scan", Some(&tt), json!({ "barcode": "0099887766554" })).await;
    call(
        &term,
        "pos.finalize",
        Some(&tt),
        json!({ "cart_id": scan["cart"]["cart_id"], "operation_id": ulid::Ulid::new().to_string(), "tenders": [{ "method": "cash", "amount_minor": 4750 }] }),
    )
    .await;
    let r = call(&term, "sync.run_now", Some(&tt), json!({})).await;
    assert!(r["pushed"].as_u64().unwrap() >= 3, "{r}");
    let hub_sales: i64 = h.core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM sales", [], |r| r.get(0))?)).unwrap();
    assert_eq!(hub_sales, 1, "sync worked through the proxy");

    let wire = wire.lock().unwrap().clone();
    assert!(wire.len() > 10_000, "the proxy saw the traffic ({} bytes)", wire.len());
    let device_key = core.secrets.get(amwapos_core::sync::SECRET_DEVICE_KEY).unwrap().unwrap();
    let pin_hash: String = h.core.db.read(|c| Ok(c.query_row("SELECT pin_hash FROM users LIMIT 1", [], |r| r.get(0))?)).unwrap();
    for (what, secret) in [
        ("product name", PRODUCT.as_bytes()),
        ("barcode", b"0099887766554".as_slice()),
        ("pairing code", code.as_bytes()),
        ("device key", device_key.as_bytes()),
        ("PIN hash", pin_hash.as_bytes()),
        ("Argon2 marker", b"$argon2id$".as_slice()),
        ("receipt number", b"T02-0000001".as_slice()),
    ] {
        assert!(!contains(&wire, secret), "{what} was visible on the wire");
    }
    // Sanity: the proxy really captured the HTTP exchange (headers stay readable).
    assert!(contains(&wire, b"/sync/push") && contains(&wire, b"x-amw-protocol"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn protocol_1_and_unsealed_requests_are_refused() {
    let h = hub().await;
    let base = format!("http://127.0.0.1:{}", h.port);
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    // Old plaintext pairing endpoint.
    let r = http.post(format!("{base}/pair")).json(&json!({ "code": "12345678" })).send().await.unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::UPGRADE_REQUIRED);
    // Protocol-2 routes without the protocol header.
    let r = http.post(format!("{base}/pair/start")).json(&json!({})).send().await.unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::UPGRADE_REQUIRED);
    let r = http.post(format!("{base}/sync/pull")).body("{}").send().await.unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::UPGRADE_REQUIRED);
    let info: Value = http.get(format!("{base}/info")).send().await.unwrap().json().await.unwrap();
    assert_eq!(info["protocol"], 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_codes_burn_the_pairing_code() {
    let h = hub().await;
    let code = call(&h.rt, "sync.pairing_code", Some(&h.token), json!({})).await["code"].as_str().unwrap().to_string();
    let wrong = if code == "11111111" { "22222222" } else { "11111111" };
    let url = format!("127.0.0.1:{}", h.port);
    let mut last = String::new();
    for i in 0..5 {
        let dir = tempfile::tempdir().unwrap();
        let core = Arc::new(AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
        let term = Runtime::with_bind(core, Ipv4Addr::LOCALHOST, Duration::from_millis(200));
        let e = term
            .dispatch("sync.join", None, json!({ "hub_url": url, "code": wrong, "device_name": "X", "device_code": format!("T1{i}") }))
            .await
            .unwrap_err();
        last = e.message;
    }
    assert!(last.contains("no longer valid"), "{last}");
    let usable: i64 = h
        .core
        .db
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM pairing_codes WHERE used_at IS NULL AND expires_at > ?1",
                [amwapos_core::time::now_str()],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(usable, 0, "the real code is burned too, so the guesser gained nothing");
    let devices: i64 = h.core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM devices", [], |r| r.get(0))?)).unwrap();
    assert_eq!(devices, 1, "no terminal was registered");
}

/// A hub on another version answers 426; the till must report "version
/// mismatch", not a generic offline state. A dead address reports "unreachable".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn version_mismatch_is_reported_distinctly_from_offline() {
    let h = hub().await;
    let code = call(&h.rt, "sync.pairing_code", Some(&h.token), json!({})).await["code"].as_str().unwrap().to_string();
    let dir = tempfile::tempdir().unwrap();
    let core = Arc::new(AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
    let term = Runtime::with_bind(core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(200));
    let url = format!("127.0.0.1:{}", h.port);
    call(&term, "sync.join", None, json!({ "hub_url": url, "code": code, "device_name": "Till 2", "device_code": "T02" })).await;
    let tt = call(&term, "auth.login", None, json!({ "user_id": h.owner, "pin": "4826" })).await["token"].as_str().unwrap().to_string();

    // A "newer" hub that refuses this till's protocol with 426.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let fake = listener.local_addr().unwrap().port();
    let app = axum::Router::new().fallback(|| async {
        let body = json!({ "code": "conflict", "message": "This hub requires AMWAPOS sync protocol 3 (encrypted). Install the same AMWAPOS version on the hub and this terminal." });
        (axum::http::StatusCode::UPGRADE_REQUIRED, axum::Json(body))
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let point = |port: u16| {
        core.db
            .write(|tx| {
                let mut s: Value = amwapos_core::settings::get_raw(tx, amwapos_core::sync::KEY_SYNC)?.unwrap();
                s["hub_url"] = json!(format!("http://127.0.0.1:{port}"));
                amwapos_core::settings::put(tx, amwapos_core::sync::KEY_SYNC, &s, None)
            })
            .unwrap()
    };
    point(fake);
    let err = term.dispatch("sync.run_now", Some(tt.clone()), json!({})).await.unwrap_err();
    assert!(err.message.contains("protocol 3"), "{}", err.message);
    let st = call(&term, "sync.status", Some(&tt), json!({})).await;
    assert_eq!(st["last_error_kind"], "version_mismatch", "{st}");

    point(free_port());
    term.dispatch("sync.run_now", Some(tt.clone()), json!({})).await.unwrap_err();
    let st = call(&term, "sync.status", Some(&tt), json!({})).await;
    assert_eq!(st["last_error_kind"], "unreachable", "{st}");

    // Back on the real hub: the error clears.
    point(h.port);
    call(&term, "sync.run_now", Some(&tt), json!({})).await;
    let st = call(&term, "sync.status", Some(&tt), json!({})).await;
    assert!(st["last_error_kind"].is_null(), "{st}");
}
