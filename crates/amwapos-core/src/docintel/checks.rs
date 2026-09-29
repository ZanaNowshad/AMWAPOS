//! Deterministic checks on a read document: line and total arithmetic, VAT
//! against the configured tax rules, duplicates, anomaly signals, purchase
//! order / receiving reconciliation, cost variance and the summary.
//!
//! All money is integer fils; no model decides any of this. Discrepancies are
//! reported, never "fixed" by changing what the supplier printed.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{DocFields, DocType};
use crate::error::AppResult;
use crate::money;

pub fn bhd(v: i64) -> String {
    format!("{} BHD", money::format_decimal(v, 3))
}

pub fn signed_bhd(v: i64) -> String {
    if v > 0 {
        format!("+{}", bhd(v))
    } else {
        bhd(v)
    }
}

/// `(new - old) / old` as a percentage with two decimals, from integers only.
pub fn pct(new: i64, old: i64) -> Option<String> {
    if old == 0 {
        return None;
    }
    // hundredths of a percent, rounded half away from zero
    let h = money::div_round((new - old) as i128 * 10_000, old as i128);
    let sign = if h > 0 {
        "+"
    } else if h < 0 {
        "-"
    } else {
        ""
    };
    let a = h.abs();
    Some(format!("{sign}{}.{:02}%", a / 100, a % 100))
}

fn variance_bp(new: i64, old: i64) -> Option<i64> {
    (old != 0).then(|| money::div_round((new - old) as i128 * 10_000, old as i128) as i64)
}

/// A line as stored for review (after matching and corrections).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LineData {
    pub line_no: i64,
    pub description: String,
    pub product_id: Option<String>,
    pub product_name: Option<String>,
    pub qty_milli: Option<i64>,
    pub base_qty_milli: Option<i64>,
    pub units_per_case: Option<i64>,
    pub unit_cost_minor: Option<i64>,
    pub discount_minor: Option<i64>,
    pub vat_rate_bp: Option<i64>,
    pub vat_minor: Option<i64>,
    pub line_total_minor: Option<i64>,
    pub include: bool,
    pub po_item_id: Option<String>,
    pub new_product: bool,
}

impl LineData {
    /// Quantity in single units (what receiving counts).
    pub fn receive_qty(&self) -> Option<i64> {
        self.base_qty_milli.or(self.qty_milli)
    }

    /// Cost of one single unit: line net ÷ single units, exact to the fil
    /// when it divides; otherwise rounded (flagged by the caller).
    pub fn receive_unit_cost(&self) -> Option<(i64, bool)> {
        let q = self.receive_qty()?;
        if q <= 0 {
            return None;
        }
        match (self.base_qty_milli, self.qty_milli, self.line_net()) {
            (Some(b), Some(printed), Some(net)) if b != printed => {
                let n = net as i128 * 1000;
                Some(((money::div_round(n, b as i128)) as i64, n % b as i128 == 0))
            }
            _ => self.unit_cost_minor.map(|c| (c, true)),
        }
    }

    /// qty × unit − discount (excluding VAT), when both are known.
    pub fn calc_net(&self) -> Option<i64> {
        let (q, u) = (self.qty_milli?, self.unit_cost_minor?);
        Some(money::extend(u, q).ok()? - self.discount_minor.unwrap_or(0))
    }

