//! Rules extraction: layout lines → classification, header fields and item
//! lines, each with its evidence. Deterministic and offline; an AI pass may
//! add to it (see `aischema`) but never bypasses the checks that follow.

use super::layout::{norm_box, DocLine, Layout};
use super::{Band, Classification, DocFields, DocType, Evidence, ExtractedLine, Extraction, Field};
use crate::money::parse_decimal;
use crate::ocrflow::normalize_digits;

/// Below this mean word confidence a value is flagged as unreliable.
pub const LOW_OCR_CONF: i64 = 60;

// ---------------------------------------------------------------- tokens

#[derive(Debug, Clone, PartialEq)]
pub struct Num {
    /// Absolute value in minor units (for integers: the integer × 10^digits).
    pub minor: i64,
    pub negative: bool,
    /// Digits after the decimal point as printed (0 for integers).
    pub decimals: usize,
    pub percent: bool,
    pub text: String,
}

impl Num {
    pub fn int(&self, digits: u32) -> Option<i64> {
        (self.decimals == 0).then(|| self.minor / 10i64.pow(digits))
    }
}

/// A printed number: `1,234.500`, `12.5`, `(1.250)`, `-3`, `10%`, `BD12.500`.
/// More than `digits` decimals is not money and not accepted.
pub fn parse_num(tok: &str, digits: u32) -> Option<Num> {
    let t = normalize_digits(tok);
    let mut t = t.trim().trim_end_matches([',', ';', ':']).to_string();
    for cur in ["BHD", "bhd", "BD", "bd", "د.ب", "Bd"] {
        t = t.trim_start_matches(cur).trim_end_matches(cur).to_string();
    }
    let percent = t.ends_with('%');
    let mut t = t.trim_end_matches('%').trim().to_string();
    let mut negative = false;
    if t.starts_with('(') && t.ends_with(')') {
        negative = true;
        t = t[1..t.len() - 1].to_string();
    }
    if let Some(r) = t.strip_prefix('-') {
        negative = true;
        t = r.to_string();
    }
    if t.is_empty() || !t.chars().next()?.is_ascii_digit() || !t.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ',') {
        return None;
    }
    let decimals = match t.rfind('.') {
        Some(p) => t.len() - p - 1,
        None => 0,
    };
    if decimals > digits as usize || t.matches('.').count() > 1 {
        return None;
    }
    // Thousands separators are only valid before a decimal point or in groups of 3.
    if t.contains(',') {
        let int_part = t.split('.').next().unwrap_or("");
        let groups: Vec<&str> = int_part.split(',').collect();
        if groups.iter().skip(1).any(|g| g.len() != 3) || groups[0].is_empty() || groups[0].len() > 3 {
            return None;
        }
    }
    let clean = t.replace(',', "");
    if clean.len() > 16 {
        return None;
    }
    let minor = parse_decimal(&clean, digits).ok()?;
    Some(Num { minor, negative, decimals, percent, text: tok.to_string() })
}

fn is_barcode(t: &str) -> bool {
    matches!(t.len(), 8 | 12 | 13 | 14) && t.chars().all(|c| c.is_ascii_digit())
}

/// A supplier item code: letters and digits (e.g. `CC-330`, `A1234`).
fn is_code(t: &str) -> bool {
    let t = t.trim_matches(|c: char| c == ',' || c == ':');
    t.len() >= 3
        && t.len() <= 20
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars().any(|c| c.is_ascii_alphabetic())
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '/')
        && !is_size_token(t)
}

fn is_size_token(t: &str) -> bool {
    let l = t.to_lowercase();
    let unit_start = l.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(l.len());
    let (n, u) = l.split_at(unit_start);
    !n.is_empty()
        && n.chars().all(|c| c.is_ascii_digit() || c == '.')
        && matches!(u, "ml" | "l" | "ltr" | "lt" | "g" | "gm" | "gms" | "kg" | "pcs" | "pc" | "cl" | "oz" | "x" | "s" | "'s")
}

const UNIT_WORDS: &[(&str, &str)] = &[
    ("ctn", "ctn"),
    ("ctns", "ctn"),
    ("carton", "ctn"),
    ("cartons", "ctn"),
    ("cs", "ctn"),
    ("case", "ctn"),
    ("cases", "ctn"),
    ("box", "box"),
    ("boxes", "box"),
    ("pkt", "pkt"),
    ("pkts", "pkt"),
    ("pack", "pkt"),
    ("packs", "pkt"),
    ("pcs", "pcs"),
    ("pc", "pcs"),
    ("ea", "pcs"),
    ("each", "pcs"),
    ("nos", "pcs"),
    ("no", "pcs"),
    ("units", "pcs"),
    ("unit", "pcs"),
    ("btl", "pcs"),
    ("kg", "kg"),
    ("كرتون", "ctn"),
    ("كراتين", "ctn"),
    ("حبة", "pcs"),
    ("حبه", "pcs"),
    ("علبة", "pkt"),
    ("كيلو", "kg"),
];

fn unit_word(t: &str) -> Option<&'static str> {
    let l = t.to_lowercase();
    let l = l.trim_matches(|c: char| c == '.' || c == ',' || c == ':');
    UNIT_WORDS.iter().find(|(w, _)| *w == l).map(|(_, u)| *u)
}

// ---------------------------------------------------------------- packs

/// Read packaging from a description: `24 x 330ml`, `6x1L`, `12 pcs`,
/// `1 CTN = 48`, `pack 6`, `48's`, `كرتون 24 حبة`.
pub fn parse_pack(desc: &str) -> (Option<i64>, Option<String>) {
    let t = normalize_digits(desc).to_lowercase().replace('×', "x");
    let words: Vec<&str> = t.split_whitespace().collect();
    let joined = words.join(" ");
    let num = |s: &str| s.trim().parse::<i64>().ok().filter(|n| (2..=500).contains(n));
    // "1 ctn = 48" / "ctn=48" / "1 carton = 24 pcs"
    if let Some(p) = joined.find('=') {
        let (l, r) = joined.split_at(p);
        if ["ctn", "carton", "case", "cs", "box", "كرتون"].iter().any(|w| l.contains(w)) {
            let n = r[1..].split_whitespace().next().and_then(|x| num(x.trim_end_matches(|c: char| !c.is_ascii_digit())));
            if let Some(n) = n {
                return (
                    Some(n),
                    Some(
                        joined[joined[..p].rfind(|c: char| c.is_ascii_digit()).map(|i| i.saturating_sub(1)).unwrap_or(0)..]
                            .trim()
                            .chars()
                            .take(40)
                            .collect(),
                    ),
                );
            }
        }
    }
    // "24x330ml", "24 x 330 ml", "6x1l", "24 x 250"
    for (i, w) in words.iter().enumerate() {
        let (a, b) = if *w == "x" && i > 0 && i + 1 < words.len() {
            (words[i - 1].to_string(), words[i + 1].to_string())
        } else if let Some((a, b)) = w.split_once('x').filter(|(a, b)| !a.is_empty() && !b.is_empty()) {
            (a.to_string(), b.to_string())
        } else if w.ends_with('x') && i + 1 < words.len() && w.len() > 1 {
            (w.trim_end_matches('x').to_string(), words[i + 1].to_string())
        } else if w.starts_with('x') && i > 0 && w.len() > 1 {
            (words[i - 1].to_string(), w.trim_start_matches('x').to_string())
        } else {
            continue;
        };
        if let Some(n) = num(&a) {
            let b0: String = b.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            if !b0.is_empty() {
                let text = format!("{a} x {b}");
                return (Some(n), Some(text));
            }
        }
    }
    // "12 pcs", "48's", "pack 6", "6 pack", "24 حبة"
    for (i, w) in words.iter().enumerate() {
        if let Some(n) = w.strip_suffix("'s").and_then(num) {
            return (Some(n), Some((*w).to_string()));
        }
        let next = words.get(i + 1).copied().unwrap_or("");
        if let Some(n) = num(w) {
            if matches!(next, "pcs" | "pc" | "pieces" | "pack" | "حبة" | "حبه" | "قطعة") {
                return (Some(n), Some(format!("{w} {next}")));
            }
        }
        if matches!(*w, "pack" | "pk" | "pkt") {
            if let Some(n) = num(next) {
                return (Some(n), Some(format!("{w} {next}")));
            }
        }
        if let Some(n) = w.strip_suffix("pcs").and_then(num) {
            return (Some(n), Some((*w).to_string()));
        }
    }
    (None, None)
}

