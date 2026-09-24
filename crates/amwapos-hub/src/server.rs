//! Hub HTTP API for terminals on the store LAN.
//!
//! Routes (no route exposes SQL or files):
//!   GET  /health            liveness (public)
//!   GET  /info              hub identity and versions (public, no secrets)
//!   POST /pair              one-time pairing with a code (rate limited)
//!   POST /heartbeat         signed
//!   POST /sync/push         signed
//!   POST /sync/pull         signed
//!   GET  /sync/status       signed
//!   GET  /devices           signed (names and codes only)

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use amwapos_core::sync::{self, Heartbeat, NonceCache, PairRequest, PullRequest, PushRequest};
use amwapos_core::{AppCore, AppError, AppResult, ErrorCode};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Serialize;

pub struct HubState {
    pub core: Arc<AppCore>,
    pub nonces: NonceCache,
    pair_attempts: Mutex<HashMap<IpAddr, (u32, i64)>>,
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

async fn pair(State(st): State<Arc<HubState>>, ConnectInfo(addr): ConnectInfo<SocketAddr>, body: Bytes) -> Response {
    // Brute-force protection: 5 attempts per IP per 5 minutes.
    {
        let now = chrono::Utc::now().timestamp();
        let mut m = match st.pair_attempts.lock() {
            Ok(m) => m,
            Err(_) => return err_response(AppError::internal("state poisoned")),
        };
        let e = m.entry(addr.ip()).or_insert((0, now));
        if now - e.1 > 300 {
            *e = (0, now);
        }
        e.0 += 1;
        if e.0 > 5 {
            return err_response(AppError::new(ErrorCode::AccountLocked, "Too many pairing attempts. Wait five minutes and try again."));
        }
    }
    let req: PairRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return err_response(AppError::validation(format!("Invalid pairing request: {e}"))),
    };
    let core = st.core.clone();
    match blocking(move || core.hub_pair(req)).await {
        Ok(r) => {
            if let Ok(mut m) = st.pair_attempts.lock() {
                m.remove(&addr.ip());
            }
            tracing::info!(peer = %addr, device = %r.device.device_code, "terminal paired");
            json_response(&r)
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
    let res = {
        let core = core.clone();
        let d = device_id.clone();
        blocking(move || f(core, d, body)).await
    };
    match res {
        Ok(v) => match serde_json::to_vec(&v) {
            Ok(b) => {
                let sig = sync::hmac_hex(&key, sync::signing_string("RESPONSE", &path, ts, &nonce, &b).as_bytes());
                let mut r = (StatusCode::OK, [("content-type", "application/json")], b).into_response();
                if let Ok(hv) = HeaderValue::from_str(&sig) {
                    r.headers_mut().insert("x-amw-signature", hv);
                }
                r
            }
            Err(e) => err_response(AppError::internal(e.to_string())),
        },
        Err(e) => err_response(e),
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

pub fn router(core: Arc<AppCore>) -> Router {
    let st = Arc::new(HubState { core, nonces: NonceCache::default(), pair_attempts: Mutex::new(HashMap::new()) });
    Router::new()
        .route("/health", get(health))
        .route("/info", get(info))
        .route("/pair", post(pair))
        .route("/heartbeat", post(heartbeat))
        .route("/sync/push", post(push))
        .route("/sync/pull", post(pull))
        .route("/sync/status", get(sync_status))
        .route("/devices", get(devices))
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
