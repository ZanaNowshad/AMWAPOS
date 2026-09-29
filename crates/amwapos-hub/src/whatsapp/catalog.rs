//! WhatsApp Business catalogue worker: publishes the POS catalogue through
//! the same linked WhatsApp session the service already runs (no second
//! connection, no Meta API). It runs next to the message I/O worker, only
//! where the session lives (hub / standalone), one product at a time.
//!
//! * Capability comes from the live connection (`catalog_capability`):
//!   personal account, Business account without a readable catalogue,
//!   supported, or unavailable right now. Nothing is written unless it is
//!   `supported` and an administrator started publishing for this account.
//! * Work is claimed from the core (`wa_catalog_claim`), which decides the
//!   action from current POS data and scopes everything to the linked
//!   account, so a new account never reuses another account's remote ids.
//! * Before creating a product without a mapping, the account's catalogue is
//!   consulted (read once, then kept in step with this worker's own writes;
//!   re-read after 10 minutes, a failed or timed-out create, a "not found",
//!   a relink, or a full sync's remote check) and a remote product carrying
//!   exactly this retailer id (the POS product code) that no other POS
//!   product owns is adopted instead: a crash between "created" and
//!   "recorded" never duplicates. If it cannot be read, nothing is created.
//! * An administrator's full sync is a run: progress is tracked, and once
//!   every product in it was processed the remote catalogue is read once to
//!   find mapped products deleted on WhatsApp, which are re-checked (and
//!   published again only as part of that explicit sync).
//! * Every job is checked against the account linked right now before any
//!   remote call; edits made while a write was in flight are re-queued.
//! * POS writes never wait for this; a disconnect only leaves work queued.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use amwapos_core::wa_catalog::{account_key, CatalogAction, CatalogJob, CatalogOutcome};
use amwapos_core::{AppError, AppResult};
use serde::Serialize;

use super::adapter::*;
use super::service::WhatsAppService;

const TICK: Duration = Duration::from_secs(5);
/// Re-check the capability this often while connected.
const RECHECK: Duration = Duration::from_secs(30 * 60);
/// Re-check an "unavailable" capability this often (not on every tick).
const RECHECK_UNAVAILABLE: Duration = Duration::from_secs(60);
const DETECT_TIMEOUT: Duration = Duration::from_secs(40);
const OP_TIMEOUT: Duration = Duration::from_secs(60);
/// Products per claim; one remote change at a time, spaced out by the
/// service's catalogue pace (1.2 s by default).
const BATCH: usize = 5;
/// Catalogue pages read for reconciliation at most (50 per page).
const MAX_LIST_PAGES: usize = 40;
/// Full POS ↔ WhatsApp comparison at least this often (auto-sync).
const SCAN_EVERY: Duration = Duration::from_secs(60);

/// What the admin screen shows about the catalogue connection.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct CatalogInfo {
    /// disconnected | checking | personal | business_no_catalog | supported |
    /// unavailable | unsupported | terminal
    pub capability: String,
    pub detail: Option<String>,
    /// The linked account's number (digits) the capability refers to.
    pub account: Option<String>,
    pub checked_at: Option<String>,
    /// Collections (POS categories) can be written through this client.
    pub collections: bool,
    /// Products written in the current / last pass.
    pub last_pass_at: Option<String>,
    pub last_pass_done: u32,
    pub last_error: Option<String>,
}

fn now() -> String {
    amwapos_core::time::now_str()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::internal(format!("worker failed: {e}")))?
}

fn capability_name(c: &CatalogCapability) -> (&'static str, Option<String>) {
    match c {
        CatalogCapability::Personal => ("personal", None),
        CatalogCapability::BusinessNoCatalog(m) => ("business_no_catalog", Some(m.clone())),
        CatalogCapability::Supported => ("supported", None),
        CatalogCapability::Unavailable(m) => ("unavailable", Some(m.clone())),
        CatalogCapability::Unsupported => ("unsupported", None),
    }
}

fn outcome_of(e: AdapterError) -> CatalogOutcome {
    if e.permanent {
        CatalogOutcome::Failed { error: e.message }
    } else {
        CatalogOutcome::Transient { error: e.message, retry_after_s: e.retry_after_s }
    }
}

/// The linked account's remote products by retailer id, for adoption. Read
/// once and then kept up to date with this worker's own creates/deletes (the
/// only writer); re-read after `INDEX_TTL`, on another account, or after a
/// failed read. Without it, a first sync of N products would read the whole
/// remote catalogue N/BATCH times.
struct RemoteIndex {
    account: String,
    at: Instant,
    by_retailer: HashMap<String, Vec<String>>,
}

const INDEX_TTL: Duration = Duration::from_secs(10 * 60);