// ---------------------------------------------------------------- dates

const MONTHS: &[(&str, u32)] = &[
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
    ("يناير", 1),
    ("فبراير", 2),
    ("مارس", 3),
    ("أبريل", 4),
    ("ابريل", 4),
    ("مايو", 5),
    ("يونيو", 6),
    ("يوليو", 7),
    ("أغسطس", 8),
    ("اغسطس", 8),
    ("سبتمبر", 9),
    ("أكتوبر", 10),
    ("اكتوبر", 10),
    ("نوفمبر", 11),
    ("ديسمبر", 12),
];

/// A date read from text. `ambiguous` holds the other reading when day and
/// month could be swapped (03/04/2026).
#[derive(Debug, Clone, PartialEq)]
pub struct DateRead {
    pub date: String,
    pub ambiguous: Option<String>,
    pub text: String,
}

fn ymd(y: i32, m: u32, d: u32) -> Option<String> {
    chrono::NaiveDate::from_ymd_opt(y, m, d).map(|d| d.format("%Y-%m-%d").to_string())
}

fn year4(y: u32) -> i32 {
    if y < 100 {
        2000 + y as i32
    } else {
        y as i32
    }
}

/// Dates in a line: `28/09/2026`, `28-09-26`, `2026-09-28`, `28.09.2026`,
/// `28 Sep 2026`, `Sep 28, 2026`, `28-Sep-2026`. Day-first (Bahrain) when
/// both readings are valid, with the other reading kept as `ambiguous`.
pub fn find_dates(line: &str) -> Vec<DateRead> {
    let t = normalize_digits(line);
    let mut out = vec![];
    let words: Vec<&str> = t.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        let tok = w.trim_matches(|c: char| !c.is_alphanumeric());
        let parts: Vec<&str> = tok.split(['/', '-', '.']).collect();
        if parts.len() == 3 {
            // 28-Sep-2026
            if let Some(m) = MONTHS.iter().find(|(n, _)| parts[1].to_lowercase().starts_with(n)).map(|x| x.1) {
                if let (Ok(d), Ok(y)) = (parts[0].parse::<u32>(), parts[2].parse::<u32>()) {
                    if let Some(s) = ymd(year4(y), m, d) {
                        out.push(DateRead { date: s, ambiguous: None, text: tok.to_string() });
                    }
                }
                continue;
            }
            if !parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) {
                continue;
            }
            let n: Vec<u32> = parts.iter().filter_map(|p| p.parse().ok()).collect();
            if parts[0].len() == 4 {
                if let Some(s) = ymd(n[0] as i32, n[1], n[2]) {
                    out.push(DateRead { date: s, ambiguous: None, text: tok.to_string() });
                }
            } else if parts[2].len() == 4 || parts[2].len() == 2 {
                let y = year4(n[2]);
                let dm = ymd(y, n[1], n[0]);
                let md = ymd(y, n[0], n[1]);
                match (dm, md) {
                    (Some(a), Some(b)) if a != b => out.push(DateRead { date: a, ambiguous: Some(b), text: tok.to_string() }),
                    (Some(a), _) => out.push(DateRead { date: a, ambiguous: None, text: tok.to_string() }),
                    (None, Some(b)) => out.push(DateRead { date: b, ambiguous: None, text: tok.to_string() }),
                    _ => {}
                }
            }
            continue;
        }
        // "28 Sep 2026" / "Sep 28, 2026" / "28 سبتمبر 2026"
        let low = tok.to_lowercase();
        if let Some(m) = MONTHS.iter().find(|(n, _)| low.starts_with(n) && low.len() <= 9).map(|x| x.1) {
            let prev = i.checked_sub(1).and_then(|j| words.get(j)).map(|x| x.trim_matches(|c: char| !c.is_ascii_digit()));
            let next = words.get(i + 1).map(|x| x.trim_matches(|c: char| !c.is_ascii_digit()));
            let next2 = words.get(i + 2).map(|x| x.trim_matches(|c: char| !c.is_ascii_digit()));
            let parsed = match (prev.and_then(|p| p.parse::<u32>().ok()), next.and_then(|p| p.parse::<u32>().ok())) {
                (Some(d), Some(y)) if d <= 31 && y >= 1000 => {
                    ymd(y as i32, m, d).map(|s| (s, format!("{} {} {}", prev.unwrap_or(""), tok, y)))
                }
                (_, Some(d)) if d <= 31 => next2
                    .and_then(|y| y.parse::<u32>().ok())
                    .filter(|y| *y >= 1000)
                    .and_then(|y| ymd(y as i32, m, d).map(|s| (s, format!("{tok} {d} {y}")))),
                _ => None,
            };
            if let Some((s, text)) = parsed {
                out.push(DateRead { date: s, ambiguous: None, text });
            }
        }
    }
    out
}

// ---------------------------------------------------------------- labels

fn has_any(low: &str, words: &[&str]) -> bool {
    words.iter().any(|w| low.contains(w))
}

const INVOICE_NO: &[&str] = &[
    "invoice no",
    "invoice number",
    "invoice #",
    "invoice#",
    "inv no",
    "inv. no",
    "inv #",
    "bill no",
    "document no",
    "doc no",
    "credit note no",
    "credit note number",
    "cn no",
    "c.n. no",
    "delivery note no",
    "dn no",
    "رقم الفاتورة",
    "فاتورة رقم",
    "رقم الإشعار",
    "رقم اشعار",
];
const PO_NO: &[&str] =
    &["po no", "po number", "p.o", "purchase order", "lpo", "order no", "po #", "رقم أمر الشراء", "أمر شراء", "امر شراء"];
const DUE_DATE: &[&str] = &["due date", "payment due", "due on", "pay by", "تاريخ الاستحقاق"];
const DELIVERY_DATE: &[&str] = &["delivery date", "delivered on", "date of delivery", "dispatch date", "تاريخ التسليم", "تاريخ التوصيل"];
const INVOICE_DATE: &[&str] = &["invoice date", "inv date", "date of issue", "issue date", "date", "تاريخ الفاتورة", "التاريخ", "تاريخ"];
const VAT_ID: &[&str] = &[
    "vat no",
    "vat reg",
    "vat number",
    "vat account",
    "vatin",
    "trn",
    "tax registration",
    "tax reg",
    "vat id",
    "tin",
    "الرقم الضريبي",
    "رقم التسجيل الضريبي",
    "رقم حساب ضريبة",
];
const CR_ID: &[&str] =
    &["c.r", "cr no", "cr.", "cr:", "cr ", "commercial registration", "comm. reg", "reg. no", "السجل التجاري", "س.ت", "سجل تجاري"];
const BUYER: &[&str] =
    &["bill to", "billed to", "sold to", "customer", "buyer", "ship to", "invoice to", "العميل", "المشتري", "فاتورة إلى"];
const SUBTOTAL: &[&str] = &[
    "subtotal",
    "sub total",
    "sub-total",
    "total excl",
    "total before vat",
    "total before tax",
    "taxable amount",
    "taxable value",
    "net amount",
    "amount before vat",
    "total (excl",
    "المجموع الفرعي",
    "الإجمالي قبل الضريبة",
    "المبلغ الخاضع",
    "المجموع قبل الضريبة",
];
const GRAND: &[&str] = &[
    "grand total",
    "total incl",
    "total amount",
    "amount due",
    "net payable",
    "total payable",
    "balance due",
    "invoice total",
    "total (incl",
    "total bhd",
    "total bd",
    "الإجمالي شامل",
    "المبلغ المستحق",
    "الإجمالي الكلي",
    "المجموع الكلي",
];
const VAT_AMT: &[&str] = &["vat", "tax", "ضريبة القيمة المضافة", "الضريبة", "ض.ق.م"];
const DISCOUNT: &[&str] = &["discount", "disc", "الخصم"];
const ZERO_RATED: &[&str] = &["zero rated", "zero-rated", "0% vat", "خاضع لنسبة الصفر"];
const EXEMPT: &[&str] = &["exempt", "معفى", "معفاة"];

