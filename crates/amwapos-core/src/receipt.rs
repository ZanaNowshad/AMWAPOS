//! Receipt documents: built from committed records only (never from current
//! catalogue data) and rendered to plain text (preview / file printer) or
//! ESC/POS bytes (thermal printers).

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};
use crate::money::{format_decimal, format_qty};
use crate::raster;
use crate::sales::load_sale_detail;
use crate::settings::{self, ReceiptSettings};
use crate::time;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    Text { text: String, align: Align, bold: bool, large: bool },
    Pair { left: String, right: String, bold: bool, large: bool },
    Rule,
    Feed { lines: u8 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReceiptDoc {
    pub width_chars: usize,
    pub blocks: Vec<Block>,
}

pub fn width_for(paper_mm: i64) -> usize {
    if paper_mm <= 58 {
        32
    } else {
        48
    }
}

impl ReceiptDoc {
    fn new(width: usize) -> Self {
        Self { width_chars: width, blocks: vec![] }
    }
    fn center(&mut self, t: impl Into<String>, bold: bool, large: bool) {
        self.blocks.push(Block::Text { text: t.into(), align: Align::Center, bold, large });
    }
    fn left(&mut self, t: impl Into<String>) {
        self.blocks.push(Block::Text { text: t.into(), align: Align::Left, bold: false, large: false });
    }
    fn pair(&mut self, l: impl Into<String>, r: impl Into<String>) {
        self.blocks.push(Block::Pair { left: l.into(), right: r.into(), bold: false, large: false });
    }
    fn pair_b(&mut self, l: impl Into<String>, r: impl Into<String>, large: bool) {
        self.blocks.push(Block::Pair { left: l.into(), right: r.into(), bold: true, large });
    }
    fn rule(&mut self) {
        self.blocks.push(Block::Rule);
    }

    /// Plain-text rendering with exact column layout (used for previews and
    /// the file printer). Large text is rendered at normal width here.
    pub fn to_text(&self) -> String {
        let w = self.width_chars;
        let mut out = String::new();
        for b in &self.blocks {
            match b {
                Block::Text { text, align, .. } => {
                    for line in wrap(text, w) {
                        out.push_str(&aligned(&line, w, *align));
                        out.push('\n');
                    }
                }
                Block::Pair { left, right, .. } => {
                    out.push_str(&pair_line(left, right, w));
                    out.push('\n');
                }
                Block::Rule => {
                    out.push_str(&"-".repeat(w));
                    out.push('\n');
                }
                Block::Feed { lines } => {
                    for _ in 0..*lines {
                        out.push('\n');
                    }
                }
            }
        }
        out
    }

    /// The whole receipt as one image (every line rasterized), used for PDF
    /// copies. Same layout and fonts as the printed receipt.
    pub fn to_bitmap(&self) -> raster::Bitmap {
        let w = self.width_chars;
        let px = w * raster::DOTS_PER_CHAR;
        let (_, line_h) = raster::metrics(false);
        let mut parts: Vec<raster::Bitmap> = vec![raster::Bitmap::new(px, 16)];
        for blk in &self.blocks {
            match blk {
                Block::Text { text, align, bold, large } => {
                    for line in raster::wrap(text, px, *bold, *large) {
                        let place = match align {
                            Align::Center => raster::Place::Center,
                            Align::Right => raster::Place::Right,
                            Align::Left if raster::is_rtl(&line) => raster::Place::Right,
                            Align::Left => raster::Place::Left,
                        };
                        parts.push(raster::render(px, &[(&line, place)], *bold, *large));
                    }
                }
                Block::Pair { left, right, bold, large } => {
                    let (size, _) = raster::metrics(*large);
                    let room = px as f32 - raster::measure(right, size, *bold) - raster::DOTS_PER_CHAR as f32;
                    let left = raster::fit(left, room.max(0.0), *bold, *large);
                    parts.push(raster::render(px, &[(&left, raster::Place::Left), (right, raster::Place::Right)], *bold, *large));
                }
                Block::Rule => {
                    let mut bm = raster::Bitmap::new(px, line_h / 2);
                    let y = line_h / 4;
                    for x in 0..bm.width {
                        if (x / 6) % 2 == 0 {
                            bm.set(x, y);
                        }
                    }
                    parts.push(bm);
                }
                Block::Feed { lines } => parts.push(raster::Bitmap::new(px, line_h * *lines as usize)),
            }
        }
        parts.push(raster::Bitmap::new(px, 24));
        raster::Bitmap::stack(&parts)
    }

    /// ESC/POS encoding. ASCII lines use the printer's text mode; any line
    /// with Arabic or other non-ASCII text is shaped and sent as a raster
    /// image (`raster`), so it prints correctly on any ESC/POS printer.
    pub fn to_escpos(&self, cut: bool) -> Vec<u8> {
        let w = self.width_chars;
        let px = w * raster::DOTS_PER_CHAR;
        let mut b: Vec<u8> = vec![0x1B, 0x40]; // ESC @ initialize
        b.extend_from_slice(&[0x1B, 0x74, 0x00]); // code page PC437
        for blk in &self.blocks {
            match blk {
                Block::Text { text, align, bold, large } if raster::needs_raster(text) => {
                    b.extend_from_slice(&[0x1B, 0x61, 0x00]);
                    for line in raster::wrap(text, px, *bold, *large) {
                        let place = match align {
                            Align::Center => raster::Place::Center,
                            Align::Right => raster::Place::Right,
                            // "Left" means the start of the line: the right edge for Arabic.
                            Align::Left if raster::is_rtl(&line) => raster::Place::Right,
                            Align::Left => raster::Place::Left,
                        };
                        b.extend(raster::render(px, &[(&line, place)], *bold, *large).to_escpos());
                    }
                }
                Block::Pair { left, right, bold, large } if raster::needs_raster(left) || raster::needs_raster(right) => {
                    b.extend_from_slice(&[0x1B, 0x61, 0x00]);
                    let (size, _) = raster::metrics(*large);
                    let room = px as f32 - raster::measure(right, size, *bold) - raster::DOTS_PER_CHAR as f32;
                    let left = raster::fit(left, room.max(0.0), *bold, *large);
                    b.extend(raster::render(px, &[(&left, raster::Place::Left), (right, raster::Place::Right)], *bold, *large).to_escpos());
                }
                Block::Text { text, align, bold, large } => {
                    b.extend_from_slice(&[
                        0x1B,
                        0x61,
                        match align {
                            Align::Left => 0,
                            Align::Center => 1,
                            Align::Right => 2,
                        },
                    ]);
                    b.extend_from_slice(&[0x1B, 0x45, *bold as u8]);
                    b.extend_from_slice(&[0x1D, 0x21, if *large { 0x11 } else { 0x00 }]);
                    let cw = if *large { w / 2 } else { w };
                    for line in wrap(text, cw) {
                        b.extend(ascii(&line));
                        b.push(b'\n');
                    }
                    b.extend_from_slice(&[0x1D, 0x21, 0x00, 0x1B, 0x45, 0x00, 0x1B, 0x61, 0x00]);
                }
                Block::Pair { left, right, bold, large } => {
                    b.extend_from_slice(&[0x1B, 0x61, 0x00, 0x1B, 0x45, *bold as u8]);
                    if *large {
                        b.extend_from_slice(&[0x1D, 0x21, 0x01]); // double height only keeps columns aligned
                    }
                    b.extend(ascii(&pair_line(left, right, w)));
                    b.push(b'\n');
                    b.extend_from_slice(&[0x1D, 0x21, 0x00, 0x1B, 0x45, 0x00]);
                }
                Block::Rule => {
                    b.extend(std::iter::repeat_n(b'-', w));
                    b.push(b'\n');
                }
                Block::Feed { lines } => {
                    b.extend_from_slice(&[0x1B, 0x64, *lines]);
                }
            }
        }
        b.extend_from_slice(&[0x1B, 0x64, 0x04]); // feed 4 lines
        if cut {
            b.extend_from_slice(&[0x1D, 0x56, 0x42, 0x00]); // partial cut with feed
        }
        b
    }
}

/// ESC p m t1 t2 — pulse drawer kick pin 2.
pub fn drawer_pulse_bytes() -> Vec<u8> {
    vec![0x1B, 0x40, 0x1B, 0x70, 0x00, 0x19, 0xFA]
}

fn ascii(s: &str) -> Vec<u8> {
    s.chars().map(|c| if c.is_ascii() && !c.is_ascii_control() { c as u8 } else { b'?' }).collect()
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn wrap(text: &str, w: usize) -> Vec<String> {
    let mut lines = vec![];
    for para in text.split('\n') {
        let mut cur = String::new();
        for word in para.split_whitespace() {
            let mut word = word.to_string();
            while char_len(&word) > w {
                if !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                }
                let head: String = word.chars().take(w).collect();
                word = word.chars().skip(w).collect();
                lines.push(head);
            }
            if cur.is_empty() {
                cur = word;
            } else if char_len(&cur) + 1 + char_len(&word) <= w {
                cur.push(' ');
                cur.push_str(&word);
            } else {
                lines.push(std::mem::replace(&mut cur, word));
            }
        }
        lines.push(cur);
    }
    lines
}

fn aligned(s: &str, w: usize, a: Align) -> String {
    let n = char_len(s);
    if n >= w {
        return s.to_string();
    }
    match a {
        Align::Left => s.to_string(),
        Align::Right => format!("{}{}", " ".repeat(w - n), s),
        Align::Center => format!("{}{}", " ".repeat((w - n) / 2), s),
    }
}

fn pair_line(l: &str, r: &str, w: usize) -> String {
    let rl = char_len(r);
    let max_l = w.saturating_sub(rl + 1);
    let left: String = if char_len(l) > max_l { l.chars().take(max_l).collect() } else { l.to_string() };
    let pad = w.saturating_sub(char_len(&left) + rl);
    format!("{left}{}{r}", " ".repeat(pad.max(1)))
}

/// Receipt labels: English, or English / Arabic when the receipt language is
/// "bilingual". Arabic prints through the raster path.
struct Labels {
    bilingual: bool,
}

impl Labels {
    fn new(cfg: &ReceiptSettings) -> Self {
        Self { bilingual: cfg.language == "bilingual" }
    }
    fn t(&self, en: &str) -> String {
        match (self.bilingual, arabic(en)) {
            (true, Some(ar)) => format!("{en} / {ar}"),
            _ => en.to_string(),
        }
    }
}

fn arabic(en: &str) -> Option<&'static str> {
    Some(match en {
        "TAX INVOICE" => "فاتورة ضريبية",
        "Receipt" => "الإيصال",
        "Cashier" => "الكاشير",
        "Customer" => "العميل",
        "Coupon" => "قسيمة",
        "You saved" => "وفّرت",
        "Discount" => "خصم",
        "Subtotal" => "المجموع",
        "VAT" => "الضريبة",
        "TOTAL" => "الإجمالي",
        "Change" => "الباقي",
        "Items" => "الأصناف",
        "Cash" => "نقداً",
        "Card" => "بطاقة",
        "BenefitPay" => "بنفت",
        "Bank Transfer" => "تحويل بنكي",
        "Wallet" => "محفظة",
        "Customer account" => "حساب العميل",
        "Pay on delivery" => "الدفع عند الاستلام",
        "Collected on delivery" => "محصّل عند التوصيل",
        "REFUND / CREDIT NOTE" => "إشعار دائن",
        "SALE VOIDED" => "إلغاء البيع",
        "ACCOUNT STATEMENT" => "كشف حساب",
        "Period" => "الفترة",
        "Opening balance" => "الرصيد الافتتاحي",
        "Closing balance" => "الرصيد الختامي",
        "Sale" => "بيع",
        "Payment" => "دفعة",
        "Adjustment" => "تعديل",
        "Owed by how late" => "المستحق حسب التأخير",
        "Not due yet" => "غير مستحق بعد",
        "1-30 days late" => "متأخر 1-30 يومًا",
        "31-60 days late" => "متأخر 31-60 يومًا",
        "61-90 days late" => "متأخر 61-90 يومًا",
        "Over 90 days late" => "متأخر أكثر من 90 يومًا",
        "Terms" => "مدة السداد",
        "days" => "يومًا",
        "Refund" => "استرجاع",
        "Original receipt" => "الإيصال الأصلي",
        "Processed by" => "بواسطة",
        "Approved by" => "موافقة",
        "Reason" => "السبب",
        "returned" => "مرتجع",
        "VAT reversed" => "الضريبة المستردة",
        "REFUND TOTAL" => "إجمالي الاسترجاع",
        "Refunded to" => "أعيد إلى",
        "Tel" => "هاتف",
        "CR" => "س.ت",
        "VAT No" => "الرقم الضريبي",
        "was" => "كان",
        "Z CLOSE" => "إغلاق اليوم (Z)",
        "X REPORT - NOT A CLOSE" => "تقرير X - ليس إغلاقًا",
        "Trading day" => "يوم التداول",
        "Branch" => "الفرع",
        "Day ends at" => "ينتهي اليوم",
        "Closed by" => "أغلقه",
        "Printed" => "طُبع",
        "Sales" => "المبيعات",
        "Before discounts" => "قبل الخصومات",
        "Discounts" => "الخصومات",
        "Refunds" => "الاسترجاعات",
        "Voids" => "الإلغاءات",
        "NET SALES" => "صافي المبيعات",
        "Net of VAT" => "بدون الضريبة",
        "Tenders" => "طرق الدفع",
        "VAT by rate" => "الضريبة حسب النسبة",
        "After-close adjustments" => "تعديلات بعد الإغلاق",
        "Arrived after their day was closed" => "وصلت بعد إغلاق يومها",
        "Drawers" => "الأدراج",
        "Float" => "الفكة",
        "Cash sales" => "مبيعات نقدية",
        "Cash refunds" => "استرجاع نقدي",
        "Cash in" => "إيداع نقدي",
        "Cash out" => "سحب نقدي",
        "Safe drops" => "إيداع في الخزنة",
        "Delivery collections" => "تحصيل التوصيل",
        "Expected" => "المتوقع",
        "Counted" => "المعدود",
        "Difference" => "الفرق",
        "Still open" => "ما زال مفتوحًا",
        "TOTAL IN THIS CLOSE" => "الإجمالي في هذا الإغلاق",
        "Fingerprint" => "البصمة",
        _ => return None,
    })
}

