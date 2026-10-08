//! AMWAPOS development bridge.
//!
//! Serves the built UI and `POST /rpc` over **loopback only**, backed by the
//! exact same `Runtime`/`AppCore` the desktop app uses. It exists so the UI can
//! be developed and end-to-end tested in a browser. It is never packaged.
//!
//! Usage: amwapos-devserver --data-dir DIR [--port 8787] [--static DIR] [--fake-whatsapp]
//!
//! `--fake-whatsapp` links the in-memory WhatsApp adapter instead of the real
//! client and adds `POST /dev/whatsapp` (`{"action":"scan"}`, `{"action":
//! "deliver","messages":[Inbound…]}`, `{"action":"sent"}`) so end-to-end tests
//! can drive customer chats without a phone.
//!
//! `--fake-terminal` adds `POST /dev/terminal` (see `fake_terminal`): real
//! in-process terminals paired with this hub, for the operational-control
//! end-to-end tests.

mod fake_terminal;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use amwapos_core::service::{AppCore, SecretStore};
use amwapos_core::{AppError, AppResult};
use amwapos_hub::whatsapp::FakeAdapter;
use amwapos_hub::Runtime;
use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

/// Development-only secret store: a plain JSON file next to the data directory,
/// so hub/terminal credentials survive devserver restarts. The desktop app uses
/// the Windows Credential Manager instead.
struct DevFileSecretStore {
    path: PathBuf,
    lock: std::sync::Mutex<()>,
}

