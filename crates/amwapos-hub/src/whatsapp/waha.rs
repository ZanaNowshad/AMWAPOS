//! WAHA gateway adapter (docs/WHATSAPP_AUTOPILOT.md, "Connection").
//!
//! WAHA (WhatsApp HTTP API, devlikeapro/waha, Apache-2.0) runs a WhatsApp Web
//! session in its own container and exposes it over REST. AMWAPOS talks to it
//! through the same `WhatsAppAdapter` contract as the built-in client, so the
//! outbox, the inbox, the order pipeline and the autopilot do not change.
//!
//! * Inbound messages are pulled, not pushed: every few seconds the recently
//!   active chats (`GET /api/{session}/chats/overview`) and their new
//!   messages (`GET /api/{session}/chats/{chat}/messages?filter.timestamp.gte=…`)
//!   are read and committed through the sink; a message is acknowledged
//!   (the cursor moves) only after AMWAPOS stored it. No port is opened on
//!   the AMWAPOS computer and a gateway on another machine works the same.
//!   The cursor is kept next to the session file, so messages that arrived
//!   while AMWAPOS was closed are fetched on start (up to six hours back).
//!   Overlapping reads are harmless: the inbox de-duplicates by message id.
//! * Sends: `POST /api/sendText` / `/api/sendFile`, at most one every
//!   `MIN_GAP` plus jitter. A retry of an AMWAPOS message first looks in the
//!   chat for the same text sent since the first attempt, so a send whose
//!   answer was lost is not sent twice.
//! * The API key travels only to the configured WAHA address (header
//!   `X-Api-Key`); media URLs on any other host are refused.
//! * Chat ids `123@c.us` are stored as `123@s.whatsapp.net`, as the built-in
//!   client does, so a customer is the same chat whichever link is used.
//! * The WhatsApp Business catalogue is not available through WAHA here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use amwapos_core::messaging::Inbound;
use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Value};
use tokio::sync::{oneshot, Notify};

use super::adapter::*;

const POLL: Duration = Duration::from_secs(3);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Shortest time between two sends (WhatsApp limits bursts; see the docs).
const MIN_GAP: Duration = Duration::from_millis(1500);
/// Messages older than this are not fetched on start.
const MAX_CATCH_UP_S: i64 = 6 * 3600;
/// Re-read this far back each pass (clock skew, late-arriving messages).
const OVERLAP_S: i64 = 60;
const MAX_MEDIA: usize = 20 * 1024 * 1024;

pub const API_KEY_SLOT: &str = amwapos_core::settings::WAHA_API_KEY_SLOT;

#[derive(Debug, Clone)]
pub struct WahaConfig {
    pub base_url: String,
    pub session: String,
    pub api_key: Option<String>,
}

pub struct WahaAdapter {
    cfg: WahaConfig,
}

impl WahaAdapter {
    pub fn new(cfg: WahaConfig) -> Self {
        Self { cfg }
    }
}

struct Api {
    base: String,
    session: String,
    key: Option<String>,
    http: reqwest::Client,
}

fn err_for(status: u16, body: &str, retry_after: Option<u64>) -> AdapterError {
    let detail: String = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().or(v["error"].as_str()).map(String::from))
        .unwrap_or_else(|| body.chars().take(200).collect());
    match status {
        401 | 403 => AdapterError::temporary("WAHA refused the API key. Check it in Settings → WhatsApp."),
        404 => AdapterError::not_found(format!("WAHA: not found ({detail})")),
        429 => AdapterError { retry_after_s: retry_after.or(Some(30)), ..AdapterError::temporary("WAHA asked to slow down (HTTP 429).") },
        400 | 422 => AdapterError::permanent(format!("WAHA refused the request: {detail}")),
        _ => AdapterError::temporary(format!("WAHA answered HTTP {status}: {detail}")),
    }
}

