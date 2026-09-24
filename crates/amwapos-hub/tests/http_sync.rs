use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::Runtime;
use serde_json::{json, Value};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Value {
    match rt.dispatch(cmd, token.map(|t| t.to_string()), args).await {
        Ok(v) => v,
        Err(e) => panic!("{cmd} failed: {} ({:?})", e.message, e.code),
    }
}

fn op() -> String {
    ulid::Ulid::new().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_pairs_and_syncs_over_http() {
    let hub_dir = tempfile::tempdir().unwrap();
    let hub_core = Arc::new(AppCore::open(hub_dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
    let hub = Runtime::with_bind(hub_core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(200));
    call(
        &hub,
        "setup.initialize",
        None,
        json!({
            "business_name": "Test Mart", "branch_name": "Main", "vat_rate_bp": 1000, "owner_name": "Owner",
            "owner_pin": "4826", "device_name": "Hub PC", "device_code": "H01"
        }),
    )
    .await;
    let users = call(&hub, "auth.users", None, json!({})).await;
    let owner = users[0]["user_id"].as_str().unwrap().to_string();
    let ht = call(&hub, "auth.login", None, json!({ "user_id": owner, "pin": "4826" })).await["token"].as_str().unwrap().to_string();
    // Use a free port for the hub API.
    let port = free_port();
    hub_core.db.write(|tx| amwapos_core::settings::put(tx, amwapos_core::sync::KEY_SYNC, &json!({ "port": port }), None)).unwrap();
    call(&hub, "settings.save", Some(&ht), json!({ "key": "features", "value": { "hub": true } })).await;
    call(&hub, "sync.enable_hub", Some(&ht), json!({})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let tax = call(&hub, "tax.list", Some(&ht), json!({})).await[0]["tax_rule_id"].as_str().unwrap().to_string();
    call(&hub, "products.create", Some(&ht), json!({
        "name": "Dates 1kg", "tax_rule_id": tax, "price_minor": 2500, "cost_minor": 1500, "barcodes": ["0012345678905"], "opening_stock_milli": 20000
    })).await;
    let code = call(&hub, "sync.pairing_code", Some(&ht), json!({ "device_name": "Till 2" })).await["code"].as_str().unwrap().to_string();

    // Fresh terminal joins over HTTP.
    let term_dir = tempfile::tempdir().unwrap();
    let term_core = Arc::new(AppCore::open(term_dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
    let term = Runtime::with_bind(term_core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(200));
    let url = format!("127.0.0.1:{port}");
    let probe = call(&term, "sync.probe", None, json!({ "hub_url": url })).await;
    assert_eq!(probe["info"]["business_name"], "Test Mart");
    // Wrong code is refused.
    let bad = term
        .dispatch("sync.join", None, json!({ "hub_url": url, "code": "00000000", "device_name": "Till 2", "device_code": "T02" }))
        .await;
    assert!(bad.is_err());
    let st = call(&term, "sync.join", None, json!({ "hub_url": url, "code": code, "device_name": "Till 2", "device_code": "T02" })).await;
    assert_eq!(st["setup_complete"], true);
    let tt = call(&term, "auth.login", None, json!({ "user_id": owner, "pin": "4826" })).await["token"].as_str().unwrap().to_string();
    call(&term, "shift.open", Some(&tt), json!({ "opening_float_minor": 0, "operation_id": op() })).await;
    let scan = call(&term, "pos.scan", Some(&tt), json!({ "barcode": "0012345678905" })).await;
    assert_eq!(scan["outcome"], "added");
    let sale = call(
        &term,
        "pos.finalize",
        Some(&tt),
        json!({
            "cart_id": scan["cart"]["cart_id"], "operation_id": op(), "tenders": [{ "method": "cash", "amount_minor": 2500 }]
        }),
    )
    .await;
    assert!(sale["receipt_number"].as_str().unwrap().starts_with("T02-"));
    let r = call(&term, "sync.run_now", Some(&tt), json!({})).await;
    assert!(r["pushed"].as_u64().unwrap() >= 3, "{r}");
    let sales_on_hub: i64 = hub_core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM sales", [], |r| r.get(0))?)).unwrap();
    assert_eq!(sales_on_hub, 1);
    let stock_on_hub: i64 = hub_core.db.read(|c| Ok(c.query_row("SELECT qty_milli FROM stock_levels", [], |r| r.get(0))?)).unwrap();
    assert_eq!(stock_on_hub, 19_000);

    // Hub goes away: terminal keeps selling; sync reports a retryable error.
    let hub_status_before = call(&term, "sync.status", Some(&tt), json!({})).await;
    assert_eq!(hub_status_before["pending"], 0);
    drop(hub);
    // Point the terminal at a dead port to simulate the outage deterministically.
    term_core
        .db
        .write(|tx| {
            let mut s: Value = amwapos_core::settings::get_raw(tx, amwapos_core::sync::KEY_SYNC)?.unwrap();
            s["hub_url"] = json!(format!("http://127.0.0.1:{}", free_port()));
            amwapos_core::settings::put(tx, amwapos_core::sync::KEY_SYNC, &s, None)
        })
        .unwrap();
    let scan = call(&term, "pos.scan", Some(&tt), json!({ "barcode": "0012345678905" })).await;
    call(
        &term,
        "pos.finalize",
        Some(&tt),
        json!({
            "cart_id": scan["cart"]["cart_id"], "operation_id": op(), "tenders": [{ "method": "cash", "amount_minor": 2500 }]
        }),
    )
    .await;
    let err = term.dispatch("sync.run_now", Some(tt.clone()), json!({})).await.unwrap_err();
    assert!(err.retryable, "{}", err.message);
    assert!(err.message.contains("not reachable"));
    let st = call(&term, "sync.status", Some(&tt), json!({})).await;
    assert!(st["pending"].as_i64().unwrap() >= 3);
    assert!(st["last_error"].is_string());
    let local_sales: i64 = term_core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM sales", [], |r| r.get(0))?)).unwrap();
    assert_eq!(local_sales, 2);
}
