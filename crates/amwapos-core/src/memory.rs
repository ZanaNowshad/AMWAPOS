//! Business Memory (Wave 8, docs/INTELLIGENCE_AND_EVIDENCE.md).
//!
//! Facts about the business that no record holds ("Gulf Dairy delivers on
//! Sundays and Wednesdays"), each one statement with its provenance, scope,
//! the record it is about, who confirmed it and when it was last checked.
//!
//! * The assistant only adds candidates; a person confirms, rejects or edits.
//! * A confirmed statement is never rewritten: a new one supersedes it.
//! * Records win. When the record a memory is about changed after the memory
//!   was confirmed, was switched off or no longer exists, the memory is shown
//!   as "May be outdated" with the reason; when a memory has passed its
//!   valid-until date or has not been checked for a long time, the same.
//! * Memory is never an authorisation, and never a store of secrets or of
//!   customers' personal details: statements that look like either are
//!   refused.
//! * Who may see a memory: memory.view, plus the permission of the record it
//!   is about (the Document Library's rule), plus its branch.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult};
use crate::ids::{new_id, next_seq};
use crate::library::ENTITY_TYPES;
use crate::service::AppCore;
use crate::time;
use crate::validate;

/// Unchecked for this long, a memory is shown as possibly outdated.
pub const STALE_DAYS: i64 = 180;
/// Candidates the assistant may add in one conversation.
pub const MAX_SUGGESTIONS_PER_CONVERSATION: i64 = 5;
/// Results per search.
pub const MAX_RESULTS: i64 = 10;