impl Api {
    fn new(cfg: &WahaConfig) -> Result<Self, AdapterError> {
        let base = cfg.base_url.trim().trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(AdapterError::permanent("Enter the WAHA address in Settings → WhatsApp (for example http://localhost:3000)."));
        }
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| AdapterError::permanent(e.to_string()))?;
        Ok(Self { base, session: cfg.session.clone(), key: cfg.api_key.clone().filter(|k| !k.is_empty()), http })
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<&Value>) -> Result<(u16, String), AdapterError> {
        let mut req = self.http.request(method, format!("{}{}", self.base, path));
        if let Some(k) = &self.key {
            req = req.header("X-Api-Key", k);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let r = req.send().await.map_err(|e| {
            AdapterError::temporary(if e.is_timeout() {
                "WAHA did not answer in time.".to_string()
            } else {
                format!("Cannot reach WAHA at {}: {e}", self.base)
            })
        })?;
        let status = r.status().as_u16();
        let text = r.text().await.unwrap_or_default();
        Ok((status, text))
    }

    async fn json(&self, method: reqwest::Method, path: &str, body: Option<&Value>) -> Result<Value, AdapterError> {
        let (status, text) = self.call(method, path, body).await?;
        if (200..300).contains(&status) {
            Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
        } else {
            Err(err_for(status, &text, None))
        }
    }

    async fn get(&self, path: &str) -> Result<Value, AdapterError> {
        self.json(reqwest::Method::GET, path, None).await
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, AdapterError> {
        self.json(reqwest::Method::POST, path, Some(body)).await
    }

    /// Bytes of a media URL, only from the configured WAHA address.
    async fn download(&self, url: &str) -> Result<Vec<u8>, AdapterError> {
        if !url.starts_with(&format!("{}/", self.base)) {
            return Err(AdapterError::permanent("The media address is not on the WAHA server; it was not fetched."));
        }
        let mut req = self.http.get(url);
        if let Some(k) = &self.key {
            req = req.header("X-Api-Key", k);
        }
        let r = req.send().await.map_err(|e| AdapterError::temporary(format!("Media download failed: {e}")))?;
        let status = r.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(err_for(status, "", None));
        }
        let b = r.bytes().await.map_err(|e| AdapterError::temporary(format!("Media download failed: {e}")))?;
        if b.len() > MAX_MEDIA {
            return Err(AdapterError::permanent("The attachment is larger than 20 MB."));
        }
        Ok(b.to_vec())
    }
}

/// `123@c.us` → `123@s.whatsapp.net` (the built-in client's form).
pub fn normalize_chat(id: &str) -> String {
    match id.strip_suffix("@c.us") {
        Some(user) => format!("{user}@s.whatsapp.net"),
        None => id.to_string(),
    }
}

/// A phone number (with or without +) as WAHA's chat id.
pub fn chat_for_phone(phone: &str) -> Result<String, AdapterError> {
    let d: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
    if !(8..=15).contains(&d.len()) {
        return Err(AdapterError::permanent(format!("'{phone}' is not a WhatsApp number.")));
    }
    Ok(format!("{d}@c.us"))
}

/// Message and chat ids are strings on some engines, objects on others.
fn id_str(v: &Value) -> Option<String> {
    v.as_str().map(String::from).or_else(|| v["_serialized"].as_str().map(String::from)).or_else(|| v["id"].as_str().map(String::from))
}

fn skip_chat(chat: &str) -> bool {
    chat.ends_with("@g.us") || chat.starts_with("status@") || chat.ends_with("@newsletter") || chat.ends_with("@broadcast")
}