fn is_summary_line(low: &str) -> bool {
    has_any(low, SUBTOTAL)
        || has_any(low, GRAND)
        || (low.contains("total") && !low.contains("line total"))
        || has_any(low, &["الإجمالي", "المجموع", "balance", "amount in words", "رصيد"])
        || ((low.starts_with("vat") || low.starts_with("tax") || low.contains("ضريبة"))
            && !has_any(low, VAT_ID)
            && !has_any(low, &["tax invoice", "فاتورة ضريبية"]))
        || low.starts_with("discount")
        || low.starts_with("rounding")
}

// ---------------------------------------------------------------- evidence

fn ev(l: &DocLine, word_range: Option<(usize, usize)>) -> Evidence {
    let words = &l.line.words;
    let (a, b) = word_range.unwrap_or((0, words.len()));
    let b = b.min(words.len());
    let sel = &words[a.min(b)..b];
    let bbox = super::layout::union(sel.iter().map(|w| w.bbox)).and_then(|bx| norm_box(bx, l.page_w, l.page_h));
    let text = if word_range.is_some() { sel.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ") } else { l.line.text() };
    let conf = if sel.is_empty() { None } else { Some(sel.iter().map(|w| w.conf).sum::<i64>() / sel.len() as i64) };
    Evidence { page: Some(l.page), line: Some(l.index), bbox, text: Some(text.chars().take(300).collect()), ocr_conf: conf }
}

/// Word index of the first word containing `needle` (case-insensitive).
fn word_at(l: &DocLine, needle: &str) -> Option<usize> {
    let n = needle.to_lowercase();
    l.line.words.iter().position(|w| normalize_digits(&w.text).to_lowercase().contains(&n))
}

fn band_for(conf: Option<i64>, base: Band) -> Band {
    match conf {
        Some(c) if c < 40 => Band::Low.min(base),
        Some(c) if c < LOW_OCR_CONF => Band::Medium.min(base),
        _ => base,
    }
}

// ---------------------------------------------------------------- header

/// Value words after a label on the same line (skipping `:`, `#`, `no.`).
fn after_label<'a>(l: &'a DocLine, label: &str) -> Vec<(usize, &'a str)> {
    let low = normalize_digits(&l.line.text()).to_lowercase();
    let Some(pos) = low.find(label) else { return vec![] };
    // Map the character position to a word index.
    let mut acc = 0usize;
    let mut start_word = l.line.words.len();
    for (i, w) in l.line.words.iter().enumerate() {
        let wl = normalize_digits(&w.text).to_lowercase();
        let end = acc + wl.len();
        if end > pos + label.len() - 1 {
            start_word = i + 1;
            // The label may end inside this word ("no:12345"): keep the rest.
            if end > pos + label.len() && !wl.ends_with(':') {
                let cut = pos + label.len() - acc;
                if cut < wl.len() && cut > 0 {
                    start_word = i;
                }
            }
            break;
        }
        acc = end + 1;
    }
    l.line.words.iter().enumerate().skip(start_word).map(|(i, w)| (i, w.text.as_str())).collect()
}

fn clean_id_token(t: &str) -> String {
    t.trim_matches(|c: char| c == ':' || c == '#' || c == ',' || c == '.' || c == '(' || c == ')').to_string()
}

fn doc_number_after(l: &DocLine, labels: &[&str]) -> Option<(String, usize)> {
    let low = normalize_digits(&l.line.text()).to_lowercase();
    let label = labels.iter().filter(|x| low.contains(**x)).max_by_key(|x| x.len())?;
    for (i, w) in after_label(l, label) {
        let t = clean_id_token(&normalize_digits(w));
        let tl = t.to_lowercase();
        if t.is_empty() || matches!(tl.as_str(), "no" | "number" | "#" | "رقم" | ":") {
            continue;
        }
        if t.len() >= 2 && t.chars().any(|c| c.is_ascii_digit()) && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '/') {
            if !find_dates(&t).is_empty() && t.contains(['/', '.']) {
                continue;
            }
            return Some((t, i));
        }
        break;
    }
    None
}

/// 15-digit VAT account number (Bahrain VAT numbers are 15 digits); allows spaces.
fn vat_number_in(text: &str) -> Option<String> {
    let t = normalize_digits(text);
    let mut run = String::new();
    let mut best = None;
    for c in t.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            run.push(c);
        } else if c == ' ' && !run.is_empty() && run.len() < 15 {
            // spaced groups "200 000 000 000 003"
            continue;
        } else {
            if run.len() == 15 {
                best = Some(run.clone());
                break;
            }
            run.clear();
        }
    }
    best.or_else(|| {
        let d: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
        (d.len() == 15).then_some(d)
    })
}

fn cr_number_in(words: &[(usize, &str)]) -> Option<(String, usize)> {
    for (i, w) in words {
        let t = clean_id_token(&normalize_digits(w));
        if t.len() >= 3
            && t.len() <= 12
            && t.chars().all(|c| c.is_ascii_digit() || c == '-')
            && t.chars().filter(|c| c.is_ascii_digit()).count() >= 3
        {
            return Some((t, *i));
        }
    }
    None
}

fn phone_in(text: &str) -> Option<String> {
    let t = normalize_digits(text);
    let low = t.to_lowercase();
    let start = ["tel", "phone", "mob", "هاتف", "جوال", "ت:"].iter().filter_map(|l| low.find(l).map(|p| p + l.len())).min().unwrap_or(0);
    let rest = &t[start.min(t.len())..];
    let rest = rest.trim_start_matches(|c: char| !c.is_ascii_digit() && c != '+');
    let end = rest.find(|c: char| c.is_alphabetic()).unwrap_or(rest.len());
    let d: String = rest[..end].chars().filter(|c| c.is_ascii_digit()).collect();
    let d = d.strip_prefix("00973").or_else(|| d.strip_prefix("973")).unwrap_or(&d).to_string();
    (d.len() == 8 && matches!(d.chars().next(), Some('1' | '3' | '6' | '7'))).then_some(d)
}

fn last_amount(l: &DocLine, digits: u32) -> Option<(Num, usize)> {
    l.line
        .words
        .iter()
        .enumerate()
        .rev()
        .filter_map(|(i, w)| parse_num(&w.text, digits).map(|n| (n, i)))
        .find(|(n, _)| !n.percent && (n.decimals > 0 || n.minor > 0))
}

fn percent_in(l: &DocLine, digits: u32) -> Option<i64> {
    let text = normalize_digits(&l.line.text());
    for w in text.split_whitespace() {
        let w = w.trim_matches(|c: char| c == '(' || c == ')' || c == '@');
        if let Some(n) = parse_num(w, 2).filter(|n| n.percent) {
            // minor has 2 implied decimals → basis points
            return Some(n.minor).filter(|bp| (0..=10_000).contains(bp));
        }
    }
    let _ = digits;
    None
}

