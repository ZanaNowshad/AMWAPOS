//! The AI extraction contract. A model reads the OCR lines and replies with
//! one JSON object; this module validates it field by field. Anything
//! malformed, out of range or not offered as a candidate is dropped and
//! listed in `rejected` — never repaired by guessing. Money must be a decimal
//! string or number with at most three decimals; ids must be ones we sent.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::layout::Layout;
use super::{Band, Classification, DocFields, DocType, Evidence, ExtractedLine, Field, Pack};

/// The JSON shape the model is asked for (sent in the prompt).
pub const SCHEMA_HINT: &str = r#"{"doc_type":"invoice|credit_note|delivery_note|unknown","supplier_name":string|null,"supplier_vat":string|null,
"supplier_cr":string|null,"invoice_number":string|null,"invoice_date":"YYYY-MM-DD"|null,"due_date":"YYYY-MM-DD"|null,
"subtotal":"0.000"|null,"vat":"0.000"|null,"total":"0.000"|null,"vat_rate_percent":number|null,
"lines":[{"src_line":number,"description":string,"barcode":string|null,"code":string|null,"qty":"0"|null,"unit":"pcs|ctn|pkt|box|kg"|null,
"units_per_case":number|null,"unit_cost":"0.000"|null,"discount":"0.000"|null,"vat_rate_percent":number|null,"vat":"0.000"|null,
"line_total":"0.000"|null,"product_id":string|null}]}"#;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AiExtraction {
    pub classification: Option<Classification>,
    pub fields: DocFields,
    pub lines: Vec<(ExtractedLine, Option<String>)>,
    pub rejected: Vec<String>,
}

fn text(v: &Value, max: usize) -> Option<String> {
    v.as_str().map(|s| s.trim().chars().take(max).collect::<String>()).filter(|s| !s.is_empty())
}

/// Money: non-negative, ≤ 3 decimals, a string or a JSON number.
pub fn money(v: &Value, digits: u32) -> Result<Option<i64>, String> {
    let s = match v {
        Value::Null => return Ok(None),
        Value::String(s) if s.trim().is_empty() => return Ok(None),
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return Err("not a number".into()),
    };
    let s = s.replace(',', "");
    if let Some(p) = s.find('.') {
        if s.len() - p - 1 > digits as usize {
            return Err(format!("{s} has more than {digits} decimals"));
        }
    }
    match crate::money::parse_decimal(&s, digits) {
        Ok(m) if m < 0 => Err(format!("{s} is negative")),
        Ok(m) if m > 100_000_000_000 => Err(format!("{s} is out of range")),
        Ok(m) => Ok(Some(m)),
        Err(_) => Err(format!("{s} is not a decimal amount")),
    }
}

fn qty(v: &Value) -> Result<Option<i64>, String> {
    match money(v, 3)? {
        Some(0) => Err("quantity 0".into()),
        Some(q) if q > 1_000_000_000 => Err("quantity out of range".into()),
        x => Ok(x),
    }
}

fn percent_bp(v: &Value) -> Result<Option<i64>, String> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => {
            let s = n.to_string();
            let bp = crate::money::parse_decimal(&s, 2).map_err(|_| format!("{s} is not a percentage"))?;
            if (0..=10_000).contains(&bp) {
                Ok(Some(bp))
            } else {
                Err(format!("{s}% is out of range"))
            }
        }
        Value::String(s) => percent_bp(&serde_json::from_str::<Value>(s.trim_end_matches('%')).unwrap_or(Value::Bool(false))),
        _ => Err("not a percentage".into()),
    }
}

fn date(v: &Value) -> Result<Option<String>, String> {
    match text(v, 10) {
        None => Ok(None),
        Some(d) => match chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d") {
            Ok(x) if (2000..=2100).contains(&chrono::Datelike::year(&x)) => Ok(Some(d)),
            _ => Err(format!("{d} is not a valid date")),
        },
    }
}

fn ai_field<T>(value: Option<T>, raw: Option<String>) -> Field<T> {
    match value {
        Some(v) => Field { value: Some(v), raw, evidence: None, source: "ai".into(), band: Band::Medium, status: "ok".into(), note: None },
        None => Field::default(),
    }
}

