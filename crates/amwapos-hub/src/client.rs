//! Terminal-side hub client and sync cycle.

use std::sync::Arc;
use std::time::Duration;

use amwapos_core::sync::{self, Change, HubInfo, PairRequest, PairResponse, PullRequest, PullResponse, PushRequest, PushResponse};
use amwapos_core::{AppCore, AppError, AppResult, ErrorCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

fn http() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(60))
        .no_proxy()
        .build()
        .map_err(|e| AppError::new(ErrorCode::Sync, format!("HTTP client error: {e}")))
}

pub fn normalize_url(url: &str) -> AppResult<String> {
    let mut u = url.trim().trim_end_matches('/').to_string();
    if u.is_empty() {
        return Err(AppError::validation("Enter the hub address."));
    }
    if !u.starts_with("http://") && !u.starts_with("https://") {
        u = format!("http://{u}");
    }
    let host = u.split("://").nth(1).unwrap_or("");
    if !host.contains(':') {
        u = format!("{u}:{}", sync::DEFAULT_PORT);
    }
    if u.chars().any(|c| c.is_whitespace()) || u.len() > 200 {
        return Err(AppError::validation("The hub address is not valid."));
    }
    Ok(u)
}

async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> AppResult<T> {
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| AppError::new(ErrorCode::Sync, format!("Hub connection dropped: {e}")))?;
    if !status.is_success() {
        if let Ok(e) = serde_json::from_slice::<AppError>(&bytes) {
            return Err(e);
        }
        return Err(AppError::new(ErrorCode::Sync, format!("Hub returned HTTP {status}.")));
    }
    serde_json::from_slice(&bytes).map_err(|e| AppError::new(ErrorCode::Sync, format!("Unexpected hub response: {e}")))
}

fn unreachable(base: &str, e: reqwest::Error) -> AppError {
    let mut err = AppError::new(
        ErrorCode::Sync,
        format!("The hub at {base} is not reachable ({e}). Local selling continues; changes will sync when the hub is back."),
    );
    err.retryable = true;
    err
}

pub async fn hub_info(base: &str) -> AppResult<HubInfo> {
    let base = normalize_url(base)?;
    let r = http()?.get(format!("{base}/info")).send().await.map_err(|e| unreachable(&base, e))?;
    decode(r).await
}

pub async fn pair(base: &str, req: &PairRequest) -> AppResult<PairResponse> {
    let base = normalize_url(base)?;
    let r = http()?.post(format!("{base}/pair")).json(req).send().await.map_err(|e| unreachable(&base, e))?;
    decode(r).await
}

pub struct HubClient {
    http: reqwest::Client,
    base: String,
    device_id: String,
    key: String,
}

impl HubClient {
    pub fn new(base: &str, device_id: &str, key: &str) -> AppResult<Self> {
        Ok(Self { http: http()?, base: normalize_url(base)?, device_id: device_id.into(), key: key.into() })
    }

    async fn call<B: Serialize, R: DeserializeOwned>(&self, method: &str, path: &str, body: Option<&B>) -> AppResult<R> {
        let bytes = match body {
            Some(b) => serde_json::to_vec(b).map_err(|e| AppError::internal(e.to_string()))?,
            None => vec![],
        };
        let ts = chrono::Utc::now().timestamp_millis();
        let nonce = ulid::Ulid::new().to_string();
        let sig = sync::hmac_hex(&self.key, sync::signing_string(method, path, ts, &nonce, &bytes).as_bytes());
        let url = format!("{}{path}", self.base);
        let rb = match method {
            "GET" => self.http.get(&url),
            _ => self.http.post(&url).header("content-type", "application/json").body(bytes),
        };
        let resp = rb
            .header("x-amw-device", &self.device_id)
            .header("x-amw-ts", ts.to_string())
            .header("x-amw-nonce", &nonce)
            .header("x-amw-signature", sig)
            .send()
            .await
            .map_err(|e| unreachable(&self.base, e))?;
        let status = resp.status();
        let rsig = resp.headers().get("x-amw-signature").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let data = resp.bytes().await.map_err(|e| AppError::new(ErrorCode::Sync, format!("Hub connection dropped: {e}")))?;
        if !status.is_success() {
            if let Ok(e) = serde_json::from_slice::<AppError>(&data) {
                return Err(e);
            }
            return Err(AppError::new(ErrorCode::Sync, format!("Hub returned HTTP {status}.")));
        }
        // The response must be signed with our device key: a rogue host on the LAN cannot forge it.
        let expect = sync::hmac_hex(&self.key, sync::signing_string("RESPONSE", path, ts, &nonce, &data).as_bytes());
        if !sync::verify_hex_eq(&expect, &rsig) {
            return Err(AppError::new(ErrorCode::Sync, "The hub's response signature is invalid. Synchronization stopped for safety."));
        }
        serde_json::from_slice(&data).map_err(|e| AppError::new(ErrorCode::Sync, format!("Unexpected hub response: {e}")))
    }

