//! WhatsApp service: supervisor + I/O worker around a `WhatsAppAdapter`.
//!
//! * The supervisor is a dedicated Tokio task that owns the client. It never
//!   holds a lock while the client runs: the client's exit future lives in
//!   the task's own state, and commands arrive over a channel. When the
//!   client ends or panics it is restarted with back-off (unless it was
//!   stopped or logged out).
//! * The I/O worker is a second task that sends queued outbox rows, pushes
//!   read receipts and downloads inbound media, using the current session
//!   from a watch channel.
//! * Sales, refunds and shifts never call into this module; they only insert
//!   outbox rows. If WhatsApp is down, the tills keep selling and the status
//!   shows WhatsApp as down or reconnecting.
//! * Inbound messages are committed to AMWAPOS tables (in the sink) before
//!   WhatsApp is acknowledged and before anything else can observe them.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amwapos_core::messaging::Inbound;
use amwapos_core::{AppCore, AppError, AppResult, ErrorCode};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio::task::JoinHandle;

use super::adapter::*;
use super::session::SessionPath;

const TICK: Duration = Duration::from_secs(3);
const SEND_TIMEOUT: Duration = Duration::from_secs(60);
const START_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Separate flags, as shown in the admin screen and diagnostics.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WaStatus {
    /// `whatsapp.enabled` feature flag.
    pub enabled: bool,
    /// Client task: disabled | stopped | starting | running | restarting | failed
    pub process: String,
    /// Link state: none | pairing | paired | logged_out
    pub session: String,
    /// Socket connected to WhatsApp.
    pub connected: bool,
    /// Connected, logged in and able to send.
    pub ready: bool,
    pub account: Option<String>,
    pub qr: Option<Value>,
    pub pair_code: Option<Value>,
    pub last_error: Option<String>,
    pub restarts: u32,
    pub next_retry_at: Option<String>,
    pub banned_until: Option<String>,
    pub adapter: String,
    pub session_file: String,
    /// Increases whenever inbound messages were committed (UI refresh hint).
    pub inbox_rev: u64,
    pub last_send_at: Option<String>,
    pub last_send_error: Option<String>,
}

impl WaStatus {
    fn initial(adapter: &str, session: &SessionPath) -> Self {
        Self {
            enabled: false,
            process: "stopped".into(),
            session: if session.exists() { "paired".into() } else { "none".into() },
            connected: false,
            ready: false,
            account: None,
            qr: None,
            pair_code: None,
            last_error: None,
            restarts: 0,
            next_retry_at: None,
            banned_until: None,
            adapter: adapter.into(),
            session_file: session.path().display().to_string(),
            inbox_rev: 0,
            last_send_at: None,
            last_send_error: None,
        }
    }
}

enum Cmd {
    Start { pair_phone: Option<String>, reply: oneshot::Sender<AppResult<()>> },
    Stop { reply: oneshot::Sender<()> },
    Logout { reply: oneshot::Sender<AppResult<()>> },
}

type Session = Option<Arc<dyn AdapterSession>>;

pub struct WhatsAppService {
    core: Arc<AppCore>,
    session_path: SessionPath,
    adapter: Mutex<Arc<dyn WhatsAppAdapter>>,
    status: watch::Sender<WaStatus>,
    session: watch::Sender<Session>,
    cmd: Mutex<Option<mpsc::Sender<Cmd>>>,
    tasks: Mutex<Option<(JoinHandle<()>, JoinHandle<()>)>>,
    /// Wakes the I/O worker (new outbox row, read mark).
    pub poke: Arc<Notify>,
    inbox_rev: Arc<AtomicU64>,
    /// Set when the session location was refused; the client never starts.
    path_error: Option<String>,
}

/// The pairing QR as an SVG image, so the UI only displays a picture.
fn qr_svg(code: &str) -> Option<String> {
    let q = qrcode::QrCode::new(code.as_bytes()).ok()?;
    Some(q.render::<qrcode::render::svg::Color>().min_dimensions(280, 280).quiet_zone(true).build())
}

