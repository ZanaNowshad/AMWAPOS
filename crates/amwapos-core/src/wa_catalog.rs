//! WhatsApp Business catalogue publishing: the POS catalogue is the source of
//! truth and WhatsApp receives a copy (one direction, POS → WhatsApp).
//!
//! This module owns the durable state; the network part is the hub's
//! WhatsApp service (the same linked session that sends receipts), which
//! claims work here and reports outcomes back. Nothing here talks to
//! WhatsApp, and no product, price or checkout write ever waits for it.
//!
//! * Mappings (`wa_catalog_products`) are keyed by (account, product_id):
//!   the linked number, then the POS product. Product names are never
//!   identifiers. A different linked account starts with no mappings, so a
//!   remote id from account A is never used with account B.
//! * What WhatsApp should show for a product is a deterministic
//!   representation (`CatalogItem`); its SHA-256 `fingerprint` is stored when
//!   a write succeeds, so an unchanged product causes no remote write.
//! * Publishing starts only when an administrator runs the first sync for the
//!   linked account ("published"); after that, with "keep synchronised" on,
//!   the worker's scan queues whatever changed.
//! * Lifecycle per product: `queued → syncing → synced | hidden | removed |
//!   failed | remote_missing`; transient errors go back to `queued` with a
//!   delay, at most `MAX_ATTEMPTS` times. A write whose product changed
//!   while it was in flight is queued again instead of being reported as
//!   up to date.
//! * An administrator's full sync is a *run* (`wa_catalog_runs`): it has
//!   progress, it re-checks mapped products against the remote catalogue
//!   once, and it re-publishes products deleted on WhatsApp. The automatic
//!   sync never re-creates a product deleted on WhatsApp by itself.
//!
//! Invariants (tested in `tests/wa_catalog.rs` and the hub tests):
//! account isolation, idempotency (no write for an unchanged product),
//! POS authority, ownership (only mapped or adopted remote products are
//! written), exact prices, pictures only from the POS image store, failure
//! isolation, bounded retries, truthful terminal states, and nothing
//! published by an upgrade.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::audit;
use crate::catalog::PRICE_SQL;
use crate::error::{AppError, AppResult};
use crate::service::AppCore;
use crate::settings;
use crate::time;
use crate::validate;

pub const KEY_WA_CATALOG: &str = "whatsapp.catalog";
/// Conservative AMWAPOS limits (not published by WhatsApp): longer text is
/// cut deterministically at a character boundary with "…".
pub const NAME_MAX_CHARS: usize = 150;
pub const DESCRIPTION_MAX_CHARS: usize = 1000;
pub const RETAILER_ID_MAX_CHARS: usize = 100;
/// Transient failures are retried at most this many times, then `failed`.
pub const MAX_ATTEMPTS: i64 = 5;
/// A claim older than this is treated as abandoned (the worker stopped).
pub const STALE_CLAIM_MINUTES: i64 = 10;
/// A picture uploaded for a write that then failed is reused this long.
pub const PENDING_IMAGE_HOURS: i64 = 24;
/// The largest price sent (thousandths): 999,999,999.999 in a 3-decimal
/// currency. Anything larger is refused as invalid, never wrapped.
pub const MAX_PRICE_1000: i64 = 999_999_999_999;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WaCatalogSettings {
    /// Keep the WhatsApp catalogue synchronised after the first sync.
    pub auto_sync: bool,
    /// Linked accounts (digits) an administrator started publishing to.
    pub published_accounts: Vec<String>,
}

impl Default for WaCatalogSettings {
    fn default() -> Self {
        Self { auto_sync: true, published_accounts: vec![] }
    }
}

/// The linked account's key: the phone number digits of its JID
/// (`97330000000:12@s.whatsapp.net` → `97330000000`).
pub fn account_key(account: &str) -> Option<String> {
    let user = account.split('@').next()?.split(':').next()?;
    let d: String = user.chars().filter(|c| c.is_ascii_digit()).collect();
    (8..=15).contains(&d.len()).then_some(d)
}

/// WhatsApp catalogue prices are integers in thousandths of the currency
/// unit (the protocol's `priceAmount1000`). POS prices are integers in the
/// currency's minor unit; BHD has 3 decimals, so fils map 1:1. No floating
/// point is involved.
pub fn to_wa_price(minor: i64, currency_digits: u32) -> AppResult<i64> {
    if currency_digits > 3 {
        return Err(AppError::validation("WhatsApp catalogue prices support at most 3 decimals."));
    }
    if minor <= 0 {
        return Err(AppError::validation("A WhatsApp catalogue price must be above zero."));
    }
    minor
        .checked_mul(10i64.pow(3 - currency_digits))
        .filter(|v| *v <= MAX_PRICE_1000)
        .ok_or_else(|| AppError::validation("The price is too large for WhatsApp."))
}

/// Cut `s` to at most `max` characters, deterministically, marking the cut.
/// Works on characters (never splits UTF-8).
pub fn truncate_chars(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out = out.trim_end().to_string();
    out.push('…');
    out
}