pub const MSG_OUTDATED: &str = "May be outdated";

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct MemoryInput {
    pub statement: String,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    /// "business" (default) or "branch" (this session's branch).
    pub scope: Option<String>,
    pub valid_until: Option<String>,
    /// From a document: its id and the passage.
    pub document_id: Option<String>,
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MemoryFilter {
    /// confirmed (default) | review | archived | history
    pub tab: Option<String>,
    pub q: Option<String>,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

fn has_letters_and_digits(w: &str) -> bool {
    w.chars().any(|c| c.is_ascii_alphabetic()) && w.chars().any(|c| c.is_ascii_digit())
}

fn luhn(digits: &[u32]) -> bool {
    let mut sum = 0;
    for (i, d) in digits.iter().rev().enumerate() {
        let mut x = *d;
        if i % 2 == 1 {
            x *= 2;
            if x > 9 {
                x -= 9;
            }
        }
        sum += x;
    }
    sum % 10 == 0
}

/// Why a statement cannot be kept: secrets and personal contact details.
pub fn refuse_reason(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    const SECRET_WORDS: &[&str] = &[
        "password",
        "passcode",
        "pin code",
        "pin is",
        "pin:",
        "api key",
        "api_key",
        "apikey",
        "secret",
        "token",
        "otp",
        "pairing code",
        "private key",
        "-----begin",
        "كلمة السر",
        "كلمة المرور",
        "الرقم السري",
        "رمز التحقق",
    ];
    if SECRET_WORDS.iter().any(|w| lower.contains(w)) {
        return Some("Business Memory does not keep passwords, PINs, keys or codes. Keep them out of the assistant.");
    }
    for w in text.split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == '"' || c == '\'') {
        let w = w.trim_matches(|c: char| c == '.' || c == ')' || c == '(');
        if w.starts_with("sk-") || w.starts_with("AKIA") || (w.len() >= 24 && has_letters_and_digits(w)) {
            return Some("Business Memory does not keep passwords, PINs, keys or codes. Keep them out of the assistant.");
        }
        if w.contains('@') && w.contains('.') && w.len() > 5 {
            return Some("Business Memory is not for personal contact details. Keep them on the customer or supplier record.");
        }
        // IBAN-like: two letters, two digits, then 10+ letters or digits.
        let b = w.as_bytes();
        if w.len() >= 15 && b[0].is_ascii_alphabetic() && b[1].is_ascii_alphabetic() && b[2].is_ascii_digit() && b[3].is_ascii_digit() {
            return Some("Bank account numbers belong on the supplier record or in Settings, not in Business Memory.");
        }
    }
    // Long digit runs (spaces, dashes and a leading + allowed inside):
    // phone numbers and card numbers.
    let mut run: Vec<u32> = vec![];
    let mut plus = false;
    // A run glued to letters ("INV-2026-0042", "PO2026001") is an identifier.
    let mut ident = false;
    let chars: Vec<char> = text.chars().chain(std::iter::once('x')).collect();
    for (i, c) in chars.iter().enumerate() {
        if let Some(d) = c.to_digit(10) {
            if run.is_empty() {
                let before = if i > 0 { Some(chars[i - 1]) } else { None };
                plus = before == Some('+');
                ident = match before {
                    Some(b) if b.is_alphabetic() => true,
                    Some('-') | Some('/') => i > 1 && chars[i - 2].is_alphanumeric(),
                    _ => false,
                };
            }
            run.push(d);
            continue;
        }
        let joiner = (*c == ' ' || *c == '-') && !run.is_empty() && chars.get(i + 1).map(|n| n.is_ascii_digit()).unwrap_or(false);
        if joiner {
            continue;
        }
        if (13..=19).contains(&run.len()) && luhn(&run) {
            return Some("Business Memory does not keep card numbers.");
        }
        if !ident && (run.len() >= 8 || (plus && run.len() >= 7)) {
            return Some("Business Memory is not for personal contact details. Keep them on the customer or supplier record.");
        }
        run.clear();
        plus = false;
        ident = false;
    }
    None
}

/// (statement, linked record, scope, branch, valid until)
type Cleaned = (String, Option<(String, String)>, String, Option<String>, Option<String>);

fn entity(t: &str) -> AppResult<&'static (&'static str, &'static str, &'static str, &'static str, &'static [&'static str])> {
    ENTITY_TYPES.iter().find(|e| e.0 == t).ok_or_else(|| AppError::validation("Choose what the memory is about."))
}

/// The SQL condition for memories a person may see (on `m`).
fn scope_sql(s: &Session) -> String {
    let types: Vec<String> = ENTITY_TYPES.iter().filter(|e| e.4.iter().any(|p| s.has(p))).map(|e| format!("'{}'", e.0)).collect();
    let types = if types.is_empty() { "''".to_string() } else { types.join(",") };
    let branch = if s.has("branches.all") {
        String::new()
    } else {
        // Ids are ULIDs (validated); quotes are doubled all the same.
        format!(" AND (m.scope='business' OR m.branch_id='{}')", s.branch_id.replace('\'', "''"))
    };
    format!("(m.entity_type IS NULL OR m.entity_type IN ({types})){branch}")
}

/// Is the memory possibly outdated, and why (records win).
fn freshness(c: &Connection, m: &Value) -> AppResult<Option<String>> {
    let today = time::business_date(time::now(), &time::day(c)?)?;
    if let Some(v) = m["valid_until"].as_str() {
        if v < today.as_str() {
            return Ok(Some(format!("{MSG_OUTDATED}: it was valid until {v}.")));
        }
    }
    if let (Some(t), Some(id)) = (m["entity_type"].as_str(), m["entity_id"].as_str()) {
        let e = entity(t)?;
        let active_col = match t {
            "supplier" | "product" | "customer" => Some("active"),
            _ => None,
        };
        let updated = if t == "day_close" { "NULL" } else { "updated_at" };
        let sql = format!("SELECT {updated}, {} FROM {} WHERE {} = ?1", active_col.unwrap_or("1"), e.1, e.2);
        let row: Option<(Option<String>, i64)> =
            c.query_row(&sql, [id], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1).unwrap_or(1)))).optional()?;
        match row {
            None => return Ok(Some(format!("{MSG_OUTDATED}: the record it is about no longer exists."))),
            Some((_, 0)) => return Ok(Some(format!("{MSG_OUTDATED}: the record it is about is switched off."))),
            Some((Some(u), _)) => {
                let since = m["last_verified_at"].as_str().or(m["confirmed_at"].as_str()).unwrap_or_default();
                if !since.is_empty() && u.as_str() > since {
                    return Ok(Some(format!(
                        "{MSG_OUTDATED}: the record it is about changed after this was confirmed. The record is right."
                    )));
                }
            }
            _ => {}
        }
    }
    if let Some(since) = m["last_verified_at"].as_str().or(m["confirmed_at"].as_str()) {
        if let Ok(t) = chrono::DateTime::parse_from_rfc3339(since) {
            if (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_days() > STALE_DAYS {
                return Ok(Some(format!("{MSG_OUTDATED}: nobody has checked it for more than {STALE_DAYS} days.")));
            }
        }
    }
    Ok(None)
}

