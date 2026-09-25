//! AI assistant end to end against a fake Messages API on loopback: request
//! shape and headers, the tool loop, proposals that change nothing until a
//! person confirms, confirmation through the normal command, and undo by a
//! compensating record.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::Runtime;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

type Seen = Arc<Mutex<Vec<(HeaderMap, Value)>>>;

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Value {
    match rt.dispatch(cmd, token.map(|t| t.to_string()), args).await {
        Ok(v) => v,
        Err(e) => panic!("{cmd} failed: {} ({:?})", e.message, e.code),
    }
}

async fn fake(State(seen): State<Seen>, headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
    let n = {
        let mut s = seen.lock().unwrap();
        s.push((headers, body.clone()));
        s.len()
    };
    let last = body["messages"].as_array().unwrap().last().unwrap().clone();
    let reply = match n {
        1 => {
            json!({ "content": [{ "type": "text", "text": "Let me look." }, { "type": "tool_use", "id": "tu1", "name": "search_products", "input": { "query": "Laban" } }],
                     "stop_reason": "tool_use", "usage": { "input_tokens": 100, "output_tokens": 20 } })
        }
        2 => {
            // The tool result arrives in one user message; pull the product id from it.
            let result: Value = serde_json::from_str(last["content"][0]["content"].as_str().unwrap()).unwrap();
            let pid = result["data"]["products"][0]["product_id"].as_str().unwrap().to_string();
            json!({ "content": [{ "type": "tool_use", "id": "tu2", "name": "propose_price_change", "input": { "product_id": pid, "new_price": "0.500", "reason": "match competitor" } }],
                    "stop_reason": "tool_use", "usage": { "input_tokens": 150, "output_tokens": 30 } })
        }
        _ => {
            json!({ "content": [{ "type": "text", "text": "I proposed a new price of 0.500. Please confirm it." }], "stop_reason": "end_turn",
                     "usage": { "input_tokens": 200, "output_tokens": 15 } })
        }
    };
    Json(reply)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assistant_reads_proposes_and_a_person_confirms() {
    let seen: Seen = Arc::new(Mutex::new(vec![]));
    let app = Router::new().route("/v1/messages", post(fake)).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

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
    let users = call(&rt, "auth.users", None, json!({})).await;
    let owner = users[0]["user_id"].as_str().unwrap().to_string();
    let t = call(&rt, "auth.login", None, json!({ "user_id": owner, "pin": "4826" })).await["token"].as_str().unwrap().to_string();
    let tax = call(&rt, "tax.list", Some(&t), json!({})).await[0]["tax_rule_id"].as_str().unwrap().to_string();
    let p = call(
        &rt,
        "products.create",
        Some(&t),
        json!({ "name": "Laban 1L", "tax_rule_id": tax, "unit": "pcs", "track_inventory": true, "price_minor": 450, "cost_minor": 300, "barcodes": ["7001"] }),
    )
    .await;
    let pid = p["product_id"].as_str().unwrap().to_string();

    // Off by default: refused in the backend.
    let e = rt.dispatch("ai.ask", Some(t.clone()), json!({ "message": "hi" })).await.unwrap_err();
    assert_eq!(e.details.unwrap()["kind"], "feature_disabled");
    call(&rt, "settings.save", Some(&t), json!({ "key": "features", "value": { "ai": true, "ai_mutations": true } })).await;
    // Default provider: the offline test model, no key or consent needed.
    let conv = call(&rt, "ai.ask", Some(&t), json!({ "message": "low stock" })).await;
    let last = conv["messages"].as_array().unwrap().last().unwrap().clone();
    assert!(last["text"].as_str().unwrap().contains("offline test model"), "{last}");
    // It drives the real proposal flow: search → propose; nothing changes until confirmed.
    let conv = call(&rt, "ai.ask", Some(&t), json!({ "message": "Set the price of Laban 1L to 0.600" })).await;
    let fake_prop = &conv["proposals"][0];
    assert_eq!(fake_prop["status"], "proposed", "{conv}");
    assert_eq!(fake_prop["preview"]["new_price_minor"], 600);
    assert_eq!(call(&rt, "products.get", Some(&t), json!({ "product_id": pid })).await["price_minor"], 450);
    // A real provider needs consent and a key.
    call(
        &rt,
        "ai.configure",
        Some(&t),
        json!({ "settings": { "provider": "anthropic", "model": "claude-opus-5", "base_url": "", "max_tokens": 16000, "fallbacks": true, "consent": false } }),
    )
    .await;
    let e = rt.dispatch("ai.ask", Some(t.clone()), json!({ "message": "hi" })).await.unwrap_err();
    assert_eq!(e.details.unwrap()["kind"], "ai_not_configured");
    let audits_before: i64 = core
        .db
        .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM audit_logs WHERE event_type IN ('ai.request','ai.proposal.created','ai.proposal.executed','ai.proposal.undone')", [], |r| r.get(0))?))
        .unwrap();
    let st = call(
        &rt,
        "ai.configure",
        Some(&t),
        json!({ "settings": { "provider": "anthropic", "model": "claude-opus-5", "base_url": format!("http://127.0.0.1:{port}"), "max_tokens": 16000,
                              "fallbacks": true, "consent": true }, "api_key": "sk-test-123" }),
    )
    .await;
    assert_eq!(st["ready"], true);
    assert!(st.to_string().find("sk-test-123").is_none(), "the key is never returned");

    let conv = call(&rt, "ai.ask", Some(&t), json!({ "message": "Raise Laban to 0.500" })).await;
    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 3);
    let (h, b) = &reqs[0];
    assert_eq!(h["x-api-key"], "sk-test-123");
    assert_eq!(h["anthropic-version"], "2023-06-01");
    assert_eq!(h["anthropic-beta"], "server-side-fallback-2026-07-01");
    assert_eq!(b["model"], "claude-opus-5");
    assert_eq!(b["fallbacks"], "default");
    assert_eq!(b["thinking"]["type"], "adaptive");
    let tools: Vec<&str> = b["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tools.contains(&"propose_price_change") && !tools.iter().any(|n| n.contains("sql")));
    // Assistant content is echoed back in full, results in one user message.
    let m2 = reqs[1].1["messages"].as_array().unwrap();
    assert_eq!(m2[1]["role"], "assistant");
    assert_eq!(m2[1]["content"][1]["type"], "tool_use");
    assert_eq!(m2[2]["content"][0]["type"], "tool_result");
    assert_eq!(m2[2]["content"][0]["tool_use_id"], "tu1");

    assert_eq!(conv["messages"].as_array().unwrap().last().unwrap()["text"], "I proposed a new price of 0.500. Please confirm it.");
    let prop = &conv["proposals"][0];
    assert_eq!(prop["status"], "proposed");
    assert_eq!(prop["risk"], "medium", "11% change");
    assert_eq!(prop["preview"]["old_price_minor"], 450);
    // Nothing changed yet.
    let p = call(&rt, "products.get", Some(&t), json!({ "product_id": pid })).await;
    assert_eq!(p["price_minor"], 450);

    // A person confirms: executed by the normal price command.
    let id = prop["proposal_id"].as_str().unwrap().to_string();
    let done = call(&rt, "ai.proposal_confirm", Some(&t), json!({ "proposal_id": id })).await;
    assert_eq!(done["status"], "executed");
    assert_eq!(call(&rt, "products.get", Some(&t), json!({ "product_id": pid })).await["price_minor"], 500);
    assert!(rt.dispatch("ai.proposal_confirm", Some(t.clone()), json!({ "proposal_id": id })).await.is_err(), "runs once");
    // Undo writes a new price record back to 0.450.
    let undone = call(&rt, "ai.proposal_undo", Some(&t), json!({ "proposal_id": id })).await;
    assert_eq!(undone["status"], "undone");
    assert_eq!(call(&rt, "products.get", Some(&t), json!({ "product_id": pid })).await["price_minor"], 450);
    let n: i64 =
        core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM product_prices WHERE product_id=?1", [&pid], |r| r.get(0))?)).unwrap();
    assert_eq!(n, 3, "history kept: original, AI change, undo");
    let audits: i64 = core
        .db
        .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM audit_logs WHERE event_type IN ('ai.request','ai.proposal.created','ai.proposal.executed','ai.proposal.undone')", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(audits - audits_before, 4);

    // A cashier cannot use the assistant.
    let cashier =
        call(&rt, "users.create", Some(&t), json!({ "user": { "display_name": "Sara", "role_id": "role_cashier", "pin": "2468" } })).await;
    let ct =
        call(&rt, "auth.login", None, json!({ "user_id": cashier["user_id"], "pin": "2468" })).await["token"].as_str().unwrap().to_string();
    let e = rt.dispatch("ai.ask", Some(ct), json!({ "message": "hi" })).await.unwrap_err();
    assert_eq!(e.code, amwapos_core::ErrorCode::Forbidden);
}