/// One line of text: control characters (tabs, new lines, zero-width
/// controls) become spaces and runs of whitespace collapse to one.
pub fn clean_line(s: &str) -> String {
    let mapped: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A block of text (descriptions): line breaks are kept (CRLF → LF), other
/// control characters become spaces, spaces inside a line collapse, and at
/// most one empty line separates paragraphs.
pub fn clean_block(s: &str) -> String {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let mut out: Vec<String> = vec![];
    for line in s.split('\n') {
        let l = clean_line(line);
        if l.is_empty() && out.last().is_none_or(|p| p.is_empty()) {
            continue;
        }
        out.push(l);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// WhatsApp's retailer id: the POS product code (SKU). SKUs are unique and
/// at most 40 characters; an over-long legacy code keeps its uniqueness with
/// a hash suffix instead of an ellipsis (two long codes never collide).
pub fn retailer_id_for(sku: &str) -> String {
    let s = clean_line(sku);
    if s.chars().count() <= RETAILER_ID_MAX_CHARS {
        return s;
    }
    let head: String = s.chars().take(RETAILER_ID_MAX_CHARS - 13).collect();
    format!("{head}-{}", &hex::encode(Sha256::digest(s.as_bytes()))[..12])
}

/// What WhatsApp should show for one product. Field order is fixed, so the
/// JSON (and the fingerprint) is deterministic.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CatalogItem {
    pub name: String,
    pub description: Option<String>,
    /// Thousandths of the currency unit; None only for a hidden product
    /// without a usable price that was never published with one.
    pub price_1000: Option<i64>,
    pub currency: String,
    /// The POS product code (SKU) as WhatsApp's retailer id.
    pub retailer_id: String,
    /// The POS-managed picture (content hash); never an outside URL.
    pub image_hash: Option<String>,
    pub hidden: bool,
}

impl CatalogItem {
    pub fn fingerprint(&self) -> String {
        hex::encode(Sha256::digest(serde_json::to_vec(self).unwrap_or_default()))
    }
}

struct ProductData {
    product_id: String,
    sku: String,
    name: String,
    name_ar: Option<String>,
    description: Option<String>,
    active: bool,
    price: Option<i64>,
    /// The product's picture, only when its bytes are in the image store.
    image_hash: Option<String>,
}

fn load_products(c: &Connection, only: Option<&str>) -> AppResult<Vec<ProductData>> {
    let sql = format!(
        "SELECT p.product_id, p.sku, p.name, p.name_ar, p.description, p.active, {PRICE_SQL},
                CASE WHEN i.image_hash IS NULL THEN NULL ELSE p.image_hash END
         FROM products p LEFT JOIN product_images i ON i.image_hash = p.image_hash {}",
        if only.is_some() { "WHERE p.product_id = ?1" } else { "" }
    );
    let mut st = c.prepare(&sql)?;
    let map = |r: &rusqlite::Row| {
        Ok(ProductData {
            product_id: r.get(0)?,
            sku: r.get(1)?,
            name: r.get(2)?,
            name_ar: r.get(3)?,
            description: r.get(4)?,
            active: r.get::<_, i64>(5)? == 1,
            price: r.get(6)?,
            image_hash: r.get(7)?,
        })
    };
    let rows = match only {
        Some(id) => st.query_map([id], map)?.collect::<Result<Vec<_>, _>>()?,
        None => st.query_map([], map)?.collect::<Result<Vec<_>, _>>()?,
    };
    Ok(rows)
}

/// Why a product is not published (None: it is publishable).
fn not_publishable_reason(p: &ProductData, digits: u32) -> Option<&'static str> {
    if !p.active {
        Some("archived")
    } else if clean_line(&p.name).is_empty() {
        Some("no_name")
    } else if p.price.unwrap_or(0) <= 0 {
        Some("no_price")
    } else if to_wa_price(p.price.unwrap_or(0), digits).is_err() {
        Some("price_not_supported")
    } else {
        None
    }
}

/// Per-mapping facts that shape the representation.
#[derive(Default, Clone)]
struct Ctx {
    /// A picture WhatsApp refused: published without it until it changes.
    image_failed: Option<String>,
    /// Last published price (a hidden product that lost its price keeps it).
    last_price: Option<i64>,
}

/// The representation of `p`. Description: the merchant's description, or
/// else the Arabic name the merchant entered, or none (nothing is invented).
fn item_for(p: &ProductData, currency: &str, digits: u32, hidden: bool, ctx: &Ctx) -> CatalogItem {
    let description = p
        .description
        .as_deref()
        .map(clean_block)
        .filter(|d| !d.is_empty())
        .or_else(|| p.name_ar.as_deref().map(clean_line).filter(|d| !d.is_empty()))
        .map(|d| truncate_chars(&d, DESCRIPTION_MAX_CHARS));
    let price_1000 = match p.price.and_then(|v| to_wa_price(v, digits).ok()) {
        Some(v) => Some(v),
        None if hidden => ctx.last_price,
        None => None,
    };
    CatalogItem {
        name: truncate_chars(&clean_line(&p.name), NAME_MAX_CHARS),
        description,
        price_1000,
        currency: currency.to_string(),
        retailer_id: retailer_id_for(&p.sku),
        image_hash: p.image_hash.clone().filter(|h| ctx.image_failed.as_deref() != Some(h.as_str())),
        hidden,
    }
}

/// What the worker must do for one claimed product.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogAction {
    /// Create (no remote id yet) or update the remote product, visible.
    Upsert,
    /// Update the remote product to hidden (archived / not publishable).
    Hide,
    /// Delete the POS-owned remote product (the POS product is gone).
    Delete,
}

/// The action and representation the POS wants right now for a product
/// (None: the POS product no longer exists).
fn desired(p: Option<&ProductData>, currency: &str, digits: u32, ctx: &Ctx) -> (CatalogAction, Option<CatalogItem>) {
    match p {
        None => (CatalogAction::Delete, None),
        Some(p) => match not_publishable_reason(p, digits) {
            None => (CatalogAction::Upsert, Some(item_for(p, currency, digits, false, ctx))),
            Some(_) => (CatalogAction::Hide, Some(item_for(p, currency, digits, true, ctx))),
        },
    }
}

#[derive(Debug, Clone)]
pub struct CatalogJob {
    pub account: String,
    pub product_id: String,
    pub action: CatalogAction,
    pub item: Option<CatalogItem>,
    pub remote_id: Option<String>,
    /// WhatsApp URL of the already uploaded picture, when it is unchanged.
    pub image_url: Option<String>,
    /// The POS-managed JPEG to upload (only when the picture changed).
    pub image_jpeg: Option<Vec<u8>>,
    pub attempt: i64,
}

#[derive(Debug, Clone)]
pub enum CatalogOutcome {
    /// The remote product now matches `item` (visible or hidden).
    Published {
        remote_id: String,
        item: CatalogItem,
        image_url: Option<String>,
        adopted: bool,
        /// The picture (hash) WhatsApp refused; `item` was sent without it.
        image_rejected: Option<String>,
    },
    Deleted,
    /// The mapped remote product no longer exists on WhatsApp.
    RemoteMissing {
        error: String,
    },
    /// Disconnected, timeout, rate limit, server error: retried (bounded).
    Transient {
        error: String,
        retry_after_s: Option<u64>,
    },
    /// Validation or capability error: terminal until the product changes
    /// or an administrator retries.
    Failed {
        error: String,
    },
}

fn backoff_minutes(attempt: i64) -> i64 {
    match attempt {
        1 => 1,
        2 => 5,
        3 => 30,
        _ => 120,
    }
}

fn load_settings(c: &Connection) -> AppResult<WaCatalogSettings> {
    settings::get(c, KEY_WA_CATALOG)
}

fn is_published(c: &Connection, account: &str) -> AppResult<bool> {
    Ok(load_settings(c)?.published_accounts.iter().any(|a| a == account))
}

fn valid_account(account: &str) -> AppResult<String> {
    account_key(account).ok_or_else(|| AppError::conflict("WhatsApp is not linked to a number yet."))
}

struct Mapping {
    status: String,
    fingerprint: Option<String>,
    attempt_fingerprint: Option<String>,
    remote_id: Option<String>,
    ctx: Ctx,
}

fn load_mappings(c: &Connection, account: &str) -> AppResult<HashMap<String, Mapping>> {
    let mut maps = HashMap::new();
    let mut st = c.prepare(
        "SELECT product_id, status, fingerprint, remote_id, attempt_fingerprint, image_failed_hash, price_1000
         FROM wa_catalog_products WHERE account=?1",
    )?;
    for r in st.query_map([account], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Mapping {
                status: r.get(1)?,
                fingerprint: r.get(2)?,
                remote_id: r.get(3)?,
                attempt_fingerprint: r.get(4)?,
                ctx: Ctx { image_failed: r.get(5)?, last_price: r.get(6)? },
            },
        ))
    })? {
        let (k, v) = r?;
        maps.insert(k, v);
    }
    Ok(maps)
}

