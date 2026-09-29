//! Resolve customer wording to real catalogue products, with current price
//! and stock. A mention becomes `resolved` only when one real product clearly
//! fits; otherwise it is `ambiguous` with real options and a question, or
//! `unmatched`. Nothing here invents a product, a price or availability.

#![allow(clippy::type_complexity)]

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::interpret::{normalize, size_token, Mention, LEXICON};
use crate::error::AppResult;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cand {
    pub product_id: String,
    pub name: String,
    pub name_ar: Option<String>,
    pub price_minor: Option<i64>,
    pub stock_milli: Option<i64>,
    /// available | low | unavailable | unknown
    pub availability: String,
    pub size: Option<(i64, String)>,
    pub score: i64,
    pub reasons: Vec<String>,
    pub category_id: Option<String>,
    pub allow_decimal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Resolution {
    /// resolved | ambiguous | unmatched
    pub status: String,
    pub product: Option<Cand>,
    pub options: Vec<Cand>,
    /// high | medium | low
    pub band: String,
    pub reasons: Vec<String>,
}

/// Words of a product name for comparison (lexicon-expanded, sizes removed).
pub fn product_words(name: &str) -> Vec<String> {
    let t = normalize(name);
    let mut out = vec![];
    for w in t.split(|c: char| !c.is_alphanumeric() && c != '\'') {
        let w = w.trim_matches('\'');
        if w.is_empty()
            || w.chars().next().is_some_and(|c| c.is_ascii_digit())
            || matches!(w, "ml" | "l" | "ltr" | "g" | "kg" | "gm" | "pcs" | "x" | "pack")
        {
            continue;
        }
        let w2 = w.trim_start_matches("ال");
        let w2 = if w2.is_empty() { w } else { w2 };
        match LEXICON.iter().find(|(k, _)| *k == w2 || *k == w) {
            Some((_, e)) => out.extend(e.split_whitespace().map(|x| x.to_string())),
            None => out.push(w2.to_string()),
        }
    }
    out.dedup();
    out
}

pub fn product_size(name: &str) -> Option<(i64, String)> {
    let t = normalize(name).replace(',', ".");
    let words: Vec<&str> = t.split(|c: char| c.is_whitespace() || c == 'x' || c == '×').filter(|w| !w.is_empty()).collect();
    for (i, w) in words.iter().enumerate() {
        if let Some(s) = size_token(w, words.get(i + 1).copied()) {
            return Some(s);
        }
    }
    None
}

pub fn fmt_size(s: &(i64, String)) -> String {
    match s.1.as_str() {
        "ml" if s.0 >= 1000 => format!("{} L", crate::money::format_decimal(s.0, 3).trim_end_matches('0').trim_end_matches('.')),
        "ml" => format!("{} ml", s.0),
        _ if s.0 >= 1000 => format!("{} kg", crate::money::format_decimal(s.0, 3).trim_end_matches('0').trim_end_matches('.')),
        _ => format!("{} g", s.0),
    }
}

/// Current stock state of a product in the branch.
pub fn availability(c: &Connection, product_id: &str, branch_id: &str) -> AppResult<(Option<i64>, String)> {
    let row: Option<(i64, i64, Option<i64>)> = c
        .query_row(
            "SELECT p.track_inventory, p.reorder_point_milli, (SELECT qty_milli FROM stock_levels s WHERE s.product_id=p.product_id AND s.branch_id=?2)
             FROM products p WHERE p.product_id=?1",
            params![product_id, branch_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(match row {
        None => (None, "unavailable".into()),
        Some((0, _, _)) => (None, "unknown".into()),
        Some((_, rp, q)) => {
            let q = q.unwrap_or(0);
            let st = if q <= 0 {
                "unavailable"
            } else if q <= rp {
                "low"
            } else {
                "available"
            };
            (Some(q), st.into())
        }
    })
}

fn load(c: &Connection, pid: &str, branch: &str) -> AppResult<Option<Cand>> {
    let sql = format!(
        "SELECT p.product_id, p.name, p.name_ar, {}, p.category_id, p.allow_decimal_quantity FROM products p WHERE p.product_id=?1 AND p.active=1",
        crate::catalog::PRICE_SQL
    );
    let row: Option<(String, String, Option<String>, Option<i64>, Option<String>, i64)> =
        c.query_row(&sql, [pid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).optional()?;
    let Some((id, name, ar, price, cat, dec)) = row else { return Ok(None) };
    let (stock, av) = availability(c, &id, branch)?;
    Ok(Some(Cand {
        size: product_size(&name).or_else(|| ar.as_deref().and_then(product_size)),
        product_id: id,
        name,
        name_ar: ar,
        price_minor: price,
        stock_milli: stock,
        availability: av,
        score: 0,
        reasons: vec![],
        category_id: cat,
        allow_decimal: dec == 1,
    }))
}

pub fn product(c: &Connection, pid: &str, branch: &str) -> AppResult<Option<Cand>> {
    load(c, pid, branch)
}

/// The phrase a person may confirm as an alias ("coke big" → a product).
pub fn alias_key(m: &Mention) -> String {
    let mut parts = m.words.clone();
    if let Some(s) = &m.size_word {
        parts.push(s.clone());
    }
    if let Some(s) = &m.size {
        parts.push(format!("{}{}", s.0, s.1));
    }
    parts.join(" ")
}

const VARIANT_MARKERS: &[&str] = &["zero", "diet", "light", "lite", "sugarfree", "free", "decaf", "max", "زيرو", "دايت", "لايت"];

fn score(m: &Mention, name: &str, ar: Option<&str>) -> (i64, Vec<String>) {
    let mw: std::collections::BTreeSet<&str> = m.words.iter().map(|x| x.as_str()).collect();
    let mut best = (0i64, vec![]);
    for n in [Some(name), ar].into_iter().flatten() {
        let pw_v = product_words(n);
        let pw: std::collections::BTreeSet<&str> = pw_v.iter().map(|x| x.as_str()).collect();
        if pw.is_empty() || mw.is_empty() {
            continue;
        }
        // Prefix matches count ("choc" ~ "chocolate", "straw" ~ "strawberry").
        let hit = |w: &str| {
            pw.contains(w) || (w.chars().count() >= 4 && pw.iter().any(|p| p.starts_with(w) || w.starts_with(*p) && p.chars().count() >= 4))
        };
        let inter = mw.iter().filter(|w| hit(w)).count() as i64;
        if inter == 0 {
            continue;
        }
        let cover_m = inter * 100 / mw.len() as i64;
        let cover_p = inter * 100 / pw.len() as i64;
        let mut s = cover_m * 70 / 100 + cover_p * 25 / 100;
        let mut why = vec![format!("{inter} of {} words match", mw.len())];
        if pw_v.first().is_some_and(|b| mw.contains(b.as_str())) {
            s += 5;
            why.push("brand matches".into());
        }
        match (&m.size, product_size(n)) {
            (Some(a), Some(b)) if *a == b => {
                s += 10;
                why.push(format!("size {} matches", fmt_size(&b)));
            }
            (Some(_), Some(b)) => {
                s -= 50;
                why.push(format!("size differs ({})", fmt_size(&b)));
            }
            (Some(_), None) => s -= 10,
            _ => {}
        }
        // A variant the customer did not ask for ("zero", "diet", "light") is
        // less likely than the plain product; base words like "original" are not.
        let unasked: Vec<&str> = pw.iter().filter(|w| VARIANT_MARKERS.contains(w) && !mw.contains(*w)).copied().collect();
        if !unasked.is_empty() {
            s -= 15;
            why.push(format!("variant not asked for ({})", unasked.join(", ")));
        }
        // Not capped at 100: two products can both fit fully and still differ.
        if s > best.0 {
            best = (s.max(0), why);
        }
    }
    best
}

/// Resolve one mention in a branch.
pub fn resolve(c: &Connection, m: &Mention, branch: &str) -> AppResult<Resolution> {
    // A phrase a person confirmed earlier.
    let key = alias_key(m);
    if let Some(pid) =
        c.query_row("SELECT product_id FROM product_aliases WHERE alias_norm=?1", [&key], |r| r.get::<_, String>(0)).optional()?
    {
        if let Some(mut p) = load(c, &pid, branch)? {
            p.score = 100;
            p.reasons = vec![format!("\"{key}\" was confirmed as this product")];
            return Ok(Resolution {
                status: "resolved".into(),
                product: Some(p.clone()),
                options: vec![],
                band: "high".into(),
                reasons: p.reasons,
            });
        }
    }
    // A barcode typed or pasted.
    if let Some(bc) = m.text.split_whitespace().find(|w| matches!(w.len(), 8 | 12 | 13 | 14) && w.chars().all(|c| c.is_ascii_digit())) {
        let pid: Option<String> = c
            .query_row("SELECT b.product_id FROM product_barcodes b JOIN products p ON p.product_id=b.product_id WHERE b.barcode=?1 AND p.active=1", [bc], |r| r.get(0))
            .optional()?;
        if let Some(p) = pid.and_then(|p| load(c, &p, branch).ok().flatten()) {
            return Ok(Resolution {
                status: "resolved".into(),
                product: Some(p),
                options: vec![],
                band: "high".into(),
                reasons: vec![format!("barcode {bc}")],
            });
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut cands: Vec<Cand> = vec![];
    for w in m.words.iter().filter(|w| w.chars().count() >= 2).take(6) {
        let like = format!("%{}%", w.replace(['%', '_'], ""));
        // Arabic words also search the Arabic name; English lexicon words find both.
        let ar: Vec<&str> = LEXICON
            .iter()
            .filter(|(_, e)| e.split_whitespace().any(|x| x == w))
            .map(|(k, _)| *k)
            .filter(|k| super::interpret::has_arabic(k))
            .collect();
        let mut ids: Vec<String> = {
            let mut st =
                c.prepare("SELECT product_id FROM products WHERE active=1 AND (lower(name) LIKE ?1 OR name_ar LIKE ?1) LIMIT 80")?;
            let r = st.query_map([&like], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            r
        };
        for a in ar {
            let mut st = c.prepare("SELECT product_id FROM products WHERE active=1 AND name_ar LIKE ?1 LIMIT 40")?;
            ids.extend(st.query_map([format!("%{a}%")], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?);
        }
        for id in ids {
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(mut p) = load(c, &id, branch)? {
                let (s, why) = score(m, &p.name, p.name_ar.as_deref());
                if s >= 45 {
                    p.score = s;
                    p.reasons = why;
                    cands.push(p);
                }
            }
        }
    }
    cands.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
    // Keep the ones that cover the mention about as well as the best.
    if let Some(top) = cands.first().map(|x| x.score) {
        cands.retain(|x| x.score + 20 >= top);
    }
    let mut reasons = vec![];
    // "big" / "small": narrow by the real sizes on offer.
    if let Some(sw) = &m.size_word {
        let sized: Vec<&Cand> = cands.iter().filter(|x| x.size.is_some()).collect();
        if sized.len() >= 2 {
            let kept: Vec<String> = match sw.as_str() {
                "big" => {
                    let big: Vec<&&Cand> = sized.iter().filter(|x| x.size.as_ref().is_some_and(|s| s.0 >= 1000)).collect();
                    let big = if big.is_empty() || big.len() == sized.len() {
                        let max = sized.iter().filter_map(|x| x.size.as_ref().map(|s| s.0)).max().unwrap_or(0);
                        sized.iter().filter(|x| x.size.as_ref().is_some_and(|s| s.0 == max)).collect()
                    } else {
                        big
                    };
                    big.iter().map(|x| x.product_id.clone()).collect()
                }
                "small" => {
                    let small: Vec<&&Cand> = sized.iter().filter(|x| x.size.as_ref().is_some_and(|s| s.0 < 1000)).collect();
                    let small = if small.is_empty() || small.len() == sized.len() {
                        let min = sized.iter().filter_map(|x| x.size.as_ref().map(|s| s.0)).min().unwrap_or(0);
                        sized.iter().filter(|x| x.size.as_ref().is_some_and(|s| s.0 == min)).collect()
                    } else {
                        small
                    };
                    small.iter().map(|x| x.product_id.clone()).collect()
                }
                _ => sized.iter().map(|x| x.product_id.clone()).collect(),
            };
            if !kept.is_empty() {
                cands.retain(|x| kept.contains(&x.product_id));
                reasons.push(format!("\"{sw}\" narrows the sizes"));
            }
        }
    }
    match cands.len() {
        0 => Ok(Resolution {
            status: "unmatched".into(),
            product: None,
            options: vec![],
            band: "low".into(),
            reasons: vec!["no catalogue product matches these words".into()],
        }),
        1 => {
            let p = cands.remove(0);
            let band = if p.score >= 85 { "high" } else { "medium" };
            reasons.extend(p.reasons.clone());
            Ok(Resolution { status: "resolved".into(), product: Some(p), options: vec![], band: band.into(), reasons })
        }
        _ => {
            let (a, b) = (cands[0].score, cands[1].score);
            if a >= 75 && a - b >= 15 {
                let p = cands.remove(0);
                reasons.extend(p.reasons.clone());
                reasons.push("clearly the closest product".into());
                Ok(Resolution {
                    status: "resolved".into(),
                    product: Some(p),
                    options: cands.into_iter().take(3).collect(),
                    band: "medium".into(),
                    reasons,
                })
            } else {
                reasons.push(format!("{} products fit equally well", cands.len()));
                Ok(Resolution {
                    status: "ambiguous".into(),
                    product: None,
                    options: cands.into_iter().take(5).collect(),
                    band: "low".into(),
                    reasons,
                })
            }
        }
    }
}

/// How to describe options to a customer: by size when that is the only
/// difference, else by name.
pub fn option_labels(options: &[Cand]) -> Vec<String> {
    let names: Vec<String> = options.iter().map(|o| product_words(&o.name).join(" ")).collect();
    let same_words = names.windows(2).all(|w| w[0] == w[1]);
    options
        .iter()
        .map(|o| match (&o.size, same_words) {
            (Some(s), true) => fmt_size(s),
            _ => o.name.clone(),
        })
        .collect()
}

/// A clarification question for an ambiguous or unmatched mention.
pub fn question(m: &Mention, r: &Resolution, arabic: bool) -> String {
    let subject = m.words.join(" ");
    if r.status == "unmatched" || r.options.is_empty() {
        return if arabic {
            format!("لم نجد \"{}\" في قائمتنا. هل يمكنك وصفه أكثر؟", m.text)
        } else {
            format!("We couldn't find \"{}\". Could you describe it a bit more?", m.text)
        };
    }
    let labels = option_labels(&r.options);
    let by_size = r.options.iter().all(|o| o.size.is_some())
        && labels.iter().zip(&r.options).all(|(l, o)| o.size.as_ref().map(fmt_size).as_deref() == Some(l.as_str()));
    let list: Vec<String> = labels.iter().enumerate().map(|(i, l)| format!("{}) {l}", i + 1)).collect();
    let brand = r.options.first().map(|o| o.name.split_whitespace().next().unwrap_or("").to_string()).unwrap_or(subject);
    match (arabic, by_size) {
        (false, true) => format!("Which {brand} size would you like: {}?", list.join(", ")),
        (false, false) => format!("Which one would you like: {}?", list.join(", ")),
        (true, true) => format!("أي حجم من {brand} تريد: {}؟", list.join("، ")),
        (true, false) => format!("أي واحد تريد: {}؟", list.join("، ")),
    }
}

/// Match a reply to the options of a pending question: an option number,
/// a size ("2.25", "1.5L"), or words that single one option out.
pub fn answer(reply: &str, options: &[Cand]) -> Option<usize> {
    let t = normalize(reply);
    let t = t.trim().trim_matches(|c: char| c == '.' || c == '!' || c == ')');
    if options.is_empty() || t.is_empty() {
        return None;
    }
    // "2.25", "1.5 l", "330ml", "2.25 please"
    let words: Vec<&str> = t.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        let sized = size_token(w, words.get(i + 1).copied()).or_else(|| {
            // a bare decimal like "2.25" / "1.5" means litres (or kg)
            (w.contains('.') && w.chars().all(|c| c.is_ascii_digit() || c == '.'))
                .then(|| w.parse::<f64>().ok())
                .flatten()
                .map(|v| ((v * 1000.0).round() as i64, String::new()))
        });
        if let Some((v, unit)) = sized {
            let hits: Vec<usize> = options
                .iter()
                .enumerate()
                .filter(|(_, o)| o.size.as_ref().is_some_and(|s| s.0 == v && (unit.is_empty() || s.1 == unit)))
                .map(|(k, _)| k)
                .collect();
            if hits.len() == 1 {
                return Some(hits[0]);
            }
            // "330" meaning 330 ml
        }
        if let Ok(n) = w.parse::<i64>() {
            let hits: Vec<usize> =
                options.iter().enumerate().filter(|(_, o)| o.size.as_ref().is_some_and(|s| s.0 == n)).map(|(k, _)| k).collect();
            if hits.len() == 1 && n > options.len() as i64 {
                return Some(hits[0]);
            }
        }
    }
    // "1", "2", "first", "second", "the big one"
    let ordinal = match t {
        "1" | "first" | "the first" | "1st" | "الاول" | "الأول" => Some(0),
        "2" | "second" | "the second" | "2nd" | "الثاني" => Some(1),
        "3" | "third" | "the third" | "3rd" | "الثالث" => Some(2),
        "4" | "fourth" | "4th" => Some(3),
        _ => None,
    };
    if let Some(k) = ordinal.filter(|k| *k < options.len()) {
        return Some(k);
    }
    // Words that pick one option ("zero", "the big one", "original").
    let rw: Vec<String> = product_words(t);
    let big = super::interpret::parse_item(t).and_then(|m| m.size_word);
    if let Some(sw) = big {
        let sizes: Vec<(usize, i64)> = options.iter().enumerate().filter_map(|(k, o)| o.size.as_ref().map(|s| (k, s.0))).collect();
        if sizes.len() == options.len() {
            let pick = if sw == "big" { sizes.iter().max_by_key(|x| x.1) } else { sizes.iter().min_by_key(|x| x.1) };
            return pick.map(|x| x.0);
        }
    }
    let hits: Vec<usize> = options
        .iter()
        .enumerate()
        .filter(|(_, o)| {
            let pw = product_words(&o.name);
            !rw.is_empty() && rw.iter().all(|w| pw.contains(w))
        })
        .map(|(k, _)| k)
        .collect();
    (hits.len() == 1).then(|| hits[0])
}

/// In-stock alternatives for an unavailable product: the same product family
/// in another size, then the same brand, then the same category.
pub fn alternatives(c: &Connection, p: &Cand, branch: &str, limit: usize) -> AppResult<Vec<Cand>> {
    let words = product_words(&p.name);
    let brand = words.first().cloned().unwrap_or_default();
    let mut out: Vec<Cand> = vec![];
    let mut st = c.prepare("SELECT product_id FROM products WHERE active=1 AND product_id<>?1 AND (lower(name) LIKE ?2 OR (?3 IS NOT NULL AND category_id=?3)) LIMIT 200")?;
    let ids: Vec<String> = st
        .query_map(params![p.product_id, format!("%{}%", brand.replace(['%', '_'], "")), p.category_id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for id in ids {
        let Some(mut x) = load(c, &id, branch)? else { continue };
        if !matches!(x.availability.as_str(), "available" | "low" | "unknown") {
            continue;
        }
        let xw = product_words(&x.name);
        let shared = words.iter().filter(|w| xw.contains(w)).count() as i64;
        x.score = if shared == words.len() as i64 && x.size != p.size {
            x.reasons = vec!["same product, other size".into()];
            90
        } else if xw.first() == Some(&brand) {
            x.reasons = vec!["same brand".into()];
            60 + shared * 5
        } else if x.category_id.is_some() && x.category_id == p.category_id {
            x.reasons = vec!["same category".into()];
            30 + shared * 5
        } else {
            continue;
        };
        out.push(x);
    }
    out.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
    out.truncate(limit);
    Ok(out)
}
