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
    /// Sell tracked items below zero stock (default: allowed, with a warning
    /// on the receipt screen). Off: a manager approves each shortfall.
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
            allow_negative_stock: true,
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
    /// Minutes after local midnight when the trading day ends (0 = midnight,
    /// at most 06:00). Applies to records made after it is changed.
    pub day_cutoff_minutes: i64,
    /// A counted drawer that differs from the expected amount by more than
    /// this gets a case to look into (0 = any difference).
    pub variance_case_minor: i64,
}
impl Default for ShiftSettings {
    fn default() -> Self {
        Self {
            blind_close: true,
            variance_approval_minor: 1000,
            paid_out_approval_minor: 0,
            day_cutoff_minutes: 0,
            variance_case_minor: 1000,
        }
    }
}

/// Expenses: who has to approve. An expense entered by someone who may
/// approve, or at or below this amount, is approved on entry (recorded as
/// such); anything else waits for an approver. 0 = only approvers' own.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ExpenseSettings {
    pub auto_approve_up_to_minor: i64,
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
                // Customer account (sell on credit). Needs the customer_credit module.
                t("account", "Customer account", false, false),
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
    /// `weighted_average`: receiving updates the average cost (audited).
    /// `manual`: receiving never changes cost; it is edited explicitly.
    pub costing_method: String,
    pub require_adjust_reason: bool,
    pub stocktake_blind_default: bool,
    /// Supplier documents: flag a unit cost that differs from the PO cost by
    /// at least this much (basis points; 500 = 5%).
    pub invoice_cost_variance_bp: i64,
    /// Expiry: a batch is "soon" within this many days, "urgent" within the
    /// next, and listed as "later" up to the last. One state per batch.
    pub expiry_later_days: i64,
    pub expiry_soon_days: i64,
    pub expiry_urgent_days: i64,
    /// An expiry date further away than this many years asks to confirm.
    pub expiry_max_years: i64,
    /// Waste worth more than this (at cost) needs a manager (0 = never).
    pub waste_approval_cost_minor: i64,
    /// Shrinkage (unexplained difference) always needs a manager.
    pub waste_shrinkage_needs_approval: bool,
}
impl Default for InventorySettings {
    fn default() -> Self {
        Self {
            costing_method: "weighted_average".into(),
            require_adjust_reason: true,
            stocktake_blind_default: true,
            invoice_cost_variance_bp: 500,
            expiry_later_days: 90,
            expiry_soon_days: 30,
            expiry_urgent_days: 7,
            expiry_max_years: 10,
            waste_approval_cost_minor: 20_000,
            waste_shrinkage_needs_approval: true,
        }
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
    /// Display size in percent (90, 100, 110, 125, 150); empty means 100.
    /// Zooms the whole app on this computer: text, buttons and spacing.
    pub scale: String,
}

/// Optional modules. Every flag defaults to off; a module whose flag is off
/// refuses its commands in the backend (not only in the UI).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct FeatureFlags {
    /// Multi-terminal: this computer may become a hub.
    pub hub: bool,
    /// WhatsApp (unofficial in-process Web client): pairing, inbox, sends.
    /// Older installs stored this as `whatsapp`.
    #[serde(rename = "whatsapp.enabled", alias = "whatsapp")]
    pub whatsapp_enabled: bool,
    /// Send receipts on WhatsApp after a sale commits (needs `whatsapp.enabled`).
    #[serde(rename = "whatsapp.send_receipts")]
    pub whatsapp_send_receipts: bool,
    /// Send dispatch / delivered notices (needs `whatsapp.enabled`).
    #[serde(rename = "whatsapp.delivery_notices")]
    pub whatsapp_delivery_notices: bool,
    /// Local OCR worker (bundled Tesseract). Older installs stored `ocr`.
    #[serde(rename = "ocr.enabled", alias = "ocr")]
    pub ocr_enabled: bool,
    /// Payment-screenshot reviews (needs `ocr.enabled`). Older: `payment_reviews`.
    #[serde(rename = "ocr.payment_screenshots", alias = "payment_reviews")]
    pub ocr_payment_screenshots: bool,
    /// Supplier invoice scanning to draft purchase orders (needs `ocr.enabled`).
    #[serde(rename = "ocr.supplier_invoices")]
    pub ocr_supplier_invoices: bool,
    /// OCR text of supplier invoices may be sent to the configured AI provider
    /// for line extraction (needs `ocr.supplier_invoices` and `ai.enabled`).
    /// The result is only a suggestion a person reviews.
    #[serde(rename = "ocr.ai_parse")]
    pub ocr_ai_parse: bool,
    /// AI assistant (read-only tools). Older installs stored `ai`.
    #[serde(rename = "ai.enabled", alias = "ai")]
    pub ai: bool,
    /// AI may propose changes (always preview + confirm + deterministic execution).
    #[serde(rename = "ai.mutations", alias = "ai_mutations")]
    pub ai_mutations: bool,
    /// D1: a high-risk AI proposal needs a second, different person to confirm.
    #[serde(rename = "ai.dual_control")]
    pub ai_dual_control: bool,
    /// Customer accounts: sell on account, take account payments, balances.
    #[serde(rename = "customers.credit", alias = "customer_credit")]
    pub customer_credit: bool,
    /// Optional Windows Hello step-up for sensitive actions (the PIN is still required).
    pub windows_hello: bool,
    /// Save a PDF copy of every receipt after the sale commits.
    pub pdf_receipts: bool,
    /// Check for signed updates.
    pub updates: bool,
    /// Stock locations inside a branch and stock transfers.
    #[serde(rename = "inventory.locations")]
    pub inventory_locations: bool,
    /// Loyalty points (earn on committed sales, redeem as a discount).
    #[serde(rename = "loyalty.enabled")]
    pub loyalty: bool,
    /// Digital order intake (phone / WhatsApp / web) converted to sales on a till.
    #[serde(rename = "orders.digital")]
    pub orders_digital: bool,
    /// WhatsApp messages are read into reviewable order drafts (needs
    /// `orders.digital` and `whatsapp.enabled`). Rules always; the AI
    /// provider helps only when `ai.enabled` and AI consent are on.
    #[serde(rename = "orders.whatsapp_ai")]
    pub orders_whatsapp_ai: bool,
    /// Show a few in-stock add-on suggestions to staff (never added by itself).
    #[serde(rename = "orders.whatsapp_upsell")]
    pub orders_whatsapp_upsell: bool,
    /// Several branches in one organisation (branch stock, prices, users).
    #[serde(rename = "org.multi_branch")]
    pub multi_branch: bool,
    /// Read-only owner companion page served by the hub on the LAN.
    #[serde(rename = "pwa.companion")]
    pub pwa_companion: bool,
}