const COLS: &str =
    "m.memory_id, m.number, m.statement, m.status, m.scope, m.branch_id, m.entity_type, m.entity_id, m.source_kind, m.source_ref,
    m.source_excerpt, m.from_untrusted, m.proposed_by, (SELECT display_name FROM users u WHERE u.user_id=m.proposed_by), m.proposed_at,
    m.confirmed_by, (SELECT display_name FROM users u WHERE u.user_id=m.confirmed_by), m.confirmed_at, m.last_verified_at, m.valid_until,
    m.supersedes, m.superseded_by, m.decided_at, m.decision_note, m.revision, m.updated_at";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "memory_id": r.get::<_, String>(0)?, "number": r.get::<_, String>(1)?, "statement": r.get::<_, String>(2)?,
        "status": r.get::<_, String>(3)?, "scope": r.get::<_, String>(4)?, "branch_id": r.get::<_, Option<String>>(5)?,
        "entity_type": r.get::<_, Option<String>>(6)?, "entity_id": r.get::<_, Option<String>>(7)?,
        "source_kind": r.get::<_, String>(8)?, "source_ref": r.get::<_, Option<String>>(9)?,
        "source_excerpt": r.get::<_, Option<String>>(10)?, "from_untrusted": r.get::<_, i64>(11)? == 1,
        "proposed_by": r.get::<_, String>(12)?, "proposed_by_name": r.get::<_, Option<String>>(13)?, "proposed_at": r.get::<_, String>(14)?,
        "confirmed_by": r.get::<_, Option<String>>(15)?, "confirmed_by_name": r.get::<_, Option<String>>(16)?,
        "confirmed_at": r.get::<_, Option<String>>(17)?, "last_verified_at": r.get::<_, Option<String>>(18)?,
        "valid_until": r.get::<_, Option<String>>(19)?, "supersedes": r.get::<_, Option<String>>(20)?,
        "superseded_by": r.get::<_, Option<String>>(21)?, "decided_at": r.get::<_, Option<String>>(22)?,
        "decision_note": r.get::<_, Option<String>>(23)?, "revision": r.get::<_, i64>(24)?, "updated_at": r.get::<_, String>(25)?,
    }))
}

fn get_row(c: &Connection, s: &Session, id: &str) -> AppResult<Value> {
    let sql = format!("SELECT {COLS} FROM business_memories m WHERE m.memory_id=?1 AND {}", scope_sql(s));
    c.query_row(&sql, [id], row).optional()?.ok_or_else(|| AppError::not_found("Memory"))
}