/// A WAHA message as an AMWAPOS inbound message (`None`: not for the inbox).
pub fn to_inbound(chat: &str, m: &Value) -> Option<Inbound> {
    if m["fromMe"].as_bool().unwrap_or(false) {
        return None;
    }
    let id = id_str(&m["id"])?;
    let ts = m["timestamp"].as_i64()?;
    let body = m["body"].as_str().unwrap_or_default().to_string();
    let has_media = m["hasMedia"].as_bool().unwrap_or(false);
    let mime = m["media"]["mimetype"].as_str().map(String::from);
    let kind = if !has_media {
        "text"
    } else {
        match mime.as_deref() {
            Some(x) if x.starts_with("image/") => "image",
            Some("application/pdf") => "document",
            Some(_) if m["media"]["filename"].is_string() => "document",
            _ => "other",
        }
    };
    if kind == "text" && body.trim().is_empty() {
        return None;
    }
    Some(Inbound {
        wa_id: id,
        chat: normalize_chat(chat),
        sender_pn: None,
        push_name: m["_data"]["notifyName"].as_str().or(m["_data"]["pushName"].as_str()).map(String::from),
        ts,
        kind: kind.into(),
        text: (!has_media).then_some(body.clone()),
        caption: (has_media && !body.trim().is_empty()).then_some(body),
        media_mime: mime,
        media_ref: m["media"]["url"].as_str().map(String::from),
    })
}

struct State {
    connected: AtomicBool,
    stopped: AtomicBool,
    stop: Notify,
    next_send: tokio::sync::Mutex<Instant>,
    /// AMWAPOS message id → unix time of its first attempt (duplicate guard).
    attempts: Mutex<HashMap<String, i64>>,
}

pub struct WahaSession {
    api: Arc<Api>,
    st: Arc<State>,
}

fn session_path(api: &Api) -> String {
    format!("/api/sessions/{}", api.session)
}

/// Create or start the WAHA session if it is not running yet.
async fn ensure_started(api: &Api) -> Result<(), AdapterError> {
    match api.get(&session_path(api)).await {
        Ok(v) => {
            if matches!(v["status"].as_str(), Some("STOPPED" | "FAILED")) {
                api.post(&format!("{}/start", session_path(api)), &json!({})).await?;
            }
            Ok(())
        }
        Err(e) if e.not_found => api.post("/api/sessions", &json!({ "name": api.session, "start": true })).await.map(|_| ()),
        Err(e) => Err(e),
    }
}

fn read_cursor(path: &PathBuf) -> i64 {
    let floor = chrono::Utc::now().timestamp() - MAX_CATCH_UP_S;
    std::fs::read_to_string(path).ok().and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(floor).max(floor)
}

/// One inbound pass: new messages of recently active chats, committed through
/// the sink before the cursor moves.
async fn pull(api: &Api, sink: &Arc<dyn AdapterSink>, cursor: &mut i64, cursor_file: &PathBuf) -> Result<usize, AdapterError> {
    let since = *cursor - OVERLAP_S;
    let chats = match api.get(&format!("/api/{}/chats/overview?limit=50", api.session)).await {
        Ok(v) => v,
        // Engines without the overview: the plain chat list, newest first.
        Err(e) if e.not_found => {
            api.get(&format!("/api/{}/chats?limit=50&sortBy=conversationTimestamp&sortOrder=desc", api.session)).await?
        }
        Err(e) => return Err(e),
    };
    let mut batch = vec![];
    for c in chats.as_array().cloned().unwrap_or_default() {
        let Some(chat) = id_str(&c["id"]) else { continue };
        if skip_chat(&chat) {
            continue;
        }
        let last = c["lastMessage"]["timestamp"].as_i64().or(c["timestamp"].as_i64()).or(c["conversationTimestamp"].as_i64());
        if last.is_some_and(|t| t < since) {
            continue;
        }
        let msgs = api
            .get(&format!(
                "/api/{}/chats/{}/messages?limit=50&downloadMedia=true&filter.fromMe=false&filter.timestamp.gte={since}",
                api.session, chat
            ))
            .await?;
        for m in msgs.as_array().cloned().unwrap_or_default() {
            if let Some(i) = to_inbound(&chat, &m) {
                batch.push(i);
            }
        }
    }
    if batch.is_empty() {
        return Ok(0);
    }
    batch.sort_by_key(|m| m.ts);
    let newest = batch.iter().map(|m| m.ts).max().unwrap_or(*cursor);
    let n = batch.len();
    sink.inbound(batch).await.map_err(AdapterError::temporary)?;
    if newest > *cursor {
        *cursor = newest;
        let _ = std::fs::write(cursor_file, cursor.to_string());
    }
    Ok(n)
}

