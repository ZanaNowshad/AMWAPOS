//! Adapter for `whatsapp-rust` 0.7.0 (pinned exactly in Cargo.toml), an
//! unofficial WhatsApp Web client. Written against the 0.7 API as published
//! (`Bot::builder`, `SqliteStore`, event handlers, the inbound durability
//! hook), not against older samples.
//!
//! The session store is the crate's own SQLite backend on the file the
//! service passes in (`<data>/whatsapp/session.db`); AMWAPOS' ledger
//! connections never open it.

use std::sync::Arc;
use std::time::Duration;

use amwapos_core::messaging::Inbound;
use async_trait::async_trait;
use base64::Engine;
use serde_json::json;
use whatsapp_rust::download::DownloadParams;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::prelude::*;
use whatsapp_rust::upload::UploadOptions;
use whatsapp_rust::InboundDurabilityHook;

use super::adapter::*;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Default)]
pub struct RustWhatsAppAdapter;

impl RustWhatsAppAdapter {
    pub fn new() -> Self {
        Self
    }
}

/// Commits each inbound batch through the sink before the crate acks it.
struct Durable {
    sink: Arc<dyn AdapterSink>,
}

#[async_trait]
impl InboundDurabilityHook for Durable {
    async fn on_messages(&self, _client: Arc<Client>, batch: &[InboundMessage]) -> anyhow::Result<()> {
        let rows: Vec<Inbound> = batch.iter().filter_map(to_inbound).collect();
        if rows.is_empty() {
            return Ok(());
        }
        self.sink.inbound(rows).await.map_err(|e| anyhow::anyhow!(e))
    }
}

fn is_pn(j: &Jid) -> bool {
    j.to_string().ends_with("@s.whatsapp.net")
}

/// One-to-one messages from other people only (no groups, status, channels
/// or our own messages).
fn to_inbound(m: &InboundMessage) -> Option<Inbound> {
    let info = &m.info;
    let src = &info.source;
    if src.is_from_me || src.is_group {
        return None;
    }
    let chat = src.chat.to_string();
    if !(chat.ends_with("@s.whatsapp.net") || chat.ends_with("@lid")) {
        return None;
    }
    let pn = [Some(&src.sender), src.sender_alt.as_ref(), Some(&src.chat)].into_iter().flatten().find(|j| is_pn(j)).map(|j| {
        // Drop the device part: `973...:12@s.whatsapp.net` → `973...@s.whatsapp.net`.
        let s = j.to_string();
        match s.split_once('@') {
            Some((u, d)) => format!("{}@{d}", u.split(':').next().unwrap_or(u)),
            None => s,
        }
    });
    let base = m.message.get_base_message();
    let mut row = Inbound {
        wa_id: info.id.to_string(),
        // Thread by phone number when known, so replies and sends line up.
        chat: pn.clone().unwrap_or(chat),
        sender_pn: pn,
        push_name: Some(info.push_name.to_string()).filter(|n| !n.is_empty()),
        ts: info.timestamp.timestamp(),
        kind: "other".into(),
        ..Default::default()
    };
    if let Some(img) = base.image_message.as_option() {
        row.kind = "image".into();
        row.caption = img.caption.clone();
        row.media_mime = img.mimetype.clone();
        row.media_ref = media_ref("image", &img.direct_path, &img.media_key, &img.file_sha256, &img.file_enc_sha256, img.file_length);
    } else if let Some(doc) = base.document_message.as_option() {
        row.kind = "document".into();
        row.caption = doc.caption.clone().or_else(|| doc.file_name.clone());
        row.media_mime = doc.mimetype.clone();
        row.media_ref = media_ref("document", &doc.direct_path, &doc.media_key, &doc.file_sha256, &doc.file_enc_sha256, doc.file_length);
    } else if let Some(t) = m.message.text_content() {
        row.kind = "text".into();
        row.text = Some(t.to_string());
    }
    Some(row)
}

