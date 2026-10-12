//! WhatsApp order autopilot (flag `orders.whatsapp_autopilot`,
//! docs/WHATSAPP_AUTOPILOT.md).
//!
//! The order pipeline (`service.rs`) already turns each message into a draft
//! with questions, real products, POS prices, stock and a delivery fee. The
//! autopilot answers the customer from that state, without a person:
//!
//! * the reply is chosen by rules and built from POS data (prices, stock,
//!   fees, order numbers); no model writes free text to a customer, so a
//!   price or a promise can never be invented;
//! * "yes" confirms only the exact order last sent to the customer (lines,
//!   address, fee and total fingerprinted); anything changed since gets a new
//!   summary instead; the confirmation runs the same stock check and audit as
//!   a person's;
//! * the customer can cancel a draft or a confirmed order that has not been
//!   rung up yet; once it is being prepared, a person takes over;
//! * STATUS, HELP and AGENT are understood in English and Arabic;
//! * a person takes over (and the autopilot stays silent in that chat) when
//!   asked for, on a complaint, when the delivery fee is unknown, when stock
//!   ran out at confirmation, and when the chat goes back and forth too often
//!   (two bots answering each other must not loop).
//!
//! Status messages after confirmation come from the order event feed
//! (`wa_autopilot_notify`), so a change made anywhere (till, delivery board,
//! another computer) reaches the customer once.
//!
//! Every decision is recorded once per inbound message (`wa_autopilot_log`);
//! every send is idempotent on its operation id.

#![allow(clippy::type_complexity)]

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use super::interpret;
use super::service::{
    add_priority, branch, event, is_arabic_chat, money_str, now, order_status, pending, product_info, product_info_text, quote_fingerprint,
    totals, Question,
};
use crate::address::AddressParts;
use crate::auth::Session;
use crate::error::AppResult;
use crate::ids::new_id;
use crate::messaging::QueueRequest;
use crate::service::AppCore;
use crate::settings::{self, WhatsAppSettings};
use crate::time;

/// The autopilot's identity in audit records, outbox rows and order history.
pub const AUTOPILOT_USER: &str = "system:autopilot";
const PERMISSIONS: &[&str] = &["orders.manage", "whatsapp.send"];
/// Messages older than this when processed (AMWAPOS was off) are left to staff.
const MAX_AGE_MINUTES: i64 = 120;
/// More automatic replies than this to one chat in the window hand it to a person.
const LOOP_WINDOW_MINUTES: i64 = 10;
const LOOP_MAX_REPLIES: i64 = 6;
/// The help text is sent unasked at most this often per chat.
const HELP_EVERY_HOURS: i64 = 12;
/// Status events older than this when the notifier sees them are not sent.
const NOTIFY_MAX_AGE_HOURS: i64 = 6;
const NOTIFIER: &str = "whatsapp_customer_notices";
const FIRST_RUN_LOOKBACK_MINUTES: i64 = 10;
/// Interpreted messages still waiting for an answer are picked up this long.
const PENDING_WINDOW_MINUTES: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Status,
    Agent,
    Help,
}

/// A whole message that is one of the customer commands (English, Arabic,
/// common spellings). Only short messages count, so "help me order 2 milk"
/// is an order, not a help request.
pub fn command(text: &str) -> Option<Command> {
    let t: String = text
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if t.is_empty() || t.chars().count() > 40 {
        return None;
    }
    const STATUS: &[&str] = &[
        "status",
        "order status",
        "my order",
        "where is my order",
        "wheres my order",
        "where s my order",
        "track",
        "track order",
        "track my order",
        "الحالة",
        "حالة الطلب",
        "وين طلبي",
        "وين الطلب",
        "طلبي وين",
        "اين طلبي",
        "أين طلبي",
    ];
    const AGENT: &[&str] = &[
        "agent",
        "human",
        "person",
        "staff",
        "talk to someone",
        "talk to a person",
        "speak to someone",
        "call me",
        "customer service",
        "موظف",
        "اكلم موظف",
        "أكلم موظف",
        "ابي موظف",
        "خدمة العملاء",
        "كلمني",
        "اتصل بي",
    ];
    const HELP: &[&str] = &["help", "menu", "options", "how to order", "مساعدة", "القائمة", "قائمة", "كيف اطلب", "كيف أطلب"];
    if STATUS.contains(&t.as_str()) {
        Some(Command::Status)
    } else if AGENT.contains(&t.as_str()) {
        Some(Command::Agent)
    } else if HELP.contains(&t.as_str()) {
        Some(Command::Help)
    } else {
        None
    }
}