/// The open (unfinished) full-sync run of an account.
fn open_run(c: &Connection, account: &str) -> AppResult<Option<String>> {
    Ok(c.query_row("SELECT run_id FROM wa_catalog_runs WHERE account=?1 AND finished_at IS NULL", [account], |r| r.get(0)).optional()?)
}

/// A row reached a terminal state: count it in its run and detach it.
fn count_in_run(c: &Connection, account: &str, product_id: &str, state: &str) -> AppResult<()> {
    let run: Option<String> = c
        .query_row("SELECT run_id FROM wa_catalog_products WHERE account=?1 AND product_id=?2", params![account, product_id], |r| r.get(0))
        .optional()?
        .flatten();
    let Some(run) = run else { return Ok(()) };
    let col = match state {
        "synced" => "done_synced",
        "hidden" => "done_hidden",
        "removed" | "not_synced" => "done_removed",
        _ => "done_failed",
    };
    c.execute(&format!("UPDATE wa_catalog_runs SET {col}={col}+1 WHERE run_id=?1"), [&run])?;
    c.execute("UPDATE wa_catalog_products SET run_id=NULL WHERE account=?1 AND product_id=?2", params![account, product_id])?;
    finish_run_if_done(c, &run)
}

/// A run is finished when its remote check ran and no product of it is left.
fn finish_run_if_done(c: &Connection, run: &str) -> AppResult<()> {
    c.execute(
        "UPDATE wa_catalog_runs SET finished_at=?2
         WHERE run_id=?1 AND finished_at IS NULL AND verify<>'pending'
           AND NOT EXISTS (SELECT 1 FROM wa_catalog_products WHERE run_id=?1)",
        params![run, time::now_str()],
    )?;
    Ok(())
}

/// Queue every product whose WhatsApp representation differs from the last
/// synchronised one (and the POS-owned remote products whose POS product is
/// gone). Unchanged products are left alone. Rows queued here join `run`
/// when an administrator's full sync is open. Returns how many were queued.
fn scan(c: &Connection, account: &str, currency: &str, digits: u32, run: Option<&str>) -> AppResult<usize> {
    let products = load_products(c, None)?;
    let maps = load_mappings(c, account)?;
    let now = time::now_str();
    let mut queued = 0usize;
    let queue = |pid: &str| -> AppResult<usize> {
        Ok(c.execute(
            "UPDATE wa_catalog_products SET status='queued', attempts=0, next_at=NULL, last_error=NULL, run_id=COALESCE(run_id, ?4), updated_at=?3
             WHERE account=?1 AND product_id=?2 AND status NOT IN ('queued','syncing')",
            params![account, pid, now, run],
        )?)
    };
    let mut seen: HashSet<&str> = HashSet::new();
    for p in &products {
        seen.insert(p.product_id.as_str());
        let m = maps.get(&p.product_id);
        match (not_publishable_reason(p, digits), m) {
            (None, None) => {
                c.execute(
                    "INSERT INTO wa_catalog_products(account, product_id, status, run_id, created_at, updated_at) VALUES (?1,?2,'queued',?4,?3,?3)",
                    params![account, p.product_id, now, run],
                )?;
                queued += 1;
            }
            (None, Some(m)) => {
                if matches!(m.status.as_str(), "queued" | "syncing" | "remote_missing") {
                    continue;
                }
                let fp = item_for(p, currency, digits, false, &m.ctx).fingerprint();
                let changed = match m.status.as_str() {
                    // Failed: only a different representation is worth another try.
                    "failed" => m.attempt_fingerprint.as_deref() != Some(fp.as_str()),
                    "synced" => m.fingerprint.as_deref() != Some(fp.as_str()),
                    // hidden (re-activated), not_synced (now publishable), removed.
                    _ => true,
                };
                if changed {
                    queued += queue(&p.product_id)?;
                }
            }
            (Some(_), Some(m)) => {
                if matches!(m.status.as_str(), "queued" | "syncing" | "remote_missing" | "hidden") {
                    continue;
                }
                if m.status == "failed" {
                    let hidden_fp = item_for(p, currency, digits, true, &m.ctx).fingerprint();
                    if m.attempt_fingerprint.as_deref() == Some(hidden_fp.as_str()) {
                        continue;
                    }
                }
                if m.remote_id.is_some() {
                    queued += queue(&p.product_id)?; // hide it
                } else if m.status != "not_synced" {
                    c.execute(
                        "UPDATE wa_catalog_products SET status='not_synced', last_error=NULL, updated_at=?3 WHERE account=?1 AND product_id=?2",
                        params![account, p.product_id, now],
                    )?;
                }
            }
            (Some(_), None) => {}
        }
    }
    // POS products that no longer exist: delete their POS-owned remote copy.
    for (pid, m) in &maps {
        if !seen.contains(pid.as_str()) && m.remote_id.is_some() && !matches!(m.status.as_str(), "queued" | "syncing" | "removed") {
            queued += queue(pid)?;
        }
    }
    // Categories → collections: recorded as unsupported (the linked client
    // cannot write collections; see docs/STATUS.md). Never re-created.
    c.execute(
        "INSERT OR IGNORE INTO wa_catalog_collections(account, category_id, status, last_error, updated_at)
         SELECT ?1, category_id, 'unsupported', 'Collections cannot be written through the linked WhatsApp client.', ?2 FROM categories",
        params![account, now],
    )?;
    if queued > 0 {
        tracing::info!(queued, "WhatsApp catalogue: changes queued");
    }
    Ok(queued)
}

/// Short, operator-facing wording for an error stored on a product.
pub fn friendly_error(e: &str) -> String {
    let l = e.to_lowercase();
    if l.contains("not connected") || l.contains("disconnected") {
        "WhatsApp was not connected; it is retried automatically.".into()
    } else if l.contains("did not answer") || l.contains("timed out") || l.contains("timeout") {
        "WhatsApp did not answer in time; it is retried automatically.".into()
    } else if l.contains("limiting") || l.contains("429") {
        "WhatsApp asked AMWAPOS to slow down; it is retried later.".into()
    } else if l.contains("could not be read to check") {
        "The WhatsApp catalogue could not be read to rule out a duplicate; it is retried.".into()
    } else if l.contains("deleted on whatsapp") || l.contains("no such catalogue product") {
        "This product was deleted in WhatsApp. A full sync or Retry publishes it again.".into()
    } else if l.contains("already linked to another pos product") {
        "WhatsApp returned a product that belongs to another POS product. Check the product codes (SKU).".into()
    } else if l.contains("refused the catalogue change") || l.contains("bad product") {
        format!(
            "WhatsApp refused this product. Check its name, price and picture, then Retry. ({})",
            e.chars().take(80).collect::<String>()
        )
    } else {
        e.chars().take(160).collect()
    }
}

impl AppCore {
    fn require_catalog_admin(&self, token: &str) -> AppResult<crate::auth::Session> {
        let s = self.session(token)?;
        s.require("whatsapp.manage")?;
        s.require("products.manage")?;
        Ok(s)
    }

