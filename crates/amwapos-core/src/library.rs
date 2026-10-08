//! Document Library (Wave 8, docs/INTELLIGENCE_AND_EVIDENCE.md).
//!
//! Evidence files kept with the business records they support: supplier
//! invoices, receipts, contracts, licences, delivery notes. A file is stored
//! once by the SHA-256 of its bytes; a document is the business record about
//! it (title, category, links). A document points at the records it is
//! evidence for; it never copies their amounts.
//!
//! Who can see a document: `documents.view`, plus the permission of every
//! record it is linked to (an expense receipt needs `expenses.view`, a supplier
//! invoice `payables.view`) and of its category (bank and tax papers need a
//! finance permission). Search, lists, the file itself and the assistant all
//! apply the same rule (`Scope`).
//!
//! Search is SQLite FTS5 over titles and the text read from the file (the
//! PDF's own text layer here, OCR on the hub for scans and photos). It works
//! with no AI provider. Text read from a document is DATA, never instructions.
//!
//! The library is kept on the hub computer (hub-local tables).

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ids::{new_id, next_seq};
use crate::service::AppCore;
use crate::setup::clean;
use crate::time;
use crate::validate;

pub const CATEGORIES: &[&str] = &[
    "invoice",
    "credit_note",
    "receipt",
    "delivery_note",
    "quotation",
    "price_list",
    "statement",
    "contract",
    "licence",
    "insurance",
    "bank",
    "tax",
    "other",
];

/// Categories that hold finance papers: any one of these permissions too.
const FINANCE_CATEGORIES: &[&str] = &["bank", "tax", "statement"];
const FINANCE_PERMS: &[&str] = &["payables.view", "expenses.view", "reports.financial"];

/// (entity type, table, id column, label column, any one of these permissions to see it)
pub const ENTITY_TYPES: &[(&str, &str, &str, &str, &[&str])] = &[
    ("supplier", "suppliers", "supplier_id", "name", &["suppliers.manage", "purchasing.manage", "payables.view"]),
    ("supplier_invoice", "supplier_invoices", "invoice_id", "number", &["payables.view"]),
    ("purchase_order", "purchase_orders", "po_id", "po_number", &["purchasing.manage", "purchasing.approve"]),
    ("expense", "expenses", "expense_id", "number", &["expenses.view"]),
    ("product", "products", "product_id", "name", &["products.view"]),
    ("case", "cases", "case_id", "case_number", &["cases.view"]),
    ("day_close", "day_closes", "close_id", "close_number", &["day.x_report", "day.close"]),
    ("supplier_return", "supplier_returns", "return_id", "number", &["supplier_returns.manage", "payables.view"]),
    ("requisition", "requisitions", "requisition_id", "number", &["requisitions.create", "purchasing.approve"]),
    ("customer", "customers", "customer_id", "name", &["customers.view"]),
    ("promotion", "promotions", "promotion_id", "name", &["promotions.manage"]),
];

/// Rows per document in the search index (row id = seq * ROWS + page).
const ROWS: i64 = 10_000;
/// Pages of text kept per file.
const MAX_PAGES: usize = 500;
/// Text kept per page.
const MAX_PAGE_CHARS: usize = 20_000;
/// Results per search.
pub const MAX_RESULTS: i64 = 20;
/// Older records adopted per call (the rest on the next one).
const ADOPT_BATCH: i64 = 500;

pub const MSG_NO_TEXT: &str = "Text could not be extracted.";
pub const MSG_HUB_ONLY: &str = "Documents are kept on the main computer. Open the library there.";

fn entity(t: &str) -> AppResult<&'static (&'static str, &'static str, &'static str, &'static str, &'static [&'static str])> {
    ENTITY_TYPES.iter().find(|e| e.0 == t).ok_or_else(|| AppError::validation("Choose what the document is for."))
}

/// What a person may see: the record types and categories open to them.
#[derive(Debug, Clone)]
pub struct Scope {
    pub entity_types: Vec<&'static str>,
    pub categories: Vec<&'static str>,
}

impl Scope {
    pub fn of(s: &Session) -> Scope {
        let any = |ps: &[&str]| ps.iter().any(|p| s.has(p));
        Scope {
            entity_types: ENTITY_TYPES.iter().filter(|e| any(e.4)).map(|e| e.0).collect(),
            categories: CATEGORIES.iter().copied().filter(|c| !FINANCE_CATEGORIES.contains(c) || any(FINANCE_PERMS)).collect(),
        }
    }

    /// SQL condition on `d` (library_documents). Values are this module's
    /// constants, never input.
    pub fn sql(&self) -> String {
        let list = |v: &[&str]| v.iter().map(|x| format!("'{x}'")).collect::<Vec<_>>().join(",");
        let cats = if self.categories.is_empty() { "''".to_string() } else { list(&self.categories) };
        let types = if self.entity_types.is_empty() { "''".to_string() } else { list(&self.entity_types) };
        format!(
            "d.category IN ({cats}) AND NOT EXISTS (SELECT 1 FROM library_links l WHERE l.document_id = d.document_id \
             AND l.removed_at IS NULL AND l.entity_type NOT IN ({types}))"
        )
    }

