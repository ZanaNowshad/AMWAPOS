//! Document Intelligence for supplier documents.
//!
//! unstructured document → quality check → OCR (with layout) → classification
//! → structured extraction → supplier / product matching → deterministic
//! validation (arithmetic, VAT) → duplicate, anomaly and PO / receiving
//! reconciliation → review → drafts.
//!
//! It extends the invoice-scan pipeline in `ocrflow` (same table, same OCR
//! worker, same permissions). Every extracted value carries its provenance
//! (page, line, box, OCR confidence, who produced it) and an application
//! confidence band. Values from OCR, rules or an AI model are proposals: the
//! database checks every id, money is exact integer fils, arithmetic is
//! recomputed here, and nothing posts stock, payables or products — a person
//! creates drafts and confirms them in the normal workflows.

pub mod aischema;
pub mod checks;
pub mod extract;
pub mod layout;
pub mod matching;
pub mod pdfdoc;
pub mod quality;
pub mod service;

use serde::{Deserialize, Serialize};

/// Application-defined confidence band. Not a calibrated probability: OCR
/// and model confidences are only signals that feed these bands.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Band {
    Unresolved,
    Low,
    Medium,
    High,
}

impl Band {
    pub fn from_score(score: i64) -> Band {
        match score {
            s if s >= 90 => Band::High,
            s if s >= 70 => Band::Medium,
            s if s >= 40 => Band::Low,
            _ => Band::Unresolved,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Band::High => "high",
            Band::Medium => "medium",
            Band::Low => "low",
            Band::Unresolved => "unresolved",
        }
    }

    pub fn parse(s: &str) -> Band {
        match s {
            "high" => Band::High,
            "medium" => Band::Medium,
            "low" => Band::Low,
            _ => Band::Unresolved,
        }
    }
}

/// Where a value came from on the document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Evidence {
    pub page: Option<u32>,
    /// Index of the line in reading order across the document.
    pub line: Option<usize>,
    /// Box on the page, 0..1 (left, top, width, height).
    pub bbox: Option<[f32; 4]>,
    /// The printed text the value was read from.
    pub text: Option<String>,
    /// Mean OCR confidence of that text (0..100).
    pub ocr_conf: Option<i64>,
}

/// One extracted header field with provenance and review status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Field<T> {
    /// Normalized value (money in fils, dates as YYYY-MM-DD).
    pub value: Option<T>,
    /// The value as printed.
    pub raw: Option<String>,
    pub evidence: Option<Evidence>,
    /// rules | ai | person
    pub source: String,
    pub band: Band,
    /// ok | ambiguous | missing | invalid | corrected
    pub status: String,
    pub note: Option<String>,
}

impl<T> Default for Field<T> {
    fn default() -> Self {
        Field {
            value: None,
            raw: None,
            evidence: None,
            source: "rules".into(),
            band: Band::Unresolved,
            status: "missing".into(),
            note: None,
        }
    }
}

impl<T> Field<T> {
    pub fn found(value: T, raw: impl Into<String>, evidence: Evidence, band: Band) -> Self {
        Field {
            value: Some(value),
            raw: Some(raw.into()),
            evidence: Some(evidence),
            source: "rules".into(),
            band,
            status: "ok".into(),
            note: None,
        }
    }

    pub fn is_set(&self) -> bool {
        self.value.is_some()
    }

    pub fn person(value: Option<T>) -> Self {
        let status = if value.is_some() { "corrected" } else { "missing" };
        Field { value, raw: None, evidence: None, source: "person".into(), band: Band::High, status: status.into(), note: None }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DocType {
    Invoice,
    CreditNote,
    DeliveryNote,
    Unknown,
}

impl DocType {
    pub fn as_str(self) -> &'static str {
        match self {
            DocType::Invoice => "invoice",
            DocType::CreditNote => "credit_note",
            DocType::DeliveryNote => "delivery_note",
            DocType::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<DocType> {
        Some(match s {
            "invoice" => DocType::Invoice,
            "credit_note" => DocType::CreditNote,
            "delivery_note" => DocType::DeliveryNote,
            "unknown" => DocType::Unknown,
            _ => return None,
        })
    }
}

/// Document classification with its reasons.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Classification {
    pub doc_type: DocType,
    pub band: Band,
    pub reasons: Vec<String>,
    pub source: String,
}

/// Header fields of a supplier document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct DocFields {
    pub supplier_name: Field<String>,
    pub supplier_vat: Field<String>,
    pub supplier_cr: Field<String>,
    pub supplier_phone: Field<String>,
    pub buyer_vat: Field<String>,
    pub invoice_number: Field<String>,
    pub invoice_date: Field<String>,
    pub due_date: Field<String>,
    pub delivery_date: Field<String>,
    pub po_number: Field<String>,
    pub subtotal_minor: Field<i64>,
    pub discount_minor: Field<i64>,
    pub vat_minor: Field<i64>,
    /// Document VAT rate in basis points (1000 = 10%) when printed.
    pub vat_rate_bp: Field<i64>,
    pub zero_rated_minor: Field<i64>,
    pub exempt_minor: Field<i64>,
    pub total_minor: Field<i64>,
    pub iban: Field<String>,
}

/// Packaging read from a line ("24 x 330 ml", "1 CTN = 48", "pack 6").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Pack {
    /// Printed case/carton count (milli) when the line is sold in cases.
    pub case_qty_milli: Option<i64>,
    pub units_per_case: Option<i64>,
    /// Quantity in single units: only when the relationship is clear.
    pub base_qty_milli: Option<i64>,
    pub text: Option<String>,
    /// The case → unit relationship is printed unambiguously.
    pub clear: bool,
}

