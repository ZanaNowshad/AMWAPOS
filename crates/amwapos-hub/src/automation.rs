//! Background loop for the WhatsApp and OCR modules. It keeps the sidecar
//! running while either module is on, pulls inbound WhatsApp messages into
//! the database, sends queued messages, pushes read marks and runs OCR on
//! imported images. It stops the sidecar when both modules are switched off.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::{AppCore, AppError, AppResult, ErrorCode};
use serde_json::{json, Value};

use crate::sidecar::Sidecar;

const TICK: Duration = Duration::from_secs(4);
const SHORT: Duration = Duration::from_secs(10);
const SEND: Duration = Duration::from_secs(60);
const OCR: Duration = Duration::from_secs(180);

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::internal(format!("worker failed: {e}")))?
}

pub async fn run(core: Arc<AppCore>, sidecar: Arc<Sidecar>, autostart: Arc<AtomicBool>) {
    let mut backoff = TICK;
    loop {
        let f = match blocking({
            let c = core.clone();
            move || c.features()
        })
        .await
        {
            Ok(f) => f,
            Err(_) => {
                tokio::time::sleep(backoff).await;
                continue;
            }
        };
        if !(f.is_on("whatsapp") || f.is_on("ocr")) {
            sidecar.stop().await;
            return;
        }
        if let Err(e) = sidecar.ensure(&core).await {
            tracing::warn!(error = %e.message, "sidecar unavailable");
            backoff = (backoff * 2).min(Duration::from_secs(120));
            tokio::time::sleep(backoff).await;
            continue;
        }
        backoff = TICK;
        if f.is_on("whatsapp") {
            if let Err(e) = whatsapp_cycle(&core, &sidecar, &autostart).await {
                tracing::debug!(error = %e.message, "whatsapp cycle");
            }
        }
        if f.is_on("ocr") {
            if let Err(e) = ocr_cycle(&core, &sidecar).await {
                tracing::debug!(error = %e.message, "ocr cycle");
            }
        }
        tokio::time::sleep(TICK).await;
    }
}

async fn whatsapp_cycle(core: &Arc<AppCore>, sidecar: &Sidecar, autostart: &AtomicBool) -> AppResult<()> {
    let st = sidecar.call("/whatsapp/status", None, SHORT).await?;
    let state = st.get("state").and_then(|v| v.as_str()).unwrap_or("stopped");
    if state == "stopped" && st.get("linked").and_then(|v| v.as_bool()).unwrap_or(false) && autostart.load(Ordering::Relaxed) {
        sidecar.call("/whatsapp/start", Some(json!({})), SHORT).await?;
    }
    // Inbound: resume from our cursor; a reset sidecar inbox restarts at 0
    // (rows are de-duplicated by chat + message id).
    let mut cursor = blocking({
        let c = core.clone();
        move || c.wa_cursor()
    })
    .await?;
    let inbox_seq = st.get("inbox_seq").and_then(|v| v.as_i64()).unwrap_or(0);
    if inbox_seq < cursor {
        cursor = 0;
    }
    if inbox_seq > cursor {
        let page = sidecar.call(&format!("/whatsapp/messages?after={cursor}&limit=200"), None, SHORT).await?;
        let msgs: Vec<Value> = page.get("messages").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let next = page.get("next").and_then(|v| v.as_i64()).unwrap_or(cursor);
        let c = core.clone();
        blocking(move || {
            let images = c.wa_ingest(&msgs)?;
            c.pr_from_inbox(&images)?;
            c.wa_set_cursor(next)
        })
        .await?;
    }
    if state != "ready" {
        return Ok(());
    }
    // Outbound.
    let jobs = blocking({
        let c = core.clone();
        move || c.wa_claim_due(10)
    })
    .await?;
    for j in jobs {
        let mut body = json!({ "client_id": j.message_id, "to": j.to_phone, "text": j.body });
        if let Some(p) = &j.document_path {
            body["document"] = json!({ "path": p, "file_name": j.document_name, "mime": "application/pdf" });
        }
        let outcome = match sidecar.call("/whatsapp/send", Some(body), SEND).await {
            Ok(v) => Ok(v.get("message_id").and_then(|m| m.as_str()).map(|s| s.to_string())),
            Err(e) => {
                let permanent = matches!(e.code, ErrorCode::Validation | ErrorCode::NotFound);
                Err((e.message, permanent))
            }
        };
        let c = core.clone();
        let id = j.message_id.clone();
        blocking(move || c.wa_send_result(&id, outcome)).await?;
    }
    // Read marks.
    let reads = blocking({
        let c = core.clone();
        move || c.wa_unsynced_reads()
    })
    .await?;
    if !reads.is_empty() {
        let keys: Vec<Value> = reads.iter().map(|(_, chat, id)| json!({ "chat": chat, "id": id })).collect();
        sidecar.call("/whatsapp/mark-read", Some(json!({ "keys": keys })), SHORT).await?;
        let seqs: Vec<i64> = reads.iter().map(|r| r.0).collect();
        let c = core.clone();
        blocking(move || c.wa_reads_synced(&seqs)).await?;
    }
    Ok(())
}

async fn ocr_cycle(core: &Arc<AppCore>, sidecar: &Sidecar) -> AppResult<()> {
    let jobs = blocking({
        let c = core.clone();
        move || c.ocr_pending(2)
    })
    .await?;
    for j in jobs {
        // Payment screenshots are often Arabic; supplier invoices mostly English.
        let langs = if j.kind == "payment" { json!(["eng", "ara"]) } else { json!(["eng"]) };
        let res = sidecar.call("/ocr/recognize", Some(json!({ "path": j.path, "langs": langs })), OCR).await;
        let outcome = match res {
            Ok(v) => Ok((
                v.get("text").and_then(|t| t.as_str()).unwrap_or_default().to_string(),
                v.get("confidence").and_then(|c| c.as_i64()).unwrap_or(0),
            )),
            // The sidecar is down or OCR models are missing: leave the job queued.
            Err(e)
                if e.details
                    .as_ref()
                    .and_then(|d| d.get("kind"))
                    .and_then(|k| k.as_str())
                    .is_some_and(|k| k == "sidecar_unavailable" || k == "ocr_unavailable") =>
            {
                return Err(e)
            }
            Err(e) => Err(e.message),
        };
        let c = core.clone();
        let (kind, id) = (j.kind, j.id.clone());
        blocking(move || c.ocr_result(kind, &id, outcome)).await?;
    }
    Ok(())
}
