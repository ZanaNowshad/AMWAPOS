//! WhatsApp messaging records: templates (English/Arabic), the outbox with
//! send-once semantics, the inbox and PDF receipt copies.
//!
//! The core never talks to WhatsApp itself and never waits for it. Sales,
//! refunds and shifts only ever insert an outbox row after they commit. The
//! WhatsApp service (hub crate) claims queued rows, sends them through its
//! adapter and records the result here; inbound messages are committed here
//! before anything is shown. Inbound text is stored as data; it is never
//! interpreted as an instruction. WhatsApp's own session keys are not in this
//! database (see the hub's `whatsapp::session`).

use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::auth::Session;
use crate::customers::normalize_phone;
use crate::error::ErrorCode;
use crate::error::{AppError, AppResult};
use crate::idempotency;
use crate::ids::new_id;
use crate::money::format_money;
use crate::service::AppCore;
use crate::settings::{self, MessageTemplate, WhatsAppSettings};
use crate::time;
use crate::validate;

const MAX_ATTEMPTS: i64 = 5;

#[derive(Debug, Clone, Deserialize)]
pub struct QueueRequest {
    pub operation_id: String,
    /// receipt | dispatch | reminder | text
    pub kind: String,
    #[serde(default)]
    pub to_phone: Option<String>,
    #[serde(default)]
    pub customer_id: Option<String>,
    #[serde(default)]
    pub sale_id: Option<String>,
    #[serde(default)]
    pub delivery_id: Option<String>,
    /// Payment review, for `payment_ack`.
    #[serde(default)]
    pub review_id: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
    /// Message text for `text`, optional caption for `document`.
    #[serde(default)]
    pub text: Option<String>,
    /// `document`: a PDF sent from the UI as base64, and its file name.
    #[serde(default)]
    pub document_b64: Option<String>,
    #[serde(default)]
    pub document_name: Option<String>,
}

/// Message kinds and the feature flag each one needs.
pub const MESSAGE_KINDS: &[(&str, &str)] = &[
    ("receipt", "whatsapp.send_receipts"),
    ("dispatch", "whatsapp.delivery_notices"),
    ("delivered", "whatsapp.delivery_notices"),
    ("reminder", "whatsapp.enabled"),
    ("payment_ack", "whatsapp.enabled"),
    ("text", "whatsapp.enabled"),
    ("document", "whatsapp.enabled"),
];

/// An inbound message as received by the adapter, before it is stored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Inbound {
    pub wa_id: String,
    pub chat: String,
    /// Phone JID of the sender when known (`...@s.whatsapp.net`).
    pub sender_pn: Option<String>,
    pub push_name: Option<String>,
    /// Unix seconds.
    pub ts: i64,
    /// text | image | document | other
    pub kind: String,
    pub text: Option<String>,
    pub caption: Option<String>,
    pub media_mime: Option<String>,
    /// Adapter-specific download parameters (JSON); downloaded later.
    pub media_ref: Option<String>,
}