fn reindex_one(c: &Connection, id: &str) -> AppResult<()> {
    let (seq, statement, status): (i64, String, String) =
        c.query_row("SELECT seq, statement, status FROM business_memories WHERE memory_id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    c.execute("DELETE FROM memory_fts WHERE rowid=?1", [seq])?;
    if status == "confirmed" || status == "candidate" {
        c.execute("INSERT INTO memory_fts(rowid, statement) VALUES (?1,?2)", params![seq, statement])?;
    }
    Ok(())
}

/// Rebuild the search index from the memories (it holds nothing else).
pub fn reindex(c: &Connection) -> AppResult<usize> {
    c.execute("DELETE FROM memory_fts", [])?;
    let ids: Vec<String> = {
        let mut st = c.prepare("SELECT memory_id FROM business_memories")?;
        let rows = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for id in &ids {
        reindex_one(c, id)?;
    }
    Ok(ids.len())
}

/// Confirmed memories about the same record, or close in wording: shown
/// next to a candidate so a person sees a contradiction before confirming.
fn related(c: &Connection, s: &Session, m: &Value) -> AppResult<Vec<Value>> {
    let id = m["memory_id"].as_str().unwrap_or_default();
    let mut out = vec![];
    if let (Some(t), Some(e)) = (m["entity_type"].as_str(), m["entity_id"].as_str()) {
        let sql = format!(
            "SELECT {COLS} FROM business_memories m WHERE m.status='confirmed' AND m.entity_type=?1 AND m.entity_id=?2 AND m.memory_id<>?3 AND {} LIMIT 10",
            scope_sql(s)
        );
        let mut st = c.prepare(&sql)?;
        out = st.query_map(params![t, e, id], row)?.collect::<Result<Vec<_>, _>>()?;
    }
    if out.is_empty() {
        let words: String = m["statement"]
            .as_str()
            .unwrap_or_default()
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|w| w.chars().count() >= 4)
            .take(6)
            .map(|w| format!("\"{}\"", w.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" OR ");
        if !words.is_empty() {
            let sql = format!(
                "SELECT {COLS} FROM business_memories m WHERE m.status='confirmed' AND m.memory_id<>?2 AND {}
                 AND m.seq IN (SELECT rowid FROM memory_fts WHERE memory_fts MATCH ?1 ORDER BY rank LIMIT 5)",
                scope_sql(s)
            );
            let mut st = c.prepare(&sql)?;
            out = st.query_map(params![words, id], row)?.collect::<Result<Vec<_>, _>>()?;
        }
    }
    Ok(out)
}

impl AppCore {
    fn memory_session(&self, token: &str, manage: bool) -> AppResult<Session> {
        let s = self.session(token)?;
        s.require("memory.view")?;
        if manage {
            s.require("memory.manage")?;
        }
        if self.require_back_office_writable().is_err() {
            return Err(AppError::conflict("Business Memory is kept on the main computer. Open it there."));
        }
        Ok(s)
    }

    fn memory_clean(&self, c: &Connection, s: &Session, input: &MemoryInput) -> AppResult<Cleaned> {
        let statement: String = input.statement.split_whitespace().collect::<Vec<_>>().join(" ");
        if statement.chars().count() < 3 || statement.chars().count() > 500 {
            return Err(AppError::validation("Write the memory as one sentence (up to 500 characters)."));
        }
        if let Some(why) = refuse_reason(&statement) {
            return Err(AppError::validation(why));
        }
        let link = match input.entity_type.as_deref().filter(|x| !x.is_empty()) {
            Some(t) => {
                let e = entity(t)?;
                if !e.4.iter().any(|p| s.has(p)) {
                    return Err(AppError::forbidden(e.4[0]));
                }
                let id = validate::id(input.entity_id.as_deref().unwrap_or_default(), "Record")?;
                let sql = format!("SELECT 1 FROM {} WHERE {} = ?1", e.1, e.2);
                if c.query_row(&sql, [&id], |_| Ok(())).optional()?.is_none() {
                    return Err(AppError::not_found("Record"));
                }
                Some((e.0.to_string(), id))
            }
            None => None,
        };
        let scope = match input.scope.as_deref().unwrap_or("business") {
            "business" => "business",
            "branch" => "branch",
            _ => return Err(AppError::validation("Choose where the memory applies.")),
        };
        let valid_until = match input.valid_until.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
            Some(d) => {
                chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|_| AppError::validation("Enter the date as YYYY-MM-DD."))?;
                Some(d.to_string())
            }
            None => None,
        };
        let branch = (scope == "branch").then(|| s.branch_id.clone());
        Ok((statement, link, scope.to_string(), branch, valid_until))
    }

    #[allow(clippy::too_many_arguments)]
    fn memory_insert(
        &self,
        tx: &Connection,
        s: &Session,
        input: &MemoryInput,
        status: &str,
        source_kind: &str,
        source_ref: Option<&str>,
        excerpt: Option<&str>,
        from_untrusted: bool,
        supersedes: Option<&str>,
    ) -> AppResult<Value> {
        let (statement, link, scope, branch, valid_until) = self.memory_clean(tx, s, input)?;
        let id = new_id();
        let seq = next_seq(tx, "business_memory")?;
        let number = format!("MEM-{seq:05}");
        let now = time::now_str();
        let proposer = if source_kind == "assistant" { "assistant".to_string() } else { s.user_id.clone() };
        let confirmed = status == "confirmed";
        let excerpt: Option<String> = excerpt.map(|x| x.chars().take(500).collect());
        tx.execute(
            "INSERT INTO business_memories(memory_id, seq, number, statement, status, scope, branch_id, entity_type, entity_id,
               source_kind, source_ref, source_excerpt, from_untrusted, proposed_by, proposed_at, confirmed_by, confirmed_at,
               last_verified_at, valid_until, supersedes, revision, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?17,?18,?19,1,?15,?15)",
            params![
                id,
                seq,
                number,
                statement,
                status,
                scope,
                branch,
                link.as_ref().map(|l| l.0.clone()),
                link.as_ref().map(|l| l.1.clone()),
                source_kind,
                source_ref,
                excerpt,
                from_untrusted as i64,
                proposer,
                now,
                confirmed.then(|| s.user_id.clone()),
                confirmed.then(|| now.clone()),
                valid_until,
                supersedes,
            ],
        )?;
        reindex_one(tx, &id)?;
        audit::record(
            tx,
            &self.actor(s, None),
            if confirmed { "memory.added" } else { "memory.suggested" },
            "memory",
            Some(&id),
            None,
            Some(&json!({ "number": number, "source": source_kind, "status": status })),
        )?;
        get_row(tx, s, &id)
    }

    /// A person writes a memory down (confirmed by them), or suggests one
    /// from a document passage (a candidate for someone to confirm).
    pub fn memory_add(&self, token: &str, input: MemoryInput, confirm: bool) -> AppResult<Value> {
        let s = self.memory_session(token, confirm)?;
        self.db.write(|tx| {
            let (kind, sref) = match input.document_id.as_deref().filter(|x| !x.is_empty()) {
                Some(d) => {
                    let d = validate::id(d, "Document")?;
                    tx.query_row("SELECT 1 FROM library_documents WHERE document_id=?1", [&d], |_| Ok(()))
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Document"))?;
                    ("document", Some(d))
                }
                None => ("person", None),
            };
            let excerpt = input.excerpt.clone();
            self.memory_insert(
                tx,
                &s,
                &input,
                if confirm { "confirmed" } else { "candidate" },
                kind,
                sref.as_deref(),
                excerpt.as_deref(),
                false,
                None,
            )
        })
    }

    /// The assistant suggests a candidate (never confirmed by it), with the
    /// person's own words as its source.
    pub fn memory_suggest_from_assistant(
        &self,
        token: &str,
        cid: &str,
        input: MemoryInput,
        asked: &str,
        untrusted: bool,
    ) -> AppResult<Value> {
        let s = self.memory_session(token, false)?;
        self.db.write(|tx| {
            let n: i64 =
                tx.query_row("SELECT COUNT(*) FROM business_memories WHERE source_kind='assistant' AND source_ref=?1", [cid], |r| {
                    r.get(0)
                })?;
            if n >= MAX_SUGGESTIONS_PER_CONVERSATION {
                return Err(AppError::conflict("Enough suggestions from this conversation. Review them in Business Memory first."));
            }
            let same: Option<String> = tx
                .query_row(
                    "SELECT number FROM business_memories WHERE status IN ('candidate','confirmed') AND lower(statement)=lower(?1)",
                    [input.statement.split_whitespace().collect::<Vec<_>>().join(" ")],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(n) = same {
                return Ok(json!({ "already": n, "message": "This is already in Business Memory." }));
            }
            let m = self.memory_insert(tx, &s, &input, "candidate", "assistant", Some(cid), Some(asked), untrusted, None)?;
            Ok(json!({
                "memory_id": m["memory_id"], "number": m["number"], "status": "candidate",
                "message": "Saved as a suggestion. A person confirms it in Business Memory before it is used.",
            }))
        })
    }

    pub fn memory_list(&self, token: &str, f: MemoryFilter) -> AppResult<Value> {
        let s = self.memory_session(token, false)?;
        let limit = validate::limit(f.limit, 50, 200);
        let offset = validate::offset(f.offset);
        let status = match f.tab.as_deref().unwrap_or("confirmed") {
            "confirmed" => "m.status='confirmed'",
            "review" => "m.status='candidate'",
            "archived" => "m.status='archived'",
            "history" => "m.status IN ('superseded','rejected')",
            _ => return Err(AppError::validation("Unknown tab.")),
        };
        let mut args: Vec<String> = vec![];
        let mut sql = format!("SELECT {COLS} FROM business_memories m WHERE {status} AND {}", scope_sql(&s));
        if let Some(t) = f.entity_type.as_deref().filter(|x| !x.is_empty()) {
            let t = entity(t)?.0;
            args.push(validate::id(f.entity_id.as_deref().unwrap_or_default(), "Record")?);
            sql.push_str(&format!(" AND m.entity_type='{t}' AND m.entity_id=?{}", args.len()));
        }
        if let Some(q) = f.q.as_deref().and_then(validate::fts_query) {
            args.push(q);
            sql.push_str(&format!(" AND m.seq IN (SELECT rowid FROM memory_fts WHERE memory_fts MATCH ?{})", args.len()));
        }
        sql.push_str(&format!(" ORDER BY m.updated_at DESC LIMIT {limit} OFFSET {offset}"));
        self.db.read(|c| {
            let mut st = c.prepare(&sql)?;
            let mut rows = st.query_map(rusqlite::params_from_iter(args.iter()), row)?.collect::<Result<Vec<_>, _>>()?;
            for r in rows.iter_mut() {
                r["outdated"] = json!(freshness(c, r)?);
                r["entity_label"] = json!(entity_label(c, r)?);
            }
            let counts = {
                let sql = format!(
                    "SELECT SUM(m.status='confirmed'), SUM(m.status='candidate'), SUM(m.status='archived') FROM business_memories m WHERE {}",
                    scope_sql(&s)
                );
                let (a, b, cc): (Option<i64>, Option<i64>, Option<i64>) = c.query_row(&sql, [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                json!({ "confirmed": a.unwrap_or(0), "review": b.unwrap_or(0), "archived": cc.unwrap_or(0) })
            };
            Ok(json!({ "rows": rows, "counts": counts }))
        })
    }

    pub fn memory_get(&self, token: &str, id: &str) -> AppResult<Value> {
        let s = self.memory_session(token, false)?;
        let id = validate::id(id, "Memory")?;
        self.db.read(|c| {
            let mut m = get_row(c, &s, &id)?;
            m["outdated"] = json!(freshness(c, &m)?);
            m["entity_label"] = json!(entity_label(c, &m)?);
            let related = related(c, &s, &m)?;
            // The chain: what this replaced and what replaced it.
            let mut chain = vec![];
            let mut cur = m["supersedes"].as_str().map(str::to_string);
            while let Some(x) = cur.take() {
                let Ok(r) = get_row(c, &s, &x) else { break };
                cur = r["supersedes"].as_str().map(str::to_string);
                chain.push(r);
                if chain.len() > 50 {
                    break;
                }
            }
            Ok(json!({ "memory": m, "related": related, "earlier": chain }))
        })
    }

    /// Confirm, reject, archive, restore or mark as checked. `revision` is
    /// the version the person looked at: a changed memory is refused.
    pub fn memory_decide(&self, token: &str, id: &str, action: &str, revision: i64, note: Option<String>) -> AppResult<Value> {
        let s = self.memory_session(token, true)?;
        let id = validate::id(id, "Memory")?;
        let note = note.map(|n| n.trim().chars().take(300).collect::<String>()).filter(|n| !n.is_empty());
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let m = get_row(tx, &s, &id)?;
            if m["revision"].as_i64() != Some(revision) {
                return Err(AppError::conflict("This memory changed while you were looking at it. Open it again."));
            }
            let status = m["status"].as_str().unwrap_or_default().to_string();
            let now = time::now_str();
            let (sql, new_status): (&str, &str) = match (action, status.as_str()) {
                ("confirm", "candidate") => (
                    "UPDATE business_memories SET status='confirmed', confirmed_by=?2, confirmed_at=?3, last_verified_at=?3, decision_note=?4, revision=revision+1, updated_at=?3 WHERE memory_id=?1",
                    "confirmed",
                ),
                ("reject", "candidate") => (
                    "UPDATE business_memories SET status='rejected', decided_by=?2, decided_at=?3, decision_note=?4, revision=revision+1, updated_at=?3 WHERE memory_id=?1",
                    "rejected",
                ),
                ("archive", "confirmed") => (
                    "UPDATE business_memories SET status='archived', decided_by=?2, decided_at=?3, decision_note=?4, revision=revision+1, updated_at=?3 WHERE memory_id=?1",
                    "archived",
                ),
                ("restore", "archived") => (
                    "UPDATE business_memories SET status='confirmed', decided_by=?2, decided_at=?3, decision_note=?4, last_verified_at=?3, revision=revision+1, updated_at=?3 WHERE memory_id=?1",
                    "confirmed",
                ),
                ("verify", "confirmed") => (
                    "UPDATE business_memories SET last_verified_at=?3, decided_by=?2, decided_at=?3, decision_note=COALESCE(?4, decision_note), revision=revision+1, updated_at=?3 WHERE memory_id=?1",
                    "confirmed",
                ),
                _ => return Err(AppError::conflict("This cannot be done to the memory as it is now.")),
            };
            if action == "reject" && note.is_none() {
                return Err(AppError::validation("Say why it is not right."));
            }
            tx.execute(sql, params![id, s.user_id, now, note])?;
            reindex_one(tx, &id)?;
            audit::record(
                tx,
                &actor,
                &format!("memory.{action}"),
                "memory",
                Some(&id),
                Some(&json!({ "status": status })),
                Some(&json!({ "status": new_status, "note": note })),
            )?;
            get_row(tx, &s, &id)
        })
    }

    /// Change a memory. A candidate is edited in place; a confirmed memory
    /// is superseded by a new confirmed one (the old one is kept).
    pub fn memory_edit(&self, token: &str, id: &str, revision: i64, input: MemoryInput) -> AppResult<Value> {
        let s = self.memory_session(token, true)?;
        let id = validate::id(id, "Memory")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let m = get_row(tx, &s, &id)?;
            if m["revision"].as_i64() != Some(revision) {
                return Err(AppError::conflict("This memory changed while you were looking at it. Open it again."));
            }
            match m["status"].as_str().unwrap_or_default() {
                "candidate" => {
                    let (statement, link, scope, branch, valid_until) = self.memory_clean(tx, &s, &input)?;
                    tx.execute(
                        "UPDATE business_memories SET statement=?2, entity_type=?3, entity_id=?4, scope=?5, branch_id=?6, valid_until=?7,
                           revision=revision+1, updated_at=?8 WHERE memory_id=?1",
                        params![
                            id,
                            statement,
                            link.as_ref().map(|l| l.0.clone()),
                            link.as_ref().map(|l| l.1.clone()),
                            scope,
                            branch,
                            valid_until,
                            time::now_str()
                        ],
                    )?;
                    reindex_one(tx, &id)?;
                    audit::record(tx, &actor, "memory.edited", "memory", Some(&id), Some(&json!({ "statement": m["statement"] })), Some(&json!({ "statement": statement })))?;
                    get_row(tx, &s, &id)
                }
                "confirmed" => {
                    let new = self.memory_insert(tx, &s, &input, "confirmed", "person", m["source_ref"].as_str(), None, false, Some(&id))?;
                    let nid = new["memory_id"].as_str().unwrap_or_default().to_string();
                    tx.execute(
                        "UPDATE business_memories SET status='superseded', superseded_by=?2, revision=revision+1, updated_at=?3 WHERE memory_id=?1",
                        params![id, nid, time::now_str()],
                    )?;
                    reindex_one(tx, &id)?;
                    audit::record(tx, &actor, "memory.superseded", "memory", Some(&id), None, Some(&json!({ "by": new["number"] })))?;
                    get_row(tx, &s, &nid)
                }
                _ => Err(AppError::conflict("Only a suggestion or a confirmed memory can be changed.")),
            }
        })
    }

    /// Confirmed memories that match the words (for people and the
    /// assistant), each with whether it may be outdated.
    pub fn memory_search(&self, token: &str, q: &str, entity_type: Option<String>, entity_id: Option<String>) -> AppResult<Value> {
        let s = self.memory_session(token, false)?;
        let mut sql = format!("SELECT {COLS} FROM business_memories m WHERE m.status='confirmed' AND {}", scope_sql(&s));
        let mut args: Vec<String> = vec![];
        if let Some(t) = entity_type.as_deref().filter(|x| !x.is_empty()) {
            let t = entity(t)?.0;
            args.push(validate::id(entity_id.as_deref().unwrap_or_default(), "Record")?);
            sql.push_str(&format!(" AND m.entity_type='{t}' AND m.entity_id=?{}", args.len()));
        }
        match validate::fts_query(q) {
            Some(fq) => {
                // Any of the words: memories are short.
                let any = fq.replace(" AND ", " OR ");
                args.push(any);
                sql.push_str(&format!(
                    " AND m.seq IN (SELECT rowid FROM memory_fts WHERE memory_fts MATCH ?{} ORDER BY rank LIMIT 200)",
                    args.len()
                ));
            }
            None if args.is_empty() => return Ok(json!({ "memories": [] })),
            None => {}
        }
        sql.push_str(&format!(" ORDER BY m.updated_at DESC LIMIT {MAX_RESULTS}"));
        self.db.read(|c| {
            let mut st = c.prepare(&sql)?;
            let rows = st.query_map(rusqlite::params_from_iter(args.iter()), row)?.collect::<Result<Vec<_>, _>>()?;
            let out = rows
                .into_iter()
                .map(|r| {
                    let outdated = freshness(c, &r)?;
                    Ok(json!({
                        "memory_id": r["memory_id"], "number": r["number"], "statement": r["statement"],
                        "entity_type": r["entity_type"], "entity_id": r["entity_id"], "entity_label": entity_label(c, &r)?,
                        "scope": r["scope"], "confirmed_at": r["confirmed_at"], "last_verified_at": r["last_verified_at"],
                        "valid_until": r["valid_until"], "outdated": outdated,
                    }))
                })
                .collect::<AppResult<Vec<_>>>()?;
            Ok(json!({ "memories": out }))
        })
    }

    pub fn memory_reindex(&self, token: &str) -> AppResult<Value> {
        let s = self.memory_session(token, true)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let n = reindex(tx)?;
            audit::record(tx, &actor, "memory.reindexed", "memory", None, None, Some(&json!({ "memories": n })))?;
            Ok(json!({ "memories": n }))
        })
    }
}

