//! Hub HTTP API for terminals on the store LAN.
//!
//! Protocol 2: every body except /health and /info is encrypted (see
//! `amwapos_core::channel`). Routes (no route exposes SQL or files):
//!   GET  /health            liveness (public)
//!   GET  /info              hub identity and versions (public, no secrets)
//!   POST /pair/start        SPAKE2 message exchange (rate limited)
//!   POST /pair/finish       sealed pairing request → sealed device key + snapshot
//!   POST /heartbeat         signed + sealed
//!   POST /sync/push         signed + sealed
//!   POST /sync/pull         signed + sealed
//!   GET  /sync/status       signed, sealed response
//!   GET  /devices           signed, sealed response (names and codes only)
//!   POST /pair              protocol 1 (plaintext): refused with 426
//!   GET  /companion/...     owner phone view (flag pwa.companion): static
//!                           page + GET /companion/api/snapshot with a
//!                           short-lived bearer token; LAN addresses only

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use amwapos_core::channel::{self, PairKeys, PROTOCOL_HEADER};
use amwapos_core::sync::{self, Heartbeat, NonceCache, PairRequest, PullRequest, PushRequest, PROTOCOL_VERSION};
use amwapos_core::{AppCore, AppError, AppResult, ErrorCode};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};

pub struct HubState {
    pub core: Arc<AppCore>,
    pub nonces: NonceCache,
    pair_attempts: Mutex<HashMap<IpAddr, (u32, i64)>>,
    /// SPAKE2 exchanges waiting for /pair/finish: pairing id → (keys, code hash, started).
    pending_pairs: Mutex<HashMap<String, (PairKeys, String, i64)>>,
    /// Wrong-code attempts per pairing code hash; the code is burned at 5.
    pair_failures: Mutex<HashMap<String, u32>>,
}

const PENDING_PAIR_TTL_SECS: i64 = 120;
const MAX_PENDING_PAIRS: usize = 32;
const MAX_CODE_FAILURES: u32 = 5;

fn pair_aad(pairing_id: &str, dir: &str) -> Vec<u8> {
    format!("amwapos/2\npair\n{dir}\n{pairing_id}").into_bytes()
}

/// `Some(426 response)` unless the request speaks the current protocol.
fn protocol_rejection(h: &HeaderMap) -> Option<Response> {
    (header(h, PROTOCOL_HEADER) != PROTOCOL_VERSION.to_string()).then(outdated)
}

fn outdated() -> Response {
    let e = AppError::conflict(format!(
        "This hub requires AMWAPOS sync protocol {PROTOCOL_VERSION} (encrypted). Install the same AMWAPOS version on the hub and this terminal."
    ));
    (StatusCode::UPGRADE_REQUIRED, [("content-type", "application/json")], serde_json::to_vec(&e).unwrap_or_default()).into_response()
}

pub fn status_for(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::Unauthenticated | ErrorCode::InvalidCredentials => StatusCode::UNAUTHORIZED,
        ErrorCode::Forbidden => StatusCode::FORBIDDEN,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::Validation | ErrorCode::IdempotencyMismatch => StatusCode::BAD_REQUEST,
        ErrorCode::Conflict | ErrorCode::Duplicate => StatusCode::CONFLICT,
        ErrorCode::DatabaseBusy | ErrorCode::OperationInProgress => StatusCode::SERVICE_UNAVAILABLE,
        ErrorCode::AccountLocked => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn err_response(e: AppError) -> Response {
    let status = status_for(e.code);
    let body = serde_json::to_vec(&e).unwrap_or_default();
    (status, [("content-type", "application/json")], body).into_response()
}

fn json_response<T: Serialize>(v: &T) -> Response {
    match serde_json::to_vec(v) {
        Ok(b) => (StatusCode::OK, [("content-type", "application/json")], b).into_response(),
        Err(e) => err_response(AppError::internal(e.to_string())),
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::internal(format!("worker failed: {e}")))?
}

fn header<'a>(h: &'a HeaderMap, k: &str) -> &'a str {
    h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("")
}

async fn health() -> Response {
    json_response(&serde_json::json!({ "ok": true, "product": "AMWAPOS", "version": amwapos_core::audit::APP_VERSION }))
}

async fn info(State(st): State<Arc<HubState>>) -> Response {
    let core = st.core.clone();
    match blocking(move || core.hub_info()).await {
        Ok(i) => json_response(&i),
        Err(e) => err_response(e),
    }
}