fn now() -> String {
    amwapos_core::time::now_str()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::internal(format!("worker failed: {e}")))?
}

fn unavailable(msg: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::Conflict, msg).with_details(json!({ "kind": "whatsapp_unavailable" }))
}

impl WhatsAppService {
    /// Never fails: a refused session location disables WhatsApp (reported
    /// in the status) instead of stopping the app.
    pub fn new(core: Arc<AppCore>, adapter: Arc<dyn WhatsAppAdapter>) -> Arc<Self> {
        let (session_path, path_error) = match SessionPath::for_data_dir(&core.data_dir) {
            Ok(p) => (p, None),
            Err(e) => (SessionPath::unchecked(&core.data_dir), Some(e.message)),
        };
        let mut initial = WaStatus::initial(adapter.name(), &session_path);
        initial.last_error = path_error.clone();
        let (status, _) = watch::channel(initial);
        let (session, _) = watch::channel(None);
        Arc::new(Self {
            core,
            session_path,
            adapter: Mutex::new(adapter),
            status,
            session,
            cmd: Mutex::new(None),
            tasks: Mutex::new(None),
            poke: Arc::new(Notify::new()),
            inbox_rev: Arc::new(AtomicU64::new(0)),
            path_error,
        })
    }

    /// Replace the adapter (tests). Takes effect on the next client start.
    pub fn set_adapter(&self, a: Arc<dyn WhatsAppAdapter>) {
        self.status.send_modify(|s| s.adapter = a.name().into());
        *self.adapter.lock().unwrap() = a;
    }

    pub fn session_path(&self) -> &SessionPath {
        &self.session_path
    }

    pub fn status(&self) -> WaStatus {
        self.status.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<WaStatus> {
        self.status.subscribe()
    }

    fn enabled(&self) -> bool {
        self.core.features().map(|f| f.is_on("whatsapp.enabled")).unwrap_or(false)
    }

    /// Start the supervisor and worker if the module is on and they are not
    /// running (also restarts them if either task died). Idempotent.
    pub fn ensure(self: &Arc<Self>) {
        let on = self.enabled();
        let mut g = self.tasks.lock().unwrap();
        let alive = g.as_ref().map(|(a, b)| !a.is_finished() && !b.is_finished()).unwrap_or(false);
        if !on {
            self.status.send_modify(|s| {
                s.enabled = false;
                if s.process != "running" {
                    s.process = "disabled".into();
                }
            });
            return; // A running supervisor notices the flag and stops itself.
        }
        if alive {
            return;
        }
        if let Some((a, b)) = g.take() {
            a.abort();
            b.abort();
        }
        let (tx, rx) = mpsc::channel(16);
        *self.cmd.lock().unwrap() = Some(tx);
        let sup = tokio::spawn(supervise(self.clone(), rx));
        let io = tokio::spawn(io_worker(self.clone()));
        *g = Some((sup, io));
    }

    async fn send_cmd<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Cmd) -> AppResult<T> {
        let tx = self.cmd.lock().unwrap().clone().ok_or_else(|| unavailable("WhatsApp is not enabled on this computer."))?;
        let (rtx, rrx) = oneshot::channel();
        tx.send(make(rtx)).await.map_err(|_| unavailable("The WhatsApp service is restarting. Try again in a moment."))?;
        tokio::time::timeout(START_TIMEOUT + Duration::from_secs(5), rrx)
            .await
            .map_err(|_| unavailable("WhatsApp did not respond in time."))?
            .map_err(|_| unavailable("The WhatsApp service is restarting. Try again in a moment."))
    }

    /// Start (or restart) the client; with `pair_phone`, link by pairing code.
    pub async fn start(self: &Arc<Self>, pair_phone: Option<String>) -> AppResult<()> {
        self.ensure();
        self.send_cmd(|reply| Cmd::Start { pair_phone, reply }).await?
    }