impl FeatureFlags {
    pub fn is_on(&self, name: &str) -> bool {
        match name {
            "hub" => self.hub,
            "whatsapp.enabled" => self.whatsapp_enabled,
            "whatsapp.send_receipts" => self.whatsapp_enabled && self.whatsapp_send_receipts,
            "whatsapp.delivery_notices" => self.whatsapp_enabled && self.whatsapp_delivery_notices,
            "ocr.enabled" => self.ocr_enabled,
            "ocr.payment_screenshots" => self.ocr_enabled && self.ocr_payment_screenshots,
            "ocr.supplier_invoices" => self.ocr_enabled && self.ocr_supplier_invoices,
            "ocr.ai_parse" => self.ocr_enabled && self.ocr_supplier_invoices && self.ai && self.ocr_ai_parse,
            "ai.enabled" => self.ai,
            "ai.mutations" => self.ai && self.ai_mutations,
            "ai.dual_control" => self.ai && self.ai_mutations && self.ai_dual_control,
            "customers.credit" => self.customer_credit,
            "windows_hello" => self.windows_hello,
            "pdf_receipts" => self.pdf_receipts,
            "updates" => self.updates,
            "inventory.locations" => self.inventory_locations,
            "loyalty.enabled" => self.loyalty,
            "orders.digital" => self.orders_digital,
            "orders.whatsapp_ai" => self.orders_digital && self.whatsapp_enabled && self.orders_whatsapp_ai,
            "orders.whatsapp_upsell" => {
                self.orders_digital && self.whatsapp_enabled && self.orders_whatsapp_ai && self.orders_whatsapp_upsell
            }
            "org.multi_branch" => self.multi_branch,
            "pwa.companion" => self.pwa_companion,
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
    /// Send the payment acknowledgement automatically after a person confirms
    /// a payment screenshot.
    pub auto_payment_ack: bool,
    /// Queue the "order received" / "on the way" / "delivered" notice by
    /// itself when a drop is created, marked Out or Delivered. Off: a person
    /// taps Message and confirms.
    pub auto_delivery_notice: bool,
    pub receipt: MessageTemplate,
    /// Sent when the shop takes a Send order (the drop is created).
    pub received: MessageTemplate,
    pub dispatch: MessageTemplate,
    pub delivered: MessageTemplate,
    /// Payment reminder.
    pub reminder: MessageTemplate,
    /// Payment acknowledgement (after a person confirmed the payment).
    pub payment_ack: MessageTemplate,
}

impl Default for WhatsAppSettings {
    fn default() -> Self {
        Self {
            default_lang: "en".into(),
            attach_pdf: true,
            send_read_receipts: true,
            auto_payment_ack: false,
            auto_delivery_notice: false,
            receipt: MessageTemplate {
                en: "Thank you for shopping at {business}.\nReceipt {receipt}\nTotal: {total}\nDate: {date}".into(),
                ar: "شكراً لتسوقك من {business}.\nالإيصال {receipt}\nالإجمالي: {total}\nالتاريخ: {date}".into(),
            },
            received: MessageTemplate {
                en: "Hello {customer}, {business} has your order {ticket}.\nTotal: {total}\nWe will message you when it is on its way."
                    .into(),
                ar: "مرحباً {customer}، استلم {business} طلبك {ticket}.\nالإجمالي: {total}\nسنراسلك عندما يكون في الطريق إليك.".into(),
            },
            dispatch: MessageTemplate {
                en: "Hello {customer}, your order {delivery} from {business} is on its way.\nAmount due: {amount}".into(),
                ar: "مرحباً {customer}، طلبك {delivery} من {business} في الطريق إليك.\nالمبلغ المستحق: {amount}".into(),
            },
            delivered: MessageTemplate {
                en: "Hello {customer}, your order {delivery} from {business} has been delivered. Thank you.".into(),
                ar: "مرحباً {customer}، تم توصيل طلبك {delivery} من {business}. شكراً لك.".into(),
            },
            reminder: MessageTemplate {
                en: "Hello {customer}, this is a reminder from {business}: {amount} is due for order {delivery}. Thank you.".into(),
                ar: "مرحباً {customer}، تذكير من {business}: المبلغ {amount} مستحق للطلب {delivery}. شكراً لك.".into(),
            },
            payment_ack: MessageTemplate {
                en: "Hello {customer}, {business} has received your payment of {amount} (ref {reference}). Thank you.".into(),
                ar: "مرحباً {customer}، استلم {business} دفعتك بمبلغ {amount} (المرجع {reference}). شكراً لك.".into(),
            },
        }
    }
}

/// Loyalty points (module `loyalty.enabled`). Amounts are in minor units
/// (fils for BHD).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LoyaltySettings {
    /// One point per this much paid (after discounts), e.g. 100 = 1 point per 0.100.
    pub earn_minor_per_point: i64,
    /// Value of one point when redeemed as a discount, e.g. 5 = 0.005.
    pub redeem_minor_per_point: i64,
    /// Lines that already carry a discount earn no points.
    pub exclude_discounted_lines: bool,
    /// Smallest redemption.
    pub min_redeem_points: i64,
}
impl Default for LoyaltySettings {
    fn default() -> Self {
        Self { earn_minor_per_point: 100, redeem_minor_per_point: 5, exclude_discounted_lines: true, min_redeem_points: 100 }
    }
}
pub const KEY_LOYALTY: &str = "loyalty";
pub const KEY_EXPENSES: &str = "expenses";

/// Where to look for signed updates. The verifying key is built into the app.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct UpdateSettings {
    /// URL of the signed update manifest (latest.json). Empty = not configured.
    pub feed_url: String,
    /// Check once a day while the app runs.
    pub auto_check: bool,
}