/// What the autopilot does for one inbound message.
#[derive(Debug, Clone, PartialEq)]
enum Plan {
    Nothing(&'static str),
    Reply { action: &'static str, text: String, quote: Option<(i64, String)> },
    Confirm { session: String, order: String },
    CancelConfirmed { session: String, order: String },
    Handoff { session: Option<String>, reason: &'static str },
}

struct Ctx {
    seq: i64,
    chat: String,
    phone: Option<String>,
    kind: String,
    text: String,
    received_at: String,
    intent: Option<String>,
    reason: Option<String>,
    session: Option<String>,
    arabic: bool,
}

fn t(arabic: bool, en: &str, ar: &str) -> String {
    if arabic {
        ar.into()
    } else {
        en.into()
    }
}

fn help_text(arabic: bool) -> String {
    t(
        arabic,
        "Hello! You can order here on WhatsApp.\n• Send what you need, e.g. \"2 milk 1L and 1 bread\"\n• Ask a price: \"price of rice 5kg\"\n• STATUS: where your order is\n• CANCEL: cancel your order\n• AGENT: talk to a person",
        "أهلاً! يمكنك الطلب هنا عبر واتساب.\n• أرسل ما تحتاجه، مثلاً: \"2 حليب 1 لتر و1 خبز\"\n• اسأل عن السعر: \"سعر الرز 5 كيلو\"\n• الحالة: لمعرفة مكان طلبك\n• إلغاء: لإلغاء طلبك\n• موظف: للتحدث مع شخص",
    )
}

fn handoff_text(reason: &str, arabic: bool) -> String {
    match reason {
        "fee_unknown" => t(
            arabic,
            "Thank you! A member of our team will confirm the delivery fee for your area and reply here shortly.",
            "شكراً لك! سيؤكد أحد أعضاء فريقنا رسوم التوصيل لمنطقتك ويرد عليك هنا قريباً.",
        ),
        "stock_changed" => t(
            arabic,
            "Sorry, some items in your order just ran out. A member of our team will reply here shortly with options.",
            "عذراً، نفدت بعض أصناف طلبك للتو. سيرد عليك أحد أعضاء فريقنا هنا قريباً بالبدائل.",
        ),
        "already_preparing" => t(
            arabic,
            "Your order is already being prepared, so it cannot be cancelled here. A member of our team will reply shortly.",
            "طلبك قيد التجهيز بالفعل، لذلك لا يمكن إلغاؤه من هنا. سيرد عليك أحد أعضاء فريقنا قريباً.",
        ),
        _ => t(arabic, "Thank you. A member of our team will reply to you here shortly.", "شكراً لك. سيرد عليك أحد أعضاء فريقنا هنا قريباً."),
    }
}

fn payment_note(c: &Connection, arabic: bool) -> AppResult<String> {
    let w: WhatsAppSettings = settings::get(c, settings::KEY_WHATSAPP)?;
    Ok(if arabic { w.autopilot_payment_note.ar } else { w.autopilot_payment_note.en }.trim().to_string())
}

/// The order as the customer will agree to it, from POS prices and fees.
fn summary_text(c: &Connection, session: &str, arabic: bool) -> AppResult<Option<(String, (i64, String))>> {
    let (order, mode, address_json, fee): (Option<String>, String, Option<String>, Option<i64>) = c.query_row(
        "SELECT order_id, delivery_mode, address_json, delivery_fee_minor FROM wa_order_sessions WHERE session_id=?1",
        [session],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let Some(order) = order else { return Ok(None) };
    let Some(quote) = quote_fingerprint(c, session)? else { return Ok(None) };
    let branch_id: String = c.query_row("SELECT branch_id FROM digital_orders WHERE order_id=?1", [&order], |r| r.get(0))?;
    let (sub, rows, _) = totals(c, &order, &branch_id)?;
    let addr: Value = address_json.and_then(|x| serde_json::from_str(&x).ok()).unwrap_or(Value::Null);
    let parts: AddressParts = serde_json::from_value(addr["parts"].clone()).unwrap_or_default();
    let mut out = t(arabic, "Your order:\n", "طلبك:\n");
    for r in &rows {
        if r["product_id"].is_null() {
            continue;
        }
        out.push_str(&format!(
            "{} × {} — {}\n",
            crate::money::format_qty(r["qty_milli"].as_i64().unwrap_or(0)),
            r["name"].as_str().unwrap_or(""),
            r["line_total_minor"].as_i64().map(money_str).unwrap_or_default()
        ));
    }
    match mode.as_str() {
        "delivery" => {
            out.push_str(&format!("{} {}\n", t(arabic, "Delivery to:", "التوصيل إلى:"), parts.line().unwrap_or_default()));
            if let Some(f) = fee {
                out.push_str(&format!("{} {} BHD\n", t(arabic, "Delivery fee:", "رسوم التوصيل:"), money_str(f)));
            }
        }
        "pickup" => out.push_str(&t(arabic, "Pickup from the shop\n", "استلام من المحل\n")),
        _ => {}
    }
    out.push_str(&format!("{} {} BHD\n", t(arabic, "Total:", "الإجمالي:"), money_str(sub + fee.unwrap_or(0))));
    let note = payment_note(c, arabic)?;
    if !note.is_empty() {
        out.push_str(&note);
        out.push('\n');
    }
    out.push_str(&t(arabic, "Reply YES to confirm, or tell us what to change.", "أرسل \"نعم\" للتأكيد، أو أخبرنا بما تريد تغييره."));
    Ok(Some((out, quote)))
}

fn questions_text(qs: &[Question]) -> String {
    qs.iter().map(|q| q.text.clone()).collect::<Vec<_>>().join("\n")
}

/// The customer's latest order and where it is, in plain words.
fn status_text(c: &Connection, phone: &str, arabic: bool) -> AppResult<String> {
    let since = time::fmt(time::now() - chrono::Duration::days(14));
    let o: Option<(String, String, String, i64, Option<String>)> = c
        .query_row(
            "SELECT order_id, order_number, status, delivery_wanted, delivery_id FROM digital_orders
              WHERE phone=?1 AND created_at >= ?2
                AND NOT (status IN ('draft','cancelled') AND NOT EXISTS (SELECT 1 FROM digital_order_lines l WHERE l.order_id=digital_orders.order_id))
              ORDER BY created_at DESC LIMIT 1",
            params![phone, since],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((order_id, number, status, delivery_wanted, delivery_id)) = o else {
        return Ok(t(
            arabic,
            "You have no open orders. Send us what you would like to order.",
            "ليس لديك طلبات مفتوحة. أرسل لنا ما تريد طلبه.",
        ));
    };
    let delivery: Option<(String, Option<String>)> = c
        .query_row(
            "SELECT status, outcome FROM delivery_orders WHERE delivery_id=?1 OR order_id=?2 ORDER BY created_at DESC LIMIT 1",
            params![delivery_id, order_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match (status.as_str(), delivery.as_ref().map(|(s, o)| (s.as_str(), o.as_deref()))) {
        ("draft", _) => t(
            arabic,
            &format!("Your order {number} is not confirmed yet. Send STATUS again after you reply YES to the summary."),
            &format!("طلبك {number} لم يتم تأكيده بعد. أرسل \"نعم\" بعد ملخص الطلب لتأكيده."),
        ),
        ("confirmed", _) => t(
            arabic,
            &format!("Order {number} is confirmed and waiting to be prepared."),
            &format!("الطلب {number} مؤكد وبانتظار التجهيز."),
        ),
        ("cancelled", _) => t(arabic, &format!("Order {number} was cancelled."), &format!("تم إلغاء الطلب {number}.")),
        (_, Some((_, Some(o)))) if o != "delivered" => t(
            arabic,
            &format!("We could not deliver order {number}. A member of our team will contact you."),
            &format!("لم نتمكن من توصيل الطلب {number}. سيتواصل معك أحد أعضاء فريقنا."),
        ),
        (_, Some(("pending" | "preparing", _))) => {
            t(arabic, &format!("Order {number} is being prepared."), &format!("الطلب {number} قيد التجهيز."))
        }
        (_, Some(("dispatched", _))) => t(arabic, &format!("Order {number} is on the way."), &format!("الطلب {number} في الطريق إليك.")),
        (_, Some(("delivered", _))) => {
            t(arabic, &format!("Order {number} was delivered. Thank you!"), &format!("تم توصيل الطلب {number}. شكراً لك!"))
        }
        (_, Some(("cancelled", _))) => t(
            arabic,
            &format!("The delivery of order {number} was cancelled. A member of our team will contact you."),
            &format!("تم إلغاء توصيل الطلب {number}. سيتواصل معك أحد أعضاء فريقنا."),
        ),
        ("converted", None) if delivery_wanted == 0 => {
            t(arabic, &format!("Order {number} was collected. Thank you!"), &format!("تم استلام الطلب {number}. شكراً لك!"))
        }
        _ => t(arabic, &format!("Order {number} is being prepared."), &format!("الطلب {number} قيد التجهيز.")),
    })
}

/// A conversation for a chat that has none open (a person must see the
/// handoff in the WhatsApp orders inbox).
fn ensure_session(tx: &Connection, chat: &str, phone: Option<&str>, intent: &str) -> AppResult<String> {
    if let Some(s) = tx
        .query_row(
            "SELECT session_id FROM wa_order_sessions WHERE chat=?1 AND state IN ('collecting','clarifying','ready') ORDER BY created_at DESC LIMIT 1",
            [chat],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(s);
    }
    let sid = new_id();
    let t = now();
    tx.execute(
        "INSERT INTO wa_order_sessions(session_id, chat, phone, customer_state, state, intent, created_at, updated_at)
         VALUES (?1,?2,?3,'provisional','collecting',?4,?5,?5)",
        params![sid, chat, phone, intent, t],
    )?;
    event(tx, &sid, None, "session_started", "system", &json!({ "intent": intent }), None)?;
    Ok(sid)
}

impl AppCore {
    fn autopilot_ctx(&self, c: &Connection, seq: i64) -> AppResult<Option<Ctx>> {
        let row: Option<(String, Option<String>, String, Option<String>, String)> = c
            .query_row("SELECT chat, phone, kind, COALESCE(body, caption), received_at FROM wa_inbox WHERE seq=?1", [seq], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .optional()?;
        let Some((chat, phone, kind, text, received_at)) = row else { return Ok(None) };
        let p: Option<(Option<String>, Option<String>, Option<String>)> = c
            .query_row("SELECT intent, reason, session_id FROM wa_inbox_processing WHERE inbox_seq=?1", [seq], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        let Some((intent, reason, session)) = p else { return Ok(None) };
        let text = text.unwrap_or_default();
        let arabic = interpret::has_arabic(&text) || is_arabic_chat(c, &chat)?;
        Ok(Some(Ctx { seq, chat, phone, kind, text, received_at, intent, reason, session, arabic }))
    }

    fn autopilot_plan(&self, c: &Connection, x: &Ctx) -> AppResult<Plan> {
        if x.chat.ends_with("@g.us") || x.chat.starts_with("status@") {
            return Ok(Plan::Nothing("not_a_customer_chat"));
        }
        if x.phone.as_deref().is_none_or(|p| p.is_empty()) {
            return Ok(Plan::Nothing("no_phone"));
        }
        let age = time::parse(&x.received_at).map(|r| (time::now() - r).num_minutes()).unwrap_or(0);
        if age > MAX_AGE_MINUTES {
            return Ok(Plan::Nothing("too_old"));
        }
        if matches!(x.reason.as_deref(), Some("staff_takeover" | "out_of_order")) || x.intent.as_deref() == Some("spam") {
            return Ok(Plan::Nothing("left_to_staff"));
        }
        if let Some(s) = &x.session {
            let takeover: i64 = c.query_row("SELECT staff_takeover FROM wa_order_sessions WHERE session_id=?1", [s], |r| r.get(0))?;
            if takeover == 1 {
                return Ok(Plan::Nothing("staff_takeover"));
            }
        }
        // Too many automatic replies in a short time: a person takes over.
        let since = time::fmt(time::now() - chrono::Duration::minutes(LOOP_WINDOW_MINUTES));
        let recent: i64 = c.query_row(
            "SELECT COUNT(*) FROM wa_autopilot_log WHERE chat=?1 AND created_at >= ?2 AND message_id IS NOT NULL",
            params![x.chat, since],
            |r| r.get(0),
        )?;
        if recent >= LOOP_MAX_REPLIES {
            return Ok(Plan::Handoff { session: x.session.clone(), reason: "too_many_messages" });
        }
        let cmd = if x.kind == "text" { command(&x.text) } else { None };
        match cmd {
            Some(Command::Agent) => return Ok(Plan::Handoff { session: x.session.clone(), reason: "customer_asked_for_person" }),
            Some(Command::Status) => {
                let text = status_text(c, x.phone.as_deref().unwrap_or_default(), x.arabic)?;
                return Ok(Plan::Reply { action: "status", text, quote: None });
            }
            Some(Command::Help) => return Ok(Plan::Reply { action: "help", text: help_text(x.arabic), quote: None }),
            None => {}
        }
        let intent = x.intent.as_deref().unwrap_or("unknown");
        // About an order that already closed (confirmed, rung up, delivered).
        if x.reason.as_deref() == Some("after_close") {
            let Some(s) = &x.session else { return Ok(Plan::Nothing("no_session")) };
            let order: Option<String> =
                c.query_row("SELECT order_id FROM wa_order_sessions WHERE session_id=?1", [s], |r| r.get(0)).optional()?.flatten();
            let Some(order) = order else { return Ok(Plan::Nothing("no_order")) };
            return Ok(match intent {
                "cancellation" => match order_status(c, &order)?.as_deref() {
                    Some("confirmed") => Plan::CancelConfirmed { session: s.clone(), order },
                    Some("cancelled") => Plan::Reply {
                        action: "status",
                        text: status_text(c, x.phone.as_deref().unwrap_or_default(), x.arabic)?,
                        quote: None,
                    },
                    _ => Plan::Handoff { session: Some(s.clone()), reason: "already_preparing" },
                },
                "payment" if x.kind != "text" => Plan::Reply { action: "payment_received", text: payment_received(x.arabic), quote: None },
                _ => Plan::Reply { action: "status", text: status_text(c, x.phone.as_deref().unwrap_or_default(), x.arabic)?, quote: None },
            });
        }
        let Some(sid) = x.session.clone() else {
            // Nothing to attach to: greet once in a while, answer the rest with the status.
            return Ok(match intent {
                "confirmation" | "cancellation" | "payment" => {
                    Plan::Reply { action: "status", text: status_text(c, x.phone.as_deref().unwrap_or_default(), x.arabic)?, quote: None }
                }
                _ => {
                    let since = time::fmt(time::now() - chrono::Duration::hours(HELP_EVERY_HOURS));
                    let helped: i64 = c.query_row(
                        "SELECT COUNT(*) FROM wa_autopilot_log WHERE chat=?1 AND action='help' AND created_at >= ?2",
                        params![x.chat, since],
                        |r| r.get(0),
                    )?;
                    if helped > 0 || (x.kind == "text" && x.text.trim().is_empty()) {
                        Plan::Nothing("help_sent_recently")
                    } else {
                        Plan::Reply { action: "help", text: help_text(x.arabic), quote: None }
                    }
                }
            });
        };
        let (state, order, mode, fee): (String, Option<String>, String, Option<i64>) = c.query_row(
            "SELECT state, order_id, delivery_mode, delivery_fee_minor FROM wa_order_sessions WHERE session_id=?1",
            [&sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        if state == "cancelled" {
            // A draft cancelled now: the order event sends the cancellation.
            return Ok(if order.is_some() {
                Plan::Nothing("cancellation_notice_follows")
            } else {
                Plan::Reply { action: "cancelled", text: t(x.arabic, "Okay, cancelled.", "تم الإلغاء."), quote: None }
            });
        }
        if intent == "support_issue" {
            return Ok(Plan::Handoff { session: Some(sid), reason: "support_issue" });
        }
        if intent == "payment" && x.kind != "text" {
            return Ok(Plan::Reply { action: "payment_received", text: payment_received(x.arabic), quote: None });
        }
        let qs = pending(c, &sid)?;
        if !qs.is_empty() {
            return Ok(Plan::Reply { action: "question", text: questions_text(&qs), quote: None });
        }
        let has_lines = match &order {
            Some(o) => c.query_row("SELECT COUNT(*) FROM digital_order_lines WHERE order_id=?1", [o], |r| r.get::<_, i64>(0))? > 0,
            None => false,
        };
        if state == "ready" && has_lines {
            if mode == "delivery" && fee.is_none() {
                return Ok(Plan::Handoff { session: Some(sid), reason: "fee_unknown" });
            }
            let current = quote_fingerprint(c, &sid)?;
            if intent == "confirmation" {
                let last_fp: Option<String> = c
                    .query_row(
                        "SELECT json_extract(data_json,'$.quote_fp') FROM wa_order_events WHERE session_id=?1 AND kind='reply_queued'
                           AND json_extract(data_json,'$.quote_fp') IS NOT NULL ORDER BY created_at DESC, rowid DESC LIMIT 1",
                        [&sid],
                        |r| r.get(0),
                    )
                    .optional()?
                    .flatten();
                if let (Some(fp), Some((_, now_fp)), Some(o)) = (&last_fp, &current, &order) {
                    if fp == now_fp && order_status(c, o)?.as_deref() == Some("draft") {
                        return Ok(Plan::Confirm { session: sid, order: o.clone() });
                    }
                }
            }
            if let Some((text, quote)) = summary_text(c, &sid, x.arabic)? {
                return Ok(Plan::Reply { action: "summary", text, quote: Some(quote) });
            }
        }
        // Questions about products before anything is ordered: real prices.
        if matches!(intent, "price_question" | "availability_question" | "product_question") {
            let branch_id = branch(self, c)?;
            let info = product_info(c, &x.chat, &branch_id)?;
            if !info.is_empty() {
                let mut text = product_info_text(&info, x.arabic);
                text.push('\n');
                text.push_str(&t(x.arabic, "Send what you would like to order.", "أرسل ما تريد طلبه."));
                return Ok(Plan::Reply { action: "product_info", text, quote: None });
            }
        }
        Ok(match state.as_str() {
            "collecting" if !has_lines => Plan::Reply { action: "help", text: help_text(x.arabic), quote: None },
            _ => Plan::Handoff { session: Some(sid), reason: "unclear" },
        })
    }

    /// Answer one processed inbound message (worker, after interpretation
    /// and AI help). Returns the action taken; `None` when the autopilot is
    /// off or this message was already handled.
    pub fn wa_autopilot(&self, seq: i64) -> AppResult<Option<String>> {
        if !self.features()?.is_on("orders.whatsapp_autopilot") {
            return Ok(None);
        }
        let Some(x) = self.db.read(|c| self.autopilot_ctx(c, seq))? else { return Ok(None) };
        // Claim the message: a restart or a second worker never answers twice.
        let claimed = self.db.write(|tx| {
            Ok(tx.execute(
                "INSERT OR IGNORE INTO wa_autopilot_log(inbox_seq, chat, action, created_at) VALUES (?1,?2,'deciding',?3)",
                params![seq, x.chat, now()],
            )?)
        })?;
        if claimed == 0 {
            return Ok(None);
        }
        let s = self.system_session(AUTOPILOT_USER, "WhatsApp autopilot", PERMISSIONS)?;
        let plan = self.db.read(|c| self.autopilot_plan(c, &x))?;
        let (action, message_id) = self.autopilot_act(&s, &x, plan)?;
        self.db.write(|tx| {
            tx.execute("UPDATE wa_autopilot_log SET action=?2, message_id=?3 WHERE inbox_seq=?1", params![seq, action, message_id])?;
            Ok(())
        })?;
        Ok(Some(action))
    }

    /// Answer every message interpreted in the last few minutes that has no
    /// decision yet (a restart between reading and answering loses nothing;
    /// turning the autopilot on never answers older chats).
    pub fn wa_autopilot_pending(&self, limit: i64) -> AppResult<Vec<String>> {
        if !self.features()?.is_on("orders.whatsapp_autopilot") {
            return Ok(vec![]);
        }
        let since = time::fmt(time::now() - chrono::Duration::minutes(PENDING_WINDOW_MINUTES));
        let seqs: Vec<i64> = self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT p.inbox_seq FROM wa_inbox_processing p WHERE p.processed_at >= ?1
                   AND NOT EXISTS (SELECT 1 FROM wa_autopilot_log l WHERE l.inbox_seq=p.inbox_seq) ORDER BY p.inbox_seq LIMIT ?2",
            )?;
            let r = st.query_map(params![since, limit], |r| r.get(0))?.collect::<Result<Vec<i64>, _>>()?;
            Ok(r)
        })?;
        let mut out = vec![];
        for seq in seqs {
            match self.wa_autopilot(seq) {
                Ok(Some(a)) => out.push(a),
                Ok(None) => {}
                Err(e) => tracing::warn!(seq, error = %e.message, "WhatsApp autopilot could not answer"),
            }
        }
        Ok(out)
    }

    fn autopilot_act(&self, s: &Session, x: &Ctx, plan: Plan) -> AppResult<(String, Option<String>)> {
        match plan {
            Plan::Nothing(why) => Ok((format!("none:{why}"), None)),
            Plan::Reply { action, text, quote } => {
                let id = self.autopilot_send(s, x, &format!("autoreply-{}", x.seq), &text)?;
                if let Some(sid) = &x.session {
                    self.db.write(|tx| {
                        event(
                            tx,
                            sid,
                            Some(x.seq),
                            "reply_queued",
                            "system",
                            &json!({ "message_id": id, "action": action, "quoted_total_minor": quote.as_ref().map(|q| q.0),
                                     "quote_fp": quote.as_ref().map(|q| q.1.clone()) }),
                            Some(AUTOPILOT_USER),
                        )?;
                        tx.execute("UPDATE wa_order_sessions SET handled=1 WHERE session_id=?1", [sid])?;
                        Ok(())
                    })?;
                }
                Ok((action.to_string(), Some(id)))
            }
            Plan::Confirm { session, order } => match self.order_confirm_as(s, &order, false) {
                Ok(_) => {
                    self.db.write(|tx| {
                        tx.execute("UPDATE digital_orders SET confirmed_by=?2 WHERE order_id=?1", params![order, AUTOPILOT_USER])?;
                        tx.execute(
                            "UPDATE wa_order_sessions SET state='confirmed', handled=1, revision=revision+1, updated_at=?2 WHERE session_id=?1",
                            params![session, now()],
                        )?;
                        event(tx, &session, Some(x.seq), "autopilot_confirmed", "system", &json!({ "order_id": order }), Some(AUTOPILOT_USER))?;
                        Ok(())
                    })?;
                    // The confirmation message itself comes from the order event.
                    Ok(("confirmed".into(), None))
                }
                Err(e) => {
                    tracing::info!(seq = x.seq, error = %e.message, "autopilot could not confirm; handed to staff");
                    let reason = if e.details.as_ref().and_then(|d| d["kind"].as_str()) == Some("stock_shortage") {
                        "stock_changed"
                    } else {
                        "confirm_failed"
                    };
                    self.autopilot_act(s, x, Plan::Handoff { session: Some(session), reason })
                }
            },
            Plan::CancelConfirmed { session, order } => {
                match self.order_cancel_as(s, &order, Some("Cancelled by the customer on WhatsApp.".into())) {
                    Ok(_) => {
                        self.db.write(|tx| {
                            event(
                                tx,
                                &session,
                                Some(x.seq),
                                "autopilot_cancelled",
                                "system",
                                &json!({ "order_id": order }),
                                Some(AUTOPILOT_USER),
                            )
                        })?;
                        Ok(("cancelled".into(), None))
                    }
                    Err(_) => self.autopilot_act(s, x, Plan::Handoff { session: Some(session), reason: "already_preparing" }),
                }
            }
            Plan::Handoff { session, reason } => {
                let sid = self.db.write(|tx| {
                    let sid = match session {
                        Some(sid) => sid,
                        None => ensure_session(tx, &x.chat, x.phone.as_deref(), x.intent.as_deref().unwrap_or("unknown"))?,
                    };
                    tx.execute(
                        "UPDATE wa_order_sessions SET staff_takeover=1, handled=0, updated_at=?2 WHERE session_id=?1",
                        params![sid, now()],
                    )?;
                    add_priority(tx, &sid, &format!("autopilot_{reason}"))?;
                    event(tx, &sid, Some(x.seq), "autopilot_handoff", "system", &json!({ "reason": reason }), Some(AUTOPILOT_USER))?;
                    Ok(sid)
                })?;
                let id = self.autopilot_send(s, x, &format!("autoreply-{}", x.seq), &handoff_text(reason, x.arabic))?;
                self.db.write(|tx| {
                    event(
                        tx,
                        &sid,
                        Some(x.seq),
                        "reply_queued",
                        "system",
                        &json!({ "message_id": id, "action": "handoff" }),
                        Some(AUTOPILOT_USER),
                    )
                })?;
                Ok((format!("handoff:{reason}"), Some(id)))
            }
        }
    }

    fn autopilot_send(&self, s: &Session, x: &Ctx, operation_id: &str, text: &str) -> AppResult<String> {
        let customer: Option<String> = match &x.session {
            Some(sid) => self
                .db
                .read(|c| {
                    Ok(c.query_row("SELECT customer_id FROM wa_order_sessions WHERE session_id=?1", [sid], |r| r.get(0)).optional()?)
                })?
                .flatten(),
            None => None,
        };
        let req = QueueRequest {
            operation_id: operation_id.into(),
            kind: "text".into(),
            to_phone: x.phone.clone(),
            customer_id: customer,
            sale_id: None,
            delivery_id: None,
            review_id: None,
            lang: Some(if x.arabic { "ar" } else { "en" }.into()),
            text: Some(text.chars().take(3000).collect()),
            document_b64: None,
            document_name: None,
        };
        Ok(self.wa_queue_as(s, req)?.message_id)
    }

    // ------------------------------------------------------- status notices

    /// Send customers of WhatsApp orders the status changes from the order
    /// event feed (confirmed, cancelled, on the way, delivered, not
    /// delivered). Each event is sent at most once; the feed position is kept
    /// in `order_event_cursors`. While the autopilot is off the position just
    /// follows the feed, so turning it on never sends old news.
    pub fn wa_autopilot_notify(&self, limit: i64) -> AppResult<usize> {
        let on = self.features()?.is_on("orders.whatsapp_autopilot");
        let cursor: Option<i64> = self.db.read(|c| {
            Ok(c.query_row("SELECT cursor_seq FROM order_event_cursors WHERE consumer=?1", [NOTIFIER], |r| r.get(0)).optional()?)
        })?;
        let head: i64 = self.db.read(|c| Ok(c.query_row("SELECT COALESCE(MAX(seq),0) FROM order_events", [], |r| r.get(0))?))?;
        let set_cursor = |seq: i64| {
            self.db.write(|tx| {
                tx.execute(
                    "INSERT INTO order_event_cursors(consumer, cursor_seq, updated_at) VALUES (?1,?2,?3)
                     ON CONFLICT(consumer) DO UPDATE SET cursor_seq=MAX(cursor_seq, ?2), updated_at=?3",
                    params![NOTIFIER, seq, now()],
                )?;
                Ok(())
            })
        };
        if !on {
            set_cursor(head)?;
            return Ok(0);
        }
        // First run: start a few minutes back, so a change made just before
        // the first pass is still announced, but nothing older.
        let cursor = match cursor {
            Some(c) => c,
            None => {
                let since = time::fmt(time::now() - chrono::Duration::minutes(FIRST_RUN_LOOKBACK_MINUTES));
                let start: i64 = self.db.read(|c| {
                    Ok(c.query_row("SELECT COALESCE(MAX(seq),0) FROM order_events WHERE created_at < ?1", [&since], |r| r.get(0))?)
                })?;
                set_cursor(start)?;
                start
            }
        };
        let events: Vec<(i64, String, String, Option<String>, Option<String>, String)> = self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT seq, event_id, kind, order_id, delivery_id, created_at FROM order_events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
            )?;
            let r = st
                .query_map(params![cursor, limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(r)
        })?;
        if events.is_empty() {
            return Ok(0);
        }
        let s = self.system_session(AUTOPILOT_USER, "WhatsApp autopilot", PERMISSIONS)?;
        let mut sent = 0;
        for (seq, event_id, kind, order_id, delivery_id, created_at) in events {
            let fresh = time::parse(&created_at).map(|t| (time::now() - t).num_hours() < NOTIFY_MAX_AGE_HOURS).unwrap_or(true);
            if fresh {
                if let Some(o) = &order_id {
                    match self.autopilot_notice(&s, &event_id, &kind, o, delivery_id.as_deref()) {
                        Ok(true) => sent += 1,
                        Ok(false) => {}
                        // A notice that cannot be queued is logged and skipped:
                        // one bad order must not hold every later customer.
                        Err(e) => tracing::warn!(seq, error = %e.message, "WhatsApp status notice not queued"),
                    }
                }
            }
            set_cursor(seq)?;
        }
        Ok(sent)
    }

    fn autopilot_notice(&self, s: &Session, event_id: &str, kind: &str, order_id: &str, delivery_id: Option<&str>) -> AppResult<bool> {
        let o: Option<(String, Option<String>, Option<String>, String, Option<String>, Option<i64>, i64)> = self.db.read(|c| {
            Ok(c.query_row(
                "SELECT order_number, phone, wa_session_id, channel, customer_id, delivery_fee_minor, delivery_wanted FROM digital_orders WHERE order_id=?1",
                [order_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .optional()?)
        })?;
        let Some((number, phone, wa_session, channel, customer, fee, delivery_wanted)) = o else { return Ok(false) };
        // Only orders that came in on WhatsApp, to the number they came from.
        let (Some(phone), Some(wa_session)) = (phone.filter(|p| !p.is_empty()), wa_session) else { return Ok(false) };
        if channel != "whatsapp" {
            return Ok(false);
        }
        let (chat, quoted): (String, i64) = self.db.read(|c| {
            Ok(c.query_row(
                "SELECT chat, (SELECT COUNT(*) FROM wa_order_events e WHERE e.session_id=w.session_id AND e.kind='reply_queued')
                   FROM wa_order_sessions w WHERE w.session_id=?1",
                [&wa_session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })?;
        let arabic = self.db.read(|c| is_arabic_chat(c, &chat))?;
        let op = format!("autonotify-{event_id}");
        let (msg_kind, text): (&str, Option<String>) = match kind {
            "order.confirmed" => {
                let total = self.db.read(|c| {
                    let branch_id: String =
                        c.query_row("SELECT branch_id FROM digital_orders WHERE order_id=?1", [order_id], |r| r.get(0))?;
                    Ok(totals(c, order_id, &branch_id)?.0 + fee.unwrap_or(0))
                })?;
                let note = self.db.read(|c| payment_note(c, arabic))?;
                let next = if delivery_wanted == 1 {
                    t(arabic, "We will message you when it is on the way.", "سنراسلك عندما يكون في الطريق إليك.")
                } else {
                    t(arabic, "You can collect it from the shop.", "يمكنك استلامه من المحل.")
                };
                let head = t(
                    arabic,
                    &format!("Order {number} is confirmed. Total: {} BHD.", money_str(total)),
                    &format!("تم تأكيد الطلب {number}. الإجمالي: {} BHD.", money_str(total)),
                );
                ("text", Some([head, note, next].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("\n")))
            }
            // A draft nobody quoted to the customer is cancelled quietly.
            "order.cancelled" if quoted > 0 => {
                ("text", Some(t(arabic, &format!("Order {number} has been cancelled."), &format!("تم إلغاء الطلب {number}."))))
            }
            "delivery.dispatched" => ("dispatch", None),
            "delivery.delivered" => ("delivered", None),
            "delivery.not_delivered" => (
                "text",
                Some(t(
                    arabic,
                    &format!("We could not deliver order {number} this time. A member of our team will contact you to arrange it."),
                    &format!("لم نتمكن من توصيل الطلب {number} هذه المرة. سيتواصل معك أحد أعضاء فريقنا لترتيب ذلك."),
                )),
            ),
            "delivery.cancelled" => (
                "text",
                Some(t(
                    arabic,
                    &format!("The delivery of order {number} was cancelled. A member of our team will contact you."),
                    &format!("تم إلغاء توصيل الطلب {number}. سيتواصل معك أحد أعضاء فريقنا."),
                )),
            ),
            _ => return Ok(false),
        };
        let req = QueueRequest {
            operation_id: op,
            kind: msg_kind.into(),
            to_phone: Some(phone),
            customer_id: customer,
            sale_id: None,
            delivery_id: if msg_kind == "text" { None } else { delivery_id.map(String::from) },
            review_id: None,
            lang: Some(if arabic { "ar" } else { "en" }.into()),
            text,
            document_b64: None,
            document_name: None,
        };
        let row = self.wa_queue_as(s, req)?;
        self.db.write(|tx| {
            event(
                tx,
                &wa_session,
                None,
                "status_notice_queued",
                "system",
                &json!({ "event": kind, "message_id": row.message_id }),
                Some(AUTOPILOT_USER),
            )
        })?;
        Ok(true)
    }

    /// True when a delivery's customer notices belong to the autopilot (a
    /// WhatsApp order while it is on), so the older per-delivery automatic
    /// notice does not send a second copy.
    pub(crate) fn autopilot_owns_delivery_notices(&self, delivery_id: &str) -> bool {
        if !self.features().map(|f| f.is_on("orders.whatsapp_autopilot")).unwrap_or(false) {
            return false;
        }
        self.db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM delivery_orders d JOIN digital_orders o ON o.order_id=d.order_id
                      WHERE d.delivery_id=?1 AND o.channel='whatsapp' AND o.wa_session_id IS NOT NULL",
                    [delivery_id],
                    |r| r.get::<_, i64>(0),
                )? > 0)
            })
            .unwrap_or(false)
    }
}

fn payment_received(arabic: bool) -> String {
    t(
        arabic,
        "Thank you, we received your payment screenshot. A member of our team will check it.",
        "شكراً لك، استلمنا صورة الدفع. سيتحقق منها أحد أعضاء فريقنا.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_whole_short_messages_in_english_and_arabic() {
        assert_eq!(command("STATUS"), Some(Command::Status));
        assert_eq!(command("  where is my order?? "), Some(Command::Status));
        assert_eq!(command("وين طلبي؟"), Some(Command::Status));
        assert_eq!(command("Agent"), Some(Command::Agent));
        assert_eq!(command("موظف"), Some(Command::Agent));
        assert_eq!(command("help"), Some(Command::Help));
        assert_eq!(command("مساعدة"), Some(Command::Help));
        assert_eq!(command("help me order 2 milk and 1 bread please"), None, "an order, not a command");
        assert_eq!(command("2 milk"), None);
        assert_eq!(command(""), None);
    }
}