fn media_ref(
    kind: &str,
    direct_path: &Option<String>,
    key: &Option<Vec<u8>>,
    sha: &Option<Vec<u8>>,
    enc_sha: &Option<Vec<u8>>,
    len: Option<u64>,
) -> Option<String> {
    let (Some(p), Some(k), Some(s), Some(e)) = (direct_path, key, sha, enc_sha) else { return None };
    Some(
        json!({ "kind": kind, "direct_path": p, "media_key": B64.encode(k), "file_sha256": B64.encode(s),
                "file_enc_sha256": B64.encode(e), "file_length": len.unwrap_or(0) })
        .to_string(),
    )
}

#[async_trait]
impl WhatsAppAdapter for RustWhatsAppAdapter {
    fn name(&self) -> &'static str {
        "whatsapp-rust 0.7.0"
    }

    async fn start(&self, opts: StartOptions, sink: Arc<dyn AdapterSink>) -> Result<(Arc<dyn AdapterSession>, ExitFuture), AdapterError> {
        if let Some(d) = opts.session_db.parent() {
            std::fs::create_dir_all(d).map_err(|e| AdapterError::temporary(format!("Could not create the WhatsApp folder: {e}")))?;
        }
        let url = opts.session_db.to_string_lossy().to_string();
        let store =
            SqliteStore::new(&url).await.map_err(|e| AdapterError::temporary(format!("Could not open the WhatsApp session store: {e}")))?;
        let ev_sink = sink.clone();
        let kinds = [
            EventKind::PairingQrCode,
            EventKind::PairingCode,
            EventKind::PairingCodeError,
            EventKind::PairingQrCodesExhausted,
            EventKind::PairSuccess,
            EventKind::Connected,
            EventKind::Disconnected,
            EventKind::LoggedOut,
            EventKind::TemporaryBan,
            EventKind::StreamReplaced,
            EventKind::ClientOutdated,
        ];
        let mut builder = Bot::builder()
            .with_backend(store)
            .skip_history_sync()
            .with_inbound_durability_hook(Durable { sink: sink.clone() })
            .on_event_for(&kinds, move |event, client| {
                let e = match &*event {
                    Event::PairingQrCode(q) => Some(AdapterEvent::Qr { code: q.code.clone(), valid_for: q.timeout }),
                    Event::PairingCode(p) => Some(AdapterEvent::PairCode { code: p.code.clone(), valid_for: p.timeout }),
                    Event::PairingCodeError(e) => Some(AdapterEvent::PairCodeError(format!("{e:?}"))),
                    Event::PairingQrCodesExhausted(_) => Some(AdapterEvent::QrExhausted),
                    Event::PairSuccess(p) => Some(AdapterEvent::Paired { account: p.id.to_string() }),
                    Event::Connected(_) => Some(AdapterEvent::Connected { account: client.pn().map(|j| j.to_string()) }),
                    Event::Disconnected(d) => Some(AdapterEvent::Disconnected(format!("{:?}", d.reason))),
                    Event::LoggedOut(l) => Some(AdapterEvent::LoggedOut(format!("{:?}", l.reason))),
                    Event::TemporaryBan(b) => Some(AdapterEvent::TemporaryBan {
                        reason: format!("{:?}", b.code),
                        expires_s: b.expire.num_seconds().max(0) as u64,
                    }),
                    Event::StreamReplaced(_) => Some(AdapterEvent::StreamReplaced),
                    Event::ClientOutdated(_) => Some(AdapterEvent::ClientOutdated),
                    _ => None,
                };
                if let Some(e) = e {
                    ev_sink.event(e);
                }
                async {}
            });
        if let Some(phone) = opts.pair_phone {
            let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
            builder = builder.with_pair_code(PairCodeOptions { phone_number: digits, ..Default::default() });
        }
        let bot = builder.build().await.map_err(|e| AdapterError::temporary(format!("WhatsApp client setup failed: {e}")))?;
        let handle = bot.spawn();
        let client = handle.client();
        // Dropping the handle aborts the client; the exit future owns it.
        let exit: ExitFuture = Box::pin(async move {
            handle.await;
            Exit::Ended
        });
        Ok((Arc::new(RustSession { client }), exit))
    }
}

struct RustSession {
    client: Arc<Client>,
}

