//! WhatsApp messaging: templates (English/Arabic), an outbox with
//! send-once semantics, the inbox fed by the sidecar, and PDF receipt copies.
//!
//! The core never talks to WhatsApp itself. The runtime claims queued
//! messages, hands them to the sidecar (which de-duplicates by message id)
//! and reports the result back. Inbound text is stored as data; it is never
//! interpreted as an instruction.

use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::customers::normalize_phone;
use crate::error::{AppError, AppResult};
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
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutboxRow {
    pub message_id: String,
    pub kind: String,
    pub to_phone: String,
    pub customer_id: Option<String>,
    pub customer_name: Option<String>,
    pub sale_id: Option<String>,
    pub delivery_id: Option<String>,
    pub lang: String,
    pub body: String,
    pub document_name: Option<String>,
    pub status: String,
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
                o.status, o.attempts, o.last_error, u.display_name, o.created_at, o.sent_at
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

    /// Check permission and module for a WhatsApp link action (connect,
    /// disconnect, unlink) and write the audit entry. The runtime performs the
    /// action on the sidecar afterwards.
    pub fn wa_link_action(&self, token: &str, action: &str) -> AppResult<()> {
        let s = self.session(token)?;
        s.require("whatsapp.manage")?;
        if action != "disconnect" && action != "unlink" {
            self.require_feature("whatsapp")?;
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

    /// Queue a WhatsApp message. Idempotent on `operation_id`.
    pub fn wa_queue(&self, token: &str, req: QueueRequest) -> AppResult<OutboxRow> {
        let s = self.session(token)?;
        self.require_feature("whatsapp")?;
        let op = validate::id(&req.operation_id, "Operation")?;
        match req.kind.as_str() {
            "receipt" | "dispatch" => {
                if !s.has("whatsapp.send") {
                    s.require("whatsapp.manage")?;
                }
            }
            "reminder" | "text" => s.require("whatsapp.manage")?,
            _ => return Err(AppError::validation("Unknown message type.")),
        }
        if let Some(existing) = self.db.read(|c| {
            Ok(c.query_row("SELECT message_id FROM wa_outbox WHERE operation_id=?1", [&op], |r| r.get::<_, String>(0)).optional()?)
        })? {
            return self.db.read(|c| load_outbox(c, &existing));
        }
        let wa: WhatsAppSettings = self.db.read(|c| settings::get(c, settings::KEY_WHATSAPP))?;
        // Receipt PDF is rendered before the transaction (it only reads committed data).
        let pdf = match (req.kind.as_str(), &req.sale_id) {
            ("receipt", Some(sale)) if wa.attach_pdf => Some(self.receipt_pdf_write("sale", &validate::id(sale, "Sale")?)?),
            _ => None,
        };
        let actor = self.actor(&s, None);
        let id = self.db.write(|tx| {
            let (currency, digits) = self.currency(tx)?;
            let business: String = tx.query_row("SELECT name FROM business LIMIT 1", [], |r| r.get(0)).optional()?.unwrap_or_default();
            let tz: String = tx.query_row("SELECT timezone FROM business LIMIT 1", [], |r| r.get(0)).optional()?.unwrap_or_else(|| "Asia/Bahrain".into());
            let mut customer_id = req.customer_id.clone().filter(|x| !x.is_empty());
            let mut phone = match req.to_phone.as_deref().filter(|x| !x.trim().is_empty()) {
                Some(p) => normalize_phone(p)?,
                None => None,
            };
            let mut vars: Vec<(&str, String)> = vec![("business", business)];
            let body_template: String;
            let (mut sale_id, mut delivery_id) = (None::<String>, None::<String>);
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
                    body_template = String::new();
                    sale_id = Some(sid);
                }
                "dispatch" | "reminder" => {
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
                    body_template = String::new();
                    delivery_id = Some(did);
                }
                _ => {
                    let text = req.text.clone().unwrap_or_default();
                    let text = text.trim();
                    if text.is_empty() || text.chars().count() > 4000 {
                        return Err(AppError::validation("Message text must be 1 to 4000 characters."));
                    }
                    body_template = text.to_string();
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
                "reminder" => render_template(&pick(&wa.reminder, &lang), &vars),
                _ => body_template,
            };
            let id = new_id();
            let now = time::now_str();
            let (doc_path, doc_name) = match &pdf {
                Some(p) => (Some(p.to_string_lossy().to_string()), p.file_name().map(|f| f.to_string_lossy().to_string())),
                None => (None, None),
            };
            tx.execute(
                "INSERT INTO wa_outbox(message_id, operation_id, kind, to_phone, customer_id, sale_id, delivery_id, lang, body, document_path, document_name,
                    status, created_by, created_at, updated_at, next_attempt_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'queued',?12,?13,?13,?13)",
                params![id, op, req.kind, phone, customer_id, sale_id, delivery_id, lang, body, doc_path, doc_name, s.user_id, now],
            )?;
            audit::record(
                tx,
                &actor,
                "whatsapp.queued",
                "whatsapp_message",
                Some(&id),
                None,
                Some(&json!({ "kind": req.kind, "to": phone, "sale_id": sale_id, "delivery_id": delivery_id })),
            )?;
            Ok(id)
        })?;
        self.db.read(|c| load_outbox(c, &id))
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
    /// (crash mid-send) are reclaimed; the sidecar de-duplicates by id, so a
    /// reclaimed message is never delivered twice.
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
        self.db.write(|tx| {
            match result {
                Ok(wa_id) => {
                    tx.execute(
                        "UPDATE wa_outbox SET status='sent', wa_message_id=?2, sent_at=?3, updated_at=?3, last_error=NULL WHERE message_id=?1",
                        params![message_id, wa_id, now],
                    )?;
                }
                Err((msg, permanent)) => {
                    let attempts: i64 =
                        tx.query_row("SELECT attempts FROM wa_outbox WHERE message_id=?1", [message_id], |r| r.get(0)).optional()?.unwrap_or(0);
                    if permanent || attempts >= MAX_ATTEMPTS {
                        tx.execute(
                            "UPDATE wa_outbox SET status='failed', last_error=?2, updated_at=?3 WHERE message_id=?1",
                            params![message_id, msg, now],
                        )?;
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

    /// Runtime: store messages polled from the sidecar. Returns the inbox
    /// sequence numbers of new image messages (candidates for payment review).
    pub fn wa_ingest(&self, messages: &[Value]) -> AppResult<Vec<i64>> {
        self.db.write(|tx| {
            let mut images = vec![];
            for m in messages {
                let s = |k: &str| m.get(k).and_then(|v| v.as_str()).map(|x| x.to_string());
                let (Some(wa_id), Some(chat)) = (s("id"), s("chat")) else { continue };
                let kind = match s("type").as_deref() {
                    Some(k @ ("text" | "image" | "document")) => k.to_string(),
                    _ => "other".to_string(),
                };
                let phone = s("sender_pn").and_then(|p| phone_from_jid(&p)).or_else(|| phone_from_jid(&chat));
                let customer = match &phone {
                    Some(p) => find_customer_by_phone(tx, p)?,
                    None => None,
                };
                let ts = m.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
                let received = chrono::DateTime::from_timestamp(ts, 0).map(time::fmt).unwrap_or_else(time::now_str);
                let media = m.get("media").filter(|v| v.is_object());
                let media_s = |k: &str| media.and_then(|x| x.get(k)).and_then(|v| v.as_str()).map(|x| x.to_string());
                let clip = |v: Option<String>| v.map(|t| t.chars().take(8000).collect::<String>());
                let n = tx.execute(
                    "INSERT OR IGNORE INTO wa_inbox(wa_id, chat, phone, push_name, received_at, kind, body, caption, media_path, media_mime, media_sha256, customer_id)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    params![
                        wa_id,
                        chat,
                        phone,
                        clip(s("push_name")),
                        received,
                        kind,
                        clip(s("text")),
                        clip(s("caption")),
                        media_s("path"),
                        media_s("mime"),
                        media_s("sha256"),
                        customer.map(|c| c.0)
                    ],
                )?;
                if n == 1 && kind == "image" && media.is_some() {
                    images.push(tx.last_insert_rowid());
                }
            }
            Ok(images)
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

    /// Mark a conversation read. Read receipts reach the phone on the next
    /// sidecar cycle when enabled in WhatsApp settings.
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
        self.require_feature("whatsapp")?;
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

    /// Runtime: the sidecar inbox cursor (local to this computer).
    pub fn wa_cursor(&self) -> AppResult<i64> {
        self.db.read(|c| Ok(settings::get::<Option<i64>>(c, "local.whatsapp_cursor")?.unwrap_or(0)))
    }

    pub fn wa_set_cursor(&self, v: i64) -> AppResult<()> {
        self.db.write(|tx| settings::put(tx, "local.whatsapp_cursor", &v, None))
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
