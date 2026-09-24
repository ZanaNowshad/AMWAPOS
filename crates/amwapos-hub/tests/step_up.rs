//! Windows Hello step-up: only when the module is on, only for sensitive
//! commands, in addition to the PIN, and audited.

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::{Runtime, StepUp};
use serde_json::{json, Value};

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Value {
    match rt.dispatch(cmd, token.map(|t| t.to_string()), args).await {
        Ok(v) => v,
        Err(e) => panic!("{cmd} failed: {} ({:?})", e.message, e.code),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_is_an_extra_step_for_approvals() {
    let dir = tempfile::tempdir().unwrap();
    let core = Arc::new(AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
    let rt = Runtime::with_bind(core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(500));
    call(&rt, "setup.initialize", None, json!({ "business_name": "T", "branch_name": "M", "vat_rate_bp": 1000, "owner_name": "O", "owner_pin": "4826", "device_name": "D", "device_code": "T01" })).await;
    let owner = call(&rt, "auth.users", None, json!({})).await[0]["user_id"].as_str().unwrap().to_string();
    let t = call(&rt, "auth.login", None, json!({ "user_id": owner, "pin": "4826" })).await["token"].as_str().unwrap().to_string();

    let calls = Arc::new(AtomicUsize::new(0));
    let answer = Arc::new(std::sync::Mutex::new(StepUp::Refused("Canceled".into())));
    {
        let (calls, answer) = (calls.clone(), answer.clone());
        *rt.step_up.lock().unwrap() = Some(Arc::new(move |_m: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            answer.lock().unwrap().clone()
        }));
    }
    let approve = json!({ "approver_user_id": owner, "pin": "4826", "permission": "pos.discount", "summary": "x" });
    // Module off: PIN alone, Hello not asked.
    call(&rt, "auth.approve", Some(&t), approve.clone()).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    call(&rt, "settings.save", Some(&t), json!({ "key": "features", "value": { "windows_hello": true } })).await;
    // Not a sensitive command: not asked.
    call(&rt, "pos.config", Some(&t), json!({})).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // Refused Hello blocks the approval even with the right PIN.
    let e = rt.dispatch("auth.approve", Some(t.clone()), approve.clone()).await.unwrap_err();
    assert_eq!(e.details.unwrap()["kind"], "step_up_failed");
    // Verified Hello + PIN: approved.
    *answer.lock().unwrap() = StepUp::Verified;
    let a = call(&rt, "auth.approve", Some(&t), approve.clone()).await;
    assert!(a["approval_token"].is_string());
    // Hello verified but wrong PIN: still refused (Hello never replaces the PIN).
    let mut bad = approve.clone();
    bad["pin"] = json!("0000");
    assert!(rt.dispatch("auth.approve", Some(t.clone()), bad).await.is_err());
    let audited: i64 = core
        .db
        .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM audit_logs WHERE event_type='security.step_up'", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(audited, 3);
}