/// Read the whole catalogue: `(ids by retailer id, all ids)`. `None`: it could
/// not be read completely (error, timeout, or more pages than allowed).
async fn read_catalogue(session: &Arc<dyn AdapterSession>) -> Option<(HashMap<String, Vec<String>>, HashSet<String>)> {
    let mut by_retailer: HashMap<String, Vec<String>> = HashMap::new();
    let mut all = HashSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_LIST_PAGES {
        let (items, next) = tokio::time::timeout(OP_TIMEOUT, session.catalog_list(cursor.as_deref())).await.ok()?.ok()?;
        for p in items {
            all.insert(p.id.clone());
            if let Some(r) = p.retailer_id {
                by_retailer.entry(r).or_default().push(p.id);
            }
        }
        match next {
            // A server repeating its cursor is treated as the end.
            Some(c) if cursor.as_deref() != Some(c.as_str()) => cursor = Some(c),
            Some(_) => return Some((by_retailer, all)),
            None => return Some((by_retailer, all)),
        }
    }
    None
}

async fn process(
    svc: &Arc<WhatsAppService>,
    session: &Arc<dyn AdapterSession>,
    job: CatalogJob,
    index: &mut Option<RemoteIndex>,
) -> CatalogOutcome {
    let core = svc.core().clone();
    match job.action {
        CatalogAction::Delete => {
            let Some(id) = job.remote_id.clone() else { return CatalogOutcome::Deleted };
            let r = match tokio::time::timeout(OP_TIMEOUT, session.catalog_delete(std::slice::from_ref(&id))).await {
                Ok(Ok(_)) => CatalogOutcome::Deleted,
                Ok(Err(e)) if e.not_found => CatalogOutcome::Deleted,
                Ok(Err(e)) => outcome_of(e),
                Err(_) => CatalogOutcome::Transient { error: "WhatsApp did not answer in time.".into(), retry_after_s: None },
            };
            if matches!(r, CatalogOutcome::Deleted) {
                if let Some(ix) = index.as_mut() {
                    ix.by_retailer.values_mut().for_each(|v| v.retain(|x| x != &id));
                }
            }
            r
        }
        CatalogAction::Upsert | CatalogAction::Hide => {
            let Some(mut item) = job.item.clone() else { return CatalogOutcome::Failed { error: "Nothing to publish.".into() } };
            // The POS-managed picture: re-used while unchanged, else uploaded.
            // A picture WhatsApp refuses is left out (the product is still
            // published); a temporary upload problem retries the product.
            let mut image_rejected = None;
            let image_url = match (&job.image_url, job.image_jpeg) {
                (Some(u), _) => Some(u.clone()),
                (None, Some(bytes)) => match tokio::time::timeout(OP_TIMEOUT, session.catalog_upload_image(bytes)).await {
                    Ok(Ok(u)) => {
                        if let Some(h) = item.image_hash.clone() {
                            let (c, a, p, url) = (core.clone(), job.account.clone(), job.product_id.clone(), u.clone());
                            let _ = blocking(move || c.wa_catalog_note_upload(&a, &p, &h, &url)).await;
                        }
                        Some(u)
                    }
                    Ok(Err(e)) if e.permanent => {
                        tracing::warn!(product_id = %job.product_id, error = %e.message, "WhatsApp refused the product picture; published without it");
                        image_rejected = item.image_hash.take();
                        None
                    }
                    Ok(Err(e)) => return outcome_of(e),
                    Err(_) => return CatalogOutcome::Transient { error: "The picture upload timed out.".into(), retry_after_s: None },
                },
                (None, None) => None,
            };
            let product = CatalogProduct {
                name: item.name.clone(),
                description: item.description.clone(),
                price_1000: item.price_1000,
                currency: item.currency.clone(),
                retailer_id: item.retailer_id.clone(),
                image_url: image_url.clone(),
                hidden: item.hidden,
            };
            let hide = job.action == CatalogAction::Hide;
            let (target, adopted) = match &job.remote_id {
                Some(id) => (Some(id.clone()), false),
                None => {
                    if index.as_ref().is_none_or(|ix| ix.account != job.account || ix.at.elapsed() > INDEX_TTL) {
                        *index = read_catalogue(session).await.map(|(by_retailer, _)| RemoteIndex {
                            account: job.account.clone(),
                            at: Instant::now(),
                            by_retailer,
                        });
                    }
                    let Some(ix) = index.as_ref() else {
                        // Without the catalogue we cannot rule out a duplicate: retry later.
                        return CatalogOutcome::Transient {
                            error: "The WhatsApp catalogue could not be read to check for an existing copy.".into(),
                            retry_after_s: None,
                        };
                    };
                    // A remote product carrying exactly this product code that no
                    // other POS product owns is ours (an interrupted earlier
                    // create): adopt it. With several such copies, the oldest
                    // (smallest id) is adopted and the others are left alone:
                    // never create yet another one.
                    let mut adopt = None;
                    if !item.retailer_id.is_empty() {
                        let mut ids = ix.by_retailer.get(&item.retailer_id).cloned().unwrap_or_default();
                        ids.sort_by(|a, b| (a.len(), a).cmp(&(b.len(), b)));
                        for rid in ids {
                            let (c, acc, r2) = (core.clone(), job.account.clone(), rid.clone());
                            if blocking(move || c.wa_catalog_remote_owner(&acc, &r2)).await.ok().flatten().is_none() {
                                adopt = Some(rid);
                                break;
                            }
                        }
                    }
                    let adopted = adopt.is_some();
                    (adopt, adopted)
                }
            };
            let res = match &target {
                Some(id) => tokio::time::timeout(OP_TIMEOUT, session.catalog_update(id, &product)).await,
                None => tokio::time::timeout(OP_TIMEOUT, session.catalog_create(&product)).await,
            };
            match res {
                Ok(Ok(remote)) => {
                    let what = match (&target, hide) {
                        (_, true) => "hidden",
                        (Some(_), _) if adopted => "adopted and updated",
                        (Some(_), _) => "updated",
                        (None, _) => "created",
                    };
                    tracing::info!(product_id = %job.product_id, remote_id = %remote.id, what, "WhatsApp catalogue product");
                    if target.is_none() {
                        if let Some(ix) = index.as_mut() {
                            ix.by_retailer.entry(item.retailer_id.clone()).or_default().push(remote.id.clone());
                        }
                    }
                    CatalogOutcome::Published { remote_id: remote.id, item, image_url, adopted, image_rejected }
                }
                Ok(Err(e)) if e.not_found => {
                    // Deleted on WhatsApp by someone else: the cached index is
                    // stale (it may still list this id and would adopt it again).
                    *index = None;
                    if hide {
                        CatalogOutcome::Deleted
                    } else {
                        CatalogOutcome::RemoteMissing { error: "The product was deleted on WhatsApp.".into() }
                    }
                }
                Ok(Err(e)) => {
                    if target.is_none() {
                        // A create may have happened despite the error: read the
                        // catalogue again before the next create.
                        *index = None;
                    }
                    outcome_of(e)
                }
                Err(_) => {
                    *index = None;
                    CatalogOutcome::Transient { error: "WhatsApp did not answer in time.".into(), retry_after_s: None }
                }
            }
        }
    }
}