    /// Worker: publishing was started for this account and auto-sync is on.
    pub fn wa_catalog_auto(&self, account: &str) -> AppResult<bool> {
        let Some(acc) = account_key(account) else { return Ok(false) };
        self.db.read(|c| {
            let s = load_settings(c)?;
            Ok(s.auto_sync && s.published_accounts.contains(&acc))
        })
    }

    /// Worker: an administrator started publishing to this account.
    pub fn wa_catalog_published(&self, account: &str) -> AppResult<bool> {
        let Some(acc) = account_key(account) else { return Ok(false) };
        self.db.read(|c| is_published(c, &acc))
    }

    /// Worker: queue what changed since the last sync (auto-sync).
    pub fn wa_catalog_scan(&self, account: &str) -> AppResult<usize> {
        let acc = valid_account(account)?;
        self.db.write(|tx| {
            if !is_published(tx, &acc)? {
                return Ok(0);
            }
            let (cur, digits) = self.currency(tx)?;
            let run = open_run(tx, &acc)?;
            scan(tx, &acc, &cur, digits, run.as_deref())
        })
    }

    /// First sync (and later full reconciliations): an administrator starts
    /// publishing the catalogue to the linked account. Opens (or joins) a
    /// run, re-publishes products deleted on WhatsApp, queues every product
    /// that differs from WhatsApp and asks the worker to re-check the remote
    /// catalogue once. The worker does the remote writes.
    pub fn wa_catalog_start(&self, token: &str, account: &str) -> AppResult<Value> {
        let s = self.require_catalog_admin(token)?;
        self.require_back_office_writable()?;
        let acc = valid_account(account)?;
        let actor = self.actor(&s, None);
        let (queued, run) = self.db.write(|tx| {
            let mut st = load_settings(tx)?;
            let first = !st.published_accounts.contains(&acc);
            if first {
                st.published_accounts.push(acc.clone());
                settings::put(tx, KEY_WA_CATALOG, &st, Some(&s.user_id))?;
            }
            let now = time::now_str();
            let run = match open_run(tx, &acc)? {
                Some(r) => {
                    // Pressing Sync again joins the run in progress.
                    tx.execute("UPDATE wa_catalog_runs SET verify='pending' WHERE run_id=?1", [&r])?;
                    r
                }
                None => {
                    let r = crate::ids::new_id();
                    tx.execute(
                        "INSERT INTO wa_catalog_runs(run_id, account, started_at, started_by) VALUES (?1,?2,?3,?4)",
                        params![r, acc, now, s.user_id],
                    )?;
                    r
                }
            };
            // An explicit full sync re-publishes what was deleted on WhatsApp
            // (the stale remote id is dropped; creation checks for a copy first).
            let revived = tx.execute(
                "UPDATE wa_catalog_products SET status='queued', remote_id=NULL, fingerprint=NULL, image_url=NULL, attempts=0,
                    next_at=NULL, last_error=NULL, run_id=?3, updated_at=?2
                 WHERE account=?1 AND status='remote_missing'",
                params![acc, now, run],
            )?;
            let (cur, digits) = self.currency(tx)?;
            let n = scan(tx, &acc, &cur, digits, Some(&run))?;
            // Rows already waiting join the run too, so its progress is complete.
            tx.execute(
                "UPDATE wa_catalog_products SET run_id=?2 WHERE account=?1 AND status IN ('queued','syncing') AND run_id IS NULL",
                params![acc, run],
            )?;
            let pending: i64 =
                tx.query_row("SELECT COUNT(*) FROM wa_catalog_products WHERE account=?1 AND run_id=?2", params![acc, run], |r| r.get(0))?;
            let unchanged: i64 = tx.query_row(
                "SELECT COUNT(*) FROM wa_catalog_products WHERE account=?1 AND status IN ('synced','hidden') AND run_id IS NULL",
                [&acc],
                |r| r.get(0),
            )?;
            tx.execute(
                "UPDATE wa_catalog_runs SET total = done_synced + done_hidden + done_removed + done_failed + ?2, unchanged=?3 WHERE run_id=?1",
                params![run, pending, unchanged],
            )?;
            audit::record(
                tx,
                &actor,
                "whatsapp.catalog_sync",
                "whatsapp",
                None,
                None,
                Some(&json!({ "account": acc, "queued": n + revived, "first": first, "run_id": run })),
            )?;
            Ok((n + revived, run))
        })?;
        tracing::info!(queued, run = %run, "WhatsApp catalogue: full sync started");
        self.wa_catalog_overview(token, Some(account))
    }

    /// Retry failed and remote-missing products (all, or one).
    pub fn wa_catalog_retry(&self, token: &str, account: &str, product_id: Option<&str>) -> AppResult<Value> {
        let s = self.require_catalog_admin(token)?;
        let acc = valid_account(account)?;
        let pid = product_id.map(|p| validate::id(p, "Product")).transpose()?;
        let actor = self.actor(&s, None);
        let n = self.db.write(|tx| {
            let now = time::now_str();
            // Retry also forgets a refused picture: the merchant may have fixed it.
            let n = match &pid {
                Some(p) => tx.execute(
                    "UPDATE wa_catalog_products SET status='queued', attempts=0, next_at=NULL, last_error=NULL, image_failed_hash=NULL,
                        remote_id=CASE WHEN status='remote_missing' THEN NULL ELSE remote_id END, updated_at=?3
                     WHERE account=?1 AND product_id=?2 AND status IN ('failed','remote_missing','not_synced','synced','hidden')",
                    params![acc, p, now],
                )?,
                None => tx.execute(
                    "UPDATE wa_catalog_products SET status='queued', attempts=0, next_at=NULL, last_error=NULL, image_failed_hash=NULL,
                        remote_id=CASE WHEN status='remote_missing' THEN NULL ELSE remote_id END, updated_at=?2
                     WHERE account=?1 AND status IN ('failed','remote_missing')",
                    params![acc, now],
                )?,
            };
            audit::record(tx, &actor, "whatsapp.catalog_retry", "whatsapp", pid.as_deref(), None, Some(&json!({ "queued": n })))?;
            Ok(n)
        })?;
        Ok(json!({ "queued": n }))
    }