struct StoreInfo {
    name: String,
    branch: String,
    address: Option<String>,
    phone: Option<String>,
    cr: Option<String>,
    vat: Option<String>,
    currency: String,
    digits: u32,
    tz: String,
}

fn store_info(c: &Connection, branch_id: &str) -> AppResult<StoreInfo> {
    let (name, cr, vat, currency, digits, tz): (String, Option<String>, Option<String>, String, i64, String) =
        c.query_row("SELECT name, cr_number, vat_number, currency, currency_digits, timezone FROM business LIMIT 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?;
    let (branch, address, phone, bcr, bvat): (String, Option<String>, Option<String>, Option<String>, Option<String>) = c
        .query_row("SELECT name, address, phone, cr_number, vat_number FROM branches WHERE branch_id=?1", [branch_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .optional()?
        .unwrap_or_default();
    Ok(StoreInfo { name, branch, address, phone, cr: bcr.or(cr), vat: bvat.or(vat), currency, digits: digits as u32, tz })
}

fn local_time(ts: &str, tz: &str) -> String {
    match (time::parse(ts), time::tz(tz)) {
        (Ok(t), Ok(z)) => t.with_timezone(&z).format("%d %b %Y %H:%M").to_string(),
        _ => ts.to_string(),
    }
}

fn header(doc: &mut ReceiptDoc, info: &StoreInfo, cfg: &ReceiptSettings) {
    let l = Labels::new(cfg);
    doc.center(info.name.clone(), true, true);
    if info.branch != info.name && !info.branch.is_empty() {
        doc.center(info.branch.clone(), false, false);
    }
    if let Some(a) = &info.address {
        doc.center(a.clone(), false, false);
    }
    if let Some(p) = &info.phone {
        doc.center(format!("{}: {p}", l.t("Tel")), false, false);
    }
    let mut ids = vec![];
    if cfg.show_cr_number {
        if let Some(cr) = &info.cr {
            ids.push(format!("{}: {cr}", l.t("CR")));
        }
    }
    if cfg.show_vat_number {
        if let Some(v) = &info.vat {
            ids.push(format!("{}: {v}", l.t("VAT No")));
        }
    }
    if !ids.is_empty() {
        doc.center(ids.join("  "), false, false);
    }
    for h in &cfg.header_lines {
        doc.center(h.clone(), false, false);
    }
}

/// Build the customer receipt for a committed sale from the records and the
/// current receipt settings (used once, at commit, to make the snapshot; and
/// for sales made before snapshots existed).
fn build_sale_receipt(c: &Connection, sale_id: &str, copy_label: Option<&str>) -> AppResult<ReceiptDoc> {
    let cfg: ReceiptSettings = settings::get(c, settings::KEY_RECEIPT)?;
    let printer: settings::PrinterSettings = settings::get(c, settings::KEY_PRINTER)?;
    let d = load_sale_detail(c, sale_id, false)?;
    let branch_id: String = c.query_row("SELECT branch_id FROM sales WHERE sale_id=?1", [sale_id], |r| r.get(0))?;
    let info = store_info(c, &branch_id)?;
    let m = |v: i64| format_decimal(v, info.digits);
    let l = Labels::new(&cfg);
    let mut doc = ReceiptDoc::new(width_for(printer.paper_width_mm.min(cfg.paper_width_mm)));
    header(&mut doc, &info, &cfg);
    doc.rule();
    doc.center(l.t(&cfg.title), true, false);
    if let Some(l) = copy_label {
        doc.center(format!("*** {l} ***"), true, false);
    }
    doc.pair(format!("{}: {}", l.t("Receipt"), d.receipt_number), local_time(&d.completed_at, &info.tz));
    if cfg.show_cashier {
        doc.pair(format!("{}: {}", l.t("Cashier"), d.cashier_name), d.device_name.clone().unwrap_or_default());
    }
    if let Some(cn) = &d.customer_name {
        doc.left(format!("{}: {cn}{}", l.t("Customer"), d.customer_phone.as_ref().map(|p| format!(" ({p})")).unwrap_or_default()));
    }
    doc.rule();
    let offer_amount = |o: &serde_json::Value| o["amount_minor"].as_i64().unwrap_or(0);
    let mut i = 0;
    while i < d.items.len() {
        // A bundle prints as one line (its price) with its items beneath.
        if let Some(b) = d.items[i].bundle.clone() {
            let group: Vec<&crate::sales::SaleItemView> =
                d.items[i..].iter().take_while(|x| x.bundle.as_ref().is_some_and(|y| y["line_no"] == b["line_no"])).collect();
            i += group.len();
            let qty = b["qty_milli"].as_i64().unwrap_or(1000);
            let gross: i64 = group.iter().map(|x| x.gross_minor).sum();
            doc.left(b["name"].as_str().unwrap_or_default().to_string());
            let unit = if qty > 0 { crate::money::div_round(gross as i128 * 1000, qty as i128) as i64 } else { gross };
            doc.pair(format!("  {} x {}", format_qty(qty), m(unit)), m(gross));
            for x in &group {
                doc.left(format!("  - {} x {}", x.name, format_qty(x.qty_milli)));
            }
            let mut offers: std::collections::BTreeMap<String, i64> = Default::default();
            for o in group.iter().flat_map(|x| x.offers.iter()).filter(|o| o["layer"] == "item") {
                *offers.entry(o["name"].as_str().unwrap_or_default().to_string()).or_insert(0) += offer_amount(o);
            }
            for (name, amt) in offers {
                doc.pair(format!("  {name}"), format!("-{}", m(amt)));
            }
            let other: i64 = group.iter().map(|x| x.discount_minor - x.promo_discount_minor).sum();
            if other > 0 {
                doc.pair(format!("  {}", l.t("Discount")), format!("-{}", m(other)));
            }
            continue;
        }
        let it = &d.items[i];
        i += 1;
        doc.left(it.name.clone());
        if let Some(ar) = it.name_ar.as_ref().filter(|a| *a != &it.name) {
            doc.left(ar.clone());
        }
        let qty = format!("  {} x {}", format_qty(it.qty_milli), m(it.unit_price_minor));
        doc.pair(qty, m(it.gross_minor));
        if it.unit_price_minor != it.original_unit_price_minor {
            doc.left(format!("  ({} {})", l.t("was"), m(it.original_unit_price_minor)));
        }
        for o in it.offers.iter().filter(|o| o["layer"] == "item") {
            doc.pair(format!("  {}", o["name"].as_str().unwrap_or_default()), format!("-{}", m(offer_amount(o))));
        }
        if it.discount_minor - it.promo_discount_minor > 0 {
            doc.pair(format!("  {}", l.t("Discount")), format!("-{}", m(it.discount_minor - it.promo_discount_minor)));
        }
        if cfg.show_barcode {
            if let Some(b) = &it.barcode {
                doc.left(format!("  {b}"));
            }
        }
    }
    doc.rule();
    doc.pair(l.t("Subtotal"), m(d.subtotal_minor));
    // Basket offers and the coupon, by name (item offers are on their lines).
    let mut basket: Vec<(String, i64)> = vec![];
    for o in d.items.iter().flat_map(|x| x.offers.iter()).filter(|o| o["layer"] != "item") {
        let label = match o["coupon_code"].as_str() {
            Some(code) => format!("{} {code}", l.t("Coupon")),
            None => o["name"].as_str().unwrap_or_default().to_string(),
        };
        match basket.iter_mut().find(|b| b.0 == label) {
            Some(b) => b.1 += offer_amount(o),
            None => basket.push((label, offer_amount(o))),
        }
    }
    for (label, amt) in &basket {
        doc.pair(label.clone(), format!("-{}", m(*amt)));
    }
    let promo_total: i64 = d.items.iter().map(|x| x.promo_discount_minor).sum();
    if d.discount_minor - promo_total > 0 {
        doc.pair(l.t("Discount"), format!("-{}", m(d.discount_minor - promo_total)));
    }
    // VAT summary by rate.
    let mut rates: std::collections::BTreeMap<(i64, bool), i64> = Default::default();
    for it in &d.items {
        *rates.entry((it.tax_rate_bp, it.tax_inclusive)).or_insert(0) += it.tax_minor;
    }
    for ((rate, incl), tax) in &rates {
        let label = format!(
            "{} {}%{}",
            l.t("VAT"),
            format_decimal(*rate, 2).trim_end_matches('0').trim_end_matches('.'),
            if *incl { " (incl.)" } else { "" }
        );
        doc.pair(label, m(*tax));
    }
    doc.pair_b(l.t("TOTAL"), format!("{} {}", info.currency, m(d.total_minor)), true);
    for p in &d.payments {
        let label = l.t(&method_label(&p.method));
        doc.pair(
            match &p.reference {
                Some(r) => format!("{label} ({r})"),
                None => label,
            },
            m(p.tendered_minor),
        );
    }
    if d.change_minor > 0 {
        doc.pair_b(l.t("Change"), m(d.change_minor), false);
    }
    if promo_total > 0 {
        doc.pair(l.t("You saved"), m(d.discount_minor));
    }
    doc.rule();
    // A bundle counts as its own quantity, not its items'.
    let mut bundles_seen = std::collections::BTreeSet::new();
    let count: i64 = d
        .items
        .iter()
        .map(|i| match &i.bundle {
            Some(b) if bundles_seen.insert(b["line_no"].as_i64()) => b["qty_milli"].as_i64().unwrap_or(0),
            Some(_) => 0,
            None => i.qty_milli,
        })
        .sum();
    doc.pair(l.t("Items"), format_qty(count));
    for f in &cfg.footer_lines {
        doc.center(f.clone(), false, false);
    }
    Ok(doc)
}

pub fn method_label(m: &str) -> String {
    match m {
        "cash" => "Cash".into(),
        "card" => "Card".into(),
        "benefitpay" => "BenefitPay".into(),
        "bank_transfer" => "Bank Transfer".into(),
        "wallet" => "Wallet".into(),
        "account" => "Customer account".into(),
        "pay_on_delivery" => "Pay on delivery".into(),
        other => other.to_string(),
    }
}

fn build_refund_receipt(c: &Connection, refund_id: &str, copy_label: Option<&str>) -> AppResult<ReceiptDoc> {
    let cfg: ReceiptSettings = settings::get(c, settings::KEY_RECEIPT)?;
    let printer: settings::PrinterSettings = settings::get(c, settings::KEY_PRINTER)?;
    let (rn, orig, branch, user, approver, reason, total, tax, at, kind): (
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        i64,
        i64,
        String,
        String,
    ) = c
        .query_row(
            "SELECT r.refund_receipt_number, s.receipt_number, r.branch_id, COALESCE(u.display_name,''), a.display_name, r.reason,
                    r.total_minor, r.tax_minor, r.created_at, r.kind
             FROM refunds r JOIN sales s ON s.sale_id=r.original_sale_id LEFT JOIN users u ON u.user_id=r.user_id
             LEFT JOIN users a ON a.user_id=r.approved_by WHERE r.refund_id=?1",
            [refund_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?)),
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("Refund"))?;
    let info = store_info(c, &branch)?;
    let m = |v: i64| format_decimal(v, info.digits);
    let l = Labels::new(&cfg);
    let mut doc = ReceiptDoc::new(width_for(printer.paper_width_mm.min(cfg.paper_width_mm)));
    header(&mut doc, &info, &cfg);
    doc.rule();
    doc.center(if kind == "void" { l.t("SALE VOIDED") } else { l.t("REFUND / CREDIT NOTE") }, true, false);
    if let Some(l) = copy_label {
        doc.center(format!("*** {l} ***"), true, false);
    }
    doc.pair(format!("{}: {rn}", l.t("Refund")), local_time(&at, &info.tz));
    doc.left(format!("{}: {orig}", l.t("Original receipt")));
    doc.left(format!("{}: {user}", l.t("Processed by")));
    if let Some(a) = approver {
        doc.left(format!("{}: {a}", l.t("Approved by")));
    }
    doc.left(format!("{}: {reason}", l.t("Reason")));
    doc.rule();
    let mut st = c.prepare(
        "SELECT si.product_name_snapshot, ri.qty_milli, ri.amount_minor, si.product_name_ar_snapshot FROM refund_items ri
         JOIN sale_items si ON si.sale_item_id=ri.original_sale_item_id WHERE ri.refund_id=?1 ORDER BY si.line_no",
    )?;
    let items = st
        .query_map([refund_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<String>>(3)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (name, q, amt, name_ar) in items {
        let ar = name_ar.filter(|a| a != &name);
        doc.left(name);
        if let Some(ar) = ar {
            doc.left(ar);
        }
        doc.pair(format!("  {} {}", format_qty(q), l.t("returned")), format!("-{}", m(amt)));
    }
    doc.rule();
    doc.pair(l.t("VAT reversed"), format!("-{}", m(tax)));
    doc.pair_b(l.t("REFUND TOTAL"), format!("{} {}", info.currency, m(total)), true);
    let mut st = c.prepare("SELECT method, amount_minor, reference FROM refund_tenders WHERE refund_id=?1")?;
    let tenders = st
        .query_map([refund_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (method, amt, _r) in tenders {
        doc.pair(format!("{} {}", l.t("Refunded to"), l.t(&method_label(&method))), m(amt));
    }
    doc.rule();
    for f in &cfg.footer_lines {
        doc.center(f.clone(), false, false);
    }
    Ok(doc)
}

pub fn shift_report(c: &Connection, shift_id: &str) -> AppResult<ReceiptDoc> {
    let cfg: ReceiptSettings = settings::get(c, settings::KEY_RECEIPT)?;
    let printer: settings::PrinterSettings = settings::get(c, settings::KEY_PRINTER)?;
    let sum = crate::shifts::shift_summary(c, shift_id)?;
    let branch: String = c.query_row("SELECT branch_id FROM shifts WHERE shift_id=?1", [shift_id], |r| r.get(0))?;
    let info = store_info(c, &branch)?;
    let m = |v: i64| format_decimal(v, info.digits);
    let mut doc = ReceiptDoc::new(width_for(printer.paper_width_mm.min(cfg.paper_width_mm)));
    doc.center(info.name.clone(), true, false);
    doc.center(format!("SHIFT REPORT {}", sum.shift_number), true, false);
    doc.pair("Cashier", sum.cashier_name.clone());
    doc.pair("Opened", local_time(&sum.opened_at, &info.tz));
    if let Some(cl) = &sum.closed_at {
        doc.pair("Closed", local_time(cl, &info.tz));
    }
    doc.rule();
    doc.pair("Sales", format!("{}", sum.sale_count));
    doc.pair("Gross sales", m(sum.sales_total_minor));
    for p in &sum.by_method {
        doc.pair(format!("  {}", method_label(&p.method)), m(p.amount_minor));
    }
    doc.pair("Refunds", format!("-{}", m(sum.refunds_total_minor)));
    doc.rule();
    doc.pair("Opening float", m(sum.opening_float_minor));
    doc.pair("Cash sales", m(sum.cash_sales_minor));
    doc.pair("Cash refunds", format!("-{}", m(sum.cash_refunds_minor)));
    doc.pair("Paid in", m(sum.paid_in_minor));
    doc.pair("Paid out", format!("-{}", m(sum.paid_out_minor)));
    doc.pair("Safe drops", format!("-{}", m(sum.safe_drop_minor)));
    if sum.cash_collections_minor > 0 {
        doc.pair("Collected on delivery", m(sum.cash_collections_minor));
    }
    doc.pair_b("Expected cash", m(sum.expected_cash_minor), false);
    if let Some(cnt) = sum.counted_cash_minor {
        doc.pair_b("Counted cash", m(cnt), false);
        doc.pair_b("Variance", m(sum.variance_minor.unwrap_or(0)), false);
    }
    Ok(doc)
}

// ---------------------------------------------------------------- snapshots

/// Version of the snapshot body; bump only with a migration note.
pub const SNAPSHOT_FORMAT: i64 = 1;

/// A receipt as issued, read back from its snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct IssuedReceipt {
    pub doc: ReceiptDoc,
    pub sha256: String,
    /// False for records made before snapshots existed: rebuilt from the
    /// records with today's receipt settings.
    pub exact: bool,
}

fn canonical(doc: &ReceiptDoc) -> AppResult<String> {
    // Struct fields serialize in declaration order and blocks are a list, so
    // the same document always gives the same bytes.
    Ok(serde_json::to_string(&serde_json::json!({ "format": SNAPSHOT_FORMAT, "doc": doc }))?)
}

pub fn digest(doc: &ReceiptDoc) -> AppResult<String> {
    Ok(hex::encode(Sha256::digest(canonical(doc)?.as_bytes())))
}

/// Where a "COPY" line goes: right after the title (the first centred bold
/// line after the first rule).
fn copy_position(doc: &ReceiptDoc) -> usize {
    let rule = doc.blocks.iter().position(|b| matches!(b, Block::Rule)).unwrap_or(0);
    rule + 2
}

fn write_snapshot(c: &Connection, kind: &str, ref_id: &str, doc: &ReceiptDoc) -> AppResult<()> {
    let body = serde_json::to_string(doc)?;
    c.execute(
        "INSERT OR IGNORE INTO receipt_snapshots(ref_kind, ref_id, format_version, doc_json, copy_at, sha256, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![kind, ref_id, SNAPSHOT_FORMAT, body, copy_position(doc) as i64, digest(doc)?, time::now_str()],
    )?;
    Ok(())
}

/// Freeze the sale's receipt. Called inside the sale's own transaction.
pub fn snapshot_sale(c: &Connection, sale_id: &str) -> AppResult<()> {
    let doc = build_sale_receipt(c, sale_id, None)?;
    write_snapshot(c, "sale", sale_id, &doc)
}

/// Freeze the refund's receipt. Called inside the refund's own transaction.
pub fn snapshot_refund(c: &Connection, refund_id: &str) -> AppResult<()> {
    let doc = build_refund_receipt(c, refund_id, None)?;
    write_snapshot(c, "refund", refund_id, &doc)
}

/// The issued receipt for a record (snapshot, or reconstructed for history).
pub fn issued(c: &Connection, kind: &str, ref_id: &str) -> AppResult<IssuedReceipt> {
    let row: Option<(String, String)> = c
        .query_row("SELECT doc_json, sha256 FROM receipt_snapshots WHERE ref_kind=?1 AND ref_id=?2", [kind, ref_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    if let Some((body, sha)) = row {
        let doc: ReceiptDoc = serde_json::from_str(&body).map_err(|e| AppError::internal(format!("Stored receipt is unreadable: {e}")))?;
        if digest(&doc)? != sha {
            return Err(AppError::internal("The stored receipt does not match its fingerprint."));
        }
        return Ok(IssuedReceipt { doc, sha256: sha, exact: true });
    }
    let doc = match kind {
        "sale" => build_sale_receipt(c, ref_id, None)?,
        "refund" => build_refund_receipt(c, ref_id, None)?,
        _ => return Err(AppError::not_found("Receipt")),
    };
    Ok(IssuedReceipt { sha256: digest(&doc)?, doc, exact: false })
}

/// The document to print or send: the issued receipt, with a "COPY" line
/// added outside the fingerprinted body when it is a reprint.
fn with_copy(c: &Connection, kind: &str, ref_id: &str, copy_label: Option<&str>) -> AppResult<ReceiptDoc> {
    let mut doc = issued(c, kind, ref_id)?.doc;
    if let Some(l) = copy_label {
        let at = copy_position(&doc).min(doc.blocks.len());
        doc.blocks.insert(at, Block::Text { text: format!("*** {l} ***"), align: Align::Center, bold: true, large: false });
    }
    Ok(doc)
}

/// The customer receipt for a committed sale, as it was issued.
pub fn sale_receipt(c: &Connection, sale_id: &str, copy_label: Option<&str>) -> AppResult<ReceiptDoc> {
    with_copy(c, "sale", sale_id, copy_label)
}

/// The refund receipt (credit note), as it was issued.
pub fn refund_receipt(c: &Connection, refund_id: &str, copy_label: Option<&str>) -> AppResult<ReceiptDoc> {
    with_copy(c, "refund", refund_id, copy_label)
}

/// A customer account statement (always English and Arabic), printed or
/// saved as PDF. Built only from the ledger; nothing here is stored.
pub fn statement_doc(c: &Connection, branch_id: &str, st: &crate::credit::Statement) -> AppResult<ReceiptDoc> {
    let cfg: ReceiptSettings = settings::get(c, settings::KEY_RECEIPT)?;
    let printer: settings::PrinterSettings = settings::get(c, settings::KEY_PRINTER)?;
    let info = store_info(c, branch_id)?;
    let m = |v: i64| format_decimal(v, info.digits);
    let mut bi = cfg.clone();
    bi.language = "bilingual".into();
    let l = Labels::new(&bi);
    let mut doc = ReceiptDoc::new(width_for(printer.paper_width_mm.min(cfg.paper_width_mm)));
    header(&mut doc, &info, &cfg);
    doc.rule();
    doc.center(l.t("ACCOUNT STATEMENT"), true, false);
    doc.left(format!("{}: {}", l.t("Customer"), st.customer_name));
    if let Some(p) = &st.phone {
        doc.left(p.clone());
    }
    doc.left(format!("{}: {} → {}", l.t("Period"), st.from, st.to));
    doc.rule();
    doc.pair(l.t("Opening balance"), m(st.opening_minor));
    for line in &st.lines {
        let what = match line.kind.as_str() {
            "sale" => l.t("Sale"),
            "payment" => l.t("Payment"),
            "refund" => l.t("Refund"),
            _ => l.t("Adjustment"),
        };
        doc.left(format!("{} {}{}", line.date, what, line.reference.as_ref().map(|r| format!(" {r}")).unwrap_or_default()));
        let amt = if line.charge_minor > 0 { m(line.charge_minor) } else { format!("-{}", m(line.credit_minor)) };
        doc.pair(format!("  {amt}"), m(line.balance_minor));
    }
    doc.rule();
    doc.pair_b(l.t("Closing balance"), format!("{} {}", info.currency, m(st.closing_minor)), true);
    doc.rule();
    doc.center(l.t("Owed by how late"), true, false);
    doc.pair(l.t("Not due yet"), m(st.ageing.current_minor));
    doc.pair(l.t("1-30 days late"), m(st.ageing.d1_30_minor));
    doc.pair(l.t("31-60 days late"), m(st.ageing.d31_60_minor));
    doc.pair(l.t("61-90 days late"), m(st.ageing.d61_90_minor));
    doc.pair(l.t("Over 90 days late"), m(st.ageing.d90_plus_minor));
    doc.left(format!("{}: {} {}", l.t("Terms"), st.terms_days, l.t("days")));
    for f in &cfg.footer_lines {
        doc.center(f.clone(), false, false);
    }
    Ok(doc)
}

/// The X or Z report as a printable document (always English and Arabic).
/// Built only from the report, so a stored close prints the same forever.
pub fn day_close_doc(c: &Connection, rep: &crate::dayclose::DayReport) -> AppResult<ReceiptDoc> {
    let printer: settings::PrinterSettings = settings::get(c, settings::KEY_PRINTER)?;
    let cfg: ReceiptSettings = settings::get(c, settings::KEY_RECEIPT)?;
    let mut bi = cfg.clone();
    bi.language = "bilingual".into();
    let l = Labels::new(&bi);
    let m = |v: i64| format_decimal(v, rep.digits);
    let mut doc = ReceiptDoc::new(width_for(printer.paper_width_mm.min(cfg.paper_width_mm)));
    doc.center(rep.business_name.clone(), true, true);
    if let Some(v) = &rep.vat_number {
        doc.center(format!("{}: {v}", l.t("VAT No")), false, false);
    }
    doc.rule();
    let title = if rep.kind == "z" { l.t("Z CLOSE") } else { l.t("X REPORT - NOT A CLOSE") };
    doc.center(title, true, false);
    if let Some(n) = &rep.close_number {
        doc.center(n.clone(), true, false);
    }
    doc.pair(l.t("Branch"), rep.branch_name.clone());
    doc.pair(l.t("Trading day"), rep.business_date.clone());
    doc.pair(l.t("Day ends at"), format!("{:02}:{:02}", rep.cutoff_minutes / 60, rep.cutoff_minutes % 60));
    if let Some(who) = &rep.closed_by_name {
        doc.pair(l.t("Closed by"), who.clone());
    }
    doc.pair(l.t("Printed"), time::display(&rep.generated_at, &rep.timezone));
    let section = |doc: &mut ReceiptDoc, t: &crate::dayclose::Totals| {
        doc.pair(format!("{} ({})", l.t("Sales"), t.sale_count), m(t.sales_minor));
        doc.pair(format!("  {}", l.t("Before discounts")), m(t.gross_minor));
        doc.pair(format!("  {}", l.t("Discounts")), m(t.discount_minor));
        doc.pair(format!("{} ({})", l.t("Refunds"), t.refund_count), format!("-{}", m(t.refunds_minor)));
        doc.pair(format!("{} ({})", l.t("Voids"), t.void_count), format!("-{}", m(t.voids_minor)));
        doc.pair_b(l.t("NET SALES"), m(t.net_sales_minor), false);
        doc.pair(l.t("VAT"), m(t.tax_minor));
        doc.pair(l.t("Net of VAT"), m(t.net_ex_vat_minor));
    };
    doc.rule();
    section(&mut doc, &rep.day);
    if rep.after_close.sale_count + rep.after_close.refund_count + rep.after_close.void_count > 0 {
        doc.rule();
        doc.center(l.t("After-close adjustments"), true, false);
        doc.left(l.t("Arrived after their day was closed"));
        section(&mut doc, &rep.after_close);
        for x in &rep.late {
            let sign = if x.kind == "sale" { "" } else { "-" };
            doc.pair(format!("  {} {}", x.business_date, x.number), format!("{sign}{}", m(x.total_minor)));
        }
        doc.rule();
        doc.pair_b(l.t("TOTAL IN THIS CLOSE"), format!("{} {}", rep.currency, m(rep.total.net_sales_minor)), true);
    } else {
        doc.pair_b(l.t("TOTAL"), format!("{} {}", rep.currency, m(rep.total.net_sales_minor)), true);
    }
    doc.rule();
    doc.center(l.t("Tenders"), true, false);
    for t in &rep.total.tenders {
        doc.pair(l.t(&method_label(&t.method)), m(t.net_minor));
    }
    if !rep.total.vat.is_empty() {
        doc.rule();
        doc.center(l.t("VAT by rate"), true, false);
        for v in &rep.total.vat {
            let rate = format_decimal(v.rate_bp, 2);
            let rate = rate.trim_end_matches('0').trim_end_matches('.');
            doc.pair(format!("{rate}%  {}", m(v.net_minor)), m(v.tax_minor));
        }
    }
    doc.rule();
    doc.center(l.t("Drawers"), true, false);
    for d in &rep.drawers {
        let name = d.register_name.clone().or(d.device_name.clone()).unwrap_or_default();
        doc.left(format!("{} {} - {}", d.shift_number, name, d.cashier_name));
        doc.pair(format!("  {}", l.t("Expected")), m(d.expected_cash_minor));
        match (d.status.as_str(), d.counted_cash_minor) {
            ("closed", Some(n)) => {
                doc.pair(format!("  {}", l.t("Counted")), m(n));
                doc.pair(format!("  {}", l.t("Difference")), m(d.variance_minor.unwrap_or(0)));
            }
            _ => doc.left(format!("  {}", l.t("Still open"))),
        }
    }
    let ct = &rep.cash;
    doc.pair(l.t("Float"), m(ct.opening_float_minor));
    doc.pair(l.t("Cash sales"), m(ct.cash_sales_minor));
    doc.pair(l.t("Cash refunds"), format!("-{}", m(ct.cash_refunds_minor)));
    doc.pair(l.t("Cash in"), m(ct.paid_in_minor));
    doc.pair(l.t("Cash out"), format!("-{}", m(ct.paid_out_minor)));
    doc.pair(l.t("Safe drops"), format!("-{}", m(ct.safe_drop_minor)));
    doc.pair(l.t("Delivery collections"), m(ct.delivery_collections_minor + ct.rider_handover_minor));
    doc.pair_b(l.t("Expected"), m(ct.expected_cash_minor), false);
    doc.pair(l.t("Counted"), m(ct.counted_cash_minor));
    doc.pair_b(l.t("Difference"), m(ct.variance_minor), false);
    doc.blocks.push(Block::Feed { lines: 2 });
    Ok(doc)
}
