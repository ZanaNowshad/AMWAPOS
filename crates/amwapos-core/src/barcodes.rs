//! Barcode kinds, PLUs and scale barcodes (docs/PRICING_AND_CATALOGUE.md).
//!
//! What a scan means, in order — the first that answers wins:
//! 1. a barcode stored on a product, exactly as scanned;
//! 2. a PLU (price look-up code) typed or scanned, leading zeros ignored;
//! 3. a scale barcode rule (weight or price inside the code). Rules are
//!    looked up by length and prefix (an index, never a scan of every rule);
//!    the highest priority wins, and two rules at the same priority that both
//!    fit the code stop the sale ("fail closed"): nothing is charged.
//! 4. otherwise the code is unknown and recorded for review, as before.
//!
//! Nothing is classified by guessing: a barcode's kind is stored only when
//! the person chose it (barcodes from before Wave 5 keep "not recorded").

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::error::{AppError, AppResult};
use crate::ids::new_id;
use crate::service::AppCore;
use crate::settings;
use crate::time;
use crate::validate;

pub const KINDS: [&str; 7] = ["ean13", "ean8", "upc_a", "upc_e", "code128", "internal", "supplier"];

pub const AMBIGUOUS_RULES: &str = "More than one scale-barcode rule matches this code.";

/// Arabic-Indic and Eastern Arabic-Indic digits become ASCII digits; other
/// characters are kept.
pub fn ascii_digits(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{0660}'..='\u{0669}' => char::from(b'0' + (c as u32 - 0x0660) as u8),
            '\u{06F0}'..='\u{06F9}' => char::from(b'0' + (c as u32 - 0x06F0) as u8),
            _ => c,
        })
        .collect()
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// GS1 check digit (EAN-8, EAN-13, UPC-A, GTIN-14): the last digit checks
/// the others.
pub fn gs1_check_ok(code: &str) -> bool {
    if !all_digits(code) || code.len() < 2 {
        return false;
    }
    let b = code.as_bytes();
    let body = &b[..b.len() - 1];
    let mut sum = 0u32;
    for (i, d) in body.iter().rev().enumerate() {
        let v = (d - b'0') as u32;
        sum += if i % 2 == 0 { v * 3 } else { v };
    }
    (10 - sum % 10) % 10 == (b[b.len() - 1] - b'0') as u32
}

/// The kind the digits look like — a suggestion shown to the person, never
/// stored by itself.
pub fn suggest_kind(code: &str) -> Option<&'static str> {
    if !all_digits(code) {
        return None;
    }
    match code.len() {
        13 if gs1_check_ok(code) => Some("ean13"),
        12 if gs1_check_ok(code) => Some("upc_a"),
        8 if gs1_check_ok(code) => Some("ean8"),
        _ => None,
    }
}

/// A chosen kind must fit the code (an EAN-13 is 13 digits with a valid
/// check digit, and so on).
pub fn validate_kind(code: &str, kind: &str) -> AppResult<()> {
    if !KINDS.contains(&kind) {
        return Err(AppError::validation("Choose a barcode type from the list."));
    }
    let ok = match kind {
        "ean13" => code.len() == 13 && gs1_check_ok(code),
        "ean8" => code.len() == 8 && gs1_check_ok(code),
        "upc_a" => code.len() == 12 && gs1_check_ok(code),
        "upc_e" => code.len() == 8 && all_digits(code),
        "code128" => code.bytes().all(|b| (32..127).contains(&b)),
        _ => true,
    };
    if !ok {
        let what = match kind {
            "ean13" => "An EAN-13 barcode has 13 digits and a valid check digit.",
            "ean8" => "An EAN-8 barcode has 8 digits and a valid check digit.",
            "upc_a" => "A UPC-A barcode has 12 digits and a valid check digit.",
            "upc_e" => "A UPC-E barcode has 8 digits.",
            _ => "This barcode does not fit the chosen type.",
        };
        return Err(AppError::validation(what));
    }
    Ok(())
}