/// Supplier name: the first prominent line of page 1 that is not a label,
/// preferring lines with a company-form word.
fn supplier_name(lines: &[DocLine], own_names: &[String]) -> Option<(String, usize)> {
    const FORMS: &[&str] = &[
        "trading",
        "w.l.l",
        "wll",
        "co.",
        "company",
        "est.",
        "establishment",
        "llc",
        "b.s.c",
        "spc",
        "factory",
        "foods",
        "distribut",
        "ذ.م.م",
        "شركة",
        "مؤسسة",
        "للتجارة",
        "مصنع",
    ];
    let head: Vec<&DocLine> = lines.iter().filter(|l| l.page == 1).take(12).collect();
    let good = |l: &&DocLine| {
        let t = l.line.text();
        let low = t.to_lowercase();
        t.chars().filter(|c| c.is_alphabetic()).count() >= 4
            && !has_any(
                &low,
                &[
                    "invoice",
                    "فاتورة",
                    "credit note",
                    "delivery note",
                    "tel",
                    "phone",
                    "fax",
                    "p.o. box",
                    "email",
                    "www",
                    "date",
                    "vat",
                    "page",
                ],
            )
            && !own_names.iter().any(|n| !n.is_empty() && super::norm_name(&t) == *n)
    };
    head.iter()
        .find(|l| good(l) && has_any(&l.line.text().to_lowercase(), FORMS))
        .or_else(|| head.iter().find(|l| good(l)))
        .map(|l| (l.line.text().trim().chars().take(120).collect(), l.index))
}

// ---------------------------------------------------------------- classification

pub fn classify(lines: &[DocLine], digits: u32) -> Classification {
    let mut credit = 0;
    let mut invoice = 0;
    let mut delivery = 0;
    let mut reasons = vec![];
    for (n, l) in lines.iter().enumerate() {
        let low = normalize_digits(&l.line.text()).to_lowercase();
        let w = if n < 15 { 3 } else { 1 };
        if has_any(&low, &["credit note", "credit memo", "إشعار دائن", "اشعار دائن", "cn no", "credit invoice"]) {
            credit += w;
            reasons.push(format!("\"{}\" on line {}", l.line.text().chars().take(40).collect::<String>(), l.index + 1));
        }
        if has_any(
            &low,
            &["delivery note", "delivery order", "goods received note", "dn no", "إذن تسليم", "اذن تسليم", "مذكرة تسليم", "سند تسليم"],
        ) {
            delivery += w;
            reasons.push(format!("\"{}\" on line {}", l.line.text().chars().take(40).collect::<String>(), l.index + 1));
        }
        if has_any(&low, &["tax invoice", "invoice", "فاتورة ضريبية", "فاتورة"]) && !has_any(&low, &["credit invoice"]) {
            invoice += w;
        }
    }
    // A negative grand total supports a credit note.
    let neg_total = lines.iter().any(|l| {
        let low = l.line.text().to_lowercase();
        has_any(&low, GRAND) && last_amount(l, digits).is_some_and(|(n, _)| n.negative)
    });
    if neg_total {
        credit += 2;
        reasons.push("negative total".into());
    }
    let (doc_type, band) = if credit > 0 && credit >= invoice {
        (DocType::CreditNote, if credit >= 3 { Band::High } else { Band::Medium })
    } else if credit > 0 {
        // Credit words and invoice words both present: do not pick the
        // financially consequential reading on our own.
        reasons.push("both invoice and credit-note wording".into());
        (DocType::Unknown, Band::Unresolved)
    } else if delivery > 0 && delivery >= invoice {
        (DocType::DeliveryNote, if delivery >= 3 { Band::High } else { Band::Medium })
    } else if invoice > 0 {
        reasons.push("invoice wording".into());
        (DocType::Invoice, if invoice >= 3 { Band::High } else { Band::Medium })
    } else {
        reasons.push("no document-type wording found".into());
        (DocType::Unknown, Band::Unresolved)
    };
    Classification { doc_type, band, reasons, source: "rules".into() }
}

// ---------------------------------------------------------------- table columns

#[derive(Debug, Clone, Copy, PartialEq)]
enum Col {
    Desc,
    Code,
    Qty,
    Unit,
    Price,
    Disc,
    VatRate,
    VatAmt,
    Amount,
}

fn header_cols(l: &DocLine) -> Option<Vec<(Col, u32)>> {
    if l.line.words.iter().all(|w| w.bbox[2] == 0) {
        return None;
    }
    let words: Vec<(String, [u32; 4])> = l.line.words.iter().map(|w| (w.text.to_lowercase(), w.bbox)).collect();
    let mut cols: Vec<(Col, u32)> = vec![];
    let mut i = 0;
    while i < words.len() {
        let (w, b) = (&words[i].0, words[i].1);
        let next = words.get(i + 1).map(|x| x.0.as_str()).unwrap_or("");
        let centre = |b: [u32; 4]| b[0] + b[2] / 2;
        // Two header words form one column title only when they sit together.
        let close = words.get(i + 1).is_some_and(|(_, b2)| b2[0].saturating_sub(b[0] + b[2]) <= b[3].max(b2[3]) * 3 / 2);
        let pair = |c: Col, cols: &mut Vec<(Col, u32)>| {
            let b2 = words[i + 1].1;
            cols.push((c, (b[0] + b2[0] + b2[2]) / 2));
        };
        let t = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '%');
        let next = if close { next } else { "" };
        match (t, next.trim_matches(|c: char| !c.is_alphanumeric() && c != '%')) {
            ("unit", "price" | "cost" | "rate") | ("u.price", _) | ("سعر", "الوحدة") => {
                pair(Col::Price, &mut cols);
                i += 2;
                continue;
            }
            ("vat" | "tax", "%" | "rate") => {
                pair(Col::VatRate, &mut cols);
                i += 2;
                continue;
            }
            ("vat" | "tax", "amt" | "amount" | "value") => {
                pair(Col::VatAmt, &mut cols);
                i += 2;
                continue;
            }
            ("line" | "net" | "total", "total" | "amount" | "value") => {
                pair(Col::Amount, &mut cols);
                i += 2;
                continue;
            }
            ("item" | "product", "code" | "no" | "#") => {
                pair(Col::Code, &mut cols);
                i += 2;
                continue;
            }
            _ => {}
        }
        let c = match t {
            "description" | "item" | "items" | "product" | "particulars" | "details" | "الصنف" | "البيان" | "الوصف" | "المنتج" => {
                Some(Col::Desc)
            }
            "code" | "sku" | "barcode" | "ref" | "الرمز" | "رمز" | "الباركود" => Some(Col::Code),
            "qty" | "quantity" | "qnty" | "الكمية" | "كمية" => Some(Col::Qty),
            "unit" | "uom" | "الوحدة" | "وحدة" => Some(Col::Unit),
            "price" | "rate" | "cost" | "السعر" => Some(Col::Price),
            "disc" | "discount" | "الخصم" => Some(Col::Disc),
            "vat%" | "tax%" => Some(Col::VatRate),
            "vat" | "tax" | "الضريبة" => Some(Col::VatAmt),
            "amount" | "total" | "value" | "المبلغ" | "الإجمالي" | "القيمة" | "المجموع" => Some(Col::Amount),
            _ => None,
        };
        if let Some(c) = c {
            cols.push((c, centre(b)));
        }
        i += 1;
    }
    let numeric = cols.iter().filter(|(c, _)| matches!(c, Col::Qty | Col::Price | Col::Amount)).count();
    (cols.iter().any(|(c, _)| *c == Col::Desc) && numeric >= 2).then_some(cols)
}

fn nearest_col(cols: &[(Col, u32)], x: u32) -> Option<Col> {
    cols.iter().min_by_key(|(_, cx)| cx.abs_diff(x)).map(|(c, _)| *c)
}

// ---------------------------------------------------------------- lines

fn is_header_line(low: &str) -> bool {
    has_any(low, INVOICE_NO)
        || has_any(low, PO_NO)
        || has_any(low, VAT_ID)
        || has_any(low, DUE_DATE)
        || has_any(low, DELIVERY_DATE)
        || has_any(low, &["invoice date", "تاريخ", "date:", "page ", "tel", "phone", "fax", "p.o. box", "c.r", "cr no", "السجل"])
        || !find_dates(low).is_empty()
}

