//! The real Node sidecar under the Rust supervisor: loopback start-up,
//! health/identity/WhatsApp/OCR states, and an OCR job run end to end
//! through the automation loop. Skipped when Node or the sidecar's modules
//! are not installed (`npm ci --prefix sidecar`).

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::sidecar::SidecarPaths;
use amwapos_hub::Runtime;
use serde_json::{json, Value};

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Value {
    match rt.dispatch(cmd, token.map(|t| t.to_string()), args).await {
        Ok(v) => v,
        Err(e) => panic!("{cmd} failed: {} ({:?})", e.message, e.code),
    }
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sidecar_supervised_states_and_ocr_job() {
    let root = repo();
    let node_ok = std::process::Command::new("node").arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    if !node_ok || !root.join("sidecar/node_modules/tesseract.js").exists() {
        eprintln!("skipped: node or sidecar/node_modules missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let core = Arc::new(AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
    let rt = Runtime::with_bind(core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(500));
    rt.sidecar.set_paths(Some(SidecarPaths { node: "node".into(), script: root.join("sidecar/src/main.mjs"), models: None }));
    call(
        &rt,
        "setup.initialize",
        None,
        json!({ "business_name": "Test Mart", "branch_name": "Main", "vat_rate_bp": 1000, "owner_name": "Owner",
                "owner_pin": "4826", "device_name": "Till", "device_code": "T01" }),
    )
    .await;
    let users = call(&rt, "auth.users", None, json!({})).await;
    let owner = users[0]["user_id"].as_str().unwrap().to_string();
    let login = call(&rt, "auth.login", None, json!({ "user_id": owner, "pin": "4826" })).await;
    let t = login["token"].as_str().unwrap().to_string();

    // Module off: nothing running, and WhatsApp connect is refused.
    let st = call(&rt, "sidecar.status", Some(&t), json!({})).await;
    assert_eq!(st["process"], "stopped");
    assert!(rt.dispatch("whatsapp.connect", Some(t.clone()), json!({})).await.is_err());

    // OCR on: the automation loop starts the sidecar.
    call(&rt, "settings.save", Some(&t), json!({ "key": "features", "value": { "ocr": true, "whatsapp": true } })).await;
    let mut st = Value::Null;
    for _ in 0..60 {
        st = call(&rt, "sidecar.status", Some(&t), json!({})).await;
        if st["identity"] == true {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(st["process"], "running", "{st}");
    assert_eq!(st["health"], true);
    assert_eq!(st["identity"], true);
    assert_eq!(st["ocr"]["enabled"], true);
    assert_eq!(st["whatsapp"]["connected"], false);
    assert_eq!(st["whatsapp"]["ready"], false);
    assert!(st["port"].as_u64().unwrap() > 0);

    // Invoice image → OCR by the loop → lines ready for review.
    let img = root.join("sidecar/test/fixtures/invoice.png");
    let data = amwapos_core::ids::b64(&std::fs::read(&img).unwrap());
    let scan = call(&rt, "invoicescan.import", Some(&t), json!({ "file_name": "invoice.png", "data": data })).await;
    let id = scan["scan_id"].as_str().unwrap().to_string();
    let mut v = Value::Null;
    for _ in 0..120 {
        v = call(&rt, "invoicescan.get", Some(&t), json!({ "scan_id": id })).await;
        if v["scan"]["status"] != "imported" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(v["scan"]["status"], "review", "{}", v["scan"]);
    assert!(v["ocr_text"].as_str().unwrap().contains("INVOICE 4471"));
    assert_eq!(v["lines"][0]["unit_cost_minor"], 450);

    // Both modules off: the sidecar is stopped.
    call(&rt, "settings.save", Some(&t), json!({ "key": "features", "value": {} })).await;
    for _ in 0..40 {
        if !rt.sidecar.is_running().await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(!rt.sidecar.is_running().await);
}