    pub async fn stop(self: &Arc<Self>) -> AppResult<()> {
        if self.cmd.lock().unwrap().is_none() {
            return Ok(());
        }
        self.send_cmd(|reply| Cmd::Stop { reply }).await
    }

    pub async fn logout(self: &Arc<Self>) -> AppResult<()> {
        self.ensure();
        self.send_cmd(|reply| Cmd::Logout { reply }).await?
    }

    fn autostart(&self) -> bool {
        self.core.db.read(|c| amwapos_core::settings::get::<Option<bool>>(c, "local.whatsapp_autostart")).ok().flatten().unwrap_or(true)
    }

    fn set_autostart(&self, v: bool) {
        let _ = self.core.db.write(|tx| amwapos_core::settings::put(tx, "local.whatsapp_autostart", &v, None));
    }
}

/// Sink handed to the adapter: status updates in memory, inbound committed
/// to the database before `Ok` is returned.
struct Sink {
    core: Arc<AppCore>,
    status: watch::Sender<WaStatus>,
    inbox_rev: Arc<AtomicU64>,
}

#[async_trait]
impl AdapterSink for Sink {
    fn event(&self, e: AdapterEvent) {
        let expires = |d: Duration| amwapos_core::time::fmt(amwapos_core::time::now() + chrono::Duration::seconds(d.as_secs() as i64));
        self.status.send_modify(|s| match e {
            AdapterEvent::Qr { code, valid_for } => {
                s.session = "pairing".into();
                s.qr = Some(json!({ "svg": qr_svg(&code), "expires_at": expires(valid_for) }));
                s.pair_code = None;
            }
            AdapterEvent::PairCode { code, valid_for } => {
                s.session = "pairing".into();
                s.pair_code = Some(json!({ "code": code, "expires_at": expires(valid_for) }));
            }
            AdapterEvent::PairCodeError(m) => s.last_error = Some(format!("Pairing code refused: {m}")),
            AdapterEvent::QrExhausted => {
                s.qr = None;
                s.last_error = Some("The QR code expired. Press Link to get a new one.".into());
            }
            AdapterEvent::Paired { account } => {
                s.session = "paired".into();
                s.account = Some(account);
                s.qr = None;
                s.pair_code = None;
            }
            AdapterEvent::Connected { account } => {
                s.session = "paired".into();
                s.connected = true;
                s.ready = true;
                s.qr = None;
                s.pair_code = None;
                s.last_error = None;
                s.banned_until = None;
                if account.is_some() {
                    s.account = account;
                }
            }
            AdapterEvent::Disconnected(m) => {
                s.connected = false;
                s.ready = false;
                s.last_error = Some(format!("Disconnected: {m}. Reconnecting."));
            }
            AdapterEvent::LoggedOut(m) => {
                s.session = "logged_out".into();
                s.connected = false;
                s.ready = false;
                s.account = None;
                s.last_error = Some(format!("Logged out: {m}"));
            }
            AdapterEvent::TemporaryBan { reason, expires_s } => {
                s.ready = false;
                s.banned_until = Some(expires(Duration::from_secs(expires_s)));
                s.last_error = Some(format!("WhatsApp temporarily banned this number: {reason}"));
            }
            AdapterEvent::StreamReplaced => {
                s.connected = false;
                s.ready = false;
                s.last_error = Some("WhatsApp was opened for this number on another computer.".into());
            }
            AdapterEvent::ClientOutdated => {
                s.last_error = Some("WhatsApp rejected this client version. An AMWAPOS update is needed.".into());
            }
        });
    }

    async fn inbound(&self, batch: Vec<Inbound>) -> Result<(), String> {
        let core = self.core.clone();
        let n = blocking(move || core.wa_ingest(&batch)).await.map_err(|e| e.message)?;
        if n > 0 {
            let rev = self.inbox_rev.fetch_add(1, Ordering::SeqCst) + 1;
            self.status.send_modify(|s| s.inbox_rev = rev);
        }
        Ok(())
    }
}

