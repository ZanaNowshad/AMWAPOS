//! Supervisor for the local Node sidecar (WhatsApp link + offline OCR).
//!
//! The sidecar is a separate process listening on 127.0.0.1 only. It is
//! started when the WhatsApp or OCR module is switched on, receives a random
//! bearer token (kept in the OS secret store) through its environment, and
//! exits when our stdin pipe closes. A lock file in the data folder keeps it
//! single-instance. Status distinguishes four things:
//! - `process`: whether the child process is running;
//! - `health`: whether it answers `/health`;
//! - `identity`: whether the answering process is *our* sidecar (same
//!   instance id it printed at start-up, not some other local service);
//! - the WhatsApp link state (`connected` / `ready`) reported by the sidecar.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::{AppCore, AppError, AppResult, ErrorCode};
use rand::RngCore;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const TOKEN_KEY: &str = "sidecar.token";
const READY_PREFIX: &str = "AMWAPOS_SIDECAR_READY ";

#[derive(Debug, Clone)]
pub struct SidecarPaths {
    pub node: PathBuf,
    pub script: PathBuf,
    pub models: Option<PathBuf>,
}

impl SidecarPaths {
    /// Find Node and the sidecar script: explicit environment overrides, then
    /// the bundled resources (installer), then a development checkout.
    pub fn discover(resource_dir: Option<&Path>) -> Option<SidecarPaths> {
        let node_name = if cfg!(windows) { "node.exe" } else { "node" };
        if let (Ok(n), Ok(s)) = (std::env::var("AMWAPOS_NODE"), std::env::var("AMWAPOS_SIDECAR")) {
            return Some(SidecarPaths { node: n.into(), script: s.into(), models: None });
        }
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(r) = resource_dir {
            roots.push(r.to_path_buf());
        }
        if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
            roots.push(exe_dir);
        }
        for r in &roots {
            let script = r.join("sidecar").join("src").join("main.mjs");
            let node = r.join("node").join(node_name);
            if script.is_file() && node.is_file() {
                return Some(SidecarPaths { node, script, models: None });
            }
        }
        // Development: repository checkout with Node on PATH.
        let mut dir = std::env::current_dir().ok();
        while let Some(d) = dir {
            let script = d.join("sidecar").join("src").join("main.mjs");
            if script.is_file() {
                return Some(SidecarPaths { node: PathBuf::from(node_name), script, models: None });
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
        None
    }
}

struct Running {
    child: Child,
    port: u16,
    pid: u32,
    instance_id: String,
    version: String,
    token: String,
}

#[derive(Default)]
struct State {
    running: Option<Running>,
    last_error: Option<String>,
    starts: u32,
}

pub struct Sidecar {
    state: Mutex<State>,
    paths: std::sync::Mutex<Option<SidecarPaths>>,
    http: reqwest::Client,
}

fn err(code: ErrorCode, msg: impl Into<String>, kind: &str) -> AppError {
    AppError::new(code, msg.into()).with_details(json!({ "kind": kind }))
}

fn unavailable(msg: impl Into<String>) -> AppError {
    err(ErrorCode::Conflict, msg, "sidecar_unavailable")
}

impl Sidecar {
    pub fn new(paths: Option<SidecarPaths>) -> Arc<Sidecar> {
        Arc::new(Sidecar {
            state: Mutex::new(State::default()),
            paths: std::sync::Mutex::new(paths),
            http: reqwest::Client::builder().no_proxy().build().unwrap_or_default(),
        })
    }

    pub fn set_paths(&self, p: Option<SidecarPaths>) {
        *self.paths.lock().unwrap() = p;
    }

    fn token(core: &AppCore) -> AppResult<String> {
        if let Some(t) = core.secrets.get(TOKEN_KEY)? {
            if t.len() >= 32 {
                return Ok(t);
            }
        }
        let mut b = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut b);
        let t = hex::encode(b);
        core.secrets.set(TOKEN_KEY, &t)?;
        Ok(t)
    }