impl DevFileSecretStore {
    fn load(&self) -> AppResult<std::collections::BTreeMap<String, String>> {
        match std::fs::read(&self.path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| AppError::internal(format!("dev secrets unreadable: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
            Err(e) => Err(e.into()),
        }
    }
    fn store(&self, m: &std::collections::BTreeMap<String, String>) -> AppResult<()> {
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(m)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl SecretStore for DevFileSecretStore {
    fn get(&self, key: &str) -> AppResult<Option<String>> {
        let _g = self.lock.lock().map_err(|_| AppError::internal("poisoned"))?;
        Ok(self.load()?.get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> AppResult<()> {
        let _g = self.lock.lock().map_err(|_| AppError::internal("poisoned"))?;
        let mut m = self.load()?;
        m.insert(key.into(), value.into());
        self.store(&m)
    }
    fn delete(&self, key: &str) -> AppResult<()> {
        let _g = self.lock.lock().map_err(|_| AppError::internal("poisoned"))?;
        let mut m = self.load()?;
        m.remove(key);
        self.store(&m)
    }
}

#[derive(Deserialize)]
struct Rpc {
    cmd: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    args: Value,
}

struct AppState {
    rt: Option<Arc<Runtime>>,
    fake_wa: Option<FakeAdapter>,
    fake_terms: Option<fake_terminal::FakeTerminals>,
    startup_error: Option<amwapos_core::AppError>,
    static_dir: Option<PathBuf>,
}

async fn rpc(State(st): State<Arc<AppState>>, Json(req): Json<Rpc>) -> Response {
    let res = match (&st.rt, &st.startup_error) {
        (Some(rt), _) => rt.dispatch(&req.cmd, req.token, if req.args.is_null() { json!({}) } else { req.args }).await,
        (None, Some(e)) => Err(e.clone()),
        _ => Err(amwapos_core::AppError::internal("not started")),
    };
    match res {
        Ok(v) => Json(json!({ "ok": true, "data": v })).into_response(),
        Err(e) => Json(json!({ "ok": false, "error": e })).into_response(),
    }
}

async fn dev_whatsapp(State(st): State<Arc<AppState>>, Json(req): Json<Value>) -> Response {
    let Some(fake) = &st.fake_wa else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match req["action"].as_str() {
        Some("scan") => {
            fake.scan();
            Json(json!({ "ok": true })).into_response()
        }
        Some("deliver") => {
            let batch: Vec<amwapos_core::messaging::Inbound> = match serde_json::from_value(req["messages"].clone()) {
                Ok(b) => b,
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            };
            let ok = fake.deliver(batch).await;
            Json(json!({ "ok": ok })).into_response()
        }
        Some("sent") => {
            Json(json!({ "ok": true, "sent": fake.sent().iter().map(|m| json!({ "to": m.to, "text": m.text })).collect::<Vec<_>>() }))
                .into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn dev_terminal(State(st): State<Arc<AppState>>, Json(req): Json<Value>) -> Response {
    let (Some(_), Some(rt)) = (&st.fake_terms, &st.rt) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let core = rt.core.clone();
    let st2 = st.clone();
    let res = tokio::task::spawn_blocking(move || match &st2.fake_terms {
        Some(t) => t.act(&core, &req),
        None => Err(AppError::internal("no fake terminals")),
    })
    .await
    .unwrap_or_else(|e| Err(AppError::internal(e.to_string())));
    match res {
        Ok(v) => Json(json!({ "ok": true, "data": v })).into_response(),
        Err(e) => Json(json!({ "ok": false, "error": e })).into_response(),
    }
}

async fn static_files(State(st): State<Arc<AppState>>, uri: Uri) -> Response {
    let Some(dir) = &st.static_dir else {
        return (StatusCode::NOT_FOUND, "UI not built").into_response();
    };
    let path = uri.path().trim_start_matches('/');
    if path.contains("..") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut file = dir.join(if path.is_empty() { "index.html" } else { path });
    if !file.is_file() {
        file = dir.join("index.html");
    }
    match tokio::fs::read(&file).await {
        Ok(bytes) => {
            let ct = match file.extension().and_then(|e| e.to_str()) {
                Some("html") => "text/html; charset=utf-8",
                Some("js") => "text/javascript",
                Some("css") => "text/css",
                Some("svg") => "image/svg+xml",
                Some("png") => "image/png",
                Some("woff2") => "font/woff2",
                Some("json") => "application/json",
                _ => "application/octet-stream",
            };
            ([(header::CONTENT_TYPE, ct)], bytes).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into())).init();
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let data_dir = PathBuf::from(get("--data-dir").unwrap_or_else(|| ".amwapos-dev/data".into()));
    let port: u16 = get("--port").and_then(|p| p.parse().ok()).unwrap_or(8787);
    let static_dir = get("--static").map(PathBuf::from);
    let _ = std::fs::create_dir_all(&data_dir);
    // The WhatsApp session file needs an absolute path.
    let data_dir = std::path::absolute(&data_dir).unwrap_or(data_dir);
    let secrets = DevFileSecretStore { path: data_dir.parent().unwrap_or(&data_dir).join("dev-secrets.json"), lock: Default::default() };
    let fake_wa = args.iter().any(|a| a == "--fake-whatsapp").then(FakeAdapter::new);
    let (rt, startup_error) = match AppCore::open(&data_dir, Arc::new(secrets)) {
        Ok(core) => {
            let rt = Runtime::new(Arc::new(core));
            if let Some(f) = &fake_wa {
                rt.set_whatsapp_adapter(Arc::new(f.clone()));
            }
            rt.ensure_services();
            (Some(rt), None)
        }
        Err(e) => {
            tracing::error!(error = %e.message, "startup failed");
            (None, Some(e))
        }
    };
    let fake_terms = args
        .iter()
        .any(|a| a == "--fake-terminal")
        .then(|| fake_terminal::FakeTerminals::new(data_dir.parent().unwrap_or(&data_dir).join("fake-terminals")));
    let st = Arc::new(AppState { rt, fake_wa, fake_terms, startup_error, static_dir });
    let app = Router::new()
        .route("/rpc", post(rpc))
        .route("/dev/whatsapp", post(dev_whatsapp))
        .route("/dev/terminal", post(dev_terminal))
        .fallback(static_files)
        .with_state(st);
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    tracing::info!(%addr, data_dir = %data_dir.display(), "AMWAPOS dev bridge listening (loopback only)");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .unwrap();
}
