//! Receipt documents: built from committed records only (never from current
//! catalogue data) and rendered to plain text (preview / file printer) or
//! ESC/POS bytes (thermal printers).

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::money::{format_decimal, format_qty};
use crate::raster;
use crate::sales::load_sale_detail;
use crate::settings::{self, ReceiptSettings};
use crate::time;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    Text { text: String, align: Align, bold: bool, large: bool },
    Pair { left: String, right: String, bold: bool, large: bool },
    Rule,
    Feed { lines: u8 },
}

#[derive(Debug, Clone, Serialize)]
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
        "REFUND / CREDIT NOTE" => "إشعار دائن",
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

/// Build the customer receipt for a committed sale.
pub fn sale_receipt(c: &Connection, sale_id: &str, copy_label: Option<&str>) -> AppResult<ReceiptDoc> {
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
    for it in &d.items {
        doc.left(it.name.clone());
        if let Some(ar) = it.name_ar.as_ref().filter(|a| *a != &it.name) {
            doc.left(ar.clone());
        }
        let qty = format!("  {} x {}", format_qty(it.qty_milli), m(it.unit_price_minor));
        doc.pair(qty, m(it.gross_minor));
        if it.unit_price_minor != it.original_unit_price_minor {
            doc.left(format!("  ({} {})", l.t("was"), m(it.original_unit_price_minor)));
        }
        if it.discount_minor > 0 {
            doc.pair(format!("  {}", l.t("Discount")), format!("-{}", m(it.discount_minor)));
        }
        if cfg.show_barcode {
            if let Some(b) = &it.barcode {
                doc.left(format!("  {b}"));
            }
        }
    }
    doc.rule();
    doc.pair(l.t("Subtotal"), m(d.subtotal_minor));
    if d.discount_minor > 0 {
        doc.pair(l.t("Discount"), format!("-{}", m(d.discount_minor)));
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
    doc.rule();
    doc.pair(l.t("Items"), format_qty(d.items.iter().map(|i| i.qty_milli).sum()));
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
        other => other.to_string(),
    }
}

pub fn refund_receipt(c: &Connection, refund_id: &str, copy_label: Option<&str>) -> AppResult<ReceiptDoc> {
    let cfg: ReceiptSettings = settings::get(c, settings::KEY_RECEIPT)?;
    let printer: settings::PrinterSettings = settings::get(c, settings::KEY_PRINTER)?;
    let (rn, orig, branch, user, approver, reason, total, tax, at): (
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        i64,
        i64,
        String,
    ) = c
        .query_row(
            "SELECT r.refund_receipt_number, s.receipt_number, r.branch_id, COALESCE(u.display_name,''), a.display_name, r.reason,
                    r.total_minor, r.tax_minor, r.created_at
             FROM refunds r JOIN sales s ON s.sale_id=r.original_sale_id LEFT JOIN users u ON u.user_id=r.user_id
             LEFT JOIN users a ON a.user_id=r.approved_by WHERE r.refund_id=?1",
            [refund_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)),
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("Refund"))?;
    let info = store_info(c, &branch)?;
    let m = |v: i64| format_decimal(v, info.digits);
    let l = Labels::new(&cfg);
    let mut doc = ReceiptDoc::new(width_for(printer.paper_width_mm.min(cfg.paper_width_mm)));
    header(&mut doc, &info, &cfg);
    doc.rule();
    doc.center(l.t("REFUND / CREDIT NOTE"), true, false);
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
    doc.pair_b("Expected cash", m(sum.expected_cash_minor), false);
    if let Some(cnt) = sum.counted_cash_minor {
        doc.pair_b("Counted cash", m(cnt), false);
        doc.pair_b("Variance", m(sum.variance_minor.unwrap_or(0)), false);
    }
    Ok(doc)
}