/// A PLU as stored: digits only, no leading zeros, at most 12 digits.
/// Empty → None (no PLU).
pub fn normalize_plu(raw: &str) -> AppResult<Option<String>> {
    let t = ascii_digits(raw.trim());
    if t.is_empty() {
        return Ok(None);
    }
    if !all_digits(&t) {
        return Err(AppError::validation("A PLU has digits only."));
    }
    let stripped = t.trim_start_matches('0');
    if stripped.is_empty() {
        return Err(AppError::validation("A PLU cannot be zero."));
    }
    if stripped.len() > 12 {
        return Err(AppError::validation("A PLU has at most 12 digits."));
    }
    Ok(Some(stripped.to_string()))
}

/// The scanned or typed code read as a PLU, when it can be one.
fn as_plu(code: &str) -> Option<String> {
    if !all_digits(code) || code.len() > 12 {
        return None;
    }
    let s = code.trim_start_matches('0');
    (!s.is_empty()).then(|| s.to_string())
}

/// Another product that already answers to this PLU — by its PLU, or by a
/// barcode that reads as the same number. Each one would make a scan
/// ambiguous, so either is a conflict to resolve first.
pub fn plu_conflict(c: &Connection, plu: &str, except: Option<&str>) -> AppResult<Option<(String, String, &'static str)>> {
    let by_plu: Option<(String, String)> = c
        .query_row("SELECT product_id, name FROM products WHERE plu=?1 AND product_id IS NOT ?2", params![plu, except], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    if let Some((id, name)) = by_plu {
        return Ok(Some((id, name, "plu")));
    }
    let by_barcode: Option<(String, String)> = c
        .query_row(
            "SELECT p.product_id, p.name FROM product_barcodes b JOIN products p ON p.product_id=b.product_id
             WHERE length(b.barcode) <= 12 AND b.barcode NOT GLOB '*[^0-9]*' AND ltrim(b.barcode,'0')=?1 AND p.product_id IS NOT ?2
             LIMIT 1",
            params![plu, except],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(by_barcode.map(|(id, name)| (id, name, "barcode")))
}

/// A barcode being added must not read as another product's PLU.
pub fn barcode_plu_conflict(c: &Connection, barcode: &str, product_id: &str) -> AppResult<()> {
    if let Some(plu) = as_plu(barcode) {
        let other: Option<String> =
            c.query_row("SELECT name FROM products WHERE plu=?1 AND product_id<>?2", params![plu, product_id], |r| r.get(0)).optional()?;
        if let Some(name) = other {
            return Err(AppError::duplicate(format!("Barcode {barcode} reads as PLU {plu}, which belongs to {name}."))
                .with_details(json!({ "barcode": barcode, "plu": plu, "product_name": name })));
        }
    }
    Ok(())
}

/// Set or clear a product's PLU (checked for conflicts).
pub(crate) fn set_plu(c: &Connection, product_id: &str, raw: Option<&str>) -> AppResult<Option<String>> {
    let plu = match raw {
        Some(r) => normalize_plu(r)?,
        None => None,
    };
    if let Some(p) = &plu {
        if let Some((_, name, how)) = plu_conflict(c, p, Some(product_id))? {
            let msg = if how == "plu" {
                format!("PLU {p} is already used by {name}. Choose another PLU or change {name} first.")
            } else {
                format!("PLU {p} is the same number as a barcode of {name}. Choose another PLU.")
            };
            return Err(AppError::duplicate(msg).with_details(json!({ "plu": p, "product_name": name, "conflict": how })));
        }
    }
    c.execute("UPDATE products SET plu=?2 WHERE product_id=?1", params![product_id, plu])?;
    Ok(plu)
}

pub fn plu_owner(c: &Connection, code: &str) -> AppResult<Option<(String, String)>> {
    let Some(plu) = as_plu(code) else { return Ok(None) };
    Ok(c.query_row("SELECT product_id, name FROM products WHERE plu=?1", [plu], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
}

// ------------------------------------------------------------------ scale rules

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaleRule {
    #[serde(default)]
    pub rule_id: String,
    pub name: String,
    pub prefix: String,
    pub length: i64,
    pub item_start: i64,
    pub item_length: i64,
    /// weight | price
    pub value_kind: String,
    pub value_start: i64,
    pub value_length: i64,
    pub decimals: i64,
    /// none | ean
    pub check_digit: String,
    #[serde(default = "yes")]
    pub active: bool,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub updated_at: Option<String>,
}

fn yes() -> bool {
    true
}

/// Rules are checked when they are saved, so a till never meets a rule
/// that cannot be read.
pub fn validate_rule(r: &ScaleRule) -> AppResult<()> {
    let name = r.name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(AppError::validation("Give the rule a name (at most 60 characters)."));
    }
    if !all_digits(&r.prefix) || r.prefix.len() > 6 {
        return Err(AppError::validation("The prefix is 1 to 6 digits."));
    }
    if !(8..=20).contains(&r.length) {
        return Err(AppError::validation("The barcode length is between 8 and 20 digits."));
    }
    if !matches!(r.value_kind.as_str(), "weight" | "price") {
        return Err(AppError::validation("Choose whether the code holds a weight or a price."));
    }
    if !matches!(r.check_digit.as_str(), "none" | "ean") {
        return Err(AppError::validation("Choose the check digit: none or EAN."));
    }
    if !(0..=3).contains(&r.decimals) {
        return Err(AppError::validation("Decimals are between 0 and 3."));
    }
    if !(1..=12).contains(&r.item_length) || !(1..=9).contains(&r.value_length) {
        return Err(AppError::validation("The item code is 1–12 digits and the value 1–9 digits."));
    }
    if r.decimals > r.value_length {
        return Err(AppError::validation("The value has fewer digits than its decimals."));
    }
    // Last usable position: the check digit, when there is one, is the last digit.
    let last = if r.check_digit == "ean" { r.length - 1 } else { r.length };
    let p = r.prefix.len() as i64;
    let item = (r.item_start, r.item_start + r.item_length - 1);
    let value = (r.value_start, r.value_start + r.value_length - 1);
    for (label, (a, b)) in [("item code", item), ("value", value)] {
        if a < 1 || b > last {
            return Err(AppError::validation(format!(
                "The {label} does not fit inside the barcode{}.",
                if r.check_digit == "ean" { " before the check digit" } else { "" }
            )));
        }
        if a <= p {
            return Err(AppError::validation(format!("The {label} overlaps the prefix.")));
        }
    }
    if item.0 <= value.1 && value.0 <= item.1 {
        return Err(AppError::validation("The item code and the value overlap."));
    }
    if r.priority.abs() > 1000 {
        return Err(AppError::validation("Priority is between -1000 and 1000."));
    }
    Ok(())
}

/// What a scale label said, as read by one rule.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScaleRead {
    pub rule_id: String,
    pub rule_name: String,
    /// The item code as a PLU (leading zeros removed).
    pub plu: String,
    pub value_kind: String,
    /// The digits of the value, as an integer (before decimals).
    pub raw_value: i64,
    /// weight: thousandths of the product's unit; price: fils.
    pub value_milli: i64,
}

/// Read `code` with `rule`; None when the code does not fit the rule.
pub fn read_with(rule: &ScaleRule, code: &str) -> Option<ScaleRead> {
    if !rule.active || code.len() as i64 != rule.length || !all_digits(code) || !code.starts_with(&rule.prefix) {
        return None;
    }
    if rule.check_digit == "ean" && !gs1_check_ok(code) {
        return None;
    }
    let seg = |start: i64, len: i64| &code[(start - 1) as usize..(start - 1 + len) as usize];
    let item = seg(rule.item_start, rule.item_length).trim_start_matches('0');
    if item.is_empty() {
        return None;
    }
    let raw: i64 = seg(rule.value_start, rule.value_length).parse().ok()?;
    // Integer conversion: value × 10^(3 − decimals) thousandths (or fils).
    let value_milli = raw.checked_mul(10i64.pow((3 - rule.decimals) as u32))?;
    Some(ScaleRead {
        rule_id: rule.rule_id.clone(),
        rule_name: rule.name.clone(),
        plu: item.to_string(),
        value_kind: rule.value_kind.clone(),
        raw_value: raw,
        value_milli,
    })
}

const RULE_COLS: &str = "rule_id, name, prefix, length, item_start, item_length, value_kind, value_start, value_length, decimals,
    check_digit, active, priority, version, updated_at";

fn map_rule(r: &rusqlite::Row) -> rusqlite::Result<ScaleRule> {
    Ok(ScaleRule {
        rule_id: r.get(0)?,
        name: r.get(1)?,
        prefix: r.get(2)?,
        length: r.get(3)?,
        item_start: r.get(4)?,
        item_length: r.get(5)?,
        value_kind: r.get(6)?,
        value_start: r.get(7)?,
        value_length: r.get(8)?,
        decimals: r.get(9)?,
        check_digit: r.get(10)?,
        active: r.get::<_, i64>(11)? == 1,
        priority: r.get(12)?,
        version: r.get(13)?,
        updated_at: r.get(14)?,
    })
}

pub enum ScaleOutcome {
    /// No rule fits the code.
    NoRule,
    /// One rule (the highest priority) read it.
    Read(ScaleRead),
    /// Two or more rules at the highest priority fit: fail closed.
    Ambiguous(Vec<String>),
}

/// The rules that could read `code`: only those with its exact length and
/// one of its first 1–6 digits as prefix (an index lookup).
pub fn resolve_scale(c: &Connection, code: &str) -> AppResult<ScaleOutcome> {
    if !all_digits(code) || !(8..=20).contains(&code.len()) {
        return Ok(ScaleOutcome::NoRule);
    }
    let prefixes: Vec<String> = (1..=6.min(code.len())).map(|n| code[..n].to_string()).collect();
    let mut st = c.prepare_cached(&format!(
        "SELECT {RULE_COLS} FROM scale_barcode_rules WHERE active=1 AND length=?1 AND prefix IN (?2,?3,?4,?5,?6,?7)
         ORDER BY priority DESC, rule_id"
    ))?;
    let pad = |i: usize| prefixes.get(i).cloned().unwrap_or_default();
    let rules = st
        .query_map(params![code.len() as i64, pad(0), pad(1), pad(2), pad(3), pad(4), pad(5)], map_rule)?
        .collect::<Result<Vec<_>, _>>()?;
    let reads: Vec<(i64, ScaleRead)> = rules.iter().filter_map(|r| read_with(r, code).map(|x| (r.priority, x))).collect();
    let Some(top) = reads.first().map(|x| x.0) else { return Ok(ScaleOutcome::NoRule) };
    let best: Vec<&(i64, ScaleRead)> = reads.iter().filter(|x| x.0 == top).collect();
    if best.len() > 1 {
        return Ok(ScaleOutcome::Ambiguous(best.iter().map(|x| x.1.rule_name.clone()).collect()));
    }
    Ok(ScaleOutcome::Read(best[0].1.clone()))
}

/// Upper limits for what a scale label may say (POS settings).
pub fn check_bounds(read: &ScaleRead, pos: &settings::PosSettings) -> AppResult<()> {
    if read.value_milli <= 0 {
        return Err(AppError::validation("The scale label shows zero. Weigh the item again."));
    }
    match read.value_kind.as_str() {
        "weight" if read.value_milli > pos.scale_max_weight_milli => {
            Err(AppError::validation("The weight on this scale label is above the allowed limit. Weigh the item again or ask a manager."))
        }
        "price" if read.value_milli > pos.scale_max_price_minor => {
            Err(AppError::validation("The price on this scale label is above the allowed limit. Check the label or ask a manager."))
        }
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScaleTest {
    /// barcode | plu | scale | ambiguous | unknown
    pub outcome: String,
    pub message: String,
    pub product_id: Option<String>,
    pub product_name: Option<String>,
    pub read: Option<ScaleRead>,
    pub rules: Vec<String>,
}

impl AppCore {
    pub fn scale_rules_list(&self, token: &str) -> AppResult<Vec<ScaleRule>> {
        let s = self.session(token)?;
        if !s.has("barcode_rules.manage") && !s.has("products.view") {
            return Err(AppError::forbidden("barcode_rules.manage"));
        }
        self.db.read(|c| {
            let mut st = c.prepare(&format!("SELECT {RULE_COLS} FROM scale_barcode_rules ORDER BY active DESC, priority DESC, name"))?;
            let rows = st.query_map([], map_rule)?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Create or change a rule (hub only; replicated to every till).
    pub fn scale_rule_save(&self, token: &str, rule: ScaleRule) -> AppResult<ScaleRule> {
        let s = self.session(token)?;
        s.require("barcode_rules.manage")?;
        self.require_back_office_writable()?;
        validate_rule(&rule)?;
        let actor = self.actor(&s, None);
        let now = time::now_str();
        let id = self.db.write(|tx| {
            let name = rule.name.trim().to_string();
            if rule.rule_id.is_empty() {
                let id = new_id();
                tx.execute(
                    "INSERT INTO scale_barcode_rules(rule_id, name, prefix, length, item_start, item_length, value_kind, value_start,
                        value_length, decimals, check_digit, active, priority, created_by, created_at, updated_at, version)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15,1)",
                    params![
                        id,
                        name,
                        rule.prefix,
                        rule.length,
                        rule.item_start,
                        rule.item_length,
                        rule.value_kind,
                        rule.value_start,
                        rule.value_length,
                        rule.decimals,
                        rule.check_digit,
                        rule.active as i64,
                        rule.priority,
                        s.user_id,
                        now
                    ],
                )?;
                audit::record(tx, &actor, "scale_rule.created", "scale_rule", Some(&id), None, Some(&json!(rule)))?;
                Ok(id)
            } else {
                let id = validate::id(&rule.rule_id, "Rule")?;
                let before: ScaleRule = tx
                    .query_row(&format!("SELECT {RULE_COLS} FROM scale_barcode_rules WHERE rule_id=?1"), [&id], map_rule)
                    .optional()?
                    .ok_or_else(|| AppError::not_found("Scale barcode rule"))?;
                if before.version != rule.version {
                    return Err(AppError::conflict("This rule was changed by someone else. Reload it and try again."));
                }
                tx.execute(
                    "UPDATE scale_barcode_rules SET name=?2, prefix=?3, length=?4, item_start=?5, item_length=?6, value_kind=?7,
                        value_start=?8, value_length=?9, decimals=?10, check_digit=?11, active=?12, priority=?13, updated_at=?14,
                        version=version+1 WHERE rule_id=?1",
                    params![
                        id,
                        name,
                        rule.prefix,
                        rule.length,
                        rule.item_start,
                        rule.item_length,
                        rule.value_kind,
                        rule.value_start,
                        rule.value_length,
                        rule.decimals,
                        rule.check_digit,
                        rule.active as i64,
                        rule.priority,
                        now
                    ],
                )?;
                audit::record(tx, &actor, "scale_rule.changed", "scale_rule", Some(&id), Some(&json!(before)), Some(&json!(rule)))?;
                Ok(id)
            }
        })?;
        self.db.read(|c| Ok(c.query_row(&format!("SELECT {RULE_COLS} FROM scale_barcode_rules WHERE rule_id=?1"), [&id], map_rule)?))
    }

    /// Try a code against the catalogue and the rules without selling it.
    pub fn scale_rule_test(&self, token: &str, code: &str) -> AppResult<ScaleTest> {
        let s = self.session(token)?;
        if !s.has("barcode_rules.manage") && !s.has("products.view") {
            return Err(AppError::forbidden("barcode_rules.manage"));
        }
        let code = validate::barcode(&ascii_digits(code))?;
        self.db.read(|c| {
            if let Some((pid, name)) = crate::catalog::barcode_owner(c, &code)? {
                return Ok(ScaleTest {
                    outcome: "barcode".into(),
                    message: format!("Exact barcode of {name}: the barcode wins over any rule."),
                    product_id: Some(pid),
                    product_name: Some(name),
                    read: None,
                    rules: vec![],
                });
            }
            if let Some((pid, name)) = plu_owner(c, &code)? {
                return Ok(ScaleTest {
                    outcome: "plu".into(),
                    message: format!("PLU of {name}."),
                    product_id: Some(pid),
                    product_name: Some(name),
                    read: None,
                    rules: vec![],
                });
            }
            match resolve_scale(c, &code)? {
                ScaleOutcome::NoRule => Ok(ScaleTest {
                    outcome: "unknown".into(),
                    message: "No product barcode, PLU or scale rule matches this code.".into(),
                    product_id: None,
                    product_name: None,
                    read: None,
                    rules: vec![],
                }),
                ScaleOutcome::Ambiguous(names) => Ok(ScaleTest {
                    outcome: "ambiguous".into(),
                    message: AMBIGUOUS_RULES.into(),
                    product_id: None,
                    product_name: None,
                    read: None,
                    rules: names,
                }),
                ScaleOutcome::Read(read) => {
                    let owner: Option<(String, String)> = c
                        .query_row("SELECT product_id, name FROM products WHERE plu=?1", [&read.plu], |r| Ok((r.get(0)?, r.get(1)?)))
                        .optional()?;
                    let message = match &owner {
                        Some((_, n)) => format!("Rule {}: {n}.", read.rule_name),
                        None => format!("Rule {}: no product has PLU {}.", read.rule_name, read.plu),
                    };
                    Ok(ScaleTest {
                        outcome: "scale".into(),
                        message,
                        product_id: owner.as_ref().map(|o| o.0.clone()),
                        product_name: owner.map(|o| o.1),
                        rules: vec![read.rule_name.clone()],
                        read: Some(read),
                    })
                }
            }
        })
    }

    /// Set or clear a product's PLU.
    pub fn product_set_plu(&self, token: &str, product_id: &str, plu: Option<String>) -> AppResult<crate::catalog::ProductDetail> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let pid = validate::id(product_id, "Product")?;
        self.db.write(|tx| {
            let before: Option<String> = tx
                .query_row("SELECT plu FROM products WHERE product_id=?1", [&pid], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Product"))?;
            let after = set_plu(tx, &pid, plu.as_deref().filter(|p| !p.trim().is_empty()))?;
            if before != after {
                tx.execute("UPDATE products SET updated_at=?2, version=version+1 WHERE product_id=?1", params![pid, time::now_str()])?;
                audit::record(
                    tx,
                    &actor,
                    "product.plu_changed",
                    "product",
                    Some(&pid),
                    Some(&json!({ "plu": before })),
                    Some(&json!({ "plu": after })),
                )?;
            }
            Ok(())
        })?;
        self.product_get(token, &pid)
    }

    /// Record the kind of a barcode (chosen by the person).
    pub fn barcode_set_kind(&self, token: &str, barcode_id: &str, kind: Option<String>) -> AppResult<crate::catalog::ProductDetail> {
        let s = self.session(token)?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let bid = validate::id(barcode_id, "Barcode")?;
        let pid = self.db.write(|tx| {
            let (pid, code, before): (String, String, Option<String>) = tx
                .query_row("SELECT product_id, barcode, kind FROM product_barcodes WHERE barcode_id=?1", [&bid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?
                .ok_or_else(|| AppError::not_found("Barcode"))?;
            let kind = kind.filter(|k| !k.is_empty());
            if let Some(k) = &kind {
                validate_kind(&code, k)?;
            }
            tx.execute("UPDATE product_barcodes SET kind=?2 WHERE barcode_id=?1", params![bid, kind])?;
            audit::record(
                tx,
                &actor,
                "barcode.kind_set",
                "product",
                Some(&pid),
                Some(&json!({ "barcode": code, "kind": before })),
                Some(&json!({ "barcode": code, "kind": kind })),
            )?;
            Ok(pid)
        })?;
        self.product_get(token, &pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(kind: &str) -> ScaleRule {
        ScaleRule {
            rule_id: "r1".into(),
            name: "Deli".into(),
            prefix: "21".into(),
            length: 13,
            item_start: 3,
            item_length: 5,
            value_kind: kind.into(),
            value_start: 8,
            value_length: 5,
            decimals: 3,
            check_digit: "ean".into(),
            active: true,
            priority: 0,
            version: 1,
            updated_at: None,
        }
    }

    fn with_check(body: &str) -> String {
        (0..10).map(|d| format!("{body}{d}")).find(|c| gs1_check_ok(c)).unwrap()
    }

    #[test]
    fn check_digits() {
        assert!(gs1_check_ok("4006381333931"));
        assert!(!gs1_check_ok("4006381333932"));
        assert!(gs1_check_ok("96385074"));
        assert_eq!(suggest_kind("4006381333931"), Some("ean13"));
        assert_eq!(suggest_kind("4006381333932"), None);
        assert!(validate_kind("4006381333932", "ean13").is_err());
        assert!(validate_kind("ABC-1", "internal").is_ok());
    }

    #[test]
    fn plu_normalization() {
        assert_eq!(normalize_plu(" 0042 ").unwrap(), Some("42".into()));
        assert_eq!(normalize_plu("٤٢").unwrap(), Some("42".into()));
        assert_eq!(normalize_plu("").unwrap(), None);
        assert!(normalize_plu("000").is_err());
        assert!(normalize_plu("4a").is_err());
        assert!(normalize_plu("1234567890123").is_err());
    }

    #[test]
    fn rule_validation() {
        assert!(validate_rule(&rule("weight")).is_ok());
        let mut r = rule("weight");
        r.value_start = 6; // overlaps the item code
        assert!(validate_rule(&r).is_err());
        let mut r = rule("weight");
        r.value_length = 6; // runs into the check digit
        assert!(validate_rule(&r).is_err());
        let mut r = rule("weight");
        r.item_start = 2; // overlaps the prefix
        assert!(validate_rule(&r).is_err());
        let mut r = rule("weight");
        r.check_digit = "none".into();
        r.value_length = 6; // fine without a check digit
        assert!(validate_rule(&r).is_ok());
    }

    #[test]
    fn weight_and_price_reads_are_integer() {
        let code = with_check("210004201250"); // item 00042, value 01250
        let w = read_with(&rule("weight"), &code).unwrap();
        assert_eq!((w.plu.as_str(), w.raw_value, w.value_milli), ("42", 1250, 1250)); // 1.250 kg
        let mut r = rule("price");
        r.decimals = 2;
        let p = read_with(&r, &code).unwrap();
        assert_eq!(p.value_milli, 12_500); // 12.50 → 12.500 BHD in fils
                                           // A wrong check digit is not read.
        let bad = format!("{}{}", &code[..12], (code.as_bytes()[12] - b'0' + 1) % 10);
        assert!(read_with(&rule("weight"), &bad).is_none());
    }
}
