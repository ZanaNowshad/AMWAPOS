//! OCR workflows: payment-screenshot reviews and supplier-invoice scans.
//!
//! OCR output is untrusted text. It is parsed by fixed rules into numbers and
//! codes, shown to a person, and only a person's confirmation changes
//! anything: a confirmed invoice scan becomes a *draft* purchase order (stock
//! moves only when goods are received on that order), and a confirmed payment
//! review marks a delivery paid with an audit entry. A screenshot is never
//! proof of settlement.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::audit;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ids::{new_id, next_seq};
use crate::money::parse_decimal;
use crate::service::AppCore;
use crate::time;
use crate::validate;

/// Below this OCR confidence a match is never automatic.
const MIN_CONFIDENCE: i64 = 60;
const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff"];

// ---------------------------------------------------------------- parsing

/// Arabic-Indic and Eastern Arabic digits, Arabic decimal/thousands marks → ASCII.
pub fn normalize_digits(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{0660}'..='\u{0669}' => char::from(b'0' + (c as u32 - 0x0660) as u8),
            '\u{06F0}'..='\u{06F9}' => char::from(b'0' + (c as u32 - 0x06F0) as u8),
            '\u{066B}' => '.',
            '\u{066C}' => ',',
            _ => c,
        })
        .collect()
}