fn entity_label(c: &Connection, m: &Value) -> AppResult<Option<String>> {
    let (Some(t), Some(id)) = (m["entity_type"].as_str(), m["entity_id"].as_str()) else { return Ok(None) };
    // Customers are named by their record only (no personal data here).
    if t == "customer" {
        return Ok(None);
    }
    let e = entity(t)?;
    let sql = format!("SELECT {} FROM {} WHERE {} = ?1", e.3, e.1, e.2);
    Ok(c.query_row(&sql, [id], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_and_contact_details_are_refused() {
        for bad in [
            "The WiFi password is falcon2026",
            "Till PIN is 4826",
            "Our API key is sk-ant-abc123",
            "Use token 9f8e7d6c5b4a39281706f5e4d3c2b1a0 for the bank",
            "Call Ahmed on +973 3312 4567",
            "Ahmed's mobile 33124567",
            "Email layla@example.com for orders",
            "Pay to BH67BMAG00001299123456",
            "Company card 4111 1111 1111 1111",
            "كلمة السر هي 1234",
        ] {
            assert!(refuse_reason(bad).is_some(), "{bad}");
        }
        for ok in [
            "Gulf Dairy delivers on Sundays and Wednesdays before 9 am",
            "The landlord wants the rent by bank transfer before the 5th",
            "Rice 5kg sells out before Eid; order 40 bags two weeks ahead",
            "Invoice INV-2026-0042 was disputed in March",
            "يتم التوصيل يوم الأحد",
        ] {
            assert!(refuse_reason(ok).is_none(), "{ok}");
        }
    }
}
