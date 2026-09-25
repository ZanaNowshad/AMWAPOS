//! In-memory adapter for tests (and for running with the flag on but no
//! network). It behaves like WhatsApp where AMWAPOS depends on it: QR pairing,
//! a persistent session file, de-duplication by message id, inbound batches
//! that must be committed before they are acknowledged, and media downloads.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amwapos_core::messaging::Inbound;
use async_trait::async_trait;
use tokio::sync::{oneshot, Notify};

use super::adapter::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeSent {
    pub wa_id: String,
    pub to: String,
    pub text: String,
    pub document: Option<(String, usize)>,
}

#[derive(Default)]
pub struct FakeState {
    /// Messages WhatsApp accepted, in order, de-duplicated by `wa_id`.
    pub sent: Mutex<Vec<FakeSent>>,
    /// Fail the next N sends with a temporary error.
    pub fail_sends: AtomicU32,
    pub starts: AtomicU32,
    pub read_marks: Mutex<Vec<(String, Vec<String>)>>,
    pub media: Mutex<HashMap<String, Vec<u8>>>,
    /// Inbound batches the sink refused (would be redelivered by WhatsApp).
    pub refused: AtomicU32,
    connected: AtomicBool,
    sink: Mutex<Option<Arc<dyn AdapterSink>>>,
    stop: Notify,
    crash: Mutex<Option<oneshot::Sender<()>>>,
    logged_out: AtomicBool,
}

#[derive(Clone, Default)]
pub struct FakeAdapter {
    pub state: Arc<FakeState>,
}