    /// Start the sidecar if it is not running. Idempotent.
    pub async fn ensure(&self, core: &Arc<AppCore>) -> AppResult<()> {
        let mut st = self.state.lock().await;
        if let Some(r) = st.running.as_mut() {
            match r.child.try_wait() {
                Ok(None) => return Ok(()),
                Ok(Some(code)) => {
                    st.last_error = Some(format!("The sidecar stopped unexpectedly ({code})."));
                    st.running = None;
                }
                Err(e) => {
                    st.last_error = Some(e.to_string());
                    st.running = None;
                }
            }
        }
        let paths =
            self.paths.lock().unwrap().clone().ok_or_else(|| {
                unavailable("The WhatsApp/OCR sidecar is not installed on this computer. Reinstall AMWAPOS to restore it.")
            })?;
        let token = {
            let c = core.clone();
            tokio::task::spawn_blocking(move || Self::token(&c)).await.map_err(|e| AppError::internal(e.to_string()))??
        };
        let mut cmd = Command::new(&paths.node);
        cmd.arg(&paths.script).arg("--data-dir").arg(&core.data_dir).arg("--port").arg("0");
        if let Some(m) = &paths.models {
            cmd.arg("--models").arg(m);
        }
        cmd.env("AMWAPOS_SIDECAR_TOKEN", &token).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: no console flashes on the till.
            cmd.creation_flags(0x0800_0000);
        }
        st.starts += 1;
        let mut child = cmd.spawn().map_err(|e| {
            let m = format!("Could not start the sidecar ({}): {e}", paths.node.display());
            st.last_error = Some(m.clone());
            unavailable(m)
        })?;
        let stdout = child.stdout.take().ok_or_else(|| AppError::internal("sidecar stdout missing"))?;
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    tracing::info!(target: "sidecar", "{l}");
                }
            });
        }
        let mut lines = BufReader::new(stdout).lines();
        let ready = tokio::time::timeout(Duration::from_secs(45), async {
            while let Ok(Some(l)) = lines.next_line().await {
                if let Some(j) = l.strip_prefix(READY_PREFIX) {
                    return serde_json::from_str::<Value>(j).ok();
                }
            }
            None
        })
        .await;
        let info = match ready {
            Ok(Some(v)) => v,
            other => {
                let _ = child.start_kill();
                let code = child.wait().await.ok().and_then(|s| s.code());
                let m = match (other, code) {
                    (_, Some(3)) => "Another AMWAPOS sidecar is already running for this data folder.".to_string(),
                    (Err(_), _) => "The sidecar did not start within 45 seconds.".to_string(),
                    _ => format!("The sidecar exited during start-up (code {code:?}). See the log for details."),
                };
                st.last_error = Some(m.clone());
                return Err(unavailable(m));
            }
        };
        // Keep draining stdout so the child never blocks on a full pipe.
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        let port = info.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
        let r = Running {
            pid: child.id().unwrap_or(0),
            child,
            port,
            instance_id: info.get("instance_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            version: info.get("version").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            token,
        };
        tracing::info!(port, pid = r.pid, version = %r.version, "sidecar started on 127.0.0.1");
        st.running = Some(r);
        st.last_error = None;
        Ok(())
    }

    pub async fn stop(&self) {
        let mut st = self.state.lock().await;
        if let Some(mut r) = st.running.take() {
            let url = format!("http://127.0.0.1:{}/shutdown", r.port);
            let _ = self.http.post(url).bearer_auth(&r.token).timeout(Duration::from_secs(2)).send().await;
            if tokio::time::timeout(Duration::from_secs(5), r.child.wait()).await.is_err() {
                let _ = r.child.kill().await;
            }
            tracing::info!("sidecar stopped");
        }
    }

    pub async fn is_running(&self) -> bool {
        let mut st = self.state.lock().await;
        matches!(st.running.as_mut().map(|r| r.child.try_wait()), Some(Ok(None)))
    }

    async fn endpoint(&self) -> Option<(u16, String)> {
        let mut st = self.state.lock().await;
        let r = st.running.as_mut()?;
        match r.child.try_wait() {
            Ok(None) => Some((r.port, r.token.clone())),
            _ => {
                st.last_error = Some("The sidecar stopped unexpectedly.".into());
                st.running = None;
                None
            }
        }
    }

    /// Call a sidecar endpoint. `body = None` issues a GET.
    pub async fn call(&self, path: &str, body: Option<Value>, timeout: Duration) -> AppResult<Value> {
        let (port, token) = self.endpoint().await.ok_or_else(|| unavailable("The WhatsApp/OCR sidecar is not running."))?;
        let url = format!("http://127.0.0.1:{port}{path}");
        let rb = match body {
            Some(b) => self.http.post(url).json(&b),
            None => self.http.get(url),
        };
        let resp =
            rb.bearer_auth(token).timeout(timeout).send().await.map_err(|e| unavailable(format!("The sidecar did not respond: {e}")))?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(v);
        }
        let e = v.get("error").cloned().unwrap_or(Value::Null);
        let msg = e.get("message").and_then(|m| m.as_str()).unwrap_or("Sidecar request failed.").to_string();
        let kind = e.get("code").and_then(|m| m.as_str()).unwrap_or("sidecar_error").to_string();
        let code = match status.as_u16() {
            400 | 413 => ErrorCode::Validation,
            404 => ErrorCode::NotFound,
            409 => ErrorCode::Conflict,
            _ => ErrorCode::Internal,
        };
        let mut details = json!({ "kind": kind });
        if let Some(d) = e.get("details").filter(|d| !d.is_null()) {
            details["sidecar"] = d.clone();
        }
        Err(AppError::new(code, msg).with_details(details))
    }

    /// Separate process / health / identity / WhatsApp / OCR states.
    pub async fn status(&self) -> Value {
        let running = self.is_running().await;
        let (pid, port, instance, version, last_error, starts, installed) = {
            let st = self.state.lock().await;
            let r = st.running.as_ref();
            (
                r.map(|r| r.pid),
                r.map(|r| r.port),
                r.map(|r| r.instance_id.clone()),
                r.map(|r| r.version.clone()),
                st.last_error.clone(),
                st.starts,
                self.paths.lock().unwrap().is_some(),
            )
        };
        let short = Duration::from_secs(3);
        let health = running && self.call("/health", None, short).await.is_ok();
        let identity = if health {
            match self.call("/identity", None, short).await {
                Ok(v) => {
                    v.get("app").and_then(|a| a.as_str()) == Some("amwapos-sidecar")
                        && v.get("instance_id").and_then(|a| a.as_str()) == instance.as_deref()
                }
                Err(_) => false,
            }
        } else {
            false
        };
        let whatsapp = if identity { self.call("/whatsapp/status", None, short).await.ok() } else { None };
        let ocr = if identity { self.call("/ocr/status", None, short).await.ok() } else { None };
        json!({
            "installed": installed,
            "process": if running { "running" } else if last_error.is_some() { "failed" } else { "stopped" },
            "pid": pid,
            "port": port,
            "version": version,
            "health": health,
            "identity": identity,
            "whatsapp": whatsapp,
            "ocr": ocr,
            "last_error": last_error,
            "starts": starts,
        })
    }
}
