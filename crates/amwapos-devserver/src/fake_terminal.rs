//! `--fake-terminal`: in-process terminals for end-to-end tests of the
//! operational-control screens (Wave 7). Each is a real `AppCore` in its own
//! data directory, paired with the dev hub through the real pairing code, and
//! exchanging changes, heartbeats and signed requests through the same core
//! functions the hub API calls. Nothing is written into the hub's database
//! by hand: refused records, health and credential state come from the real
//! paths. Development only; never packaged.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::FinalizeRequest;
use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_core::sync::{self, Change, NonceCache, PairRequest, PullRequest, PushRequest};
use amwapos_core::{AppError, AppResult};
use serde_json::{json, Value};

pub struct FakeTerminals {
    dir: PathBuf,
    terms: Mutex<HashMap<String, Arc<AppCore>>>,
    nonces: NonceCache,
}

impl FakeTerminals {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, terms: Mutex::new(HashMap::new()), nonces: NonceCache::default() }
    }

    fn term(&self, code: &str) -> AppResult<Arc<AppCore>> {
        self.terms.lock().unwrap().get(code).cloned().ok_or_else(|| AppError::not_found("Fake terminal"))
    }

    fn owner(term: &AppCore, hub: &AppCore, hub_token: &str) -> AppResult<String> {
        let me = hub.session_info(hub_token)?;
        Ok(term.login(&me.user_id, "4826")?.token)
    }

    /// A signed request from the terminal, checked by the hub exactly as the
    /// hub API checks it (the real signature, nonce and credential rules).
    fn authenticate(&self, hub: &AppCore, term: &AppCore) -> AppResult<()> {
        let dev = term.device().ok_or_else(|| AppError::internal("no device"))?.device_id;
        let key = term.terminal_device_key()?;
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
        let nonce = amwapos_core::ids::new_id();
        let body = br#"{}"#;
        let sig = sync::hmac_hex(&key, sync::signing_string("POST", "/heartbeat", ts, &nonce, body).as_bytes());
        hub.hub_authenticate_key(&self.nonces, &dev, ts, &nonce, &sig, "POST", "/heartbeat", body).map(|_| ())
    }

    fn pull_all(hub: &AppCore, term: &AppCore) -> AppResult<()> {
        let dev = term.device().ok_or_else(|| AppError::internal("no device"))?.device_id;
        loop {
            let since = term.terminal_sync_settings()?.pull_cursor;
            let resp = hub.hub_pull(&dev, PullRequest { device_id: dev.clone(), since_seq: since, limit: Some(500) })?;
            term.terminal_apply_pull(&resp)?;
            if !resp.has_more {
                return Ok(());
            }
        }
    }

    fn push(hub: &AppCore, term: &AppCore, keep: impl Fn(&Change) -> bool) -> AppResult<Value> {
        let dev = term.device().ok_or_else(|| AppError::internal("no device"))?.device_id;
        let (changes, scanned) = term.terminal_collect_push(1000)?;
        let sent: Vec<Change> = changes.into_iter().filter(|c| keep(c)).collect();
        let resp = hub.hub_apply_push(&dev, PushRequest { device_id: dev.clone(), changes: sent.clone() })?;
        term.terminal_record_push(scanned, &resp, &sent)?;
        Ok(json!({ "accepted": resp.accepted, "refused": resp.rejected.len() }))
    }

    pub fn act(&self, hub: &AppCore, req: &Value) -> AppResult<Value> {
        let token = req["token"].as_str().unwrap_or_default();
        let code = req["code"].as_str().unwrap_or("T90").to_string();
        match req["action"].as_str().unwrap_or_default() {
            // Pair a new terminal with the dev hub and load the catalogue.
            "pair" => {
                let name = req["name"].as_str().unwrap_or("Till 90").to_string();
                let pc = hub.sync_issue_pairing_code(token, Some(name.clone()), None)?;
                let dir = self.dir.join(&code);
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir)?;
                let term = AppCore::open(&dir, Arc::new(MemorySecretStore::default()))?;
                let resp = hub.hub_pair(PairRequest {
                    code: pc["code"].as_str().unwrap_or_default().into(),
                    device_name: name,
                    device_code: code.clone(),
                    app_version: amwapos_core::audit::APP_VERSION.into(),
                    schema_version: amwapos_core::db::latest_schema_version(),
                    os_info: None,
                })?;
                term.terminal_bootstrap("http://127.0.0.1:47800", resp)?;
                Self::pull_all(hub, &term)?;
                let dev = term.device().map(|d| d.device_id).unwrap_or_default();
                self.terms.lock().unwrap().insert(code, Arc::new(term));
                Ok(json!({ "device_id": dev }))
            }
            // Sell one item and send everything except the sale itself: its
            // lines and payment are refused until the sale arrives.
            "sell_send_parts" => {
                let term = self.term(&code)?;
                let t = Self::owner(&term, hub, token)?;
                if term.shift_current(&t).ok().flatten().is_none() {
                    term.shift_open(&t, 0, &amwapos_core::ids::new_id())?;
                }
                let cart = term.pos_scan(&t, req["barcode"].as_str().unwrap_or_default(), Some(1000))?.cart;
                let total = cart.totals.total_minor;
                term.pos_finalize(
                    &t,
                    FinalizeRequest {
                        cart_id: cart.cart_id.unwrap_or_default(),
                        operation_id: amwapos_core::ids::new_id(),
                        tenders: vec![TenderInput { method: "cash".into(), amount_minor: total, reference: None }],
                        approval_token: None,
                        expected_total_minor: None,
                        fulfilment: None,
                    },
                )?;
                Self::push(hub, &term, |c| c.table != "sales")
            }
            // The till sends its sales again (as after a fix on the till).
            "send_sales" => {
                let term = self.term(&code)?;
                let own = term.device().map(|d| d.device_id).unwrap_or_default();
                term.db.write(|c| {
                    Ok(c.execute(
                        "INSERT INTO sync_outbox(table_name, row_pk, op, origin)
                         SELECT 'sales', json_object('sale_id', sale_id), 'upsert', NULL FROM sales WHERE device_id=?1",
                        [&own],
                    )?)
                })?;
                Self::push(hub, &term, |c| c.table == "sales")
            }
            // Another till sends this till's sale as its own: refused, and no
            // retry can fix it.
            "forged_sale" => {
                let term = self.term(&code)?;
                let other = self.term(req["as"].as_str().unwrap_or_default())?;
                let dev = other.device().map(|d| d.device_id).unwrap_or_default();
                let own = term.device().map(|d| d.device_id).unwrap_or_default();
                let row = term.db.read(|c| {
                    Ok(c.query_row("SELECT sale_id FROM sales WHERE device_id=?1 ORDER BY created_at DESC LIMIT 1", [&own], |r| {
                        r.get::<_, String>(0)
                    })?)
                })?;
                term.db.write(|c| {
                    Ok(c.execute("INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('sales', json_object('sale_id', ?1), 'upsert', NULL)", [&row])?)
                })?;
                let (changes, _) = term.terminal_collect_push(1000)?;
                let sales: Vec<Change> = changes.into_iter().filter(|c| c.table == "sales").collect();
                let resp = hub.hub_apply_push(&dev, PushRequest { device_id: dev.clone(), changes: sales })?;
                Ok(json!({ "refused": resp.rejected.len() }))
            }
            // A signed request, then a heartbeat, then what the till does with
            // the reply (a staged credential is installed; a confirmed one
            // drops the previous).
            "heartbeat" => {
                let term = self.term(&code)?;
                self.authenticate(hub, &term)?;
                let dev = term.device().map(|d| d.device_id).unwrap_or_default();
                let resp = hub.hub_heartbeat(&dev, term.terminal_heartbeat()?)?;
                term.terminal_after_heartbeat(&resp)?;
                Ok(json!({ "credential_version": term.terminal_sync_settings()?.credential_version.unwrap_or(1) }))
            }
            _ => Err(AppError::validation("Unknown fake terminal action.")),
        }
    }
}