async fn run(api: Arc<Api>, st: Arc<State>, sink: Arc<dyn AdapterSink>, cursor_file: PathBuf, done: oneshot::Sender<Exit>) {
    let mut cursor = read_cursor(&cursor_file);
    let mut last_status = String::new();
    let mut last_qr: Option<String> = None;
    let mut last_restart = Instant::now() - Duration::from_secs(3600);
    loop {
        if st.stopped.load(Ordering::SeqCst) {
            break;
        }
        match api.get(&session_path(&api)).await {
            Ok(v) => {
                let status = v["status"].as_str().unwrap_or("UNKNOWN").to_string();
                let working = status == "WORKING";
                st.connected.store(working, Ordering::SeqCst);
                if status != last_status {
                    match status.as_str() {
                        "WORKING" => {
                            last_qr = None;
                            sink.event(AdapterEvent::Connected { account: v["me"]["id"].as_str().map(normalize_chat) });
                        }
                        "SCAN_QR_CODE" => {}
                        "STOPPED" | "FAILED" => {
                            sink.event(AdapterEvent::Disconnected(format!("The WAHA session is {}.", status.to_lowercase())))
                        }
                        other => sink.event(AdapterEvent::Disconnected(format!("The WAHA session is {}.", other.to_lowercase()))),
                    }
                    last_status = status.clone();
                }
                match status.as_str() {
                    "SCAN_QR_CODE" => {
                        if let Ok(q) = api.get(&format!("/api/{}/auth/qr?format=raw", api.session)).await {
                            if let Some(code) = q["value"].as_str().filter(|c| Some(c.to_string()) != last_qr) {
                                last_qr = Some(code.to_string());
                                sink.event(AdapterEvent::Qr { code: code.to_string(), valid_for: Duration::from_secs(20) });
                            }
                        }
                    }
                    "WORKING" => {
                        if let Err(e) = pull(&api, &sink, &mut cursor, &cursor_file).await {
                            tracing::debug!(error = %e.message, "WAHA inbound pass");
                        }
                    }
                    "STOPPED" | "FAILED" if last_restart.elapsed() > Duration::from_secs(60) => {
                        last_restart = Instant::now();
                        let _ = api.post(&format!("{}/start", session_path(&api)), &json!({})).await;
                    }
                    _ => {}
                }
            }
            Err(e) => {
                st.connected.store(false, Ordering::SeqCst);
                if last_status != "UNREACHABLE" {
                    sink.event(AdapterEvent::Disconnected(e.message.clone()));
                    last_status = "UNREACHABLE".into();
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL) => {}
            _ = st.stop.notified() => {}
        }
    }
    st.connected.store(false, Ordering::SeqCst);
    let _ = done.send(Exit::Ended);
}

#[async_trait]
impl WhatsAppAdapter for WahaAdapter {
    fn name(&self) -> &'static str {
        "waha"
    }

    async fn start(&self, opts: StartOptions, sink: Arc<dyn AdapterSink>) -> Result<(Arc<dyn AdapterSession>, ExitFuture), AdapterError> {
        let api = Arc::new(Api::new(&self.cfg)?);
        ensure_started(&api).await?;
        if let Some(phone) = &opts.pair_phone {
            let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
            match api.post(&format!("/api/{}/auth/request-code", api.session), &json!({ "phoneNumber": digits })).await {
                Ok(v) => match v["code"].as_str() {
                    Some(c) => sink.event(AdapterEvent::PairCode { code: c.to_string(), valid_for: Duration::from_secs(160) }),
                    None => sink.event(AdapterEvent::PairCodeError("WAHA returned no code.".into())),
                },
                Err(e) => sink.event(AdapterEvent::PairCodeError(e.message)),
            }
        }
        let st = Arc::new(State {
            connected: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            stop: Notify::new(),
            next_send: tokio::sync::Mutex::new(Instant::now()),
            attempts: Mutex::new(HashMap::new()),
        });
        let (tx, rx) = oneshot::channel();
        let cursor_file = opts.session_db.with_extension("waha-cursor");
        tokio::spawn(run(api.clone(), st.clone(), sink, cursor_file, tx));
        let exit: ExitFuture = Box::pin(async move { rx.await.unwrap_or_else(|_| Exit::Failed("WAHA poller ended".into())) });
        Ok((Arc::new(WahaSession { api, st }), exit))
    }
}