fn jid(phone_or_jid: &str) -> Result<Jid, AdapterError> {
    let s = if phone_or_jid.contains('@') { phone_or_jid.to_string() } else { phone_to_jid(phone_or_jid)? };
    s.parse::<Jid>().map_err(|e| AdapterError::permanent(format!("Invalid WhatsApp address: {e}")))
}

fn send_err(e: SendError) -> AdapterError {
    match e {
        SendError::InvalidRequest(m) => AdapterError::permanent(m),
        other => AdapterError::temporary(other.to_string()),
    }
}

#[async_trait]
impl AdapterSession for RustSession {
    fn connected(&self) -> bool {
        self.client.is_connected()
    }

    fn logged_in(&self) -> bool {
        self.client.is_logged_in()
    }

    async fn send_text(&self, send_id: &str, to_phone: &str, text: &str) -> Result<String, AdapterError> {
        let to = jid(to_phone)?;
        let opts = SendOptions::default().with_message_id(wa_message_id(send_id));
        let r = self.client.send_message_with_options(to, wa::Message::text(text), opts).await.map_err(send_err)?;
        Ok(r.message_id)
    }

    async fn send_document(
        &self,
        send_id: &str,
        to_phone: &str,
        bytes: Vec<u8>,
        file_name: &str,
        mime: &str,
        caption: Option<&str>,
    ) -> Result<String, AdapterError> {
        let to = jid(to_phone)?;
        let up = self
            .client
            .upload(bytes, whatsapp_rust::download::MediaType::Document, UploadOptions::default())
            .await
            .map_err(|e| AdapterError::temporary(format!("Upload failed: {e}")))?;
        let msg = wa::Message {
            document_message: MessageField::some(wa::message::DocumentMessage {
                url: Some(up.url),
                direct_path: Some(up.direct_path),
                media_key: Some(up.media_key.to_vec()),
                file_sha256: Some(up.file_sha256.to_vec()),
                file_enc_sha256: Some(up.file_enc_sha256.to_vec()),
                file_length: Some(up.file_length),
                media_key_timestamp: Some(up.media_key_timestamp),
                mimetype: Some(mime.to_string()),
                file_name: Some(file_name.to_string()),
                title: Some(file_name.to_string()),
                caption: caption.filter(|c| !c.is_empty()).map(str::to_string),
                ..Default::default()
            }),
            ..Default::default()
        };
        let opts = SendOptions::default().with_message_id(wa_message_id(send_id));
        let r = self.client.send_message_with_options(to, msg, opts).await.map_err(send_err)?;
        Ok(r.message_id)
    }

    async fn mark_read(&self, chat: &str, ids: &[String]) -> Result<(), AdapterError> {
        let chat = jid(chat)?;
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        self.client.mark_as_read(&chat, None, &refs).await.map_err(|e| AdapterError::temporary(e.to_string()))
    }

    async fn download(&self, media_ref: &str) -> Result<Vec<u8>, AdapterError> {
        let v: serde_json::Value = serde_json::from_str(media_ref).map_err(|_| AdapterError::permanent("bad media reference"))?;
        let b = |k: &str| {
            v.get(k).and_then(|x| x.as_str()).and_then(|s| B64.decode(s).ok()).ok_or_else(|| AdapterError::permanent("bad media reference"))
        };
        let kind = match v.get("kind").and_then(|k| k.as_str()) {
            Some("image") => whatsapp_rust::download::MediaType::Image,
            _ => whatsapp_rust::download::MediaType::Document,
        };
        let params = DownloadParams::encrypted(
            v.get("direct_path").and_then(|x| x.as_str()).unwrap_or_default(),
            &b("media_key")?,
            &b("file_sha256")?,
            &b("file_enc_sha256")?,
            v.get("file_length").and_then(|x| x.as_u64()).unwrap_or(0),
            kind,
        );
        tokio::time::timeout(Duration::from_secs(90), self.client.download_from_params(&params))
            .await
            .map_err(|_| AdapterError::temporary("media download timed out"))?
            .map_err(|e| AdapterError::temporary(format!("media download failed: {e}")))
    }

    async fn stop(&self) {
        self.client.disconnect().await;
    }

    async fn logout(&self) {
        self.client.logout().await;
    }
}