fn rate_limited(st: &HubState, ip: IpAddr) -> Option<Response> {
    // Brute-force protection: 5 attempts per IP per 5 minutes.
    let now = chrono::Utc::now().timestamp();
    let mut m = match st.pair_attempts.lock() {
        Ok(m) => m,
        Err(_) => return Some(err_response(AppError::internal("state poisoned"))),
    };
    let e = m.entry(ip).or_insert((0, now));
    if now - e.1 > 300 {
        *e = (0, now);
    }
    e.0 += 1;
    if e.0 > 5 {
        return Some(err_response(AppError::new(ErrorCode::AccountLocked, "Too many pairing attempts. Wait five minutes and try again.")));
    }
    None
}

#[derive(Deserialize)]
struct PairStart {
    pairing_id: String,
    message: String,
}

#[derive(Serialize, Deserialize)]
pub struct PairStartReply {
    pub message: String,
}

async fn pair_v1() -> Response {
    outdated()
}

async fn pair_start(
    State(st): State<Arc<HubState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(r) = protocol_rejection(&headers) {
        return r;
    }
    if let Some(r) = rate_limited(&st, addr.ip()) {
        return r;
    }
    let req: PairStart = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return err_response(AppError::validation(format!("Invalid pairing request: {e}"))),
    };
    if req.pairing_id.len() != 26 || !req.pairing_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return err_response(AppError::validation("Invalid pairing id."));
    }
    let Ok(terminal_msg) = hex::decode(&req.message) else {
        return err_response(AppError::validation("Invalid pairing message."));
    };
    let core = st.core.clone();
    let code_hash = match blocking(move || core.hub_active_pairing_code()).await {
        Ok(h) => h,
        Err(e) => return err_response(e),
    };
    let (hub, hub_msg) = channel::HubPairing::start(&code_hash);
    let keys = match hub.finish(&terminal_msg, &req.pairing_id) {
        Ok(k) => k,
        Err(e) => return err_response(e),
    };
    let now = chrono::Utc::now().timestamp();
    match st.pending_pairs.lock() {
        Ok(mut m) => {
            m.retain(|_, v| now - v.2 <= PENDING_PAIR_TTL_SECS);
            if m.len() >= MAX_PENDING_PAIRS {
                return err_response(AppError::new(ErrorCode::AccountLocked, "Too many pairings in progress. Try again in two minutes."));
            }
            // Never replace a pending exchange: the id is visible on the wire, and an
            // overwrite would make the real terminal's finish look like a wrong code.
            if m.contains_key(&req.pairing_id) {
                return err_response(AppError::conflict("This pairing session already exists. Start pairing again."));
            }
            m.insert(req.pairing_id, (keys, code_hash, now));
        }
        Err(_) => return err_response(AppError::internal("state poisoned")),
    }
    json_response(&PairStartReply { message: hex::encode(hub_msg) })
}

async fn pair_finish(
    State(st): State<Arc<HubState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(r) = protocol_rejection(&headers) {
        return r;
    }
    let pairing_id = header(&headers, "x-amw-pairing").to_string();
    let pending = match st.pending_pairs.lock() {
        Ok(mut m) => m.remove(&pairing_id),
        Err(_) => return err_response(AppError::internal("state poisoned")),
    };
    let Some((keys, code_hash, started)) = pending else {
        return err_response(AppError::new(ErrorCode::InvalidCredentials, "Pairing session expired. Start pairing again."));
    };
    if chrono::Utc::now().timestamp() - started > PENDING_PAIR_TTL_SECS {
        return err_response(AppError::new(ErrorCode::InvalidCredentials, "Pairing session expired. Start pairing again."));
    }
    // Only a terminal that derived the same SPAKE2 key (knew the code) can seal this.
    let plain = match channel::open(&keys.request, &pair_aad(&pairing_id, "request"), &body) {
        Ok(p) => p,
        Err(_) => {
            let failures = st
                .pair_failures
                .lock()
                .map(|mut m| {
                    let n = m.entry(code_hash.clone()).or_insert(0);
                    *n += 1;
                    *n
                })
                .unwrap_or(MAX_CODE_FAILURES);
            if failures >= MAX_CODE_FAILURES {
                let core = st.core.clone();
                let h = code_hash.clone();
                let _ = blocking(move || core.hub_burn_pairing_code(&h)).await;
                tracing::warn!(peer = %addr, "pairing code burned after repeated wrong attempts");
                return err_response(AppError::new(
                    ErrorCode::InvalidCredentials,
                    "Too many wrong pairing codes. This code is no longer valid; generate a new one on the hub.",
                ));
            }
            tracing::warn!(peer = %addr, "pairing refused: wrong code");
            return err_response(AppError::new(ErrorCode::InvalidCredentials, "Invalid pairing code."));
        }
    };
    let req: PairRequest = match serde_json::from_slice(&plain) {
        Ok(r) => r,
        Err(e) => return err_response(AppError::validation(format!("Invalid pairing request: {e}"))),
    };
    let core = st.core.clone();
    let h = code_hash.clone();
    match blocking(move || core.hub_pair_verified(req, &h)).await {
        Ok(r) => {
            if let Ok(mut m) = st.pair_attempts.lock() {
                m.remove(&addr.ip());
            }
            if let Ok(mut m) = st.pair_failures.lock() {
                m.remove(&code_hash);
            }
            tracing::info!(peer = %addr, device = %r.device.device_code, "terminal paired");
            match serde_json::to_vec(&r) {
                Ok(b) => {
                    let sealed = channel::seal(&keys.response, &pair_aad(&pairing_id, "response"), &b);
                    (StatusCode::OK, [("content-type", "application/octet-stream")], sealed).into_response()
                }
                Err(e) => err_response(AppError::internal(e.to_string())),
            }
        }
        Err(e) => {
            tracing::warn!(peer = %addr, error = %e.message, "pairing refused");
            err_response(e)
        }
    }
}