fn line_from_row(l: &DocLine, cols: Option<&[(Col, u32)]>, digits: u32) -> Option<ExtractedLine> {
    let text = normalize_digits(&l.line.text());
    if is_header_line(&text.to_lowercase()) {
        return None;
    }
    let words = &l.line.words;
    let mut barcode = None;
    let mut code = None;
    let mut unit = None;
    let mut desc_words: Vec<usize> = vec![];
    let mut qty: Option<(i64, usize)>;
    let mut decs: Vec<(Num, usize)> = vec![];
    let mut ints: Vec<(i64, usize)> = vec![];
    let mut pct: Option<i64> = None;
    let mut by_col: Vec<(Col, Num, usize)> = vec![];
    let one = 10i64.pow(digits);
    // "24 x 330 ml": the numbers of a pack expression belong to the description.
    let mut in_pack = vec![false; words.len()];
    for i in 1..words.len().saturating_sub(1) {
        let w = words[i].text.to_lowercase();
        let right = words[i + 1].text.to_lowercase();
        let right_is_size = right.starts_with(|c: char| c.is_ascii_digit())
            && (!right.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.').is_empty() && is_size_token(&right)
                || words
                    .get(i + 2)
                    .is_some_and(|nw| matches!(nw.text.to_lowercase().as_str(), "ml" | "l" | "ltr" | "g" | "gm" | "kg" | "cl")));
        if matches!(w.as_str(), "x" | "×") && words[i - 1].text.chars().all(|c| c.is_ascii_digit()) && right_is_size {
            in_pack[i - 1] = true;
            in_pack[i] = true;
            in_pack[i + 1] = true;
            if let Some(nw) = words.get(i + 2) {
                if matches!(nw.text.to_lowercase().as_str(), "ml" | "l" | "ltr" | "g" | "gm" | "kg" | "cl") {
                    in_pack[i + 2] = true;
                }
            }
        }
    }
    let mut qty_before_unit: Option<(i64, usize)> = None;
    for (i, w) in words.iter().enumerate() {
        let t = clean_id_token(&normalize_digits(&w.text));
        let col = cols.and_then(|c| nearest_col(c, w.bbox[0] + w.bbox[2] / 2));
        if in_pack[i] && col.is_none_or(|c| c == Col::Desc) {
            desc_words.push(i);
            continue;
        }
        if is_barcode(&t) && barcode.is_none() {
            barcode = Some((t, i));
            continue;
        }
        if let Some(u) = unit_word(&t) {
            // "10 PCS", "2 CTN": a unit right after a quantity, or in the unit column.
            let after_qty = i > 0 && parse_num(&words[i - 1].text, digits).is_some_and(|n| n.decimals == 0 && !n.percent);
            if unit.is_none() && (after_qty || col == Some(Col::Unit)) {
                unit = Some(u.to_string());
                if after_qty {
                    qty_before_unit = parse_num(&words[i - 1].text, digits).and_then(|n| n.int(digits)).map(|q| (q * 1000, i - 1));
                }
                continue;
            }
        }
        if let Some(n) = parse_num(&w.text, digits) {
            if n.percent {
                pct = Some(n.minor / 10i64.pow(digits.saturating_sub(2)));
                if let Some(bp) = parse_num(&w.text, 2).filter(|x| x.percent).map(|x| x.minor) {
                    pct = Some(bp);
                }
                continue;
            }
            // A size inside the description ("1.5 L", "330 ml") is not a number column.
            let next_is_size_unit =
                words.get(i + 1).is_some_and(|nw| matches!(nw.text.to_lowercase().as_str(), "ml" | "l" | "ltr" | "g" | "gm" | "kg" | "cl"));
            if (next_is_size_unit && cols.is_none()) || col == Some(Col::Desc) {
                desc_words.push(i);
                continue;
            }
            if let (Some(_), Some(c)) = (cols, col) {
                if c != Col::Code {
                    by_col.push((c, n, i));
                    continue;
                }
            }
            if n.decimals > 0 {
                decs.push((n, i));
            } else if let Some(v) = n.int(digits) {
                ints.push((v, i));
            }
            continue;
        }
        if matches!(t.as_str(), "x" | "X" | "×" | "@" | "-" | "|") {
            continue;
        }
        if is_code(&t) && code.is_none() && (desc_words.is_empty() || col == Some(Col::Code)) {
            code = Some((t, i));
            continue;
        }
        desc_words.push(i);
    }
    let description: String = desc_words.iter().map(|i| words[*i].text.as_str()).collect::<Vec<_>>().join(" ");
    let description = description.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    if description.chars().filter(|c| c.is_alphabetic()).count() < 2 {
        return None;
    }
    let mut line = ExtractedLine {
        raw_text: text.chars().take(300).collect(),
        description: description.chars().take(200).collect(),
        barcode: barcode.as_ref().map(|b| b.0.clone()),
        barcode_valid: barcode.as_ref().map(|b| super::gtin_valid(&b.0)),
        supplier_code: code.map(|c| c.0),
        unit,
        evidence: ev(l, None),
        ..Default::default()
    };
    if !by_col.is_empty() {
        for (c, n, _) in &by_col {
            match c {
                Col::Qty => {
                    line.qty_milli =
                        Some(if n.decimals == 0 { n.minor / one * 1000 } else { parse_decimal(&n.text.replace(',', ""), 3).unwrap_or(0) })
                }
                Col::Price => line.unit_cost_minor = Some(n.minor),
                Col::Disc => line.discount_minor = Some(n.minor),
                Col::VatRate => line.vat_rate_bp = Some(n.minor / 10i64.pow(digits.saturating_sub(2))),
                Col::VatAmt => {
                    if n.decimals == 0 && n.minor / one <= 100 && line.vat_rate_bp.is_none() {
                        line.vat_rate_bp = Some(n.minor / one * 100);
                    } else {
                        line.vat_minor = Some(n.minor);
                    }
                }
                Col::Amount => line.line_total_minor = Some(n.minor),
                Col::Unit | Col::Desc | Col::Code => {}
            }
        }
        if pct.is_some() && line.vat_rate_bp.is_none() {
            line.vat_rate_bp = pct;
        }
    } else {
        // Heuristic columns: ints are quantities, decimals are prices; the
        // last decimal is the line total, the first the unit cost.
        qty = qty_before_unit.or_else(|| ints.iter().find(|(v, _)| *v > 0 && *v <= 100_000).map(|(v, i)| (*v * 1000, *i)));
        if let Some(bp) = pct {
            line.vat_rate_bp = Some(bp);
        }
        match decs.len() {
            0 => {}
            1 => line.line_total_minor = Some(decs[0].0.minor),
            2 => {
                line.unit_cost_minor = Some(decs[0].0.minor);
                line.line_total_minor = Some(decs[1].0.minor);
            }
            _ => {
                let unit_c = decs[0].0.minor;
                let total = decs[decs.len() - 1].0.minor;
                let mid = decs[decs.len() - 2].0.minor;
                line.unit_cost_minor = Some(unit_c);
                line.line_total_minor = Some(total);
                if let Some((q, _)) = qty {
                    let ext = crate::money::extend(unit_c, q).unwrap_or(0);
                    if ext - mid == total {
                        line.discount_minor = Some(mid);
                    } else if ext + mid == total || line.vat_rate_bp.is_some() {
                        line.vat_minor = Some(mid);
                    }
                } else {
                    line.vat_minor = Some(mid);
                }
            }
        }
        // A decimal quantity (2.5 kg) is printed with decimals before the price.
        if qty.is_none() && decs.len() >= 3 && matches!(line.unit.as_deref(), Some("kg")) {
            qty = parse_decimal(&decs[0].0.text, 3).ok().map(|q| (q, decs[0].1));
            line.unit_cost_minor = Some(decs[1].0.minor);
        }
        // The printed arithmetic decides between candidate quantities: in
        // "Tea 100 bags 5 0.850 4.250" the quantity is 5, not the 100 in the name.
        if let (Some(u), Some(t)) = (line.unit_cost_minor, line.line_total_minor) {
            let fits = |q: i64| crate::money::extend(u, q).ok() == Some(t - line.vat_minor.unwrap_or(0) + line.discount_minor.unwrap_or(0))
                || crate::money::extend(u, q).ok() == Some(t);
            if !qty.is_some_and(|(q, _)| fits(q)) {
                if let Some((v, i)) = ints.iter().rev().find(|(v, _)| *v > 0 && fits(*v * 1000)) {
                    qty = Some((*v * 1000, *i));
                    line.flags.push("qty_from_arithmetic".into());
                }
            }
        }
        line.qty_milli = qty.map(|q| q.0);
    }
    // Missing quantity or unit cost: derive only when the division is exact.
    match (line.qty_milli, line.unit_cost_minor, line.line_total_minor) {
        (None, Some(u), Some(t)) if u > 0 => {
            let base = t - line.vat_minor.filter(|_| false).unwrap_or(0) + line.discount_minor.unwrap_or(0);
            if base % u == 0 {
                line.qty_milli = Some(base / u * 1000);
                line.flags.push("qty_derived".into());
            }
        }
        (Some(q), None, Some(t)) if q > 0 => {
            let base = (t + line.discount_minor.unwrap_or(0)) as i128 * 1000;
            if base % q as i128 == 0 {
                line.unit_cost_minor = Some((base / q as i128) as i64);
                line.flags.push("unit_cost_derived".into());
            }
        }
        _ => {}
    }
    // Packaging.
    let (upc, pack_text) = parse_pack(&line.description);
    line.pack.text = pack_text;
    line.pack.units_per_case = upc;
    match (line.unit.as_deref(), upc, line.qty_milli) {
        (Some("ctn" | "box"), Some(n), Some(q)) => {
            line.pack.case_qty_milli = Some(q);
            line.pack.base_qty_milli = Some(q * n);
            line.pack.clear = true;
        }
        (Some("ctn" | "box"), None, Some(q)) => {
            line.pack.case_qty_milli = Some(q);
            line.flags.push("pack_size_unknown".into());
        }
        (Some("pcs" | "kg"), _, Some(q)) => {
            line.pack.base_qty_milli = Some(q);
            line.pack.clear = true;
        }
        (None, Some(_), Some(_)) => line.flags.push("pack_unclear".into()),
        (None, None, Some(q)) => {
            line.pack.base_qty_milli = Some(q);
            line.pack.clear = true;
        }
        _ => {}
    }
    if l.line.conf() < LOW_OCR_CONF && l.line.words.iter().any(|w| w.bbox[2] > 0) {
        line.flags.push("low_ocr_confidence".into());
    }
    if l.line.min_conf() < 30 && l.line.words.iter().any(|w| w.bbox[2] > 0) {
        line.flags.push("unclear_text".into());
    }
    if line.barcode_valid == Some(false) {
        line.flags.push("barcode_check_digit".into());
    }
    if line.qty_milli.is_none() && line.line_total_minor.is_none() {
        return None;
    }
    // Without table columns an item needs a price, or a quantity with a printed unit.
    if cols.is_none() && line.line_total_minor.is_none() && line.unit_cost_minor.is_none() && line.unit.is_none() {
        return None;
    }
    Some(line)
}

