//! AI provider calls and the tool loop. The core supplies the conversation,
//! system prompt and tool catalogue, executes every tool call itself (as the
//! signed-in user) and stores every message; this module only speaks HTTP.
//!
//! Providers:
//! - `fake`: an offline, deterministic test model (the default until an owner
//!   adds a key). It exercises the real tool loop and proposal flow with no
//!   network and no data leaving the computer.
//! - `anthropic`: Messages API (`POST /v1/messages`), manual tool loop,
//!   adaptive thinking on current models, server-side refusal fallbacks.
//! - `openai_compatible`: `POST {base}/chat/completions` with function tools,
//!   for stores that run another provider or a local model. Messages are kept
//!   in one neutral block format and translated here.

use std::sync::Arc;
use std::time::Duration;

use amwapos_core::ai::AiTurn;
use amwapos_core::{AppCore, AppError, AppResult, ErrorCode};
use serde_json::{json, Value};

const MAX_ROUNDS: usize = 8;
const ANTHROPIC_URL: &str = "https://api.anthropic.com";

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::internal(format!("worker failed: {e}")))?
}

struct Reply {
    content: Value,
    stop_reason: String,
    input_tokens: i64,
    output_tokens: i64,
}

fn provider_error(status: u16, body: &Value) -> AppError {
    let msg = body
        .pointer("/error/message")
        .and_then(|m| m.as_str())
        .unwrap_or("The AI provider returned an error.")
        .chars()
        .take(300)
        .collect::<String>();
    let (code, kind) = match status {
        401 | 403 => (ErrorCode::Conflict, "ai_auth"),
        429 => (ErrorCode::Conflict, "ai_rate_limited"),
        400 | 404 | 413 | 422 => (ErrorCode::Validation, "ai_request"),
        _ => (ErrorCode::Conflict, "ai_unavailable"),
    };
    AppError::new(code, format!("AI provider error ({status}): {msg}")).with_details(json!({ "kind": kind, "status": status }))
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
    let base = if st.base_url.is_empty() { ANTHROPIC_URL } else { st.base_url.as_str() };
    let mut body = json!({
        "model": st.model,
        "max_tokens": st.max_tokens,
        "system": turn.system,
        "tools": turn.tools,
        "messages": turn.messages,
        "cache_control": { "type": "ephemeral" },
    });
    if supports_adaptive_thinking(&st.model) {
        body["thinking"] = json!({ "type": "adaptive" });
    }
    let mut req = http
        .post(format!("{base}/v1/messages"))
        .header("x-api-key", &turn.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");
    if st.fallbacks && supports_default_fallbacks(&st.model) {
        body["fallbacks"] = json!("default");
        req = req.header("anthropic-beta", "server-side-fallback-2026-07-01");
    }
    let resp = req.json(&body).send().await.map_err(|e| {
        AppError::new(ErrorCode::Conflict, format!("Could not reach the AI provider: {e}"))
            .with_details(json!({ "kind": "ai_unreachable" }))
    })?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if status != 200 {
        return Err(provider_error(status, &v));
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
        .find_map(|m| m["content"].as_array()?.iter().find(|b| b["type"] == "text")?.get("text")?.as_str().map(str::to_lowercase))
        .unwrap_or_default();
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
    if let Some((name, _)) = &price_intent {
        return call("search_products", json!({ "query": name, "limit": 1 }));
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
    let body = json!({ "model": st.model, "max_tokens": st.max_tokens, "messages": to_openai_messages(&turn.system, &turn.messages), "tools": tools });
    let resp = http.post(format!("{}/chat/completions", st.base_url)).bearer_auth(&turn.api_key).json(&body).send().await.map_err(|e| {
        AppError::new(ErrorCode::Conflict, format!("Could not reach the AI provider: {e}"))
            .with_details(json!({ "kind": "ai_unreachable" }))
    })?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if status != 200 {
        return Err(provider_error(status, &v));
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
        _ => "end_turn",
    };
    Ok(Reply {
        content: json!(content),
        stop_reason: stop.to_string(),
        input_tokens: v.pointer("/usage/prompt_tokens").and_then(|x| x.as_i64()).unwrap_or(0),
        output_tokens: v.pointer("/usage/completion_tokens").and_then(|x| x.as_i64()).unwrap_or(0),
    })
}

/// One request without tools (extraction). Returns the text of the reply.
pub async fn complete_once(turn: &AiTurn) -> AppResult<String> {
    let http = reqwest::Client::builder().timeout(Duration::from_secs(180)).build().map_err(|e| AppError::internal(e.to_string()))?;
    let reply = match turn.settings.provider.as_str() {
        "openai_compatible" => openai_compatible(&http, turn).await?,
        "anthropic" => anthropic(&http, turn).await?,
        _ => return Err(AppError::conflict("No AI provider is configured.")),
    };
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
/// limit), storing every message. Returns the conversation for display.
pub async fn ask(core: Arc<AppCore>, token: String, conversation_id: Option<String>, text: String) -> AppResult<Value> {
    let mut turn = {
        let (c, t) = (core.clone(), token.clone());
        blocking(move || c.ai_begin(&t, conversation_id, &text)).await?
    };
    let cid = turn.conversation_id.clone();
    let http = reqwest::Client::builder().timeout(Duration::from_secs(600)).build().map_err(|e| AppError::internal(e.to_string()))?;
    let (mut input, mut output, mut rounds) = (0i64, 0i64, 0i64);
    let mut outcome = "completed";
    for _ in 0..MAX_ROUNDS {
        rounds += 1;
        let reply = match turn.settings.provider.as_str() {
            "fake" => Ok(fake(&turn)),
            "openai_compatible" => openai_compatible(&http, &turn).await,
            _ => anthropic(&http, &turn).await,
        };
        let reply = match reply {
            Ok(r) => r,
            Err(e) => {
                let (c, t, id) = (core.clone(), token.clone(), cid.clone());
                let _ = blocking(move || c.ai_audit_request(&t, &id, rounds, input, output, "error")).await;
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
                let calls: Vec<Value> =
                    reply.content.as_array().cloned().unwrap_or_default().into_iter().filter(|b| b["type"] == "tool_use").collect();
                let (c, t, id) = (core.clone(), token.clone(), cid.clone());
                // All results go back in one user message.
                let results = blocking(move || {
                    let mut out = vec![];
                    for call in &calls {
                        let (v, is_error) = c.ai_tool(&t, &id, call["name"].as_str().unwrap_or_default(), &call["input"]);
                        out.push(
                            json!({ "type": "tool_result", "tool_use_id": call["id"], "content": v.to_string(), "is_error": is_error }),
                        );
                    }
                    c.ai_store_tool_results(&id, &json!(out))?;
                    Ok(())
                })
                .await;
                results?;
            }
            "pause_turn" => {}
            "refusal" => {
                outcome = "refused";
                break;
            }
            "max_tokens" => {
                outcome = "truncated";
                break;
            }
            _ => break,
        }
        let (c, t, id) = (core.clone(), token.clone(), cid.clone());
        turn = blocking(move || c.ai_continue(&t, &id)).await?;
        if rounds as usize == MAX_ROUNDS {
            outcome = "round_limit";
        }
    }
    let (c, t, id) = (core.clone(), token.clone(), cid.clone());
    blocking(move || {
        c.ai_audit_request(&t, &id, rounds, input, output, outcome)?;
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
