//! Order event sync to other systems (core: `amwapos_core::order_sync`).
//!
//! Each target is an `OrderSink` adapter. The worker drains every enabled
//! target in feed order, one event at a time, and reports each outcome to the
//! core, which keeps the cursor, the retry wait and the parked events. A new
//! kind of system is one more `OrderSink`; the order logic never changes.
//!
//! Adapters:
//! * `webhook`: `POST` of the event JSON with `X-Amwapos-Event`,
//!   `X-Amwapos-Event-Id` and `X-Amwapos-Signature: sha256=<hex HMAC of the
//!   body>` (secret in the credential store). 2xx and 409 (the receiver
//!   already has this or a newer version) count as delivered; 408, 425, 429
//!   and 5xx are retried (honouring `Retry-After`); other 4xx are refused.
//!   Redirects are not followed.
//! * `csv`: one line per event appended to a file (header written on
//!   creation). An event already in the file's tail is not written twice. A
//!   file locked by another program (Excel) is retried.
//!
//! Runs on the hub or a standalone till, never on a terminal (terminals'
//! orders reach the hub by sync and produce the hub's events).

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amwapos_core::order_sync::{hmac_sha256, secret_slot, SyncJob, SyncOutcome};
use amwapos_core::{AppCore, AppError, AppResult};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

const TICK: Duration = Duration::from_secs(3);
/// Events per target per pass (others get their turn in between).
const BATCH: usize = 20;
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

#[async_trait]
pub trait OrderSink: Send + Sync {
    async fn deliver(&self, event_id: &str, payload: &Value) -> SyncOutcome;
}

pub struct WebhookSink {
    url: String,
    secret: Option<String>,
    http: reqwest::Client,
}

impl WebhookSink {
    pub fn new(url: &str, secret: Option<String>) -> AppResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("AMWAPOS/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| AppError::internal(e.to_string()))?;
        Ok(Self { url: url.to_string(), secret, http })
    }
}

/// How an HTTP status counts for delivery.
pub fn classify_status(status: u16, retry_after: Option<i64>) -> SyncOutcome {
    match status {
        200..=299 | 409 => SyncOutcome::Delivered,
        408 | 425 | 429 | 500..=599 => SyncOutcome::Retry { error: format!("The receiver answered HTTP {status}."), after_s: retry_after },
        _ => SyncOutcome::Refused { error: format!("The receiver refused the event (HTTP {status}).") },
    }
}

#[async_trait]
impl OrderSink for WebhookSink {
    async fn deliver(&self, event_id: &str, payload: &Value) -> SyncOutcome {
        let body = payload.to_string();
        let mut req = self
            .http
            .post(&self.url)
            .header("content-type", "application/json")
            .header("x-amwapos-event", payload["kind"].as_str().unwrap_or(""))
            .header("x-amwapos-event-id", event_id);
        if let Some(s) = &self.secret {
            req = req.header("x-amwapos-signature", format!("sha256={}", hex::encode(hmac_sha256(s.as_bytes(), body.as_bytes()))));
        }
        match req.body(body).send().await {
            Ok(r) => {
                let after = r.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<i64>().ok());
                classify_status(r.status().as_u16(), after)
            }
            Err(e) => {
                let why = if e.is_timeout() {
                    "The receiver did not answer in time.".to_string()
                } else {
                    format!("Could not reach the receiver: {e}")
                };
                SyncOutcome::Retry { error: why, after_s: None }
            }
        }
    }
}

pub struct CsvSink {
    path: PathBuf,
}

pub const CSV_HEADER: &str = "event_id,seq,occurred_at,kind,version,order_number,order_status,payment_state,channel,phone,customer_id,delivery_number,delivery_status,delivery_outcome,receipt_number,total_minor,items";