/// One item line as read from the document (before matching).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ExtractedLine {
    pub raw_text: String,
    pub description: String,
    pub barcode: Option<String>,
    pub barcode_valid: Option<bool>,
    pub supplier_code: Option<String>,
    /// Quantity as printed, in the printed unit.
    pub qty_milli: Option<i64>,
    /// Printed unit: pcs | ctn | pkt | box | kg | l | …
    pub unit: Option<String>,
    pub pack: Pack,
    pub unit_cost_minor: Option<i64>,
    pub discount_minor: Option<i64>,
    pub vat_rate_bp: Option<i64>,
    pub vat_minor: Option<i64>,
    pub line_total_minor: Option<i64>,
    pub evidence: Evidence,
    /// Review flags raised while reading (e.g. `pack_unclear`, `low_ocr_confidence`).
    pub flags: Vec<String>,
}

/// Everything read from one document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Extraction {
    pub classification: Classification,
    pub fields: DocFields,
    pub lines: Vec<ExtractedLine>,
    /// Currency the amounts are printed in (BHD unless the document says otherwise).
    pub currency: String,
    pub warnings: Vec<String>,
}

/// Normalize an invoice/document number for duplicate checks: upper-case
/// letters and digits only. Leading zeros are kept (they are meaningful).
pub fn norm_doc_number(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_uppercase()).collect()
}

/// Lower-case words for name comparison, legal suffixes removed.
pub fn norm_name(s: &str) -> String {
    const DROP: &[&str] = &[
        "wll",
        "w.l.l",
        "w.l.l.",
        "co",
        "co.",
        "company",
        "trading",
        "est",
        "est.",
        "establishment",
        "llc",
        "bsc",
        "b.s.c",
        "spc",
        "s.p.c",
        "ltd",
        "limited",
        "and",
        "&",
        "ذ.م.م",
        "شركة",
        "مؤسسة",
        "للتجارة",
        "تجارة",
        "ش.م.ب",
    ];
    let lower = crate::ocrflow::normalize_digits(s).to_lowercase();
    lower
        .split(|c: char| c.is_whitespace() || c == ',' || c == '-' || c == '/' || c == '(' || c == ')')
        .map(|w| w.trim_matches(|c: char| c == '.' || c == '\'' || c == '"'))
        .filter(|w| !w.is_empty() && !DROP.contains(w))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Digits only (VAT, CR, phone comparisons).
pub fn digits_only(s: &str) -> String {
    crate::ocrflow::normalize_digits(s).chars().filter(|c| c.is_ascii_digit()).collect()
}

/// GTIN/EAN/UPC check digit (8, 12, 13 or 14 digits).
pub fn gtin_valid(code: &str) -> bool {
    if !matches!(code.len(), 8 | 12 | 13 | 14) || !code.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let d: Vec<u32> = code.chars().map(|c| c.to_digit(10).unwrap_or(0)).collect();
    let (body, check) = d.split_at(d.len() - 1);
    let sum: u32 = body.iter().rev().enumerate().map(|(i, v)| if i % 2 == 0 { v * 3 } else { *v }).sum();
    (10 - sum % 10) % 10 == check[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_digits() {
        assert!(gtin_valid("6291041500213"));
        assert!(!gtin_valid("6291041500214"));
        assert!(gtin_valid("96385074"));
        assert!(gtin_valid("036000291452"));
        assert!(!gtin_valid("12345"));
    }

    #[test]
    fn numbers_and_names() {
        assert_eq!(norm_doc_number("inv-00123 / A"), "INV00123A");
        assert_eq!(norm_name("Al Noor Trading Co. W.L.L."), "al noor");
        assert_eq!(norm_name("شركة النور للتجارة ذ.م.م"), "النور");
        assert_eq!(digits_only("+973 1234-5678"), "97312345678");
        assert_eq!(Band::from_score(95), Band::High);
        assert_eq!(Band::from_score(10), Band::Unresolved);
        assert!(Band::High > Band::Low);
    }
}
