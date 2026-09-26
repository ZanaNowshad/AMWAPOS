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

use crate::ocr_worker::{OcrPaths, OcrWorker};
use crate::whatsapp::{RustWhatsAppAdapter, WhatsAppAdapter, WhatsAppService};
use crate::{client, discovery, server};

/// Result of an OS step-up check (Windows Hello) for a sensitive action.
#[derive(Debug, Clone)]
pub enum StepUp {
    Verified,
    /// No Hello on this computer (or not set up for the Windows user).
    Unavailable(String),
    /// The person cancelled or failed the check.
    Refused(String),
}

pub type StepUpHook = Arc<dyn Fn(&str) -> StepUp + Send + Sync>;

/// Commands that also need the step-up when the `windows_hello` module is on.
/// The staff PIN (or manager approval) is always still required.
pub const STEP_UP_COMMANDS: &[&str] = &["auth.approve", "refunds.create", "cash.event", "whatsapp.session_backup"];

/// Whether this call needs the step-up: manager overrides (approvals),
/// refunds, cash paid out, and the WhatsApp session backup acknowledgement.
pub fn needs_step_up(cmd: &str, args: &Value) -> bool {
    match cmd {
        "cash.event" => args.get("kind").or_else(|| args.get("event_type")).and_then(|k| k.as_str()) == Some("paid_out"),
        c => STEP_UP_COMMANDS.contains(&c),
    }
}

struct HubTasks {
    server: JoinHandle<()>,
    discovery: JoinHandle<()>,
    port: u16,
    ip: Ipv4Addr,
}