/// Validate a model reply. `allowed_products` = the candidate ids we sent.
pub fn validate(v: &Value, layout: &Layout, digits: u32, allowed_products: &[String]) -> Result<AiExtraction, String> {
    let obj = v.as_object().ok_or("the reply is not a JSON object")?;
    let mut rejected = vec![];
    let mut take_money = |key: &str| -> Option<i64> {
        match money(obj.get(key).unwrap_or(&Value::Null), digits) {
            Ok(x) => x,
            Err(e) => {
                rejected.push(format!("{key}: {e}"));
                None
            }
        }
    };
    let subtotal = take_money("subtotal");
    let vat = take_money("vat");
    let total = take_money("total");
    let classification = match obj.get("doc_type").and_then(|x| x.as_str()) {
        None => None,
        Some(s) => match DocType::parse(s) {
            Some(t) => Some(Classification { doc_type: t, band: Band::Medium, reasons: vec!["AI reading".into()], source: "ai".into() }),
            None => {
                rejected.push(format!("doc_type: '{s}' is not supported"));
                None
            }
        },
    };
    let mut f = DocFields::default();
    let mut d = |key: &str| -> Option<String> {
        match date(obj.get(key).unwrap_or(&Value::Null)) {
            Ok(x) => x,
            Err(e) => {
                rejected.push(format!("{key}: {e}"));
                None
            }
        }
    };
    f.invoice_date = ai_field(d("invoice_date"), None);
    f.due_date = ai_field(d("due_date"), None);
    let vat_id = |k: &str| text(obj.get(k).unwrap_or(&Value::Null), 30).map(|s| super::digits_only(&s)).filter(|s| s.len() == 15);
    f.supplier_name = ai_field(text(obj.get("supplier_name").unwrap_or(&Value::Null), 120), None);
    f.supplier_vat = ai_field(vat_id("supplier_vat"), None);
    f.supplier_cr = ai_field(
        text(obj.get("supplier_cr").unwrap_or(&Value::Null), 20)
            .filter(|s| s.chars().all(|c| c.is_ascii_digit() || c == '-') && s.len() >= 3),
        None,
    );
    f.invoice_number = ai_field(
        text(obj.get("invoice_number").unwrap_or(&Value::Null), 60).filter(|s| s.chars().all(|c| c.is_alphanumeric() || "-/ ".contains(c))),
        None,
    );
    f.subtotal_minor = ai_field(subtotal, None);
    f.vat_minor = ai_field(vat, None);
    f.total_minor = ai_field(total, None);
    match percent_bp(obj.get("vat_rate_percent").unwrap_or(&Value::Null)) {
        Ok(x) => f.vat_rate_bp = ai_field(x, None),
        Err(e) => rejected.push(format!("vat_rate_percent: {e}")),
    }
    let doc_lines = layout.lines();
    let mut lines = vec![];
    let arr = match obj.get("lines") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Err("lines is not a list".into()),
    };
    for (i, l) in arr.iter().take(400).enumerate() {
        let Some(desc) = text(&l["description"], 200) else {
            rejected.push(format!("line {}: no description", i + 1));
            continue;
        };
        let mut errs = vec![];
        let mut m = |k: &str| match money(&l[k], digits) {
            Ok(x) => x,
            Err(e) => {
                errs.push(format!("{k}: {e}"));
                None
            }
        };
        let unit_cost = m("unit_cost");
        let discount = m("discount");
        let line_vat = m("vat");
        let line_total = m("line_total");
        let q = match qty(&l["qty"]) {
            Ok(x) => x,
            Err(e) => {
                errs.push(format!("qty: {e}"));
                None
            }
        };
        let rate = percent_bp(&l["vat_rate_percent"]).unwrap_or_else(|e| {
            errs.push(format!("vat_rate_percent: {e}"));
            None
        });
        let unit = text(&l["unit"], 10).and_then(|u| match u.to_lowercase().as_str() {
            x @ ("pcs" | "ctn" | "pkt" | "box" | "kg") => Some(x.to_string()),
            other => {
                errs.push(format!("unit: '{other}' is not supported"));
                None
            }
        });
        let upc = l["units_per_case"].as_i64().filter(|n| (2..=500).contains(n));
        if !l["units_per_case"].is_null() && upc.is_none() {
            errs.push("units_per_case out of range".into());
        }
        // A barcode given as the item code is still a barcode.
        let code_digits = text(&l["code"], 20).filter(|b| b.chars().all(|c| c.is_ascii_digit()) && matches!(b.len(), 8 | 12 | 13 | 14));
        let barcode = text(&l["barcode"], 20)
            .filter(|b| b.chars().all(|c| c.is_ascii_digit()) && matches!(b.len(), 8 | 12 | 13 | 14))
            .or(code_digits.clone());
        if text(&l["barcode"], 20).is_some() && barcode.is_none() {
            errs.push("barcode is not an 8/12/13/14-digit number".into());
        }
        let product = match text(&l["product_id"], 40) {
            Some(p) if allowed_products.contains(&p) => Some(p),
            Some(p) => {
                errs.push(format!("product_id {p} was not one of the candidates (ignored)"));
                None
            }
            None => None,
        };
        let src = l["src_line"].as_u64().map(|x| x as usize).filter(|x| *x < doc_lines.len());
        let evidence = match src {
            Some(k) => {
                let dl = &doc_lines[k];
                Evidence {
                    page: Some(dl.page),
                    line: Some(k),
                    bbox: dl.line.bbox().and_then(|b| super::layout::norm_box(b, dl.page_w, dl.page_h)),
                    text: Some(dl.line.text().chars().take(300).collect()),
                    ocr_conf: Some(dl.line.conf()),
                }
            }
            None => {
                errs.push("src_line missing or out of range".into());
                Evidence::default()
            }
        };
        for e in &errs {
            rejected.push(format!("line {}: {e}", i + 1));
        }
        if q.is_none() && line_total.is_none() {
            continue;
        }
        let mut pack = Pack { units_per_case: upc, ..Default::default() };
        match (unit.as_deref(), upc, q) {
            (Some("ctn" | "box"), Some(n), Some(qq)) => {
                pack.case_qty_milli = Some(qq);
                pack.base_qty_milli = Some(qq * n);
                pack.clear = true;
            }
            (Some("pcs" | "kg") | None, None, Some(qq)) => {
                pack.base_qty_milli = Some(qq);
                pack.clear = true;
            }
            _ => {}
        }
        let mut flags = vec!["ai_read".to_string()];
        if !pack.clear {
            flags.push("pack_unclear".into());
        }
        lines.push((
            ExtractedLine {
                raw_text: evidence.text.clone().unwrap_or_else(|| desc.clone()),
                description: desc,
                barcode_valid: barcode.as_deref().map(super::gtin_valid),
                barcode,
                supplier_code: text(&l["code"], 40).filter(|c| code_digits.as_deref() != Some(c.as_str())),
                qty_milli: q,
                unit,
                pack,
                unit_cost_minor: unit_cost,
                discount_minor: discount,
                vat_rate_bp: rate,
                vat_minor: line_vat,
                line_total_minor: line_total,
                evidence,
                flags,
            },
            product,
        ));
    }
    Ok(AiExtraction { classification, fields: f, lines, rejected })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strict_validation() {
        let layout = Layout::from_text("Milk 10 0.450 4.500\nBread 2 0.200 0.400", 90);
        let v = json!({
            "doc_type": "invoice", "invoice_number": "INV-9", "invoice_date": "2026-09-31", "total": "4.9001",
            "subtotal": 4.9, "vat_rate_percent": 10,
            "lines": [
                { "src_line": 0, "description": "Milk", "qty": "10", "unit_cost": "0.450", "line_total": "4.500", "product_id": "P1" },
                { "src_line": 7, "description": "Bread", "qty": 0, "unit_cost": "-0.2", "line_total": "0.400", "product_id": "FAKE", "unit": "crate" },
                { "description": "", "qty": "1" }
            ]
        });
        let r = validate(&v, &layout, 3, &["P1".to_string()]).unwrap();
        assert_eq!(r.classification.unwrap().doc_type, DocType::Invoice);
        assert_eq!(r.fields.invoice_date.value, None, "31 Sep is not a date");
        assert_eq!(r.fields.total_minor.value, None, "4 decimals rejected");
        assert_eq!(r.fields.subtotal_minor.value, Some(4_900));
        assert_eq!(r.fields.vat_rate_bp.value, Some(1_000));
        assert_eq!(r.lines.len(), 2);
        assert_eq!(r.lines[0].1.as_deref(), Some("P1"));
        assert_eq!(r.lines[0].0.evidence.line, Some(0));
        assert_eq!(r.lines[1].1, None, "a fabricated product id is dropped");
        assert_eq!(r.lines[1].0.qty_milli, None);
        assert_eq!(r.lines[1].0.unit_cost_minor, None);
        let all = r.rejected.join("\n");
        for s in ["invoice_date", "total", "FAKE", "quantity 0", "negative", "crate", "src_line", "no description"] {
            assert!(all.contains(s), "{s} missing in {all}");
        }
        assert!(validate(&json!("text"), &layout, 3, &[]).is_err());
        assert!(validate(&json!({ "lines": "x" }), &layout, 3, &[]).is_err());
        assert!(validate(&json!({ "doc_type": "receipt" }), &layout, 3, &[]).unwrap().rejected[0].contains("not supported"));
    }
}