/// Verify the request signature, run `f`, and sign the response body.
async fn signed<F, T>(st: Arc<HubState>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes, f: F) -> Response
where
    F: FnOnce(Arc<AppCore>, String, Bytes) -> AppResult<T> + Send + 'static,
    T: Serialize + Send + 'static,
{
    if let Some(r) = protocol_rejection(&headers) {
        return r;
    }
    let device = header(&headers, "x-amw-device").to_string();
    let ts: i64 = header(&headers, "x-amw-ts").parse().unwrap_or(0);
    let nonce = header(&headers, "x-amw-nonce").to_string();
    let sig = header(&headers, "x-amw-signature").to_string();
    let path = uri.path().to_string();
    let m = method.as_str().to_string();
    let core = st.core.clone();
    let st2 = st.clone();
    let body2 = body.clone();
    let dev = device.clone();
    let nonce2 = nonce.clone();
    let path2 = path.clone();
    let auth = blocking(move || st2.core.hub_authenticate(&st2.nonces, &dev, ts, &nonce2, &sig, &m, &path2, &body2)).await;
    let device_id = match auth {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(device = %device, path = %path, error = %e.message, "rejected hub request");
            return err_response(e);
        }
    };
    let key = {
        let core = core.clone();
        let d = device_id.clone();
        match blocking(move || core.hub_device_key(&d)).await {
            Ok(k) => k,
            Err(e) => return err_response(e),
        }
    };
    let (k_req, k_resp) = channel::device_keys(&key);
    let method_s = method.as_str().to_string();
    let seal_reply = |status: StatusCode, plain: Vec<u8>| -> Response {
        let sealed = channel::seal(&k_resp, &channel::aad("RESPONSE", &path, &device_id, ts, &nonce), &plain);
        let sig = sync::hmac_hex(&key, sync::signing_string("RESPONSE", &path, ts, &nonce, &sealed).as_bytes());
        let mut r = (status, [("content-type", "application/octet-stream")], sealed).into_response();
        r.headers_mut().insert("x-amw-sealed", HeaderValue::from_static("1"));
        if let Ok(hv) = HeaderValue::from_str(&sig) {
            r.headers_mut().insert("x-amw-signature", hv);
        }
        r
    };
    let seal_error = |e: AppError| seal_reply(status_for(e.code), serde_json::to_vec(&e).unwrap_or_default());
    let plain = if body.is_empty() {
        Bytes::new()
    } else {
        match channel::open(&k_req, &channel::aad(&method_s, &path, &device_id, ts, &nonce), &body) {
            Ok(p) => Bytes::from(p),
            Err(e) => return err_response(e),
        }
    };
    let res = {
        let core = core.clone();
        let d = device_id.clone();
        blocking(move || f(core, d, plain)).await
    };
    match res.and_then(|v| serde_json::to_vec(&v).map_err(|e| AppError::internal(e.to_string()))) {
        Ok(b) => seal_reply(StatusCode::OK, b),
        Err(e) => seal_error(e),
    }
}

async fn push(State(st): State<Arc<HubState>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    signed(st, method, uri, headers, body, |core, dev, body| {
        let req: PushRequest = serde_json::from_slice(&body).map_err(|e| AppError::validation(e.to_string()))?;
        core.hub_apply_push(&dev, req)
    })
    .await
}

async fn pull(State(st): State<Arc<HubState>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    signed(st, method, uri, headers, body, |core, dev, body| {
        let req: PullRequest = serde_json::from_slice(&body).map_err(|e| AppError::validation(e.to_string()))?;
        if req.device_id != dev {
            return Err(AppError::new(ErrorCode::Forbidden, "Device mismatch."));
        }
        core.hub_pull(&dev, req)
    })
    .await
}

