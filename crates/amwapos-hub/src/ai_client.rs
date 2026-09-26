//! AI provider calls and the tool loop. The core supplies the conversation,
//! the system prompt (constitution + playbooks, assembled in Rust) and the
//! tool catalogue, executes every tool call itself (as the signed-in user)
//! and stores every message; this module only speaks HTTP.
//!
//! Bring your own API key. Consumer subscriptions (ChatGPT Plus, Claude Pro,
//! Gemini Advanced, Codex) cannot be signed into: there is no OAuth here.
//!
//! Providers:
//! - `fake`: offline deterministic test model (the default); no network.
//! - `openai`, `openrouter`, `custom`: OpenAI Chat Completions
//!   (`GET {base}/models`, `POST {base}/chat/completions`, base ends in /v1).
//! - `anthropic`: Messages API (`POST /v1/messages`, `GET /v1/models`).
//! - `google`: Gemini `models/{id}:generateContent` and `models` (AI Studio key,
//!   sent as the `x-goog-api-key` header, never in the URL).
//!
//! One retry on 429/502/503; the timeout comes from settings. Errors carry the
//! HTTP status and a message with any secret replaced.

use std::sync::Arc;
use std::time::Duration;

use amwapos_core::ai::{AiConnection, AiSettings, AiTurn};
use amwapos_core::{AppError, AppResult, ErrorCode};
use serde_json::{json, Value};

const MAX_ROUNDS: usize = 8;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::internal(format!("worker failed: {e}")))?
}

struct Reply {
    content: Value,
    stop_reason: String,
    input_tokens: i64,
    output_tokens: i64,
}

/// Replace every secret in provider text before it reaches the UI or a log.
fn sanitize(text: &str, secrets: &[&str]) -> String {
    let mut t: String = text.chars().take(300).collect();
    for s in secrets.iter().filter(|s| s.len() >= 4) {
        t = t.replace(*s, "***");
    }
    t
}

fn secrets_of<'a>(key: &'a str, header: &'a Option<(String, String)>) -> Vec<&'a str> {
    let mut v = vec![key];
    if let Some((_, h)) = header {
        v.push(h.as_str());
    }
    v
}

fn provider_error(status: u16, body: &Value, secrets: &[&str], model_call: bool) -> AppError {
    let raw = body
        .pointer("/error/message")
        .or_else(|| body.pointer("/error"))
        .and_then(|m| m.as_str())
        .unwrap_or("The AI provider returned an error.");
    let msg = sanitize(raw, secrets);
    let lower = msg.to_lowercase();
    if model_call
        && (status == 404
            || (status == 400 && lower.contains("model") && (lower.contains("not found") || lower.contains("does not exist"))))
    {
        return AppError::new(ErrorCode::AiModelNotFound, format!("The model was not found at the provider ({status}): {msg}"))
            .with_details(json!({ "kind": "ai_model", "status": status }));
    }
    let kind = match status {
        401 | 403 => "ai_auth",
        429 => "ai_rate_limited",
        _ => "ai_provider",
    };
    AppError::new(ErrorCode::AiProviderError, format!("AI provider error ({status}): {msg}"))
        .with_details(json!({ "kind": kind, "status": status }))
}

fn net_error(e: reqwest::Error, secrets: &[&str]) -> AppError {
    if e.is_timeout() {
        return AppError::new(
            ErrorCode::AiTimeout,
            "The AI provider did not answer in time. Try again, or raise the timeout in Settings → AI.",
        )
        .with_details(json!({ "kind": "ai_timeout" }));
    }
    AppError::new(ErrorCode::AiProviderError, format!("Could not reach the AI provider: {}", sanitize(&e.to_string(), secrets)))
        .with_details(json!({ "kind": "ai_unreachable", "status": 0 }))
}

