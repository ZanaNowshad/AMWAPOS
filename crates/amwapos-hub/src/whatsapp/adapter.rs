//! The boundary between AMWAPOS and a WhatsApp client library.
//!
//! Nothing outside this module family knows which library is behind it:
//! `RustWhatsAppAdapter` wraps the unofficial `whatsapp-rust` crate and
//! `FakeAdapter` stands in for tests. The UI never sees adapter types; it
//! reads AMWAPOS tables and the service status through commands.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::messaging::Inbound;
use async_trait::async_trait;

/// Resolves when the client's run loop has ended (stop, logout, crash).
pub type ExitFuture = Pin<Box<dyn Future<Output = Exit> + Send>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// The run loop returned (graceful stop, logout or a closed connection).
    Ended,
    /// The client task panicked or failed; the supervisor restarts it.
    Failed(String),
}

/// Lifecycle events reported by the adapter. Delivered synchronously to the
/// sink, which only updates in-memory status (no I/O).
#[derive(Debug, Clone, PartialEq)]
pub enum AdapterEvent {
    Qr {
        code: String,
        valid_for: Duration,
    },
    PairCode {
        code: String,
        valid_for: Duration,
    },
    PairCodeError(String),
    /// All QR codes of this pairing attempt expired.
    QrExhausted,
    Paired {
        account: String,
    },
    Connected {
        account: Option<String>,
    },
    Disconnected(String),
    LoggedOut(String),
    TemporaryBan {
        reason: String,
        expires_s: u64,
    },
    /// Another client took over this session.
    StreamReplaced,
    ClientOutdated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterError {
    pub message: String,
    /// Retrying will not help (bad number, bad input, not a WhatsApp user).
    pub permanent: bool,
}

impl AdapterError {
    pub fn temporary(m: impl Into<String>) -> Self {
        Self { message: m.into(), permanent: false }
    }
    pub fn permanent(m: impl Into<String>) -> Self {
        Self { message: m.into(), permanent: true }
    }
}

pub struct StartOptions {
    /// WhatsApp session store (its own SQLite file; see `session.rs`).
    pub session_db: PathBuf,
    /// Link with an 8-character pairing code for this number instead of QR.
    pub pair_phone: Option<String>,
}

/// Where the adapter reports to. Implemented by the service.
#[async_trait]
pub trait AdapterSink: Send + Sync {
    /// Status only; must return quickly.
    fn event(&self, e: AdapterEvent);
    /// Commit inbound messages to AMWAPOS tables. The adapter acknowledges the
    /// messages to WhatsApp only after this returns `Ok`; on `Err` WhatsApp
    /// delivers them again.
    async fn inbound(&self, batch: Vec<Inbound>) -> Result<(), String>;
}

#[async_trait]
pub trait WhatsAppAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    /// Open the session store and start the client on its own task.
    async fn start(&self, opts: StartOptions, sink: Arc<dyn AdapterSink>) -> Result<(Arc<dyn AdapterSession>, ExitFuture), AdapterError>;
}

/// A running client. All methods are safe to call from any task.
#[async_trait]
pub trait AdapterSession: Send + Sync {
    fn connected(&self) -> bool;
    fn logged_in(&self) -> bool;
    /// `send_id` is AMWAPOS' outbox id; the adapter derives the WhatsApp
    /// message id from it, so a retry of the same message is the same message.
    async fn send_text(&self, send_id: &str, to_phone: &str, text: &str) -> Result<String, AdapterError>;
    async fn send_document(
        &self,
        send_id: &str,
        to_phone: &str,
        bytes: Vec<u8>,
        file_name: &str,
        mime: &str,
        caption: Option<&str>,
    ) -> Result<String, AdapterError>;
    async fn mark_read(&self, chat: &str, ids: &[String]) -> Result<(), AdapterError>;
    async fn download(&self, media_ref: &str) -> Result<Vec<u8>, AdapterError>;
    /// Disconnect gracefully; the exit future resolves afterwards.
    async fn stop(&self);
    /// Unlink this computer from the phone.
    async fn logout(&self);
}

/// `+97333334444` → `97333334444@s.whatsapp.net`.
pub fn phone_to_jid(phone: &str) -> Result<String, AdapterError> {
    let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
    if !(8..=15).contains(&digits.len()) {
        return Err(AdapterError::permanent("The WhatsApp number is not valid."));
    }
    Ok(format!("{digits}@s.whatsapp.net"))
}

/// WhatsApp message id derived from the outbox id: stable across retries.
pub fn wa_message_id(send_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let h = hex::encode(Sha256::digest(format!("amwapos-wa:{send_id}")));
    format!("3EB0{}", h[..18].to_ascii_uppercase())
}