async fn wait_exit(e: &mut Option<ExitFuture>) -> Exit {
    match e {
        Some(f) => f.await,
        None => std::future::pending().await,
    }
}

async fn sleep_until_opt(t: Option<tokio::time::Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

async fn supervise(svc: Arc<WhatsAppService>, mut rx: mpsc::Receiver<Cmd>) {
    let mut session: Session = None;
    let mut exit: Option<ExitFuture> = None;
    let mut wanted = svc.session_path.exists() && svc.autostart();
    let mut backoff = Duration::from_secs(2);
    let mut retry_at = wanted.then(tokio::time::Instant::now);
    let mut pair_phone: Option<String> = None;
    let mut flag_check = tokio::time::interval(Duration::from_secs(5));
    svc.status.send_modify(|s| {
        s.enabled = true;
        s.process = "stopped".into();
    });

    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Cmd::Start { pair_phone: p, reply } => {
                        wanted = true;
                        svc.set_autostart(true);
                        pair_phone = p;
                        if let Some(s) = session.take() {
                            s.stop().await;
                            let _ = tokio::time::timeout(Duration::from_secs(10), wait_exit(&mut exit)).await;
                            exit = None;
                        }
                        let r = start_client(&svc, pair_phone.clone()).await;
                        match r {
                            Ok((s, e)) => {
                                session = Some(s);
                                exit = Some(e);
                                backoff = Duration::from_secs(2);
                                retry_at = None;
                                let _ = reply.send(Ok(()));
                            }
                            Err(e) => {
                                retry_at = Some(tokio::time::Instant::now() + backoff);
                                let _ = reply.send(Err(unavailable(e.message)));
                            }
                        }
                    }
                    Cmd::Stop { reply } => {
                        wanted = false;
                        svc.set_autostart(false);
                        retry_at = None;
                        if let Some(s) = session.take() {
                            s.stop().await;
                            let _ = tokio::time::timeout(Duration::from_secs(10), wait_exit(&mut exit)).await;
                        }
                        exit = None;
                        svc.session.send_replace(None);
                        svc.status.send_modify(|s| {
                            s.process = "stopped".into();
                            s.connected = false;
                            s.ready = false;
                            s.qr = None;
                            s.pair_code = None;
                            s.next_retry_at = None;
                        });
                        let _ = reply.send(());
                    }
                    Cmd::Logout { reply } => {
                        wanted = false;
                        retry_at = None;
                        if let Some(s) = session.take() {
                            s.logout().await;
                            let _ = tokio::time::timeout(Duration::from_secs(10), wait_exit(&mut exit)).await;
                        }
                        exit = None;
                        svc.session.send_replace(None);
                        let removed = svc.session_path.remove();
                        svc.status.send_modify(|s| {
                            s.process = "stopped".into();
                            s.session = "logged_out".into();
                            s.connected = false;
                            s.ready = false;
                            s.account = None;
                            s.qr = None;
                            s.pair_code = None;
                        });
                        let _ = reply.send(removed.map_err(|e| AppError::internal(format!("Could not remove the WhatsApp session file: {e}"))));
                    }
                }
            }
            ended = wait_exit(&mut exit) => {
                exit = None;
                session = None;
                svc.session.send_replace(None);
                // Let a LoggedOut event from the client land first.
                tokio::time::sleep(Duration::from_millis(300)).await;
                let logged_out = svc.status.borrow().session == "logged_out";
                if logged_out {
                    wanted = false;
                    let _ = svc.session_path.remove();
                }
                let failed = match &ended { Exit::Failed(m) => Some(m.clone()), Exit::Ended => None };
                if let Some(m) = &failed {
                    tracing::error!(error = %m, "WhatsApp client stopped unexpectedly; tills are unaffected");
                }
                svc.status.send_modify(|s| {
                    s.connected = false;
                    s.ready = false;
                    s.qr = None;
                    s.pair_code = None;
                    if let Some(m) = &failed {
                        s.last_error = Some(m.clone());
                    }
                });
                if wanted {
                    retry_at = Some(tokio::time::Instant::now() + backoff);
                    let at = amwapos_core::time::fmt(amwapos_core::time::now() + chrono::Duration::seconds(backoff.as_secs() as i64));
                    svc.status.send_modify(|s| {
                        s.process = "restarting".into();
                        s.restarts += 1;
                        s.next_retry_at = Some(at);
                    });
                } else {
                    svc.status.send_modify(|s| s.process = "stopped".into());
                }
            }
            _ = sleep_until_opt(retry_at), if session.is_none() && wanted => {
                match start_client(&svc, pair_phone.clone()).await {
                    Ok((s, e)) => {
                        session = Some(s);
                        exit = Some(e);
                        retry_at = None;
                        backoff = Duration::from_secs(2);
                    }
                    Err(_) => {
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        retry_at = Some(tokio::time::Instant::now() + backoff);
                        let at = amwapos_core::time::fmt(amwapos_core::time::now() + chrono::Duration::seconds(backoff.as_secs() as i64));
                        svc.status.send_modify(|s| { s.process = "failed".into(); s.next_retry_at = Some(at); });
                    }
                }
            }
            _ = flag_check.tick() => {
                let c = svc.core.clone();
                let on = blocking(move || c.features().map(|f| f.is_on("whatsapp.enabled"))).await.unwrap_or(false);
                if !on {
                    if let Some(s) = session.take() {
                        s.stop().await;
                        let _ = tokio::time::timeout(Duration::from_secs(10), wait_exit(&mut exit)).await;
                    }
                    svc.session.send_replace(None);
                    svc.status.send_modify(|s| {
                        s.enabled = false;
                        s.process = "disabled".into();
                        s.connected = false;
                        s.ready = false;
                        s.qr = None;
                        s.pair_code = None;
                    });
                    *svc.cmd.lock().unwrap() = None;
                    return;
                }
            }
        }
    }
}