/// Send once, and once more on 429/502/503. Returns (status, JSON body).
async fn send(req: reqwest::RequestBuilder, secrets: &[&str]) -> AppResult<(u16, Value)> {
    let retry = req.try_clone();
    let resp = req.send().await.map_err(|e| net_error(e, secrets))?;
    let mut status = resp.status().as_u16();
    let mut v: Value = resp.json().await.unwrap_or(Value::Null);
    if matches!(status, 429 | 502 | 503) {
        if let Some(r) = retry {
            tokio::time::sleep(Duration::from_millis(800)).await;
            let resp = r.send().await.map_err(|e| net_error(e, secrets))?;
            status = resp.status().as_u16();
            v = resp.json().await.unwrap_or(Value::Null);
        }
    }
    Ok((status, v))
}

fn with_header(req: reqwest::RequestBuilder, header: &Option<(String, String)>) -> reqwest::RequestBuilder {
    match header {
        Some((k, v)) => req.header(k.as_str(), v.as_str()),
        None => req,
    }
}

fn supports_adaptive_thinking(model: &str) -> bool {
    ["claude-opus-5", "claude-fable-5", "claude-sonnet-5", "claude-opus-4-8", "claude-opus-4-7", "claude-opus-4-6", "claude-sonnet-4-6"]
        .iter()
        .any(|p| model.starts_with(p))
}

fn supports_default_fallbacks(model: &str) -> bool {
    model == "claude-opus-5" || model.starts_with("claude-fable-5-1")
}