/// Inbound media waiting to be downloaded by the WhatsApp service.
#[derive(Debug, Clone, Serialize)]
pub struct MediaJob {
    pub seq: i64,
    pub media_ref: String,
    pub mime: Option<String>,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutboxRow {
    pub message_id: String,
    pub operation_id: String,
    pub kind: String,
    pub to_phone: String,
    pub customer_id: Option<String>,
    pub customer_name: Option<String>,
    pub sale_id: Option<String>,
    pub delivery_id: Option<String>,
    pub review_id: Option<String>,
    pub lang: String,
    pub body: String,
    pub document_name: Option<String>,
    pub status: String,
    pub wa_message_id: Option<String>,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub created_by_name: Option<String>,
    pub created_at: String,
    pub sent_at: Option<String>,
}

/// A message claimed for sending (runtime → sidecar).
#[derive(Debug, Clone, Serialize)]
pub struct OutboundJob {
    pub message_id: String,
    pub to_phone: String,
    pub body: String,
    pub document_path: Option<String>,
    pub document_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InboxRow {
    pub seq: i64,
    pub chat: String,
    pub phone: Option<String>,
    pub push_name: Option<String>,
    pub customer_id: Option<String>,
    pub customer_name: Option<String>,
    pub received_at: String,
    pub kind: String,
    pub body: Option<String>,
    pub caption: Option<String>,
    pub has_media: bool,
    pub media_mime: Option<String>,
    pub read_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Conversation {
    pub chat: String,
    pub phone: Option<String>,
    pub name: Option<String>,
    pub customer_id: Option<String>,
    pub last_at: String,
    pub last_text: Option<String>,
    pub unread: i64,
}

/// Fill `{placeholders}`. Values are plain text; braces inside values are
/// not expanded again.
pub fn render_template(t: &str, vars: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(t.len() + 64);
    let mut rest = t;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('}') {
            Some(j) if after[..j].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                let key = &after[..j];
                match vars.iter().find(|(k, _)| *k == key) {
                    Some((_, v)) => out.push_str(v),
                    None => {
                        out.push('{');
                        out.push_str(key);
                        out.push('}');
                    }
                }
                rest = &after[j + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `97333334444@s.whatsapp.net` → `+97333334444`.
pub fn phone_from_jid(jid: &str) -> Option<String> {
    let user = jid.split('@').next()?.split(':').next()?;
    if jid.ends_with("@s.whatsapp.net") && user.chars().all(|c| c.is_ascii_digit()) && (8..=15).contains(&user.len()) {
        Some(format!("+{user}"))
    } else {
        None
    }
}

fn pick(t: &MessageTemplate, lang: &str) -> String {
    if lang == "ar" {
        t.ar.clone()
    } else {
        t.en.clone()
    }
}

fn load_outbox(c: &Connection, id: &str) -> AppResult<OutboxRow> {
    c.query_row(
        "SELECT o.message_id, o.kind, o.to_phone, o.customer_id, cu.name, o.sale_id, o.delivery_id, o.lang, o.body, o.document_name,
                o.status, o.attempts, o.last_error, u.display_name, o.created_at, o.sent_at, o.operation_id, o.review_id, o.wa_message_id
         FROM wa_outbox o LEFT JOIN customers cu ON cu.customer_id=o.customer_id LEFT JOIN users u ON u.user_id=o.created_by
         WHERE o.message_id=?1",
        [id],
        |r| {
            Ok(OutboxRow {
                message_id: r.get(0)?,
                kind: r.get(1)?,
                to_phone: r.get(2)?,
                customer_id: r.get(3)?,
                customer_name: r.get(4)?,
                sale_id: r.get(5)?,
                delivery_id: r.get(6)?,
                lang: r.get(7)?,
                body: r.get(8)?,
                document_name: r.get(9)?,
                status: r.get(10)?,
                attempts: r.get(11)?,
                last_error: r.get(12)?,
                created_by_name: r.get(13)?,
                created_at: r.get(14)?,
                sent_at: r.get(15)?,
                operation_id: r.get(16)?,
                review_id: r.get(17)?,
                wa_message_id: r.get(18)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("Message"))
}

fn customer_phone(c: &Connection, customer_id: &str) -> AppResult<Option<String>> {
    Ok(c.query_row("SELECT COALESCE(NULLIF(whatsapp,''), phone) FROM customers WHERE customer_id=?1", [customer_id], |r| r.get(0))
        .optional()?
        .flatten())
}

fn find_customer_by_phone(c: &Connection, phone: &str) -> AppResult<Option<(String, String)>> {
    Ok(c.query_row(
        "SELECT customer_id, name FROM customers WHERE whatsapp=?1 OR phone=?1 ORDER BY active DESC, updated_at DESC LIMIT 1",
        [phone],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

impl AppCore {
    fn receipts_dir(&self) -> PathBuf {
        self.data_dir.join("receipts")
    }

    /// Write the PDF copy of a sale or refund receipt and return its path.
    /// Re-rendering the same receipt overwrites the same file.
    pub fn receipt_pdf_write(&self, kind: &str, ref_id: &str) -> AppResult<PathBuf> {
        let (doc, number) = self.db.read(|c| {
            let (doc, number) = match kind {
                "sale" => (
                    crate::receipt::sale_receipt(c, ref_id, None)?,
                    c.query_row("SELECT receipt_number FROM sales WHERE sale_id=?1", [ref_id], |r| r.get::<_, String>(0))?,
                ),
                "refund" => (
                    crate::receipt::refund_receipt(c, ref_id, None)?,
                    c.query_row("SELECT refund_receipt_number FROM refunds WHERE refund_id=?1", [ref_id], |r| r.get::<_, String>(0))?,
                ),
                _ => return Err(AppError::validation("Unknown receipt kind.")),
            };
            Ok((doc, number))
        })?;
        let safe: String = number.chars().map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' { ch } else { '_' }).collect();
        let dir = self.receipts_dir().join(&time::now_str()[..7]);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{safe}.pdf"));
        let tmp = path.with_extension("pdf.tmp");
        std::fs::write(&tmp, crate::pdf::bitmap_pdf(&doc.to_bitmap(), &format!("Receipt {number}")))?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// Post-commit hook: save the PDF copy when the module is on. A failure is
    /// logged and never affects the committed sale or refund.
    pub fn receipt_pdf_after_commit(&self, kind: &str, ref_id: &str) {
        if !self.features().map(|f| f.is_on("pdf_receipts")).unwrap_or(false) {
            return;
        }
        if let Err(e) = self.receipt_pdf_write(kind, ref_id) {
            tracing::warn!(kind, ref_id, error = %e.message, "PDF receipt could not be saved; the sale is unaffected");
        }
    }

    /// Check permission and module for a WhatsApp session action (start,
    /// stop, logout, pair code, session backup) and write the audit entry. The
    /// WhatsApp service performs the action afterwards.
    pub fn wa_link_action(&self, token: &str, action: &str) -> AppResult<()> {
        let s = self.session(token)?;
        s.require("whatsapp.manage")?;
        if action != "stop" && action != "logout" {
            self.require_feature("whatsapp.enabled")?;
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            audit::record(tx, &actor, &format!("whatsapp.{action}"), "whatsapp", None, None, None)?;
            Ok(())
        })
    }

    /// Download a receipt PDF (any user who can view sales).
    pub fn receipt_pdf(&self, token: &str, kind: &str, ref_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !(s.has("sales.view") || s.has("pos.reprint")) {
            s.require("sales.view")?;
        }
        let id = validate::id(ref_id, "Receipt")?;
        let path = self.receipt_pdf_write(kind, &id)?;
        let bytes = std::fs::read(&path)?;
        Ok(json!({
            "file_name": path.file_name().map(|f| f.to_string_lossy().to_string()),
            "path": path.to_string_lossy(),
            "base64": crate::ids::b64(&bytes),
        }))
    }

    /// Queue a WhatsApp message. Idempotent on `operation_id`: the same key
    /// with the same payload returns the first result; the same key with a
    /// different payload is refused. Only inserts a row; sending happens in
    /// the WhatsApp service, so callers never wait for WhatsApp.
    pub fn wa_queue(&self, token: &str, req: QueueRequest) -> AppResult<OutboxRow> {
        let s = self.session(token)?;
        let flag = MESSAGE_KINDS
            .iter()
            .find(|(k, _)| *k == req.kind)
            .map(|(_, f)| *f)
            .ok_or_else(|| AppError::validation("Unknown message type."))?;
        match req.kind.as_str() {
            "receipt" | "dispatch" | "delivered" => {
                if !s.has("whatsapp.send") {
                    s.require("whatsapp.manage")?;
                }
            }
            _ => s.require("whatsapp.manage")?,
        }
        self.require_feature(flag)?;
        self.wa_queue_as(&s, req)
    }

    /// Queue on behalf of an already-authorised session (post-commit hooks).
    pub(crate) fn wa_queue_as(&self, s: &Session, req: QueueRequest) -> AppResult<OutboxRow> {
        let op = validate::id(&req.operation_id, "Operation")?;
        // Uploaded PDF: validated and hashed before the payload hash.
        let upload = match req.kind.as_str() {
            "document" => {
                let b64 = req.document_b64.as_deref().unwrap_or_default();
                if b64.len() > 22 * 1024 * 1024 {
                    return Err(AppError::validation("The PDF is larger than 16 MB."));
                }
                let bytes = crate::ids::b64_decode(b64).ok_or_else(|| AppError::validation("The document could not be read."))?;
                if !bytes.starts_with(b"%PDF-") {
                    return Err(AppError::validation("Only PDF documents can be sent."));
                }
                let name = req.document_name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("document.pdf");
                let name: String =
                    name.chars().map(|c| if c.is_alphanumeric() || " ._-()".contains(c) { c } else { '_' }).take(120).collect();
                let name = if name.to_ascii_lowercase().ends_with(".pdf") { name } else { format!("{name}.pdf") };
                let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
                Some((bytes, name, sha))
            }
            _ => None,
        };
        let hash = idempotency::payload_hash(
            "whatsapp.send",
            &json!({
                "kind": req.kind, "to": req.to_phone, "customer": req.customer_id, "sale": req.sale_id,
                "delivery": req.delivery_id, "review": req.review_id, "lang": req.lang, "text": req.text,
                "document_sha256": upload.as_ref().map(|u| u.2.clone()),
            }),
        )?;
        if let Some((existing, stored)) = self.db.read(|c| {
            Ok(c.query_row("SELECT message_id, payload_hash FROM wa_outbox WHERE operation_id=?1", [&op], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?)
        })? {
            if !stored.is_empty() && stored != hash {
                return Err(AppError::new(
                    ErrorCode::IdempotencyMismatch,
                    "This send key was already used for a different message. Nothing was sent.",
                )
                .with_details(json!({ "operation_id": op, "message_id": existing })));
            }
            return self.db.read(|c| load_outbox(c, &existing));
        }
        let wa: WhatsAppSettings = self.db.read(|c| settings::get(c, settings::KEY_WHATSAPP))?;
        // Receipt PDF is rendered before the transaction (it only reads committed data).
        let pdf = match (req.kind.as_str(), &req.sale_id) {
            ("receipt", Some(sale)) if wa.attach_pdf => Some(self.receipt_pdf_write("sale", &validate::id(sale, "Sale")?)?),
            _ => None,
        };
        let id = new_id();
        let doc = match (&pdf, upload) {
            (Some(p), _) => Some((p.clone(), p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default())),
            (None, Some((bytes, name, _))) => {
                let dir = self.data_dir.join("whatsapp-outbox");
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(format!("{id}.pdf"));
                std::fs::write(&path, &bytes)?;
                Some((path, name))
            }
            _ => None,
        };
        let actor = self.actor(s, None);
        self.db.write(|tx| {
            let (currency, digits) = self.currency(tx)?;
            let business: String = tx.query_row("SELECT name FROM business LIMIT 1", [], |r| r.get(0)).optional()?.unwrap_or_default();
            let tz: String = tx.query_row("SELECT timezone FROM business LIMIT 1", [], |r| r.get(0)).optional()?.unwrap_or_else(|| "Asia/Bahrain".into());
            let mut customer_id = req.customer_id.clone().filter(|x| !x.is_empty());
            let mut phone = match req.to_phone.as_deref().filter(|x| !x.trim().is_empty()) {
                Some(p) => normalize_phone(p)?,
                None => None,
            };
            let mut vars: Vec<(&str, String)> = vec![("business", business)];
            let mut body_text = String::new();
            let (mut sale_id, mut delivery_id, mut review_id) = (None::<String>, None::<String>, None::<String>);
            match req.kind.as_str() {
                "receipt" => {
                    let sid = validate::id(req.sale_id.as_deref().unwrap_or(""), "Sale")?;
                    let (receipt, total, at, cust): (String, i64, String, Option<String>) = tx
                        .query_row("SELECT receipt_number, total_minor, completed_at, customer_id FROM sales WHERE sale_id=?1", [&sid], |r| {
                            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                        })
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Sale"))?;
                    customer_id = customer_id.or(cust);
                    vars.push(("receipt", receipt));
                    vars.push(("total", format_money(total, &currency, digits)));
                    vars.push(("date", time::display(&at, &tz)));
                    sale_id = Some(sid);
                }
                "dispatch" | "delivered" | "reminder" => {
                    let did = validate::id(req.delivery_id.as_deref().unwrap_or(""), "Delivery")?;
                    let (number, amount, cust, dphone, pay): (String, i64, Option<String>, Option<String>, String) = tx
                        .query_row(
                            "SELECT delivery_number, amount_minor, customer_id, phone, payment_status FROM delivery_orders WHERE delivery_id=?1",
                            [&did],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                        )
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Delivery"))?;
                    customer_id = customer_id.or(cust);
                    phone = phone.or(dphone);
                    vars.push(("delivery", number));
                    vars.push(("amount", format_money(if pay == "paid" { 0 } else { amount }, &currency, digits)));
                    delivery_id = Some(did);
                }
                "payment_ack" => {
                    let rid = validate::id(req.review_id.as_deref().unwrap_or(""), "Payment review")?;
                    type Ack = (String, String, Option<i64>, Option<i64>, Option<String>, Option<String>, Option<String>);
                    let (number, status, detected, expected, rphone, cust, did): Ack = tx
                        .query_row(
                            "SELECT review_number, status, detected_minor, expected_minor, phone, customer_id, delivery_id FROM payment_reviews WHERE review_id=?1",
                            [&rid],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
                        )
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Payment review"))?;
                    // Only a payment a person confirmed is acknowledged.
                    if status != "confirmed" {
                        return Err(AppError::conflict("Only a confirmed payment can be acknowledged."));
                    }
                    customer_id = customer_id.or(cust);
                    phone = phone.or(rphone);
                    let delivery_number: Option<String> = match &did {
                        Some(d) => tx.query_row("SELECT delivery_number FROM delivery_orders WHERE delivery_id=?1", [d], |r| r.get(0)).optional()?,
                        None => None,
                    };
                    vars.push(("reference", number));
                    vars.push(("delivery", delivery_number.unwrap_or_default()));
                    vars.push(("amount", format_money(expected.or(detected).unwrap_or(0), &currency, digits)));
                    delivery_id = did;
                    review_id = Some(rid);
                }
                "document" => {
                    let caption = req.text.clone().unwrap_or_default();
                    if caption.chars().count() > 1000 {
                        return Err(AppError::validation("The caption must be at most 1000 characters."));
                    }
                    body_text = caption.trim().to_string();
                }
                _ => {
                    let text = req.text.clone().unwrap_or_default();
                    let text = text.trim();
                    if text.is_empty() || text.chars().count() > 4000 {
                        return Err(AppError::validation("Message text must be 1 to 4000 characters."));
                    }
                    body_text = text.to_string();
                }
            }
            let mut customer_name = None;
            if let Some(cid) = &customer_id {
                let cid = validate::id(cid, "Customer")?;
                customer_name = tx.query_row("SELECT name FROM customers WHERE customer_id=?1", [&cid], |r| r.get::<_, String>(0)).optional()?;
                if customer_name.is_none() {
                    return Err(AppError::not_found("Customer"));
                }
                if phone.is_none() {
                    phone = customer_phone(tx, &cid)?;
                }
                customer_id = Some(cid);
            }
            let phone = phone.ok_or_else(|| AppError::validation("Enter the customer's WhatsApp number."))?;
            vars.push(("customer", customer_name.clone().unwrap_or_default()));
            let lang = req.lang.clone().filter(|l| l == "en" || l == "ar").unwrap_or_else(|| wa.default_lang.clone());
            let body = match req.kind.as_str() {
                "receipt" => render_template(&pick(&wa.receipt, &lang), &vars),
                "dispatch" => render_template(&pick(&wa.dispatch, &lang), &vars),
                "delivered" => render_template(&pick(&wa.delivered, &lang), &vars),
                "reminder" => render_template(&pick(&wa.reminder, &lang), &vars),
                "payment_ack" => render_template(&pick(&wa.payment_ack, &lang), &vars),
                _ => body_text,
            };
            let now = time::now_str();
            let (doc_path, doc_name) = match &doc {
                Some((p, n)) => (Some(p.to_string_lossy().to_string()), Some(n.clone())),
                None => (None, None),
            };
            tx.execute(
                "INSERT INTO wa_outbox(message_id, operation_id, payload_hash, kind, to_phone, customer_id, sale_id, delivery_id, review_id, lang, body,
                    document_path, document_name, status, created_by, created_at, updated_at, next_attempt_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'queued',?14,?15,?15,?15)",
                params![id, op, hash, req.kind, phone, customer_id, sale_id, delivery_id, review_id, lang, body, doc_path, doc_name, s.user_id, now],
            )?;
            audit::record(
                tx,
                &actor,
                "whatsapp.queued",
                "whatsapp_message",
                Some(&id),
                None,
                Some(&json!({ "kind": req.kind, "to": phone, "sale_id": sale_id, "delivery_id": delivery_id, "review_id": review_id })),
            )?;
            Ok(())
        })?;
        self.db.read(|c| load_outbox(c, &id))
    }

    /// Post-commit: queue the WhatsApp receipt when `whatsapp.send_receipts`
    /// is on and the sale has a customer with a number. Never fails the sale.
    pub fn wa_after_sale(&self, s: &Session, sale_id: &str) {
        if !self.features().map(|f| f.is_on("whatsapp.send_receipts")).unwrap_or(false) {
            return;
        }
        let has_number = self
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT COALESCE(NULLIF(cu.whatsapp,''), cu.phone) FROM sales sa JOIN customers cu ON cu.customer_id=sa.customer_id WHERE sa.sale_id=?1",
                    [sale_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
            })
            .ok()
            .flatten()
            .is_some();
        if !has_number {
            return;
        }
        let req = QueueRequest {
            operation_id: format!("auto-receipt-{sale_id}"),
            kind: "receipt".into(),
            to_phone: None,
            customer_id: None,
            sale_id: Some(sale_id.to_string()),
            delivery_id: None,
            review_id: None,
            lang: None,
            text: None,
            document_b64: None,
            document_name: None,
        };
        if let Err(e) = self.wa_queue_as(s, req) {
            tracing::warn!(sale_id, error = %e.message, "WhatsApp receipt not queued; the sale is unaffected");
        }
    }

    /// Post-commit: dispatch / delivered notices (`whatsapp.delivery_notices`).
    pub fn wa_after_delivery(&self, s: &Session, delivery_id: &str, status: &str) {
        let kind = match status {
            "dispatched" => "dispatch",
            "delivered" => "delivered",
            _ => return,
        };
        if !self.features().map(|f| f.is_on("whatsapp.delivery_notices")).unwrap_or(false) {
            return;
        }
        let req = QueueRequest {
            operation_id: format!("auto-{kind}-{delivery_id}"),
            kind: kind.into(),
            to_phone: None,
            customer_id: None,
            sale_id: None,
            delivery_id: Some(delivery_id.to_string()),
            review_id: None,
            lang: None,
            text: None,
            document_b64: None,
            document_name: None,
        };
        if let Err(e) = self.wa_queue_as(s, req) {
            tracing::info!(delivery_id, error = %e.message, "WhatsApp delivery notice not queued");
        }
    }

    /// Post-commit: acknowledge a payment a person confirmed (when the
    /// WhatsApp setting `auto_payment_ack` is on and a number is known).
    pub fn wa_after_payment_confirmed(&self, s: &Session, review_id: &str) {
        let on = self.features().map(|f| f.is_on("whatsapp.enabled")).unwrap_or(false)
            && self.db.read(|c| settings::get::<WhatsAppSettings>(c, settings::KEY_WHATSAPP)).map(|w| w.auto_payment_ack).unwrap_or(false);
        if !on {
            return;
        }
        let req = QueueRequest {
            operation_id: format!("auto-payack-{review_id}"),
            kind: "payment_ack".into(),
            to_phone: None,
            customer_id: None,
            sale_id: None,
            delivery_id: None,
            review_id: Some(review_id.to_string()),
            lang: None,
            text: None,
            document_b64: None,
            document_name: None,
        };
        if let Err(e) = self.wa_queue_as(s, req) {
            tracing::info!(review_id, error = %e.message, "WhatsApp payment acknowledgement not queued");
        }
    }

    pub fn wa_outbox_list(&self, token: &str, status: Option<String>, limit: Option<i64>) -> AppResult<Vec<OutboxRow>> {
        let s = self.session(token)?;
        if !s.has("whatsapp.send") {
            s.require("whatsapp.manage")?;
        }
        let limit = validate::limit(limit, 200, 1000);
        self.db.read(|c| {
            let mut st = c.prepare("SELECT message_id FROM wa_outbox WHERE (?1 IS NULL OR status=?1) ORDER BY created_at DESC LIMIT ?2")?;
            let ids = st
                .query_map(params![status.filter(|x| !x.is_empty()), limit], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids.iter().map(|id| load_outbox(c, id)).collect()
        })
    }

    /// Cancel a queued or failed message, or put a failed one back in the queue.
    pub fn wa_outbox_action(&self, token: &str, message_id: &str, action: &str) -> AppResult<OutboxRow> {
        let s = self.session(token)?;
        s.require("whatsapp.manage")?;
        let id = validate::id(message_id, "Message")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let row = load_outbox(tx, &id)?;
            let now = time::now_str();
            match (action, row.status.as_str()) {
                ("cancel", "queued" | "failed") => {
                    tx.execute("UPDATE wa_outbox SET status='cancelled', updated_at=?2 WHERE message_id=?1", params![id, now])?;
                }
                ("retry", "failed") => {
                    tx.execute(
                        "UPDATE wa_outbox SET status='queued', attempts=0, next_attempt_at=?2, updated_at=?2 WHERE message_id=?1",
                        params![id, now],
                    )?;
                }
                _ => return Err(AppError::conflict(format!("A {} message cannot be changed that way.", row.status))),
            }
            audit::record(tx, &actor, &format!("whatsapp.{action}"), "whatsapp_message", Some(&id), None, None)?;
            Ok(())
        })?;
        self.db.read(|c| load_outbox(c, &id))
    }

    /// Runtime: claim due messages for sending. Messages stuck in `sending`
    /// (crash mid-send) are reclaimed; the WhatsApp service keeps a record of
    /// message ids it already delivered, so a reclaimed one is not re-sent.
    pub fn wa_claim_due(&self, limit: i64) -> AppResult<Vec<OutboundJob>> {
        let now = time::now_str();
        let stale = time::fmt(time::now() - chrono::Duration::minutes(2));
        self.db.write(|tx| {
            tx.execute("UPDATE wa_outbox SET status='queued' WHERE status='sending' AND updated_at < ?1", [&stale])?;
            let mut st = tx.prepare(
                "SELECT message_id, to_phone, body, document_path, document_name FROM wa_outbox
                 WHERE status='queued' AND (next_attempt_at IS NULL OR next_attempt_at <= ?1) ORDER BY created_at LIMIT ?2",
            )?;
            let jobs = st
                .query_map(params![now, limit], |r| {
                    Ok(OutboundJob {
                        message_id: r.get(0)?,
                        to_phone: r.get(1)?,
                        body: r.get(2)?,
                        document_path: r.get(3)?,
                        document_name: r.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            for j in &jobs {
                tx.execute(
                    "UPDATE wa_outbox SET status='sending', attempts=attempts+1, updated_at=?2 WHERE message_id=?1",
                    params![j.message_id, now],
                )?;
            }
            Ok(jobs)
        })
    }

    /// Runtime: record the outcome of a send. `permanent` errors (number not on
    /// WhatsApp, bad input) fail at once; others retry with back-off.
    pub fn wa_send_result(&self, message_id: &str, result: Result<Option<String>, (String, bool)>) -> AppResult<()> {
        let now = time::now_str();
        let system = audit::Actor { user_id: None, device_id: None, branch_id: None, approved_by: None };
        self.db.write(|tx| {
            match result {
                Ok(wa_id) => {
                    tx.execute(
                        "UPDATE wa_outbox SET status='sent', wa_message_id=?2, sent_at=?3, updated_at=?3, last_error=NULL WHERE message_id=?1",
                        params![message_id, wa_id, now],
                    )?;
                    audit::record(tx, &system, "whatsapp.sent", "whatsapp_message", Some(message_id), None, Some(&json!({ "wa_message_id": wa_id })))?;
                }
                Err((msg, permanent)) => {
                    let attempts: i64 =
                        tx.query_row("SELECT attempts FROM wa_outbox WHERE message_id=?1", [message_id], |r| r.get(0)).optional()?.unwrap_or(0);
                    if permanent || attempts >= MAX_ATTEMPTS {
                        tx.execute(
                            "UPDATE wa_outbox SET status='failed', last_error=?2, updated_at=?3 WHERE message_id=?1",
                            params![message_id, msg, now],
                        )?;
                        audit::record(tx, &system, "whatsapp.failed", "whatsapp_message", Some(message_id), None, Some(&json!({ "error": msg, "attempts": attempts })))?;
                    } else {
                        let next = time::fmt(time::now() + chrono::Duration::seconds(30 * (1 << attempts.clamp(0, 5))));
                        tx.execute(
                            "UPDATE wa_outbox SET status='queued', last_error=?2, next_attempt_at=?3, updated_at=?4 WHERE message_id=?1",
                            params![message_id, msg, next, now],
                        )?;
                    }
                }
            }
            Ok(())
        })
    }

    /// Runtime: commit inbound messages (the adapter calls this before the
    /// messages are acknowledged to WhatsApp and before any UI sees them).
    /// Rows are de-duplicated by chat + message id, so redelivery is harmless.
    /// Media is recorded as `pending` and downloaded later.
    pub fn wa_ingest(&self, messages: &[Inbound]) -> AppResult<usize> {
        self.db.write(|tx| {
            let mut n = 0;
            for m in messages {
                if m.wa_id.is_empty() || m.chat.is_empty() {
                    continue;
                }
                let kind = match m.kind.as_str() {
                    k @ ("text" | "image" | "document") => k,
                    _ => "other",
                };
                let phone = m.sender_pn.as_deref().and_then(phone_from_jid).or_else(|| phone_from_jid(&m.chat));
                let customer = match &phone {
                    Some(p) => find_customer_by_phone(tx, p)?,
                    None => None,
                };
                let received = chrono::DateTime::from_timestamp(m.ts, 0).map(time::fmt).unwrap_or_else(time::now_str);
                let clip = |v: &Option<String>| v.as_ref().map(|t| t.chars().take(8000).collect::<String>());
                let media_state = if m.media_ref.is_some() && matches!(kind, "image" | "document") { "pending" } else { "none" };
                n += tx.execute(
                    "INSERT OR IGNORE INTO wa_inbox(wa_id, chat, phone, push_name, received_at, kind, body, caption, media_mime, media_ref, media_state, customer_id)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    params![
                        m.wa_id,
                        m.chat,
                        phone,
                        m.push_name.as_ref().map(|t| t.chars().take(100).collect::<String>()),
                        received,
                        kind,
                        clip(&m.text),
                        clip(&m.caption),
                        m.media_mime,
                        m.media_ref,
                        media_state,
                        customer.map(|c| c.0)
                    ],
                )?;
            }
            Ok(n)
        })
    }

    /// Runtime: inbound media still to download (oldest first, bounded retries).
    pub fn wa_media_pending(&self, limit: i64) -> AppResult<Vec<MediaJob>> {
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT seq, media_ref, media_mime, kind FROM wa_inbox WHERE media_state='pending' AND media_ref IS NOT NULL AND media_attempts < 5
                 ORDER BY seq LIMIT ?1",
            )?;
            let rows = st
                .query_map([limit], |r| Ok(MediaJob { seq: r.get(0)?, media_ref: r.get(1)?, mime: r.get(2)?, kind: r.get(3)? }))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Runtime: store downloaded media inside the data folder and, for images,
    /// open a payment review when that module is on. Returns review ids.
    pub fn wa_media_saved(&self, seq: i64, bytes: &[u8], mime: Option<&str>) -> AppResult<Vec<String>> {
        let ext = match mime.unwrap_or_default() {
            "image/png" => "png",
            "image/webp" => "webp",
            "application/pdf" => "pdf",
            m if m.starts_with("image/") => "jpg",
            _ => "bin",
        };
        let dir = self.data_dir.join("whatsapp-media");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{seq}.{ext}"));
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(bytes));
        let kind: String = self.db.write(|tx| {
            tx.execute(
                "UPDATE wa_inbox SET media_path=?2, media_sha256=?3, media_state='saved', media_error=NULL, media_ref=NULL WHERE seq=?1",
                params![seq, path.to_string_lossy(), sha],
            )?;
            Ok(tx.query_row("SELECT kind FROM wa_inbox WHERE seq=?1", [seq], |r| r.get(0))?)
        })?;
        if kind == "image" {
            self.pr_from_inbox(&[seq])
        } else {
            Ok(vec![])
        }
    }

    /// Runtime: a media download failed; `permanent` stops further attempts.
    pub fn wa_media_failed(&self, seq: i64, error: &str, permanent: bool) -> AppResult<()> {
        self.db.write(|tx| {
            tx.execute(
                "UPDATE wa_inbox SET media_attempts=media_attempts+1, media_error=?2,
                    media_state=CASE WHEN ?3 OR media_attempts+1 >= 5 THEN 'failed' ELSE 'pending' END WHERE seq=?1",
                params![seq, error.chars().take(300).collect::<String>(), permanent],
            )?;
            Ok(())
        })
    }

    /// Recent send and receive references (ids and states), newest first.
    pub fn wa_recent(&self, token: &str, limit: Option<i64>) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("whatsapp.send") {
            s.require("whatsapp.manage")?;
        }
        let limit = validate::limit(limit, 50, 500);
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT message_id, operation_id, kind, status, wa_message_id, to_phone, created_at, sent_at, last_error FROM wa_outbox ORDER BY created_at DESC LIMIT ?1",
            )?;
            let sent = st
                .query_map([limit], |r| {
                    Ok(json!({ "message_id": r.get::<_, String>(0)?, "operation_id": r.get::<_, String>(1)?, "kind": r.get::<_, String>(2)?,
                        "status": r.get::<_, String>(3)?, "wa_message_id": r.get::<_, Option<String>>(4)?, "to_phone": r.get::<_, String>(5)?,
                        "created_at": r.get::<_, String>(6)?, "sent_at": r.get::<_, Option<String>>(7)?, "last_error": r.get::<_, Option<String>>(8)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut st = c.prepare(
                "SELECT seq, wa_id, chat, kind, received_at, media_state, media_error FROM wa_inbox ORDER BY seq DESC LIMIT ?1",
            )?;
            let received = st
                .query_map([limit], |r| {
                    Ok(json!({ "seq": r.get::<_, i64>(0)?, "wa_id": r.get::<_, String>(1)?, "chat": r.get::<_, String>(2)?,
                        "kind": r.get::<_, String>(3)?, "received_at": r.get::<_, String>(4)?, "media_state": r.get::<_, String>(5)?,
                        "media_error": r.get::<_, Option<String>>(6)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({ "sent": sent, "received": received }))
        })
    }

    pub fn wa_conversations(&self, token: &str) -> AppResult<Vec<Conversation>> {
        let s = self.session(token)?;
        s.require("whatsapp.manage")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT i.chat, MAX(i.phone), MAX(COALESCE(cu.name, i.push_name)), MAX(i.customer_id), MAX(i.received_at),
                        (SELECT COALESCE(body, caption, '[' || kind || ']') FROM wa_inbox x WHERE x.chat=i.chat ORDER BY seq DESC LIMIT 1),
                        SUM(CASE WHEN i.read_at IS NULL THEN 1 ELSE 0 END)
                 FROM wa_inbox i LEFT JOIN customers cu ON cu.customer_id=i.customer_id
                 GROUP BY i.chat ORDER BY MAX(i.seq) DESC LIMIT 300",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(Conversation {
                        chat: r.get(0)?,
                        phone: r.get(1)?,
                        name: r.get(2)?,
                        customer_id: r.get(3)?,
                        last_at: r.get(4)?,
                        last_text: r.get(5)?,
                        unread: r.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// One conversation: inbound messages and what we sent to that number.
    pub fn wa_thread(&self, token: &str, chat: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("whatsapp.manage")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT i.seq, i.chat, i.phone, i.push_name, i.customer_id, cu.name, i.received_at, i.kind, i.body, i.caption,
                        i.media_path IS NOT NULL, i.media_mime, i.read_at
                 FROM wa_inbox i LEFT JOIN customers cu ON cu.customer_id=i.customer_id WHERE i.chat=?1 ORDER BY i.seq DESC LIMIT 200",
            )?;
            let inbound = st
                .query_map([chat], |r| {
                    Ok(InboxRow {
                        seq: r.get(0)?,
                        chat: r.get(1)?,
                        phone: r.get(2)?,
                        push_name: r.get(3)?,
                        customer_id: r.get(4)?,
                        customer_name: r.get(5)?,
                        received_at: r.get(6)?,
                        kind: r.get(7)?,
                        body: r.get(8)?,
                        caption: r.get(9)?,
                        has_media: r.get(10)?,
                        media_mime: r.get(11)?,
                        read_at: r.get(12)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let phone = inbound.iter().find_map(|m| m.phone.clone()).or_else(|| phone_from_jid(chat));
            let mut outbound = vec![];
            if let Some(p) = &phone {
                let mut st = c.prepare("SELECT message_id FROM wa_outbox WHERE to_phone=?1 ORDER BY created_at DESC LIMIT 200")?;
                let ids = st.query_map([p], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                for id in ids {
                    outbound.push(load_outbox(c, &id)?);
                }
            }
            Ok(json!({ "chat": chat, "phone": phone, "inbound": inbound, "outbound": outbound }))
        })
    }

    /// Image attached to an inbound message (for review screens).
    pub fn wa_media(&self, token: &str, seq: i64) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("payments.review") {
            s.require("whatsapp.manage")?;
        }
        let (path, mime): (Option<String>, Option<String>) = self.db.read(|c| {
            Ok(c.query_row("SELECT media_path, media_mime FROM wa_inbox WHERE seq=?1", [seq], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .unwrap_or((None, None)))
        })?;
        let path = path.ok_or_else(|| AppError::new(crate::error::ErrorCode::NotFound, "This message has no saved attachment."))?;
        self.read_data_file(&path, mime.as_deref().unwrap_or("application/octet-stream"))
    }

    /// Read a file that lives inside the data folder (attachments, scans).
    pub(crate) fn read_data_file(&self, path: &str, mime: &str) -> AppResult<Value> {
        let p = std::fs::canonicalize(path)
            .map_err(|_| AppError::new(crate::error::ErrorCode::NotFound, "The file is no longer on this computer."))?;
        let root = std::fs::canonicalize(&self.data_dir)?;
        if !p.starts_with(&root) {
            return Err(AppError::new(crate::error::ErrorCode::Forbidden, "File is outside the AMWAPOS data folder."));
        }
        let bytes = std::fs::read(&p)?;
        Ok(json!({ "mime": mime, "base64": crate::ids::b64(&bytes), "size": bytes.len() }))
    }

    /// Mark a conversation read. Read receipts reach the phone from the
    /// WhatsApp service when enabled in WhatsApp settings.
    pub fn wa_mark_read(&self, token: &str, chat: &str) -> AppResult<i64> {
        let s = self.session(token)?;
        s.require("whatsapp.manage")?;
        let now = time::now_str();
        self.db.write(|tx| Ok(tx.execute("UPDATE wa_inbox SET read_at=?2 WHERE chat=?1 AND read_at IS NULL", params![chat, now])? as i64))
    }

    /// Contact import: create customers for WhatsApp senders that are not yet
    /// customers (name from the WhatsApp profile, phone from the chat), and
    /// link their messages. `chats` limits the import to chosen conversations.
    pub fn wa_import_contacts(&self, token: &str, chats: Option<Vec<String>>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("whatsapp.manage")?;
        s.require("customers.manage")?;
        self.require_feature("whatsapp.enabled")?;
        let candidates: Vec<(String, String, Option<String>)> = self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT chat, MAX(phone), MAX(push_name) FROM wa_inbox WHERE customer_id IS NULL AND phone IS NOT NULL GROUP BY chat ORDER BY MAX(seq) DESC LIMIT 500",
            )?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        let (mut created, mut linked, mut skipped) = (0, 0, 0);
        for (chat, phone, name) in candidates {
            if let Some(only) = &chats {
                if !only.contains(&chat) {
                    continue;
                }
            }
            let existing = self.db.read(|c| find_customer_by_phone(c, &phone))?;
            let cid = match existing {
                Some((id, _)) => {
                    linked += 1;
                    id
                }
                None => {
                    let name = name
                        .clone()
                        .map(|n| n.trim().chars().take(100).collect::<String>())
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| phone.clone());
                    let input: crate::customers::CustomerInput =
                        serde_json::from_value(json!({ "name": name, "phone": phone, "whatsapp": phone }))
                            .map_err(|e| AppError::internal(e.to_string()))?;
                    match self.customer_save(token, None, input) {
                        Ok(c) => {
                            created += 1;
                            c.customer_id
                        }
                        Err(_) => {
                            skipped += 1;
                            continue;
                        }
                    }
                }
            };
            self.db.write(|tx| {
                Ok(tx.execute("UPDATE wa_inbox SET customer_id=?2 WHERE chat=?1 AND customer_id IS NULL", params![chat, cid])?)
            })?;
        }
        Ok(json!({ "created": created, "linked": linked, "skipped": skipped }))
    }

    /// Runtime: read marks not yet sent to the phone.
    pub fn wa_unsynced_reads(&self) -> AppResult<Vec<(i64, String, String)>> {
        let wa: WhatsAppSettings = self.db.read(|c| settings::get(c, settings::KEY_WHATSAPP))?;
        if !wa.send_read_receipts {
            self.db.write(|tx| Ok(tx.execute("UPDATE wa_inbox SET read_synced=1 WHERE read_at IS NOT NULL AND read_synced=0", [])?))?;
            return Ok(vec![]);
        }
        self.db.read(|c| {
            let mut st = c.prepare("SELECT seq, chat, wa_id FROM wa_inbox WHERE read_at IS NOT NULL AND read_synced=0 LIMIT 100")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn wa_reads_synced(&self, seqs: &[i64]) -> AppResult<()> {
        self.db.write(|tx| {
            for s in seqs {
                tx.execute("UPDATE wa_inbox SET read_synced=1 WHERE seq=?1", [s])?;
            }
            Ok(())
        })
    }

    /// Unread count and queue health for the header badge and diagnostics.
    pub fn wa_summary(&self, token: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("whatsapp.send") {
            s.require("whatsapp.manage")?;
        }
        self.db.read(|c| {
            let unread: i64 = c.query_row("SELECT COUNT(*) FROM wa_inbox WHERE read_at IS NULL", [], |r| r.get(0))?;
            let queued: i64 = c.query_row("SELECT COUNT(*) FROM wa_outbox WHERE status IN ('queued','sending')", [], |r| r.get(0))?;
            let failed: i64 = c.query_row("SELECT COUNT(*) FROM wa_outbox WHERE status='failed'", [], |r| r.get(0))?;
            Ok(json!({ "unread": unread, "queued": queued, "failed": failed }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_fill_known_placeholders_only() {
        let out = render_template(
            "Hi {customer}, total {total} {unknown} {not a key} {",
            &[("customer", "{total}".into()), ("total", "BHD 1.000".into())],
        );
        assert_eq!(out, "Hi {total}, total BHD 1.000 {unknown} {not a key} {");
    }

    #[test]
    fn jid_to_phone() {
        assert_eq!(phone_from_jid("97333334444@s.whatsapp.net").as_deref(), Some("+97333334444"));
        assert_eq!(phone_from_jid("97333334444:12@s.whatsapp.net").as_deref(), Some("+97333334444"));
        assert_eq!(phone_from_jid("12345@g.us"), None);
        assert_eq!(phone_from_jid("1234567@lid"), None);
    }
}
