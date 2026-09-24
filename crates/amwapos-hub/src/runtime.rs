//! Runtime: starts background services appropriate to the device mode and
//! routes network-backed commands. Everything else goes to the core
//! dispatcher on a blocking worker thread.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amwapos_core::commands;
use amwapos_core::sync::{PairRequest, DEFAULT_PORT};
use amwapos_core::{AppCore, AppError, AppResult};
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use crate::sidecar::{Sidecar, SidecarPaths};
use crate::{client, discovery, server};

struct HubTasks {
    server: JoinHandle<()>,
    discovery: JoinHandle<()>,
    port: u16,
}

pub struct Runtime {
    pub core: Arc<AppCore>,
    hub: Mutex<Option<HubTasks>>,
    sync_loop: Mutex<Option<JoinHandle<()>>>,
    maintenance: Mutex<Option<JoinHandle<()>>>,
    automation: Mutex<Option<JoinHandle<()>>>,
    /// WhatsApp/OCR sidecar supervisor.
    pub sidecar: Arc<Sidecar>,
    /// Reconnect a linked WhatsApp automatically (off after a manual disconnect).
    wa_autostart: Arc<std::sync::atomic::AtomicBool>,
    /// Override for the hub bind address (tests use 127.0.0.1 and port 0-style ports).
    pub bind_ip: Ipv4Addr,
    pub sync_interval: Duration,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::internal(format!("worker failed: {e}")))?
}

fn arg(args: &Value, k: &str) -> AppResult<String> {
    args.get(k).and_then(|v| v.as_str()).map(|s| s.to_string()).ok_or_else(|| AppError::validation(format!("Missing argument '{k}'.")))
}

impl Runtime {
    pub fn new(core: Arc<AppCore>) -> Arc<Self> {
        Arc::new(Self {
            core,
            hub: Mutex::new(None),
            sync_loop: Mutex::new(None),
            maintenance: Mutex::new(None),
            automation: Mutex::new(None),
            sidecar: Sidecar::new(SidecarPaths::discover(None)),
            wa_autostart: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            bind_ip: Ipv4Addr::UNSPECIFIED,
            sync_interval: Duration::from_secs(5),
        })
    }

    pub fn with_bind(core: Arc<AppCore>, ip: Ipv4Addr, sync_interval: Duration) -> Arc<Self> {
        Arc::new(Self {
            core,
            hub: Mutex::new(None),
            sync_loop: Mutex::new(None),
            maintenance: Mutex::new(None),
            automation: Mutex::new(None),
            sidecar: Sidecar::new(SidecarPaths::discover(None)),
            wa_autostart: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            bind_ip: ip,
            sync_interval,
        })
    }

    fn hub_port(&self) -> u16 {
        self.core.terminal_sync_settings().map(|s| if s.port == 0 { DEFAULT_PORT } else { s.port }).unwrap_or(DEFAULT_PORT)
    }

    /// Point the supervisor at the installed sidecar (Tauri resource folder).
    pub fn set_sidecar_resources(&self, resource_dir: Option<&std::path::Path>) {
        self.sidecar.set_paths(SidecarPaths::discover(resource_dir));
    }

    fn automation_wanted(&self) -> bool {
        self.core.features().map(|f| f.is_on("whatsapp") || f.is_on("ocr")).unwrap_or(false)
    }

