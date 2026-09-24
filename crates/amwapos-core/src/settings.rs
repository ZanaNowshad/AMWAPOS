//! Typed store settings persisted as JSON documents in the `settings` table.
//!
//! Keys starting with `local.` are device-local and never replicated.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::time;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PosSettings {
    pub allow_negative_stock: bool,
    pub allow_custom_item: bool,
    /// Largest discount a user with only `pos.discount` may give, in basis points of the line/cart.
    pub cashier_max_discount_bp: i64,
    pub idle_lock_minutes: i64,
    pub receipt_auto_print: bool,
    pub return_to_scan_seconds: i64,
    pub scan_sound: bool,
    pub duplicate_scan_window_ms: i64,
}
impl Default for PosSettings {
    fn default() -> Self {
        Self {
            allow_negative_stock: false,
            allow_custom_item: true,
            cashier_max_discount_bp: 1000,
            idle_lock_minutes: 10,
            receipt_auto_print: true,
            return_to_scan_seconds: 4,
            scan_sound: true,
            duplicate_scan_window_ms: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShiftSettings {
    /// Hide the expected drawer amount until the cashier has counted.
    pub blind_close: bool,
    /// Absolute variance above which a manager must acknowledge the close.
    pub variance_approval_minor: i64,
    /// Cash events above this amount need a manager.
    pub paid_out_approval_minor: i64,
}
impl Default for ShiftSettings {
    fn default() -> Self {
        Self { blind_close: true, variance_approval_minor: 1000, paid_out_approval_minor: 0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TenderConfig {
    pub method: String,
    pub label: String,
    pub enabled: bool,
    pub requires_reference: bool,
    /// Only cash may be over-tendered to give change.
    pub allows_change: bool,
}
impl Default for TenderConfig {
    fn default() -> Self {
        Self { method: String::new(), label: String::new(), enabled: true, requires_reference: false, allows_change: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PaymentSettings {
    pub tenders: Vec<TenderConfig>,
}
impl Default for PaymentSettings {
    fn default() -> Self {
        let t = |m: &str, l: &str, en: bool, change: bool| TenderConfig {
            method: m.into(),
            label: l.into(),
            enabled: en,
            requires_reference: false,
            allows_change: change,
        };
        Self {
            tenders: vec![
                t("cash", "Cash", true, true),
                t("card", "Card", true, false),
                t("benefitpay", "BenefitPay", true, false),
                t("bank_transfer", "Bank Transfer", false, false),
                // A manual wallet (e.g. a mobile wallet) recorded like any other tender; no live settlement.
                t("wallet", "Wallet", false, false),
            ],
        }
    }
}
/// Payment settings with any built-in tender added since the store was set up
/// appended (disabled), so new tender types appear in Settings for old stores.
pub fn payments(c: &Connection) -> AppResult<PaymentSettings> {
    let mut p: PaymentSettings = get(c, KEY_PAYMENTS)?;
    for d in PaymentSettings::default().tenders {
        if p.tender(&d.method).is_none() {
            p.tenders.push(TenderConfig { enabled: false, ..d });
        }
    }
    Ok(p)
}

impl PaymentSettings {
    pub fn tender(&self, method: &str) -> Option<&TenderConfig> {
        self.tenders.iter().find(|t| t.method == method)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReceiptSettings {
    pub header_lines: Vec<String>,
    pub footer_lines: Vec<String>,
    pub show_vat_number: bool,
    pub show_cr_number: bool,
    pub show_cashier: bool,
    pub show_barcode: bool,
    pub paper_width_mm: i64,
    pub title: String,
    /// "en" (English labels) or "bilingual" (English / Arabic labels).
    pub language: String,
}
impl Default for ReceiptSettings {
    fn default() -> Self {
        Self {
            header_lines: vec![],
            footer_lines: vec!["Thank you for shopping with us".into()],
            show_vat_number: true,
            show_cr_number: true,
            show_cashier: true,
            show_barcode: false,
            paper_width_mm: 80,
            title: "TAX INVOICE".into(),
            language: "en".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SecuritySettings {
    pub pin_min_length: usize,
    pub pin_max_length: usize,
    pub max_failed_attempts: i64,
    pub lockout_minutes: i64,
}
impl Default for SecuritySettings {
    fn default() -> Self {
        Self { pin_min_length: 4, pin_max_length: 8, max_failed_attempts: 5, lockout_minutes: 15 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InventorySettings {
    /// The single costing method implemented in v1.
    pub costing_method: String,
    pub require_adjust_reason: bool,
    pub stocktake_blind_default: bool,
}
impl Default for InventorySettings {
    fn default() -> Self {
        Self { costing_method: "weighted_average".into(), require_adjust_reason: true, stocktake_blind_default: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrinterSettings {
    /// none | network | windows | file
    pub mode: String,
    /// host:port for network, printer name for windows, path for file.
    pub target: String,
    pub paper_width_mm: i64,
    pub cut: bool,
    pub drawer_pulse: bool,
    pub code_page: String,
}
impl Default for PrinterSettings {
    fn default() -> Self {
        Self { mode: "none".into(), target: String::new(), paper_width_mm: 80, cut: true, drawer_pulse: true, code_page: "cp437".into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupSettings {
    pub directory: String,
    pub automatic: bool,
    pub interval_hours: i64,
    pub keep: i64,
}
impl Default for BackupSettings {
    fn default() -> Self {
        Self { directory: String::new(), automatic: true, interval_hours: 24, keep: 14 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppearanceSettings {
    pub theme: String,
    pub density: String,
    pub cashier_font: String,
}

/// Optional modules. Every flag defaults to off; a module whose flag is off
/// refuses its commands in the backend (not only in the UI).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct FeatureFlags {
    /// Multi-terminal: this computer may become a hub.
    pub hub: bool,
    /// WhatsApp sidecar (pairing, messages, receipts, delivery updates).
    pub whatsapp: bool,
    /// Local OCR (invoice scan, payment screenshots).
    pub ocr: bool,
    /// Payment-screenshot review pipeline (needs OCR for extraction).
    pub payment_reviews: bool,
    /// AI assistant (read-only tools).
    pub ai: bool,
    /// AI may propose changes (always preview + confirm + deterministic execution).
    pub ai_mutations: bool,
    /// Customer accounts: sell on account, take account payments, balances.
    pub customer_credit: bool,
    /// Optional Windows Hello step-up for sensitive actions (the PIN is still required).
    pub windows_hello: bool,
    /// Save a PDF copy of every receipt after the sale commits.
    pub pdf_receipts: bool,
    /// Check for signed updates.
    pub updates: bool,
}

impl FeatureFlags {
    pub fn is_on(&self, name: &str) -> bool {
        match name {
            "hub" => self.hub,
            "whatsapp" => self.whatsapp,
            "ocr" => self.ocr,
            "payment_reviews" => self.payment_reviews,
            "ai" => self.ai,
            "ai_mutations" => self.ai && self.ai_mutations,
            "customer_credit" => self.customer_credit,
            "windows_hello" => self.windows_hello,
            "pdf_receipts" => self.pdf_receipts,
            "updates" => self.updates,
            _ => false,
        }
    }
}

/// One message template in English and Arabic. Placeholders in braces, e.g.
/// `{customer}`, `{receipt}`, `{total}`; unknown placeholders are left as is.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct MessageTemplate {
    pub en: String,
    pub ar: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WhatsAppSettings {
    /// Language used when the customer has no preference.
    pub default_lang: String,
    /// Attach the PDF receipt when sending a receipt message.
    pub attach_pdf: bool,
    /// Mark inbound messages as read on the phone when opened in AMWAPOS.
    pub send_read_receipts: bool,
    pub receipt: MessageTemplate,
    pub dispatch: MessageTemplate,
    pub reminder: MessageTemplate,
}

impl Default for WhatsAppSettings {
    fn default() -> Self {
        Self {
            default_lang: "en".into(),
            attach_pdf: true,
            send_read_receipts: true,
            receipt: MessageTemplate {
                en: "Thank you for shopping at {business}.\nReceipt {receipt}\nTotal: {total}\nDate: {date}".into(),
                ar: "شكراً لتسوقك من {business}.\nالإيصال {receipt}\nالإجمالي: {total}\nالتاريخ: {date}".into(),
            },
            dispatch: MessageTemplate {
                en: "Hello {customer}, your order {delivery} from {business} is on its way.\nAmount due: {amount}".into(),
                ar: "مرحباً {customer}، طلبك {delivery} من {business} في الطريق إليك.\nالمبلغ المستحق: {amount}".into(),
            },
            reminder: MessageTemplate {
                en: "Hello {customer}, this is a reminder from {business}: {amount} is due for order {delivery}. Thank you.".into(),
                ar: "مرحباً {customer}، تذكير من {business}: المبلغ {amount} مستحق للطلب {delivery}. شكراً لك.".into(),
            },
        }
    }
}

pub const KEY_FEATURES: &str = "features";
pub const KEY_WHATSAPP: &str = "whatsapp";
pub const KEY_POS: &str = "pos";
pub const KEY_SHIFT: &str = "shift";
pub const KEY_PAYMENTS: &str = "payments";
pub const KEY_RECEIPT: &str = "receipt";
pub const KEY_SECURITY: &str = "security";
pub const KEY_INVENTORY: &str = "inventory";
pub const KEY_PRINTER: &str = "local.printer";
pub const KEY_BACKUP: &str = "local.backup";
pub const KEY_APPEARANCE: &str = "local.appearance";
pub const KEY_DEVICE: &str = "local.device";
pub const KEY_SETUP_COMPLETE: &str = "local.setup_complete";

pub const EDITABLE_KEYS: &[&str] = &[
    KEY_POS,
    KEY_SHIFT,
    KEY_PAYMENTS,
    KEY_RECEIPT,
    KEY_SECURITY,
    KEY_INVENTORY,
    KEY_PRINTER,
    KEY_BACKUP,
    KEY_APPEARANCE,
    KEY_FEATURES,
    KEY_WHATSAPP,
];

pub fn get<T: DeserializeOwned + Default>(conn: &Connection, key: &str) -> AppResult<T> {
    let v: Option<String> = conn.query_row("SELECT value_json FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?;
    match v {
        Some(s) => serde_json::from_str(&s).map_err(|e| AppError::internal(format!("Setting '{key}' is unreadable: {e}"))),
        None => Ok(T::default()),
    }
}

pub fn get_raw(conn: &Connection, key: &str) -> AppResult<Option<serde_json::Value>> {
    let v: Option<String> = conn.query_row("SELECT value_json FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?;
    Ok(match v {
        Some(s) => Some(serde_json::from_str(&s)?),
        None => None,
    })
}

pub fn put<T: Serialize>(conn: &Connection, key: &str, value: &T, user: Option<&str>) -> AppResult<()> {
    let json = serde_json::to_string(value)?;
    conn.execute(
        "INSERT INTO settings(key, value_json, updated_at, updated_by) VALUES (?1,?2,?3,?4)
         ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json, updated_at=excluded.updated_at, updated_by=excluded.updated_by
         WHERE settings.value_json IS NOT excluded.value_json",
        params![key, json, time::now_str(), user],
    )?;
    Ok(())
}

/// Validate a settings document before saving. Returns the normalized JSON.
pub fn validate(key: &str, value: serde_json::Value) -> AppResult<serde_json::Value> {
    fn roundtrip<T: DeserializeOwned + Serialize>(v: serde_json::Value) -> AppResult<serde_json::Value> {
        let t: T = serde_json::from_value(v).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
        Ok(serde_json::to_value(t)?)
    }
    let v = match key {
        KEY_POS => {
            let p: PosSettings = serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            if !(0..=10000).contains(&p.cashier_max_discount_bp) {
                return Err(AppError::validation("Cashier discount limit must be between 0% and 100%."));
            }
            if !(0..=240).contains(&p.idle_lock_minutes) {
                return Err(AppError::validation("Idle lock must be between 0 and 240 minutes."));
            }
            serde_json::to_value(p)?
        }
        KEY_SHIFT => roundtrip::<ShiftSettings>(value)?,
        KEY_FEATURES => roundtrip::<FeatureFlags>(value)?,
        KEY_WHATSAPP => {
            let w: WhatsAppSettings = serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            if !["en", "ar"].contains(&w.default_lang.as_str()) {
                return Err(AppError::validation("Default message language must be English or Arabic."));
            }
            for t in [&w.receipt, &w.dispatch, &w.reminder] {
                if t.en.trim().is_empty() || t.ar.trim().is_empty() || t.en.len() > 2000 || t.ar.len() > 2000 {
                    return Err(AppError::validation("Every message template needs English and Arabic text (up to 2000 characters)."));
                }
            }
            serde_json::to_value(w)?
        }
        KEY_PAYMENTS => {
            let p: PaymentSettings = serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            if !p.tenders.iter().any(|t| t.enabled) {
                return Err(AppError::validation("At least one payment method must be enabled."));
            }
            for t in &p.tenders {
                if t.allows_change && t.method != "cash" {
                    return Err(AppError::validation("Only cash may give change."));
                }
            }
            serde_json::to_value(p)?
        }
        KEY_RECEIPT => {
            let r: ReceiptSettings = serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            if r.paper_width_mm != 80 && r.paper_width_mm != 58 {
                return Err(AppError::validation("Paper width must be 80 mm or 58 mm."));
            }
            if r.language != "en" && r.language != "bilingual" {
                return Err(AppError::validation("Receipt language must be English or bilingual (English / Arabic)."));
            }
            serde_json::to_value(r)?
        }
        KEY_SECURITY => {
            let s: SecuritySettings = serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            if s.pin_min_length < 4 || s.pin_max_length > 12 || s.pin_min_length > s.pin_max_length {
                return Err(AppError::validation("PIN length must be between 4 and 12 digits."));
            }
            if !(1..=20).contains(&s.max_failed_attempts) || !(1..=1440).contains(&s.lockout_minutes) {
                return Err(AppError::validation("Lockout policy values are out of range."));
            }
            serde_json::to_value(s)?
        }
        KEY_INVENTORY => {
            let s: InventorySettings = serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            if s.costing_method != "weighted_average" {
                return Err(AppError::validation("Only weighted-average costing is supported in this version."));
            }
            serde_json::to_value(s)?
        }
        KEY_PRINTER => {
            let s: PrinterSettings = serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            if !["none", "network", "windows", "file"].contains(&s.mode.as_str()) {
                return Err(AppError::validation("Unknown printer mode."));
            }
            if s.paper_width_mm != 80 && s.paper_width_mm != 58 {
                return Err(AppError::validation("Paper width must be 80 mm or 58 mm."));
            }
            serde_json::to_value(s)?
        }
        KEY_BACKUP => {
            let s: BackupSettings = serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            if !(1..=720).contains(&s.interval_hours) || !(1..=365).contains(&s.keep) {
                return Err(AppError::validation("Backup schedule values are out of range."));
            }
            serde_json::to_value(s)?
        }
        KEY_APPEARANCE => roundtrip::<AppearanceSettings>(value)?,
        _ => return Err(AppError::validation(format!("'{key}' is not an editable setting."))),
    };
    Ok(v)
}