/// Start the client on its own task. A panic during start is caught.
async fn start_client(
    svc: &Arc<WhatsAppService>,
    pair_phone: Option<String>,
) -> Result<(Arc<dyn AdapterSession>, ExitFuture), AdapterError> {
    if let Some(e) = &svc.path_error {
        svc.status.send_modify(|s| {
            s.process = "failed".into();
            s.last_error = Some(e.clone());
        });
        return Err(AdapterError::permanent(e.clone()));
    }
    svc.status.send_modify(|s| {
        s.process = "starting".into();
        s.next_retry_at = None;
    });
    let adapter = svc.adapter.lock().unwrap().clone();
    let sink: Arc<dyn AdapterSink> =
        Arc::new(Sink { core: svc.core.clone(), status: svc.status.clone(), inbox_rev: svc.inbox_rev.clone() });
    let opts = StartOptions { session_db: svc.session_path.path().to_path_buf(), pair_phone };
    let task = tokio::spawn(async move { adapter.start(opts, sink).await });
    let r = match tokio::time::timeout(START_TIMEOUT, task).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Err(AdapterError::temporary(if e.is_panic() {
            "the WhatsApp client panicked while starting".to_string()
        } else {
            e.to_string()
        })),
        Err(_) => Err(AdapterError::temporary("the WhatsApp client did not start within 60 seconds")),
    };
    match &r {
        Ok((s, _)) => {
            svc.session.send_replace(Some(s.clone()));
            svc.status.send_modify(|st| {
                st.process = "running".into();
                st.since_start();
            });
        }
        Err(e) => {
            tracing::warn!(error = %e.message, "WhatsApp client could not start");
            svc.status.send_modify(|st| {
                st.process = "failed".into();
                st.last_error = Some(e.message.clone());
            });
        }
    }
    r
}

impl WaStatus {
    fn since_start(&mut self) {
        self.last_error = None;
    }
}

