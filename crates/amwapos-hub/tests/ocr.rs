//! OCR worker with the real bundled models and a real Tesseract: payment
//! screenshots and invoices are read on the worker task, and missing models
//! keep OCR off with `ocr_model_missing`. Skips when Tesseract is not
//! installed unless AMWAPOS_REQUIRE_OCR=1 (CI sets it).

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::ocr_worker::OcrPaths;
use amwapos_hub::Runtime;
use serde_json::{json, Value};

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Value {
    match rt.dispatch(cmd, token.map(|t| t.to_string()), args).await {
        Ok(v) => v,
        Err(e) => panic!("{cmd} failed: {} ({:?})", e.message, e.code),
    }
}

fn fixture(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    amwapos_core::ids::b64(&std::fs::read(p).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ocr_worker_reads_screenshots_and_invoices() {
    let have = std::process::Command::new("tesseract").arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    if !have {
        assert!(std::env::var("AMWAPOS_REQUIRE_OCR").is_err(), "Tesseract is required in CI");
        eprintln!("skipped: tesseract not installed");
        return;
    }
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
    let features =
        json!({ "key": "features", "value": { "ocr.enabled": true, "ocr.payment_screenshots": true, "ocr.supplier_invoices": true } });

    // Models missing: turning OCR on is refused and the flag stays off.
    let empty = tempfile::tempdir().unwrap();
    rt.ocr.set_paths(OcrPaths { tesseract: Some("tesseract".into()), models: Some(empty.path().to_path_buf()) });
    let e = rt.dispatch("settings.save", Some(t.clone()), features.clone()).await.unwrap_err();
    assert_eq!(e.code, amwapos_core::ErrorCode::OcrModelMissing);
    assert!(!core.features().unwrap().is_on("ocr.enabled"));

    // Bundled models: OCR turns on and the worker reads images.
    rt.ocr.set_paths(OcrPaths::discover(None));
    call(&rt, "settings.save", Some(&t), features).await;
    let st = call(&rt, "ocr.status", Some(&t), json!({})).await;
    assert_eq!(st["ocr"]["available"], true, "{st}");
    assert_eq!(st["ocr"]["languages"], json!(["ara", "eng"]));
    assert!(dir.path().join("ocr/tessdata/eng.traineddata").exists());

    let r = call(
        &rt,
        "payreviews.upload",
        Some(&t),
        json!({ "file_name": "shot.png", "data": fixture("payment.png"), "expected_minor": 12_500 }),
    )
    .await;
    let id = r["review_id"].as_str().unwrap().to_string();
    let mut review = Value::Null;
    for _ in 0..300 {
        review = call(&rt, "payreviews.get", Some(&t), json!({ "review_id": id })).await["review"].clone();
        if review["status"] != "pending" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(review["detected_minor"], 12_500, "{review}");
    assert!(matches!(review["status"].as_str(), Some("ocr_match" | "likely_match")), "{review}");
    // Never settlement by itself.
    assert!(review["decided_at"].is_null());

    let s = call(&rt, "invoicescan.import", Some(&t), json!({ "file_name": "invoice.png", "data": fixture("invoice.png") })).await;
    let sid = s["scan_id"].as_str().unwrap().to_string();
    let mut v = Value::Null;
    for _ in 0..300 {
        v = call(&rt, "invoicescan.get", Some(&t), json!({ "scan_id": sid })).await;
        if v["scan"]["status"] != "imported" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(v["scan"]["status"], "review", "{v}");
    assert!(v["ocr_text"].as_str().unwrap().contains("Milk"), "{v}");
    assert_eq!(v["scan"]["invoice_number"], "4471");
}