async fn anthropic(http: &reqwest::Client, turn: &AiTurn) -> AppResult<Reply> {
    let st = &turn.settings;
    let base = st.effective_base();
    let mut body = json!({
        "model": st.model_id,
        "max_tokens": st.max_output_tokens,
        "system": turn.system,
        "tools": turn.tools,
        "messages": turn.messages,
        "cache_control": { "type": "ephemeral" },
    });
    if supports_adaptive_thinking(&st.model_id) {
        body["thinking"] = json!({ "type": "adaptive" });
    }
    let mut req = http
        .post(format!("{base}/v1/messages"))
        .header("x-api-key", &turn.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");
    if st.fallbacks && supports_default_fallbacks(&st.model_id) {
        body["fallbacks"] = json!("default");
        req = req.header("anthropic-beta", "server-side-fallback-2026-07-01");
    }
    let secrets = secrets_of(&turn.api_key, &turn.extra_header);
    let (status, v) = send(with_header(req, &turn.extra_header).json(&body), &secrets).await?;
    if status != 200 {
        return Err(provider_error(status, &v, &secrets, true));
    }
    Ok(Reply {
        content: v.get("content").cloned().unwrap_or(json!([])),
        stop_reason: v.get("stop_reason").and_then(|s| s.as_str()).unwrap_or("end_turn").to_string(),
        input_tokens: v.pointer("/usage/input_tokens").and_then(|x| x.as_i64()).unwrap_or(0),
        output_tokens: v.pointer("/usage/output_tokens").and_then(|x| x.as_i64()).unwrap_or(0),
    })
}

/// Offline test model. Understands a few phrasings and otherwise explains
/// itself; tool calls go through the same permission-checked tools.
fn fake(turn: &AiTurn) -> Reply {
    let has = |n: &str| turn.tools.iter().any(|t| t["name"] == n);
    let question = turn
        .messages
        .iter()
        .rev()
        .filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"].as_array()?.iter().find(|b| b["type"] == "text")?.get("text")?.as_str().map(str::to_lowercase))
        .find(|t| !t.starts_with(&amwapos_core::ai::NUDGE_PREFIX.to_lowercase()))
        .unwrap_or_default();
    let nudged =
        turn.messages.last().and_then(|m| m["content"][0]["text"].as_str()).is_some_and(|t| t.starts_with(amwapos_core::ai::NUDGE_PREFIX));
    let last = turn.messages.last().cloned().unwrap_or(Value::Null);
    let results: Vec<Value> = if last["role"] == "user" {
        last["content"].as_array().cloned().unwrap_or_default().into_iter().filter(|b| b["type"] == "tool_result").collect()
    } else {
        vec![]
    };
    let prev_tool = turn.messages.iter().rev().find(|m| m["role"] == "assistant").and_then(|m| {
        m["content"].as_array()?.iter().find(|b| b["type"] == "tool_use").and_then(|b| b["name"].as_str().map(str::to_string))
    });
    // "price of <name> to <amount>"
    let price_intent = question.split_once("price of ").and_then(|(_, rest)| {
        let (name, amount) = rest.rsplit_once(" to ")?;
        let amount = amount.split_whitespace().next()?.trim_end_matches(['.', '?']).to_string();
        amount.parse::<f64>().ok()?;
        Some((name.trim().to_string(), amount))
    });
    let n = turn.messages.len();
    let call = |name: &str, input: Value| Reply {
        content: json!([{ "type": "tool_use", "id": format!("fake_{n}"), "name": name, "input": input }]),
        stop_reason: "tool_use".into(),
        input_tokens: 0,
        output_tokens: 0,
    };
    let say = |text: String| Reply {
        content: json!([{ "type": "text", "text": text }]),
        stop_reason: "end_turn".into(),
        input_tokens: 0,
        output_tokens: 0,
    };
    if let Some(r) = results.first() {
        let body = r["content"].as_str().unwrap_or_default().to_string();
        if prev_tool.as_deref() == Some("search_products") {
            if let (Some((_, amount)), true) = (&price_intent, has("propose_price_change")) {
                let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let pid = find_key(&parsed, "product_id");
                return match pid {
                    Some(pid) => call(
                        "propose_price_change",
                        json!({ "product_id": pid, "new_price": amount, "reason": "Requested in the assistant (offline test model)" }),
                    ),
                    None => say("I could not find that product.".into()),
                };
            }
        }
        if prev_tool.as_deref().is_some_and(|t| t.starts_with("propose_")) {
            return say("I prepared a proposal. Review it below and confirm it if it is right; nothing changes until you do.".into());
        }
        let short: String = body.chars().take(800).collect();
        return say(format!("Here is what I found (offline test model):\n{short}"));
    }
    // B1 test phrasings: "guess" keeps stating figures without a tool;
    // "estimate" answers from memory first, then calls a tool when nudged.
    if question.contains("guess") || (question.contains("estimate") && !nudged) {
        return say("Sales today were about 123.450.".into());
    }
    if question.contains("estimate") && has("dashboard_kpis") {
        return call("dashboard_kpis", json!({}));
    }
    if let Some((name, _)) = &price_intent {
        return call("search_products", json!({ "query": name, "limit": 1 }));
    }
    // Eval pack (B10): golden phrasings → the tool a real model should pick.
    for (words, tool, input) in EVAL_ROUTES {
        if has(tool) && words.iter().any(|w| question.contains(w)) {
            return call(tool, serde_json::from_str(input).unwrap_or(json!({})));
        }
    }
    if question.contains("low stock") || question.contains("reorder") {
        return call("low_stock", json!({ "limit": 20 }));
    }
    if question.contains("report") {
        return call("list_reports", json!({}));
    }
    if let Some(q) = question.strip_prefix("find ").or_else(|| question.strip_prefix("search ")) {
        return call("search_products", json!({ "query": q.trim(), "limit": 10 }));
    }
    say("This is the offline test model; it only understands: \"low stock\", \"reports\", \"find <product>\" and \"price of <product> to <amount>\". \
         An owner can add a provider key in Settings → AI for real answers."
        .into())
}

/// Golden questions for the offline eval pack: phrasing → expected tool.
pub const EVAL_ROUTES: &[(&[&str], &str, &str)] = &[
    (&["kpi", "dashboard"], "dashboard_kpis", "{}"),
    (&["end of day pack", "eod pack"], "eod_pack", "{}"),
    (&["shifts"], "list_shifts", "{\"limit\":10}"),
    (&["suppliers"], "search_suppliers", "{\"q\":\"\"}"),
    (&["audit"], "audit_search", "{\"limit\":20}"),
    (&["feature flags", "which modules"], "list_feature_flags", "{}"),
    (&["backup health", "last backup"], "backup_health", "{}"),
    (&["staff", "users"], "list_users", "{}"),
    (&["deliveries"], "list_deliveries", "{}"),
    (&["purchase orders"], "list_pos", "{}"),
    (&["customers"], "search_customers", "{\"q\":\"\",\"limit\":10}"),
    (&["unknown barcodes", "ghost barcodes"], "unknown_barcodes_list", "{}"),
    (&["diagnostics"], "diagnostics_summary", "{}"),
    (&["whatsapp status"], "whatsapp_status", "{}"),
    (&["devices"], "list_devices", "{}"),
    (&["categories"], "list_categories", "{}"),
    (&["pending proposals", "action inbox"], "pending_proposals", "{}"),
];

fn find_key(v: &Value, key: &str) -> Option<String> {
    match v {
        Value::Object(m) => m.get(key).and_then(|x| x.as_str()).map(str::to_string).or_else(|| m.values().find_map(|x| find_key(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

/// Neutral (block) history → chat-completions messages.
fn to_openai_messages(system: &str, messages: &[Value]) -> Vec<Value> {
    let mut out = vec![json!({ "role": "system", "content": system })];
    for m in messages {
        let blocks = m["content"].as_array().cloned().unwrap_or_default();
        if m["role"] == "assistant" {
            let text: Vec<&str> = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
            let calls: Vec<Value> = blocks
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .map(|b| json!({ "id": b["id"], "type": "function", "function": { "name": b["name"], "arguments": b["input"].to_string() } }))
                .collect();
            let mut msg = json!({ "role": "assistant", "content": text.join("\n") });
            if !calls.is_empty() {
                msg["tool_calls"] = json!(calls);
            }
            out.push(msg);
        } else {
            for b in &blocks {
                match b["type"].as_str() {
                    Some("tool_result") => out.push(json!({ "role": "tool", "tool_call_id": b["tool_use_id"], "content": b["content"] })),
                    Some("text") => out.push(json!({ "role": "user", "content": b["text"] })),
                    _ => {}
                }
            }
        }
    }
    out
}

async fn openai_compatible(http: &reqwest::Client, turn: &AiTurn) -> AppResult<Reply> {
    let st = &turn.settings;
    let tools: Vec<Value> = turn
        .tools
        .iter()
        .map(|t| json!({ "type": "function", "function": { "name": t["name"], "description": t["description"], "parameters": t["input_schema"] } }))
        .collect();
    let mut body =
        json!({ "model": st.model_id, "max_tokens": st.max_output_tokens, "messages": to_openai_messages(&turn.system, &turn.messages) });
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    let secrets = secrets_of(&turn.api_key, &turn.extra_header);
    let req = with_header(http.post(format!("{}/chat/completions", st.effective_base())).bearer_auth(&turn.api_key), &turn.extra_header);
    let (status, v) = send(req.json(&body), &secrets).await?;
    if status != 200 {
        return Err(provider_error(status, &v, &secrets, true));
    }
    let msg = v.pointer("/choices/0/message").cloned().unwrap_or(Value::Null);
    let mut content = vec![];
    if let Some(t) = msg["content"].as_str().filter(|t| !t.is_empty()) {
        content.push(json!({ "type": "text", "text": t }));
    }
    for c in msg["tool_calls"].as_array().cloned().unwrap_or_default() {
        let input: Value =
            c.pointer("/function/arguments").and_then(|a| a.as_str()).and_then(|a| serde_json::from_str(a).ok()).unwrap_or(json!({}));
        content.push(json!({ "type": "tool_use", "id": c["id"], "name": c.pointer("/function/name"), "input": input }));
    }
    let stop = match v.pointer("/choices/0/finish_reason").and_then(|f| f.as_str()) {
        Some("tool_calls") => "tool_use",
        Some("length") => "max_tokens",
        Some("content_filter") => "refusal",
        _ if content.iter().any(|b| b["type"] == "tool_use") => "tool_use",
        _ => "end_turn",
    };
    Ok(Reply {
        content: json!(content),
        stop_reason: stop.to_string(),
        input_tokens: v.pointer("/usage/prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0),
        output_tokens: v.pointer("/usage/completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0),
    })
}

/// JSON schema subset Gemini accepts (no additionalProperties).
fn gemini_schema(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter().filter(|(k, _)| k.as_str() != "additionalProperties").map(|(k, x)| (k.clone(), gemini_schema(x))).collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(gemini_schema).collect()),
        x => x.clone(),
    }
}

/// Neutral (block) history → Gemini contents.
fn to_gemini_contents(messages: &[Value]) -> Vec<Value> {
    let mut names = std::collections::HashMap::new();
    let mut out = vec![];
    for m in messages {
        let blocks = m["content"].as_array().cloned().unwrap_or_default();
        let mut parts = vec![];
        for b in &blocks {
            match b["type"].as_str() {
                Some("text") => parts.push(json!({ "text": b["text"] })),
                Some("tool_use") => {
                    names.insert(b["id"].as_str().unwrap_or_default().to_string(), b["name"].clone());
                    parts.push(json!({ "functionCall": { "name": b["name"], "args": b["input"] } }));
                }
                Some("tool_result") => {
                    let name = names.get(b["tool_use_id"].as_str().unwrap_or_default()).cloned().unwrap_or(json!("tool"));
                    let content = b["content"].as_str().and_then(|c| serde_json::from_str::<Value>(c).ok()).unwrap_or(b["content"].clone());
                    parts.push(json!({ "functionResponse": { "name": name, "response": { "content": content } } }));
                }
                _ => {}
            }
        }
        if !parts.is_empty() {
            out.push(json!({ "role": if m["role"] == "assistant" { "model" } else { "user" }, "parts": parts }));
        }
    }
    out
}

async fn google(http: &reqwest::Client, turn: &AiTurn) -> AppResult<Reply> {
    let st = &turn.settings;
    let decls: Vec<Value> = turn
        .tools
        .iter()
        .map(|t| {
            let mut d = json!({ "name": t["name"], "description": t["description"] });
            if t["input_schema"]["properties"].as_object().is_some_and(|p| !p.is_empty()) {
                d["parameters"] = gemini_schema(&t["input_schema"]);
            }
            d
        })
        .collect();
    let mut body = json!({
        "systemInstruction": { "parts": [{ "text": turn.system }] },
        "contents": to_gemini_contents(&turn.messages),
        "generationConfig": { "maxOutputTokens": st.max_output_tokens },
    });
    if !decls.is_empty() {
        body["tools"] = json!([{ "functionDeclarations": decls }]);
    }
    let model = st.model_id.trim_start_matches("models/");
    let secrets = secrets_of(&turn.api_key, &turn.extra_header);
    let req = with_header(
        http.post(format!("{}/models/{model}:generateContent", st.effective_base())).header("x-goog-api-key", &turn.api_key),
        &turn.extra_header,
    );
    let (status, v) = send(req.json(&body), &secrets).await?;
    if status != 200 {
        return Err(provider_error(status, &v, &secrets, true));
    }
    let parts = v.pointer("/candidates/0/content/parts").and_then(|p| p.as_array()).cloned().unwrap_or_default();
    let mut content = vec![];
    for (i, p) in parts.iter().enumerate() {
        if let Some(t) = p["text"].as_str().filter(|t| !t.is_empty()) {
            content.push(json!({ "type": "text", "text": t }));
        }
        if let Some(fc) = p.get("functionCall") {
            content.push(json!({ "type": "tool_use", "id": format!("g{}_{i}", turn.messages.len()), "name": fc["name"], "input": fc.get("args").cloned().unwrap_or(json!({})) }));
        }
    }
    let stop = if content.iter().any(|b| b["type"] == "tool_use") {
        "tool_use"
    } else {
        match v.pointer("/candidates/0/finishReason").and_then(|f| f.as_str()) {
            Some("MAX_TOKENS") => "max_tokens",
            Some("SAFETY") | Some("PROHIBITED_CONTENT") | Some("BLOCKLIST") => "refusal",
            _ => "end_turn",
        }
    };
    Ok(Reply {
        content: json!(content),
        stop_reason: stop.to_string(),
        input_tokens: v.pointer("/usageMetadata/promptTokenCount").and_then(|x| x.as_i64()).unwrap_or(0),
        output_tokens: v.pointer("/usageMetadata/candidatesTokenCount").and_then(|x| x.as_i64()).unwrap_or(0),
    })
}

fn http_client(st: &AiSettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(st.timeout_ms.max(1) as u64))
        .build()
        .map_err(|e| AppError::internal(e.to_string()))
}

async fn call_provider(http: &reqwest::Client, turn: &AiTurn) -> AppResult<Reply> {
    match turn.settings.provider.as_str() {
        "fake" => Ok(fake(turn)),
        "openai" | "openrouter" | "custom" => openai_compatible(http, turn).await,
        "anthropic" => anthropic(http, turn).await,
        "google" => google(http, turn).await,
        _ => Err(AppError::new(ErrorCode::AiProviderError, "Unknown AI provider.")),
    }
}

/// Model ids the provider offers (for the Settings dropdown).
pub async fn list_models(conn: &AiConnection) -> AppResult<Vec<String>> {
    let st = &conn.settings;
    if st.provider == "fake" {
        return Ok(vec!["fake-local".into()]);
    }
    let http = http_client(st)?;
    let base = st.effective_base();
    let secrets = secrets_of(&conn.api_key, &conn.extra_header);
    let req = match st.provider.as_str() {
        "anthropic" => http.get(format!("{base}/v1/models")).header("x-api-key", &conn.api_key).header("anthropic-version", "2023-06-01"),
        "google" => http.get(format!("{base}/models")).header("x-goog-api-key", &conn.api_key),
        _ => http.get(format!("{base}/models")).bearer_auth(&conn.api_key),
    };
    let (status, v) = send(with_header(req, &conn.extra_header), &secrets).await?;
    if status != 200 {
        return Err(provider_error(status, &v, &secrets, false));
    }
    let mut ids: Vec<String> = if st.provider == "google" {
        v["models"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m["supportedGenerationMethods"].as_array().is_none_or(|a| a.iter().any(|x| x == "generateContent")))
            .filter_map(|m| m["name"].as_str().map(|n| n.trim_start_matches("models/").to_string()))
            .collect()
    } else {
        v["data"].as_array().cloned().unwrap_or_default().into_iter().filter_map(|m| m["id"].as_str().map(str::to_string)).collect()
    };
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// "Test connection": list models; the result never contains the key.
pub async fn test_connection(conn: &AiConnection) -> Value {
    match list_models(conn).await {
        Ok(ids) => json!({ "ok": true, "status": 200, "models": ids.len(),
                            "model_listed": conn.settings.model_id.is_empty() || ids.contains(&conn.settings.model_id) }),
        Err(e) => json!({ "ok": false, "status": e.details.as_ref().and_then(|d| d.get("status")).cloned().unwrap_or(Value::Null),
                           "code": e.code, "error": e.message }),
    }
}

/// One request without tools (extraction). Returns the text of the reply.
pub async fn complete_once(turn: &AiTurn) -> AppResult<String> {
    if turn.settings.provider == "fake" {
        return Err(AppError::conflict("No AI provider is configured."));
    }
    let http = http_client(&turn.settings)?;
    let reply = call_provider(&http, turn).await?;
    Ok(reply
        .content
        .as_array()
        .map(|a| a.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default())
}

/// The first JSON object in a model reply (tolerates code fences).
pub fn json_object(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(text.get(start..=end)?).ok()
}

/// Ask a question: run the tool loop until the model finishes (or the round
/// limit), storing every message. Settings are re-read on every round, so a
/// provider or model change applies to the next request.
pub async fn ask(
    rt: Arc<crate::runtime::Runtime>,
    token: String,
    conversation_id: Option<String>,
    text: String,
    locale: String,
) -> AppResult<Value> {
    let core = rt.core.clone();
    let mut turn = {
        let (c, t, l) = (core.clone(), token.clone(), locale.clone());
        blocking(move || c.ai_begin_locale(&t, conversation_id, &text, &l)).await?
    };
    let cid = turn.conversation_id.clone();
    let (mut input, mut output, mut rounds) = (0i64, 0i64, 0i64);
    let mut outcome = "completed".to_string();
    // B1: a reply with figures must be backed by a tool call in this question.
    let (mut tools_called, mut nudged) = (false, false);
    for _ in 0..MAX_ROUNDS {
        rounds += 1;
        let http = http_client(&turn.settings)?;
        let reply = match call_provider(&http, &turn).await {
            Ok(r) => r,
            Err(e) => {
                let (c, t, id) = (core.clone(), token.clone(), cid.clone());
                let last = format!("error: {:?} {}", e.code, e.message.chars().take(160).collect::<String>());
                let _ = blocking(move || c.ai_audit_request(&t, &id, rounds, input, output, &last)).await;
                return Err(e);
            }
        };
        input += reply.input_tokens;
        output += reply.output_tokens;
        {
            let (c, id, content, stop) = (core.clone(), cid.clone(), reply.content.clone(), reply.stop_reason.clone());
            let usage = (reply.input_tokens, reply.output_tokens);
            blocking(move || c.ai_store_reply(&id, &content, Some(usage), &stop)).await?;
        }
        match reply.stop_reason.as_str() {
            "tool_use" => {
                tools_called = true;
                let calls: Vec<Value> =
                    reply.content.as_array().cloned().unwrap_or_default().into_iter().filter(|b| b["type"] == "tool_use").collect();
                // All results go back in one user message; every tool is authorized in the core.
                let mut out = vec![];
                for call in calls {
                    let name = call["name"].as_str().unwrap_or_default().to_string();
                    let (c, t, n, i) = (core.clone(), token.clone(), name.clone(), call["input"].clone());
                    let routed = blocking(move || Ok(c.ai_tool_runtime(&t, &n, &i))).await?;
                    let (v, is_error) = match routed {
                        // Runtime-backed reads (WhatsApp/OCR status, hub addresses, updates).
                        Ok(Some((cmd, args))) => {
                            let r = Box::pin(rt.dispatch(&cmd, Some(token.clone()), args)).await;
                            let (c, id) = (core.clone(), cid.clone());
                            blocking(move || Ok(c.ai_tool_runtime_wrap(&id, &name, r))).await?
                        }
                        Err(e) => {
                            let (c, id) = (core.clone(), cid.clone());
                            blocking(move || Ok(c.ai_tool_runtime_wrap(&id, &name, Err(e)))).await?
                        }
                        Ok(None) => {
                            let (c, t, id, i) = (core.clone(), token.clone(), cid.clone(), call["input"].clone());
                            blocking(move || Ok(c.ai_tool(&t, &id, &name, &i))).await?
                        }
                    };
                    out.push(json!({ "type": "tool_result", "tool_use_id": call["id"], "content": v.to_string(), "is_error": is_error }));
                }
                let (c, id) = (core.clone(), cid.clone());
                blocking(move || c.ai_store_tool_results(&id, &json!(out))).await?;
            }
            "pause_turn" => {}
            "refusal" => {
                outcome = "refused".into();
                break;
            }
            "max_tokens" => {
                outcome = "truncated".into();
                break;
            }
            _ => {
                let said: String = reply
                    .content
                    .as_array()
                    .map(|a| a.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                if tools_called || !amwapos_core::ai::has_figures(&said) {
                    break;
                }
                let (c, id) = (core.clone(), cid.clone());
                if nudged {
                    blocking(move || c.ai_mark_unverified(&id)).await?;
                    outcome = "unverified".into();
                    break;
                }
                nudged = true;
                blocking(move || c.ai_nudge(&id)).await?;
            }
        }
        let (c, t, id, l) = (core.clone(), token.clone(), cid.clone(), locale.clone());
        turn = blocking(move || c.ai_continue_locale(&t, &id, &l)).await?;
        if rounds as usize == MAX_ROUNDS {
            outcome = "round_limit".into();
        }
    }
    let (c, t, id) = (core.clone(), token.clone(), cid.clone());
    blocking(move || {
        c.ai_audit_request(&t, &id, rounds, input, output, &outcome)?;
        c.ai_conversation(&t, &id)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_translation_keeps_tool_pairs() {
        let msgs = vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": "hi" }] }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": "looking" }, { "type": "tool_use", "id": "t1", "name": "low_stock", "input": {} }] }),
            json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "{}" }] }),
        ];
        let out = to_openai_messages("sys", &msgs);
        assert_eq!(out.len(), 4);
        assert_eq!(out[2]["tool_calls"][0]["function"]["name"], "low_stock");
        assert_eq!(out[3]["role"], "tool");
        assert_eq!(out[3]["tool_call_id"], "t1");
    }
}
