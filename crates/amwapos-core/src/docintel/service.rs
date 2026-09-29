//! Document Intelligence services on `AppCore`: ingestion, the analysis that
//! runs after OCR, the review payload, corrections (with deterministic
//! learning), drafts, and the explicit posting of a receiving draft through
//! the normal receiving workflows.

// Row tuples read straight from SQL are kept inline where they are used.
#![allow(clippy::type_complexity)]

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::checks::{self, LineData, Recon, Validation};
use super::extract::extract;
use super::layout::Layout;
use super::matching::{self, MatchResult};
use super::quality::DocQuality;
use super::{Band, DocFields, DocType, ExtractedLine, Field};
use crate::audit;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ids::{new_id, next_seq};
use crate::service::AppCore;
use crate::time;
use crate::validate;

pub const DOC_EXT: &[&str] = &["pdf", "png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff"];
const MAX_BYTES: usize = 20 * 1024 * 1024;

fn sniff_ok(ext: &str, b: &[u8]) -> bool {
    match ext {
        "pdf" => b.starts_with(b"%PDF"),
        _ => true, // images are checked by the quality step (OCR decides readability)
    }
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        _ => "image/jpeg",
    }
}

/// Keep the original bytes (never modified) in the data folder.
pub fn store_original(dir: &Path, file_name: &str, bytes: &[u8], id: &str) -> AppResult<(std::path::PathBuf, String, &'static str)> {
    let ext = Path::new(file_name).extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    if !DOC_EXT.contains(&ext.as_str()) {
        return Err(AppError::validation("Choose a PDF or an image (JPG, PNG, WEBP, BMP or TIFF)."));
    }
    if bytes.is_empty() {
        return Err(AppError::validation("The file is empty."));
    }
    if bytes.len() > MAX_BYTES {
        return Err(AppError::validation("The file is larger than 20 MB."));
    }
    if !sniff_ok(&ext, bytes) {
        return Err(AppError::validation("The file is not a PDF, although its name ends in .pdf."));
    }
    std::fs::create_dir_all(dir)?;
    let dst = dir.join(format!("{id}.{ext}"));
    std::fs::write(&dst, bytes)?;
    Ok((dst, hex::encode(Sha256::digest(bytes)), mime_for(&ext)))
}