fn csv_cell(s: &str) -> String {
    // Spreadsheet formula injection is neutralised like every other AMWAPOS export.
    let s = amwapos_core::validate::csv_safe_cell(s.to_string());
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

pub fn csv_line(event_id: &str, p: &Value) -> String {
    let o = &p["order"];
    let d = &p["delivery"];
    let items: Vec<String> = o["lines"]
        .as_array()
        .map(|ls| {
            ls.iter()
                .map(|l| {
                    format!(
                        "{} x {}",
                        amwapos_core::money::format_qty(l["qty_milli"].as_i64().unwrap_or(0)),
                        l["sku"].as_str().or(l["name"].as_str()).unwrap_or("")
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let s = |v: &Value| match v {
        Value::Null => String::new(),
        Value::String(x) => x.clone(),
        other => other.to_string(),
    };
    let total = if o["sale"]["total_minor"].is_i64() { &o["sale"]["total_minor"] } else { &o["total_minor"] };
    [
        event_id.to_string(),
        s(&p["seq"]),
        s(&p["occurred_at"]),
        s(&p["kind"]),
        s(&p["version"]),
        s(&o["order_number"]),
        s(&o["status"]),
        s(&o["payment_state"]),
        s(&o["channel"]),
        s(&o["phone"]),
        s(&o["customer_id"]),
        s(&d["delivery_number"]),
        s(&d["status"]),
        s(&d["outcome"]),
        s(&o["sale"]["receipt_number"]),
        s(total),
        items.join("; "),
    ]
    .iter()
    .map(|c| csv_cell(c))
    .collect::<Vec<_>>()
    .join(",")
}

impl CsvSink {
    pub fn new(path: &str) -> Self {
        Self { path: PathBuf::from(path) }
    }

    fn append(&self, event_id: &str, payload: &Value) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::OpenOptions::new().read(true).append(true).create(true).open(&self.path)?;
        let len = f.metadata()?.len();
        if len == 0 {
            f.write_all(CSV_HEADER.as_bytes())?;
            f.write_all(b"\r\n")?;
        } else {
            // Written already (a crash after writing, before recording): not twice.
            let tail = len.min(16 * 1024);
            f.seek(SeekFrom::End(-(tail as i64)))?;
            let mut buf = String::new();
            let _ = f.read_to_string(&mut buf);
            if buf.contains(event_id) {
                return Ok(());
            }
        }
        f.write_all(csv_line(event_id, payload).as_bytes())?;
        f.write_all(b"\r\n")?;
        f.flush()
    }
}

#[async_trait]
impl OrderSink for CsvSink {
    async fn deliver(&self, event_id: &str, payload: &Value) -> SyncOutcome {
        let (sink, id, p) = (CsvSink { path: self.path.clone() }, event_id.to_string(), payload.clone());
        match tokio::task::spawn_blocking(move || sink.append(&id, &p)).await {
            Ok(Ok(())) => SyncOutcome::Delivered,
            Ok(Err(e)) => SyncOutcome::Retry {
                error: format!("Could not write {}: {e} (is it open in another program?)", self.path.display()),
                after_s: None,
            },
            Err(e) => SyncOutcome::Retry { error: format!("CSV writer failed: {e}"), after_s: None },
        }
    }
}

/// The adapter for a target. New systems are added here.
pub fn build(core: &AppCore, job: &SyncJob) -> AppResult<Box<dyn OrderSink>> {
    match job.kind.as_str() {
        "webhook" => {
            let url = job.config["url"].as_str().ok_or_else(|| AppError::validation("The webhook has no address."))?;
            let secret = core.secrets.get(&secret_slot(&job.sink_id))?;
            Ok(Box::new(WebhookSink::new(url, secret)?))
        }
        "csv" => Ok(Box::new(CsvSink::new(job.config["path"].as_str().unwrap_or_default()))),
        other => Err(AppError::validation(format!("Unknown sync target kind '{other}'."))),
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::internal(format!("worker failed: {e}")))?
}

pub struct OrderSyncWorker {
    core: Arc<AppCore>,
    task: Mutex<Option<JoinHandle<()>>>,
    pub poke: Arc<Notify>,
}

impl OrderSyncWorker {
    pub fn new(core: Arc<AppCore>) -> Arc<Self> {
        Arc::new(Self { core, task: Mutex::new(None), poke: Arc::new(Notify::new()) })
    }

    /// Start on the hub or a standalone till. Idempotent; restarts a dead task.
    pub fn ensure(self: &Arc<Self>) {
        let mode = self.core.device().map(|d| d.mode);
        if !matches!(mode.as_deref(), Some("hub" | "standalone")) {
            return;
        }
        let mut g = self.task.lock().unwrap();
        if g.as_ref().map(|h| h.is_finished()).unwrap_or(true) {
            *g = Some(tokio::spawn(run(self.clone())));
        }
    }

    /// One pass over every due target (tests and the loop). Returns events delivered.
    pub async fn tick(&self) -> AppResult<usize> {
        let c = self.core.clone();
        let due = blocking(move || c.order_sync_due()).await?;
        let mut delivered = 0;
        for sink_id in due {
            for _ in 0..BATCH {
                let (c, id) = (self.core.clone(), sink_id.clone());
                let Some(job) = blocking(move || c.order_sync_next(&id)).await? else { break };
                let outcome = if job.payload["missing"] == true {
                    SyncOutcome::Delivered
                } else {
                    match build(&self.core, &job) {
                        Ok(sink) => sink.deliver(&job.event_id, &job.payload).await,
                        Err(e) => SyncOutcome::Refused { error: e.message },
                    }
                };
                let ok = outcome == SyncOutcome::Delivered;
                if !ok {
                    tracing::info!(sink = %job.sink_id, seq = job.event_seq, outcome = ?outcome, "order sync not delivered");
                }
                let c = self.core.clone();
                blocking(move || c.order_sync_complete(&job, outcome)).await?;
                if !ok {
                    break;
                }
                delivered += 1;
            }
        }
        Ok(delivered)
    }

    /// "Send a test": the latest event (or a sample) to one target, now,
    /// without touching its cursor or counters.
    pub async fn test(&self, token: &str, sink_id: &str) -> AppResult<Value> {
        let (c, t, id) = (self.core.clone(), token.to_string(), sink_id.to_string());
        let job = blocking(move || c.order_sync_test_job(&t, &id)).await?;
        let sink = build(&self.core, &job)?;
        Ok(match sink.deliver(&job.event_id, &job.payload).await {
            SyncOutcome::Delivered => json!({ "ok": true }),
            SyncOutcome::Retry { error, .. } | SyncOutcome::Refused { error } => json!({ "ok": false, "error": error }),
        })
    }
}

async fn run(w: Arc<OrderSyncWorker>) {
    let mut passes: u64 = 0;
    loop {
        if let Err(e) = w.tick().await {
            tracing::warn!(error = %e.message, "order sync pass failed");
        }
        passes += 1;
        // Old events everyone has taken are removed about once an hour.
        if passes % 1200 == 1 {
            let c = w.core.clone();
            if let Ok(Ok(n)) = tokio::task::spawn_blocking(move || c.order_events_prune()).await {
                if n > 0 {
                    tracing::info!(removed = n, "old order events removed");
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            _ = w.poke.notified() => {}
        }
    }
}