// ---------------------------------------------------------------- entry

/// Read a document. `own` = this business's VAT number and names (to tell
/// the buyer apart from the supplier).
pub fn extract(layout: &Layout, digits: u32, own_vat: Option<&str>, own_names: &[String]) -> Extraction {
    let lines = layout.lines();
    let mut f = DocFields::default();
    let mut warnings = vec![];
    let classification = classify(&lines, digits);
    let own_vat = own_vat.map(super::digits_only).filter(|v| !v.is_empty());
    let own_names: Vec<String> = own_names.iter().map(|n| super::norm_name(n)).collect();
    let mut in_buyer_block = false;
    let mut table: Option<Vec<(Col, u32)>> = None;
    let mut items: Vec<ExtractedLine> = vec![];
    let mut all_dates: Vec<(DateRead, usize)> = vec![];
    let mut seen_total_block = false;
    let mut currency = "BHD".to_string();
    for l in &lines {
        let text = normalize_digits(&l.line.text());
        let low = text.to_lowercase();
        if has_any(&low, BUYER) {
            in_buyer_block = true;
        } else if has_any(&low, &["supplier", "vendor", "from:", "المورد"]) {
            in_buyer_block = false;
        }
        if has_any(&low, &[" usd", "usd ", "us$", " aed", "sar ", " eur"]) && !has_any(&low, &["bhd", "bd"]) {
            currency = if low.contains("usd") || low.contains("us$") {
                "USD"
            } else if low.contains("aed") {
                "AED"
            } else if low.contains("sar") {
                "SAR"
            } else {
                "EUR"
            }
            .into();
        }
        // ---- identifiers
        if has_any(&low, VAT_ID) {
            if let Some(v) = vat_number_in(&text) {
                let is_own = own_vat.as_deref() == Some(v.as_str());
                let target = if is_own || in_buyer_block { &mut f.buyer_vat } else { &mut f.supplier_vat };
                if !target.is_set() {
                    let idx = word_at(l, &v[..4]);
                    *target = Field::found(v.clone(), v, ev(l, idx.map(|i| (i, i + 1))), band_for(Some(l.line.conf()), Band::High));
                }
            }
        }
        if !f.supplier_cr.is_set() && !in_buyer_block && has_any(&format!("{low} "), CR_ID) {
            let label = CR_ID.iter().filter(|x| format!("{low} ").contains(**x)).max_by_key(|x| x.len()).copied().unwrap_or("cr");
            if let Some((cr, i)) = cr_number_in(&after_label(l, label.trim())) {
                f.supplier_cr = Field::found(cr.clone(), cr, ev(l, Some((i, i + 1))), band_for(Some(l.line.conf()), Band::High));
            }
        }
        if !f.supplier_phone.is_set() && !in_buyer_block && has_any(&low, &["tel", "phone", "mob", "هاتف", "جوال", "ت:"]) {
            if let Some(p) = phone_in(&text) {
                f.supplier_phone = Field::found(p.clone(), p, ev(l, None), Band::Medium);
            }
        }
        if !f.iban.is_set() && low.contains("iban") {
            if let Some(w) = text
                .split_whitespace()
                .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
                .find(|w| w.len() >= 15 && w.to_uppercase().starts_with("BH"))
            {
                f.iban = Field::found(w.to_uppercase(), w, ev(l, None), Band::Medium);
            }
        }
        // ---- numbers
        if !f.invoice_number.is_set() {
            if let Some((n, i)) = doc_number_after(l, INVOICE_NO) {
                f.invoice_number =
                    Field::found(n.clone(), n, ev(l, Some((i, i + 1))), band_for(l.line.words.get(i).map(|w| w.conf), Band::High));
            }
        }
        // "INVOICE 4471" / "TAX INVOICE #88" / "فاتورة 12": a number right after the title.
        if !f.invoice_number.is_set() && !has_any(&low, PO_NO) {
            if let Some(k) = l.line.words.iter().position(|w| {
                let t = w.text.to_lowercase();
                matches!(t.trim_matches(|c: char| !c.is_alphanumeric()), "invoice" | "inv" | "فاتورة" | "note")
            }) {
                if let Some(w) = l.line.words.get(k + 1) {
                    let t = clean_id_token(&normalize_digits(&w.text));
                    if t.len() >= 2
                        && t.chars().any(|c| c.is_ascii_digit())
                        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '/')
                        && find_dates(&t).is_empty()
                    {
                        f.invoice_number = Field::found(t.clone(), t, ev(l, Some((k + 1, k + 2))), Band::Medium);
                    }
                }
            }
        }
        if !f.po_number.is_set() && !has_any(&low, INVOICE_NO) {
            if let Some((n, i)) = doc_number_after(l, PO_NO) {
                f.po_number = Field::found(n.clone(), n, ev(l, Some((i, i + 1))), Band::Medium);
            }
        }
        // ---- dates
        let dates = find_dates(&text);
        for d in &dates {
            all_dates.push((d.clone(), l.index));
        }
        if let Some(d) = dates.first() {
            let target = if has_any(&low, DUE_DATE) {
                Some(&mut f.due_date)
            } else if has_any(&low, DELIVERY_DATE) {
                Some(&mut f.delivery_date)
            } else if has_any(&low, INVOICE_DATE) && !has_any(&low, &["print", "expiry", "exp", "mfg", "production"]) {
                Some(&mut f.invoice_date)
            } else {
                None
            };
            if let Some(t) = target {
                if !t.is_set() {
                    let idx = word_at(l, d.text.split_whitespace().next().unwrap_or(&d.text));
                    let mut fld = Field::found(d.date.clone(), d.text.clone(), ev(l, idx.map(|i| (i, i + 1))), Band::High);
                    if let Some(alt) = &d.ambiguous {
                        fld.status = "ambiguous".into();
                        fld.band = Band::Medium;
                        fld.note = Some(format!("Could also be {alt}; read day-first (Bahrain) — check the date."));
                    }
                    *t = fld;
                }
            }
        }
        // ---- table header / items / totals
        if let Some(cols) = header_cols(l) {
            table = Some(cols);
            continue;
        }
        let summary = is_summary_line(&low);
        if summary {
            seen_total_block = true;
            let amount = last_amount(l, digits);
            let set = |fld: &mut Field<i64>, amt: &(Num, usize)| {
                if !fld.is_set() {
                    let (n, i) = amt;
                    let mut x = Field::found(
                        n.minor,
                        n.text.clone(),
                        ev(l, Some((*i, *i + 1))),
                        band_for(l.line.words.get(*i).map(|w| w.conf), Band::High),
                    );
                    if n.negative {
                        x.note = Some("Printed as a negative amount.".into());
                    }
                    *fld = x;
                }
            };
            if let Some(a) = &amount {
                if has_any(&low, SUBTOTAL) {
                    set(&mut f.subtotal_minor, a);
                } else if has_any(&low, ZERO_RATED) {
                    set(&mut f.zero_rated_minor, a);
                } else if has_any(&low, EXEMPT) {
                    set(&mut f.exempt_minor, a);
                } else if has_any(&low, GRAND) {
                    set(&mut f.total_minor, a);
                } else if has_any(&low, DISCOUNT) {
                    set(&mut f.discount_minor, a);
                } else if has_any(&low, VAT_AMT) && !has_any(&low, VAT_ID) {
                    set(&mut f.vat_minor, a);
                    if let Some(bp) = percent_in(l, digits) {
                        if !f.vat_rate_bp.is_set() {
                            f.vat_rate_bp = Field::found(bp, format!("{}%", crate::money::format_decimal(bp, 2)), ev(l, None), Band::High);
                        }
                    }
                } else if low.contains("total") || has_any(&low, &["الإجمالي", "المجموع"]) {
                    // A plain "Total": the grand total unless a later, more specific line says so.
                    if !f.total_minor.is_set() {
                        set(&mut f.total_minor, a);
                        f.total_minor.band = f.total_minor.band.min(Band::Medium);
                    }
                }
            }
            continue;
        }
        if seen_total_block && table.is_none() {
            continue;
        }
        if let Some(item) = line_from_row(l, table.as_deref(), digits) {
            items.push(item);
        }
    }
    // Supplier name.
    if let Some((name, idx)) = supplier_name(&lines, &own_names) {
        let l = &lines[idx];
        f.supplier_name = Field::found(name.clone(), name, ev(l, None), Band::Medium);
    }
    // Date with no label: the first date on page 1.
    if !f.invoice_date.is_set() {
        if let Some((d, idx)) = all_dates.iter().find(|(_, i)| lines[*i].page == 1) {
            let mut fld = Field::found(d.date.clone(), d.text.clone(), ev(&lines[*idx], None), Band::Low);
            fld.note = Some("No date label was found; this is the first date on the page.".into());
            if let Some(alt) = &d.ambiguous {
                fld.status = "ambiguous".into();
                fld.note = Some(format!("No label, and it could also be {alt}. Check the date."));
            }
            f.invoice_date = fld;
        }
    }
    // Resolve day/month ambiguity from the document's other dates: a date
    // printed with day > 12 proves the day-first order.
    let proven_dm = all_dates.iter().any(|(d, _)| {
        d.ambiguous.is_none()
            && d.text.contains(['/', '.'])
            && d.text.split(['/', '-', '.']).next().and_then(|x| x.parse::<u32>().ok()).is_some_and(|x| x > 12 && x <= 31)
    });
    let proven_md = all_dates.iter().any(|(d, _)| {
        d.ambiguous.is_none() && d.text.split(['/', '-', '.']).nth(1).and_then(|x| x.parse::<u32>().ok()).is_some_and(|x| x > 12 && x <= 31)
    });
    for fld in [&mut f.invoice_date, &mut f.due_date, &mut f.delivery_date] {
        if fld.status == "ambiguous" {
            if proven_dm && !proven_md {
                fld.status = "ok".into();
                fld.band = Band::High;
                fld.note = Some("Day-first order confirmed by another date on the document.".into());
            } else if proven_md && !proven_dm {
                let alt = fld
                    .note
                    .as_deref()
                    .and_then(|n| n.split_whitespace().find(|w| w.len() == 10 && w.contains('-')))
                    .map(|s| s.trim_end_matches(';').to_string());
                if let Some(a) = alt {
                    fld.value = Some(a);
                    fld.status = "ok".into();
                    fld.band = Band::Medium;
                    fld.note = Some("Month-first order used by another date on the document.".into());
                }
            }
        }
    }
    if f.due_date.is_set() && f.invoice_date.is_set() && f.due_date.value < f.invoice_date.value {
        warnings.push("The due date is before the invoice date.".into());
    }
    if items.is_empty() {
        warnings.push("No item lines were found.".into());
    }
    if currency != "BHD" {
        warnings.push(format!("Amounts appear to be in {currency}, not BHD."));
    }
    Extraction { classification, fields: f, lines: items, currency, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(text: &str) -> Extraction {
        extract(&Layout::from_text(text, 90), 3, Some("200000000000003"), &["Al Noor Supermarket".into()])
    }

    #[test]
    fn money_tokens() {
        assert_eq!(parse_num("1,234.500", 3).unwrap().minor, 1_234_500);
        assert_eq!(parse_num("12.5", 3).unwrap().minor, 12_500);
        assert_eq!(parse_num("0.100", 3).unwrap().minor, 100);
        assert_eq!(parse_num("BD12.345", 3).unwrap().minor, 12_345);
        let n = parse_num("(1.250)", 3).unwrap();
        assert!(n.negative && n.minor == 1250);
        assert_eq!(parse_num("١٢٫٣٤٥", 3).unwrap().minor, 12_345);
        assert!(parse_num("1.2345", 3).is_none(), "4 decimals is not BHD money");
        assert!(parse_num("12,34.5", 3).is_none());
        assert!(parse_num("10%", 3).unwrap().percent);
    }

    #[test]
    fn packs() {
        assert_eq!(parse_pack("COKE REG 24 x 330ML").0, Some(24));
        assert_eq!(parse_pack("Water 6x1.5L").0, Some(6));
        assert_eq!(parse_pack("Tissue 1 CTN = 48").0, Some(48));
        assert_eq!(parse_pack("Chips pack 6").0, Some(6));
        assert_eq!(parse_pack("Eggs 30's").0, Some(30));
        assert_eq!(parse_pack("Juice 12 pcs").0, Some(12));
        assert_eq!(parse_pack("عصير 24 حبة").0, Some(24));
        assert_eq!(parse_pack("Milk 1L").0, None);
    }

    #[test]
    fn dates() {
        let d = find_dates("Date: 28/09/2026");
        assert_eq!(d[0].date, "2026-09-28");
        assert!(d[0].ambiguous.is_none());
        let d = find_dates("03/04/2026");
        assert_eq!((d[0].date.as_str(), d[0].ambiguous.as_deref()), ("2026-04-03", Some("2026-03-04")));
        assert_eq!(find_dates("28 Sep 2026")[0].date, "2026-09-28");
        assert_eq!(find_dates("Sep 28, 2026")[0].date, "2026-09-28");
        assert_eq!(find_dates("2026-09-28")[0].date, "2026-09-28");
        assert_eq!(find_dates("28-Sep-26")[0].date, "2026-09-28");
        assert_eq!(find_dates("٢٨/٠٩/٢٠٢٦")[0].date, "2026-09-28");
    }

    #[test]
    fn english_invoice_header_lines_totals() {
        let e = ex("Al Waha Trading Co. W.L.L.\nTel: 1771 2345  CR No: 45678-2\nVAT No: 200011122233344\nTAX INVOICE\n\
                    Invoice No: INV-00123   Date: 28/09/2026\nDue Date: 28/10/2026\nBill To: Al Noor Supermarket VAT No: 200000000000003\n\
                    6291041500213 Milk Full Cream 1L 10 PCS 0.450 4.500\nCC330 Coca Cola 24 x 330ml 2 CTN 5.400 10.800\n\
                    Subtotal 15.300\nVAT 10% 1.530\nGrand Total 16.830");
        assert_eq!(e.classification.doc_type, DocType::Invoice);
        assert_eq!(e.fields.invoice_number.value.as_deref(), Some("INV-00123"));
        assert_eq!(e.fields.invoice_date.value.as_deref(), Some("2026-09-28"));
        assert_eq!(e.fields.due_date.value.as_deref(), Some("2026-10-28"));
        assert_eq!(e.fields.supplier_vat.value.as_deref(), Some("200011122233344"));
        assert_eq!(e.fields.buyer_vat.value.as_deref(), Some("200000000000003"), "own VAT is the buyer's");
        assert_eq!(e.fields.supplier_cr.value.as_deref(), Some("45678-2"));
        assert_eq!(e.fields.supplier_phone.value.as_deref(), Some("17712345"));
        assert_eq!(e.fields.supplier_name.value.as_deref(), Some("Al Waha Trading Co. W.L.L."));
        assert_eq!(e.fields.subtotal_minor.value, Some(15_300));
        assert_eq!(e.fields.vat_minor.value, Some(1_530));
        assert_eq!(e.fields.vat_rate_bp.value, Some(1000));
        assert_eq!(e.fields.total_minor.value, Some(16_830));
        assert_eq!(e.lines.len(), 2);
        let milk = &e.lines[0];
        assert_eq!((milk.barcode.as_deref(), milk.barcode_valid), (Some("6291041500213"), Some(true)));
        assert_eq!((milk.qty_milli, milk.unit_cost_minor, milk.line_total_minor), (Some(10_000), Some(450), Some(4_500)));
        let coke = &e.lines[1];
        assert_eq!(coke.supplier_code.as_deref(), Some("CC330"));
        assert_eq!(coke.unit.as_deref(), Some("ctn"));
        assert_eq!((coke.pack.units_per_case, coke.pack.case_qty_milli, coke.pack.base_qty_milli), (Some(24), Some(2_000), Some(48_000)));
        assert!(coke.pack.clear);
    }

    #[test]
    fn ambiguous_date_is_flagged_not_silently_chosen() {
        let e = ex("Invoice No: 55\nDate: 03/04/2026\nBread 2 0.200 0.400\nTotal 0.400");
        assert_eq!(e.fields.invoice_date.status, "ambiguous");
        assert_eq!(e.fields.invoice_date.value.as_deref(), Some("2026-04-03"));
        // Another date with day > 12 proves day-first.
        let e = ex("Invoice No: 55\nDate: 03/04/2026\nDue Date: 25/04/2026\nBread 2 0.200 0.400\nTotal 0.400");
        assert_eq!(e.fields.invoice_date.status, "ok");
    }

    #[test]
    fn credit_note_and_conflicts() {
        let e = ex("CREDIT NOTE\nCredit Note No: CN-77\nDate 01/09/2026\nReturned Milk 2 0.450 0.900\nTotal (0.900)");
        assert_eq!(e.classification.doc_type, DocType::CreditNote);
        assert_eq!(e.fields.invoice_number.value.as_deref(), Some("CN-77"));
        assert_eq!(e.fields.total_minor.value, Some(900));
        let e = ex("DELIVERY NOTE\nDN No: 991\nMilk 10 PCS");
        assert_eq!(e.classification.doc_type, DocType::DeliveryNote);
        let e = ex("Some shop\nMilk 2 0.450 0.900");
        assert_eq!(e.classification.doc_type, DocType::Unknown);
        assert_eq!(e.classification.band, Band::Unresolved);
    }

    #[test]
    fn arabic_invoice() {
        let e = ex("شركة الواحة للتجارة ذ.م.م\nالرقم الضريبي: 200011122233344\nفاتورة ضريبية\nرقم الفاتورة: ٤٥٦\nالتاريخ: ٢٨/٠٩/٢٠٢٦\n\
                    حليب كامل الدسم ١٠ ٠٫٤٥٠ ٤٫٥٠٠\nالمجموع الفرعي ٤٫٥٠٠\nضريبة القيمة المضافة ١٠٪ ٠٫٤٥٠\nالإجمالي شامل الضريبة ٤٫٩٥٠");
        assert_eq!(e.classification.doc_type, DocType::Invoice);
        assert_eq!(e.fields.invoice_number.value.as_deref(), Some("456"));
        assert_eq!(e.fields.invoice_date.value.as_deref(), Some("2026-09-28"));
        assert_eq!(e.fields.supplier_vat.value.as_deref(), Some("200011122233344"));
        assert_eq!(e.fields.subtotal_minor.value, Some(4_500));
        assert_eq!(e.fields.vat_minor.value, Some(450));
        assert_eq!(e.fields.total_minor.value, Some(4_950));
        assert_eq!(e.lines.len(), 1);
        assert_eq!((e.lines[0].qty_milli, e.lines[0].unit_cost_minor), (Some(10_000), Some(450)));
        assert!(e.fields.supplier_name.value.as_deref().unwrap_or("").contains("الواحة"));
    }

    #[test]
    fn pack_ambiguity_is_flagged() {
        let e = ex("Invoice No: 1\nPepsi 24x250ml 3 1.800 5.400\nTotal 5.400");
        let l = &e.lines[0];
        assert_eq!(l.pack.units_per_case, Some(24));
        assert!(l.pack.base_qty_milli.is_none(), "not multiplied without a printed carton unit");
        assert!(l.flags.contains(&"pack_unclear".to_string()));
    }

    #[test]
    fn column_layout_uses_positions() {
        use crate::docintel::layout::{Line, Page, Word};
        let w = |t: &str, x: u32, y: u32| Word { text: t.into(), conf: 92, bbox: [x, y, 40, 12] };
        let lines = vec![
            Line { block: 1, words: vec![w("Invoice", 10, 10), w("No:", 60, 10), w("A-9", 100, 10)] },
            Line {
                block: 2,
                words: vec![
                    w("Description", 10, 50),
                    w("Qty", 300, 50),
                    w("Unit", 380, 50),
                    w("Price", 420, 50),
                    w("VAT", 500, 50),
                    w("Amount", 580, 50),
                ],
            },
            // Qty column holds 12 even though the description contains numbers.
            Line {
                block: 2,
                words: vec![
                    w("Water", 10, 70),
                    w("500", 60, 70),
                    w("ml", 100, 70),
                    w("12", 300, 70),
                    w("0.100", 420, 70),
                    w("0.120", 500, 70),
                    w("1.200", 580, 70),
                ],
            },
            Line { block: 3, words: vec![w("Total", 420, 100), w("1.320", 580, 100)] },
        ];
        let layout = Layout {
            pages: vec![Page { number: 1, width: 700, height: 900, source: "ocr".into(), rotation: 0, variant: "original".into(), lines }],
        };
        let e = extract(&layout, 3, None, &[]);
        assert_eq!(e.lines.len(), 1);
        let l = &e.lines[0];
        assert_eq!(l.description, "Water 500 ml");
        assert_eq!((l.qty_milli, l.unit_cost_minor, l.vat_minor, l.line_total_minor), (Some(12_000), Some(100), Some(120), Some(1_200)));
        assert!(l.evidence.bbox.is_some());
        assert_eq!(e.fields.invoice_number.evidence.as_ref().and_then(|x| x.bbox).map(|b| b[0] > 0.1), Some(true));
    }
}