pub struct Runtime {
    pub core: Arc<AppCore>,
    hub: Mutex<Option<HubTasks>>,
    sync_loop: Mutex<Option<JoinHandle<()>>>,
    maintenance: Mutex<Option<JoinHandle<()>>>,
    /// WhatsApp (in-process adapter + supervisor), feature `whatsapp.enabled`.
    pub whatsapp: Arc<WhatsAppService>,
    /// OCR worker (bundled Tesseract), feature `ocr.enabled`.
    pub ocr: Arc<OcrWorker>,
    /// Signed update checker/installer.
    pub updater: Arc<crate::updater::Updater>,
    /// OS step-up provider (set by the desktop shell on Windows).
    pub step_up: Mutex<Option<StepUpHook>>,
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
            core: core.clone(),
            hub: Mutex::new(None),
            sync_loop: Mutex::new(None),
            maintenance: Mutex::new(None),
            whatsapp: WhatsAppService::new(core.clone(), Arc::new(RustWhatsAppAdapter::new())),
            ocr: OcrWorker::new(core.clone()),
            updater: crate::updater::Updater::new(),
            step_up: Mutex::new(None),
            bind_ip: Ipv4Addr::UNSPECIFIED,
            sync_interval: Duration::from_secs(5),
        })
    }

    pub fn with_bind(core: Arc<AppCore>, ip: Ipv4Addr, sync_interval: Duration) -> Arc<Self> {
        Arc::new(Self {
            core: core.clone(),
            hub: Mutex::new(None),
            sync_loop: Mutex::new(None),
            maintenance: Mutex::new(None),
            whatsapp: WhatsAppService::new(core.clone(), Arc::new(RustWhatsAppAdapter::new())),
            ocr: OcrWorker::new(core.clone()),
            updater: crate::updater::Updater::new(),
            step_up: Mutex::new(None),
            bind_ip: ip,
            sync_interval,
        })
    }

    /// Listen address: the test override, else the configured store-network
    /// address when it still belongs to this computer, else all interfaces.
    fn hub_ip(&self) -> Ipv4Addr {
        if self.bind_ip != Ipv4Addr::UNSPECIFIED {
            return self.bind_ip;
        }
        let want = self.core.terminal_sync_settings().map(|s| s.bind_address).unwrap_or_default();
        match want.parse::<Ipv4Addr>() {
            Ok(ip) if discovery::local_addresses().contains(&ip.to_string()) => ip,
            Ok(ip) => {
                tracing::warn!(%ip, "configured hub address is not on this computer; listening on all interfaces");
                Ipv4Addr::UNSPECIFIED
            }
            Err(_) => Ipv4Addr::UNSPECIFIED,
        }
    }

    fn hub_port(&self) -> u16 {
        self.core.terminal_sync_settings().map(|s| if s.port == 0 { DEFAULT_PORT } else { s.port }).unwrap_or(DEFAULT_PORT)
    }

    async fn step_up_check(&self, cmd: &str, token: Option<&str>) -> AppResult<()> {
        let on = self.core.features().map(|f| f.is_on("windows_hello")).unwrap_or(false);
        if !on {
            return Ok(());
        }
        let hook = self.step_up.lock().unwrap().clone();
        let msg = format!("AMWAPOS: confirm {}", cmd.replace(['.', '_'], " "));
        let result = match hook {
            Some(h) => tokio::task::spawn_blocking(move || h(&msg)).await.unwrap_or_else(|e| StepUp::Refused(e.to_string())),
            None => StepUp::Unavailable("Windows Hello is not available in this build.".into()),
        };
        let (outcome, detail, ok) = match &result {
            StepUp::Verified => ("verified", String::new(), true),
            StepUp::Unavailable(d) => ("unavailable", d.clone(), true),
            StepUp::Refused(d) => ("refused", d.clone(), false),
        };
        if let Some(t) = token {
            let (c, t, cmd2) = (self.core.clone(), t.to_string(), cmd.to_string());
            let _ = blocking(move || c.audit_step_up(&t, &cmd2, outcome, &detail)).await;
        }
        if ok {
            Ok(())
        } else {
            Err(AppError::new(
                amwapos_core::ErrorCode::Forbidden,
                "Windows Hello verification was not completed. The action was not performed.",
            )
            .with_details(json!({ "kind": "step_up_failed" })))
        }
    }

    /// Point the OCR worker at the installed engine and models (Tauri resource folder).
    pub fn set_resources(&self, resource_dir: Option<&std::path::Path>) {
        self.ocr.set_paths(OcrPaths::discover(resource_dir));
    }

    /// Replace the placeholder WhatsApp / OCR rows of the diagnostics report
    /// with the live state of the service and worker.
    async fn live_diagnostics(&self, v: &mut Value) {
        let f = self.core.features().unwrap_or_default();
        let mut wa = self.whatsapp.status();
        // A QR or pairing code would let someone link the number: never exported.
        wa.qr = None;
        wa.pair_code = None;
        let w = self.ocr.clone();
        let _ = tokio::task::spawn_blocking(move || w.prepare()).await;
        let ocr = self.ocr.status();
        let (wa_state, wa_summary) = if !f.is_on("whatsapp.enabled") {
            ("info", "Off (feature whatsapp.enabled)".to_string())
        } else if wa.ready {
            ("ok", format!("Ready · session {}", wa.session))
        } else if wa.process == "stopped" {
            ("info", format!("Stopped · session {}", wa.session))
        } else {
            ("warning", format!("{} · session {} · not ready (tills unaffected)", wa.process, wa.session))
        };
        let (ocr_state, ocr_summary) = if !f.is_on("ocr.enabled") {
            ("info", "Off (feature ocr.enabled)".to_string())
        } else if ocr.available {
            ("ok", format!("Ready ({})", ocr.languages.join(", ")))
        } else {
            ("error", ocr.error_code.clone().unwrap_or_else(|| "unavailable".into()))
        };
        if let Some(items) = v.as_array_mut() {
            for it in items.iter_mut() {
                match it.get("component").and_then(|c| c.as_str()) {
                    Some("WhatsApp") => {
                        *it = json!({ "component": "WhatsApp", "state": wa_state, "summary": wa_summary, "details": wa });
                    }
                    Some("OCR") => {
                        *it = json!({ "component": "OCR", "state": ocr_state, "summary": ocr_summary, "details": ocr });
                    }
                    _ => {}
                }
            }
        }
    }

    /// Use another WhatsApp adapter (tests use `FakeAdapter`).
    pub fn set_whatsapp_adapter(&self, a: Arc<dyn WhatsAppAdapter>) {
        self.whatsapp.set_adapter(a);
    }

    /// Start or stop services to match the current device mode. Idempotent.
    pub fn ensure_services(self: &Arc<Self>) {
        let mode = self.core.device().map(|d| d.mode);
        // Hub server + discovery responder.
        if mode.as_deref() == Some("hub") {
            let port = self.hub_port();
            let ip = self.hub_ip();
            let mut g = self.hub.lock().unwrap();
            let restart = match g.as_ref() {
                Some(t) => t.port != port || t.ip != ip || t.server.is_finished(),
                None => true,
            };
            if restart {
                if let Some(t) = g.take() {
                    t.server.abort();
                    t.discovery.abort();
                }
                let core = self.core.clone();
                let addr = SocketAddr::from((ip, port));
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
                *g = Some(HubTasks { server: srv, discovery: disc, port, ip });
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
        // Optional modules; each runs on its own tasks and never blocks selling.
        self.whatsapp.ensure();
        self.ocr.ensure();
        // Maintenance: scheduled backups.
        let mut g = self.maintenance.lock().unwrap();
        if g.as_ref().map(|h| h.is_finished()).unwrap_or(true) {
            let core = self.core.clone();
            let updater = self.updater.clone();
            let (wa, ocr) = (self.whatsapp.clone(), self.ocr.clone());
            *g = Some(tokio::spawn(async move {
                let mut minutes: u64 = 0;
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    minutes += 1;
                    // Watchdog: restart a WhatsApp or OCR task that died.
                    wa.ensure();
                    ocr.ensure();
                    let c = core.clone();
                    if let Ok(Ok((ok, _))) = tokio::task::spawn_blocking(move || c.receipt_pdf_retry_due()).await {
                        if ok > 0 {
                            tracing::info!(ok, "PDF receipt copies saved on retry");
                        }
                    }
                    // Daily signed-update check when the owner turned it on.
                    if minutes % 1440 == 5 {
                        let c = core.clone();
                        let st = tokio::task::spawn_blocking(move || {
                            let on = c.features().map(|f| f.is_on("updates")).unwrap_or(false);
                            let s: amwapos_core::settings::UpdateSettings =
                                c.db.read(|x| amwapos_core::settings::get(x, amwapos_core::settings::KEY_UPDATES)).unwrap_or_default();
                            (on && s.auto_check && !s.feed_url.is_empty()).then_some(s.feed_url)
                        })
                        .await
                        .ok()
                        .flatten();
                        if let Some(url) = st {
                            if let Err(e) = updater.check(&core, &url).await {
                                tracing::info!(error = %e.message, "update check");
                            }
                        }
                    }
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
        if needs_step_up(cmd, &args) {
            self.step_up_check(cmd, token.as_deref()).await?;
        }
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
                let ips = discovery::local_addresses();
                let bind = self.hub_ip();
                let shown: Vec<String> = if bind == Ipv4Addr::UNSPECIFIED { ips.clone() } else { vec![bind.to_string()] };
                let configured = self.core.terminal_sync_settings().map(|s| s.bind_address).unwrap_or_default();
                Ok(json!({ "addresses": shown.into_iter().map(|a| format!("http://{a}:{port}")).collect::<Vec<_>>(), "port": port,
                           "running": running, "ips": ips, "bind_address": configured }))
            }
            "updates.status" | "updates.check" | "updates.download" | "updates.install" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                let action = cmd.split('.').nth(1).unwrap_or_default().to_string();
                let (c, t2, a2) = (self.core.clone(), t.clone(), action.clone());
                let st = blocking(move || c.updates_authorize(&t2, &a2)).await?;
                match action.as_str() {
                    "status" => Ok(self.updater.status(&self.core).await),
                    "check" => self.updater.check(&self.core, &st.feed_url).await,
                    "download" => self.updater.download(&self.core).await,
                    _ => self.updater.install(&self.core, &t).await,
                }
            }
            "ai.ask" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                let conv = args.get("conversation_id").and_then(|v| v.as_str()).map(|s| s.to_string());
                let locale = args.get("locale").and_then(|v| v.as_str()).filter(|l| *l == "ar").unwrap_or("en").to_string();
                crate::ai_client::ask(self.clone(), t, conv, arg(&args, "message")?, locale).await
            }
            // A proposal runs the same command the admin page runs, through
            // this dispatcher, so Hello step-up and manager approval apply.
            "ai.proposal_confirm" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                let id = arg(&args, "proposal_id")?;
                let approval = args.get("approval_token").and_then(|v| v.as_str()).map(|s| s.to_string());
                let inputs = args.get("inputs").cloned().unwrap_or(Value::Null);
                let (c, t2, id2) = (self.core.clone(), t.clone(), id.clone());
                let prep = blocking(move || c.ai_proposal_prepare(&t2, &id2, approval, &inputs)).await?;
                let Some(prep) = prep else {
                    let c = self.core.clone();
                    return blocking(move || commands::dispatch(&c, "ai.proposal_confirm", Some(&t), args)).await;
                };
                let outcome = Box::pin(self.dispatch(&prep.command, Some(t.clone()), prep.args.clone())).await;
                let c = self.core.clone();
                blocking(move || c.ai_proposal_finish(&t, &prep.proposal_id, outcome, prep.secret_result)).await
            }
            "invoicescan.ai_parse" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                let scan_id = arg(&args, "scan_id")?;
                let c = self.core.clone();
                blocking(move || c.session(&t)?.require("ocr.scan")).await?;
                let replaced = crate::ocr_worker::ai_parse_now(&self.core, &scan_id).await?;
                Ok(json!({ "scan_id": scan_id, "replaced": replaced }))
            }
            "ai.test" | "ai.models" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                let (c, t2) = (self.core.clone(), t.clone());
                let conn = blocking(move || c.ai_connection(&t2)).await?;
                if cmd == "ai.test" {
                    Ok(crate::ai_client::test_connection(&conn).await)
                } else {
                    let ids = crate::ai_client::list_models(&conn).await?;
                    let c = self.core.clone();
                    blocking(move || c.ai_store_models(&t, ids)).await
                }
            }
            "sync.set_bind_address" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                let address = args.get("address").and_then(|v| v.as_str()).unwrap_or_default().trim().to_string();
                if let Ok(ip) = address.parse::<Ipv4Addr>() {
                    if !discovery::local_addresses().contains(&ip.to_string()) {
                        return Err(AppError::validation("That address does not belong to this computer."));
                    }
                }
                let c = self.core.clone();
                let r = blocking(move || c.sync_set_bind_address(&t, &address)).await?;
                self.ensure_services();
                Ok(r)
            }
            "whatsapp.status" | "ocr.status" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                let c = self.core.clone();
                let (features, summary) = blocking(move || {
                    let s = c.session(&t)?;
                    let ok =
                        ["whatsapp.manage", "whatsapp.send", "ocr.scan", "payments.review", "diagnostics.view"].iter().any(|p| s.has(p));
                    if !ok {
                        s.require("whatsapp.manage")?;
                    }
                    let summary = c.wa_summary(&t).unwrap_or(Value::Null);
                    Ok((c.features()?, summary))
                })
                .await?;
                self.whatsapp.ensure();
                let w = self.ocr.clone();
                let _ = tokio::task::spawn_blocking(move || w.prepare()).await;
                let flag = |n: &str| features.is_on(n);
                Ok(json!({
                    "whatsapp": self.whatsapp.status(),
                    "ocr": self.ocr.status(),
                    "queue": summary,
                    "features": {
                        "whatsapp.enabled": flag("whatsapp.enabled"),
                        "whatsapp.send_receipts": flag("whatsapp.send_receipts"),
                        "whatsapp.delivery_notices": flag("whatsapp.delivery_notices"),
                        "ocr.enabled": flag("ocr.enabled"),
                        "ocr.payment_screenshots": flag("ocr.payment_screenshots"),
                        "ocr.supplier_invoices": flag("ocr.supplier_invoices"),
                    },
                }))
            }
            "whatsapp.start" | "whatsapp.pair_code" | "whatsapp.stop" | "whatsapp.logout" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                let action = cmd.split('.').nth(1).unwrap_or_default().to_string();
                let (c, a2) = (self.core.clone(), action.clone());
                blocking(move || c.wa_link_action(&t, &a2)).await?;
                match action.as_str() {
                    "start" => self.whatsapp.start(None).await?,
                    "pair_code" => {
                        let phone = arg(&args, "phone")?;
                        let p = amwapos_core::customers::normalize_phone(&phone)?
                            .ok_or_else(|| AppError::validation("Enter the WhatsApp number of the shop's phone."))?;
                        self.whatsapp.start(Some(p)).await?
                    }
                    "stop" => self.whatsapp.stop().await?,
                    _ => self.whatsapp.logout().await?,
                }
                Ok(serde_json::to_value(self.whatsapp.status()).unwrap_or(Value::Null))
            }
            "whatsapp.session_backup" => {
                let t = token.clone().ok_or_else(|| AppError::new(amwapos_core::ErrorCode::Unauthenticated, "Please log in."))?;
                if args.get("acknowledge_risk").and_then(|v| v.as_bool()) != Some(true) {
                    return Err(AppError::validation(
                        "Confirm that you understand the risk: anyone with this file can read and send this shop's WhatsApp messages.",
                    ));
                }
                let c = self.core.clone();
                blocking(move || {
                    c.session(&t)?.require("settings.manage")?;
                    c.wa_link_action(&t, "session_backup")
                })
                .await?;
                let (sp, dir) = (self.whatsapp.session_path().clone(), self.core.data_dir.join("backups").join("whatsapp-session"));
                let path = blocking(move || sp.backup_to(&dir)).await?;
                Ok(json!({ "path": path.display().to_string() }))
            }
            _ => {
                // Turning OCR on needs the bundled models; otherwise it stays off.
                if cmd == "settings.save"
                    && args.get("key").and_then(|k| k.as_str()) == Some("features")
                    && args.get("value").and_then(|v| v.get("ocr.enabled")).and_then(|v| v.as_bool()) == Some(true)
                {
                    let w = self.ocr.clone();
                    tokio::task::spawn_blocking(move || w.prepare()).await.map_err(|e| AppError::internal(e.to_string()))??;
                }
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
                let res = match (cmd, res) {
                    ("diagnostics.get", Ok(mut v)) => {
                        self.live_diagnostics(&mut v).await;
                        Ok(v)
                    }
                    ("diagnostics.export", Ok(mut v)) => {
                        if let Some(items) = v.get_mut("items") {
                            self.live_diagnostics(items).await;
                        }
                        Ok(v)
                    }
                    (_, r) => r,
                };
                if res.is_ok() {
                    match cmd {
                        "setup.initialize" | "sync.enable_hub" | "backup.restore" | "settings.save" => self.ensure_services(),
                        "whatsapp.queue"
                        | "whatsapp.mark_read"
                        | "whatsapp.outbox_action"
                        | "payreviews.decide"
                        | "deliveries.update"
                        | "pos.finalize" => self.whatsapp.poke.notify_one(),
                        "invoicescan.import" | "payreviews.upload" | "ocr.retry" => self.ocr.poke.notify_one(),
                        _ => {}
                    }
                }
                res
            }
        }
    }
}