impl WahaSession {
    /// Wait for the send gap (plus a little jitter), then reserve the next slot.
    async fn pace(&self) {
        let mut next = self.st.next_send.lock().await;
        let now = Instant::now();
        if *next > now {
            tokio::time::sleep(*next - now).await;
        }
        let jitter = Duration::from_millis(rand::random::<u64>() % 700);
        *next = Instant::now() + MIN_GAP + jitter;
    }

    /// The id of `text` already sent to `chat` since `since` (a retry whose
    /// first attempt reached WhatsApp although its answer was lost).
    async fn already_sent(&self, chat: &str, text: &str, since: i64) -> Option<String> {
        let path = format!(
            "/api/{}/chats/{}/messages?limit=20&downloadMedia=false&filter.fromMe=true&filter.timestamp.gte={}",
            self.api.session,
            chat,
            since - 5
        );
        let msgs = self.api.get(&path).await.ok()?;
        msgs.as_array()?.iter().find(|m| m["body"].as_str() == Some(text)).and_then(|m| id_str(&m["id"]))
    }

    fn first_attempt(&self, send_id: &str) -> Option<i64> {
        let mut a = self.st.attempts.lock().unwrap();
        let prev = a.get(send_id).copied();
        if prev.is_none() {
            if a.len() > 5000 {
                a.clear();
            }
            a.insert(send_id.to_string(), chrono::Utc::now().timestamp());
        }
        prev
    }

    fn done(&self, send_id: &str) {
        self.st.attempts.lock().unwrap().remove(send_id);
    }
}

#[async_trait]
impl AdapterSession for WahaSession {
    fn connected(&self) -> bool {
        self.st.connected.load(Ordering::SeqCst)
    }

    fn logged_in(&self) -> bool {
        self.connected()
    }

    async fn send_text(&self, send_id: &str, to_phone: &str, text: &str) -> Result<String, AdapterError> {
        let chat = chat_for_phone(to_phone)?;
        if let Some(t0) = self.first_attempt(send_id) {
            if let Some(id) = self.already_sent(&chat, text, t0).await {
                self.done(send_id);
                return Ok(id);
            }
        }
        self.pace().await;
        let v = self
            .api
            .post("/api/sendText", &json!({ "session": self.api.session, "chatId": chat, "text": text, "linkPreview": false }))
            .await?;
        self.done(send_id);
        Ok(id_str(&v["id"]).unwrap_or_else(|| send_id.to_string()))
    }