/// Sends, read receipts and media downloads. Never touches the supervisor's
/// state; a slow send never delays a command or a sale.
async fn io_worker(svc: Arc<WhatsAppService>) {
    let mut rx = svc.session.subscribe();
    loop {
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            _ = svc.poke.notified() => {}
            _ = rx.changed() => {}
        }
        let session = rx.borrow().clone();
        let Some(session) = session else { continue };
        if !session.connected() {
            continue;
        }
        if let Err(e) = io_cycle(&svc, &session).await {
            tracing::debug!(error = %e.message, "WhatsApp I/O cycle");
        }
    }
}

async fn io_cycle(svc: &Arc<WhatsAppService>, session: &Arc<dyn AdapterSession>) -> AppResult<()> {
    let core = svc.core.clone();
    let jobs = blocking({
        let c = core.clone();
        move || c.wa_claim_due(10)
    })
    .await?;
    for j in jobs {
        let res = match &j.document_path {
            Some(p) => {
                let p = PathBuf::from(p);
                match tokio::fs::read(&p).await {
                    Ok(bytes) => {
                        let name = j.document_name.clone().unwrap_or_else(|| "document.pdf".into());
                        let caption = (!j.body.is_empty()).then_some(j.body.as_str());
                        tokio::time::timeout(
                            SEND_TIMEOUT,
                            session.send_document(&j.message_id, &j.to_phone, bytes, &name, "application/pdf", caption),
                        )
                        .await
                    }
                    Err(e) => Ok(Err(AdapterError::permanent(format!("The document is no longer on this computer: {e}")))),
                }
            }
            None => tokio::time::timeout(SEND_TIMEOUT, session.send_text(&j.message_id, &j.to_phone, &j.body)).await,
        };
        let outcome = match res {
            Ok(Ok(id)) => Ok(Some(id)),
            Ok(Err(e)) => Err((e.message, e.permanent)),
            Err(_) => Err(("WhatsApp did not confirm the message within 60 seconds.".to_string(), false)),
        };
        let ok = outcome.is_ok();
        let err = outcome.as_ref().err().map(|e| e.0.clone());
        let (c, id) = (core.clone(), j.message_id.clone());
        blocking(move || c.wa_send_result(&id, outcome)).await?;
        svc.status.send_modify(|s| {
            if ok {
                s.last_send_at = Some(now());
                s.last_send_error = None;
            } else {
                s.last_send_error = err;
            }
        });
    }
    // Read receipts, grouped by chat.
    let reads = blocking({
        let c = core.clone();
        move || c.wa_unsynced_reads()
    })
    .await?;
    if !reads.is_empty() {
        let mut by_chat: std::collections::BTreeMap<String, Vec<(i64, String)>> = Default::default();
        for (seq, chat, id) in reads {
            by_chat.entry(chat).or_default().push((seq, id));
        }
        for (chat, items) in by_chat {
            let ids: Vec<String> = items.iter().map(|i| i.1.clone()).collect();
            if session.mark_read(&chat, &ids).await.is_ok() {
                let seqs: Vec<i64> = items.iter().map(|i| i.0).collect();
                let c = core.clone();
                blocking(move || c.wa_reads_synced(&seqs)).await?;
            }
        }
    }
    // Inbound media (payment screenshots, documents).
    let media = blocking({
        let c = core.clone();
        move || c.wa_media_pending(3)
    })
    .await?;
    for m in media {
        let r = tokio::time::timeout(SEND_TIMEOUT, session.download(&m.media_ref)).await;
        let c = core.clone();
        match r {
            Ok(Ok(bytes)) => {
                let mime = m.mime.clone();
                blocking(move || c.wa_media_saved(m.seq, &bytes, mime.as_deref()).map(|_| ())).await?;
            }
            Ok(Err(e)) => blocking(move || c.wa_media_failed(m.seq, &e.message, e.permanent)).await?,
            Err(_) => blocking(move || c.wa_media_failed(m.seq, "download timed out", false)).await?,
        }
    }
    Ok(())
}