/// One row of the block → area list: blocks `from..=to` are in `area`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockArea {
    pub from: u32,
    pub to: u32,
    pub area: String,
}

/// Delivery settings. The block list is only a starting point: the shop's own
/// past drops and customers decide a block's area first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DeliverySettings {
    pub blocks: Vec<BlockArea>,
    /// Delivery zones with their fee (WhatsApp orders, digital orders).
    /// Empty: no fee is suggested.
    pub zones: Vec<DeliveryZone>,
}

/// A delivery zone: blocks and/or area names, and the fee the shop charges.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DeliveryZone {
    pub zone_id: String,
    pub name: String,
    /// Block ranges in the zone ("256-258", as from/to).
    pub blocks: Vec<BlockRange>,
    /// Area names in the zone (matched without case).
    pub areas: Vec<String>,
    pub fee_minor: i64,
    /// Free delivery from this order subtotal (fils); None = never free.
    pub free_over_minor: Option<i64>,
    pub active: bool,
}
impl Default for DeliveryZone {
    fn default() -> Self {
        Self {
            zone_id: String::new(),
            name: String::new(),
            blocks: vec![],
            areas: vec![],
            fee_minor: 0,
            free_over_minor: None,
            active: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BlockRange {
    pub from: u32,
    pub to: u32,
}
impl Default for DeliverySettings {
    fn default() -> Self {
        let b = |from, to, area: &str| BlockArea { from, to, area: area.into() };
        Self {
            blocks: vec![
                b(256, 258, "Amwaj"),
                b(340, 342, "Juffair"),
                b(428, 428, "Seef"),
                b(436, 436, "Seef"),
                b(801, 841, "Isa Town"),
                b(1201, 1217, "Hamad Town"),
            ],
            zones: vec![],
        }
    }
}
pub const KEY_DELIVERY: &str = "delivery";

pub const KEY_UPDATES: &str = "updates";
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
    KEY_LOYALTY,
    KEY_DELIVERY,
    KEY_EXPENSES,
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
        KEY_SHIFT => {
            let v: ShiftSettings = serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            if !(0..=crate::time::MAX_CUTOFF_MINUTES).contains(&v.day_cutoff_minutes) {
                return Err(AppError::validation("The trading day must end between midnight and 06:00."));
            }
            if v.variance_case_minor < 0 || v.variance_approval_minor < 0 || v.paid_out_approval_minor < 0 {
                return Err(AppError::validation("The amount cannot be negative."));
            }
            serde_json::to_value(v)?
        }
        KEY_FEATURES => roundtrip::<FeatureFlags>(value)?,
        KEY_EXPENSES => {
            let v: ExpenseSettings = serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            if v.auto_approve_up_to_minor < 0 {
                return Err(AppError::validation("The amount cannot be negative."));
            }
            serde_json::to_value(v)?
        }
        KEY_UPDATES => {
            let u: UpdateSettings = serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            let f = u.feed_url.trim();
            if !(f.is_empty() || f.starts_with("https://") || f.starts_with("http://127.0.0.1") || f.starts_with("http://localhost")) {
                return Err(AppError::validation("The update address must use https://."));
            }
            serde_json::to_value(UpdateSettings { feed_url: f.to_string(), auto_check: u.auto_check })?
        }
        KEY_LOYALTY => {
            let l: LoyaltySettings = serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            if !(1..=1_000_000).contains(&l.earn_minor_per_point) || !(1..=1_000_000).contains(&l.redeem_minor_per_point) {
                return Err(AppError::validation("Earn and redeem rates must be positive amounts."));
            }
            if !(0..=1_000_000).contains(&l.min_redeem_points) {
                return Err(AppError::validation("The minimum redemption must be between 0 and 1,000,000 points."));
            }
            serde_json::to_value(l)?
        }
        KEY_WHATSAPP => {
            let w: WhatsAppSettings = serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            if !["en", "ar"].contains(&w.default_lang.as_str()) {
                return Err(AppError::validation("Default message language must be English or Arabic."));
            }
            for t in [&w.receipt, &w.received, &w.dispatch, &w.delivered, &w.reminder, &w.payment_ack] {
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
            if !["weighted_average", "manual"].contains(&s.costing_method.as_str()) {
                return Err(AppError::validation("Costing method must be weighted average or manual."));
            }
            if !(0 < s.expiry_urgent_days
                && s.expiry_urgent_days <= s.expiry_soon_days
                && s.expiry_soon_days <= s.expiry_later_days
                && s.expiry_later_days <= 3650)
            {
                return Err(AppError::validation("Expiry days must go up: urgent, then soon, then later (at most 3650)."));
            }
            if !(1..=100).contains(&s.expiry_max_years) || s.waste_approval_cost_minor < 0 {
                return Err(AppError::validation("Check the expiry and waste limits."));
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
        KEY_DELIVERY => {
            let mut d: DeliverySettings =
                serde_json::from_value(value).map_err(|e| AppError::validation(format!("Invalid settings: {e}")))?;
            if d.blocks.len() > 500 {
                return Err(AppError::validation("The block list can hold up to 500 rows."));
            }
            if d.zones.len() > 100 {
                return Err(AppError::validation("Up to 100 delivery zones."));
            }
            let mut ids = std::collections::BTreeSet::new();
            for z in &mut d.zones {
                z.name = z.name.trim().to_string();
                if z.zone_id.trim().is_empty() {
                    z.zone_id = crate::ids::new_id();
                }
                if !ids.insert(z.zone_id.clone()) {
                    return Err(AppError::validation("Two delivery zones have the same id."));
                }
                if z.name.is_empty() || z.name.chars().count() > 80 {
                    return Err(AppError::validation("Each delivery zone needs a name (up to 80 characters)."));
                }
                if !(0..=100_000).contains(&z.fee_minor) || z.free_over_minor.is_some_and(|f| !(0..=100_000_000).contains(&f)) {
                    return Err(AppError::validation(format!("{}: the fee is out of range.", z.name)));
                }
                if z.blocks.iter().any(|r| r.from == 0 || r.to > 9999 || r.from > r.to) {
                    return Err(AppError::validation(format!("{}: block ranges are between 1 and 9999, lowest first.", z.name)));
                }
                z.areas = z.areas.iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
                if z.blocks.is_empty() && z.areas.is_empty() {
                    return Err(AppError::validation(format!("{}: add at least one block range or area.", z.name)));
                }
            }
            for r in &mut d.blocks {
                r.area = r.area.trim().to_string();
                if r.area.is_empty() || r.area.chars().count() > 80 {
                    return Err(AppError::validation("Each block row needs an area name (up to 80 characters)."));
                }
                if r.from == 0 || r.to > 9999 || r.from > r.to {
                    return Err(AppError::validation(format!(
                        "Blocks {}–{}: enter a range between 1 and 9999, lowest first.",
                        r.from, r.to
                    )));
                }
            }
            d.blocks.sort_by_key(|r| r.from);
            if let Some(w) = d.blocks.windows(2).find(|w| w[1].from <= w[0].to) {
                return Err(AppError::validation(format!(
                    "Blocks {}–{} ({}) overlap blocks {}–{} ({}).",
                    w[0].from, w[0].to, w[0].area, w[1].from, w[1].to, w[1].area
                )));
            }
            serde_json::to_value(d)?
        }
        _ => return Err(AppError::validation(format!("'{key}' is not an editable setting."))),
    };
    Ok(v)
}