    pub fn wa_catalog_configure(&self, token: &str, auto_sync: bool) -> AppResult<Value> {
        let s = self.require_catalog_admin(token)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let mut st = load_settings(tx)?;
            st.auto_sync = auto_sync;
            settings::put(tx, KEY_WA_CATALOG, &st, Some(&s.user_id))?;
            audit::record(tx, &actor, "settings.whatsapp_catalog", "settings", None, None, Some(&json!({ "auto_sync": auto_sync })))?;
            Ok(())
        })?;
        Ok(json!({ "auto_sync": auto_sync }))
    }

    /// Counts and state for the admin screen (for the linked account).
    pub fn wa_catalog_overview(&self, token: &str, account: Option<&str>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("whatsapp.manage")?;
        let acc = account.and_then(account_key);
        self.db.read(|c| {
            let st = load_settings(c)?;
            let (cur, digits) = self.currency(c)?;
            let products = load_products(c, None)?;
            let mut publishable = 0i64;
            let mut not_publishable: HashMap<&str, i64> = HashMap::new();
            for p in &products {
                match not_publishable_reason(p, digits) {
                    None => publishable += 1,
                    Some(r) => *not_publishable.entry(r).or_default() += 1,
                }
            }
            let mut counts: HashMap<String, i64> = HashMap::new();
            let (mut last_synced, mut failures, mut collections, mut out_of_date, mut retrying) = (None::<String>, vec![], 0i64, 0i64, 0i64);
            let mut run = Value::Null;
            let mut last_run = Value::Null;
            if let Some(a) = &acc {
                let mut q = c.prepare("SELECT status, COUNT(*) FROM wa_catalog_products WHERE account=?1 GROUP BY status")?;
                counts = q.query_map([a], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<Result<HashMap<_, _>, _>>()?;
                retrying = c.query_row(
                    "SELECT COUNT(*) FROM wa_catalog_products WHERE account=?1 AND status='queued' AND next_at IS NOT NULL",
                    [a],
                    |r| r.get(0),
                )?;
                last_synced = c.query_row("SELECT MAX(last_synced_at) FROM wa_catalog_products WHERE account=?1", [a], |r| r.get(0))?;
                let mut q = c.prepare(
                    "SELECT m.product_id, p.name, m.status, m.last_error FROM wa_catalog_products m LEFT JOIN products p ON p.product_id=m.product_id
                     WHERE m.account=?1 AND m.status IN ('failed','remote_missing') ORDER BY m.updated_at DESC LIMIT 20",
                )?;
                failures = q
                    .query_map([a], |r| {
                        let err: Option<String> = r.get(3)?;
                        Ok(json!({ "product_id": r.get::<_, String>(0)?, "name": r.get::<_, Option<String>>(1)?,
                                   "status": r.get::<_, String>(2)?, "error": err.as_deref().map(friendly_error), "detail": err }))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                collections = c.query_row("SELECT COUNT(*) FROM wa_catalog_collections WHERE account=?1", [a], |r| r.get(0))?;
                // Products changed in AMWAPOS since their last publication
                // (automatic sync off, or not scanned yet).
                let maps = load_mappings(c, a)?;
                let by_id: HashMap<&str, &ProductData> = products.iter().map(|p| (p.product_id.as_str(), p)).collect();
                for (pid, m) in &maps {
                    if !matches!(m.status.as_str(), "synced" | "hidden") {
                        continue;
                    }
                    let (_, item) = desired(by_id.get(pid.as_str()).copied(), &cur, digits, &m.ctx);
                    let fresh = match (&item, m.status.as_str()) {
                        (Some(i), "synced") => !i.hidden && m.fingerprint.as_deref() == Some(i.fingerprint().as_str()),
                        (Some(i), _) => i.hidden,
                        (None, _) => false,
                    };
                    if !fresh {
                        out_of_date += 1;
                    }
                }
                let run_json = |r: &rusqlite::Row| -> rusqlite::Result<Value> {
                    let total: i64 = r.get(3)?;
                    let done: i64 = r.get::<_, i64>(5)? + r.get::<_, i64>(6)? + r.get::<_, i64>(7)? + r.get::<_, i64>(8)?;
                    Ok(json!({ "run_id": r.get::<_, String>(0)?, "started_at": r.get::<_, String>(1)?, "finished_at": r.get::<_, Option<String>>(2)?,
                               "total": total, "processed": done, "unchanged": r.get::<_, i64>(4)?, "synced": r.get::<_, i64>(5)?,
                               "hidden": r.get::<_, i64>(6)?, "removed": r.get::<_, i64>(7)?, "failed": r.get::<_, i64>(8)?,
                               "verify": r.get::<_, String>(9)?, "verify_note": r.get::<_, Option<String>>(10)? }))
                };
                let sel = "SELECT run_id, started_at, finished_at, total, unchanged, done_synced, done_hidden, done_removed, done_failed, verify, verify_note
                           FROM wa_catalog_runs WHERE account=?1";
                run = c.query_row(&format!("{sel} AND finished_at IS NULL"), [a], run_json).optional()?.unwrap_or(Value::Null);
                last_run = c
                    .query_row(&format!("{sel} AND finished_at IS NOT NULL ORDER BY finished_at DESC LIMIT 1"), [a], run_json)
                    .optional()?
                    .unwrap_or(Value::Null);
            }
            Ok(json!({
                "account": acc,
                "published": acc.as_ref().is_some_and(|a| st.published_accounts.contains(a)),
                "auto_sync": st.auto_sync,
                "publishable": publishable,
                "not_publishable": not_publishable,
                "counts": counts,
                "retrying": retrying,
                "out_of_date": out_of_date,
                "last_synced_at": last_synced,
                "failures": failures,
                "categories": c.query_row("SELECT COUNT(*) FROM categories", [], |r| r.get::<_, i64>(0))?,
                "collections_recorded": collections,
                "run": run,
                "last_run": last_run,
            }))
        })
    }

    /// One product's catalogue state (product editor).
    pub fn wa_catalog_product_state(&self, token: &str, account: Option<&str>, product_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("products.view")?;
        let pid = validate::id(product_id, "Product")?;
        let Some(acc) = account.and_then(account_key) else { return Ok(json!({ "status": null, "published": false })) };
        self.db.read(|c| {
            let published = is_published(c, &acc)?;
            let (cur, digits) = self.currency(c)?;
            let maps = load_mappings(c, &acc)?;
            let product = load_products(c, Some(&pid))?.into_iter().next();
            let reason = product.as_ref().and_then(|p| not_publishable_reason(p, digits));
            let row: Option<(Option<String>, Option<String>)> = c
                .query_row(
                    "SELECT last_synced_at, last_error FROM wa_catalog_products WHERE account=?1 AND product_id=?2",
                    params![acc, pid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            Ok(match (maps.get(&pid), row) {
                (Some(m), Some((synced, err))) => {
                    let (_, item) = desired(product.as_ref(), &cur, digits, &m.ctx);
                    let out_of_date = m.status == "synced" && item.map(|i| i.fingerprint()) != m.fingerprint;
                    json!({ "published": published, "status": m.status, "on_whatsapp": m.remote_id.is_some(),
                            "last_synced_at": synced, "last_error": err.as_deref().map(friendly_error), "not_publishable": reason,
                            "out_of_date": out_of_date, "picture_refused": m.ctx.image_failed.is_some() })
                }
                _ => json!({ "published": published, "status": "not_synced", "on_whatsapp": false, "not_publishable": reason }),
            })
        })
    }

    /// Worker: the remote id is already mapped to a POS product (adoption guard).
    pub fn wa_catalog_remote_owner(&self, account: &str, remote_id: &str) -> AppResult<Option<String>> {
        let acc = valid_account(account)?;
        self.db.read(|c| {
            Ok(c.query_row("SELECT product_id FROM wa_catalog_products WHERE account=?1 AND remote_id=?2", params![acc, remote_id], |r| {
                r.get(0)
            })
            .optional()?)
        })
    }

    /// Worker start: nothing can be in flight yet, so every claim left by a
    /// previous worker (crash, restart, task restart) is released at once
    /// instead of after the claim timeout.
    pub fn wa_catalog_release_claims(&self) -> AppResult<usize> {
        let n = self.db.write(|tx| {
            Ok(tx.execute(
                "UPDATE wa_catalog_products SET status='queued', claimed_at=NULL, updated_at=?1 WHERE status='syncing'",
                [time::now_str()],
            )?)
        })?;
        if n > 0 {
            tracing::warn!(n, "WhatsApp catalogue: claims of the previous worker released");
        }
        Ok(n)
    }

    /// Worker: a picture was uploaded for this product; a retry of a failed
    /// write reuses it for `PENDING_IMAGE_HOURS` instead of uploading again.
    pub fn wa_catalog_note_upload(&self, account: &str, product_id: &str, image_hash: &str, url: &str) -> AppResult<()> {
        let acc = valid_account(account)?;
        self.db.write(|tx| {
            tx.execute(
                "UPDATE wa_catalog_products SET pending_image_hash=?3, pending_image_url=?4, pending_image_at=?5 WHERE account=?1 AND product_id=?2",
                params![acc, product_id, image_hash, url, time::now_str()],
            )?;
            Ok(())
        })
    }

    /// Worker: the open run of this account still wants its remote check.
    pub fn wa_catalog_verify_due(&self, account: &str) -> AppResult<Option<String>> {
        let acc = valid_account(account)?;
        self.db.read(|c| {
            Ok(c.query_row(
                "SELECT run_id FROM wa_catalog_runs WHERE account=?1 AND finished_at IS NULL AND verify='pending'",
                [&acc],
                |r| r.get(0),
            )
            .optional()?)
        })
    }

    /// Worker: the remote catalogue as listed for the run's check (`None`:
    /// it could not be read completely). Mapped products that are not listed
    /// are queued once: the write itself tells whether they still exist (a
    /// missing product answers "not found"), so an incomplete listing can
    /// never cause a duplicate.
    pub fn wa_catalog_verify(&self, account: &str, run_id: &str, listed: Option<&HashSet<String>>) -> AppResult<usize> {
        let acc = valid_account(account)?;
        self.db.write(|tx| {
            let open: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM wa_catalog_runs WHERE run_id=?1 AND account=?2 AND finished_at IS NULL)",
                params![run_id, acc],
                |r| r.get(0),
            )?;
            if !open {
                return Ok(0);
            }
            let Some(listed) = listed else {
                tx.execute(
                    "UPDATE wa_catalog_runs SET verify='skipped', verify_note='The WhatsApp catalogue could not be read completely.' WHERE run_id=?1",
                    [run_id],
                )?;
                finish_run_if_done(tx, run_id)?;
                return Ok(0);
            };
            let rows: Vec<(String, String)> = {
                let mut st = tx.prepare(
                    "SELECT product_id, remote_id FROM wa_catalog_products WHERE account=?1 AND remote_id IS NOT NULL AND status IN ('synced','hidden')",
                )?;
                let r = st.query_map([&acc], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
                r
            };
            let now = time::now_str();
            let mut n = 0usize;
            for (pid, rid) in rows {
                if !listed.contains(&rid) {
                    n += tx.execute(
                        "UPDATE wa_catalog_products SET status='queued', attempts=0, next_at=NULL, run_id=?3, updated_at=?4
                         WHERE account=?1 AND product_id=?2 AND status IN ('synced','hidden')",
                        params![acc, pid, run_id, now],
                    )?;
                }
            }
            tx.execute(
                "UPDATE wa_catalog_runs SET verify='done', verify_note=?2, total=total+?3 WHERE run_id=?1",
                params![run_id, format!("{} mapped products not listed by WhatsApp were checked again.", n), n as i64],
            )?;
            finish_run_if_done(tx, run_id)?;
            if n > 0 {
                tracing::info!(n, "WhatsApp catalogue: mapped products not listed remotely queued for a check");
            }
            Ok(n)
        })
    }

    /// Worker: claim up to `limit` due products. The claim is a conditional
    /// update, so two workers never take the same product; the action and
    /// payload are computed from the POS data at claim time.
    pub fn wa_catalog_claim(&self, account: &str, limit: usize) -> AppResult<Vec<CatalogJob>> {
        let acc = valid_account(account)?;
        let now = time::now_str();
        let stale = time::fmt(time::now() - chrono::Duration::minutes(STALE_CLAIM_MINUTES));
        let fresh_upload = time::fmt(time::now() - chrono::Duration::hours(PENDING_IMAGE_HOURS));
        let due: bool = self.db.read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM wa_catalog_products WHERE account=?1 AND
                   ((status='queued' AND (next_at IS NULL OR next_at <= ?2)) OR (status='syncing' AND claimed_at < ?3)))",
                params![acc, now, stale],
                |r| r.get(0),
            )?)
        })?;
        if !due {
            return Ok(vec![]);
        }
        self.db.write(|tx| {
            if !is_published(tx, &acc)? {
                return Ok(vec![]);
            }
            let n = tx.execute(
                "UPDATE wa_catalog_products SET status='queued', claimed_at=NULL WHERE account=?1 AND status='syncing' AND claimed_at < ?2",
                params![acc, stale],
            )?;
            if n > 0 {
                tracing::warn!(n, "WhatsApp catalogue: abandoned claims re-queued");
            }
            let (cur, digits) = self.currency(tx)?;
            // Oldest first: a product that keeps failing is re-queued with a
            // later `updated_at` and a delay, so it never blocks the others.
            let pids: Vec<String> = {
                let mut st = tx.prepare(
                    "SELECT product_id FROM wa_catalog_products WHERE account=?1 AND status='queued' AND (next_at IS NULL OR next_at <= ?2)
                     ORDER BY updated_at, product_id LIMIT ?3",
                )?;
                let rows = st.query_map(params![acc, now, limit as i64], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
                rows
            };
            let maps = load_mappings(tx, &acc)?;
            let mut jobs = vec![];
            for pid in pids {
                let claimed = tx.execute(
                    "UPDATE wa_catalog_products SET status='syncing', claimed_at=?3, last_attempt_at=?3, attempts=attempts+1, updated_at=?3
                     WHERE account=?1 AND product_id=?2 AND status='queued'",
                    params![acc, pid, now],
                )?;
                if claimed != 1 {
                    continue;
                }
                type Row = (Option<String>, Option<String>, Option<String>, i64, Option<String>, Option<String>, Option<String>);
                let (remote_id, image_hash, image_url, attempt, p_hash, p_url, p_at): Row = tx.query_row(
                    "SELECT remote_id, image_hash, image_url, attempts, pending_image_hash, pending_image_url, pending_image_at
                     FROM wa_catalog_products WHERE account=?1 AND product_id=?2",
                    params![acc, pid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
                )?;
                let mut ctx = maps.get(&pid).map(|m| m.ctx.clone()).unwrap_or_default();
                let product = load_products(tx, Some(&pid))?.into_iter().next();
                let (action, mut item) = desired(product.as_ref(), &cur, digits, &ctx);
                if remote_id.is_none() && action != CatalogAction::Upsert {
                    // Nothing on WhatsApp to hide or delete.
                    let st = if action == CatalogAction::Delete { "removed" } else { "not_synced" };
                    count_in_run(tx, &acc, &pid, st)?;
                    tx.execute(
                        "UPDATE wa_catalog_products SET status=?3, claimed_at=NULL, attempts=0, updated_at=?4 WHERE account=?1 AND product_id=?2",
                        params![acc, pid, st, now],
                    )?;
                    continue;
                }
                // The POS-managed picture: re-used while unchanged (or freshly
                // uploaded by a failed attempt), else read from the image store.
                // A picture that cannot be read as a JPEG is left out (and
                // remembered), instead of failing the product.
                let want_hash = item.as_ref().and_then(|i| i.image_hash.clone());
                let (mut reuse, mut jpeg) = (None, None);
                if let Some(h) = &want_hash {
                    if image_hash.as_deref() == Some(h.as_str()) && image_url.is_some() {
                        reuse = image_url.clone();
                    } else if p_hash.as_deref() == Some(h.as_str()) && p_url.is_some() && p_at.as_deref().is_some_and(|t| t > fresh_upload.as_str()) {
                        reuse = p_url.clone();
                    } else {
                        let data: Option<String> =
                            tx.query_row("SELECT data_b64 FROM product_images WHERE image_hash=?1", [h], |r| r.get(0)).optional()?;
                        jpeg = data.and_then(|d| crate::ids::b64_decode(&d)).filter(|b| b.starts_with(&[0xFF, 0xD8, 0xFF]));
                        if jpeg.is_none() {
                            tracing::warn!(product_id = %pid, "WhatsApp catalogue: stored picture unreadable; published without it");
                            tx.execute(
                                "UPDATE wa_catalog_products SET image_failed_hash=?3 WHERE account=?1 AND product_id=?2",
                                params![acc, pid, h],
                            )?;
                            ctx.image_failed = Some(h.clone());
                            item = desired(product.as_ref(), &cur, digits, &ctx).1;
                        }
                    }
                }
                if let Some(i) = &item {
                    tx.execute(
                        "UPDATE wa_catalog_products SET attempt_fingerprint=?3 WHERE account=?1 AND product_id=?2",
                        params![acc, pid, i.fingerprint()],
                    )?;
                }
                jobs.push(CatalogJob {
                    account: acc.clone(),
                    product_id: pid,
                    action,
                    item,
                    remote_id,
                    image_url: reuse,
                    image_jpeg: jpeg,
                    attempt,
                });
            }
            Ok(jobs)
        })
    }

    /// Worker: record the outcome of a claimed product. Only a claimed row
    /// is written; a failed remote write is never recorded as synchronised,
    /// and a write whose product changed meanwhile is queued again.
    pub fn wa_catalog_complete(&self, account: &str, product_id: &str, outcome: CatalogOutcome) -> AppResult<String> {
        let acc = valid_account(account)?;
        let now = time::now_str();
        self.db.write(|tx| {
            let row: Option<(i64, Option<String>)> = tx
                .query_row(
                    "SELECT attempts, run_id FROM wa_catalog_products WHERE account=?1 AND product_id=?2 AND status='syncing'",
                    params![acc, product_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((attempts, run)) = row else { return Ok("superseded".to_string()) };
            let run_open = match &run {
                Some(r) => tx.query_row("SELECT finished_at IS NULL FROM wa_catalog_runs WHERE run_id=?1", [r], |x| x.get::<_, bool>(0)).optional()?.unwrap_or(false),
                None => false,
            };
            let state = match outcome {
                CatalogOutcome::Published { remote_id, item, image_url, adopted, image_rejected } => {
                    let owner: Option<String> = tx
                        .query_row(
                            "SELECT product_id FROM wa_catalog_products WHERE account=?1 AND remote_id=?2 AND product_id<>?3",
                            params![acc, remote_id, product_id],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if owner.is_some() {
                        tx.execute(
                            "UPDATE wa_catalog_products SET status='failed', claimed_at=NULL, next_at=NULL, updated_at=?3,
                                last_error='WhatsApp returned a product that is already linked to another POS product.'
                             WHERE account=?1 AND product_id=?2",
                            params![acc, product_id, now],
                        )?;
                        "failed"
                    } else {
                        let status = if item.hidden { "hidden" } else { "synced" };
                        tx.execute(
                            "UPDATE wa_catalog_products SET status=?3, remote_id=?4, fingerprint=?5, image_hash=?6, image_url=?7,
                                adopted=MAX(adopted, ?8), attempts=0, next_at=NULL, claimed_at=NULL, last_error=NULL,
                                price_1000=COALESCE(?10, price_1000), image_failed_hash=COALESCE(?11, image_failed_hash),
                                pending_image_hash=NULL, pending_image_url=NULL, pending_image_at=NULL,
                                last_synced_at=?9, updated_at=?9
                             WHERE account=?1 AND product_id=?2",
                            params![
                                acc,
                                product_id,
                                status,
                                remote_id,
                                item.fingerprint(),
                                item.image_hash,
                                image_url,
                                adopted as i64,
                                now,
                                item.price_1000,
                                image_rejected
                            ],
                        )?;
                        // Changed while the write was in flight? Then WhatsApp
                        // has the older version: queue the new one (when
                        // syncing is automatic or part of a full sync), never
                        // report it as up to date.
                        let (cur, digits) = self.currency(tx)?;
                        let ctx = load_mappings(tx, &acc)?.remove(product_id).map(|m| m.ctx).unwrap_or_default();
                        let product = load_products(tx, Some(product_id))?.into_iter().next();
                        let (_, want) = desired(product.as_ref(), &cur, digits, &ctx);
                        let stale = want.map(|w| w.fingerprint()) != Some(item.fingerprint());
                        if stale && (run_open || load_settings(tx)?.auto_sync) {
                            tx.execute(
                                "UPDATE wa_catalog_products SET status='queued', updated_at=?3 WHERE account=?1 AND product_id=?2",
                                params![acc, product_id, now],
                            )?;
                            return Ok("requeued".to_string());
                        }
                        status
                    }
                }
                CatalogOutcome::Deleted => {
                    tx.execute(
                        "UPDATE wa_catalog_products SET status='removed', remote_id=NULL, fingerprint=NULL, image_url=NULL, claimed_at=NULL,
                            attempts=0, last_error=NULL, last_synced_at=?3, updated_at=?3 WHERE account=?1 AND product_id=?2",
                        params![acc, product_id, now],
                    )?;
                    "removed"
                }
                CatalogOutcome::RemoteMissing { error } => {
                    if run_open {
                        // Part of an administrator's full sync: publish it again
                        // (creation checks the catalogue for a copy first).
                        tx.execute(
                            "UPDATE wa_catalog_products SET status='queued', remote_id=NULL, fingerprint=NULL, image_url=NULL, claimed_at=NULL,
                                attempts=0, next_at=NULL, last_error=?3, updated_at=?4 WHERE account=?1 AND product_id=?2",
                            params![acc, product_id, error, now],
                        )?;
                        return Ok("requeued".to_string());
                    }
                    tx.execute(
                        "UPDATE wa_catalog_products SET status='remote_missing', claimed_at=NULL, last_error=?3, updated_at=?4
                         WHERE account=?1 AND product_id=?2",
                        params![acc, product_id, error, now],
                    )?;
                    "remote_missing"
                }
                CatalogOutcome::Failed { error } => {
                    tx.execute(
                        "UPDATE wa_catalog_products SET status='failed', claimed_at=NULL, next_at=NULL, last_error=?3, updated_at=?4
                         WHERE account=?1 AND product_id=?2",
                        params![acc, product_id, error, now],
                    )?;
                    "failed"
                }
                CatalogOutcome::Transient { error, retry_after_s } => {
                    if attempts >= MAX_ATTEMPTS {
                        tx.execute(
                            "UPDATE wa_catalog_products SET status='failed', claimed_at=NULL, next_at=NULL, last_error=?3, updated_at=?4
                             WHERE account=?1 AND product_id=?2",
                            params![acc, product_id, error, now],
                        )?;
                        "failed"
                    } else {
                        let secs = (backoff_minutes(attempts) * 60).max(retry_after_s.unwrap_or(0).min(24 * 3600) as i64);
                        let next = time::fmt(time::now() + chrono::Duration::seconds(secs));
                        tx.execute(
                            "UPDATE wa_catalog_products SET status='queued', claimed_at=NULL, next_at=?3, last_error=?4, updated_at=?5
                             WHERE account=?1 AND product_id=?2",
                            params![acc, product_id, next, error, now],
                        )?;
                        return Ok("retry".to_string());
                    }
                }
            };
            count_in_run(tx, &acc, product_id, state)?;
            Ok(state.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bhd_prices_convert_exactly_to_thousandths() {
        // BHD has 3 decimals: fils are thousandths already.
        for (minor, want) in [
            (1, 1),
            (10, 10),
            (100, 100),
            (999, 999),
            (1000, 1000),
            (1250, 1250),
            (9990, 9990),
            (10_005, 10_005),
            (99_999, 99_999),
            (100_000, 100_000),
            (MAX_PRICE_1000, MAX_PRICE_1000),
        ] {
            assert_eq!(to_wa_price(minor, 3).unwrap(), want, "{minor} fils");
        }
        // Two-decimal and zero-decimal currencies scale up exactly.
        assert_eq!(to_wa_price(1999, 2).unwrap(), 19_990);
        assert_eq!(to_wa_price(5, 0).unwrap(), 5000);
        assert!(to_wa_price(1, 4).is_err(), "more than 3 decimals cannot be represented");
        assert!(to_wa_price(i64::MAX, 2).is_err(), "overflow is refused, never wrapped");
        assert!(to_wa_price(MAX_PRICE_1000 + 1, 3).is_err());
        assert!(to_wa_price(0, 3).is_err() && to_wa_price(-5, 3).is_err(), "zero and negative prices are invalid");
    }

    #[test]
    fn accounts_are_keyed_by_number_without_device() {
        assert_eq!(account_key("97330000000:12@s.whatsapp.net").as_deref(), Some("97330000000"));
        assert_eq!(account_key("97330000000@s.whatsapp.net").as_deref(), Some("97330000000"));
        assert_eq!(account_key("+973 3000 0000").as_deref(), Some("97330000000"));
        assert!(account_key("12@lid").is_none());
        assert!(account_key("").is_none());
    }

    #[test]
    fn truncation_is_deterministic_and_marked() {
        assert_eq!(truncate_chars("  Milk  ", 10), "Milk");
        let long = "حليب ".repeat(60);
        let t = truncate_chars(&long, NAME_MAX_CHARS);
        assert!(t.ends_with('…') && t.chars().count() <= NAME_MAX_CHARS);
        assert_eq!(t, truncate_chars(&long, NAME_MAX_CHARS));
        // Emoji and combining marks are never split into invalid UTF-8.
        let e = "🥛".repeat(2000);
        let t = truncate_chars(&e, DESCRIPTION_MAX_CHARS);
        assert!(t.chars().count() <= DESCRIPTION_MAX_CHARS && std::str::from_utf8(t.as_bytes()).is_ok());
    }

    #[test]
    fn text_is_cleaned_without_inventing_anything() {
        assert_eq!(clean_line("  Laban\tUp\n 1L\u{0}  "), "Laban Up 1L");
        assert_eq!(clean_line("حليب  المراعي\r\nFresh 🥛"), "حليب المراعي Fresh 🥛");
        assert_eq!(clean_line("\u{7}\u{1b}"), "");
        assert_eq!(clean_block("Line one\r\n\r\n\r\n  Line   two\u{0}\n\n"), "Line one\n\nLine two");
        assert_eq!(clean_block("\n\n"), "");
    }

    #[test]
    fn retailer_ids_are_the_product_code_and_never_collide() {
        assert_eq!(retailer_id_for("100001"), "100001");
        let a = "A".repeat(120) + "1";
        let b = "A".repeat(120) + "2";
        let (ra, rb) = (retailer_id_for(&a), retailer_id_for(&b));
        assert_ne!(ra, rb);
        assert!(ra.chars().count() <= RETAILER_ID_MAX_CHARS && ra.is_ascii());
        assert_eq!(ra, retailer_id_for(&a), "deterministic");
    }

    #[test]
    fn fingerprint_changes_only_with_the_published_fields() {
        let base = CatalogItem {
            name: "Almarai Milk 1L".into(),
            description: None,
            price_1000: Some(850),
            currency: "BHD".into(),
            retailer_id: "100001".into(),
            image_hash: None,
            hidden: false,
        };
        let fp = base.fingerprint();
        assert_eq!(fp, base.clone().fingerprint());
        for changed in [
            CatalogItem { name: "Almarai Milk 2L".into(), ..base.clone() },
            CatalogItem { price_1000: Some(900), ..base.clone() },
            CatalogItem { description: Some("Fresh".into()), ..base.clone() },
            CatalogItem { image_hash: Some("ab".repeat(32)), ..base.clone() },
            CatalogItem { hidden: true, ..base.clone() },
            CatalogItem { retailer_id: "100002".into(), ..base.clone() },
            CatalogItem { currency: "SAR".into(), ..base.clone() },
        ] {
            assert_ne!(changed.fingerprint(), fp);
        }
    }

    #[test]
    fn friendly_errors_explain_what_happens_next() {
        assert!(friendly_error("WhatsApp is not connected.").contains("retried automatically"));
        assert!(friendly_error("WhatsApp refused the catalogue change (400 bad product)").contains("Retry"));
        assert!(friendly_error("The product was deleted on WhatsApp.").contains("publishes it again"));
        assert_eq!(friendly_error("something else"), "something else");
    }
}