async fn heartbeat(State(st): State<Arc<HubState>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    signed(st, method, uri, headers, body, |core, dev, body| {
        let hb: Heartbeat = serde_json::from_slice(&body).map_err(|e| AppError::validation(e.to_string()))?;
        if hb.device_id != dev {
            return Err(AppError::new(ErrorCode::Forbidden, "Device mismatch."));
        }
        core.hub_heartbeat(&dev, hb)
    })
    .await
}

async fn sync_status(State(st): State<Arc<HubState>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    signed(st, method, uri, headers, body, |core, _dev, _| core.hub_info()).await
}

async fn devices(State(st): State<Arc<HubState>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    signed(st, method, uri, headers, body, |core, _dev, _| {
        core.db.read(|c| {
            let mut s = c.prepare("SELECT name, device_code, operating_mode, active FROM devices ORDER BY device_code")?;
            let rows = s
                .query_map([], |r| {
                    Ok(serde_json::json!({ "name": r.get::<_, String>(0)?, "code": r.get::<_, String>(1)?, "mode": r.get::<_, String>(2)?, "active": r.get::<_, i64>(3)? == 1 }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    })
    .await
}

/// Private, loopback or link-local peers only (the phone view is LAN-only).
fn lan_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local(),
        IpAddr::V6(v) => {
            v.is_loopback()
                || (v.segments()[0] & 0xfe00) == 0xfc00
                || (v.segments()[0] & 0xffc0) == 0xfe80
                || v.to_ipv4_mapped().is_some_and(|m| m.is_private() || m.is_loopback() || m.is_link_local())
        }
    }
}

fn companion_file(path: &str) -> Option<(&'static str, &'static [u8])> {
    Some(match path {
        "" | "index.html" => ("text/html; charset=utf-8", include_bytes!("companion/index.html")),
        "app.js" => ("text/javascript; charset=utf-8", include_bytes!("companion/app.js")),
        "app.css" => ("text/css; charset=utf-8", include_bytes!("companion/app.css")),
        "sw.js" => ("text/javascript; charset=utf-8", include_bytes!("companion/sw.js")),
        "manifest.webmanifest" => ("application/manifest+json", include_bytes!("companion/manifest.webmanifest")),
        "icon.svg" => ("image/svg+xml", include_bytes!("companion/icon.svg")),
        _ => return None,
    })
}

const COMPANION_HEADERS: [(&str, &str); 4] = [
    ("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'"),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("x-frame-options", "DENY"),
];

async fn companion_static(State(st): State<Arc<HubState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, uri: Uri) -> Response {
    let core = st.core.clone();
    let on = blocking(move || Ok(core.features()?.is_on("pwa.companion"))).await.unwrap_or(false);
    if !on || !lan_peer(peer.ip()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = uri.path().trim_start_matches("/companion").trim_start_matches('/');
    match companion_file(path) {
        Some((ct, body)) => {
            let mut r = (StatusCode::OK, [("content-type", ct), ("cache-control", "no-cache")], body).into_response();
            for (k, v) in COMPANION_HEADERS {
                r.headers_mut().insert(k, HeaderValue::from_static(v));
            }
            r
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn companion_snapshot(State(st): State<Arc<HubState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> Response {
    if !lan_peer(peer.ip()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let bearer = header(&headers, "authorization").strip_prefix("Bearer ").unwrap_or("").trim().to_string();
    let core = st.core.clone();
    let mut r = match blocking(move || core.companion_snapshot(&bearer)).await {
        Ok(v) => json_response(&v),
        Err(e) => err_response(e),
    };
    r.headers_mut().insert("cache-control", HeaderValue::from_static("no-store"));
    for (k, v) in COMPANION_HEADERS {
        r.headers_mut().insert(k, HeaderValue::from_static(v));
    }
    r
}

pub fn router(core: Arc<AppCore>) -> Router {
    let st = Arc::new(HubState {
        core,
        nonces: NonceCache::default(),
        pair_attempts: Mutex::new(HashMap::new()),
        pending_pairs: Mutex::new(HashMap::new()),
        pair_failures: Mutex::new(HashMap::new()),
    });
    Router::new()
        .route("/health", get(health))
        .route("/info", get(info))
        .route("/pair", post(pair_v1))
        .route("/pair/start", post(pair_start))
        .route("/pair/finish", post(pair_finish))
        .route("/heartbeat", post(heartbeat))
        .route("/sync/push", post(push))
        .route("/sync/pull", post(pull))
        .route("/sync/status", get(sync_status))
        .route("/devices", get(devices))
        .route("/companion", get(companion_static))
        .route("/companion/", get(companion_static))
        .route("/companion/api/snapshot", get(companion_snapshot))
        .route("/companion/{file}", get(companion_static))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(st)
}

/// Serve the hub API until `shutdown` resolves.
pub async fn serve(
    core: Arc<AppCore>,
    addr: SocketAddr,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "hub API listening");
    axum::serve(listener, router(core).into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(shutdown).await
}