    /// Start or stop services to match the current device mode. Idempotent.
    pub fn ensure_services(self: &Arc<Self>) {
        let mode = self.core.device().map(|d| d.mode);
        // Hub server + discovery responder.
        if mode.as_deref() == Some("hub") {
            let port = self.hub_port();
            let mut g = self.hub.lock().unwrap();
            let restart = match g.as_ref() {
                Some(t) => t.port != port || t.server.is_finished(),
                None => true,
            };
            if restart {
                if let Some(t) = g.take() {
                    t.server.abort();
                    t.discovery.abort();
                }
                let core = self.core.clone();
                let addr = SocketAddr::from((self.bind_ip, port));
                let srv = tokio::spawn(async move {
                    if let Err(e) = server::serve(core, addr, std::future::pending()).await {
                        tracing::error!(%addr, error = %e, "hub API stopped");
                    }
                });
                let core = self.core.clone();
                let disc = tokio::spawn(async move {
                    if let Err(e) = discovery::respond(core, port).await {
                        tracing::warn!(error = %e, "discovery responder unavailable");
                    }
                });
                *g = Some(HubTasks { server: srv, discovery: disc, port });
            }
        } else if let Some(t) = self.hub.lock().unwrap().take() {
            t.server.abort();
            t.discovery.abort();
        }
        // Terminal sync loop.
        if mode.as_deref() == Some("terminal") {
            let mut g = self.sync_loop.lock().unwrap();
            if g.as_ref().map(|h| h.is_finished()).unwrap_or(true) {
                *g = Some(client::spawn_sync_loop(self.core.clone(), self.sync_interval));
            }
        }
        // WhatsApp / OCR automation (sidecar) when either module is on.
        {
            let mut g = self.automation.lock().unwrap();
            let running = g.as_ref().map(|h| !h.is_finished()).unwrap_or(false);
            if self.automation_wanted() && !running {
                *g = Some(tokio::spawn(crate::automation::run(self.core.clone(), self.sidecar.clone(), self.wa_autostart.clone())));
            }
        }
        // Maintenance: scheduled backups.
        let mut g = self.maintenance.lock().unwrap();
        if g.as_ref().map(|h| h.is_finished()).unwrap_or(true) {
            let core = self.core.clone();
            *g = Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    let c = core.clone();
                    match tokio::task::spawn_blocking(move || c.backup_run_scheduled()).await {
                        Ok(Ok(Some(b))) => tracing::info!(path = %b.path, "automatic backup completed"),
                        Ok(Err(e)) => tracing::error!(error = %e.message, "automatic backup failed"),
                        _ => {}
                    }
                }
            }));
        }
    }

    /// Route a command. Network-backed commands are handled here; all others
    /// go to the core dispatcher.
    pub async fn dispatch(self: &Arc<Self>, cmd: &str, token: Option<String>, args: Value) -> AppResult<Value> {
        match cmd {
            "sync.discover" => {
                let found = discovery::discover(Duration::from_millis(1500))
                    .await
                    .map_err(|e| AppError::new(amwapos_core::ErrorCode::Sync, e.to_string()))?;
                Ok(serde_json::to_value(found).unwrap_or(Value::Null))
            }
            "sync.probe" => {
                let url = arg(&args, "hub_url")?;
                let info = client::hub_info(&url).await?;
                Ok(json!({ "url": client::normalize_url(&url)?, "info": info }))
            }
            "sync.join" => {
                if self.core.device().is_some() {
                    return Err(AppError::conflict("This installation is already set up."));
                }
                let url = client::normalize_url(&arg(&args, "hub_url")?)?;
                let code = arg(&args, "code")?;
                let req = PairRequest {
                    code: String::new(),
                    device_name: arg(&args, "device_name")?,
                    device_code: arg(&args, "device_code")?,
                    app_version: amwapos_core::audit::APP_VERSION.into(),
                    schema_version: amwapos_core::db::latest_schema_version(),
                    os_info: Some(format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)),
                };
                let resp = client::pair(&url, &code, &req).await?;
                let core = self.core.clone();
                let u2 = url.clone();
                blocking(move || core.terminal_bootstrap(&u2, resp)).await?;
                self.ensure_services();
                let status = blocking({
                    let c = self.core.clone();
                    move || c.setup_status()
                })
                .await?;
                Ok(serde_json::to_value(status).unwrap_or(Value::Null))
            }
            "sync.run_now" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                let c = self.core.clone();
                blocking(move || c.session(&t).map(|_| ())).await?;
                let r = client::sync_cycle(self.core.clone()).await?;
                Ok(serde_json::to_value(r).unwrap_or(Value::Null))
            }
            "sync.hub_addresses" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                let c = self.core.clone();
                blocking(move || c.session(&t).map(|_| ())).await?;
                let port = self.hub_port();
                let running = self.hub.lock().map(|g| g.as_ref().map(|t| !t.server.is_finished()).unwrap_or(false)).unwrap_or(false);
                Ok(
                    json!({ "addresses": discovery::local_addresses().into_iter().map(|a| format!("http://{a}:{port}")).collect::<Vec<_>>(), "port": port, "running": running }),
                )
            }
            "ai.ask" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                let conv = args.get("conversation_id").and_then(|v| v.as_str()).map(|s| s.to_string());
                crate::ai_client::ask(self.core.clone(), t, conv, arg(&args, "message")?).await
            }
            "sidecar.status" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                let c = self.core.clone();
                let features = blocking(move || {
                    let s = c.session(&t)?;
                    if !(s.has("whatsapp.manage") || s.has("ocr.scan") || s.has("payments.review") || s.has("diagnostics.view")) {
                        s.require("whatsapp.manage")?;
                    }
                    c.features()
                })
                .await?;
                let mut st = self.sidecar.status().await;
                st["features"] = json!({ "whatsapp": features.is_on("whatsapp"), "ocr": features.is_on("ocr"), "payment_reviews": features.is_on("payment_reviews") });
                st["autostart"] = json!(self.wa_autostart.load(std::sync::atomic::Ordering::Relaxed));
                Ok(st)
            }
            "whatsapp.connect" | "whatsapp.disconnect" | "whatsapp.unlink" | "sidecar.restart" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                let action = cmd.split('.').nth(1).unwrap_or_default().to_string();
                let c = self.core.clone();
                let a2 = action.clone();
                blocking(move || if a2 == "restart" { c.session(&t)?.require("settings.manage") } else { c.wa_link_action(&t, &a2) })
                    .await?;
                use std::sync::atomic::Ordering;
                let long = Duration::from_secs(20);
                let r = match action.as_str() {
                    "connect" => {
                        self.wa_autostart.store(true, Ordering::Relaxed);
                        self.sidecar.ensure(&self.core).await?;
                        self.ensure_services();
                        self.sidecar.call("/whatsapp/start", Some(json!({})), long).await?
                    }
                    "disconnect" => {
                        self.wa_autostart.store(false, Ordering::Relaxed);
                        self.sidecar.call("/whatsapp/stop", Some(json!({})), long).await.unwrap_or(Value::Null)
                    }
                    "unlink" => {
                        self.wa_autostart.store(false, Ordering::Relaxed);
                        self.sidecar.ensure(&self.core).await?;
                        self.sidecar.call("/whatsapp/logout", Some(json!({})), long).await?
                    }
                    _ => {
                        self.sidecar.stop().await;
                        if self.automation_wanted() {
                            self.sidecar.ensure(&self.core).await?;
                        }
                        self.ensure_services();
                        Value::Null
                    }
                };
                Ok(r)
            }
            _ => {
                let core = self.core.clone();
                let c = cmd.to_string();
                let res = blocking(move || match c.as_str() {
                    "sync.enable_hub" => {
                        let t =
                            token.as_deref().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                        core.sync_enable_hub(t)
                    }
                    "sync.unblock" => {
                        let t =
                            token.as_deref().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                        core.sync_unblock(t, args.get("accept_new_hub").and_then(|v| v.as_bool()).unwrap_or(false))
                    }
                    _ => commands::dispatch(&core, &c, token.as_deref(), args),
                })
                .await;
                if matches!(cmd, "setup.initialize" | "sync.enable_hub" | "backup.restore" | "settings.save") && res.is_ok() {
                    self.ensure_services();
                }
                res
            }
        }
    }
}
