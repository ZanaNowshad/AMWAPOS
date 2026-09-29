//! WhatsApp AI orders worker: interprets new inbound messages into order
//! drafts on its own task (never on the WhatsApp receive loop). Rules always
//! run; the configured AI provider is asked only for messages the rules left
//! unresolved, and its reply is validated and applied only if the
//! conversation has not changed meanwhile. AI failures leave the rules result.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use amwapos_core::{AppCore, AppError, AppResult};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

pub struct OrdersWorker {
    core: Arc<AppCore>,
    task: Mutex<Option<JoinHandle<()>>>,
    pub poke: Arc<Notify>,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::internal(format!("worker failed: {e}")))?
}

impl OrdersWorker {
    pub fn new(core: Arc<AppCore>) -> Arc<Self> {
        Arc::new(Self { core, task: Mutex::new(None), poke: Arc::new(Notify::new()) })
    }

    /// Start when the module is on. Idempotent; restarts a dead task.
    pub fn ensure(self: &Arc<Self>) {
        let on = self.core.features().map(|f| f.is_on("orders.whatsapp_ai")).unwrap_or(false);
        let mut g = self.task.lock().unwrap();
        let alive = g.as_ref().map(|h| !h.is_finished()).unwrap_or(false);
        if on && !alive {
            *g = Some(tokio::spawn(run(self.clone())));
        }
    }

    /// One pass (tests and the loop): interpret, then AI help where wanted.
    pub async fn tick(&self) -> AppResult<usize> {
        let c = self.core.clone();
        let done = blocking(move || c.wa_orders_process(25)).await?;
        let n = done.len();
        for p in done.into_iter().filter(|p| p.ai_wanted) {
            let c = self.core.clone();
            let seq = p.seq;
            let Ok(Some((turn, revision, allowed))) = blocking(move || c.wa_order_ai_turn(seq)).await else { continue };
            let model = format!("{}:{}", turn.settings.provider, turn.settings.model_id);
            match crate::ai_client::complete_once(&turn).await {
                Ok(reply) => {
                    let Some(v) = crate::ai_client::json_object(&reply) else {
                        tracing::info!(seq, "AI order reading was not JSON; rules result kept");
                        continue;
                    };
                    let c = self.core.clone();
                    if let Err(e) = blocking(move || c.wa_order_apply_ai(seq, revision, &allowed, &v, &model)).await {
                        tracing::info!(seq, error = %e.message, "AI order reading not applied");
                    }
                }
                Err(e) => tracing::info!(seq, error = %e.message, "AI unavailable for WhatsApp orders; rules result kept"),
            }
        }
        Ok(n)
    }
}

async fn run(w: Arc<OrdersWorker>) {
    loop {
        let c = w.core.clone();
        let on = blocking(move || c.features().map(|f| f.is_on("orders.whatsapp_ai"))).await.unwrap_or(false);
        if !on {
            return;
        }
        if let Err(e) = w.tick().await {
            tracing::warn!(error = %e.message, "WhatsApp orders pass failed");
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(3)) => {}
            _ = w.poke.notified() => {}
        }
    }
}