    async fn send_document(
        &self,
        send_id: &str,
        to_phone: &str,
        bytes: Vec<u8>,
        file_name: &str,
        mime: &str,
        caption: Option<&str>,
    ) -> Result<String, AdapterError> {
        let chat = chat_for_phone(to_phone)?;
        self.pace().await;
        let body = json!({
            "session": self.api.session, "chatId": chat, "caption": caption,
            "file": { "mimetype": mime, "filename": file_name, "data": base64::engine::general_purpose::STANDARD.encode(&bytes) }
        });
        let (status, text) = self.api.call(reqwest::Method::POST, "/api/sendFile", Some(&body)).await?;
        if (200..300).contains(&status) {
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            return Ok(id_str(&v["id"]).unwrap_or_else(|| send_id.to_string()));
        }
        // WAHA Core cannot send files (WAHA Plus can): the text still goes.
        if status == 422 || status == 501 || text.contains("Plus") {
            tracing::info!("WAHA cannot send files; sending the message text without the attachment");
            let t = caption.filter(|c| !c.trim().is_empty()).unwrap_or(file_name);
            return self.send_text(send_id, to_phone, t).await;
        }
        Err(err_for(status, &text, None))
    }

    async fn mark_read(&self, chat: &str, _ids: &[String]) -> Result<(), AdapterError> {
        let id = match chat.strip_suffix("@s.whatsapp.net") {
            Some(u) => format!("{u}@c.us"),
            None => chat.to_string(),
        };
        self.api.post("/api/sendSeen", &json!({ "session": self.api.session, "chatId": id })).await.map(|_| ())
    }

    async fn download(&self, media_ref: &str) -> Result<Vec<u8>, AdapterError> {
        self.api.download(media_ref).await
    }

    async fn stop(&self) {
        self.st.stopped.store(true, Ordering::SeqCst);
        self.st.stop.notify_one();
    }

    async fn logout(&self) {
        let _ = self.api.post(&format!("{}/logout", session_path(&self.api)), &json!({})).await;
        self.stop().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chats_and_numbers_map_between_waha_and_amwapos() {
        assert_eq!(normalize_chat("97333112233@c.us"), "97333112233@s.whatsapp.net");
        assert_eq!(normalize_chat("123@g.us"), "123@g.us");
        assert_eq!(chat_for_phone("+973 3311 2233").unwrap(), "97333112233@c.us");
        assert!(chat_for_phone("12").unwrap_err().permanent);
        assert!(skip_chat("123@g.us") && skip_chat("status@broadcast") && !skip_chat("973@c.us"));
    }

    #[test]
    fn waha_messages_become_inbox_messages() {
        let m = json!({ "id": "false_973@c.us_ABC", "timestamp": 1700000000, "from": "973@c.us", "fromMe": false, "body": "2 milk", "hasMedia": false });
        let i = to_inbound("97333112233@c.us", &m).unwrap();
        assert_eq!((i.wa_id.as_str(), i.chat.as_str(), i.kind.as_str()), ("false_973@c.us_ABC", "97333112233@s.whatsapp.net", "text"));
        assert_eq!(i.text.as_deref(), Some("2 milk"));
        let img = json!({ "id": { "_serialized": "X1" }, "timestamp": 1, "fromMe": false, "body": "paid", "hasMedia": true,
                          "media": { "url": "http://waha:3000/api/files/x.jpeg", "mimetype": "image/jpeg" } });
        let i = to_inbound("97333112233@c.us", &img).unwrap();
        assert_eq!(
            (i.kind.as_str(), i.caption.as_deref(), i.media_ref.as_deref()),
            ("image", Some("paid"), Some("http://waha:3000/api/files/x.jpeg"))
        );
        assert!(to_inbound("x@c.us", &json!({ "id": "Y", "timestamp": 1, "fromMe": true, "body": "hi" })).is_none(), "our own messages");
        assert!(to_inbound("x@c.us", &json!({ "id": "Z", "timestamp": 1, "fromMe": false, "body": " " })).is_none(), "empty");
    }

    #[test]
    fn http_errors_say_whether_to_retry() {
        assert!(!err_for(401, "", None).permanent);
        assert!(err_for(422, r#"{"message":"bad chat"}"#, None).permanent);
        assert_eq!(err_for(429, "", Some(12)).retry_after_s, Some(12));
        assert!(err_for(404, "", None).not_found);
        assert!(!err_for(502, "", None).permanent);
    }
}