    pub fn can_see(&self, c: &Connection, document_id: &str) -> AppResult<bool> {
        let sql = format!("SELECT 1 FROM library_documents d WHERE d.document_id = ?1 AND {}", self.sql());
        Ok(c.query_row(&sql, [document_id], |_| Ok(())).optional()?.is_some())
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LinkInput {
    pub entity_type: String,
    pub entity_id: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct AddInput {
    pub file_name: String,
    pub data_base64: String,
    pub title: Option<String>,
    pub category: String,
    pub document_date: Option<String>,
    pub note: Option<String>,
    pub links: Vec<LinkInput>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct UpdateInput {
    pub title: Option<String>,
    pub category: Option<String>,
    /// "" clears.
    pub document_date: Option<String>,
    /// "" clears.
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ListFilter {
    pub q: Option<String>,
    pub category: Option<String>,
    /// active (default) | archived | replaced | all
    pub status: Option<String>,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

fn sha_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A stored path: relative to the data folder when inside it.
fn rel_path(data_dir: &Path, p: &Path) -> String {
    match p.strip_prefix(data_dir) {
        Ok(r) => r.to_string_lossy().replace('\\', "/"),
        Err(_) => p.to_string_lossy().into_owned(),
    }
}

pub fn abs_path(data_dir: &Path, stored: &str) -> PathBuf {
    let p = Path::new(stored);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        data_dir.join(p)
    }
}

fn check_date(d: &Option<String>) -> AppResult<Option<String>> {
    match d.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
        Some(x) => {
            chrono::NaiveDate::parse_from_str(x, "%Y-%m-%d").map_err(|_| AppError::validation("Enter the date as YYYY-MM-DD."))?;
            Ok(Some(x.to_string()))
        }
        None => Ok(None),
    }
}

fn check_category(c: &str) -> AppResult<&'static str> {
    CATEGORIES.iter().copied().find(|x| *x == c).ok_or_else(|| AppError::validation("Choose a category."))
}

fn opt_clean(v: &Option<String>, label: &str, max: usize) -> AppResult<Option<String>> {
    match v.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
        Some(x) => Ok(Some(clean(x, label, max, false)?)),
        None => Ok(None),
    }
}

/// Keep the text of one page: printable, bounded.
fn tidy(text: &str) -> String {
    let t: String = text.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').take(MAX_PAGE_CHARS).collect();
    t.trim().to_string()
}

/// Rebuild one document's rows in the search index.
pub fn index_document(c: &Connection, document_id: &str) -> AppResult<()> {
    let Some((seq, title, sha)): Option<(i64, String, String)> = c
        .query_row("SELECT seq, title, sha256 FROM library_documents WHERE document_id=?1", [document_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?
    else {
        return Ok(());
    };
    c.execute("DELETE FROM library_fts WHERE rowid BETWEEN ?1 AND ?2", params![seq * ROWS, seq * ROWS + ROWS - 1])?;
    let mut pages: Vec<(i64, String)> = {
        let mut st = c.prepare("SELECT page, text FROM library_file_pages WHERE sha256=?1 ORDER BY page")?;
        let rows = st.query_map([&sha], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    if !pages.iter().any(|p| p.0 == 0) {
        pages.insert(0, (0, String::new()));
    }
    for (page, text) in pages.into_iter().filter(|p| p.0 < ROWS) {
        c.execute("INSERT INTO library_fts(rowid, title, body) VALUES (?1,?2,?3)", params![seq * ROWS + page, title, text])?;
    }
    Ok(())
}

/// Rebuild the whole search index (it holds nothing that is not elsewhere).
pub fn reindex(c: &Connection) -> AppResult<usize> {
    c.execute("DELETE FROM library_fts", [])?;
    let ids: Vec<String> = {
        let mut st = c.prepare("SELECT document_id FROM library_documents ORDER BY seq")?;
        let rows = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for id in &ids {
        index_document(c, id)?;
    }
    Ok(ids.len())
}

/// Store the text read from a file and reindex the documents using it.
pub fn set_text(c: &Connection, sha: &str, pages: &[(u32, String)], source: &str, note: Option<&str>) -> AppResult<()> {
    c.execute("DELETE FROM library_file_pages WHERE sha256=?1", [sha])?;
    let mut kept = 0;
    for (p, t) in pages.iter().take(MAX_PAGES) {
        let t = tidy(t);
        if t.chars().filter(|c| c.is_alphanumeric()).count() < 3 {
            continue;
        }
        c.execute("INSERT OR REPLACE INTO library_file_pages(sha256, page, text) VALUES (?1,?2,?3)", params![sha, *p as i64, t])?;
        kept += 1;
    }
    let status = if kept > 0 { "extracted" } else { "none" };
    c.execute(
        "UPDATE library_files SET text_status=?2, text_source=?3, text_note=?4, page_count=COALESCE(page_count, ?5) WHERE sha256=?1",
        params![
            sha,
            status,
            if kept > 0 { Some(source) } else { None },
            note,
            if pages.is_empty() { None } else { Some(pages.len() as i64) }
        ],
    )?;
    let docs: Vec<String> = {
        let mut st = c.prepare("SELECT document_id FROM library_documents WHERE sha256=?1")?;
        let rows = st.query_map([sha], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for d in docs {
        index_document(c, &d)?;
    }
    Ok(())
}

/// Text from a PDF's own text layer, per page. Empty when it has none (a
/// scan): the hub's OCR reads those.
fn pdf_pages(bytes: &[u8]) -> Option<Vec<(u32, String)>> {
    let pages = crate::docintel::pdfdoc::read_pdf(bytes).ok()?;
    let texts: Vec<(u32, String)> = pages.iter().filter(|p| p.has_text()).map(|p| (p.number, p.text.clone())).collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts)
    }
}

fn insert_file(c: &Connection, sha: &str, path: &str, mime: &str, bytes: Option<i64>) -> AppResult<bool> {
    Ok(c.execute(
        "INSERT OR IGNORE INTO library_files(sha256, path, mime, bytes, text_status, created_at) VALUES (?1,?2,?3,?4,'pending',?5)",
        params![sha, path, mime, bytes, time::now_str()],
    )? == 1)
}

#[allow(clippy::too_many_arguments)]
fn insert_document(
    c: &Connection,
    sha: &str,
    title: &str,
    category: &str,
    original_name: &str,
    document_date: Option<&str>,
    note: Option<&str>,
    source: &str,
    branch_id: Option<&str>,
    added_by: &str,
    added_at: &str,
    version: i64,
    replaces: Option<&str>,
) -> AppResult<(String, String)> {
    let id = new_id();
    let seq = next_seq(c, "library_document")?;
    let number = format!("DOC-{seq:06}");
    c.execute(
        "INSERT INTO library_documents(document_id, seq, number, title, category, sha256, original_name, document_date, note,
           version, replaces_id, status, source, branch_id, added_by, added_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'active',?12,?13,?14,?15,?15)",
        params![
            id,
            seq,
            number,
            title,
            category,
            sha,
            original_name,
            document_date,
            note,
            version,
            replaces,
            source,
            branch_id,
            added_by,
            added_at
        ],
    )?;
    index_document(c, &id)?;
    Ok((id, number))
}

fn add_link(c: &Connection, document_id: &str, entity_type: &str, entity_id: &str, by: &str, at: &str) -> AppResult<bool> {
    Ok(c.execute(
        "INSERT OR IGNORE INTO library_links(link_id, document_id, entity_type, entity_id, linked_by, linked_at) VALUES (?1,?2,?3,?4,?5,?6)",
        params![new_id(), document_id, entity_type, entity_id, by, at],
    )? == 1)
}

fn entity_label(c: &Connection, entity_type: &str, entity_id: &str) -> AppResult<Option<String>> {
    let Ok(e) = entity(entity_type) else { return Ok(None) };
    let sql = format!("SELECT {} FROM {} WHERE {} = ?1", e.3, e.1, e.2);
    Ok(c.query_row(&sql, [entity_id], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())
}

type Row8 = (String, String, String, String, String, String, String, String);
type ScanRow = (String, String, String, String, Option<String>, String, Option<String>, String, String);
type ScanRows = (ScanRow, Option<String>, Option<String>, Option<String>);
type CaseRow8 = (String, String, String, String, String, String, Option<String>, String);

/// Bring earlier evidence into the library once: expense receipts, supplier
/// documents read by Document Intelligence and case evidence. Their files stay where
/// they are; identical bytes become one document linked to each record.
pub fn adopt_existing(c: &Connection, data_dir: &Path) -> AppResult<usize> {
    let mut n = 0;
    // Expense attachments.
    let rows: Vec<Row8> = {
        let mut st = c.prepare(
            "SELECT a.attachment_id, a.expense_id, a.file_name, a.path, a.mime, a.sha256, a.added_by, a.added_at
             FROM expense_attachments a
             WHERE NOT EXISTS (SELECT 1 FROM library_adoptions x WHERE x.source='expense_attachment' AND x.source_ref=a.attachment_id)
             ORDER BY a.added_at LIMIT ?1",
        )?;
        let rows = st
            .query_map([ADOPT_BATCH], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (aid, expense_id, name, path, mime, sha, by, at) in rows {
        let doc = adopt_one(c, data_dir, &sha, &path, &mime, &name, "receipt", "expense_attachment", &by, &at, None)?;
        add_link(c, &doc, "expense", &expense_id, &by, &at)?;
        c.execute(
            "INSERT INTO library_adoptions(source, source_ref, document_id, adopted_at) VALUES ('expense_attachment',?1,?2,?3)",
            params![aid, doc, time::now_str()],
        )?;
        n += 1;
    }
    // Supplier documents (not rejected ones: those were wrong uploads).
    let rows: Vec<ScanRows> = {
        let mut st = c.prepare(
            "SELECT s.scan_id, s.scan_number, s.image_path, s.image_sha256, s.file_name, s.doc_type, s.ocr_text, s.created_by, s.created_at,
                    s.supplier_id, s.supplier_invoice_id, s.po_id, s.mime
             FROM invoice_scans s
             WHERE s.status <> 'rejected'
               AND NOT EXISTS (SELECT 1 FROM library_adoptions x WHERE x.source='invoice_scan' AND x.source_ref=s.scan_id)
             ORDER BY s.created_at LIMIT ?1",
        )?;
        let rows = st
            .query_map([ADOPT_BATCH], |r| {
                Ok((
                    (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?),
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for ((scan_id, number, path, sha, name, doc_type, ocr, by, at), supplier, invoice, po) in rows {
        let category = match doc_type.as_str() {
            "invoice" => "invoice",
            "credit_note" => "credit_note",
            "delivery_note" => "delivery_note",
            "statement" => "statement",
            "receipt" => "receipt",
            _ => "other",
        };
        let ext_mime = if path.to_ascii_lowercase().ends_with(".pdf") { "application/pdf" } else { "image/jpeg" };
        let title = name.clone().unwrap_or_else(|| number.clone());
        let doc = adopt_one(c, data_dir, &sha, &path, ext_mime, &title, category, "invoice_scan", &by, &at, ocr.as_deref())?;
        for (t, id) in [("supplier", supplier), ("supplier_invoice", invoice), ("purchase_order", po)] {
            if let Some(id) = id.filter(|x| !x.is_empty()) {
                add_link(c, &doc, t, &id, &by, &at)?;
            }
        }
        c.execute(
            "INSERT INTO library_adoptions(source, source_ref, document_id, adopted_at) VALUES ('invoice_scan',?1,?2,?3)",
            params![scan_id, doc, time::now_str()],
        )?;
        n += 1;
    }
    // Evidence attached to cases (Wave 2 and 7).
    let rows: Vec<CaseRow8> = {
        let mut st = c.prepare(
            "SELECT e.event_id, e.case_id, json_extract(e.evidence_json, '$.file_name'), json_extract(e.evidence_json, '$.path'),
                    json_extract(e.evidence_json, '$.mime'), json_extract(e.evidence_json, '$.sha256'), e.user_id, e.at
             FROM case_events e
             WHERE e.kind='evidence' AND json_valid(e.evidence_json)
               AND length(COALESCE(json_extract(e.evidence_json, '$.sha256'), '')) = 64
               AND NOT EXISTS (SELECT 1 FROM library_adoptions x WHERE x.source='case_evidence' AND x.source_ref=e.event_id)
             ORDER BY e.at LIMIT ?1",
        )?;
        let rows = st
            .query_map([ADOPT_BATCH], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (event_id, case_id, name, path, mime, sha, by, at) in rows {
        let by = by.unwrap_or_else(|| "system".into());
        let doc = adopt_one(c, data_dir, &sha, &path, &mime, &name, "other", "case_evidence", &by, &at, None)?;
        add_link(c, &doc, "case", &case_id, &by, &at)?;
        c.execute(
            "INSERT INTO library_adoptions(source, source_ref, document_id, adopted_at) VALUES ('case_evidence',?1,?2,?3)",
            params![event_id, doc, time::now_str()],
        )?;
        n += 1;
    }
    Ok(n)
}

#[allow(clippy::too_many_arguments)]
fn adopt_one(
    c: &Connection,
    data_dir: &Path,
    sha: &str,
    path: &str,
    mime: &str,
    title: &str,
    category: &str,
    source: &str,
    by: &str,
    at: &str,
    text: Option<&str>,
) -> AppResult<String> {
    let rel = rel_path(data_dir, Path::new(path));
    let size = std::fs::metadata(abs_path(data_dir, &rel)).ok().map(|m| m.len() as i64).filter(|b| *b > 0);
    let new_file = insert_file(c, sha, &rel, mime, size)?;
    if new_file {
        if let Some(t) = text.filter(|t| !t.trim().is_empty()) {
            // Text read earlier from the whole file: its page is not known.
            set_text(c, sha, &[(0, t.to_string())], "ocr", None)?;
        }
    }
    // Identical bytes adopted earlier from the same kind of record: one document.
    let existing: Option<String> = c
        .query_row(
            "SELECT document_id FROM library_documents WHERE sha256=?1 AND source=?2 AND status='active' ORDER BY seq LIMIT 1",
            params![sha, source],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(d) = existing {
        return Ok(d);
    }
    let title: String = title.chars().take(120).collect();
    Ok(insert_document(c, sha, &title, category, &title, None, None, source, None, by, at, 1, None)?.0)
}

/// Copy every library file the backup folder does not hold yet into
/// `<dir>/AMWAPOS-files/<ab>/<sha256>`, checking each copy's hash. Files are
/// named by content, so each is copied once however many backups are made.
pub fn backup_files(c: &Connection, data_dir: &Path, dir: &Path) -> AppResult<(usize, Vec<String>)> {
    let files: Vec<(String, String)> = {
        let mut st = c.prepare("SELECT sha256, path FROM library_files ORDER BY sha256")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let root = dir.join("AMWAPOS-files");
    let (mut copied, mut missing) = (0, vec![]);
    for (sha, path) in files {
        let dst = root.join(&sha[..2]).join(&sha);
        if dst.exists() {
            continue;
        }
        let Ok(bytes) = std::fs::read(abs_path(data_dir, &path)) else {
            missing.push(sha);
            continue;
        };
        if sha_hex(&bytes) != sha {
            missing.push(sha);
            continue;
        }
        std::fs::create_dir_all(dst.parent().unwrap_or(&root))?;
        let tmp = dst.with_extension("partial");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &dst)?;
        copied += 1;
    }
    Ok((copied, missing))
}

/// After a restore: put back every library file that is missing (or whose
/// bytes no longer match) from the backup folder's copies.
pub fn restore_files(c: &Connection, data_dir: &Path, backup_dir: &Path) -> AppResult<(usize, Vec<String>)> {
    let files: Vec<(String, String)> = {
        let mut st = c.prepare("SELECT sha256, path FROM library_files ORDER BY sha256")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let root = backup_dir.join("AMWAPOS-files");
    let (mut restored, mut missing) = (0, vec![]);
    for (sha, path) in files {
        let dst = abs_path(data_dir, &path);
        if std::fs::read(&dst).map(|b| sha_hex(&b) == sha).unwrap_or(false) {
            continue;
        }
        match std::fs::read(root.join(&sha[..2]).join(&sha)) {
            Ok(b) if sha_hex(&b) == sha => {
                if let Some(p) = dst.parent() {
                    std::fs::create_dir_all(p)?;
                }
                std::fs::write(&dst, &b)?;
                restored += 1;
            }
            _ => missing.push(sha),
        }
    }
    Ok((restored, missing))
}

impl AppCore {
    /// A library session: `documents.view` (or `documents.manage` for
    /// changes), on the hub. Earlier evidence is adopted on the way in.
    fn library_session(&self, token: &str, manage: bool) -> AppResult<(Session, Scope)> {
        let s = self.session(token)?;
        s.require("documents.view")?;
        if manage {
            s.require("documents.manage")?;
        }
        if self.require_back_office_writable().is_err() {
            return Err(AppError::conflict(MSG_HUB_ONLY));
        }
        let dir = self.data_dir.clone();
        if self.db.read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS (SELECT 1 FROM expense_attachments a WHERE NOT EXISTS
                   (SELECT 1 FROM library_adoptions x WHERE x.source='expense_attachment' AND x.source_ref=a.attachment_id))
                 OR EXISTS (SELECT 1 FROM invoice_scans s WHERE s.status <> 'rejected' AND NOT EXISTS
                   (SELECT 1 FROM library_adoptions x WHERE x.source='invoice_scan' AND x.source_ref=s.scan_id))
                 OR EXISTS (SELECT 1 FROM case_events e WHERE e.kind='evidence' AND json_valid(e.evidence_json)
                   AND length(COALESCE(json_extract(e.evidence_json, '$.sha256'), '')) = 64 AND NOT EXISTS
                   (SELECT 1 FROM library_adoptions x WHERE x.source='case_evidence' AND x.source_ref=e.event_id))",
                [],
                |r| r.get::<_, bool>(0),
            )?)
        })? {
            self.db.write(|tx| adopt_existing(tx, &dir))?;
        }
        let scope = Scope::of(&s);
        Ok((s, scope))
    }

    fn library_visible(&self, c: &Connection, scope: &Scope, id: &str) -> AppResult<()> {
        if scope.can_see(c, id)? {
            Ok(())
        } else {
            // The same answer whether it does not exist or may not be seen.
            Err(AppError::not_found("Document"))
        }
    }

    fn check_link(&self, c: &Connection, s: &Session, l: &LinkInput) -> AppResult<(&'static str, String)> {
        let e = entity(&l.entity_type)?;
        if !e.4.iter().any(|p| s.has(p)) {
            return Err(AppError::forbidden(e.4[0]));
        }
        let id = validate::id(&l.entity_id, "Record")?;
        let sql = format!("SELECT 1 FROM {} WHERE {} = ?1", e.1, e.2);
        if c.query_row(&sql, [&id], |_| Ok(())).optional()?.is_none() {
            return Err(AppError::not_found("Record"));
        }
        Ok((e.0, id))
    }

    /// Validate, hash and keep a file in the library store (once per content).
    fn library_store(
        &self,
        c_has: impl Fn(&str) -> AppResult<bool>,
        file_name: &str,
        b64: &str,
    ) -> AppResult<(String, String, String, Vec<u8>, String)> {
        let name = clean(file_name, "File name", 120, true)?;
        let bytes = crate::ids::b64_decode(b64).ok_or_else(|| AppError::validation("The file could not be read."))?;
        let sha = sha_hex(&bytes);
        let dir = self.data_dir.join("library").join(&sha[..2]);
        if c_has(&sha)? {
            // Already stored: validate the same way, write nothing.
            let ext = Path::new(&name).extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
            if !crate::docintel::service::DOC_EXT.contains(&ext.as_str()) {
                return Err(AppError::validation("Choose a PDF or an image (JPG, PNG, WEBP, BMP or TIFF)."));
            }
            return Ok((name, sha, String::new(), bytes, String::new()));
        }
        let (path, sha2, mime) = crate::docintel::service::store_original(&dir, &name, &bytes, &sha)?;
        debug_assert_eq!(sha, sha2);
        Ok((name, sha, rel_path(&self.data_dir, &path), bytes, mime.to_string()))
    }

    /// Add a document. Identical bytes already in a document the person can
    /// see return that document (links are added to it); otherwise a new
    /// document shares the stored file.
    pub fn library_add(&self, token: &str, input: AddInput) -> AppResult<Value> {
        let (s, scope) = self.library_session(token, true)?;
        let category = check_category(&input.category)?;
        let document_date = check_date(&input.document_date)?;
        let note = opt_clean(&input.note, "Note", 500)?;
        let has = |sha: &str| {
            self.db.read(|c| Ok(c.query_row("SELECT 1 FROM library_files WHERE sha256=?1", [sha], |_| Ok(())).optional()?.is_some()))
        };
        let (name, sha, path, bytes, mime) = self.library_store(has, &input.file_name, &input.data_base64)?;
        let title = match opt_clean(&input.title, "Title", 120)? {
            Some(t) => t,
            None => name.clone(),
        };
        let pdf_text = if mime == "application/pdf" { pdf_pages(&bytes) } else { None };
        let actor = self.actor(&s, None);
        let branch = s.branch_id.clone();
        let res = self.db.write(|tx| {
            let links = input.links.iter().map(|l| self.check_link(tx, &s, l)).collect::<AppResult<Vec<_>>>()?;
            let now = time::now_str();
            if !path.is_empty() && insert_file(tx, &sha, &path, &mime, Some(bytes.len() as i64))? {
                if let Some(p) = &pdf_text {
                    set_text(tx, &sha, p, "pdf_text", None)?;
                }
            }
            let sql = format!(
                "SELECT d.document_id, d.number FROM library_documents d WHERE d.sha256=?1 AND d.status='active' AND {} ORDER BY d.seq LIMIT 1",
                scope.sql()
            );
            let same: Option<(String, String)> = tx.query_row(&sql, [&sha], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let (id, number, duplicate) = match same {
                Some((id, number)) => (id, number, true),
                None => {
                    let (id, number) = insert_document(
                        tx,
                        &sha,
                        &title,
                        category,
                        &name,
                        document_date.as_deref(),
                        note.as_deref(),
                        "upload",
                        Some(&branch),
                        &s.user_id,
                        &now,
                        1,
                        None,
                    )?;
                    (id, number, false)
                }
            };
            for (t, eid) in &links {
                add_link(tx, &id, t, eid, &s.user_id, &now)?;
            }
            audit::record(
                tx,
                &actor,
                if duplicate { "library.duplicate_upload" } else { "library.document_added" },
                "library_document",
                Some(&id),
                None,
                Some(&json!({ "number": number, "sha256": sha, "category": category, "links": links.len() })),
            )?;
            Ok(json!({ "document_id": id, "number": number, "duplicate": duplicate, "sha256": sha }))
        });
        self.drop_unrecorded(&path, &res);
        res
    }

    /// A file written for a change that was then refused is not kept.
    fn drop_unrecorded<T>(&self, path: &str, res: &AppResult<T>) {
        if res.is_err() && !path.is_empty() {
            let _ = std::fs::remove_file(abs_path(&self.data_dir, path));
        }
    }

    pub fn library_list(&self, token: &str, f: ListFilter) -> AppResult<Value> {
        let (_, scope) = self.library_session(token, false)?;
        let limit = validate::limit(f.limit, 50, 200);
        let offset = validate::offset(f.offset);
        let mut sql = format!(
            "SELECT d.document_id, d.number, d.title, d.category, d.status, d.version, d.document_date, d.added_at, d.source,
                    f.mime, f.text_status, f.page_count,
                    (SELECT COUNT(*) FROM library_links l WHERE l.document_id=d.document_id AND l.removed_at IS NULL)
             FROM library_documents d JOIN library_files f ON f.sha256=d.sha256 WHERE {}",
            scope.sql()
        );
        let mut args: Vec<String> = vec![];
        match f.status.as_deref().unwrap_or("active") {
            "all" => {}
            st @ ("active" | "archived" | "replaced") => sql.push_str(&format!(" AND d.status='{st}'")),
            _ => return Err(AppError::validation("Unknown status.")),
        }
        if let Some(c) = f.category.as_deref().filter(|x| !x.is_empty()) {
            sql.push_str(&format!(" AND d.category='{}'", check_category(c)?));
        }
        if let Some(t) = f.entity_type.as_deref().filter(|x| !x.is_empty()) {
            let t = entity(t)?.0;
            let id = validate::id(f.entity_id.as_deref().unwrap_or_default(), "Record")?;
            args.push(id);
            sql.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM library_links l WHERE l.document_id=d.document_id AND l.removed_at IS NULL AND l.entity_type='{t}' AND l.entity_id=?{})",
                args.len()
            ));
        }
        if let Some(q) = f.q.as_deref().and_then(validate::fts_query) {
            args.push(q);
            sql.push_str(&format!(" AND d.seq IN (SELECT rowid / {ROWS} FROM library_fts WHERE library_fts MATCH ?{})", args.len()));
        }
        sql.push_str(&format!(" ORDER BY d.added_at DESC, d.seq DESC LIMIT {limit} OFFSET {offset}"));
        self.db.read(|c| {
            let mut st = c.prepare(&sql)?;
            let rows = st
                .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                    Ok(json!({
                        "document_id": r.get::<_, String>(0)?, "number": r.get::<_, String>(1)?, "title": r.get::<_, String>(2)?,
                        "category": r.get::<_, String>(3)?, "status": r.get::<_, String>(4)?, "version": r.get::<_, i64>(5)?,
                        "document_date": r.get::<_, Option<String>>(6)?, "added_at": r.get::<_, String>(7)?, "source": r.get::<_, String>(8)?,
                        "mime": r.get::<_, String>(9)?, "text_status": r.get::<_, String>(10)?, "page_count": r.get::<_, Option<i64>>(11)?,
                        "links": r.get::<_, i64>(12)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let counts = {
                let sql = format!("SELECT d.status, COUNT(*) FROM library_documents d WHERE {} GROUP BY d.status", scope.sql());
                let mut st = c.prepare(&sql)?;
                let m: serde_json::Map<String, Value> =
                    st.query_map([], |r| Ok((r.get::<_, String>(0)?, json!(r.get::<_, i64>(1)?))))?.collect::<Result<_, _>>()?;
                m
            };
            Ok(json!({ "rows": rows, "counts": counts }))
        })
    }

    /// Full-text search with bounded snippets and real citations (a page
    /// only when the page is known). Archived and replaced versions are
    /// searched only when asked for.
    pub fn library_search(&self, token: &str, q: &str, include_old: bool, limit: Option<i64>) -> AppResult<Value> {
        let (_, scope) = self.library_session(token, false)?;
        let Some(fq) = validate::fts_query(q) else { return Ok(json!({ "results": [] })) };
        let limit = validate::limit(limit, 10, MAX_RESULTS);
        let status = if include_old { "" } else { "AND d.status='active'" };
        let sql = format!(
            "SELECT d.document_id, d.number, d.title, d.category, d.status, d.document_date, x.rowid
             FROM (SELECT rowid, rank FROM library_fts WHERE library_fts MATCH ?1 ORDER BY rank LIMIT 400) x
             JOIN library_documents d ON d.seq = x.rowid / {ROWS}
             WHERE {} {status}
             ORDER BY x.rank LIMIT ?2",
            scope.sql()
        );
        self.db.read(|c| {
            let mut st = c.prepare(&sql)?;
            let rows = st
                .query_map(params![fq, limit * 3], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut snip =
                c.prepare("SELECT snippet(library_fts, 1, '[', ']', '…', 16) FROM library_fts WHERE library_fts MATCH ?1 AND rowid = ?2")?;
            let mut per_doc: std::collections::HashMap<String, (usize, bool)> = std::collections::HashMap::new();
            let mut out = vec![];
            for (id, number, title, category, status, date, rowid) in rows {
                // At most three pages per document.
                let n = per_doc.entry(id.clone()).or_default();
                if n.0 >= 3 {
                    continue;
                }
                let snippet: String = snip.query_row(params![fq, rowid], |r| r.get(0)).optional()?.unwrap_or_default();
                // The title is on every page's row: a match in the title alone
                // is one result for the document, with no page.
                let in_body = snippet.contains('[');
                if !in_body {
                    if n.1 {
                        continue;
                    }
                    n.1 = true;
                }
                n.0 += 1;
                let page = if in_body { rowid % ROWS } else { 0 };
                out.push(json!({
                    "document_id": id, "number": number, "title": title, "category": category, "status": status,
                    "document_date": date, "page": if page > 0 { Some(page) } else { None },
                    "snippet": snippet.chars().take(300).collect::<String>(),
                }));
            }
            // A title-only result adds nothing when the document has page results.
            let with_pages: std::collections::HashSet<String> =
                out.iter().filter(|r| !r["page"].is_null()).filter_map(|r| r["document_id"].as_str().map(str::to_string)).collect();
            out.retain(|r| !r["page"].is_null() || !with_pages.contains(r["document_id"].as_str().unwrap_or_default()));
            out.truncate(limit as usize);
            Ok(json!({ "results": out }))
        })
    }

    pub fn library_get(&self, token: &str, id: &str) -> AppResult<Value> {
        let (_, scope) = self.library_session(token, false)?;
        let id = validate::id(id, "Document")?;
        self.db.read(|c| {
            self.library_visible(c, &scope, &id)?;
            let d = c.query_row(
                "SELECT d.document_id, d.number, d.title, d.category, d.status, d.version, d.document_date, d.note, d.original_name,
                        d.added_by, (SELECT display_name FROM users u WHERE u.user_id=d.added_by), d.added_at, d.source,
                        d.replaces_id, d.replaced_by, d.archived_at, d.archive_reason, d.sha256,
                        f.mime, f.bytes, f.page_count, f.text_status, f.text_source, f.text_note
                 FROM library_documents d JOIN library_files f ON f.sha256=d.sha256 WHERE d.document_id=?1",
                [&id],
                |r| {
                    Ok(json!({
                        "document_id": r.get::<_, String>(0)?, "number": r.get::<_, String>(1)?, "title": r.get::<_, String>(2)?,
                        "category": r.get::<_, String>(3)?, "status": r.get::<_, String>(4)?, "version": r.get::<_, i64>(5)?,
                        "document_date": r.get::<_, Option<String>>(6)?, "note": r.get::<_, Option<String>>(7)?,
                        "original_name": r.get::<_, String>(8)?, "added_by": r.get::<_, String>(9)?,
                        "added_by_name": r.get::<_, Option<String>>(10)?, "added_at": r.get::<_, String>(11)?,
                        "source": r.get::<_, String>(12)?, "replaces_id": r.get::<_, Option<String>>(13)?,
                        "replaced_by": r.get::<_, Option<String>>(14)?, "archived_at": r.get::<_, Option<String>>(15)?,
                        "archive_reason": r.get::<_, Option<String>>(16)?, "sha256": r.get::<_, String>(17)?,
                        "mime": r.get::<_, String>(18)?, "bytes": r.get::<_, Option<i64>>(19)?, "page_count": r.get::<_, Option<i64>>(20)?,
                        "text_status": r.get::<_, String>(21)?, "text_source": r.get::<_, Option<String>>(22)?,
                        "text_note": r.get::<_, Option<String>>(23)?,
                    }))
                },
            )?;
            let links: Vec<Value> = {
                let mut st = c.prepare(
                    "SELECT link_id, entity_type, entity_id, linked_by, linked_at FROM library_links
                     WHERE document_id=?1 AND removed_at IS NULL ORDER BY linked_at",
                )?;
                let rows = st
                    .query_map([&id], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                rows.into_iter()
                    .map(|(lid, t, eid, by, at)| {
                        let label = entity_label(c, &t, &eid)?;
                        Ok(json!({ "link_id": lid, "entity_type": t, "entity_id": eid, "label": label, "exists": label.is_some(), "linked_by": by, "linked_at": at }))
                    })
                    .collect::<AppResult<_>>()?
            };
            // The version chain, oldest first, as far as the person may see.
            let mut versions = vec![];
            let mut cur: Option<String> = Some(id.clone());
            while let Some(x) = cur.take() {
                let r: Option<(Option<String>, String, String, i64, String)> = c
                    .query_row("SELECT replaces_id, document_id, number, version, status FROM library_documents WHERE document_id=?1", [&x], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })
                    .optional()?;
                let Some((prev, did, number, version, status)) = r else { break };
                versions.insert(0, json!({ "document_id": did, "number": number, "version": version, "status": status }));
                cur = prev;
                if versions.len() > 100 {
                    break;
                }
            }
            let mut next = d["replaced_by"].as_str().map(str::to_string);
            while let Some(x) = next.take() {
                let r: Option<(Option<String>, String, i64, String)> = c
                    .query_row("SELECT replaced_by, number, version, status FROM library_documents WHERE document_id=?1", [&x], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })
                    .optional()?;
                let Some((after, number, version, status)) = r else { break };
                versions.push(json!({ "document_id": x, "number": number, "version": version, "status": status }));
                next = after;
                if versions.len() > 200 {
                    break;
                }
            }
            let pages: Vec<i64> = {
                let mut st = c.prepare("SELECT page FROM library_file_pages WHERE sha256=?1 ORDER BY page")?;
                let rows = st.query_map([d["sha256"].as_str().unwrap_or_default()], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
                rows
            };
            let can_delete = d["source"] == "upload"
                && d["replaces_id"].is_null()
                && d["replaced_by"].is_null()
                && c.query_row("SELECT 1 FROM library_links WHERE document_id=?1 LIMIT 1", [&id], |_| Ok(())).optional()?.is_none();
            Ok(json!({ "document": d, "links": links, "versions": versions, "text_pages": pages, "can_delete": can_delete }))
        })
    }

    /// The text read from one page (bounded), for the viewer and the assistant.
    pub fn library_text(&self, token: &str, id: &str, page: Option<i64>) -> AppResult<Value> {
        let (_, scope) = self.library_session(token, false)?;
        let id = validate::id(id, "Document")?;
        self.db.read(|c| {
            self.library_visible(c, &scope, &id)?;
            let (sha, status): (String, String) = c.query_row(
                "SELECT d.sha256, f.text_status FROM library_documents d JOIN library_files f ON f.sha256=d.sha256 WHERE d.document_id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let row: Option<(i64, String)> = match page {
                Some(p) => c.query_row("SELECT page, text FROM library_file_pages WHERE sha256=?1 AND page=?2", params![sha, p], |r| Ok((r.get(0)?, r.get(1)?))),
                None => c.query_row("SELECT page, text FROM library_file_pages WHERE sha256=?1 ORDER BY page LIMIT 1", [&sha], |r| Ok((r.get(0)?, r.get(1)?))),
            }
            .optional()?;
            Ok(match row {
                Some((p, t)) => json!({ "document_id": id, "page": if p > 0 { Some(p) } else { None }, "text": t.chars().take(4000).collect::<String>(), "truncated": t.chars().count() > 4000, "text_status": status }),
                None => json!({ "document_id": id, "page": page, "text": Value::Null, "text_status": status,
                    "message": if status == "pending" { "The text has not been read yet." } else { MSG_NO_TEXT } }),
            })
        })
    }

    /// The file itself, for viewing or saving.
    pub fn library_file(&self, token: &str, id: &str) -> AppResult<Value> {
        let (s, scope) = self.library_session(token, false)?;
        let id = validate::id(id, "Document")?;
        let (name, path, mime, sha) = self.db.read(|c| {
            self.library_visible(c, &scope, &id)?;
            Ok(c.query_row(
                "SELECT d.original_name, f.path, f.mime, f.sha256 FROM library_documents d JOIN library_files f ON f.sha256=d.sha256 WHERE d.document_id=?1",
                [&id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)),
            )?)
        })?;
        let bytes = std::fs::read(abs_path(&self.data_dir, &path))
            .map_err(|_| AppError::new(ErrorCode::NotFound, "The file is missing from this computer. Restore it from a backup."))?;
        let intact = sha_hex(&bytes) == sha;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            audit::record(tx, &actor, "library.document_opened", "library_document", Some(&id), None, None)?;
            Ok(())
        })?;
        Ok(json!({ "file_name": name, "mime": mime, "base64": crate::ids::b64(&bytes), "intact": intact }))
    }

    pub fn library_update(&self, token: &str, id: &str, u: UpdateInput) -> AppResult<Value> {
        let (s, scope) = self.library_session(token, true)?;
        let id = validate::id(id, "Document")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            self.library_visible(tx, &scope, &id)?;
            let (title, category, date, note, status): (String, String, Option<String>, Option<String>, String) = tx.query_row(
                "SELECT title, category, document_date, note, status FROM library_documents WHERE document_id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            if status != "active" {
                return Err(AppError::conflict("Only the current version can be changed."));
            }
            let new_title = match &u.title {
                Some(t) => clean(t, "Title", 120, true)?,
                None => title.clone(),
            };
            let new_category = match &u.category {
                Some(c) => check_category(c)?.to_string(),
                None => category.clone(),
            };
            let new_date = match &u.document_date {
                Some(_) => check_date(&u.document_date)?,
                None => date.clone(),
            };
            let new_note = match &u.note {
                Some(_) => opt_clean(&u.note, "Note", 500)?,
                None => note.clone(),
            };
            // A new category must still be one the person may see.
            if !Scope::of(&s).categories.contains(&new_category.as_str()) {
                return Err(AppError::forbidden("payables.view"));
            }
            tx.execute(
                "UPDATE library_documents SET title=?2, category=?3, document_date=?4, note=?5, updated_at=?6 WHERE document_id=?1",
                params![id, new_title, new_category, new_date, new_note, time::now_str()],
            )?;
            if new_title != title {
                index_document(tx, &id)?;
            }
            audit::record(
                tx,
                &actor,
                "library.document_updated",
                "library_document",
                Some(&id),
                Some(&json!({ "title": title, "category": category, "document_date": date })),
                Some(&json!({ "title": new_title, "category": new_category, "document_date": new_date })),
            )?;
            Ok(json!({ "document_id": id }))
        })
    }

    pub fn library_link(&self, token: &str, id: &str, link: LinkInput) -> AppResult<Value> {
        let (s, scope) = self.library_session(token, true)?;
        let id = validate::id(id, "Document")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            self.library_visible(tx, &scope, &id)?;
            let (t, eid) = self.check_link(tx, &s, &link)?;
            let status: String = tx.query_row("SELECT status FROM library_documents WHERE document_id=?1", [&id], |r| r.get(0))?;
            if status != "active" {
                return Err(AppError::conflict("Link the current version of the document."));
            }
            let added = add_link(tx, &id, t, &eid, &s.user_id, &time::now_str())?;
            if added {
                audit::record(
                    tx,
                    &actor,
                    "library.linked",
                    "library_document",
                    Some(&id),
                    None,
                    Some(&json!({ "entity_type": t, "entity_id": eid })),
                )?;
            }
            Ok(json!({ "document_id": id, "linked": added }))
        })
    }

    pub fn library_unlink(&self, token: &str, id: &str, link_id: &str) -> AppResult<Value> {
        let (s, scope) = self.library_session(token, true)?;
        let id = validate::id(id, "Document")?;
        let link_id = validate::id(link_id, "Link")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            self.library_visible(tx, &scope, &id)?;
            let (t, eid): (String, String) = tx
                .query_row(
                    "SELECT entity_type, entity_id FROM library_links WHERE link_id=?1 AND document_id=?2 AND removed_at IS NULL",
                    params![link_id, id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| AppError::not_found("Link"))?;
            tx.execute(
                "UPDATE library_links SET removed_by=?2, removed_at=?3 WHERE link_id=?1",
                params![link_id, s.user_id, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "library.unlinked",
                "library_document",
                Some(&id),
                Some(&json!({ "entity_type": t, "entity_id": eid })),
                None,
            )?;
            Ok(json!({ "document_id": id }))
        })
    }

    /// Replace a document's file: a new version, linked to the same records.
    /// The old version keeps its file and hash, marked as replaced.
    pub fn library_replace(&self, token: &str, id: &str, file_name: &str, data_b64: &str, note: Option<String>) -> AppResult<Value> {
        let (s, scope) = self.library_session(token, true)?;
        let id = validate::id(id, "Document")?;
        let note = opt_clean(&note, "Note", 500)?;
        let has = |sha: &str| {
            self.db.read(|c| Ok(c.query_row("SELECT 1 FROM library_files WHERE sha256=?1", [sha], |_| Ok(())).optional()?.is_some()))
        };
        let (name, sha, path, bytes, mime) = self.library_store(has, file_name, data_b64)?;
        let pdf_text = if mime == "application/pdf" { pdf_pages(&bytes) } else { None };
        let actor = self.actor(&s, None);
        let res = self.db.write(|tx| {
            self.library_visible(tx, &scope, &id)?;
            let (old_sha, title, category, version, status, date): (String, String, String, i64, String, Option<String>) = tx.query_row(
                "SELECT sha256, title, category, version, status, document_date FROM library_documents WHERE document_id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )?;
            if status != "active" {
                return Err(AppError::conflict("Only the current version can be replaced."));
            }
            if old_sha == sha {
                return Err(AppError::validation("This file is identical to the current version."));
            }
            if !path.is_empty() && insert_file(tx, &sha, &path, &mime, Some(bytes.len() as i64))? {
                if let Some(p) = &pdf_text {
                    set_text(tx, &sha, p, "pdf_text", None)?;
                }
            }
            let now = time::now_str();
            let (new_id_, number) = insert_document(
                tx,
                &sha,
                &title,
                &category,
                &name,
                date.as_deref(),
                note.as_deref(),
                "upload",
                Some(&s.branch_id),
                &s.user_id,
                &now,
                version + 1,
                Some(&id),
            )?;
            tx.execute(
                "UPDATE library_documents SET status='replaced', replaced_by=?2, updated_at=?3 WHERE document_id=?1",
                params![id, new_id_, now],
            )?;
            let links: Vec<(String, String)> = {
                let mut st = tx.prepare("SELECT entity_type, entity_id FROM library_links WHERE document_id=?1 AND removed_at IS NULL")?;
                let rows = st.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                rows
            };
            for (t, eid) in &links {
                add_link(tx, &new_id_, t, eid, &s.user_id, &now)?;
            }
            audit::record(
                tx,
                &actor,
                "library.document_replaced",
                "library_document",
                Some(&id),
                Some(&json!({ "sha256": old_sha, "version": version })),
                Some(&json!({ "document_id": new_id_, "number": number, "sha256": sha, "version": version + 1 })),
            )?;
            Ok(json!({ "document_id": new_id_, "number": number, "version": version + 1 }))
        });
        self.drop_unrecorded(&path, &res);
        res
    }

    pub fn library_archive(&self, token: &str, id: &str, reason: &str, archive: bool) -> AppResult<Value> {
        let (s, scope) = self.library_session(token, true)?;
        let id = validate::id(id, "Document")?;
        let reason = if archive { Some(clean(reason, "Reason", 300, true)?) } else { None };
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            self.library_visible(tx, &scope, &id)?;
            let status: String = tx.query_row("SELECT status FROM library_documents WHERE document_id=?1", [&id], |r| r.get(0))?;
            let now = time::now_str();
            match (archive, status.as_str()) {
                (true, "active") => {
                    tx.execute(
                        "UPDATE library_documents SET status='archived', archived_by=?2, archived_at=?3, archive_reason=?4, updated_at=?3 WHERE document_id=?1",
                        params![id, s.user_id, now, reason],
                    )?;
                }
                (false, "archived") => {
                    tx.execute(
                        "UPDATE library_documents SET status='active', archived_by=NULL, archived_at=NULL, archive_reason=NULL, updated_at=?2 WHERE document_id=?1",
                        params![id, now],
                    )?;
                }
                _ => return Err(AppError::conflict(if archive { "Only the current version can be archived." } else { "This document is not archived." })),
            }
            audit::record(
                tx,
                &actor,
                if archive { "library.document_archived" } else { "library.document_restored" },
                "library_document",
                Some(&id),
                None,
                Some(&json!({ "reason": reason })),
            )?;
            Ok(json!({ "document_id": id, "status": if archive { "archived" } else { "active" } }))
        })
    }

    /// Delete a document that was never evidence for anything: added here,
    /// never linked, not part of a version chain. The file goes too when no
    /// other document uses it.
    pub fn library_delete(&self, token: &str, id: &str) -> AppResult<Value> {
        let (s, scope) = self.library_session(token, true)?;
        let id = validate::id(id, "Document")?;
        let actor = self.actor(&s, None);
        let remove = self.db.write(|tx| {
            self.library_visible(tx, &scope, &id)?;
            let (seq, sha, number, source, prev, next): (i64, String, String, String, Option<String>, Option<String>) = tx.query_row(
                "SELECT seq, sha256, number, source, replaces_id, replaced_by FROM library_documents WHERE document_id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )?;
            let linked = tx.query_row("SELECT 1 FROM library_links WHERE document_id=?1 LIMIT 1", [&id], |_| Ok(())).optional()?.is_some();
            if linked || source != "upload" || prev.is_some() || next.is_some() {
                return Err(AppError::conflict("This document is evidence for a record. Archive it instead."));
            }
            tx.execute("DELETE FROM library_fts WHERE rowid BETWEEN ?1 AND ?2", params![seq * ROWS, seq * ROWS + ROWS - 1])?;
            tx.execute("DELETE FROM library_documents WHERE document_id=?1", [&id])?;
            let others = tx.query_row("SELECT 1 FROM library_documents WHERE sha256=?1 LIMIT 1", [&sha], |_| Ok(())).optional()?.is_some();
            let mut path = None;
            if !others {
                let p: String = tx.query_row("SELECT path FROM library_files WHERE sha256=?1", [&sha], |r| r.get(0))?;
                // Only files the library itself stored; earlier records keep theirs.
                if p.starts_with("library/") {
                    path = Some(p);
                }
                tx.execute("DELETE FROM library_file_pages WHERE sha256=?1", [&sha])?;
                tx.execute("DELETE FROM library_files WHERE sha256=?1", [&sha])?;
            }
            audit::record(
                tx,
                &actor,
                "library.document_deleted",
                "library_document",
                Some(&id),
                Some(&json!({ "number": number, "sha256": sha })),
                None,
            )?;
            Ok(path)
        })?;
        if let Some(p) = remove {
            let _ = std::fs::remove_file(abs_path(&self.data_dir, &p));
        }
        Ok(json!({ "document_id": id, "deleted": true }))
    }

    pub fn library_reindex(&self, token: &str) -> AppResult<Value> {
        let (s, _) = self.library_session(token, true)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let n = reindex(tx)?;
            audit::record(tx, &actor, "library.reindexed", "library", None, None, Some(&json!({ "documents": n })))?;
            Ok(json!({ "documents": n }))
        })
    }

    // ---- Text reading (the hub's worker) ----------------------------------

    /// Files whose text has not been read: (sha256, absolute path, mime).
    pub fn library_text_pending(&self, limit: i64) -> AppResult<Vec<(String, PathBuf, String)>> {
        let rows: Vec<(String, String, String)> = self.db.read(|c| {
            let mut st =
                c.prepare("SELECT sha256, path, mime FROM library_files WHERE text_status='pending' ORDER BY created_at LIMIT ?1")?;
            let rows = st.query_map([limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        Ok(rows.into_iter().map(|(s, p, m)| (s, abs_path(&self.data_dir, &p), m)).collect())
    }

    /// Read a pending PDF's own text layer here; scans and photos are left
    /// for OCR. Returns true when the file was settled.
    pub fn library_read_pdf_text(&self, sha: &str, path: &Path) -> AppResult<bool> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => {
                self.library_text_failed(sha, "The file is missing from this computer.")?;
                return Ok(true);
            }
        };
        match pdf_pages(&bytes) {
            Some(p) => {
                self.db.write(|tx| set_text(tx, sha, &p, "pdf_text", None))?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Read the text layer of PDFs waiting for it (no OCR needed). A PDF
    /// without one (a scan) is left for OCR. Returns how many were settled.
    pub fn library_read_pending_pdfs(&self, limit: i64) -> AppResult<usize> {
        let rows: Vec<(String, String)> = self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT sha256, path FROM library_files WHERE text_status='pending' AND text_note IS NULL
                 AND mime='application/pdf' ORDER BY created_at LIMIT ?1",
            )?;
            let rows = st.query_map([limit], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        let mut n = 0;
        for (sha, path) in rows {
            if self.library_read_pdf_text(&sha, &abs_path(&self.data_dir, &path))? {
                n += 1;
            } else {
                self.db.write(|tx| {
                    tx.execute("UPDATE library_files SET text_note='Waiting for OCR.' WHERE sha256=?1", [&sha])?;
                    Ok(())
                })?;
            }
        }
        Ok(n)
    }

    pub fn library_text_done(&self, sha: &str, pages: Vec<(u32, String)>, source: &str) -> AppResult<()> {
        let source = if source == "pdf_text" { "pdf_text" } else { "ocr" };
        self.db.write(|tx| set_text(tx, sha, &pages, source, None))
    }

    pub fn library_text_failed(&self, sha: &str, why: &str) -> AppResult<()> {
        let why: String = why.chars().take(200).collect();
        self.db.write(|tx| {
            tx.execute("UPDATE library_files SET text_status='failed', text_note=?2 WHERE sha256=?1", params![sha, why])?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_sql_lists_only_constants() {
        let s = Scope { entity_types: vec!["expense"], categories: vec!["receipt", "other"] };
        let q = s.sql();
        assert!(q.contains("d.category IN ('receipt','other')"));
        assert!(q.contains("NOT IN ('expense')"));
        let none = Scope { entity_types: vec![], categories: vec![] };
        assert!(none.sql().contains("IN ('')"));
    }

    #[test]
    fn page_text_is_bounded_and_printable() {
        let t = tidy(&format!("a\u{0007}b{}", "x".repeat(MAX_PAGE_CHARS * 2)));
        assert!(t.starts_with("ab"));
        assert_eq!(t.chars().count(), MAX_PAGE_CHARS);
    }
}