/// An administrator's full sync asks for one comparison with the remote
/// catalogue: mapped products that WhatsApp does not list are re-checked.
/// The fresh read also replaces the cached remote index.
async fn verify_run(svc: &Arc<WhatsAppService>, session: &Arc<dyn AdapterSession>, account: &str, index: &mut Option<RemoteIndex>) {
    let core = svc.core().clone();
    let (c, a) = (core.clone(), account.to_string());
    let Ok(Some(run)) = blocking(move || c.wa_catalog_verify_due(&a)).await else { return };
    let read = read_catalogue(session).await;
    *index = read.as_ref().map(|(by_retailer, _)| RemoteIndex {
        account: account.to_string(),
        at: Instant::now(),
        by_retailer: by_retailer.clone(),
    });
    let listed = read.map(|(_, all)| all);
    let (c, a) = (core, account.to_string());
    if let Err(e) = blocking(move || c.wa_catalog_verify(&a, &run, listed.as_ref())).await {
        tracing::warn!(error = %e.message, "WhatsApp catalogue: remote check not recorded");
    }
}

/// Runs for the life of the WhatsApp service.
pub(super) async fn catalog_worker(svc: Arc<WhatsAppService>) {
    // Nothing is in flight yet: rows a previous worker had claimed are free.
    let c = svc.core().clone();
    let _ = blocking(move || c.wa_catalog_release_claims()).await;
    let mut rx = svc.session_watch();
    // (account, checked at, result was "unavailable")
    let mut detected: Option<(String, Instant, bool)> = None;
    let mut last_session: Option<usize> = None;
    let mut last_scan: Option<Instant> = None;
    let mut index: Option<RemoteIndex> = None;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            _ = svc.catalog_poke.notified() => {}
            _ = rx.changed() => {}
        }
        let session = rx.borrow().clone();
        let Some(session) = session.filter(|s| s.connected()) else {
            svc.set_catalog(|i| {
                i.capability = "disconnected".into();
                i.detail = None;
            });
            detected = None;
            continue;
        };
        // A new client (restart, relink) is re-checked.
        let sid = Arc::as_ptr(&session) as *const () as usize;
        if last_session != Some(sid) {
            last_session = Some(sid);
            detected = None;
            index = None;
        }
        let core = svc.core().clone();
        if core.device().is_some_and(|d| d.mode == "terminal") {
            svc.set_catalog(|i| i.capability = "terminal".into());
            continue;
        }
        let Some(account) = svc.status().account.as_deref().and_then(account_key) else {
            svc.set_catalog(|i| i.capability = "checking".into());
            continue;
        };
        let force = svc.take_catalog_recheck();
        let stale = force
            || detected.as_ref().is_none_or(|(a, t, unavailable)| {
                // "Unavailable" is re-checked after a minute, the others after RECHECK.
                a != &account || t.elapsed() > if *unavailable { RECHECK_UNAVAILABLE } else { RECHECK }
            });
        if stale {
            svc.set_catalog(|i| {
                i.capability = "checking".into();
                i.account = Some(account.clone());
            });
            let cap = match tokio::time::timeout(DETECT_TIMEOUT, session.catalog_capability()).await {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => CatalogCapability::Unavailable(e.message),
                Err(_) => CatalogCapability::Unavailable("WhatsApp did not answer in time.".into()),
            };
            let (name, detail) = capability_name(&cap);
            tracing::info!(capability = name, "WhatsApp catalogue capability detected");
            let collections = session.catalog_collections_writable();
            svc.set_catalog(|i| {
                i.capability = name.into();
                i.detail = detail.clone();
                i.account = Some(account.clone());
                i.checked_at = Some(now());
                i.collections = collections;
            });
            detected = Some((account.clone(), Instant::now(), name == "unavailable"));
        }
        if svc.catalog().capability != "supported" {
            continue;
        }
        let (c, a) = (core.clone(), account.clone());
        let published = blocking(move || c.wa_catalog_published(&a)).await.unwrap_or(false);
        if !published {
            continue;
        }
        // Compare POS and WhatsApp after a catalogue change, and at least
        // once a minute (changes made outside a command, e.g. a picture the
        // image worker found). Unchanged products produce no remote write.
        let changed = svc.take_catalog_dirty();
        let due_scan = changed || last_scan.is_none_or(|t| t.elapsed() > SCAN_EVERY);
        let (c, a) = (core.clone(), account.clone());
        if due_scan && blocking(move || c.wa_catalog_auto(&a)).await.unwrap_or(false) {
            last_scan = Some(Instant::now());
            let (c, a) = (core.clone(), account.clone());
            if let Err(e) = blocking(move || c.wa_catalog_scan(&a)).await {
                tracing::warn!(error = %e.message, "WhatsApp catalogue scan");
            }
        }
        verify_run(&svc, &session, &account, &mut index).await;
        let (c, a) = (core.clone(), account.clone());
        let jobs = match blocking(move || c.wa_catalog_claim(&a, BATCH)).await {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e.message, "WhatsApp catalogue claim");
                continue;
            }
        };
        if jobs.is_empty() {
            continue;
        }
        let mut done = 0u32;
        let mut failed = false;
        for job in jobs {
            // Stop the pass when the session dropped or another number is
            // linked now: an outcome is never attached to the wrong account.
            let linked = svc.status().account.as_deref().and_then(account_key);
            if !session.connected() || linked.as_deref() != Some(job.account.as_str()) {
                let why = if session.connected() { "The linked WhatsApp number changed." } else { "WhatsApp disconnected." };
                let (c, a, p) = (core.clone(), job.account.clone(), job.product_id.clone());
                let _ =
                    blocking(move || c.wa_catalog_complete(&a, &p, CatalogOutcome::Transient { error: why.into(), retry_after_s: None }))
                        .await;
                continue;
            }
            let pid = job.product_id.clone();
            let acc = job.account.clone();
            let outcome = process(&svc, &session, job, &mut index).await;
            if let CatalogOutcome::Failed { error } | CatalogOutcome::Transient { error, .. } = &outcome {
                tracing::warn!(product_id = %pid, error = %error, "WhatsApp catalogue product not synchronised");
                let e = error.clone();
                failed = true;
                svc.set_catalog(|i| i.last_error = Some(e));
            }
            let c = core.clone();
            match blocking(move || c.wa_catalog_complete(&acc, &pid, outcome)).await {
                Ok(state) => {
                    if state != "retry" && state != "failed" {
                        done += 1;
                    }
                }
                Err(e) => tracing::warn!(error = %e.message, "WhatsApp catalogue: outcome not recorded"),
            }
            tokio::time::sleep(svc.catalog_pace()).await;
        }
        svc.set_catalog(|i| {
            i.last_pass_at = Some(now());
            i.last_pass_done = done;
            if !failed {
                i.last_error = None;
            }
        });
        // More may be due: go again without waiting for the tick.
        svc.catalog_poke.notify_one();
    }
}