    pub async fn push(&self, changes: Vec<Change>) -> AppResult<PushResponse> {
        self.call("POST", "/sync/push", Some(&PushRequest { device_id: self.device_id.clone(), changes })).await
    }
    pub async fn pull(&self, since: i64) -> AppResult<PullResponse> {
        self.call("POST", "/sync/pull", Some(&PullRequest { device_id: self.device_id.clone(), since_seq: since, limit: Some(500) })).await
    }
    pub async fn heartbeat(&self, hb: &sync::Heartbeat) -> AppResult<serde_json::Value> {
        self.call("POST", "/heartbeat", Some(hb)).await
    }
    pub async fn info(&self) -> AppResult<HubInfo> {
        self.call::<(), HubInfo>("GET", "/sync/status", None).await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CycleReport {
    pub pushed: usize,
    pub rejected: usize,
    pub pulled: usize,
    pub pull_failed: usize,
    pub duration_ms: u128,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::internal(format!("worker failed: {e}")))?
}

/// One full terminal sync cycle: push local changes, pull hub changes, heartbeat.
pub async fn sync_cycle(core: Arc<AppCore>) -> AppResult<CycleReport> {
    let started = std::time::Instant::now();
    let d = core.require_device()?;
    if d.mode != "terminal" {
        return Err(AppError::conflict("Only terminals synchronize with a hub."));
    }
    let ss = core.terminal_sync_settings()?;
    if let Some(reason) = ss.blocked_reason {
        return Err(AppError::new(ErrorCode::Sync, reason));
    }
    let key = core.terminal_device_key()?;
    let client = HubClient::new(&ss.hub_url, &d.device_id, &key)?;
    let result: AppResult<CycleReport> = async {
        let info = client.info().await?;
        if info.schema_version != amwapos_core::db::latest_schema_version() {
            return Err(AppError::new(
                ErrorCode::Sync,
                format!(
                    "Version mismatch: hub runs AMWAPOS {} (schema {}), this terminal runs {} (schema {}). Update both to the same version.",
                    info.app_version,
                    info.schema_version,
                    amwapos_core::audit::APP_VERSION,
                    amwapos_core::db::latest_schema_version()
                ),
            ));
        }
        let mut report = CycleReport::default();
        loop {
            let c2 = core.clone();
            let (changes, scanned) = blocking(move || c2.terminal_collect_push(1000)).await?;
            if changes.is_empty() {
                let c2 = core.clone();
                blocking(move || c2.terminal_record_push(scanned, &PushResponse { accepted: 0, rejected: vec![], up_to_seq: 0 }, &[])).await?;
                break;
            }
            let n = changes.len();
            let resp = client.push(changes.clone()).await?;
            report.pushed += resp.accepted;
            report.rejected += resp.rejected.len();
            let c2 = core.clone();
            blocking(move || c2.terminal_record_push(scanned, &resp, &changes)).await?;
            if n < 1000 {
                break;
            }
        }
        loop {
            let since = core.terminal_sync_settings()?.pull_cursor;
            let resp = client.pull(since).await?;
            let more = resp.has_more;
            let c2 = core.clone();
            let (a, f) = blocking(move || c2.terminal_apply_pull(&resp)).await?;
            report.pulled += a;
            report.pull_failed += f;
            if !more {
                break;
            }
        }
        let c2 = core.clone();
        let hb = blocking(move || c2.terminal_heartbeat()).await?;
        client.heartbeat(&hb).await?;
        report.duration_ms = started.elapsed().as_millis();
        let c2 = core.clone();
        let ver = info.app_version.clone();
        blocking(move || c2.terminal_note_result(None, Some(ver))).await?;
        Ok(report)
    }
    .await;
    if let Err(e) = &result {
        let c2 = core.clone();
        let msg = e.message.clone();
        let _ = blocking(move || c2.terminal_note_result(Some(msg), None)).await;
    }
    result
}

/// Background loop for terminals. Errors are recorded and retried with backoff.
pub fn spawn_sync_loop(core: Arc<AppCore>, interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = interval;
        loop {
            let is_terminal = core.device().map(|d| d.mode == "terminal").unwrap_or(false);
            if is_terminal {
                match sync_cycle(core.clone()).await {
                    Ok(r) => {
                        if r.pushed + r.pulled > 0 {
                            tracing::info!(pushed = r.pushed, pulled = r.pulled, ms = r.duration_ms as u64, "sync cycle");
                        }
                        backoff = interval;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e.message, "sync cycle failed");
                        backoff = (backoff * 2).min(Duration::from_secs(60));
                    }
                }
            }
            tokio::time::sleep(backoff).await;
        }
    })
}
