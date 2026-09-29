//! Supplier and product matching against the real AMWAPOS records.
//!
//! Strong identifiers win over names: a VAT or CR number beats a name, a
//! barcode or a confirmed supplier mapping beats a description. Every result
//! is a list of real candidates (ids read from the database) with a score,
//! reasons and a band; ties are left for a person rather than guessed.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::{digits_only, norm_name, Band, DocFields, ExtractedLine};
use crate::error::AppResult;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    pub score: i64,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MatchResult {
    pub id: Option<String>,
    pub name: Option<String>,
    /// barcode | supplier_map | supplier_code | sku | name | fuzzy | vat | cr | alias | phone | person | ai | none
    pub kind: String,
    pub score: i64,
    pub band: Band,
    pub reasons: Vec<String>,
    pub alternatives: Vec<Candidate>,
}

impl MatchResult {
    pub fn none(alternatives: Vec<Candidate>) -> Self {
        MatchResult { id: None, name: None, kind: "none".into(), score: 0, band: Band::Unresolved, reasons: vec![], alternatives }
    }
}

// ---------------------------------------------------------------- supplier

/// id, name, VAT, CR, phone, WhatsApp
type SupplierRow = (String, String, Option<String>, Option<String>, Option<String>, Option<String>);

fn add(cands: &mut Vec<(Candidate, &'static str)>, id: String, name: String, score: i64, reason: String, kind: &'static str) {
    if let Some((c, k)) = cands.iter_mut().find(|(c, _)| c.id == id) {
        if score > c.score {
            c.score = score;
            *k = kind;
        }
        c.reasons.push(reason);
    } else {
        cands.push((Candidate { id, name, score, reasons: vec![reason] }, kind));
    }
}

/// Match the document's supplier. `preset` = a supplier a person chose at upload.
pub fn match_supplier(c: &Connection, f: &DocFields, preset: Option<&str>) -> AppResult<MatchResult> {
    let mut cands: Vec<(Candidate, &'static str)> = vec![];
    let suppliers: Vec<SupplierRow> = {
        let mut st = c.prepare("SELECT supplier_id, name, vat_number, cr_number, phone, whatsapp FROM suppliers WHERE active=1")?;
        let rows =
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let vat = f.supplier_vat.value.as_deref().map(digits_only).filter(|v| v.len() >= 8);
    let cr = f.supplier_cr.value.as_deref().map(digits_only).filter(|v| v.len() >= 3);
    let phone = f.supplier_phone.value.as_deref().map(digits_only).filter(|v| v.len() >= 8);
    let name = f.supplier_name.value.as_deref().map(norm_name).filter(|n| !n.is_empty());
    for (id, sname, svat, scr, sphone, swa) in &suppliers {
        if let (Some(v), Some(sv)) = (&vat, svat) {
            if digits_only(sv) == *v {
                add(&mut cands, id.clone(), sname.clone(), 99, format!("VAT number {v} matches"), "vat");
            }
        }
        if let (Some(x), Some(sx)) = (&cr, scr) {
            if digits_only(sx) == *x {
                add(
                    &mut cands,
                    id.clone(),
                    sname.clone(),
                    96,
                    format!("CR number {} matches", f.supplier_cr.value.clone().unwrap_or_default()),
                    "cr",
                );
            }
        }
        if let Some(n) = &name {
            let sn = norm_name(sname);
            if !sn.is_empty() && sn == *n {
                add(&mut cands, id.clone(), sname.clone(), 88, "Name matches".into(), "name");
            } else {
                let s = crate::ocrflow::name_score(n, &sn);
                if s >= 50 {
                    add(&mut cands, id.clone(), sname.clone(), (s * 75 / 100).min(80), format!("Name is similar ({s}%)"), "fuzzy");
                }
            }
        }
        if let Some(p) = &phone {
            for sp in [sphone, swa].into_iter().flatten() {
                let d = digits_only(sp);
                if d.len() >= 8 && d.ends_with(p.as_str()) {
                    add(&mut cands, id.clone(), sname.clone(), 80, "Telephone matches".into(), "phone");
                }
            }
        }
    }
    // Confirmed aliases (learned from earlier reviews).
    let mut alias = |key: Option<String>, kind: &str, score: i64| -> AppResult<()> {
        if let Some(k) = key {
            let hit: Option<(String, String)> = c
                .query_row(
                    "SELECT a.supplier_id, s.name FROM supplier_aliases a JOIN suppliers s ON s.supplier_id=a.supplier_id WHERE a.alias_norm=?1 AND a.kind=?2 AND s.active=1",
                    params![k, kind],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((id, n)) = hit {
                add(&mut cands, id, n, score, format!("Confirmed earlier as this supplier ({kind})"), "alias");
            }
        }
        Ok(())
    };
    alias(vat.clone(), "vat", 98)?;
    alias(cr.clone(), "cr", 95)?;
    alias(name.clone(), "name", 92)?;
    alias(phone.clone(), "phone", 82)?;
    if let Some(p) = preset {
        let n: Option<String> = c.query_row("SELECT name FROM suppliers WHERE supplier_id=?1", [p], |r| r.get(0)).optional()?;
        if let Some(n) = n {
            add(&mut cands, p.to_string(), n, 100, "Chosen at upload".into(), "person");
        }
    }
    cands.sort_by(|a, b| b.0.score.cmp(&a.0.score).then(a.0.name.cmp(&b.0.name)));
    let Some((best, kind)) = cands.first().cloned() else { return Ok(MatchResult::none(vec![])) };
    let alternatives: Vec<Candidate> = cands.iter().skip(1).take(4).map(|x| x.0.clone()).collect();
    // Two different suppliers with near-equal evidence: leave it to a person.
    let tie = cands.get(1).is_some_and(|(x, _)| best.score - x.score < 5 && x.score >= 80);
    let band = if tie { Band::Low } else { Band::from_score(best.score) };
    Ok(MatchResult {
        id: Some(best.id.clone()),
        name: Some(best.name.clone()),
        kind: kind.into(),
        score: best.score,
        band,
        reasons: best.reasons.clone(),
        alternatives,
    })
}

// ---------------------------------------------------------------- products

const ABBREV: &[(&str, &str)] = &[
    ("coke", "coca cola"),
    ("cocacola", "coca cola"),
    ("reg", "original regular"),
    ("orig", "original"),
    ("btl", "bottle"),
    ("ff", "full fat"),
    ("lf", "low fat"),
    ("choc", "chocolate"),
    ("straw", "strawberry"),
    ("ltr", "l"),
    ("lt", "l"),
    ("gm", "g"),
    ("gms", "g"),
    ("kgs", "kg"),
    ("pcs", ""),
    ("pc", ""),
    ("ctn", ""),
    ("x", ""),
];

/// Size in the smallest unit: (value, "ml" | "g").
pub fn size_of(text: &str) -> Option<(i64, &'static str)> {
    let t = crate::ocrflow::normalize_digits(text).to_lowercase().replace(',', ".");
    let words: Vec<&str> = t.split(|c: char| c.is_whitespace() || c == 'x' || c == '×').filter(|w| !w.is_empty()).collect();
    for (i, w) in words.iter().enumerate() {
        let num_end = w.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(w.len());
        let (n, mut u) = w.split_at(num_end);
        if n.is_empty() {
            continue;
        }
        if u.is_empty() {
            u = words.get(i + 1).copied().unwrap_or("");
        }
        let v: f64 = match n.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let r = match u.trim_matches('.') {
            "ml" => Some(((v).round() as i64, "ml")),
            "cl" => Some(((v * 10.0).round() as i64, "ml")),
            "l" | "ltr" | "lt" | "litre" | "liter" => Some(((v * 1000.0).round() as i64, "ml")),
            "g" | "gm" | "gms" | "gr" => Some(((v).round() as i64, "g")),
            "kg" | "kgs" => Some(((v * 1000.0).round() as i64, "g")),
            "مل" => Some(((v).round() as i64, "ml")),
            "لتر" => Some(((v * 1000.0).round() as i64, "ml")),
            "جم" | "غرام" => Some(((v).round() as i64, "g")),
            _ => None,
        };
        if r.is_some() {
            return r;
        }
    }
    None
}

fn words_of(text: &str) -> Vec<String> {
    let t = crate::ocrflow::normalize_digits(text).to_lowercase();
    let mut out = vec![];
    for w in t.split(|c: char| !c.is_alphanumeric()) {
        if w.is_empty() {
            continue;
        }
        // Size tokens are compared separately.
        if w.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        match ABBREV.iter().find(|(a, _)| *a == w) {
            Some((_, e)) => out.extend(e.split_whitespace().map(|x| x.to_string())),
            None if w.chars().count() >= 2 => out.push(w.to_string()),
            None => {}
        }
    }
    out
}

/// Similarity 0..100 between a supplier description and a product name.
pub fn product_score(desc: &str, name: &str, name_ar: Option<&str>) -> (i64, Vec<String>) {
    let d = words_of(desc);
    let mut best = (0i64, vec![]);
    for n in [Some(name), name_ar].into_iter().flatten() {
        let p = words_of(n);
        if d.is_empty() || p.is_empty() {
            continue;
        }
        let ds: std::collections::BTreeSet<&String> = d.iter().collect();
        let ps: std::collections::BTreeSet<&String> = p.iter().collect();
        let inter = ds.intersection(&ps).count() as i64;
        // Share of the product's words found in the description matters most.
        let cover = inter * 100 / ps.len() as i64;
        let dice = inter * 200 / (ds.len() + ps.len()) as i64;
        let mut s = (cover * 6 + dice * 4) / 10;
        let mut why = vec![];
        if inter > 0 {
            why.push(format!("{inter} of {} name words match", ps.len()));
        }
        if p.first().is_some_and(|b| ds.contains(b)) {
            s += 8;
            why.push("brand matches".into());
        }
        match (size_of(desc), size_of(n)) {
            (Some(a), Some(b)) if a == b => {
                s += 12;
                why.push("size matches".into());
            }
            (Some(_), Some(_)) => {
                s -= 40;
                why.push("different size".into());
            }
            _ => {}
        }
        let s = s.clamp(0, 100);
        if s > best.0 {
            best = (s, why);
        }
    }
    best
}

/// The key under which a confirmed supplier line → product mapping is kept.
pub fn desc_key(desc: &str) -> String {
    words_of(desc).join(" ")
}

pub fn code_key(code: &str) -> String {
    super::norm_doc_number(code)
}

/// Match one line to the catalogue.
pub fn match_line(c: &Connection, supplier: Option<&str>, l: &ExtractedLine) -> AppResult<(MatchResult, Option<i64>)> {
    let mut cands: Vec<(Candidate, &'static str)> = vec![];
    let mut learned_upc: Option<i64> = None;
    let pname = |pid: &str| -> AppResult<Option<String>> {
        Ok(c.query_row("SELECT name FROM products WHERE product_id=?1 AND active=1", [pid], |r| r.get(0)).optional()?)
    };
    if let Some(b) = &l.barcode {
        let hit: Option<(String, String)> = c
            .query_row(
                "SELECT p.product_id, p.name FROM product_barcodes b JOIN products p ON p.product_id=b.product_id WHERE b.barcode=?1 AND p.active=1",
                [b],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((id, n)) = hit {
            let valid = l.barcode_valid == Some(true);
            add(
                &mut cands,
                id,
                n,
                if valid { 100 } else { 94 },
                format!("Barcode {b} matches{}", if valid { "" } else { " (check digit not standard)" }),
                "barcode",
            );
        }
    }
    if let Some(sid) = supplier {
        let mut keys = vec![];
        if let Some(code) = &l.supplier_code {
            keys.push(("code", code_key(code), 97i64));
        }
        if let Some(b) = &l.barcode {
            keys.push(("code", code_key(b), 97));
        }
        let dk = desc_key(&l.description);
        if !dk.is_empty() {
            keys.push(("desc", dk, 93));
        }
        for (kind, key, score) in keys {
            let hit: Option<(String, Option<i64>)> = c
                .query_row(
                    "SELECT product_id, units_per_case FROM supplier_product_map WHERE supplier_id=?1 AND key_kind=?2 AND key_norm=?3",
                    params![sid, kind, key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((pid, upc)) = hit {
                if let Some(n) = pname(&pid)? {
                    learned_upc = learned_upc.or(upc);
                    add(
                        &mut cands,
                        pid,
                        n,
                        score,
                        format!("Confirmed earlier for this supplier ({})", if kind == "code" { "item code" } else { "description" }),
                        "supplier_map",
                    );
                }
            }
        }
    }
    if let Some(code) = &l.supplier_code {
        let hit: Option<(String, String)> = c
            .query_row("SELECT product_id, name FROM products WHERE sku=?1 COLLATE NOCASE AND active=1", [code], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        if let Some((id, n)) = hit {
            add(&mut cands, id, n, 90, format!("Item code {code} is the product SKU"), "sku");
        }
    }
    // Name candidates: products sharing a significant word.
    let words = words_of(&l.description);
    let mut seen = std::collections::BTreeSet::new();
    let dnorm = norm_name(&l.description);
    for w in words.iter().filter(|w| w.chars().count() >= 3).take(6) {
        let like = format!("%{}%", w.replace(['%', '_'], ""));
        let mut st = c.prepare(
            "SELECT product_id, name, name_ar FROM products WHERE active=1 AND (lower(name) LIKE ?1 OR name_ar LIKE ?1) LIMIT 60",
        )?;
        let rows = st.query_map([&like], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))?;
        for row in rows {
            let (id, n, ar) = row?;
            if !seen.insert(id.clone()) {
                continue;
            }
            let same_words = |a: &str| {
                let mut x: Vec<&str> = a.split_whitespace().collect();
                let mut y: Vec<&str> = dnorm.split_whitespace().collect();
                x.sort_unstable();
                y.sort_unstable();
                !x.is_empty() && x == y
            };
            if same_words(&norm_name(&n)) || ar.as_deref().map(norm_name).is_some_and(|a| same_words(&a)) {
                add(&mut cands, id, n, 92, "Name matches exactly".into(), "name");
                continue;
            }
            let (s, why) = product_score(&l.description, &n, ar.as_deref());
            if s >= 35 {
                add(&mut cands, id, n, s.min(85), why.join(", "), "fuzzy");
            }
        }
    }
    cands.sort_by(|a, b| b.0.score.cmp(&a.0.score).then(a.0.name.cmp(&b.0.name)));
    let alternatives: Vec<Candidate> = cands.iter().skip(1).take(4).map(|x| x.0.clone()).collect();
    let Some((best, kind)) = cands.first().cloned() else { return Ok((MatchResult::none(vec![]), None)) };
    if best.score < 40 {
        return Ok((MatchResult::none(cands.iter().take(5).map(|x| x.0.clone()).collect()), None));
    }
    let second = cands.get(1).map(|x| x.0.score).unwrap_or(0);
    let mut band = Band::from_score(best.score);
    // Fuzzy names are never "high"; a close runner-up makes it low.
    if kind == "fuzzy" {
        band = band.min(Band::Medium);
        if best.score - second < 10 {
            band = Band::Low;
        }
    }
    Ok((
        MatchResult {
            id: Some(best.id.clone()),
            name: Some(best.name.clone()),
            kind: kind.into(),
            score: best.score,
            band,
            reasons: best.reasons.clone(),
            alternatives,
        },
        learned_upc,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size_of("Coca Cola 330 ml"), Some((330, "ml")));
        assert_eq!(size_of("COKE REG 24x330ML"), Some((330, "ml")));
        assert_eq!(size_of("Water 1.5L"), Some((1500, "ml")));
        assert_eq!(size_of("Rice 5kg"), Some((5000, "g")));
        assert_eq!(size_of("Milk"), None);
    }

    #[test]
    fn scores() {
        let (a, _) = product_score("COKE REG 330ML X24", "Coca-Cola Original 330 ml", None);
        let (b, _) = product_score("COKE REG 330ML X24", "Coca-Cola Original 1.5 L", None);
        let (c, _) = product_score("COKE REG 330ML X24", "Pepsi 330 ml", None);
        assert!(a >= 70, "{a}");
        assert!(a > b + 30, "{a} vs {b}: size decides");
        assert!(c < 40, "{c}");
    }
}