impl FakeAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The phone scanned the QR code.
    pub fn scan(&self) {
        let sink = self.state.sink.lock().unwrap().clone();
        if let Some(s) = sink {
            self.state.connected.store(true, Ordering::SeqCst);
            s.event(AdapterEvent::Paired { account: "97330000000@s.whatsapp.net".into() });
            s.event(AdapterEvent::Connected { account: Some("97330000000@s.whatsapp.net".into()) });
        }
    }

    /// WhatsApp delivers inbound messages. Returns whether they were committed.
    pub async fn deliver(&self, batch: Vec<Inbound>) -> bool {
        let sink = self.state.sink.lock().unwrap().clone();
        match sink {
            Some(s) => match s.inbound(batch).await {
                Ok(()) => true,
                Err(_) => {
                    self.state.refused.fetch_add(1, Ordering::SeqCst);
                    false
                }
            },
            None => false,
        }
    }

    /// The client task panics (simulates a bug in the WhatsApp library).
    pub fn panic_now(&self) {
        if let Some(tx) = self.state.crash.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }

    /// The phone unlinked this computer.
    pub fn remote_logout(&self) {
        let sink = self.state.sink.lock().unwrap().clone();
        self.state.logged_out.store(true, Ordering::SeqCst);
        self.state.connected.store(false, Ordering::SeqCst);
        if let Some(s) = sink {
            s.event(AdapterEvent::LoggedOut("unlinked from the phone".into()));
        }
        self.state.stop.notify_one();
    }

    pub fn sent(&self) -> Vec<FakeSent> {
        self.state.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl WhatsAppAdapter for FakeAdapter {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn start(&self, opts: StartOptions, sink: Arc<dyn AdapterSink>) -> Result<(Arc<dyn AdapterSession>, ExitFuture), AdapterError> {
        self.state.starts.fetch_add(1, Ordering::SeqCst);
        self.state.logged_out.store(false, Ordering::SeqCst);
        // Like the real store: a session file in the WhatsApp folder.
        let paired = std::fs::metadata(&opts.session_db).map(|m| m.len() > 0).unwrap_or(false);
        if let Some(d) = opts.session_db.parent() {
            std::fs::create_dir_all(d).map_err(|e| AdapterError::temporary(e.to_string()))?;
        }
        if !paired {
            std::fs::write(&opts.session_db, b"fake-session").map_err(|e| AdapterError::temporary(e.to_string()))?;
        }
        *self.state.sink.lock().unwrap() = Some(sink.clone());
        if paired {
            self.state.connected.store(true, Ordering::SeqCst);
            sink.event(AdapterEvent::Connected { account: Some("97330000000@s.whatsapp.net".into()) });
        } else if let Some(p) = opts.pair_phone {
            sink.event(AdapterEvent::PairCode {
                code: format!("FAKE{}", &p[p.len().saturating_sub(4)..]),
                valid_for: Duration::from_secs(180),
            });
        } else {
            sink.event(AdapterEvent::Qr { code: "2@fake-qr-payload".into(), valid_for: Duration::from_secs(60) });
        }
        let (crash_tx, crash_rx) = oneshot::channel::<()>();
        *self.state.crash.lock().unwrap() = Some(crash_tx);
        // The "client task": ends on stop/logout, panics on crash.
        let state = self.state.clone();
        let task = tokio::spawn(async move {
            tokio::select! {
                _ = state.stop.notified() => {}
                r = crash_rx => if r.is_ok() { panic!("fake WhatsApp client crashed") },
            }
        });
        let exit: ExitFuture = Box::pin(async move {
            match task.await {
                Ok(()) => Exit::Ended,
                Err(e) => Exit::Failed(if e.is_panic() { "the WhatsApp client panicked".into() } else { e.to_string() }),
            }
        });
        Ok((Arc::new(FakeSession { state: self.state.clone() }), exit))
    }
}

struct FakeSession {
    state: Arc<FakeState>,
}

impl FakeSession {
    fn accept(&self, send_id: &str, to: &str, text: &str, document: Option<(String, usize)>) -> Result<String, AdapterError> {
        if !self.state.connected.load(Ordering::SeqCst) {
            return Err(AdapterError::temporary("not connected"));
        }
        let jid = phone_to_jid(to)?;
        if self.state.fail_sends.load(Ordering::SeqCst) > 0 {
            self.state.fail_sends.fetch_sub(1, Ordering::SeqCst);
            return Err(AdapterError::temporary("network timeout"));
        }
        let wa_id = wa_message_id(send_id);
        let mut sent = self.state.sent.lock().unwrap();
        if !sent.iter().any(|m| m.wa_id == wa_id) {
            sent.push(FakeSent { wa_id: wa_id.clone(), to: jid, text: text.to_string(), document });
        }
        Ok(wa_id)
    }
}

#[async_trait]
impl AdapterSession for FakeSession {
    fn connected(&self) -> bool {
        self.state.connected.load(Ordering::SeqCst)
    }
    fn logged_in(&self) -> bool {
        !self.state.logged_out.load(Ordering::SeqCst)
    }
    async fn send_text(&self, send_id: &str, to_phone: &str, text: &str) -> Result<String, AdapterError> {
        self.accept(send_id, to_phone, text, None)
    }
    async fn send_document(
        &self,
        send_id: &str,
        to_phone: &str,
        bytes: Vec<u8>,
        file_name: &str,
        _mime: &str,
        caption: Option<&str>,
    ) -> Result<String, AdapterError> {
        self.accept(send_id, to_phone, caption.unwrap_or_default(), Some((file_name.to_string(), bytes.len())))
    }
    async fn mark_read(&self, chat: &str, ids: &[String]) -> Result<(), AdapterError> {
        self.state.read_marks.lock().unwrap().push((chat.to_string(), ids.to_vec()));
        Ok(())
    }
    async fn download(&self, media_ref: &str) -> Result<Vec<u8>, AdapterError> {
        self.state.media.lock().unwrap().get(media_ref).cloned().ok_or_else(|| AdapterError::permanent("media expired"))
    }
    async fn stop(&self) {
        self.state.connected.store(false, Ordering::SeqCst);
        self.state.stop.notify_one();
    }
    async fn logout(&self) {
        self.state.logged_out.store(true, Ordering::SeqCst);
        self.state.connected.store(false, Ordering::SeqCst);
        let sink = self.state.sink.lock().unwrap().clone();
        if let Some(s) = sink {
            s.event(AdapterEvent::LoggedOut("logged out from AMWAPOS".into()));
        }
        self.state.stop.notify_one();
    }
}