/// OCR output handed over by the worker.
#[derive(Debug, Clone, Default)]
pub struct DocOcr {
    pub layout: Layout,
    pub quality: Option<DocQuality>,
    /// Page images derived for review (PDF scans, rotated/cleaned copies).
    pub page_images: Vec<(u32, String)>,
    pub notes: Vec<String>,
    pub page_count: u32,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct DocPatch {
    pub revision: i64,
    pub doc_type: Option<String>,
    /// "" clears.
    pub supplier_id: Option<String>,
    pub invoice_number: Option<String>,
    pub invoice_date: Option<String>,
    pub due_date: Option<String>,
    pub subtotal_minor: Option<i64>,
    pub vat_minor: Option<i64>,
    pub total_minor: Option<i64>,
    pub vat_rate_bp: Option<i64>,
    /// "" = no purchase order; absent = keep.
    pub po_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct DocLinePatch {
    pub revision: i64,
    pub line_no: i64,
    pub product_id: Option<String>,
    pub clear_product: bool,
    pub qty_milli: Option<i64>,
    pub unit_cost_minor: Option<i64>,
    /// 0 clears.
    pub units_per_case: Option<i64>,
    pub unit: Option<String>,
    pub include: Option<bool>,
    pub new_product: Option<bool>,
}

fn j<T: serde::de::DeserializeOwned + Default>(s: Option<String>) -> T {
    s.and_then(|x| serde_json::from_str(&x).ok()).unwrap_or_default()
}

fn opt_json(s: Option<String>) -> Value {
    s.and_then(|x| serde_json::from_str(&x).ok()).unwrap_or(Value::Null)
}

pub(crate) fn decision(
    tx: &Connection,
    subject_id: &str,
    kind: &str,
    source: &str,
    model: Option<&str>,
    data: &Value,
    user: Option<&str>,
) -> AppResult<()> {
    tx.execute(
        "INSERT INTO ai_decisions(decision_id, subject, subject_id, kind, source, model, data_json, user_id, created_at) VALUES (?1,'document',?2,?3,?4,?5,?6,?7,?8)",
        params![new_id(), subject_id, kind, source, model, data.to_string(), user, time::now_str()],
    )?;
    Ok(())
}

fn system_actor() -> audit::Actor {
    audit::Actor { user_id: None, device_id: None, branch_id: None, approved_by: None }
}

fn branch_of(core: &AppCore, c: &Connection) -> AppResult<String> {
    if let Some(d) = core.device() {
        return Ok(d.branch_id);
    }
    Ok(c.query_row("SELECT branch_id FROM branches ORDER BY created_at LIMIT 1", [], |r| r.get(0)).optional()?.unwrap_or_default())
}

fn threshold_bp(c: &Connection) -> AppResult<i64> {
    Ok(crate::settings::get::<crate::settings::InventorySettings>(c, crate::settings::KEY_INVENTORY)?.invoice_cost_variance_bp)
}

/// Load the review lines of a scan.
pub fn load_lines(c: &Connection, id: &str) -> AppResult<Vec<LineData>> {
    let mut st = c.prepare(
        "SELECT l.line_no, COALESCE(l.description, l.raw_text), l.product_id, p.name, l.qty_milli, l.base_qty_milli, l.units_per_case, l.unit_cost_minor,
                l.discount_minor, l.vat_rate_bp, l.vat_minor, l.line_total_minor, l.include, l.po_item_id, l.new_product
         FROM invoice_scan_lines l LEFT JOIN products p ON p.product_id=l.product_id WHERE l.scan_id=?1 ORDER BY l.line_no",
    )?;
    let rows = st
        .query_map([id], |r| {
            Ok(LineData {
                line_no: r.get(0)?,
                description: r.get(1)?,
                product_id: r.get(2)?,
                product_name: r.get(3)?,
                qty_milli: r.get(4)?,
                base_qty_milli: r.get(5)?,
                units_per_case: r.get(6)?,
                unit_cost_minor: r.get(7)?,
                discount_minor: r.get(8)?,
                vat_rate_bp: r.get(9)?,
                vat_minor: r.get(10)?,
                line_total_minor: r.get(11)?,
                include: r.get::<_, i64>(12)? == 1,
                po_item_id: r.get(13)?,
                new_product: r.get::<_, i64>(14)? == 1,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn store_line(
    tx: &Connection,
    id: &str,
    no: i64,
    l: &ExtractedLine,
    m: &MatchResult,
    learned_upc: Option<i64>,
    source_kind: Option<&str>,
) -> AppResult<()> {
    let mut pack = l.pack.clone();
    let mut flags = l.flags.clone();
    if pack.units_per_case.is_none() {
        if let (Some(u), Some(q), Some("ctn" | "box")) = (learned_upc, l.qty_milli, l.unit.as_deref()) {
            pack.units_per_case = Some(u);
            pack.case_qty_milli = Some(q);
            pack.base_qty_milli = Some(q * u);
            pack.clear = true;
            flags.retain(|f| f != "pack_size_unknown");
            flags.push("pack_from_confirmed_mapping".into());
        }
    }
    let kind = source_kind.unwrap_or(m.kind.as_str());
    let kind = match kind {
        "barcode" | "supplier_map" | "supplier_code" | "sku" | "name" | "fuzzy" | "ai" | "manual" => kind,
        _ => "none",
    };
    let new_product = m.id.is_none() && !l.description.is_empty();
    let mut alts = m.alternatives.clone();
    if let (Some(id), Some(n)) = (&m.id, &m.name) {
        alts.insert(0, matching::Candidate { id: id.clone(), name: n.clone(), score: m.score, reasons: m.reasons.clone() });
    }
    tx.execute(
        "INSERT INTO invoice_scan_lines(scan_id, line_no, raw_text, description, code, qty_milli, unit_cost_minor, line_total_minor, product_id, match_kind,
            match_score, include, barcode, barcode_valid, unit, case_qty_milli, units_per_case, base_qty_milli, pack_text, pack_clear, discount_minor,
            vat_rate_bp, vat_minor, match_band, candidates_json, reasons_json, evidence_json, flags_json, new_product)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,1,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28)",
        params![
            id,
            no,
            l.raw_text,
            l.description,
            l.supplier_code.clone().or(l.barcode.clone()),
            l.qty_milli,
            l.unit_cost_minor,
            l.line_total_minor,
            m.id,
            kind,
            m.score,
            l.barcode,
            l.barcode_valid.map(|b| b as i64),
            l.unit,
            pack.case_qty_milli,
            pack.units_per_case,
            pack.base_qty_milli,
            pack.text,
            pack.clear as i64,
            l.discount_minor,
            l.vat_rate_bp,
            l.vat_minor,
            m.band.as_str(),
            serde_json::to_string(&alts).unwrap_or_default(),
            serde_json::to_string(&m.reasons).unwrap_or_default(),
            serde_json::to_string(&l.evidence).unwrap_or_default(),
            serde_json::to_string(&flags).unwrap_or_default(),
            new_product as i64,
        ],
    )?;
    Ok(())
}

/// Numbers and dates of the header, copied to their own columns (lists,
/// duplicate checks, drafts).
fn store_header(tx: &Connection, id: &str, f: &DocFields) -> AppResult<()> {
    tx.execute(
        "UPDATE invoice_scans SET fields_json=?2, invoice_number=?3, invoice_number_norm=?4, invoice_date=?5, due_date=?6, delivery_date=?7,
            subtotal_minor=?8, vat_minor=?9, total_minor=?10, supplier_vat=?11, supplier_cr=?12, buyer_vat=?13, updated_at=?14 WHERE scan_id=?1",
        params![
            id,
            serde_json::to_string(f).unwrap_or_default(),
            f.invoice_number.value,
            f.invoice_number.value.as_deref().map(super::norm_doc_number),
            f.invoice_date.value,
            f.due_date.value,
            f.delivery_date.value,
            f.subtotal_minor.value,
            f.vat_minor.value,
            f.total_minor.value,
            f.supplier_vat.value,
            f.supplier_cr.value,
            f.buyer_vat.value,
            time::now_str(),
        ],
    )?;
    Ok(())
}

fn own_identity(c: &Connection) -> AppResult<(Option<String>, Vec<String>)> {
    let (vat, name, ar): (Option<String>, String, Option<String>) = c
        .query_row("SELECT vat_number, name, name_ar FROM business LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?
        .unwrap_or((None, String::new(), None));
    Ok((vat, [Some(name), ar].into_iter().flatten().filter(|x| !x.is_empty()).collect()))
}

/// Supplier chosen by a person (at upload or in review) — kept on re-analysis.
fn person_supplier(c: &Connection, id: &str) -> AppResult<Option<String>> {
    let (sid, m): (Option<String>, Option<String>) =
        c.query_row("SELECT supplier_id, supplier_match_json FROM invoice_scans WHERE scan_id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let kind = m.and_then(|x| serde_json::from_str::<Value>(&x).ok()).and_then(|v| v["kind"].as_str().map(|s| s.to_string()));
    Ok(match kind.as_deref() {
        None | Some("person") => sid,
        _ => None,
    })
}

/// Read → classify → match → check. Replaces the lines of a scan in review.
pub(crate) fn analyze(core: &AppCore, tx: &Connection, id: &str, layout: &Layout) -> AppResult<()> {
    let digits = core.currency(tx)?.1;
    let (own_vat, own_names) = own_identity(tx)?;
    let ex = extract(layout, digits, own_vat.as_deref(), &own_names);
    let preset = person_supplier(tx, id)?;
    let sm = matching::match_supplier(tx, &ex.fields, preset.as_deref())?;
    let supplier = if sm.band >= Band::Medium { sm.id.clone() } else { preset.clone() };
    let keep_type: Option<String> =
        tx.query_row("SELECT doc_type FROM invoice_scans WHERE scan_id=?1 AND doc_type_source='person'", [id], |r| r.get(0)).optional()?;
    tx.execute("DELETE FROM invoice_scan_lines WHERE scan_id=?1", [id])?;
    for (i, l) in ex.lines.iter().enumerate() {
        let (m, upc) = matching::match_line(tx, supplier.as_deref(), l)?;
        store_line(tx, id, i as i64 + 1, l, &m, upc, None)?;
    }
    store_header(tx, id, &ex.fields)?;
    let cls = &ex.classification;
    tx.execute(
        "UPDATE invoice_scans SET supplier_id=?2, supplier_match_json=?3, doc_type=COALESCE(?4, ?5), doc_type_band=CASE WHEN ?4 IS NULL THEN ?6 ELSE 'high' END,
            doc_type_source=CASE WHEN ?4 IS NULL THEN ?7 ELSE 'person' END, parser='rules', updated_at=?8 WHERE scan_id=?1",
        params![
            id,
            supplier,
            serde_json::to_string(&sm).unwrap_or_default(),
            keep_type,
            cls.doc_type.as_str(),
            cls.band.as_str(),
            json!({ "source": cls.source, "reasons": cls.reasons }).to_string(),
            time::now_str()
        ],
    )?;
    decision(
        tx,
        id,
        "extraction",
        "rules",
        None,
        &json!({ "doc_type": cls.doc_type.as_str(), "band": cls.band.as_str(), "lines": ex.lines.len(), "supplier": sm.id, "supplier_band": sm.band.as_str(), "warnings": ex.warnings }),
        None,
    )?;
    recheck(core, tx, id, &ex.warnings)
}

/// The deterministic checks, recomputed after any change.
pub(crate) fn recheck(core: &AppCore, tx: &Connection, id: &str, warnings: &[String]) -> AppResult<()> {
    let (fields_json, supplier, sha, doc_type, po_sel, recon_json): (
        Option<String>,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<String>,
    ) = tx.query_row(
        "SELECT fields_json, supplier_id, image_sha256, doc_type, po_id, recon_json FROM invoice_scans WHERE scan_id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    )?;
    let f: DocFields = j(fields_json);
    let lines = load_lines(tx, id)?;
    let rates: Vec<i64> = {
        let mut st = tx.prepare("SELECT DISTINCT rate_bp FROM tax_rules WHERE active=1")?;
        let r = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<i64>, _>>()?;
        r
    };
    let mut product_rates = vec![];
    for l in &lines {
        if let Some(pid) = &l.product_id {
            let r: Option<i64> = tx
                .query_row(
                    "SELECT t.rate_bp FROM products p JOIN tax_rules t ON t.tax_rule_id=p.tax_rule_id WHERE p.product_id=?1",
                    [pid],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(r) = r {
                product_rates.push((l.line_no, r));
            }
        }
    }
    let mut v: Validation = checks::validate(&f, &lines, &rates, &product_rates);
    for w in warnings {
        v.issues.push(checks::Issue {
            code: "extraction".into(),
            severity: "info".into(),
            message: w.clone(),
            line_no: None,
            field: None,
            printed_minor: None,
            calculated_minor: None,
        });
    }
    let fp = (!lines.is_empty()).then(|| checks::line_fingerprint(&lines));
    let dups = checks::duplicates(
        tx,
        id,
        &sha,
        supplier.as_deref(),
        f.invoice_number.value.as_deref().map(super::norm_doc_number).as_deref(),
        f.invoice_date.value.as_deref(),
        f.total_minor.value,
        fp.as_deref(),
    )?;
    let branch = branch_of(core, tx)?;
    let thr = threshold_bp(tx)?;
    // Purchase order: the one a person picked, else a strong candidate.
    let prev: Recon = j(recon_json);
    let mut recon: Option<Recon> = None;
    if let Some(sid) = &supplier {
        let cands = checks::po_candidates(tx, sid, &f, &lines)?;
        let (pick, by) = match (&po_sel, prev.selected_by.as_deref()) {
            (Some(p), Some("person")) => (Some(p.clone()), "person"),
            (Some(p), _) if prev.selected_by.is_none() && po_sel.is_some() => (Some(p.clone()), "person"),
            _ => (cands.first().filter(|c| c.score >= 60).map(|c| c.po_id.clone()), "suggested"),
        };
        let mut r = match &pick {
            Some(p) => checks::reconcile(tx, p, &lines, thr, &branch)?,
            None => Recon::default(),
        };
        if pick.is_none() && prev.selected_by.as_deref() == Some("person") {
            r.selected_by = Some("person".into());
        } else {
            r.selected_by = pick.as_ref().map(|_| by.to_string());
        }
        r.candidates = cands;
        recon = Some(r);
    }
    let sm: MatchResult = tx
        .query_row("SELECT supplier_match_json FROM invoice_scans WHERE scan_id=?1", [id], |r| r.get::<_, Option<String>>(0))?
        .and_then(|x| serde_json::from_str(&x).ok())
        .unwrap_or_else(|| MatchResult::none(vec![]));
    let anomalies = checks::anomalies(tx, &f, &v, &dups, &sm, &lines, thr, &branch, supplier.as_deref(), id)?;
    let supplier_name: Option<String> = match &supplier {
        Some(s) => tx.query_row("SELECT name FROM suppliers WHERE supplier_id=?1", [s], |r| r.get(0)).optional()?,
        None => None,
    };
    let bands: Vec<String> = {
        let mut st = tx.prepare("SELECT match_band FROM invoice_scan_lines WHERE scan_id=?1 AND include=1 ORDER BY line_no")?;
        let r = st.query_map([id], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
        r
    };
    let summary = checks::summary(&checks::SummaryInput {
        supplier: supplier_name.as_deref().or(f.supplier_name.value.as_deref()),
        doc_type: DocType::parse(&doc_type).unwrap_or(DocType::Unknown),
        f: &f,
        lines: &lines,
        bands: &bands,
        validation: &v,
        recon: recon.as_ref().filter(|r| r.po_id.is_some()),
        duplicates: dups.len(),
    });
    let po_id = recon.as_ref().and_then(|r| r.po_id.clone());
    // PO item links for the draft receiving.
    if let Some(r) = &recon {
        tx.execute("UPDATE invoice_scan_lines SET po_item_id=NULL WHERE scan_id=?1", [id])?;
        for rl in &r.lines {
            if let (Some(no), Some(iid)) = (rl.line_no, &rl.po_item_id) {
                tx.execute("UPDATE invoice_scan_lines SET po_item_id=?3 WHERE scan_id=?1 AND line_no=?2", params![id, no, iid])?;
            }
        }
    }
    tx.execute(
        "UPDATE invoice_scans SET validation_json=?2, duplicate_json=?3, anomalies_json=?4, recon_json=?5, summary=?6, line_fingerprint=?7,
            po_id=CASE WHEN status='review' THEN ?8 ELSE po_id END, updated_at=?9 WHERE scan_id=?1",
        params![
            id,
            checks::to_json(&v),
            checks::to_json(&dups),
            checks::to_json(&anomalies),
            recon.as_ref().map(checks::to_json),
            summary,
            fp,
            po_id,
            time::now_str()
        ],
    )?;
    Ok(())
}

fn doc_row(c: &Connection, id: &str) -> AppResult<(String, i64, Option<String>)> {
    c.query_row("SELECT status, revision, supplier_id FROM invoice_scans WHERE scan_id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?
        .ok_or_else(|| AppError::not_found("Document"))
}

fn check_revision(c: &Connection, id: &str, revision: i64, statuses: &[&str]) -> AppResult<(String, Option<String>)> {
    let (status, rev, supplier) = doc_row(c, id)?;
    if !statuses.contains(&status.as_str()) {
        return Err(AppError::conflict("This document is no longer waiting for review."));
    }
    if revision != rev {
        return Err(AppError::conflict(
            "Someone else changed this document (or its reading finished) while you were looking at it. Reload it and try again.",
        )
        .with_details(json!({ "kind": "stale_revision", "current": rev })));
    }
    Ok((status, supplier))
}

fn bump(tx: &Connection, id: &str) -> AppResult<()> {
    tx.execute(
        "UPDATE invoice_scans SET revision=revision+1, corrections=corrections+1, updated_at=?2 WHERE scan_id=?1",
        params![id, time::now_str()],
    )?;
    Ok(())
}

fn learn_supplier(tx: &Connection, id: &str, supplier: &str, user: &str) -> AppResult<()> {
    let f: DocFields = j(tx.query_row("SELECT fields_json FROM invoice_scans WHERE scan_id=?1", [id], |r| r.get(0))?);
    let now = time::now_str();
    let put = |key: Option<String>, kind: &str| -> AppResult<()> {
        if let Some(k) = key.filter(|k| !k.is_empty()) {
            tx.execute(
                "INSERT INTO supplier_aliases(alias_norm, kind, supplier_id, created_by, created_at) VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(alias_norm, kind) DO UPDATE SET supplier_id=excluded.supplier_id, created_by=excluded.created_by, created_at=excluded.created_at",
                params![k, kind, supplier, user, now],
            )?;
        }
        Ok(())
    };
    put(f.supplier_name.value.as_deref().map(super::norm_name), "name")?;
    put(f.supplier_vat.value.as_deref().map(super::digits_only), "vat")?;
    put(f.supplier_cr.value.as_deref().map(super::digits_only), "cr")?;
    put(f.supplier_phone.value.as_deref().map(super::digits_only), "phone")?;
    Ok(())
}

fn learn_line(tx: &Connection, id: &str, line_no: i64, supplier: &str, user: &str) -> AppResult<()> {
    let (pid, code, barcode, desc, upc): (Option<String>, Option<String>, Option<String>, Option<String>, Option<i64>) = tx.query_row(
        "SELECT product_id, code, barcode, description, units_per_case FROM invoice_scan_lines WHERE scan_id=?1 AND line_no=?2",
        params![id, line_no],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    let Some(pid) = pid else { return Ok(()) };
    let now = time::now_str();
    let mut keys = vec![];
    if let Some(c) = code.filter(|c| barcode.as_deref() != Some(c.as_str())) {
        keys.push(("code", matching::code_key(&c)));
    }
    if let Some(d) = desc.map(|d| matching::desc_key(&d)).filter(|d| !d.is_empty()) {
        keys.push(("desc", d));
    }
    for (kind, key) in keys {
        tx.execute(
            "INSERT INTO supplier_product_map(supplier_id, key_kind, key_norm, product_id, units_per_case, uses, confirmed_by, confirmed_at) VALUES (?1,?2,?3,?4,?5,1,?6,?7)
             ON CONFLICT(supplier_id, key_kind, key_norm) DO UPDATE SET product_id=excluded.product_id, units_per_case=COALESCE(excluded.units_per_case, units_per_case),
               uses=uses+1, confirmed_by=excluded.confirmed_by, confirmed_at=excluded.confirmed_at",
            params![supplier, kind, key, pid, upc, user, now],
        )?;
    }
    Ok(())
}

/// A change to one field: the value a person set, flagged as such.
fn person_field<T: Clone>(old: &Field<T>, v: Option<T>) -> Field<T> {
    let mut f = Field::person(v);
    f.raw = old.raw.clone();
    f.evidence = old.evidence.clone();
    f
}

impl AppCore {
    fn doc_session(&self, token: &str) -> AppResult<crate::auth::Session> {
        let s = self.session(token)?;
        s.require("ocr.scan")?;
        Ok(s)
    }

    /// Upload a supplier document (PDF or image). The original is kept as
    /// uploaded; reading happens on the OCR worker.
    pub fn doc_import(
        &self,
        token: &str,
        file_name: &str,
        data_b64: &str,
        supplier_id: Option<String>,
    ) -> AppResult<crate::ocrflow::InvoiceScan> {
        let s = self.doc_session(token)?;
        self.require_feature("ocr.supplier_invoices")?;
        if data_b64.len() > 28 * 1024 * 1024 {
            return Err(AppError::validation("The file is larger than 20 MB."));
        }
        let bytes = crate::ids::b64_decode(data_b64).ok_or_else(|| AppError::validation("The file could not be read."))?;
        let id = new_id();
        let (dst, sha, mime) = store_original(&self.data_dir.join("invoice-scans"), file_name, &bytes, &id)?;
        let supplier = supplier_id.filter(|x| !x.is_empty()).map(|x| validate::id(&x, "Supplier")).transpose()?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            if let Some(sid) = &supplier {
                tx.query_row("SELECT 1 FROM suppliers WHERE supplier_id=?1", [sid], |_| Ok(())).optional()?.ok_or_else(|| AppError::not_found("Supplier"))?;
            }
            let number = format!("IS-{:05}", next_seq(tx, "invoice_scan")?);
            let now = time::now_str();
            tx.execute(
                "INSERT INTO invoice_scans(scan_id, scan_number, supplier_id, image_path, image_sha256, file_name, status, stage, mime, source, created_by, created_at, updated_at,
                    supplier_match_json)
                 VALUES (?1,?2,?3,?4,?5,?6,'imported','uploaded',?7,'upload',?8,?9,?9,?10)",
                params![
                    id,
                    number,
                    supplier,
                    dst.to_string_lossy(),
                    sha,
                    file_name.chars().take(200).collect::<String>(),
                    mime,
                    s.user_id,
                    now,
                    supplier.as_ref().map(|_| json!({ "kind": "person" }).to_string())
                ],
            )?;
            audit::record(tx, &actor, "invoice_scan.imported", "invoice_scan", Some(&id), None, Some(&json!({ "number": number, "mime": mime, "sha256": sha })))?;
            Ok(())
        })?;
        self.db.read(|c| crate::ocrflow::load_scan_pub(c, &id))
    }

    /// Pass a document received on WhatsApp (already downloaded by the
    /// existing WhatsApp link) into the same pipeline.
    pub fn doc_from_inbox(&self, token: &str, seq: i64) -> AppResult<crate::ocrflow::InvoiceScan> {
        let s = self.doc_session(token)?;
        self.require_feature("ocr.supplier_invoices")?;
        let (path, mime, kind): (Option<String>, Option<String>, String) = self.db.read(|c| {
            c.query_row("SELECT media_path, media_mime, kind FROM wa_inbox WHERE seq=?1", [seq], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?
                .ok_or_else(|| AppError::not_found("Message"))
        })?;
        if !matches!(kind.as_str(), "image" | "document") {
            return Err(AppError::validation("Only images and documents can be read as supplier documents."));
        }
        let path = path.ok_or_else(|| AppError::conflict("The attachment has not been downloaded yet."))?;
        let bytes = std::fs::read(&path).map_err(|_| AppError::not_found("Attachment file"))?;
        let ext = match mime.as_deref().unwrap_or("") {
            "application/pdf" => "pdf",
            "image/png" => "png",
            "image/webp" => "webp",
            _ => "jpg",
        };
        let existing: Option<String> = self.db.read(|c| {
            Ok(c.query_row("SELECT scan_id FROM invoice_scans WHERE inbox_seq=?1 AND status<>'rejected'", [seq], |r| r.get(0))
                .optional()?)
        })?;
        if let Some(e) = existing {
            return self.db.read(|c| crate::ocrflow::load_scan_pub(c, &e));
        }
        let scan = self.doc_import(token, &format!("whatsapp-{seq}.{ext}"), &crate::ids::b64(&bytes), None)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            tx.execute("UPDATE invoice_scans SET source='whatsapp', inbox_seq=?2 WHERE scan_id=?1", params![scan.scan_id, seq])?;
            audit::record(
                tx,
                &actor,
                "invoice_scan.from_whatsapp",
                "invoice_scan",
                Some(&scan.scan_id),
                None,
                Some(&json!({ "inbox_seq": seq })),
            )?;
            Ok(())
        })?;
        self.db.read(|c| crate::ocrflow::load_scan_pub(c, &scan.scan_id))
    }

    /// Worker: pipeline stage (for the review list's progress).
    pub fn doc_stage(&self, id: &str, stage: &str) -> AppResult<()> {
        if !["preprocessing", "ocr", "extracting", "matching", "validating"].contains(&stage) {
            return Err(AppError::validation("Unknown stage."));
        }
        self.db.write(|tx| {
            tx.execute(
                "UPDATE invoice_scans SET stage=?2, started_at=COALESCE(started_at, ?3), updated_at=?3 WHERE scan_id=?1 AND status='imported'",
                params![id, stage, time::now_str()],
            )?;
            Ok(())
        })
    }

    /// Worker: OCR finished. Runs extraction → matching → checks and opens the review.
    pub fn doc_ocr_done(&self, id: &str, r: DocOcr) -> AppResult<()> {
        let layout_path = self.data_dir.join("invoice-scans").join(format!("{id}.layout.json"));
        std::fs::create_dir_all(layout_path.parent().unwrap_or(&self.data_dir))?;
        std::fs::write(&layout_path, serde_json::to_vec(&r.layout).unwrap_or_default())?;
        let text: String = r.layout.text().chars().take(20_000).collect();
        let conf = r.layout.mean_conf();
        let quality = r.quality.clone();
        self.db.write(|tx| {
            let status: Option<String> = tx.query_row("SELECT status FROM invoice_scans WHERE scan_id=?1", [id], |r| r.get(0)).optional()?;
            if status.as_deref() != Some("imported") {
                return Ok(());
            }
            let mut qjson = quality.as_ref().map(|q| serde_json::to_value(q).unwrap_or_default()).unwrap_or(json!({ "status": null, "pages": [], "messages": [] }));
            if !r.notes.is_empty() {
                if let Some(m) = qjson["messages"].as_array_mut() {
                    m.extend(r.notes.iter().map(|n| json!(n)));
                }
            }
            qjson["page_images"] = json!(r.page_images.iter().map(|(p, path)| json!({ "page": p, "path": path })).collect::<Vec<_>>());
            tx.execute(
                "UPDATE invoice_scans SET status='review', stage='ready_for_review', ocr_text=?2, ocr_confidence=?3, layout_path=?4, quality_status=?5, quality_json=?6,
                    page_count=?7, error=NULL, finished_at=?8, started_at=COALESCE(started_at, ?8), revision=revision+1, updated_at=?8 WHERE scan_id=?1",
                params![
                    id,
                    text,
                    conf,
                    layout_path.to_string_lossy(),
                    quality.as_ref().map(|q| q.status.clone()),
                    qjson.to_string(),
                    r.page_count.max(r.layout.pages.len() as u32),
                    time::now_str()
                ],
            )?;
            analyze(self, tx, id, &r.layout)?;
            audit::record(tx, &system_actor(), "invoice_scan.read", "invoice_scan", Some(id), None, Some(&json!({ "confidence": conf, "quality": quality.as_ref().map(|q| q.status.clone()) })))?;
            Ok(())
        })
    }

    pub fn doc_failed(&self, id: &str, error: &str) -> AppResult<()> {
        self.db.write(|tx| {
            tx.execute(
                "UPDATE invoice_scans SET status='failed', stage='failed', error=?2, finished_at=?3, updated_at=?3 WHERE scan_id=?1 AND status='imported'",
                params![id, error.chars().take(500).collect::<String>(), time::now_str()],
            )?;
            Ok(())
        })
    }

    fn doc_layout(&self, c: &Connection, id: &str) -> AppResult<Layout> {
        let (path, text, conf): (Option<String>, Option<String>, Option<i64>) =
            c.query_row("SELECT layout_path, ocr_text, ocr_confidence FROM invoice_scans WHERE scan_id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        if let Some(p) = path {
            if let Some(l) = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Layout>(&b).ok()) {
                return Ok(l);
            }
        }
        Ok(Layout::from_text(&text.unwrap_or_default(), conf.unwrap_or(0)))
    }

    /// Everything the review screen shows.
    pub fn doc_get(&self, token: &str, id: &str) -> AppResult<Value> {
        let _ = self.doc_session(token)?;
        let id = validate::id(id, "Document")?;
        self.db.read(|c| {
            let scan = crate::ocrflow::load_scan_pub(c, &id)?;
            type Row = (
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                i64,
                Option<String>,
                Option<i64>,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<i64>,
                String,
                Option<String>,
                i64,
            );
            let row: Row = c.query_row(
                "SELECT doc_type, doc_type_band, doc_type_source, quality_status, quality_json, fields_json, supplier_match_json, validation_json, duplicate_json,
                        anomalies_json, recon_json, revision, summary, page_count, stage, mime, supplier_invoice_id, receiving_draft_id, inbox_seq, source, ai_model, corrections
                 FROM invoice_scans WHERE scan_id=?1",
                [&id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                        r.get(10)?,
                        r.get(11)?,
                        r.get(12)?,
                        r.get(13)?,
                        r.get(14)?,
                        r.get(15)?,
                        r.get(16)?,
                        r.get(17)?,
                        r.get(18)?,
                        r.get(19)?,
                        r.get(20)?,
                        r.get(21)?,
                    ))
                },
            )?;
            let mut st = c.prepare(
                "SELECT l.line_no, l.raw_text, l.description, l.code, l.barcode, l.barcode_valid, l.qty_milli, l.unit, l.case_qty_milli, l.units_per_case,
                        l.base_qty_milli, l.pack_text, l.pack_clear, l.unit_cost_minor, l.discount_minor, l.vat_rate_bp, l.vat_minor, l.line_total_minor,
                        l.product_id, p.name, l.match_kind, l.match_score, l.match_band, l.candidates_json, l.reasons_json, l.evidence_json, l.flags_json,
                        l.include, l.new_product, l.corrected, l.po_item_id,
                        (SELECT MAX(pc.last_cost_minor) FROM product_costs pc WHERE pc.product_id=l.product_id)
                 FROM invoice_scan_lines l LEFT JOIN products p ON p.product_id=l.product_id WHERE l.scan_id=?1 ORDER BY l.line_no",
            )?;
            let lines: Vec<Value> = st
                .query_map([&id], |r| {
                    let js = |i: usize| -> rusqlite::Result<Value> { Ok(opt_json(r.get(i)?)) };
                    Ok(json!({
                        "line_no": r.get::<_, i64>(0)?, "raw_text": r.get::<_, String>(1)?, "description": r.get::<_, Option<String>>(2)?,
                        "code": r.get::<_, Option<String>>(3)?, "barcode": r.get::<_, Option<String>>(4)?, "barcode_valid": r.get::<_, Option<i64>>(5)?.map(|x| x == 1),
                        "qty_milli": r.get::<_, Option<i64>>(6)?, "unit": r.get::<_, Option<String>>(7)?, "case_qty_milli": r.get::<_, Option<i64>>(8)?,
                        "units_per_case": r.get::<_, Option<i64>>(9)?, "base_qty_milli": r.get::<_, Option<i64>>(10)?, "pack_text": r.get::<_, Option<String>>(11)?,
                        "pack_clear": r.get::<_, i64>(12)? == 1, "unit_cost_minor": r.get::<_, Option<i64>>(13)?, "discount_minor": r.get::<_, Option<i64>>(14)?,
                        "vat_rate_bp": r.get::<_, Option<i64>>(15)?, "vat_minor": r.get::<_, Option<i64>>(16)?, "line_total_minor": r.get::<_, Option<i64>>(17)?,
                        "product_id": r.get::<_, Option<String>>(18)?, "product_name": r.get::<_, Option<String>>(19)?, "match_kind": r.get::<_, String>(20)?,
                        "match_score": r.get::<_, i64>(21)?, "match_band": r.get::<_, String>(22)?, "candidates": js(23)?, "reasons": js(24)?,
                        "evidence": js(25)?, "flags": js(26)?, "include": r.get::<_, i64>(27)? == 1, "new_product": r.get::<_, i64>(28)? == 1,
                        "corrected": r.get::<_, i64>(29)? == 1, "po_item_id": r.get::<_, Option<String>>(30)?, "last_cost_minor": r.get::<_, Option<i64>>(31)?,
                    }))
                })?
                .collect::<Result<_, _>>()?;
            let receive: Vec<Value> = load_lines(c, &id)?
                .iter()
                .map(|l| {
                    let (cost, exact) = l.receive_unit_cost().map(|x| (Some(x.0), x.1)).unwrap_or((None, true));
                    json!({ "line_no": l.line_no, "receive_qty_milli": l.receive_qty(), "receive_unit_cost_minor": cost, "cost_exact": exact })
                })
                .collect();
            let cls_meta = opt_json(row.2.clone());
            Ok(json!({
                "scan": scan,
                "revision": row.11,
                "stage": row.14,
                "mime": row.15,
                "source": row.19,
                "inbox_seq": row.18,
                "page_count": row.13,
                "ai_model": row.20,
                "corrections": row.21,
                "classification": { "doc_type": row.0, "band": row.1, "source": cls_meta.get("source").cloned().unwrap_or(json!(row.2)), "reasons": cls_meta.get("reasons").cloned().unwrap_or(json!([])) },
                "quality": opt_json(row.4).as_object().map(|o| { let mut o = o.clone(); o.remove("page_images"); Value::Object(o) }).unwrap_or(json!({ "status": row.3 })),
                "fields": opt_json(row.5),
                "supplier_match": opt_json(row.6),
                "validation": opt_json(row.7),
                "duplicates": opt_json(row.8),
                "anomalies": opt_json(row.9),
                "recon": opt_json(row.10),
                "summary": row.12,
                "lines": lines,
                "receive": receive,
                "supplier_invoice_id": row.16,
                "receiving_draft_id": row.17,
            }))
        })
    }

    /// A page image for the review screen (original image, or the page
    /// picture taken from a PDF). Served inline, never from a public URL.
    pub fn doc_page(&self, token: &str, id: &str, page: u32) -> AppResult<Value> {
        let _ = self.doc_session(token)?;
        let id = validate::id(id, "Document")?;
        let (path, mime, q): (String, Option<String>, Option<String>) = self.db.read(|c| {
            Ok(c.query_row("SELECT image_path, mime, quality_json FROM invoice_scans WHERE scan_id=?1", [&id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?)
        })?;
        let derived = opt_json(q)["page_images"].as_array().and_then(|a| {
            a.iter().find(|x| x["page"].as_u64() == Some(page as u64)).and_then(|x| x["path"].as_str().map(|s| s.to_string()))
        });
        let (p, m) = match derived {
            Some(d) => (d.clone(), if d.ends_with(".png") { "image/png" } else { "image/jpeg" }),
            None if mime.as_deref() != Some("application/pdf") && page == 1 => {
                (path, mime.as_deref().map(|m| if m.starts_with("image/") { m } else { "image/jpeg" }).unwrap_or("image/jpeg"))
            }
            None => return Ok(json!({ "page": page, "image": null })),
        };
        let m: &'static str = match m {
            "image/png" => "image/png",
            "image/webp" => "image/webp",
            _ => "image/jpeg",
        };
        Ok(json!({ "page": page, "image": self.read_data_file(&p, m).ok() }))
    }

    /// Correct header fields, the document type, the supplier or the PO.
    pub fn doc_update(&self, token: &str, id: &str, p: DocPatch) -> AppResult<Value> {
        let s = self.doc_session(token)?;
        let id = validate::id(id, "Document")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let (_, current_supplier) = check_revision(tx, &id, p.revision, &["review"])?;
            let mut f: DocFields = j(tx.query_row("SELECT fields_json FROM invoice_scans WHERE scan_id=?1", [&id], |r| r.get(0))?);
            let mut changed = serde_json::Map::new();
            if let Some(t) = &p.doc_type {
                let dt = DocType::parse(t).ok_or_else(|| AppError::validation("Document type must be invoice, credit note, delivery note or unknown."))?;
                tx.execute(
                    "UPDATE invoice_scans SET doc_type=?2, doc_type_band='high', doc_type_source='person' WHERE scan_id=?1",
                    params![id, dt.as_str()],
                )?;
                changed.insert("doc_type".into(), json!(dt.as_str()));
            }
            if let Some(sid) = &p.supplier_id {
                let sid = sid.trim();
                let new = if sid.is_empty() {
                    None
                } else {
                    let sid = validate::id(sid, "Supplier")?;
                    let name: String =
                        tx.query_row("SELECT name FROM suppliers WHERE supplier_id=?1", [&sid], |r| r.get(0)).optional()?.ok_or_else(|| AppError::not_found("Supplier"))?;
                    Some((sid, name))
                };
                let m = match &new {
                    Some((sid, name)) => MatchResult { id: Some(sid.clone()), name: Some(name.clone()), kind: "person".into(), score: 100, band: Band::High, reasons: vec!["Chosen by a person".into()], alternatives: vec![] },
                    None => MatchResult { kind: "person".into(), ..MatchResult::none(vec![]) },
                };
                tx.execute(
                    "UPDATE invoice_scans SET supplier_id=?2, supplier_match_json=?3 WHERE scan_id=?1",
                    params![id, new.as_ref().map(|x| x.0.clone()), serde_json::to_string(&m).unwrap_or_default()],
                )?;
                if let Some((sid, _)) = &new {
                    learn_supplier(tx, &id, sid, &s.user_id)?;
                }
                changed.insert("supplier_id".into(), json!(new.as_ref().map(|x| x.0.clone())));
                // Re-match the lines against the confirmed supplier's mappings (keeps manual choices).
                if new.as_ref().map(|x| &x.0) != current_supplier.as_ref() {
                    if let Some((sid, _)) = &new {
                        let mut st = tx.prepare("SELECT line_no, description, code, barcode FROM invoice_scan_lines WHERE scan_id=?1 AND match_kind<>'manual' AND corrected=0")?;
                        let rows: Vec<(i64, Option<String>, Option<String>, Option<String>)> =
                            st.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
                        drop(st);
                        for (no, desc, code, bc) in rows {
                            let el = ExtractedLine {
                                description: desc.unwrap_or_default(),
                                supplier_code: code.filter(|c| bc.as_deref() != Some(c.as_str())),
                                barcode_valid: bc.as_deref().map(super::gtin_valid),
                                barcode: bc,
                                ..Default::default()
                            };
                            let (m, _) = matching::match_line(tx, Some(sid), &el)?;
                            if m.id.is_some() {
                                let mut alts = m.alternatives.clone();
                                alts.insert(0, matching::Candidate { id: m.id.clone().unwrap_or_default(), name: m.name.clone().unwrap_or_default(), score: m.score, reasons: m.reasons.clone() });
                                tx.execute(
                                    "UPDATE invoice_scan_lines SET product_id=?3, match_kind=?4, match_score=?5, match_band=?6, candidates_json=?7, reasons_json=?8, new_product=0
                                     WHERE scan_id=?1 AND line_no=?2",
                                    params![id, no, m.id, if matches!(m.kind.as_str(), "barcode" | "supplier_map" | "sku" | "name" | "fuzzy") { m.kind.as_str() } else { "fuzzy" }, m.score, m.band.as_str(), serde_json::to_string(&alts).unwrap_or_default(), serde_json::to_string(&m.reasons).unwrap_or_default()],
                                )?;
                            }
                        }
                    }
                }
            }
            if let Some(v) = &p.invoice_number {
                let v = v.trim();
                if v.chars().count() > 60 {
                    return Err(AppError::validation("The invoice number is too long."));
                }
                f.invoice_number = person_field(&f.invoice_number, (!v.is_empty()).then(|| v.to_string()));
                changed.insert("invoice_number".into(), json!(v));
            }
            for (key, val, fld) in [("invoice_date", &p.invoice_date, &mut f.invoice_date), ("due_date", &p.due_date, &mut f.due_date)] {
                if let Some(d) = val {
                    let d = d.trim();
                    if !d.is_empty() && chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_err() {
                        return Err(AppError::validation("Dates are YYYY-MM-DD."));
                    }
                    *fld = person_field(fld, (!d.is_empty()).then(|| d.to_string()));
                    changed.insert(key.into(), json!(d));
                }
            }
            for (key, val, fld) in [
                ("subtotal_minor", p.subtotal_minor, &mut f.subtotal_minor),
                ("vat_minor", p.vat_minor, &mut f.vat_minor),
                ("total_minor", p.total_minor, &mut f.total_minor),
            ] {
                if let Some(v) = val {
                    validate::money_non_negative(v, "Amount")?;
                    *fld = person_field(fld, Some(v));
                    changed.insert(key.into(), json!(v));
                }
            }
            if let Some(r) = p.vat_rate_bp {
                if !(0..=10_000).contains(&r) {
                    return Err(AppError::validation("The VAT rate must be between 0% and 100%."));
                }
                f.vat_rate_bp = person_field(&f.vat_rate_bp, Some(r));
                changed.insert("vat_rate_bp".into(), json!(r));
            }
            store_header(tx, &id, &f)?;
            if let Some(po) = &p.po_id {
                let po = po.trim();
                let sel = if po.is_empty() {
                    None
                } else {
                    let po = validate::id(po, "Purchase order")?;
                    let sup: String = tx
                        .query_row("SELECT supplier_id FROM purchase_orders WHERE po_id=?1", [&po], |r| r.get(0))
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Purchase order"))?;
                    let cur: Option<String> = tx.query_row("SELECT supplier_id FROM invoice_scans WHERE scan_id=?1", [&id], |r| r.get(0))?;
                    if cur.as_deref() != Some(sup.as_str()) {
                        return Err(AppError::validation("That purchase order belongs to another supplier."));
                    }
                    Some(po)
                };
                tx.execute(
                    "UPDATE invoice_scans SET po_id=?2, recon_json=json_set(COALESCE(recon_json,'{}'), '$.selected_by', 'person') WHERE scan_id=?1",
                    params![id, sel],
                )?;
                changed.insert("po_id".into(), json!(sel));
            }
            if changed.is_empty() {
                return Ok(());
            }
            bump(tx, &id)?;
            decision(tx, &id, "correction", "person", None, &Value::Object(changed.clone()), Some(&s.user_id))?;
            audit::record(tx, &actor, "invoice_scan.corrected", "invoice_scan", Some(&id), None, Some(&Value::Object(changed)))?;
            recheck(self, tx, &id, &[])
        })?;
        self.doc_get(token, &id)
    }

    /// Correct one line: product, quantity, unit cost, pack size, include.
    pub fn doc_update_line(&self, token: &str, id: &str, p: DocLinePatch) -> AppResult<Value> {
        let s = self.doc_session(token)?;
        let id = validate::id(id, "Document")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let (_, supplier) = check_revision(tx, &id, p.revision, &["review"])?;
            tx.query_row("SELECT 1 FROM invoice_scan_lines WHERE scan_id=?1 AND line_no=?2", params![id, p.line_no], |_| Ok(()))
                .optional()?
                .ok_or_else(|| AppError::not_found("Document line"))?;
            let mut changed = serde_json::Map::new();
            if let Some(pid) = p.product_id.as_deref().filter(|x| !x.is_empty()) {
                let pid = validate::id(pid, "Product")?;
                tx.query_row("SELECT 1 FROM products WHERE product_id=?1", [&pid], |_| Ok(())).optional()?.ok_or_else(|| AppError::not_found("Product"))?;
                tx.execute(
                    "UPDATE invoice_scan_lines SET product_id=?3, match_kind='manual', match_score=100, match_band='high', new_product=0, corrected=1 WHERE scan_id=?1 AND line_no=?2",
                    params![id, p.line_no, pid],
                )?;
                changed.insert("product_id".into(), json!(pid));
            } else if p.clear_product {
                tx.execute(
                    "UPDATE invoice_scan_lines SET product_id=NULL, match_kind='none', match_score=0, match_band='unresolved', corrected=1 WHERE scan_id=?1 AND line_no=?2",
                    params![id, p.line_no],
                )?;
                changed.insert("product_id".into(), Value::Null);
            }
            if let Some(q) = p.qty_milli {
                validate::qty_positive(q, true, "Quantity")?;
                tx.execute("UPDATE invoice_scan_lines SET qty_milli=?3, corrected=1 WHERE scan_id=?1 AND line_no=?2", params![id, p.line_no, q])?;
                changed.insert("qty_milli".into(), json!(q));
            }
            if let Some(c) = p.unit_cost_minor {
                validate::money_non_negative(c, "Unit cost")?;
                tx.execute("UPDATE invoice_scan_lines SET unit_cost_minor=?3, corrected=1 WHERE scan_id=?1 AND line_no=?2", params![id, p.line_no, c])?;
                changed.insert("unit_cost_minor".into(), json!(c));
            }
            if let Some(u) = &p.unit {
                let u = u.trim().to_lowercase();
                if !["", "pcs", "ctn", "pkt", "box", "kg"].contains(&u.as_str()) {
                    return Err(AppError::validation("Unit must be pcs, ctn, pkt, box or kg."));
                }
                tx.execute("UPDATE invoice_scan_lines SET unit=?3, corrected=1 WHERE scan_id=?1 AND line_no=?2", params![id, p.line_no, (!u.is_empty()).then_some(u.clone())])?;
                changed.insert("unit".into(), json!(u));
            }
            if let Some(upc) = p.units_per_case {
                if upc != 0 && !(2..=1000).contains(&upc) {
                    return Err(AppError::validation("Units per case must be between 2 and 1,000."));
                }
                tx.execute(
                    "UPDATE invoice_scan_lines SET units_per_case=?3, corrected=1 WHERE scan_id=?1 AND line_no=?2",
                    params![id, p.line_no, (upc != 0).then_some(upc)],
                )?;
                changed.insert("units_per_case".into(), json!(upc));
            }
            if let Some(inc) = p.include {
                tx.execute("UPDATE invoice_scan_lines SET include=?3 WHERE scan_id=?1 AND line_no=?2", params![id, p.line_no, inc as i64])?;
                changed.insert("include".into(), json!(inc));
            }
            if let Some(n) = p.new_product {
                tx.execute("UPDATE invoice_scan_lines SET new_product=?3 WHERE scan_id=?1 AND line_no=?2", params![id, p.line_no, n as i64])?;
                changed.insert("new_product".into(), json!(n));
            }
            // Single-unit quantity from the (possibly corrected) case size: only
            // when the unit says the quantity is in cases.
            tx.execute(
                "UPDATE invoice_scan_lines SET
                   case_qty_milli = CASE WHEN unit IN ('ctn','box') THEN qty_milli ELSE NULL END,
                   base_qty_milli = CASE WHEN unit IN ('ctn','box') AND units_per_case IS NOT NULL THEN qty_milli * units_per_case
                                         WHEN unit IN ('ctn','box') THEN NULL ELSE qty_milli END,
                   pack_clear = CASE WHEN unit IN ('ctn','box') AND units_per_case IS NULL THEN 0 WHEN unit IS NULL AND units_per_case IS NOT NULL AND corrected=0 THEN pack_clear ELSE 1 END
                 WHERE scan_id=?1 AND line_no=?2",
                params![id, p.line_no],
            )?;
            if changed.is_empty() {
                return Ok(());
            }
            if let Some(sid) = &supplier {
                if changed.contains_key("product_id") || changed.contains_key("units_per_case") {
                    learn_line(tx, &id, p.line_no, sid, &s.user_id)?;
                }
            }
            bump(tx, &id)?;
            changed.insert("line_no".into(), json!(p.line_no));
            decision(tx, &id, "line_correction", "person", None, &Value::Object(changed.clone()), Some(&s.user_id))?;
            audit::record(tx, &actor, "invoice_scan.line_corrected", "invoice_scan", Some(&id), None, Some(&Value::Object(changed)))?;
            recheck(self, tx, &id, &[])
        })?;
        self.doc_get(token, &id)
    }

    /// A proposed catalogue product for an unmatched line, built only from
    /// what the document shows. Nothing is created: the product form opens
    /// with these values and a person saves it.
    pub fn doc_new_product_draft(&self, token: &str, id: &str, line_no: i64) -> AppResult<Value> {
        let _ = self.doc_session(token)?;
        let id = validate::id(id, "Document")?;
        self.db.read(|c| {
            let (desc, barcode, valid, upc, supplier): (Option<String>, Option<String>, Option<i64>, Option<i64>, Option<String>) = c
                .query_row(
                    "SELECT l.description, l.barcode, l.barcode_valid, l.units_per_case, s.supplier_id FROM invoice_scan_lines l JOIN invoice_scans s ON s.scan_id=l.scan_id
                     WHERE l.scan_id=?1 AND l.line_no=?2",
                    params![id, line_no],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Document line"))?;
            let line = load_lines(c, &id)?.into_iter().find(|l| l.line_no == line_no).unwrap_or_default();
            let cost = line.receive_unit_cost().map(|x| x.0);
            // A category only when the closest real product is clearly similar.
            let cands: Vec<matching::Candidate> =
                serde_json::from_str(&c.query_row("SELECT COALESCE(candidates_json,'[]') FROM invoice_scan_lines WHERE scan_id=?1 AND line_no=?2", params![id, line_no], |r| r.get::<_, String>(0))?)
                    .unwrap_or_default();
            let category: Option<(String, String)> = match cands.first().filter(|c| c.score >= 55) {
                Some(best) => c
                    .query_row(
                        "SELECT c.category_id, c.name FROM products p JOIN categories c ON c.category_id=p.category_id WHERE p.product_id=?1",
                        [&best.id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?,
                None => None,
            };
            let supplier_name: Option<String> = match &supplier {
                Some(s) => c.query_row("SELECT name FROM suppliers WHERE supplier_id=?1", [s], |r| r.get(0)).optional()?,
                None => None,
            };
            Ok(json!({
                "name": desc,
                "barcode": barcode.filter(|_| valid == Some(1)),
                "barcode_printed": null,
                "supplier_id": supplier,
                "supplier_name": supplier_name,
                "units_per_case": upc,
                "purchase_cost_minor": cost,
                "suggested_category": category.map(|(id, name)| json!({ "category_id": id, "name": name, "reason": "category of the closest existing product" })),
                "selling_price": null,
                "selling_price_note": "No pricing rule is configured, so no selling price is suggested; enter it yourself.",
                "created": false
            }))
        })
    }

    /// Draft supplier invoice / credit note from a reviewed document. It is a
    /// record for review only: no payable, payment or stock follows.
    pub fn doc_create_supplier_invoice(&self, token: &str, id: &str, revision: i64) -> AppResult<Value> {
        let s = self.doc_session(token)?;
        s.require("purchasing.manage")?;
        let id = validate::id(id, "Document")?;
        let actor = self.actor(&s, None);
        let inv_id = self.db.write(|tx| {
            let (_, supplier) = check_revision(tx, &id, revision, &["review", "confirmed"])?;
            let supplier = supplier.ok_or_else(|| AppError::validation("Choose the supplier first."))?;
            let (doc_type, fields, validation, po, rd): (String, Option<String>, Option<String>, Option<String>, Option<String>) = tx.query_row(
                "SELECT doc_type, fields_json, validation_json, po_id, receiving_draft_id FROM invoice_scans WHERE scan_id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            let kind = match doc_type.as_str() {
                "invoice" => "invoice",
                "credit_note" => "credit_note",
                _ => return Err(AppError::validation("Confirm the document type (invoice or credit note) first.")),
            };
            let f: DocFields = j(fields);
            let v: Validation = j(validation);
            let lines: Vec<LineData> = load_lines(tx, &id)?.into_iter().filter(|l| l.include).collect();
            if lines.is_empty() {
                return Err(AppError::validation("Include at least one line."));
            }
            if let Some(l) = lines.iter().find(|l| l.line_net().is_none() || l.qty_milli.is_none()) {
                return Err(AppError::validation(format!("Line {} needs a quantity and an amount (or exclude it).", l.line_no)));
            }
            let subtotal = f.subtotal_minor.value.unwrap_or(v.lines_net_minor);
            let vat = f.vat_minor.value.or(v.calc_vat_minor).unwrap_or(0);
            let total = f.total_minor.value.unwrap_or(subtotal + vat);
            let inv_id = new_id();
            let number = format!("SI-{:05}", next_seq(tx, "supplier_invoice")?);
            let now = time::now_str();
            tx.execute(
                "INSERT INTO supplier_invoices(invoice_id, number, doc_type, supplier_id, scan_id, invoice_number, invoice_date, due_date, po_id, receiving_draft_id,
                    subtotal_minor, vat_minor, total_minor, status, posting, created_by, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'draft','not_supported',?14,?15,?15)",
                params![inv_id, number, kind, supplier, id, f.invoice_number.value, f.invoice_date.value, f.due_date.value, po, rd, subtotal, vat, total, s.user_id, now],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(x, _) if x.code == rusqlite::ErrorCode::ConstraintViolation => {
                    AppError::conflict("A supplier invoice was already created from this document.")
                }
                e => e.into(),
            })?;
            for (i, l) in lines.iter().enumerate() {
                tx.execute(
                    "INSERT INTO supplier_invoice_lines(invoice_id, line_no, product_id, description, qty_milli, unit_cost_minor, vat_rate_bp, vat_minor, line_total_minor)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![inv_id, i as i64 + 1, l.product_id, l.description, l.qty_milli, l.unit_cost_minor.unwrap_or(0), l.vat_rate_bp, l.vat_minor, l.line_net().unwrap_or(0)],
                )?;
            }
            tx.execute("UPDATE invoice_scans SET supplier_invoice_id=?2, status='confirmed', revision=revision+1 WHERE scan_id=?1", params![id, inv_id])?;
            decision(tx, &id, "supplier_invoice_draft", "person", None, &json!({ "invoice_id": inv_id, "doc_type": kind, "total_minor": total }), Some(&s.user_id))?;
            audit::record(tx, &actor, "supplier_invoice.drafted", "supplier_invoice", Some(&inv_id), None, Some(&json!({ "scan_id": id, "number": number, "doc_type": kind })))?;
            Ok(inv_id)
        })?;
        self.supplier_invoice_get(token, &inv_id)
    }

    /// Draft receiving from a reviewed invoice / delivery note. Stock does not
    /// move until someone with receiving rights posts it.
    pub fn doc_create_receiving(&self, token: &str, id: &str, revision: i64) -> AppResult<Value> {
        let s = self.doc_session(token)?;
        s.require("purchasing.manage")?;
        let id = validate::id(id, "Document")?;
        let actor = self.actor(&s, None);
        let draft_id = self.db.write(|tx| {
            let (_, supplier) = check_revision(tx, &id, revision, &["review", "confirmed"])?;
            let supplier = supplier.ok_or_else(|| AppError::validation("Choose the supplier first."))?;
            let (doc_type, po, number): (String, Option<String>, Option<String>) =
                tx.query_row("SELECT doc_type, po_id, invoice_number FROM invoice_scans WHERE scan_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            match doc_type.as_str() {
                "invoice" | "delivery_note" => {}
                "credit_note" => return Err(AppError::validation("A credit note does not receive stock. Create a supplier credit draft instead.")),
                _ => return Err(AppError::validation("Confirm the document type first.")),
            }
            let lines: Vec<LineData> = load_lines(tx, &id)?.into_iter().filter(|l| l.include).collect();
            if lines.is_empty() {
                return Err(AppError::validation("Include at least one line."));
            }
            for l in &lines {
                if l.product_id.is_none() {
                    return Err(AppError::validation(format!("Line {} is not matched to a product: match it, create the product first, or exclude it.", l.line_no)));
                }
                if l.receive_qty().unwrap_or(0) <= 0 || l.receive_unit_cost().is_none() {
                    return Err(AppError::validation(format!("Line {} needs a quantity and a cost.", l.line_no)));
                }
            }
            let did = new_id();
            let num = format!("RD-{:05}", next_seq(tx, "receiving_draft")?);
            let now = time::now_str();
            tx.execute(
                "INSERT INTO receiving_drafts(draft_id, number, supplier_id, po_id, scan_id, reference, status, created_by, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,'draft',?7,?8,?8)",
                params![did, num, supplier, po, id, number, s.user_id, now],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(x, _) if x.code == rusqlite::ErrorCode::ConstraintViolation => AppError::conflict("A receiving draft already exists for this document."),
                e => e.into(),
            })?;
            for (i, l) in lines.iter().enumerate() {
                let (cost, _) = l.receive_unit_cost().unwrap_or((0, true));
                tx.execute(
                    "INSERT INTO receiving_draft_lines(draft_id, line_no, product_id, description, qty_milli, unit_cost_minor, case_qty_milli, units_per_case, po_item_id, scan_line_no)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    params![
                        did,
                        i as i64 + 1,
                        l.product_id,
                        l.description,
                        l.receive_qty(),
                        cost,
                        l.units_per_case.and(l.qty_milli),
                        l.units_per_case,
                        l.po_item_id.clone().filter(|_| po.is_some()),
                        l.line_no
                    ],
                )?;
            }
            tx.execute("UPDATE invoice_scans SET receiving_draft_id=?2, status='confirmed', revision=revision+1 WHERE scan_id=?1", params![id, did])?;
            tx.execute("UPDATE supplier_invoices SET receiving_draft_id=?2 WHERE scan_id=?1 AND status<>'void'", params![id, did])?;
            decision(tx, &id, "receiving_draft", "person", None, &json!({ "draft_id": did, "lines": lines.len() }), Some(&s.user_id))?;
            audit::record(tx, &actor, "receiving.drafted", "receiving_draft", Some(&did), None, Some(&json!({ "scan_id": id, "number": num })))?;
            Ok(did)
        })?;
        self.receiving_draft_get(token, &draft_id)
    }

    pub fn receiving_drafts_list(&self, token: &str, status: Option<String>) -> AppResult<Vec<Value>> {
        let s = self.session(token)?;
        if !s.has("purchasing.manage") {
            s.require("inventory.receive")?;
        }
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT d.draft_id, d.number, d.status, s.name, d.po_id, po.po_number, d.reference, d.created_at,
                        (SELECT COUNT(*) FROM receiving_draft_lines l WHERE l.draft_id=d.draft_id),
                        (SELECT COALESCE(SUM(l.qty_milli * l.unit_cost_minor / 1000),0) FROM receiving_draft_lines l WHERE l.draft_id=d.draft_id)
                 FROM receiving_drafts d JOIN suppliers s ON s.supplier_id=d.supplier_id LEFT JOIN purchase_orders po ON po.po_id=d.po_id
                 WHERE (?1 IS NULL OR d.status=?1) ORDER BY d.created_at DESC LIMIT 300",
            )?;
            let rows = st
                .query_map([status.filter(|x| !x.is_empty())], |r| {
                    Ok(json!({ "draft_id": r.get::<_, String>(0)?, "number": r.get::<_, String>(1)?, "status": r.get::<_, String>(2)?, "supplier_name": r.get::<_, String>(3)?,
                               "po_id": r.get::<_, Option<String>>(4)?, "po_number": r.get::<_, Option<String>>(5)?, "reference": r.get::<_, Option<String>>(6)?,
                               "created_at": r.get::<_, String>(7)?, "lines": r.get::<_, i64>(8)?, "estimate_minor": r.get::<_, i64>(9)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn receiving_draft_get(&self, token: &str, draft_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("purchasing.manage") {
            s.require("inventory.receive")?;
        }
        let id = validate::id(draft_id, "Receiving draft")?;
        self.db.read(|c| {
            let head = c
                .query_row(
                    "SELECT d.draft_id, d.number, d.status, d.supplier_id, s.name, d.po_id, po.po_number, po.status, d.scan_id, x.scan_number, d.reference, d.receipt_ids,
                            d.created_at, d.posted_at, u.display_name, d.revision
                     FROM receiving_drafts d JOIN suppliers s ON s.supplier_id=d.supplier_id LEFT JOIN purchase_orders po ON po.po_id=d.po_id
                     LEFT JOIN invoice_scans x ON x.scan_id=d.scan_id LEFT JOIN users u ON u.user_id=d.posted_by WHERE d.draft_id=?1",
                    [&id],
                    |r| {
                        Ok(json!({ "draft_id": r.get::<_, String>(0)?, "number": r.get::<_, String>(1)?, "status": r.get::<_, String>(2)?, "supplier_id": r.get::<_, String>(3)?,
                                   "supplier_name": r.get::<_, String>(4)?, "po_id": r.get::<_, Option<String>>(5)?, "po_number": r.get::<_, Option<String>>(6)?,
                                   "po_status": r.get::<_, Option<String>>(7)?, "scan_id": r.get::<_, Option<String>>(8)?, "scan_number": r.get::<_, Option<String>>(9)?,
                                   "reference": r.get::<_, Option<String>>(10)?, "receipt_ids": opt_json(r.get(11)?), "created_at": r.get::<_, String>(12)?,
                                   "posted_at": r.get::<_, Option<String>>(13)?, "posted_by_name": r.get::<_, Option<String>>(14)?, "revision": r.get::<_, i64>(15)? }))
                    },
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Receiving draft"))?;
            let mut st = c.prepare(
                "SELECT l.line_no, l.product_id, p.name, l.description, l.qty_milli, l.unit_cost_minor, l.case_qty_milli, l.units_per_case, l.po_item_id, l.scan_line_no
                 FROM receiving_draft_lines l JOIN products p ON p.product_id=l.product_id WHERE l.draft_id=?1 ORDER BY l.line_no",
            )?;
            let lines: Vec<Value> = st
                .query_map([&id], |r| {
                    Ok(json!({ "line_no": r.get::<_, i64>(0)?, "product_id": r.get::<_, String>(1)?, "product_name": r.get::<_, String>(2)?, "description": r.get::<_, String>(3)?,
                               "qty_milli": r.get::<_, i64>(4)?, "unit_cost_minor": r.get::<_, i64>(5)?, "case_qty_milli": r.get::<_, Option<i64>>(6)?,
                               "units_per_case": r.get::<_, Option<i64>>(7)?, "po_item_id": r.get::<_, Option<String>>(8)?, "scan_line_no": r.get::<_, Option<i64>>(9)? }))
                })?
                .collect::<Result<_, _>>()?;
            let total: i64 = lines.iter().map(|l| crate::money::extend(l["unit_cost_minor"].as_i64().unwrap_or(0), l["qty_milli"].as_i64().unwrap_or(0)).unwrap_or(0)).sum();
            let mut h = head;
            h["lines"] = json!(lines);
            h["total_minor"] = json!(total);
            Ok(h)
        })
    }

    /// Edit or remove a line of a receiving draft (before posting).
    pub fn receiving_draft_update_line(
        &self,
        token: &str,
        draft_id: &str,
        line_no: i64,
        qty_milli: Option<i64>,
        unit_cost_minor: Option<i64>,
        remove: bool,
    ) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("purchasing.manage")?;
        let id = validate::id(draft_id, "Receiving draft")?;
        self.db.write(|tx| {
            let st: String = tx
                .query_row("SELECT status FROM receiving_drafts WHERE draft_id=?1", [&id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Receiving draft"))?;
            if st != "draft" {
                return Err(AppError::conflict("Only a draft can be edited."));
            }
            if remove {
                tx.execute("DELETE FROM receiving_draft_lines WHERE draft_id=?1 AND line_no=?2", params![id, line_no])?;
            }
            if let Some(q) = qty_milli {
                validate::qty_positive(q, true, "Quantity")?;
                tx.execute("UPDATE receiving_draft_lines SET qty_milli=?3 WHERE draft_id=?1 AND line_no=?2", params![id, line_no, q])?;
            }
            if let Some(cst) = unit_cost_minor {
                validate::money_non_negative(cst, "Unit cost")?;
                tx.execute(
                    "UPDATE receiving_draft_lines SET unit_cost_minor=?3 WHERE draft_id=?1 AND line_no=?2",
                    params![id, line_no, cst],
                )?;
            }
            tx.execute("UPDATE receiving_drafts SET revision=revision+1, updated_at=?2 WHERE draft_id=?1", params![id, time::now_str()])?;
            Ok(())
        })?;
        self.receiving_draft_get(token, &id)
    }

    pub fn receiving_draft_cancel(&self, token: &str, draft_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("purchasing.manage")?;
        let id = validate::id(draft_id, "Receiving draft")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let n = tx.execute(
                "UPDATE receiving_drafts SET status='cancelled', updated_at=?2 WHERE draft_id=?1 AND status='draft'",
                params![id, time::now_str()],
            )?;
            if n == 0 {
                return Err(AppError::conflict("Only a draft can be cancelled."));
            }
            tx.execute("UPDATE invoice_scans SET receiving_draft_id=NULL WHERE receiving_draft_id=?1", [&id])?;
            audit::record(tx, &actor, "receiving.draft_cancelled", "receiving_draft", Some(&id), None, None)?;
            Ok(())
        })?;
        self.receiving_draft_get(token, &id)
    }

    /// Post a receiving draft: a person with receiving rights confirms it and
    /// the normal receiving writes the goods receipt, costs and stock
    /// (lines on the PO through PO receiving, the rest as direct receiving).
    /// Safe to retry with the same operation id.
    pub fn receiving_draft_post(&self, token: &str, draft_id: &str, operation_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("inventory.receive")?;
        let id = validate::id(draft_id, "Receiving draft")?;
        let op = operation_id.trim();
        if op.is_empty() || op.len() > 100 {
            return Err(AppError::validation("An operation id is required."));
        }
        let head = self.receiving_draft_get(token, &id)?;
        if head["status"] == "posted" {
            return Ok(head);
        }
        if head["status"] != "draft" {
            return Err(AppError::conflict("Only a draft can be posted."));
        }
        let lines = head["lines"].as_array().cloned().unwrap_or_default();
        if lines.is_empty() {
            return Err(AppError::validation("The draft has no lines."));
        }
        let po = head["po_id"].as_str().map(|x| x.to_string());
        let (po_lines, direct): (Vec<&Value>, Vec<&Value>) = lines.iter().partition(|l| po.is_some() && l["po_item_id"].is_string());
        let mut receipts = vec![];
        if let Some(po_id) = &po {
            if !po_lines.is_empty() {
                let st = head["po_status"].as_str().unwrap_or("");
                if !matches!(st, "ordered" | "partially_received") {
                    return Err(AppError::conflict(format!(
                        "Purchase order {} is {st}: place the order first (Purchasing), then post this receiving.",
                        head["po_number"].as_str().unwrap_or("")
                    )));
                }
                let req = crate::purchasing::PoReceiveRequest {
                    po_id: po_id.clone(),
                    reference: head["reference"].as_str().map(|x| x.to_string()),
                    lines: po_lines
                        .iter()
                        .map(|l| crate::purchasing::PoReceiveLine {
                            po_item_id: l["po_item_id"].as_str().unwrap_or_default().to_string(),
                            qty_milli: l["qty_milli"].as_i64().unwrap_or(0),
                            unit_cost_minor: l["unit_cost_minor"].as_i64(),
                        })
                        .collect(),
                    operation_id: format!("{op}-po"),
                };
                self.purchase_order_receive(token, req)?;
                receipts.push(json!({ "kind": "po", "operation_id": format!("{op}-po") }));
            }
        }
        if !direct.is_empty() {
            let req = crate::inventory::ReceiveRequest {
                supplier_id: head["supplier_id"].as_str().map(|x| x.to_string()),
                reference: head["reference"].as_str().map(|x| x.to_string()),
                lines: direct
                    .iter()
                    .map(|l| crate::inventory::ReceiveLine {
                        product_id: l["product_id"].as_str().unwrap_or_default().to_string(),
                        qty_milli: l["qty_milli"].as_i64().unwrap_or(0),
                        unit_cost_minor: l["unit_cost_minor"].as_i64().unwrap_or(0),
                        po_item_id: None,
                    })
                    .collect(),
                operation_id: format!("{op}-direct"),
            };
            let r = self.inventory_receive(token, req)?;
            receipts.push(json!({ "kind": "direct", "receipt_id": r["receipt_id"] }));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            tx.execute(
                "UPDATE receiving_drafts SET status='posted', receipt_ids=?2, posted_by=?3, posted_at=?4, updated_at=?4 WHERE draft_id=?1 AND status='draft'",
                params![id, json!(receipts).to_string(), s.user_id, time::now_str()],
            )?;
            audit::record(tx, &actor, "receiving.posted", "receiving_draft", Some(&id), None, Some(&json!({ "receipts": receipts })))?;
            Ok(())
        })?;
        self.receiving_draft_get(token, &id)
    }

    pub fn supplier_invoices_list(&self, token: &str, status: Option<String>) -> AppResult<Vec<Value>> {
        let s = self.session(token)?;
        s.require("purchasing.manage")?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT i.invoice_id, i.number, i.doc_type, i.status, s.name, i.invoice_number, i.invoice_date, i.total_minor, i.created_at
                 FROM supplier_invoices i JOIN suppliers s ON s.supplier_id=i.supplier_id WHERE (?1 IS NULL OR i.status=?1) ORDER BY i.created_at DESC LIMIT 300",
            )?;
            let rows = st
                .query_map([status.filter(|x| !x.is_empty())], |r| {
                    Ok(json!({ "invoice_id": r.get::<_, String>(0)?, "number": r.get::<_, String>(1)?, "doc_type": r.get::<_, String>(2)?, "status": r.get::<_, String>(3)?,
                               "supplier_name": r.get::<_, String>(4)?, "invoice_number": r.get::<_, Option<String>>(5)?, "invoice_date": r.get::<_, Option<String>>(6)?,
                               "total_minor": r.get::<_, i64>(7)?, "created_at": r.get::<_, String>(8)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn supplier_invoice_get(&self, token: &str, invoice_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("purchasing.manage")?;
        let id = validate::id(invoice_id, "Supplier invoice")?;
        self.db.read(|c| {
            let mut h = c
                .query_row(
                    "SELECT i.invoice_id, i.number, i.doc_type, i.status, i.posting, i.supplier_id, s.name, i.scan_id, x.scan_number, i.invoice_number, i.invoice_date,
                            i.due_date, i.po_id, po.po_number, i.receiving_draft_id, i.subtotal_minor, i.vat_minor, i.total_minor, i.created_at, i.approved_at, u.display_name
                     FROM supplier_invoices i JOIN suppliers s ON s.supplier_id=i.supplier_id LEFT JOIN invoice_scans x ON x.scan_id=i.scan_id
                     LEFT JOIN purchase_orders po ON po.po_id=i.po_id LEFT JOIN users u ON u.user_id=i.approved_by WHERE i.invoice_id=?1",
                    [&id],
                    |r| {
                        Ok(json!({ "invoice_id": r.get::<_, String>(0)?, "number": r.get::<_, String>(1)?, "doc_type": r.get::<_, String>(2)?, "status": r.get::<_, String>(3)?,
                                   "posting": r.get::<_, String>(4)?, "supplier_id": r.get::<_, String>(5)?, "supplier_name": r.get::<_, String>(6)?,
                                   "scan_id": r.get::<_, Option<String>>(7)?, "scan_number": r.get::<_, Option<String>>(8)?, "invoice_number": r.get::<_, Option<String>>(9)?,
                                   "invoice_date": r.get::<_, Option<String>>(10)?, "due_date": r.get::<_, Option<String>>(11)?, "po_id": r.get::<_, Option<String>>(12)?,
                                   "po_number": r.get::<_, Option<String>>(13)?, "receiving_draft_id": r.get::<_, Option<String>>(14)?, "subtotal_minor": r.get::<_, i64>(15)?,
                                   "vat_minor": r.get::<_, i64>(16)?, "total_minor": r.get::<_, i64>(17)?, "created_at": r.get::<_, String>(18)?,
                                   "approved_at": r.get::<_, Option<String>>(19)?, "approved_by_name": r.get::<_, Option<String>>(20)? }))
                    },
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Supplier invoice"))?;
            let mut st = c.prepare(
                "SELECT l.line_no, l.product_id, p.name, l.description, l.qty_milli, l.unit_cost_minor, l.vat_rate_bp, l.vat_minor, l.line_total_minor
                 FROM supplier_invoice_lines l LEFT JOIN products p ON p.product_id=l.product_id WHERE l.invoice_id=?1 ORDER BY l.line_no",
            )?;
            let lines: Vec<Value> = st
                .query_map([&id], |r| {
                    Ok(json!({ "line_no": r.get::<_, i64>(0)?, "product_id": r.get::<_, Option<String>>(1)?, "product_name": r.get::<_, Option<String>>(2)?,
                               "description": r.get::<_, String>(3)?, "qty_milli": r.get::<_, i64>(4)?, "unit_cost_minor": r.get::<_, i64>(5)?,
                               "vat_rate_bp": r.get::<_, Option<i64>>(6)?, "vat_minor": r.get::<_, Option<i64>>(7)?, "line_total_minor": r.get::<_, i64>(8)? }))
                })?
                .collect::<Result<_, _>>()?;
            h["lines"] = json!(lines);
            h["posting_note"] = json!("AMWAPOS has no supplier payables ledger: approving records the review only. No liability, payment or stock is created.");
            Ok(h)
        })
    }

    pub fn supplier_invoice_set_status(&self, token: &str, invoice_id: &str, status: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("purchasing.manage")?;
        let id = validate::id(invoice_id, "Supplier invoice")?;
        if !matches!(status, "approved" | "void") {
            return Err(AppError::validation("Status must be approved or void."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let cur: String = tx.query_row("SELECT status FROM supplier_invoices WHERE invoice_id=?1", [&id], |r| r.get(0)).optional()?.ok_or_else(|| AppError::not_found("Supplier invoice"))?;
            if cur != "draft" && !(cur == "approved" && status == "void") {
                return Err(AppError::conflict("This supplier invoice can no longer be changed."));
            }
            let now = time::now_str();
            if status == "approved" {
                tx.execute(
                    "UPDATE supplier_invoices SET status='approved', approved_by=?2, approved_at=?3, updated_at=?3, revision=revision+1 WHERE invoice_id=?1",
                    params![id, s.user_id, now],
                )?;
            } else {
                tx.execute("UPDATE supplier_invoices SET status='void', updated_at=?2, revision=revision+1 WHERE invoice_id=?1", params![id, now])?;
                tx.execute("UPDATE invoice_scans SET supplier_invoice_id=NULL WHERE supplier_invoice_id=?1", [&id])?;
            }
            audit::record(tx, &actor, &format!("supplier_invoice.{status}"), "supplier_invoice", Some(&id), Some(&json!({ "status": cur })), Some(&json!({ "status": status })))?;
            Ok(())
        })?;
        self.supplier_invoice_get(token, &id)
    }

    // ------------------------------------------------------------ AI

    /// Candidate product ids already offered for this document (what a model may pick from).
    fn doc_allowed_products(&self, c: &Connection, id: &str) -> AppResult<Vec<String>> {
        let mut st = c.prepare("SELECT candidates_json FROM invoice_scan_lines WHERE scan_id=?1")?;
        let rows: Vec<Option<String>> = st.query_map([id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        let mut out = vec![];
        for r in rows {
            let cands: Vec<matching::Candidate> = r.and_then(|x| serde_json::from_str(&x).ok()).unwrap_or_default();
            for c in cands {
                if !out.contains(&c.id) {
                    out.push(c.id);
                }
            }
        }
        Ok(out)
    }

    /// The AI reading prompt: numbered OCR lines and, per rules line, the real
    /// candidate products (the only ids the model may return).
    pub(crate) fn doc_ai_prompt(&self, c: &Connection, id: &str) -> AppResult<Option<String>> {
        let status: Option<String> = c.query_row("SELECT status FROM invoice_scans WHERE scan_id=?1", [id], |r| r.get(0)).optional()?;
        if status.as_deref() != Some("review") {
            return Ok(None);
        }
        let layout = self.doc_layout(c, id)?;
        if layout.word_count() == 0 {
            return Ok(None);
        }
        let numbered: String = layout.lines().iter().map(|l| format!("{}: {}", l.index, l.line.text())).collect::<Vec<_>>().join("\n");
        let mut st = c.prepare("SELECT line_no, description, candidates_json FROM invoice_scan_lines WHERE scan_id=?1 ORDER BY line_no")?;
        let cands: Vec<Value> = st
            .query_map([id], |r| {
                let cj: Option<String> = r.get(2)?;
                let list: Vec<matching::Candidate> = cj.and_then(|x| serde_json::from_str(&x).ok()).unwrap_or_default();
                Ok(json!({ "line": r.get::<_, i64>(0)?, "text": r.get::<_, Option<String>>(1)?, "candidates": list.iter().map(|c| json!({ "product_id": c.id, "name": c.name })).collect::<Vec<_>>() }))
            })?
            .collect::<Result<_, _>>()?;
        let body = json!({ "ocr_lines": numbered.chars().take(20_000).collect::<String>(), "catalogue_candidates": cands });
        Ok(Some(body.to_string()))
    }

    /// Apply a model reading. Values are validated (`aischema`); lines replace
    /// the rules reading only when that reading is missing or does not add up
    /// and the model's does. A person still reviews everything.
    pub fn doc_apply_ai(&self, id: &str, v: &Value, model: &str) -> AppResult<bool> {
        self.db.write(|tx| {
            let status: Option<String> = tx.query_row("SELECT status FROM invoice_scans WHERE scan_id=?1", [id], |r| r.get(0)).optional()?;
            if status.as_deref() != Some("review") {
                return Ok(false);
            }
            let digits = self.currency(tx)?.1;
            let layout = self.doc_layout(tx, id)?;
            let allowed = self.doc_allowed_products(tx, id)?;
            let ai = match super::aischema::validate(v, &layout, digits, &allowed) {
                Ok(a) => a,
                Err(e) => {
                    decision(tx, id, "ai_rejected", "ai", Some(model), &json!({ "error": e }), None)?;
                    return Ok(false);
                }
            };
            let mut f: DocFields = j(tx.query_row("SELECT fields_json FROM invoice_scans WHERE scan_id=?1", [id], |r| r.get(0))?);
            // Header: fill what the rules could not read (never override a person).
            macro_rules! fill {
                ($name:ident) => {
                    if (!f.$name.is_set() || f.$name.status == "ambiguous") && f.$name.source != "person" && ai.fields.$name.is_set() {
                        let mut x = ai.fields.$name.clone();
                        if f.$name.status == "ambiguous" && f.$name.value != x.value {
                            x.status = "ambiguous".into();
                            x.note = Some("The AI reading differs from the printed date order; check it.".into());
                        }
                        f.$name = x;
                    }
                };
            }
            fill!(invoice_number);
            fill!(invoice_date);
            fill!(due_date);
            fill!(supplier_name);
            fill!(supplier_vat);
            fill!(supplier_cr);
            fill!(subtotal_minor);
            fill!(vat_minor);
            fill!(total_minor);
            fill!(vat_rate_bp);
            store_header(tx, id, &f)?;
            if let Some(c) = &ai.classification {
                tx.execute(
                    "UPDATE invoice_scans SET doc_type=?2, doc_type_band='medium', doc_type_source=?3 WHERE scan_id=?1 AND doc_type='unknown' AND COALESCE(json_extract(doc_type_source,'$.source'),'')<>'person' AND COALESCE(doc_type_source,'')<>'person'",
                    params![id, c.doc_type.as_str(), json!({ "source": "ai", "reasons": ["AI reading"] }).to_string()],
                )?;
            }
            // Lines: compare the arithmetic of both readings.
            let rules_lines = load_lines(tx, id)?;
            let rates: Vec<i64> = vec![];
            let rules_ok = !rules_lines.is_empty() && checks::validate(&f, &rules_lines, &rates, &[]).arithmetic_ok;
            let corrected: i64 = tx.query_row("SELECT COUNT(*) FROM invoice_scan_lines WHERE scan_id=?1 AND corrected=1", [id], |r| r.get(0))?;
            let mut replaced = false;
            if !ai.lines.is_empty() && !rules_ok && corrected == 0 {
                let ai_data: Vec<LineData> = ai
                    .lines
                    .iter()
                    .enumerate()
                    .map(|(i, (l, _))| LineData {
                        line_no: i as i64 + 1,
                        description: l.description.clone(),
                        qty_milli: l.qty_milli,
                        base_qty_milli: l.pack.base_qty_milli,
                        unit_cost_minor: l.unit_cost_minor,
                        discount_minor: l.discount_minor,
                        vat_minor: l.vat_minor,
                        line_total_minor: l.line_total_minor,
                        include: true,
                        ..Default::default()
                    })
                    .collect();
                let ai_ok = checks::validate(&f, &ai_data, &rates, &[]).arithmetic_ok;
                if ai_ok || rules_lines.is_empty() {
                    let supplier: Option<String> = tx.query_row("SELECT supplier_id FROM invoice_scans WHERE scan_id=?1", [id], |r| r.get(0))?;
                    tx.execute("DELETE FROM invoice_scan_lines WHERE scan_id=?1", [id])?;
                    for (i, (l, pid)) in ai.lines.iter().enumerate() {
                        let (mut m, upc) = matching::match_line(tx, supplier.as_deref(), l)?;
                        let mut kind = None;
                        // The model may only choose among our candidates; deterministic
                        // strong matches (barcode, confirmed mapping) still win.
                        if let Some(p) = pid {
                            if m.band < Band::High && m.id.as_deref() != Some(p.as_str()) {
                                let name: Option<String> = tx.query_row("SELECT name FROM products WHERE product_id=?1 AND active=1", [p], |r| r.get(0)).optional()?;
                                if let Some(n) = name {
                                    m.alternatives.retain(|a| &a.id != p);
                                    m = MatchResult { id: Some(p.clone()), name: Some(n), kind: "ai".into(), score: 70, band: Band::Medium, reasons: vec!["Chosen by the AI reading from the catalogue candidates".into()], alternatives: m.alternatives };
                                    kind = Some("ai");
                                }
                            }
                        }
                        store_line(tx, id, i as i64 + 1, l, &m, upc, kind)?;
                    }
                    tx.execute("UPDATE invoice_scans SET parser='ai' WHERE scan_id=?1", [id])?;
                    replaced = true;
                }
            }
            tx.execute("UPDATE invoice_scans SET ai_model=?2, revision=revision+1 WHERE scan_id=?1", params![id, model])?;
            decision(tx, id, "ai_extraction", "ai", Some(model), &json!({ "lines": ai.lines.len(), "replaced_lines": replaced, "rejected": ai.rejected }), None)?;
            audit::record(tx, &system_actor(), "invoice_scan.ai_parsed", "invoice_scan", Some(id), None, Some(&json!({ "lines": ai.lines.len(), "replaced": replaced })))?;
            recheck(self, tx, id, &[])?;
            Ok(replaced || ai.fields.invoice_number.is_set() || ai.fields.total_minor.is_set())
        })
    }

    /// Counts for the Document Intelligence dashboard (no document content).
    pub fn doc_metrics(&self, token: &str) -> AppResult<Value> {
        let _ = self.doc_session(token)?;
        self.db.read(|c| {
            let n = |sql: &str| -> AppResult<i64> { Ok(c.query_row(sql, [], |r| r.get(0))?) };
            let total = n("SELECT COUNT(*) FROM invoice_scans")?;
            let failed = n("SELECT COUNT(*) FROM invoice_scans WHERE status='failed'")?;
            let read = n("SELECT COUNT(*) FROM invoice_scans WHERE status IN ('review','confirmed','rejected')")?;
            let with_lines = n("SELECT COUNT(DISTINCT scan_id) FROM invoice_scan_lines")?;
            let supplier_auto = n("SELECT COUNT(*) FROM invoice_scans WHERE json_extract(supplier_match_json,'$.kind') NOT IN ('person','none') AND json_extract(supplier_match_json,'$.band') IN ('high','medium')")?;
            let lines = n("SELECT COUNT(*) FROM invoice_scan_lines")?;
            let auto = n("SELECT COUNT(*) FROM invoice_scan_lines WHERE match_band IN ('high','medium') AND corrected=0 AND product_id IS NOT NULL")?;
            let corrected_docs = n("SELECT COUNT(*) FROM invoice_scans WHERE corrections>0")?;
            let dups = n("SELECT COUNT(*) FROM invoice_scans WHERE duplicate_json IS NOT NULL AND duplicate_json<>'[]'")?;
            let avg: Option<f64> = c.query_row(
                "SELECT AVG((julianday(finished_at)-julianday(started_at))*86400.0) FROM invoice_scans WHERE finished_at IS NOT NULL AND started_at IS NOT NULL",
                [],
                |r| r.get(0),
            )?;
            let rate = |a: i64, b: i64| if b == 0 { Value::Null } else { json!((a * 1000 / b) as f64 / 10.0) };
            Ok(json!({
                "documents": total, "read": read, "failed": failed, "ocr_failure_pct": rate(failed, total), "extraction_success_pct": rate(with_lines, read),
                "supplier_auto_match_pct": rate(supplier_auto, read), "lines": lines, "product_auto_match_pct": rate(auto, lines),
                "correction_rate_pct": rate(corrected_docs, read), "duplicates_detected": dups, "avg_processing_seconds": avg.map(|x| (x * 10.0).round() / 10.0),
            }))
        })
    }
}

/// Error kind helper for the worker.
pub fn is_unreadable(e: &AppError) -> bool {
    e.code == ErrorCode::Validation
}