/// Money-looking numbers in a line: `1,234.500`, `12.5`, `7`. Returns (value in
/// minor units, had decimals).
fn numbers(line: &str, digits: u32) -> Vec<(i64, bool, usize)> {
    let b = line.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !(b[i - 1].is_ascii_alphanumeric())) {
            let start = i;
            while i < b.len() && (b[i].is_ascii_digit() || ((b[i] == b',' || b[i] == b'.') && i + 1 < b.len() && b[i + 1].is_ascii_digit()))
            {
                i += 1;
            }
            if i < b.len() && b[i].is_ascii_alphabetic() {
                continue; // part of a code like 12AB
            }
            let tok = &line[start..i];
            let has_dec = tok.rfind('.').map(|p| tok.len() - p - 1 <= digits as usize && tok.len() - p - 1 > 0).unwrap_or(false);
            let clean: String = if has_dec { tok.replace(',', "") } else { tok.chars().filter(|c| c.is_ascii_digit()).collect() };
            if clean.len() <= 12 {
                if let Ok(v) = parse_decimal(&clean, digits) {
                    out.push((v, has_dec, start));
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

const AMOUNT_WORDS: &[&str] = &["amount", "total", "paid", "bhd", "bd", "sent", "المبلغ", "د.ب", "دينار", "المدفوع", "الإجمالي"];
const REF_WORDS: &[&str] = &["reference", "ref", "transaction", "txn", "trx", "رقم المرجع", "المرجع", "رقم العملية"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PaymentExtract {
    pub amount_minor: Option<i64>,
    pub reference: Option<String>,
}

/// Pick the paid amount and reference from a payment screenshot's text.
/// Prefers an amount on a line with a currency/amount word, with decimals.
pub fn parse_payment(text: &str, digits: u32) -> PaymentExtract {
    let text = normalize_digits(text);
    let mut best: Option<(i32, i64)> = None;
    let mut reference = None;
    for line in text.lines() {
        let low = line.to_lowercase();
        let keyword = AMOUNT_WORDS.iter().any(|w| low.contains(w));
        for (v, dec, _) in numbers(line, digits) {
            if v <= 0 {
                continue;
            }
            let score = keyword as i32 * 4 + dec as i32 * 2 + (low.contains("bhd") || low.contains("bd") || low.contains("د.ب")) as i32;
            if score >= 2 && best.map(|(s, _)| score > s).unwrap_or(true) {
                best = Some((score, v));
            }
        }
        if reference.is_none() && REF_WORDS.iter().any(|w| low.contains(w)) {
            reference = line
                .split(|c: char| c.is_whitespace() || c == ':' || c == '#')
                .rfind(|t| t.len() >= 6 && t.chars().all(|c| c.is_ascii_alphanumeric()) && t.chars().any(|c| c.is_ascii_digit()))
                .map(|t| t.to_string());
        }
    }
    PaymentExtract { amount_minor: best.map(|b| b.1), reference }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InvoiceLine {
    pub raw_text: String,
    pub description: String,
    pub code: Option<String>,
    pub qty_milli: Option<i64>,
    pub unit_cost_minor: Option<i64>,
    pub line_total_minor: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InvoiceExtract {
    pub invoice_number: Option<String>,
    pub invoice_date: Option<String>,
    pub total_minor: Option<i64>,
    pub lines: Vec<InvoiceLine>,
}

fn is_summary(low: &str) -> bool {
    ["total", "subtotal", "vat", "tax", "balance", "discount", "الإجمالي", "المجموع", "ضريبة"].iter().any(|w| low.contains(w))
}

/// Parse invoice text into header fields and item lines. An item line has a
/// description and at least one price with decimals; quantity is the leading
/// or `n x` integer, or derived from total ÷ unit.
pub fn parse_invoice(text: &str, digits: u32) -> InvoiceExtract {
    let text = normalize_digits(text);
    let mut ex = InvoiceExtract { invoice_number: None, invoice_date: None, total_minor: None, lines: vec![] };
    let one = 10i64.pow(digits);
    for raw in text.lines() {
        let line = raw.trim();
        if line.len() < 3 {
            continue;
        }
        let low = line.to_lowercase();
        if ex.invoice_number.is_none() && (low.contains("invoice") || low.contains("inv") || low.contains("فاتورة")) {
            if let Some(t) = line
                .split(|c: char| c.is_whitespace() || c == ':' || c == '#')
                .filter(|t| {
                    t.len() >= 3
                        && t.chars().any(|c| c.is_ascii_digit())
                        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '/')
                })
                .find(|t| !looks_like_date(t))
            {
                ex.invoice_number = Some(t.to_string());
            }
        }
        if ex.invoice_date.is_none() {
            if let Some(d) = line.split_whitespace().find_map(parse_date) {
                ex.invoice_date = Some(d);
            }
        }
        let nums = numbers(line, digits);
        if is_summary(&low) {
            if low.contains("total") || low.contains("الإجمالي") {
                if let Some((v, _, _)) = nums.iter().filter(|n| n.1).max_by_key(|n| n.0) {
                    ex.total_minor = Some(ex.total_minor.map_or(*v, |t| t.max(*v)));
                }
            }
            continue;
        }
        let prices: Vec<i64> = nums.iter().filter(|n| n.1).map(|n| n.0).collect();
        if prices.is_empty() {
            continue;
        }
        let first_num = nums.first().map(|n| n.2).unwrap_or(line.len());
        // Code: 8/12/13/14-digit barcode, or an alphanumeric SKU token.
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let code = tokens
            .iter()
            .find(|t| t.chars().all(|c| c.is_ascii_digit()) && matches!(t.len(), 8 | 12 | 13 | 14))
            .or_else(|| {
                tokens.iter().find(|t| {
                    t.len() >= 4
                        && t.len() <= 20
                        && t.chars().any(|c| c.is_ascii_digit())
                        && t.chars().any(|c| c.is_ascii_alphabetic())
                        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                })
            })
            .map(|t| t.to_string());
        let desc: String = line[..first_num.min(line.len())]
            .split_whitespace()
            .filter(|t| Some(t.to_string()) != code)
            .collect::<Vec<_>>()
            .join(" ")
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_string();
        let desc = if desc.is_empty() {
            line.split_whitespace().filter(|t| t.chars().any(|c| c.is_alphabetic())).collect::<Vec<_>>().join(" ")
        } else {
            desc
        };
        if desc.chars().filter(|c| c.is_alphabetic()).count() < 2 {
            continue;
        }
        // Quantity: an integer that is not the barcode, e.g. "2 x", "x2", "Qty 2".
        let qty_int = nums
            .iter()
            .filter(|n| !n.1 && n.0 > 0 && n.0 <= 100_000 * one)
            .map(|n| n.0 / one)
            .find(|q| code.as_deref() != Some(&q.to_string()));
        let (unit, total) = match prices.len() {
            1 => (None, Some(prices[0])),
            _ => (Some(prices[prices.len() - 2]), Some(prices[prices.len() - 1])),
        };
        let qty_milli = match (qty_int, unit, total) {
            (Some(q), _, _) => Some(q * 1000),
            (None, Some(u), Some(t)) if u > 0 && t % u == 0 => Some(t / u * 1000),
            _ => None,
        };
        let unit = unit.or_else(|| match (qty_milli, total) {
            (Some(q), Some(t)) if q > 0 => Some(crate::money::div_round(t as i128 * 1000, q as i128) as i64),
            _ => None,
        });
        ex.lines.push(InvoiceLine {
            raw_text: line.chars().take(300).collect(),
            description: desc.chars().take(200).collect(),
            code,
            qty_milli,
            unit_cost_minor: unit,
            line_total_minor: total,
        });
    }
    ex
}

fn looks_like_date(t: &str) -> bool {
    parse_date(t).is_some()
}

/// `dd/mm/yyyy`, `dd-mm-yyyy`, `yyyy-mm-dd` → `yyyy-mm-dd`.
fn parse_date(t: &str) -> Option<String> {
    let parts: Vec<&str> = t.trim_matches(|c: char| !c.is_ascii_digit()).split(['/', '-', '.']).collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let n: Vec<u32> = parts.iter().filter_map(|p| p.parse().ok()).collect();
    let (y, m, d) = if parts[0].len() == 4 {
        (n[0], n[1], n[2])
    } else if parts[2].len() == 4 {
        (n[2], n[1], n[0])
    } else {
        return None;
    };
    chrono::NaiveDate::from_ymd_opt(y as i32, m, d).map(|d| d.format("%Y-%m-%d").to_string())
}

/// Word-overlap similarity 0..100 between two product names.
pub fn name_score(a: &str, b: &str) -> i64 {
    let words = |s: &str| -> std::collections::BTreeSet<String> {
        s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 2).map(|w| w.to_string()).collect()
    };
    let (x, y) = (words(a), words(b));
    if x.is_empty() || y.is_empty() {
        return 0;
    }
    let inter = x.intersection(&y).count() as i64;
    inter * 200 / (x.len() + y.len()) as i64
}

/// Match an invoice line to a product: barcode, then SKU, then name.
fn match_product(c: &Connection, line: &InvoiceLine) -> AppResult<(Option<String>, &'static str, i64)> {
    if let Some(code) = &line.code {
        if let Some(pid) =
            c.query_row("SELECT product_id FROM product_barcodes WHERE barcode=?1", [code], |r| r.get::<_, String>(0)).optional()?
        {
            return Ok((Some(pid), "barcode", 100));
        }
        if let Some(pid) =
            c.query_row("SELECT product_id FROM products WHERE sku=?1 COLLATE NOCASE", [code], |r| r.get::<_, String>(0)).optional()?
        {
            return Ok((Some(pid), "sku", 100));
        }
    }
    let first =
        line.description.split_whitespace().filter(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 3).max_by_key(|w| w.len());
    let Some(first) = first else { return Ok((None, "none", 0)) };
    let mut st = c.prepare("SELECT product_id, name FROM products WHERE active=1 AND name LIKE ?1 LIMIT 50")?;
    let cands = st.query_map([format!("%{}%", first.replace(['%', '_'], ""))], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut best: Option<(String, i64)> = None;
    for cand in cands {
        let (pid, name) = cand?;
        let s = name_score(&line.description, &name);
        if best.as_ref().map(|b| s > b.1).unwrap_or(true) {
            best = Some((pid, s));
        }
    }
    Ok(match best {
        Some((pid, s)) if s >= 60 => (Some(pid), "name", s),
        _ => (None, "none", 0),
    })
}

// ---------------------------------------------------------------- records

#[derive(Debug, Clone, Serialize)]
pub struct PaymentReview {
    pub review_id: String,
    pub review_number: String,
    pub source: String,
    pub inbox_seq: Option<i64>,
    pub phone: Option<String>,
    pub customer_id: Option<String>,
    pub customer_name: Option<String>,
    pub sale_id: Option<String>,
    pub delivery_id: Option<String>,
    pub delivery_number: Option<String>,
    pub expected_minor: Option<i64>,
    pub detected_minor: Option<i64>,
    pub detected_reference: Option<String>,
    pub ocr_confidence: Option<i64>,
    pub ocr_text: Option<String>,
    pub duplicate_of: Option<String>,
    pub status: String,
    pub reason: Option<String>,
    pub decided_by_name: Option<String>,
    pub decided_at: Option<String>,
    pub note: Option<String>,
    pub created_at: String,
}

fn load_review(c: &Connection, id: &str) -> AppResult<PaymentReview> {
    c.query_row(
        "SELECT p.review_id, p.review_number, p.source, p.inbox_seq, p.phone, p.customer_id, cu.name, p.sale_id, p.delivery_id, d.delivery_number,
                p.expected_minor, p.detected_minor, p.detected_reference, p.ocr_confidence, p.ocr_text, p.duplicate_of, p.status, p.reason,
                u.display_name, p.decided_at, p.note, p.created_at
         FROM payment_reviews p LEFT JOIN customers cu ON cu.customer_id=p.customer_id
         LEFT JOIN delivery_orders d ON d.delivery_id=p.delivery_id LEFT JOIN users u ON u.user_id=p.decided_by
         WHERE p.review_id=?1",
        [id],
        |r| {
            Ok(PaymentReview {
                review_id: r.get(0)?,
                review_number: r.get(1)?,
                source: r.get(2)?,
                inbox_seq: r.get(3)?,
                phone: r.get(4)?,
                customer_id: r.get(5)?,
                customer_name: r.get(6)?,
                sale_id: r.get(7)?,
                delivery_id: r.get(8)?,
                delivery_number: r.get(9)?,
                expected_minor: r.get(10)?,
                detected_minor: r.get(11)?,
                detected_reference: r.get(12)?,
                ocr_confidence: r.get(13)?,
                ocr_text: r.get(14)?,
                duplicate_of: r.get(15)?,
                status: r.get(16)?,
                reason: r.get(17)?,
                decided_by_name: r.get(18)?,
                decided_at: r.get(19)?,
                note: r.get(20)?,
                created_at: r.get(21)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("Payment review"))
}

/// Compare what OCR found with what we expect.
fn evaluate(expected: Option<i64>, detected: Option<i64>, confidence: Option<i64>, duplicate: bool) -> (&'static str, &'static str) {
    if duplicate {
        return ("needs_review", "duplicate_image");
    }
    match (expected, detected) {
        (_, None) => ("needs_review", "amount_not_found"),
        (None, Some(_)) => ("needs_review", "no_expected_amount"),
        (Some(e), Some(d)) if e != d => ("mismatch", "amount_differs"),
        _ if confidence.unwrap_or(0) < MIN_CONFIDENCE => ("needs_review", "low_confidence"),
        _ => ("matched", "amount_matches"),
    }
}

/// Save an uploaded image (sent by the UI as base64) inside the data folder.
fn store_image(dir: &Path, file_name: &str, data_b64: &str, id: &str) -> AppResult<(std::path::PathBuf, String)> {
    let ext = Path::new(file_name).extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    if !IMAGE_EXT.contains(&ext.as_str()) {
        return Err(AppError::validation(
            "Choose an image file (PNG, JPG, WEBP, BMP or TIFF). For a PDF invoice, save a page as an image first.",
        ));
    }
    if data_b64.len() > 28 * 1024 * 1024 {
        return Err(AppError::validation("The image is larger than 20 MB."));
    }
    let bytes = crate::ids::b64_decode(data_b64).ok_or_else(|| AppError::validation("The image could not be read."))?;
    if bytes.is_empty() {
        return Err(AppError::validation("The image is empty."));
    }
    std::fs::create_dir_all(dir)?;
    let dst = dir.join(format!("{id}.{ext}"));
    std::fs::write(&dst, &bytes)?;
    Ok((dst, hex::encode(Sha256::digest(&bytes))))
}

/// Latest open delivery for a phone/customer with money still due.
fn expected_for(c: &Connection, phone: Option<&str>, customer_id: Option<&str>) -> AppResult<Option<(String, i64)>> {
    Ok(c.query_row(
        "SELECT delivery_id, amount_minor FROM delivery_orders
         WHERE status NOT IN ('cancelled') AND payment_status IN ('pending','cod') AND amount_minor > 0
           AND ((?1 IS NOT NULL AND phone=?1) OR (?2 IS NOT NULL AND customer_id=?2))
         ORDER BY created_at DESC LIMIT 1",
        params![phone, customer_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReviewDecision {
    pub review_id: String,
    /// confirm | reject
    pub decision: String,
    #[serde(default)]
    pub note: Option<String>,
    /// Confirm against this delivery (defaults to the linked one).
    #[serde(default)]
    pub delivery_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InvoiceScan {
    pub scan_id: String,
    pub scan_number: String,
    pub supplier_id: Option<String>,
    pub supplier_name: Option<String>,
    pub file_name: Option<String>,
    pub status: String,
    pub ocr_confidence: Option<i64>,
    pub invoice_number: Option<String>,
    pub invoice_date: Option<String>,
    pub total_minor: Option<i64>,
    pub lines_total_minor: i64,
    pub po_id: Option<String>,
    pub po_number: Option<String>,
    pub error: Option<String>,
    pub duplicate_of: Option<String>,
    pub created_by_name: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InvoiceScanLine {
    pub line_no: i64,
    pub raw_text: String,
    pub description: Option<String>,
    pub code: Option<String>,
    pub qty_milli: Option<i64>,
    pub unit_cost_minor: Option<i64>,
    pub line_total_minor: Option<i64>,
    pub product_id: Option<String>,
    pub product_name: Option<String>,
    pub current_cost_minor: Option<i64>,
    pub match_kind: String,
    pub match_score: i64,
    pub include: bool,
}

fn load_scan(c: &Connection, id: &str) -> AppResult<InvoiceScan> {
    c.query_row(
        "SELECT s.scan_id, s.scan_number, s.supplier_id, su.name, s.file_name, s.status, s.ocr_confidence, s.invoice_number, s.invoice_date,
                s.total_minor, (SELECT COALESCE(SUM(line_total_minor),0) FROM invoice_scan_lines l WHERE l.scan_id=s.scan_id AND l.include=1),
                s.po_id, po.po_number, s.error,
                (SELECT x.scan_number FROM invoice_scans x WHERE x.image_sha256=s.image_sha256 AND x.scan_id<>s.scan_id AND x.created_at<s.created_at
                   ORDER BY x.created_at LIMIT 1),
                u.display_name, s.created_at
         FROM invoice_scans s LEFT JOIN suppliers su ON su.supplier_id=s.supplier_id LEFT JOIN purchase_orders po ON po.po_id=s.po_id
         LEFT JOIN users u ON u.user_id=s.created_by WHERE s.scan_id=?1",
        [id],
        |r| {
            Ok(InvoiceScan {
                scan_id: r.get(0)?,
                scan_number: r.get(1)?,
                supplier_id: r.get(2)?,
                supplier_name: r.get(3)?,
                file_name: r.get(4)?,
                status: r.get(5)?,
                ocr_confidence: r.get(6)?,
                invoice_number: r.get(7)?,
                invoice_date: r.get(8)?,
                total_minor: r.get(9)?,
                lines_total_minor: r.get(10)?,
                po_id: r.get(11)?,
                po_number: r.get(12)?,
                error: r.get(13)?,
                duplicate_of: r.get(14)?,
                created_by_name: r.get(15)?,
                created_at: r.get(16)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| AppError::not_found("Invoice scan"))
}

fn load_scan_lines(c: &Connection, id: &str) -> AppResult<Vec<InvoiceScanLine>> {
    let mut st = c.prepare(
        "SELECT l.line_no, l.raw_text, l.description, l.code, l.qty_milli, l.unit_cost_minor, l.line_total_minor, l.product_id, p.name,
                (SELECT MAX(pc.last_cost_minor) FROM product_costs pc WHERE pc.product_id=l.product_id), l.match_kind, l.match_score, l.include
         FROM invoice_scan_lines l LEFT JOIN products p ON p.product_id=l.product_id WHERE l.scan_id=?1 ORDER BY l.line_no",
    )?;
    let rows = st
        .query_map([id], |r| {
            Ok(InvoiceScanLine {
                line_no: r.get(0)?,
                raw_text: r.get(1)?,
                description: r.get(2)?,
                code: r.get(3)?,
                qty_milli: r.get(4)?,
                unit_cost_minor: r.get(5)?,
                line_total_minor: r.get(6)?,
                product_id: r.get(7)?,
                product_name: r.get(8)?,
                current_cost_minor: r.get(9)?,
                match_kind: r.get(10)?,
                match_score: r.get(11)?,
                include: r.get::<_, i64>(12)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScanLineUpdate {
    pub scan_id: String,
    pub line_no: i64,
    #[serde(default)]
    pub product_id: Option<String>,
    #[serde(default)]
    pub clear_product: bool,
    #[serde(default)]
    pub qty_milli: Option<i64>,
    #[serde(default)]
    pub unit_cost_minor: Option<i64>,
    #[serde(default)]
    pub include: Option<bool>,
}

/// Work for the runtime: an image to read with OCR.
#[derive(Debug, Clone, Serialize)]
pub struct OcrJob {
    pub kind: &'static str,
    pub id: String,
    pub path: String,
}

impl AppCore {
    // ------------------------------------------------------------ payment reviews

    /// Runtime: open reviews for new WhatsApp images (payment_reviews module on).
    pub fn pr_from_inbox(&self, seqs: &[i64]) -> AppResult<Vec<String>> {
        if seqs.is_empty() || !self.features().map(|f| f.is_on("payment_reviews")).unwrap_or(false) {
            return Ok(vec![]);
        }
        self.db.write(|tx| {
            let mut ids = vec![];
            for seq in seqs {
                type InboxImage = (String, Option<String>, Option<String>, Option<String>);
                let row: Option<InboxImage> = tx
                    .query_row(
                        "SELECT media_path, media_sha256, phone, customer_id FROM wa_inbox WHERE seq=?1 AND kind='image'",
                        [seq],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?;
                let Some((path, sha, phone, customer)) = row else { continue };
                ids.push(insert_review(tx, "whatsapp", Some(*seq), &path, &sha.unwrap_or_default(), phone, customer, None, None)?);
            }
            Ok(ids)
        })
    }

    /// Upload a screenshot received another way.
    pub fn pr_upload(
        &self,
        token: &str,
        file_name: &str,
        data_b64: &str,
        expected_minor: Option<i64>,
        delivery_id: Option<String>,
    ) -> AppResult<PaymentReview> {
        let s = self.session(token)?;
        s.require("payments.review")?;
        self.require_feature("payment_reviews")?;
        let id = new_id();
        let (dst, sha) = store_image(&self.data_dir.join("payment-reviews"), file_name, data_b64, &id)?;
        if let Some(e) = expected_minor {
            validate::money_non_negative(e, "Expected amount")?;
        }
        let actor = self.actor(&s, None);
        let rid = self.db.write(|tx| {
            let (delivery, expected, phone, customer) = match delivery_id.filter(|d| !d.is_empty()) {
                Some(d) => {
                    let d = validate::id(&d, "Delivery")?;
                    let (amt, phone, cust): (i64, Option<String>, Option<String>) = tx
                        .query_row("SELECT amount_minor, phone, customer_id FROM delivery_orders WHERE delivery_id=?1", [&d], |r| {
                            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                        })
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Delivery"))?;
                    (Some(d), expected_minor.or(Some(amt)), phone, cust)
                }
                None => (None, expected_minor, None, None),
            };
            let rid = insert_review(tx, "upload", None, &dst.to_string_lossy(), &sha, phone, customer, delivery, expected)?;
            audit::record(tx, &actor, "payment_review.uploaded", "payment_review", Some(&rid), None, None)?;
            Ok(rid)
        })?;
        self.db.read(|c| load_review(c, &rid))
    }

    pub fn pr_list(&self, token: &str, status: Option<String>) -> AppResult<Vec<PaymentReview>> {
        let s = self.session(token)?;
        s.require("payments.review")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT review_id FROM payment_reviews WHERE (?1 IS NULL OR status=?1 OR (?1='open' AND status IN ('pending','matched','mismatch','needs_review')))
                 ORDER BY created_at DESC LIMIT 500",
            )?;
            let ids = st.query_map([status.filter(|x| !x.is_empty())], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            ids.iter().map(|id| load_review(c, id)).collect()
        })
    }

    pub fn pr_get(&self, token: &str, review_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("payments.review")?;
        let id = validate::id(review_id, "Payment review")?;
        let (review, path) = self.db.read(|c| {
            let r = load_review(c, &id)?;
            let path: String = c.query_row("SELECT image_path FROM payment_reviews WHERE review_id=?1", [&id], |r| r.get(0))?;
            Ok((r, path))
        })?;
        let mime = match Path::new(&path).extension().and_then(|e| e.to_str()).unwrap_or("") {
            "png" => "image/png",
            "webp" => "image/webp",
            _ => "image/jpeg",
        };
        let image = self.read_data_file(&path, mime).ok();
        Ok(json!({ "review": review, "image": image }))
    }

    /// Link a review to a delivery / expected amount and evaluate again.
    pub fn pr_set_expected(
        &self,
        token: &str,
        review_id: &str,
        expected_minor: Option<i64>,
        delivery_id: Option<String>,
    ) -> AppResult<PaymentReview> {
        let s = self.session(token)?;
        s.require("payments.review")?;
        let id = validate::id(review_id, "Payment review")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let r = load_review(tx, &id)?;
            if matches!(r.status.as_str(), "confirmed" | "rejected") {
                return Err(AppError::conflict("This review is already decided."));
            }
            let delivery = match delivery_id.filter(|d| !d.is_empty()) {
                Some(d) => {
                    let d = validate::id(&d, "Delivery")?;
                    let amt: i64 = tx
                        .query_row("SELECT amount_minor FROM delivery_orders WHERE delivery_id=?1", [&d], |r| r.get(0))
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Delivery"))?;
                    Some((d, amt))
                }
                None => None,
            };
            let expected = expected_minor.or(delivery.as_ref().map(|d| d.1));
            if let Some(e) = expected {
                validate::money_non_negative(e, "Expected amount")?;
            }
            let (status, reason) = if r.ocr_text.is_some() {
                evaluate(expected, r.detected_minor, r.ocr_confidence, r.duplicate_of.is_some())
            } else {
                ("pending", "awaiting_ocr")
            };
            tx.execute(
                "UPDATE payment_reviews SET expected_minor=?2, delivery_id=COALESCE(?3, delivery_id), status=?4, reason=?5, updated_at=?6 WHERE review_id=?1",
                params![id, expected, delivery.map(|d| d.0), status, reason, time::now_str()],
            )?;
            audit::record(tx, &actor, "payment_review.expected_set", "payment_review", Some(&id), None, Some(&json!({ "expected_minor": expected })))?;
            Ok(())
        })?;
        self.db.read(|c| load_review(c, &id))
    }

    /// Confirm or reject. Confirming a mismatch or unclear review needs a note.
    pub fn pr_decide(&self, token: &str, d: ReviewDecision) -> AppResult<PaymentReview> {
        let s = self.session(token)?;
        s.require("payments.review")?;
        let id = validate::id(&d.review_id, "Payment review")?;
        let note = d.note.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(|n| n.chars().take(500).collect::<String>());
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let r = load_review(tx, &id)?;
            if matches!(r.status.as_str(), "confirmed" | "rejected") {
                return Err(AppError::conflict("This review is already decided."));
            }
            let now = time::now_str();
            match d.decision.as_str() {
                "confirm" => {
                    if r.status != "matched" && note.is_none() {
                        return Err(AppError::validation("Add a note explaining why you confirm a payment that did not match automatically."));
                    }
                    let delivery = d.delivery_id.clone().filter(|x| !x.is_empty()).map(|x| validate::id(&x, "Delivery")).transpose()?.or(r.delivery_id.clone());
                    if let Some(did) = &delivery {
                        let (status, pay): (String, String) = tx
                            .query_row("SELECT status, payment_status FROM delivery_orders WHERE delivery_id=?1", [did], |r| Ok((r.get(0)?, r.get(1)?)))
                            .optional()?
                            .ok_or_else(|| AppError::not_found("Delivery"))?;
                        if status == "cancelled" {
                            return Err(AppError::conflict("That delivery is cancelled."));
                        }
                        if pay != "paid" {
                            tx.execute("UPDATE delivery_orders SET payment_status='paid', updated_at=?2 WHERE delivery_id=?1", params![did, now])?;
                            tx.execute(
                                "INSERT INTO delivery_events(event_id, delivery_id, previous_status, new_status, note, user_id, created_at) VALUES (?1,?2,?3,?3,?4,?5,?6)",
                                params![new_id(), did, status, format!("Payment screenshot {} confirmed", r.review_number), s.user_id, now],
                            )?;
                        }
                    }
                    tx.execute(
                        "UPDATE payment_reviews SET status='confirmed', delivery_id=COALESCE(?2, delivery_id), decided_by=?3, decided_at=?4, note=?5, updated_at=?4 WHERE review_id=?1",
                        params![id, delivery, s.user_id, now, note],
                    )?;
                    audit::record(
                        tx,
                        &actor,
                        "payment_review.confirmed",
                        "payment_review",
                        Some(&id),
                        Some(&json!({ "status": r.status, "detected_minor": r.detected_minor, "expected_minor": r.expected_minor })),
                        Some(&json!({ "delivery_id": delivery, "note": note })),
                    )?;
                }
                "reject" => {
                    let note = note.ok_or_else(|| AppError::validation("Add a note explaining the rejection."))?;
                    tx.execute(
                        "UPDATE payment_reviews SET status='rejected', decided_by=?2, decided_at=?3, note=?4, updated_at=?3 WHERE review_id=?1",
                        params![id, s.user_id, now, note],
                    )?;
                    audit::record(tx, &actor, "payment_review.rejected", "payment_review", Some(&id), None, Some(&json!({ "note": note })))?;
                }
                _ => return Err(AppError::validation("Decision must be confirm or reject.")),
            }
            Ok(())
        })?;
        self.db.read(|c| load_review(c, &id))
    }

    // ------------------------------------------------------------ invoice scans

    pub fn inv_import(&self, token: &str, file_name: &str, data_b64: &str, supplier_id: Option<String>) -> AppResult<InvoiceScan> {
        let s = self.session(token)?;
        s.require("ocr.scan")?;
        self.require_feature("ocr")?;
        let id = new_id();
        let (dst, sha) = store_image(&self.data_dir.join("invoice-scans"), file_name, data_b64, &id)?;
        let supplier = supplier_id.filter(|x| !x.is_empty()).map(|x| validate::id(&x, "Supplier")).transpose()?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            if let Some(sid) = &supplier {
                tx.query_row("SELECT 1 FROM suppliers WHERE supplier_id=?1", [sid], |_| Ok(())).optional()?.ok_or_else(|| AppError::not_found("Supplier"))?;
            }
            let number = format!("IS-{:05}", next_seq(tx, "invoice_scan")?);
            let now = time::now_str();
            tx.execute(
                "INSERT INTO invoice_scans(scan_id, scan_number, supplier_id, image_path, image_sha256, file_name, status, created_by, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,'imported',?7,?8,?8)",
                params![id, number, supplier, dst.to_string_lossy(), sha, file_name.chars().take(200).collect::<String>(), s.user_id, now],
            )?;
            audit::record(tx, &actor, "invoice_scan.imported", "invoice_scan", Some(&id), None, Some(&json!({ "number": number })))?;
            Ok(())
        })?;
        self.db.read(|c| load_scan(c, &id))
    }

    pub fn inv_list(&self, token: &str, status: Option<String>) -> AppResult<Vec<InvoiceScan>> {
        let s = self.session(token)?;
        s.require("ocr.scan")?;
        self.db.read(|c| {
            let mut st =
                c.prepare("SELECT scan_id FROM invoice_scans WHERE (?1 IS NULL OR status=?1) ORDER BY created_at DESC LIMIT 300")?;
            let ids = st.query_map([status.filter(|x| !x.is_empty())], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            ids.iter().map(|id| load_scan(c, id)).collect()
        })
    }

    pub fn inv_get(&self, token: &str, scan_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("ocr.scan")?;
        let id = validate::id(scan_id, "Invoice scan")?;
        let (scan, lines, text, path) = self.db.read(|c| {
            let (text, path): (Option<String>, String) =
                c.query_row("SELECT ocr_text, image_path FROM invoice_scans WHERE scan_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok((load_scan(c, &id)?, load_scan_lines(c, &id)?, text, path))
        })?;
        let mime = if path.ends_with(".png") { "image/png" } else { "image/jpeg" };
        Ok(json!({ "scan": scan, "lines": lines, "ocr_text": text, "image": self.read_data_file(&path, mime).ok() }))
    }

    pub fn inv_update_line(&self, token: &str, u: ScanLineUpdate) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("ocr.scan")?;
        let id = validate::id(&u.scan_id, "Invoice scan")?;
        self.db.write(|tx| {
            let scan = load_scan(tx, &id)?;
            if scan.status != "review" {
                return Err(AppError::conflict("Only scans waiting for review can be edited."));
            }
            let exists = tx
                .query_row("SELECT 1 FROM invoice_scan_lines WHERE scan_id=?1 AND line_no=?2", params![id, u.line_no], |_| Ok(()))
                .optional()?;
            exists.ok_or_else(|| AppError::not_found("Invoice line"))?;
            if let Some(pid) = u.product_id.as_deref().filter(|p| !p.is_empty()) {
                let pid = validate::id(pid, "Product")?;
                tx.query_row("SELECT 1 FROM products WHERE product_id=?1", [&pid], |_| Ok(()))
                    .optional()?
                    .ok_or_else(|| AppError::not_found("Product"))?;
                tx.execute(
                    "UPDATE invoice_scan_lines SET product_id=?3, match_kind='manual', match_score=100 WHERE scan_id=?1 AND line_no=?2",
                    params![id, u.line_no, pid],
                )?;
            } else if u.clear_product {
                tx.execute(
                    "UPDATE invoice_scan_lines SET product_id=NULL, match_kind='none', match_score=0 WHERE scan_id=?1 AND line_no=?2",
                    params![id, u.line_no],
                )?;
            }
            if let Some(q) = u.qty_milli {
                validate::qty_positive(q, true, "Quantity")?;
                tx.execute("UPDATE invoice_scan_lines SET qty_milli=?3 WHERE scan_id=?1 AND line_no=?2", params![id, u.line_no, q])?;
            }
            if let Some(c) = u.unit_cost_minor {
                validate::money_non_negative(c, "Unit cost")?;
                tx.execute("UPDATE invoice_scan_lines SET unit_cost_minor=?3 WHERE scan_id=?1 AND line_no=?2", params![id, u.line_no, c])?;
            }
            if let Some(inc) = u.include {
                tx.execute("UPDATE invoice_scan_lines SET include=?3 WHERE scan_id=?1 AND line_no=?2", params![id, u.line_no, inc as i64])?;
            }
            tx.execute("UPDATE invoice_scans SET updated_at=?2 WHERE scan_id=?1", params![id, time::now_str()])?;
            Ok(())
        })?;
        self.inv_get(token, &id)
    }

    /// Turn a reviewed scan into a draft purchase order. Nothing is received:
    /// stock changes only when the order is received on the Receiving page.
    pub fn inv_confirm(&self, token: &str, scan_id: &str, supplier_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("ocr.scan")?;
        s.require("purchasing.manage")?;
        let id = validate::id(scan_id, "Invoice scan")?;
        let supplier = validate::id(supplier_id, "Supplier")?;
        // Claim the scan first so two confirmations cannot create two orders.
        let lines = self.db.write(|tx| {
            let scan = load_scan(tx, &id)?;
            if scan.status != "review" {
                return Err(AppError::conflict("This scan is not waiting for review."));
            }
            let lines: Vec<InvoiceScanLine> = load_scan_lines(tx, &id)?.into_iter().filter(|l| l.include).collect();
            if lines.is_empty() {
                return Err(AppError::validation("Include at least one line."));
            }
            if let Some(l) = lines.iter().find(|l| l.product_id.is_none() || l.qty_milli.unwrap_or(0) <= 0 || l.unit_cost_minor.is_none()) {
                return Err(AppError::validation(format!(
                    "Line {} needs a product, a quantity and a unit cost (or exclude it).",
                    l.line_no
                )));
            }
            tx.execute(
                "UPDATE invoice_scans SET status='confirmed', updated_at=?2 WHERE scan_id=?1 AND status='review'",
                params![id, time::now_str()],
            )?;
            Ok(lines)
        })?;
        let (reference, date) = self.db.read(|c| {
            Ok(c.query_row("SELECT invoice_number, scan_number FROM invoice_scans WHERE scan_id=?1", [&id], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
            })?)
        })?;
        let input = crate::purchasing::PoInput {
            supplier_id: supplier.clone(),
            reference: reference.clone().or(Some(date.clone())),
            expected_at: None,
            notes: Some(format!("Created from invoice scan {date}. Check quantities and costs before receiving.")),
            lines: lines
                .iter()
                .map(|l| crate::purchasing::PoLineInput {
                    product_id: l.product_id.clone().unwrap_or_default(),
                    qty_milli: l.qty_milli.unwrap_or(0),
                    unit_cost_minor: l.unit_cost_minor.unwrap_or(0),
                    tax_rate_bp: 0,
                })
                .collect(),
        };
        let po = match self.purchase_order_save(token, None, input) {
            Ok(po) => po,
            Err(e) => {
                let _ = self.db.write(|tx| {
                    tx.execute("UPDATE invoice_scans SET status='review', updated_at=?2 WHERE scan_id=?1", params![id, time::now_str()])?;
                    Ok(())
                });
                return Err(e);
            }
        };
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            tx.execute(
                "UPDATE invoice_scans SET po_id=?2, supplier_id=?3, updated_at=?4 WHERE scan_id=?1",
                params![id, po.header.po_id, supplier, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "invoice_scan.confirmed",
                "invoice_scan",
                Some(&id),
                None,
                Some(&json!({ "po_id": po.header.po_id, "lines": lines.len() })),
            )?;
            Ok(())
        })?;
        self.inv_get(token, &id)
    }

    pub fn inv_reject(&self, token: &str, scan_id: &str, reason: &str) -> AppResult<InvoiceScan> {
        let s = self.session(token)?;
        s.require("ocr.scan")?;
        let id = validate::id(scan_id, "Invoice scan")?;
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(AppError::validation("Enter a reason."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let scan = load_scan(tx, &id)?;
            if matches!(scan.status.as_str(), "confirmed" | "rejected") {
                return Err(AppError::conflict("This scan is already closed."));
            }
            tx.execute(
                "UPDATE invoice_scans SET status='rejected', error=?2, updated_at=?3 WHERE scan_id=?1",
                params![id, reason.chars().take(300).collect::<String>(), time::now_str()],
            )?;
            audit::record(tx, &actor, "invoice_scan.rejected", "invoice_scan", Some(&id), None, Some(&json!({ "reason": reason })))?;
            Ok(())
        })?;
        self.db.read(|c| load_scan(c, &id))
    }

    /// Queue a failed scan or review for another OCR attempt.
    pub fn ocr_retry(&self, token: &str, kind: &str, id: &str) -> AppResult<()> {
        let s = self.session(token)?;
        let id = validate::id(id, "Record")?;
        self.db.write(|tx| {
            let n = match kind {
                "invoice" => {
                    s.require("ocr.scan")?;
                    tx.execute("UPDATE invoice_scans SET status='imported', error=NULL WHERE scan_id=?1 AND status='failed'", [&id])?
                }
                "payment" => {
                    s.require("payments.review")?;
                    tx.execute("UPDATE payment_reviews SET ocr_text=NULL, status='pending', reason='awaiting_ocr' WHERE review_id=?1 AND status NOT IN ('confirmed','rejected')", [&id])?
                }
                _ => return Err(AppError::validation("Unknown OCR record type.")),
            };
            if n == 0 {
                return Err(AppError::conflict("Nothing to retry."));
            }
            Ok(())
        })
    }

    // ------------------------------------------------------------ runtime hooks

    /// Runtime: images waiting for OCR (only when the OCR module is on).
    pub fn ocr_pending(&self, limit: i64) -> AppResult<Vec<OcrJob>> {
        let f = self.features()?;
        if !f.is_on("ocr") {
            return Ok(vec![]);
        }
        self.db.read(|c| {
            let mut jobs = vec![];
            let mut st = c.prepare("SELECT scan_id, image_path FROM invoice_scans WHERE status='imported' ORDER BY created_at LIMIT ?1")?;
            for r in st.query_map([limit], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
                let (id, path) = r?;
                jobs.push(OcrJob { kind: "invoice", id, path });
            }
            if f.is_on("payment_reviews") {
                let mut st = c.prepare("SELECT review_id, image_path FROM payment_reviews WHERE status='pending' AND ocr_text IS NULL ORDER BY created_at LIMIT ?1")?;
                for r in st.query_map([limit], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
                    let (id, path) = r?;
                    jobs.push(OcrJob { kind: "payment", id, path });
                }
            }
            Ok(jobs)
        })
    }

    /// Runtime: store OCR output (or the failure) for a job.
    pub fn ocr_result(&self, kind: &str, id: &str, result: Result<(String, i64), String>) -> AppResult<()> {
        let now = time::now_str();
        let digits = self.db.read(|c| self.currency(c))?.1;
        self.db.write(|tx| {
            match (kind, result) {
                ("invoice", Ok((text, conf))) => {
                    let ex = parse_invoice(&text, digits);
                    tx.execute("DELETE FROM invoice_scan_lines WHERE scan_id=?1", [id])?;
                    for (i, l) in ex.lines.iter().enumerate() {
                        let (pid, kind, score) = match_product(tx, l)?;
                        tx.execute(
                            "INSERT INTO invoice_scan_lines(scan_id, line_no, raw_text, description, code, qty_milli, unit_cost_minor, line_total_minor, product_id, match_kind, match_score, include)
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,1)",
                            params![id, i as i64 + 1, l.raw_text, l.description, l.code, l.qty_milli, l.unit_cost_minor, l.line_total_minor, pid, kind, score],
                        )?;
                    }
                    tx.execute(
                        "UPDATE invoice_scans SET status='review', ocr_text=?2, ocr_confidence=?3, invoice_number=?4, invoice_date=?5, total_minor=?6, error=NULL, updated_at=?7
                         WHERE scan_id=?1 AND status='imported'",
                        params![id, text.chars().take(50_000).collect::<String>(), conf, ex.invoice_number, ex.invoice_date, ex.total_minor, now],
                    )?;
                }
                ("invoice", Err(e)) => {
                    tx.execute("UPDATE invoice_scans SET status='failed', error=?2, updated_at=?3 WHERE scan_id=?1", params![id, e, now])?;
                }
                ("payment", Ok((text, conf))) => {
                    let ex = parse_payment(&text, digits);
                    let (expected, dup): (Option<i64>, Option<String>) = tx.query_row(
                        "SELECT expected_minor, duplicate_of FROM payment_reviews WHERE review_id=?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    // The same bank reference on another review is also a duplicate.
                    let ref_dup: Option<String> = match &ex.reference {
                        Some(r) => tx
                            .query_row(
                                "SELECT review_number FROM payment_reviews WHERE detected_reference=?1 AND review_id<>?2 LIMIT 1",
                                params![r, id],
                                |x| x.get(0),
                            )
                            .optional()?,
                        None => None,
                    };
                    let dup = dup.or(ref_dup);
                    let (status, reason) = evaluate(expected, ex.amount_minor, Some(conf), dup.is_some());
                    tx.execute(
                        "UPDATE payment_reviews SET ocr_text=?2, ocr_confidence=?3, detected_minor=?4, detected_reference=?5, duplicate_of=?6, status=?7, reason=?8, updated_at=?9
                         WHERE review_id=?1 AND status='pending'",
                        params![id, text.chars().take(20_000).collect::<String>(), conf, ex.amount_minor, ex.reference, dup, status, reason, now],
                    )?;
                }
                ("payment", Err(e)) => {
                    tx.execute(
                        "UPDATE payment_reviews SET ocr_text='', status='needs_review', reason='ocr_failed', note=COALESCE(note, ?2), updated_at=?3 WHERE review_id=?1 AND status='pending'",
                        params![id, e.chars().take(300).collect::<String>(), now],
                    )?;
                }
                _ => return Err(AppError::new(ErrorCode::Validation, "Unknown OCR job.")),
            }
            Ok(())
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_review(
    tx: &Connection,
    source: &str,
    inbox_seq: Option<i64>,
    path: &str,
    sha: &str,
    phone: Option<String>,
    customer: Option<String>,
    delivery: Option<String>,
    expected: Option<i64>,
) -> AppResult<String> {
    let dup: Option<String> = if sha.is_empty() {
        None
    } else {
        tx.query_row("SELECT review_number FROM payment_reviews WHERE image_sha256=?1 ORDER BY created_at LIMIT 1", [sha], |r| r.get(0))
            .optional()?
    };
    let (delivery, expected) = match (delivery, expected) {
        (None, None) => match expected_for(tx, phone.as_deref(), customer.as_deref())? {
            Some((d, amt)) => (Some(d), Some(amt)),
            None => (None, None),
        },
        other => other,
    };
    let id = new_id();
    let number = format!("PR-{:05}", next_seq(tx, "payment_review")?);
    let now = time::now_str();
    tx.execute(
        "INSERT INTO payment_reviews(review_id, review_number, source, inbox_seq, image_path, image_sha256, phone, customer_id, delivery_id, expected_minor,
            duplicate_of, status, reason, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'pending','awaiting_ocr',?12,?12)",
        params![id, number, source, inbox_seq, path, sha, phone, customer, delivery, expected, dup, now],
    )?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payment_amount_and_reference() {
        let t = "BenefitPay\nPayment successful\nAmount BHD 12.500\nTo: Store\nReference No: 123456789012\n24/09/2026 10:15";
        let e = parse_payment(t, 3);
        assert_eq!(e.amount_minor, Some(12_500));
        assert_eq!(e.reference.as_deref(), Some("123456789012"));
        // Arabic digits and currency
        let e = parse_payment("المبلغ ١٢٫٥٠٠ د.ب", 3);
        assert_eq!(e.amount_minor, Some(12_500));
        // A phone number alone is not an amount
        assert_eq!(parse_payment("Call 33334444", 3).amount_minor, None);
    }

    #[test]
    fn review_evaluation() {
        assert_eq!(evaluate(Some(12_500), Some(12_500), Some(90), false).0, "matched");
        assert_eq!(evaluate(Some(12_500), Some(12_000), Some(90), false).0, "mismatch");
        assert_eq!(evaluate(Some(12_500), Some(12_500), Some(40), false).0, "needs_review");
        assert_eq!(evaluate(Some(12_500), Some(12_500), Some(90), true).1, "duplicate_image");
        assert_eq!(evaluate(None, Some(1), Some(90), false).1, "no_expected_amount");
        assert_eq!(evaluate(Some(1), None, Some(90), false).1, "amount_not_found");
    }

    #[test]
    fn invoice_lines_and_header() {
        let t = "ACME Trading W.L.L.\nInvoice No: INV-2291\nDate: 21/09/2026\n\
                 6291041500213 Milk Full Cream 1L 12 x 0.450 5.400\n\
                 Rice Basmati 5kg 2 3.250 6.500\n\
                 SKU-77A Tea Bags 100s 1.900\n\
                 Subtotal 13.800\nVAT 10% 1.380\nTotal 15.180";
        let ex = parse_invoice(t, 3);
        assert_eq!(ex.invoice_number.as_deref(), Some("INV-2291"));
        assert_eq!(ex.invoice_date.as_deref(), Some("2026-09-21"));
        assert_eq!(ex.total_minor, Some(15_180));
        assert_eq!(ex.lines.len(), 3, "{:#?}", ex.lines);
        let l = &ex.lines[0];
        assert_eq!(l.code.as_deref(), Some("6291041500213"));
        assert_eq!(l.qty_milli, Some(12_000));
        assert_eq!(l.unit_cost_minor, Some(450));
        assert_eq!(l.line_total_minor, Some(5_400));
        assert!(l.description.contains("Milk"));
        let l = &ex.lines[1];
        assert_eq!((l.qty_milli, l.unit_cost_minor, l.line_total_minor), (Some(2_000), Some(3_250), Some(6_500)));
        let l = &ex.lines[2];
        assert_eq!(l.code.as_deref(), Some("SKU-77A"));
        assert_eq!(l.line_total_minor, Some(1_900));
        assert_eq!(l.qty_milli, None);
    }

    #[test]
    fn name_similarity() {
        assert!(name_score("Milk Full Cream 1L", "Almarai Milk Full Cream 1L") >= 60);
        assert!(name_score("Rice Basmati", "Tea Bags") < 60);
    }
}
