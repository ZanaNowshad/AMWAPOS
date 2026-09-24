//! AMWAPOS development bridge.
//!
//! Serves the built UI and `POST /rpc` over **loopback only**, backed by the
//! exact same `Runtime`/`AppCore` the desktop app uses. It exists so the UI can
//! be developed and end-to-end tested in a browser. It is never packaged.
//!
//! Usage: amwapos-devserver --data-dir DIR [--port 8787] [--static DIR]

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::Runtime;
use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

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
    let (rt, startup_error) = match AppCore::open(&data_dir, Arc::new(MemorySecretStore::default())) {
        Ok(core) => {
            let rt = Runtime::new(Arc::new(core));
            rt.ensure_services();
            (Some(rt), None)
        }
        Err(e) => {
            tracing::error!(error = %e.message, "startup failed");
            (None, Some(e))
        }
    };
    let st = Arc::new(AppState { rt, startup_error, static_dir });
    let app = Router::new().route("/rpc", post(rpc)).fallback(static_files).with_state(st);
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