    /// Net amount of the line: the printed total less its VAT when the VAT
    /// column is included in it, else the printed total, else the calculation.
    pub fn line_net(&self) -> Option<i64> {
        match (self.line_total_minor, self.calc_net(), self.vat_minor) {
            (Some(t), Some(c), Some(v)) if t == c + v => Some(c),
            (Some(t), _, _) => Some(t),
            (None, c, _) => c,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Issue {
    pub code: String,
    /// error | warning | info
    pub severity: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_no: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub printed_minor: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calculated_minor: Option<i64>,
}

fn issue(code: &str, sev: &str, message: String) -> Issue {
    Issue { code: code.into(), severity: sev.into(), message, line_no: None, field: None, printed_minor: None, calculated_minor: None }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Validation {
    pub arithmetic_ok: bool,
    pub vat_ok: Option<bool>,
    pub lines_net_minor: i64,
    pub calc_vat_minor: Option<i64>,
    pub calc_total_minor: i64,
    /// `excl` (line totals before VAT) or `incl`.
    pub line_basis: String,
    pub issues: Vec<Issue>,
}

/// Arithmetic and VAT. `rates` = configured active tax rates (bp);
/// `product_rates` = tax rate of each matched product by line_no.
pub fn validate(f: &DocFields, lines: &[LineData], rates: &[i64], product_rates: &[(i64, i64)]) -> Validation {
    let mut v = Validation { arithmetic_ok: true, line_basis: "excl".into(), ..Default::default() };
    let inc: Vec<&LineData> = lines.iter().filter(|l| l.include).collect();
    // ---- lines
    for l in &inc {
        if let (Some(t), Some(c)) = (l.line_total_minor, l.calc_net()) {
            let with_vat = l.vat_minor.map(|x| c + x);
            if t != c && Some(t) != with_vat {
                v.arithmetic_ok = false;
                let mut i = issue(
                    "line_arithmetic",
                    "error",
                    format!(
                        "Line {}: {} × {} {}= {}, but the line total is printed as {}.",
                        l.line_no,
                        money::format_qty(l.qty_milli.unwrap_or(0)),
                        bhd(l.unit_cost_minor.unwrap_or(0)),
                        l.discount_minor.map(|d| format!("− discount {} ", bhd(d))).unwrap_or_default(),
                        bhd(c),
                        bhd(t)
                    ),
                );
                i.line_no = Some(l.line_no);
                i.printed_minor = Some(t);
                i.calculated_minor = Some(c);
                v.issues.push(i);
            }
        } else if l.line_total_minor.is_none() && l.calc_net().is_none() {
            let mut i = issue(
                "line_incomplete",
                "warning",
                format!("Line {}: the quantity, unit cost or line total could not be read.", l.line_no),
            );
            i.line_no = Some(l.line_no);
            v.issues.push(i);
        }
    }
    let nets: Vec<i64> = inc.iter().filter_map(|l| l.line_net()).collect();
    v.lines_net_minor = nets.iter().sum();
    let lines_total: i64 = inc.iter().filter_map(|l| l.line_total_minor.or(l.calc_net())).sum();
    // Line totals that already include VAT: they add up to the grand total.
    if f.subtotal_minor.value != Some(lines_total) && f.total_minor.value == Some(lines_total) && f.vat_minor.is_set() {
        v.line_basis = "incl".into();
    }
    // ---- subtotal
    let doc_discount = f.discount_minor.value.unwrap_or(0);
    if v.line_basis == "excl" {
        if let Some(sub) = f.subtotal_minor.value {
            let calc = v.lines_net_minor;
            if sub != calc && sub != calc - doc_discount {
                v.arithmetic_ok = false;
                let mut i = issue(
                    "subtotal_mismatch",
                    "error",
                    format!(
                        "Printed subtotal: {}. Calculated from extracted lines: {}. Difference: {}.",
                        bhd(sub),
                        bhd(calc),
                        bhd((sub - calc).abs())
                    ),
                );
                i.field = Some("subtotal_minor".into());
                i.printed_minor = Some(sub);
                i.calculated_minor = Some(calc);
                v.issues.push(i);
            }
        }
    }
    // ---- VAT: per line (printed rate, else the product's rate, else the document rate)
    let doc_rate = f.vat_rate_bp.value;
    let mut by_rate: std::collections::BTreeMap<i64, i64> = Default::default();
    let mut line_level = 0i64;
    let mut known = true;
    for l in &inc {
        let net = match l.line_net() {
            Some(n) => n,
            None => {
                known = false;
                continue;
            }
        };
        let prod = product_rates.iter().find(|(n, _)| *n == l.line_no).map(|x| x.1);
        let rate = l.vat_rate_bp.or(prod).or(doc_rate);
        if let (Some(p), Some(r)) = (l.vat_rate_bp, prod) {
            if p != r {
                let mut i = issue(
                    "tax_treatment_conflict",
                    "warning",
                    format!(
                        "Line {}: the invoice charges {}% VAT but the product is set up at {}% in AMWAPOS.",
                        l.line_no,
                        money::format_decimal(p, 2),
                        money::format_decimal(r, 2)
                    ),
                );
                i.line_no = Some(l.line_no);
                v.issues.push(i);
            }
        }
        if let Some(p) = l.vat_rate_bp {
            if !rates.contains(&p) {
                let mut i = issue(
                    "unexpected_tax_rate",
                    "warning",
                    format!("Line {}: VAT rate {}% is not one of the configured tax rates.", l.line_no, money::format_decimal(p, 2)),
                );
                i.line_no = Some(l.line_no);
                v.issues.push(i);
            }
        }
        match rate {
            Some(r) => {
                let taxable = if v.line_basis == "incl" { net - money::tax_from_inclusive(net, r).unwrap_or(0) } else { net };
                *by_rate.entry(r).or_default() += taxable;
                line_level += money::tax_on_exclusive(taxable, r).unwrap_or(0);
                if let (Some(pv), true) = (l.vat_minor, v.line_basis == "excl") {
                    let lv = money::tax_on_exclusive(taxable, r).unwrap_or(0);
                    if (pv - lv).abs() > 1 {
                        let mut i = issue(
                            "line_vat_mismatch",
                            "warning",
                            format!(
                                "Line {}: VAT printed {} but {}% of {} is {}.",
                                l.line_no,
                                bhd(pv),
                                money::format_decimal(r, 2),
                                bhd(taxable),
                                bhd(lv)
                            ),
                        );
                        i.line_no = Some(l.line_no);
                        i.printed_minor = Some(pv);
                        i.calculated_minor = Some(lv);
                        v.issues.push(i);
                    }
                }
            }
            None => known = false,
        }
    }
    if let Some(r) = doc_rate {
        if !rates.contains(&r) {
            v.issues.push(issue(
                "unexpected_tax_rate",
                "warning",
                format!("The document VAT rate {}% is not one of the configured tax rates.", money::format_decimal(r, 2)),
            ));
        }
    }
    let doc_level: i64 = by_rate.iter().map(|(r, t)| money::tax_on_exclusive(*t, *r).unwrap_or(0)).sum();
    if known && !inc.is_empty() {
        v.calc_vat_minor = Some(doc_level);
    }
    if let (Some(pv), true) = (f.vat_minor.value, known && !inc.is_empty()) {
        let ok = pv == doc_level || pv == line_level;
        v.vat_ok = Some(ok);
        if !ok {
            let mut i = issue(
                "vat_mismatch",
                "error",
                format!(
                    "Printed VAT: {}. Calculated with the applicable rates: {}. Difference: {}.",
                    bhd(pv),
                    bhd(doc_level),
                    bhd((pv - doc_level).abs())
                ),
            );
            i.field = Some("vat_minor".into());
            i.printed_minor = Some(pv);
            i.calculated_minor = Some(doc_level);
            v.issues.push(i);
        }
    } else if f.vat_minor.is_set() && !known {
        v.issues.push(issue("vat_unverified", "info", "VAT could not be verified: some lines have no amount or no known tax rate.".into()));
    }
    // ---- grand total
    let vat_for_total = f.vat_minor.value.or(v.calc_vat_minor).unwrap_or(0);
    v.calc_total_minor = if v.line_basis == "incl" { lines_total } else { v.lines_net_minor - doc_discount + vat_for_total };
    if let Some(t) = f.total_minor.value {
        let calc = v.calc_total_minor;
        let alt = f.subtotal_minor.value.map(|s| s + vat_for_total);
        if t != calc && Some(t) != alt {
            v.arithmetic_ok = false;
            let mut i = issue(
                "total_mismatch",
                "error",
                format!(
                    "Printed grand total: {}. Calculated from extracted lines: {}. Difference: {}.",
                    bhd(t),
                    bhd(calc),
                    bhd((t - calc).abs())
                ),
            );
            i.field = Some("total_minor".into());
            i.printed_minor = Some(t);
            i.calculated_minor = Some(calc);
            v.issues.push(i);
        } else if t != calc {
            v.issues.push(issue(
                "lines_incomplete",
                "warning",
                format!(
                    "Subtotal and VAT add up to the printed total ({}), but the extracted lines add up to {}: a line may be missing or misread.",
                    bhd(t),
                    bhd(calc)
                ),
            ));
            v.arithmetic_ok = false;
        }
    } else {
        v.issues.push(issue("total_missing", "warning", "No grand total was found on the document.".into()));
    }
    v
}

// ---------------------------------------------------------------- duplicates

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Duplicate {
    /// exact_document | same_invoice | same_scanned_differently | possible_duplicate
    pub kind: String,
    pub scan_id: String,
    pub scan_number: String,
    pub status: String,
    pub reasons: Vec<String>,
}

/// Order-independent fingerprint of the lines: code/description, qty, total.
pub fn line_fingerprint(lines: &[LineData]) -> String {
    use sha2::Digest;
    let mut parts: Vec<String> = lines
        .iter()
        .map(|l| format!("{}|{}|{}", super::matching::desc_key(&l.description), l.qty_milli.unwrap_or(0), l.line_total_minor.unwrap_or(0)))
        .collect();
    parts.sort();
    hex::encode(sha2::Sha256::digest(parts.join("\n").as_bytes()))[..32].to_string()
}

#[allow(clippy::too_many_arguments)]
pub fn duplicates(
    c: &Connection,
    scan_id: &str,
    sha: &str,
    supplier: Option<&str>,
    number_norm: Option<&str>,
    date: Option<&str>,
    total: Option<i64>,
    fingerprint: Option<&str>,
) -> AppResult<Vec<Duplicate>> {
    let mut out: Vec<Duplicate> = vec![];
    let mut push = |kind: &str, id: String, num: String, status: String, reason: String| {
        if let Some(d) = out.iter_mut().find(|d| d.scan_id == id) {
            d.reasons.push(reason);
            // keep the strongest kind
            let rank = |k: &str| {
                ["possible_duplicate", "same_scanned_differently", "same_invoice", "exact_document"]
                    .iter()
                    .position(|x| *x == k)
                    .unwrap_or(0)
            };
            if rank(kind) > rank(&d.kind) {
                d.kind = kind.into();
            }
        } else {
            out.push(Duplicate { kind: kind.into(), scan_id: id, scan_number: num, status, reasons: vec![reason] });
        }
    };
    type Row = (String, String, String);
    let q = |sql: &str, p: &[&dyn rusqlite::ToSql]| -> AppResult<Vec<Row>> {
        let mut st = c.prepare(sql)?;
        let rows = st.query_map(p, |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    };
    for (id, n, s) in q("SELECT scan_id, scan_number, status FROM invoice_scans WHERE image_sha256=?1 AND scan_id<>?2", &[&sha, &scan_id])?
    {
        push("exact_document", id, n, s, "The same file was uploaded before.".into());
    }
    if let (Some(sup), Some(num)) = (supplier, number_norm.filter(|x| !x.is_empty())) {
        for (id, n, s) in q(
            "SELECT scan_id, scan_number, status FROM invoice_scans WHERE supplier_id=?1 AND invoice_number_norm=?2 AND scan_id<>?3",
            &[&sup, &num, &scan_id],
        )? {
            push("same_invoice", id, n, s, "Same supplier and invoice number.".into());
        }
        let si: Vec<Row> = q(
            "SELECT invoice_id, number, status FROM supplier_invoices WHERE supplier_id=?1 AND status<>'void' AND upper(replace(replace(replace(COALESCE(invoice_number,''),'-',''),'/',''),' ',''))=?2 AND COALESCE(scan_id,'')<>?3",
            &[&sup, &num, &scan_id],
        )?;
        for (id, n, s) in si {
            push("same_invoice", id, n, s, "A supplier invoice with this number is already recorded.".into());
        }
    }
    if let (Some(sup), Some(fp)) = (supplier, fingerprint) {
        for (id, n, s) in q(
            "SELECT scan_id, scan_number, status FROM invoice_scans WHERE supplier_id=?1 AND line_fingerprint=?2 AND scan_id<>?3",
            &[&sup, &fp, &scan_id],
        )? {
            push("same_scanned_differently", id, n, s, "Same supplier and the same item lines.".into());
        }
    }
    if let (Some(sup), Some(d), Some(t)) = (supplier, date, total) {
        for (id, n, s) in q(
            "SELECT scan_id, scan_number, status FROM invoice_scans WHERE supplier_id=?1 AND invoice_date=?2 AND total_minor=?3 AND scan_id<>?4",
            &[&sup, &d, &t, &scan_id],
        )? {
            push("possible_duplicate", id, n, s, "Same supplier, date and total.".into());
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- anomalies

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Anomaly {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_no: Option<i64>,
}

fn anomaly(code: &str, message: String) -> Anomaly {
    Anomaly { code: code.into(), message, line_no: None }
}

/// Document anomaly signals: each with the reason it was raised. They are
/// prompts to look closer, not findings of fraud.
#[allow(clippy::too_many_arguments)]
pub fn anomalies(
    c: &Connection,
    f: &DocFields,
    v: &Validation,
    dups: &[Duplicate],
    supplier: &super::matching::MatchResult,
    lines: &[LineData],
    threshold_bp: i64,
    branch_id: &str,
    supplier_id: Option<&str>,
    scan_id: &str,
) -> AppResult<Vec<Anomaly>> {
    let mut out = vec![];
    if !v.arithmetic_ok {
        out.push(anomaly("total_arithmetic", "The printed totals do not add up from the printed lines (see the checks above).".into()));
    }
    if v.vat_ok == Some(false) {
        out.push(anomaly("vat_inconsistent", "The printed VAT does not match the rates that apply to these lines.".into()));
    }
    if dups.iter().any(|d| d.kind == "same_invoice") {
        out.push(anomaly("duplicate_number", "This supplier's invoice number has been seen before.".into()));
    }
    // A VAT or CR number that belongs to a different supplier than the name.
    if matches!(supplier.kind.as_str(), "vat" | "cr" | "alias") {
        if let Some(alt) = supplier.alternatives.iter().find(|a| a.reasons.iter().any(|r| r.starts_with("Name"))) {
            if alt.score >= 70 {
                out.push(anomaly(
                    "supplier_identifier_mismatch",
                    format!(
                        "The tax/registration number belongs to {}, but the printed name looks like {}.",
                        supplier.name.clone().unwrap_or_default(),
                        alt.name
                    ),
                ));
            }
        }
    }
    for fld in [(&f.total_minor, "total"), (&f.vat_minor, "VAT")] {
        if let Some(conf) = fld.0.evidence.as_ref().and_then(|e| e.ocr_conf) {
            if conf < 45 {
                out.push(anomaly(
                    "unclear_key_value",
                    format!(
                        "The printed {} is hard to read (OCR confidence {conf}): it may be handwritten, damaged or overwritten.",
                        fld.1
                    ),
                ));
            }
        }
    }
    // Bank details different from this supplier's earlier documents.
    if let (Some(iban), Some(sid)) = (f.iban.value.as_deref(), supplier_id) {
        let prev: Option<String> = c
            .query_row(
                "SELECT json_extract(fields_json,'$.iban.value') FROM invoice_scans WHERE supplier_id=?1 AND scan_id<>?2 AND json_extract(fields_json,'$.iban.value') IS NOT NULL ORDER BY created_at DESC LIMIT 1",
                params![sid, scan_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(p) = prev.filter(|p| p != iban) {
            out.push(anomaly(
                "bank_details_changed",
                format!(
                    "The bank account (IBAN ending {}) differs from this supplier's earlier documents (ending {}).",
                    &iban[iban.len().saturating_sub(4)..],
                    &p[p.len().saturating_sub(4)..]
                ),
            ));
        }
    }
    // Invoice sequence: a later date with a lower number than the supplier's last invoice.
    if let (Some(sid), Some(num), Some(date)) = (supplier_id, f.invoice_number.value.as_deref(), f.invoice_date.value.as_deref()) {
        let n: Option<i64> = super::digits_only(num).parse().ok();
        let prev: Option<(String, String)> = c
            .query_row(
                "SELECT invoice_number, invoice_date FROM invoice_scans WHERE supplier_id=?1 AND scan_id<>?2 AND invoice_date IS NOT NULL AND invoice_number IS NOT NULL AND invoice_date < ?3 ORDER BY invoice_date DESC LIMIT 1",
                params![sid, scan_id, date],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let (Some(n), Some((pn, pd))) = (n, prev) {
            if let Ok(p) = super::digits_only(&pn).parse::<i64>() {
                if n < p {
                    out.push(anomaly("invoice_sequence", format!("Invoice number {num} is lower than {pn} from an earlier date ({pd}).")));
                }
            }
        }
    }
    // Unusually large price change against the last purchase cost.
    for l in lines.iter().filter(|l| l.include) {
        let (Some(pid), Some((cost, _))) = (&l.product_id, l.receive_unit_cost()) else { continue };
        let last: Option<i64> = c
            .query_row("SELECT last_cost_minor FROM product_costs WHERE product_id=?1 AND branch_id=?2", params![pid, branch_id], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(last) = last.filter(|x| *x > 0) {
            if let Some(bp) = variance_bp(cost, last) {
                if bp.abs() >= (threshold_bp * 5).max(2500) {
                    out.push(Anomaly {
                        code: "price_deviation".into(),
                        message: format!(
                            "Line {}: unit cost {} is {} against the last purchase cost {}.",
                            l.line_no,
                            bhd(cost),
                            pct(cost, last).unwrap_or_default(),
                            bhd(last)
                        ),
                        line_no: Some(l.line_no),
                    });
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- PO + receiving

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoCandidate {
    pub po_id: String,
    pub po_number: String,
    pub status: String,
    pub score: i64,
    pub reasons: Vec<String>,
}

/// Open purchase orders of the supplier that best fit the document.
pub fn po_candidates(c: &Connection, supplier: &str, f: &DocFields, lines: &[LineData]) -> AppResult<Vec<PoCandidate>> {
    let mut st = c.prepare(
        "SELECT po_id, po_number, status, reference, COALESCE(ordered_at, created_at) FROM purchase_orders
         WHERE supplier_id=?1 AND status IN ('draft','ordered','partially_received','received') ORDER BY created_at DESC LIMIT 30",
    )?;
    let pos: Vec<(String, String, String, Option<String>, String)> =
        st.query_map([supplier], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<_, _>>()?;
    let doc_products: std::collections::BTreeSet<&str> =
        lines.iter().filter(|l| l.include).filter_map(|l| l.product_id.as_deref()).collect();
    let mut out = vec![];
    for (id, number, status, reference, at) in pos {
        let mut score = 0;
        let mut reasons = vec![];
        if let Some(pn) = f.po_number.value.as_deref().map(super::norm_doc_number) {
            if super::norm_doc_number(&number) == pn || reference.as_deref().map(super::norm_doc_number).as_deref() == Some(pn.as_str()) {
                score += 60;
                reasons.push("PO number printed on the document".into());
            }
        }
        let mut st = c.prepare("SELECT product_id FROM purchase_order_items WHERE po_id=?1")?;
        let items: std::collections::BTreeSet<String> = st.query_map([&id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        let shared = doc_products.iter().filter(|p| items.contains(**p)).count();
        if !doc_products.is_empty() && shared > 0 {
            let s = (shared * 40 / doc_products.len().max(items.len())) as i64;
            score += s;
            reasons.push(format!("{shared} of {} products on the order", doc_products.len()));
        }
        if let Some(d) = f.invoice_date.value.as_deref() {
            if at.as_str() <= d && at.len() >= 10 && d.len() >= 10 {
                score += 5;
            }
        }
        if score > 0 {
            out.push(PoCandidate { po_id: id, po_number: number, status, score: score.min(100), reasons });
        }
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.score));
    Ok(out)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconLine {
    pub product_id: Option<String>,
    pub product_name: Option<String>,
    pub line_no: Option<i64>,
    pub po_item_id: Option<String>,
    pub ordered_milli: Option<i64>,
    pub received_milli: Option<i64>,
    pub invoiced_milli: Option<i64>,
    pub po_cost_minor: Option<i64>,
    pub invoice_cost_minor: Option<i64>,
    pub last_cost_minor: Option<i64>,
    pub cost_variance_minor: Option<i64>,
    pub cost_variance_pct: Option<String>,
    pub last_variance_pct: Option<String>,
    /// fully_matched | quantity_variance | cost_variance | invoice_exceeds_received |
    /// received_exceeds_invoice | not_on_po | missing_from_invoice | unmatched_product
    pub states: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Recon {
    pub po_id: Option<String>,
    pub po_number: Option<String>,
    pub po_status: Option<String>,
    pub selected_by: Option<String>,
    pub candidates: Vec<PoCandidate>,
    pub three_way: bool,
    pub lines: Vec<ReconLine>,
    pub summary: Vec<String>,
}

/// Line-by-line PO ↔ invoice (↔ goods received when there is a receipt).
pub fn reconcile(c: &Connection, po_id: &str, lines: &[LineData], threshold_bp: i64, branch_id: &str) -> AppResult<Recon> {
    let (po_number, po_status): (String, String) =
        c.query_row("SELECT po_number, status FROM purchase_orders WHERE po_id=?1", [po_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let mut st = c.prepare(
        "SELECT i.po_item_id, i.product_id, p.name, i.qty_ordered_milli, i.qty_received_milli, i.unit_cost_minor
         FROM purchase_order_items i JOIN products p ON p.product_id=i.product_id WHERE i.po_id=?1 ORDER BY i.line_no",
    )?;
    type PoRow = (String, String, String, i64, i64, i64);
    let items: Vec<PoRow> =
        st.query_map([po_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<Result<_, _>>()?;
    let received_any: bool =
        c.query_row("SELECT EXISTS(SELECT 1 FROM goods_receipts WHERE po_id=?1)", [po_id], |r| r.get::<_, i64>(0))? == 1;
    let mut used = vec![false; items.len()];
    let mut out = vec![];
    let last_cost = |pid: &str| -> AppResult<Option<i64>> {
        Ok(c.query_row("SELECT last_cost_minor FROM product_costs WHERE product_id=?1 AND branch_id=?2", params![pid, branch_id], |r| {
            r.get(0)
        })
        .optional()?
        .filter(|x: &i64| *x > 0))
    };
    for l in lines.iter().filter(|l| l.include) {
        let cost = l.receive_unit_cost().map(|x| x.0);
        let qty = l.receive_qty();
        let Some(pid) = l.product_id.clone() else {
            out.push(ReconLine {
                product_id: None,
                product_name: None,
                line_no: Some(l.line_no),
                po_item_id: None,
                ordered_milli: None,
                received_milli: None,
                invoiced_milli: qty,
                po_cost_minor: None,
                invoice_cost_minor: cost,
                last_cost_minor: None,
                cost_variance_minor: None,
                cost_variance_pct: None,
                last_variance_pct: None,
                states: vec!["unmatched_product".into()],
                notes: vec!["Match the line to a product to compare it with the order.".into()],
            });
            continue;
        };
        let idx = l
            .po_item_id
            .as_ref()
            .and_then(|x| items.iter().position(|i| &i.0 == x))
            .or_else(|| items.iter().enumerate().position(|(k, i)| !used[k] && i.1 == pid));
        let last = last_cost(&pid)?;
        let mut rl = ReconLine {
            product_id: Some(pid.clone()),
            product_name: l.product_name.clone(),
            line_no: Some(l.line_no),
            po_item_id: None,
            ordered_milli: None,
            received_milli: None,
            invoiced_milli: qty,
            po_cost_minor: None,
            invoice_cost_minor: cost,
            last_cost_minor: last,
            cost_variance_minor: None,
            cost_variance_pct: None,
            last_variance_pct: match (cost, last) {
                (Some(a), Some(b)) => pct(a, b),
                _ => None,
            },
            states: vec![],
            notes: vec![],
        };
        match idx {
            None => rl.states.push("not_on_po".into()),
            Some(k) => {
                used[k] = true;
                let (iid, _, name, ordered, received, pcost) = &items[k];
                rl.po_item_id = Some(iid.clone());
                rl.product_name = Some(name.clone());
                rl.ordered_milli = Some(*ordered);
                rl.received_milli = received_any.then_some(*received);
                rl.po_cost_minor = Some(*pcost);
                if let Some(q) = qty {
                    if q != *ordered {
                        rl.states.push("quantity_variance".into());
                        rl.notes.push(format!("Ordered {}, invoiced {}.", money::format_qty(*ordered), money::format_qty(q)));
                    }
                    if received_any && q > *received {
                        rl.states.push("invoice_exceeds_received".into());
                        rl.notes.push(format!("Invoiced {} but only {} received.", money::format_qty(q), money::format_qty(*received)));
                    } else if received_any && q < *received {
                        rl.states.push("received_exceeds_invoice".into());
                        rl.notes.push(format!("Received {} but only {} invoiced.", money::format_qty(*received), money::format_qty(q)));
                    }
                }
                if let Some(ic) = cost {
                    let d = ic - pcost;
                    rl.cost_variance_minor = Some(d);
                    rl.cost_variance_pct = pct(ic, *pcost);
                    if d != 0 {
                        let over = variance_bp(ic, *pcost).map(|b| b.abs() >= threshold_bp).unwrap_or(true);
                        if over {
                            rl.states.push("cost_variance".into());
                        }
                        rl.notes.push(format!(
                            "PO: {}  Invoice: {}  Variance: {} ({})",
                            bhd(*pcost),
                            bhd(ic),
                            signed_bhd(d),
                            rl.cost_variance_pct.clone().unwrap_or_default()
                        ));
                    }
                }
                if rl.states.is_empty() {
                    rl.states.push("fully_matched".into());
                }
            }
        }
        out.push(rl);
    }
    for (k, (iid, pid, name, ordered, received, pcost)) in items.iter().enumerate() {
        if !used[k] {
            out.push(ReconLine {
                product_id: Some(pid.clone()),
                product_name: Some(name.clone()),
                line_no: None,
                po_item_id: Some(iid.clone()),
                ordered_milli: Some(*ordered),
                received_milli: received_any.then_some(*received),
                invoiced_milli: None,
                po_cost_minor: Some(*pcost),
                invoice_cost_minor: None,
                last_cost_minor: last_cost(pid)?,
                cost_variance_minor: None,
                cost_variance_pct: None,
                last_variance_pct: None,
                states: vec!["missing_from_invoice".into()],
                notes: vec![],
            });
        }
    }
    let count = |s: &str| out.iter().filter(|l| l.states.iter().any(|x| x == s)).count();
    let mut summary = vec![];
    let matched = count("fully_matched");
    summary.push(format!("{matched} of {} lines fully matched to {po_number}.", out.len()));
    for (s, label) in [
        ("cost_variance", "above/below PO cost"),
        ("quantity_variance", "with a quantity different from the order"),
        ("not_on_po", "not on the order"),
        ("missing_from_invoice", "ordered but not invoiced"),
        ("invoice_exceeds_received", "invoiced beyond what was received"),
        ("received_exceeds_invoice", "received beyond what was invoiced"),
    ] {
        let n = count(s);
        if n > 0 {
            summary.push(format!("{n} {} {label}.", if n == 1 { "line" } else { "lines" }));
        }
    }
    Ok(Recon {
        po_id: Some(po_id.to_string()),
        po_number: Some(po_number),
        po_status: Some(po_status),
        selected_by: None,
        candidates: vec![],
        three_way: received_any,
        lines: out,
        summary,
    })
}

// ---------------------------------------------------------------- summary

pub struct SummaryInput<'a> {
    pub supplier: Option<&'a str>,
    pub doc_type: DocType,
    pub f: &'a DocFields,
    pub lines: &'a [LineData],
    pub bands: &'a [String],
    pub validation: &'a Validation,
    pub recon: Option<&'a Recon>,
    pub duplicates: usize,
}

/// One paragraph for staff, built only from the checked structured data.
pub fn summary(s: &SummaryInput) -> String {
    let kind = match s.doc_type {
        DocType::Invoice => "invoice",
        DocType::CreditNote => "credit note",
        DocType::DeliveryNote => "delivery note",
        DocType::Unknown => "document (type not confirmed)",
    };
    let mut out = String::new();
    out.push_str(&format!("{} {kind}", s.supplier.unwrap_or("Unknown supplier")));
    if let Some(n) = &s.f.invoice_number.value {
        out.push_str(&format!(" {n}"));
    }
    if let Some(d) = &s.f.invoice_date.value {
        if let Ok(dt) = chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d") {
            out.push_str(&format!(" dated {}", dt.format("%-d %b %Y")));
        }
    }
    out.push('.');
    let inc: Vec<&LineData> = s.lines.iter().filter(|l| l.include).collect();
    out.push_str(&format!(" {} {}", inc.len(), if inc.len() == 1 { "line" } else { "lines" }));
    let mut money_parts = vec![];
    if let Some(v) = s.f.subtotal_minor.value {
        money_parts.push(format!("subtotal {}", bhd(v)));
    }
    if let Some(v) = s.f.vat_minor.value {
        money_parts.push(format!("VAT {}", bhd(v)));
    }
    if let Some(v) = s.f.total_minor.value {
        money_parts.push(format!("total {}", bhd(v)));
    }
    if !money_parts.is_empty() {
        out.push_str(&format!(", {}", money_parts.join(", ")));
    }
    out.push('.');
    let high = s.bands.iter().filter(|b| *b == "high" || *b == "medium").count();
    let low = s.bands.iter().filter(|b| *b == "low").count();
    let new = inc.iter().filter(|l| l.new_product).count();
    let unresolved = s.bands.iter().filter(|b| *b == "unresolved").count().saturating_sub(new);
    let mut m = vec![format!("{high} products matched")];
    if low > 0 {
        m.push(format!("{low} low-confidence {}", if low == 1 { "match" } else { "matches" }));
    }
    if new > 0 {
        m.push(format!("{new} new product {}", if new == 1 { "candidate" } else { "candidates" }));
    }
    if unresolved > 0 {
        m.push(format!("{unresolved} unmatched"));
    }
    out.push_str(&format!(" {}.", join_and(&m)));
    if let Some(r) = s.recon {
        let above = r.lines.iter().filter(|l| l.cost_variance_minor.is_some_and(|d| d > 0)).count();
        if above > 0 {
            out.push_str(&format!(" {above} {} above PO cost.", if above == 1 { "item is" } else { "items are" }));
        }
        let qty = r.lines.iter().filter(|l| l.states.iter().any(|x| x == "quantity_variance")).count();
        if qty > 0 {
            out.push_str(&format!(" {qty} {} a quantity different from the order.", if qty == 1 { "line has" } else { "lines have" }));
        }
    }
    if s.validation.arithmetic_ok {
        out.push_str(" Invoice total arithmetic is valid.");
    } else {
        out.push_str(" The totals do not add up — see the checks.");
    }
    match s.validation.vat_ok {
        Some(true) => out.push_str(" VAT checks out."),
        Some(false) => out.push_str(" VAT does not match the rates."),
        None => {}
    }
    if s.duplicates > 0 {
        out.push_str(" Possible duplicate — see the warning.");
    }
    out
}

fn join_and(parts: &[String]) -> String {
    match parts.len() {
        0 => String::new(),
        1 => parts[0].clone(),
        n => format!("{} and {}", parts[..n - 1].join(", "), parts[n - 1]),
    }
}

/// Everything above as JSON for storage (keeps the review payload stable).
pub fn to_json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| json!(null).to_string())
}

#[cfg(test)]
mod tests {
    use super::super::Field;
    use super::*;

    fn line(no: i64, q: i64, u: i64, t: i64) -> LineData {
        LineData {
            line_no: no,
            description: format!("Item {no}"),
            qty_milli: Some(q),
            unit_cost_minor: Some(u),
            line_total_minor: Some(t),
            include: true,
            ..Default::default()
        }
    }

    fn fields(sub: Option<i64>, vat: Option<i64>, rate: Option<i64>, total: Option<i64>) -> DocFields {
        let mk = |v: Option<i64>| match v {
            Some(x) => Field::found(x, x.to_string(), Default::default(), super::super::Band::High),
            None => Field::default(),
        };
        DocFields { subtotal_minor: mk(sub), vat_minor: mk(vat), vat_rate_bp: mk(rate), total_minor: mk(total), ..Default::default() }
    }

    #[test]
    fn percentages_are_exact() {
        assert_eq!(pct(925, 850).as_deref(), Some("+8.82%"));
        assert_eq!(pct(850, 925).as_deref(), Some("-8.11%"));
        assert_eq!(pct(100, 100).as_deref(), Some("0.00%"));
        assert_eq!(signed_bhd(75), "+0.075 BHD");
    }

    #[test]
    fn arithmetic_and_vat_ok() {
        let l = vec![line(1, 10_000, 450, 4_500), line(2, 2_000, 5_400, 10_800)];
        let v = validate(&fields(Some(15_300), Some(1_530), Some(1000), Some(16_830)), &l, &[0, 1000], &[]);
        assert!(v.arithmetic_ok, "{:?}", v.issues);
        assert_eq!(v.vat_ok, Some(true));
        assert_eq!(v.calc_total_minor, 16_830);
    }

    #[test]
    fn total_discrepancy_message() {
        let l = vec![line(1, 10_000, 450, 4_500), line(2, 2_000, 5_400, 10_800)];
        let v = validate(&fields(None, Some(1_530), Some(1000), Some(17_330)), &l, &[1000], &[]);
        assert!(!v.arithmetic_ok);
        let m = v.issues.iter().find(|i| i.code == "total_mismatch").unwrap();
        assert_eq!(m.message, "Printed grand total: 17.330 BHD. Calculated from extracted lines: 16.830 BHD. Difference: 0.500 BHD.");
    }

    #[test]
    fn line_and_vat_discrepancies() {
        let l = vec![line(1, 3_000, 400, 1_300)];
        let v = validate(&fields(Some(1_300), Some(100), Some(1000), Some(1_400)), &l, &[1000], &[]);
        assert!(v.issues.iter().any(|i| i.code == "line_arithmetic"));
        assert!(v.issues.iter().any(|i| i.code == "vat_mismatch"), "{:?}", v.issues);
        // Mixed treatment: a zero-rated product and a standard one.
        let l = vec![line(1, 1_000, 1_000, 1_000), line(2, 1_000, 2_000, 2_000)];
        let v = validate(&fields(Some(3_000), Some(200), None, Some(3_200)), &l, &[0, 1000], &[(1, 0), (2, 1000)]);
        assert_eq!(v.vat_ok, Some(true), "{:?}", v.issues);
        let mut l2 = l.clone();
        l2[0].vat_rate_bp = Some(1000);
        let v = validate(&fields(Some(3_000), Some(300), None, Some(3_300)), &l2, &[0, 1000], &[(1, 0), (2, 1000)]);
        assert!(v.issues.iter().any(|i| i.code == "tax_treatment_conflict"));
        let mut l3 = l.clone();
        l3[1].vat_rate_bp = Some(1500);
        let v = validate(&fields(Some(3_000), Some(300), None, Some(3_300)), &l3, &[0, 1000], &[]);
        assert!(v.issues.iter().any(|i| i.code == "unexpected_tax_rate"));
    }

    #[test]
    fn inclusive_line_totals() {
        // Lines printed VAT-inclusive; VAT shown separately; total = sum of lines.
        let l = vec![line(1, 1_000, 1_100, 1_100)];
        let v = validate(&fields(None, Some(100), Some(1000), Some(1_100)), &l, &[1000], &[]);
        assert_eq!(v.line_basis, "incl");
        assert!(v.arithmetic_ok, "{:?}", v.issues);
        assert_eq!(v.vat_ok, Some(true), "{:?}", v.issues);
    }

    #[test]
    fn case_unit_cost() {
        let l = LineData {
            qty_milli: Some(2_000),
            base_qty_milli: Some(48_000),
            unit_cost_minor: Some(7_200),
            line_total_minor: Some(14_400),
            include: true,
            ..Default::default()
        };
        assert_eq!(l.receive_qty(), Some(48_000));
        assert_eq!(l.receive_unit_cost(), Some((300, true)));
    }
}
