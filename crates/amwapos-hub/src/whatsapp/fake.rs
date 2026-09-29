//! In-memory adapter for tests (and for running with the flag on but no
//! network). It behaves like WhatsApp where AMWAPOS depends on it: QR pairing,
//! a persistent session file, de-duplication by message id, inbound batches
//! that must be committed before they are acknowledged, and media downloads.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amwapos_core::messaging::Inbound;
use async_trait::async_trait;
use tokio::sync::{oneshot, Notify};

use super::adapter::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeSent {
    pub wa_id: String,
    pub to: String,
    pub text: String,
    pub document: Option<(String, usize)>,
}

#[derive(Default)]
pub struct FakeState {
    /// Messages WhatsApp accepted, in order, de-duplicated by `wa_id`.
    pub sent: Mutex<Vec<FakeSent>>,
    /// Fail the next N sends with a temporary error.
    pub fail_sends: AtomicU32,
    pub starts: AtomicU32,
    pub read_marks: Mutex<Vec<(String, Vec<String>)>>,
    pub media: Mutex<HashMap<String, Vec<u8>>>,
    /// Inbound batches the sink refused (would be redelivered by WhatsApp).
    pub refused: AtomicU32,
    connected: AtomicBool,
    sink: Mutex<Option<Arc<dyn AdapterSink>>>,
    stop: Notify,
    crash: Mutex<Option<oneshot::Sender<()>>>,
    logged_out: AtomicBool,
    /// Contacts "saved on the phone": sent by `resync_contacts`.
    pub phone_contacts: Mutex<Vec<WaContact>>,
    /// The linked number (changes when a different phone links).
    pub account: Mutex<String>,
    pub catalog: FakeCatalog,
}

/// A WhatsApp Business catalogue per linked account, as the fake server.
#[derive(Default)]
pub struct FakeCatalog {
    /// Per account: `personal` unless set. Missing = personal.
    pub capability: Mutex<HashMap<String, CatalogCapability>>,
    /// Per account: remote id → product.
    pub products: Mutex<HashMap<String, BTreeMap<String, CatalogProduct>>>,
    pub next_id: AtomicU32,
    pub creates: AtomicU32,
    pub updates: AtomicU32,
    pub deletes: AtomicU32,
    /// Picture bytes uploaded, in order.
    pub uploads: Mutex<Vec<Vec<u8>>>,
    /// Fail the next N writes with a temporary error.
    pub fail_writes: AtomicU32,
    /// Refuse writes for this retailer id (a permanent validation error).
    pub reject_retailer: Mutex<Option<String>>,
    /// Reading the catalogue fails.
    pub fail_list: AtomicBool,
    /// Picture uploads are refused (permanent).
    pub reject_uploads: AtomicBool,
    /// The next N creates are stored remotely but answer with a temporary
    /// error (the reply is lost): the crash-after-create case.
    pub fail_after_create: AtomicU32,
    /// Every catalogue call takes this long (ms): a slow WhatsApp.
    pub delay_ms: std::sync::atomic::AtomicU64,
    /// Calls made, for rate assertions.
    pub list_calls: AtomicU32,
    pub capability_checks: AtomicU32,
    /// Capability checks fail (temporary) while set.
    pub fail_capability: AtomicBool,
}

#[derive(Clone, Default)]
pub struct FakeAdapter {
    pub state: Arc<FakeState>,
}

impl FakeAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The phone scanned the QR code.
    pub fn scan(&self) {
        let sink = self.state.sink.lock().unwrap().clone();
        if let Some(s) = sink {
            self.state.connected.store(true, Ordering::SeqCst);
            s.event(AdapterEvent::Paired { account: self.account() });
            s.event(AdapterEvent::Connected { account: Some(self.account()) });
        }
    }

    /// WhatsApp delivers inbound messages. Returns whether they were committed.
    pub async fn deliver(&self, batch: Vec<Inbound>) -> bool {
        let sink = self.state.sink.lock().unwrap().clone();
        match sink {
            Some(s) => match s.inbound(batch).await {
                Ok(()) => true,
                Err(_) => {
                    self.state.refused.fetch_add(1, Ordering::SeqCst);
                    false
                }
            },
            None => false,
        }
    }

    /// The client task panics (simulates a bug in the WhatsApp library).
    pub fn panic_now(&self) {
        if let Some(tx) = self.state.crash.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }

    /// The phone unlinked this computer.
    pub fn remote_logout(&self) {
        let sink = self.state.sink.lock().unwrap().clone();
        self.state.logged_out.store(true, Ordering::SeqCst);
        self.state.connected.store(false, Ordering::SeqCst);
        if let Some(s) = sink {
            s.event(AdapterEvent::LoggedOut("unlinked from the phone".into()));
        }
        self.state.stop.notify_one();
    }

    pub fn sent(&self) -> Vec<FakeSent> {
        self.state.sent.lock().unwrap().clone()
    }

    fn account(&self) -> String {
        self.state.account()
    }

    /// Link a different phone number on the next start.
    pub fn set_account(&self, jid: &str) {
        *self.state.account.lock().unwrap() = jid.to_string();
    }

    /// Make the linked account a WhatsApp Business account with a catalogue.
    pub fn make_business(&self, jid: &str, cap: CatalogCapability) {
        self.state.catalog.capability.lock().unwrap().insert(jid.to_string(), cap);
    }

    /// The remote catalogue of `jid` (id → product).
    pub fn remote(&self, jid: &str) -> BTreeMap<String, CatalogProduct> {
        self.state.catalog.products.lock().unwrap().get(jid).cloned().unwrap_or_default()
    }

    /// A product someone created in the WhatsApp Business app.
    pub fn add_remote(&self, jid: &str, p: CatalogProduct) -> String {
        let id = format!("{}", 900_000 + self.state.catalog.next_id.fetch_add(1, Ordering::SeqCst));
        self.state.catalog.products.lock().unwrap().entry(jid.to_string()).or_default().insert(id.clone(), p);
        id
    }

    pub fn current_account(&self) -> String {
        self.account()
    }
}

impl FakeState {
    fn account(&self) -> String {
        let a = self.account.lock().unwrap().clone();
        if a.is_empty() {
            "97330000000@s.whatsapp.net".into()
        } else {
            a
        }
    }
}

#[async_trait]
impl WhatsAppAdapter for FakeAdapter {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn start(&self, opts: StartOptions, sink: Arc<dyn AdapterSink>) -> Result<(Arc<dyn AdapterSession>, ExitFuture), AdapterError> {
        self.state.starts.fetch_add(1, Ordering::SeqCst);
        self.state.logged_out.store(false, Ordering::SeqCst);
        // Like the real store: a session file in the WhatsApp folder.
        let paired = std::fs::metadata(&opts.session_db).map(|m| m.len() > 0).unwrap_or(false);
        if let Some(d) = opts.session_db.parent() {
            std::fs::create_dir_all(d).map_err(|e| AdapterError::temporary(e.to_string()))?;
        }
        if !paired {
            std::fs::write(&opts.session_db, b"fake-session").map_err(|e| AdapterError::temporary(e.to_string()))?;
        }
        *self.state.sink.lock().unwrap() = Some(sink.clone());
        if paired {
            self.state.connected.store(true, Ordering::SeqCst);
            sink.event(AdapterEvent::Connected { account: Some(self.state.account()) });
        } else if let Some(p) = opts.pair_phone {
            sink.event(AdapterEvent::PairCode {
                code: format!("FAKE{}", &p[p.len().saturating_sub(4)..]),
                valid_for: Duration::from_secs(180),
            });
        } else {
            sink.event(AdapterEvent::Qr { code: "2@fake-qr-payload".into(), valid_for: Duration::from_secs(60) });
        }
        let (crash_tx, crash_rx) = oneshot::channel::<()>();
        *self.state.crash.lock().unwrap() = Some(crash_tx);
        // The "client task": ends on stop/logout, panics on crash.
        let state = self.state.clone();
        let task = tokio::spawn(async move {
            tokio::select! {
                _ = state.stop.notified() => {}
                r = crash_rx => if r.is_ok() { panic!("fake WhatsApp client crashed") },
            }
        });
        let exit: ExitFuture = Box::pin(async move {
            match task.await {
                Ok(()) => Exit::Ended,
                Err(e) => Exit::Failed(if e.is_panic() { "the WhatsApp client panicked".into() } else { e.to_string() }),
            }
        });
        Ok((Arc::new(FakeSession { state: self.state.clone() }), exit))
    }
}

struct FakeSession {
    state: Arc<FakeState>,
}

impl FakeSession {
    /// Connected, Business with a catalogue, and not asked to fail.
    async fn slow(&self) {
        let ms = self.state.catalog.delay_ms.load(Ordering::SeqCst);
        if ms > 0 {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
    }

    fn catalog_gate(&self, retailer: Option<&str>) -> Result<(), AdapterError> {
        if !self.state.connected.load(Ordering::SeqCst) {
            return Err(AdapterError::temporary("WhatsApp is not connected."));
        }
        let acc = self.state.account();
        if self.state.catalog.capability.lock().unwrap().get(&acc) != Some(&CatalogCapability::Supported) {
            return Err(AdapterError::permanent("not a Business catalogue"));
        }
        if self.state.catalog.fail_writes.load(Ordering::SeqCst) > 0 {
            self.state.catalog.fail_writes.fetch_sub(1, Ordering::SeqCst);
            return Err(AdapterError::temporary("server busy"));
        }
        if retailer.is_some() && self.state.catalog.reject_retailer.lock().unwrap().as_deref() == retailer {
            return Err(AdapterError::permanent("WhatsApp refused the catalogue change (400 bad product)"));
        }
        Ok(())
    }

    fn accept(&self, send_id: &str, to: &str, text: &str, document: Option<(String, usize)>) -> Result<String, AdapterError> {
        if !self.state.connected.load(Ordering::SeqCst) {
            return Err(AdapterError::temporary("not connected"));
        }
        let jid = phone_to_jid(to)?;
        if self.state.fail_sends.load(Ordering::SeqCst) > 0 {
            self.state.fail_sends.fetch_sub(1, Ordering::SeqCst);
            return Err(AdapterError::temporary("network timeout"));
        }
        let wa_id = wa_message_id(send_id);
        let mut sent = self.state.sent.lock().unwrap();
        if !sent.iter().any(|m| m.wa_id == wa_id) {
            sent.push(FakeSent { wa_id: wa_id.clone(), to: jid, text: text.to_string(), document });
        }
        Ok(wa_id)
    }
}

#[async_trait]
impl AdapterSession for FakeSession {
    fn connected(&self) -> bool {
        self.state.connected.load(Ordering::SeqCst)
    }
    async fn resync_contacts(&self) -> Result<(), AdapterError> {
        if !self.state.connected.load(Ordering::SeqCst) {
            return Err(AdapterError::temporary("WhatsApp is not connected."));
        }
        let sink = self.state.sink.lock().unwrap().clone();
        let batch = self.state.phone_contacts.lock().unwrap().clone();
        if let Some(s) = sink {
            s.contacts(batch).await.map_err(AdapterError::temporary)?;
        }
        Ok(())
    }
    fn logged_in(&self) -> bool {
        !self.state.logged_out.load(Ordering::SeqCst)
    }
    async fn send_text(&self, send_id: &str, to_phone: &str, text: &str) -> Result<String, AdapterError> {
        self.accept(send_id, to_phone, text, None)
    }
    async fn send_document(
        &self,
        send_id: &str,
        to_phone: &str,
        bytes: Vec<u8>,
        file_name: &str,
        _mime: &str,
        caption: Option<&str>,
    ) -> Result<String, AdapterError> {
        self.accept(send_id, to_phone, caption.unwrap_or_default(), Some((file_name.to_string(), bytes.len())))
    }
    async fn catalog_capability(&self) -> Result<CatalogCapability, AdapterError> {
        self.state.catalog.capability_checks.fetch_add(1, Ordering::SeqCst);
        if self.state.catalog.fail_capability.load(Ordering::SeqCst) {
            return Err(AdapterError::temporary("WhatsApp did not answer in time."));
        }
        if !self.state.connected.load(Ordering::SeqCst) {
            return Err(AdapterError::temporary("WhatsApp is not connected."));
        }
        let acc = self.state.account();
        Ok(self.state.catalog.capability.lock().unwrap().get(&acc).cloned().unwrap_or(CatalogCapability::Personal))
    }
    async fn catalog_upload_image(&self, jpeg: Vec<u8>) -> Result<String, AdapterError> {
        self.slow().await;
        self.catalog_gate(None)?;
        if self.state.catalog.reject_uploads.load(Ordering::SeqCst) {
            return Err(AdapterError::permanent("The product picture is not a stored JPEG."));
        }
        let mut u = self.state.catalog.uploads.lock().unwrap();
        u.push(jpeg);
        Ok(format!("https://mmg.whatsapp.net/product/image/fake-{}", u.len()))
    }
    async fn catalog_create(&self, product: &CatalogProduct) -> Result<RemoteProduct, AdapterError> {
        self.slow().await;
        self.catalog_gate(Some(&product.retailer_id))?;
        let id = format!("{}", 700_000 + self.state.catalog.next_id.fetch_add(1, Ordering::SeqCst));
        self.state.catalog.products.lock().unwrap().entry(self.state.account()).or_default().insert(id.clone(), product.clone());
        self.state.catalog.creates.fetch_add(1, Ordering::SeqCst);
        let lost = self.state.catalog.fail_after_create.load(Ordering::SeqCst);
        if lost > 0 {
            self.state.catalog.fail_after_create.store(lost - 1, Ordering::SeqCst);
            return Err(AdapterError::temporary("WhatsApp did not answer in time."));
        }
        Ok(RemoteProduct { id, retailer_id: Some(product.retailer_id.clone()), name: Some(product.name.clone()), hidden: product.hidden })
    }
    async fn catalog_update(&self, remote_id: &str, product: &CatalogProduct) -> Result<RemoteProduct, AdapterError> {
        self.slow().await;
        self.catalog_gate(Some(&product.retailer_id))?;
        let mut all = self.state.catalog.products.lock().unwrap();
        let cat = all.entry(self.state.account()).or_default();
        let Some(p) = cat.get_mut(remote_id) else { return Err(AdapterError::not_found("no such product")) };
        *p = product.clone();
        self.state.catalog.updates.fetch_add(1, Ordering::SeqCst);
        Ok(RemoteProduct {
            id: remote_id.into(),
            retailer_id: Some(product.retailer_id.clone()),
            name: Some(product.name.clone()),
            hidden: product.hidden,
        })
    }
    async fn catalog_delete(&self, remote_ids: &[String]) -> Result<u32, AdapterError> {
        self.slow().await;
        self.catalog_gate(None)?;
        let mut all = self.state.catalog.products.lock().unwrap();
        let cat = all.entry(self.state.account()).or_default();
        let n = remote_ids.iter().filter(|id| cat.remove(*id).is_some()).count() as u32;
        self.state.catalog.deletes.fetch_add(n, Ordering::SeqCst);
        Ok(n)
    }
    async fn catalog_list(&self, cursor: Option<&str>) -> Result<(Vec<RemoteProduct>, Option<String>), AdapterError> {
        self.slow().await;
        self.state.catalog.list_calls.fetch_add(1, Ordering::SeqCst);
        if !self.state.connected.load(Ordering::SeqCst) || self.state.catalog.fail_list.load(Ordering::SeqCst) {
            return Err(AdapterError::temporary("catalogue read failed"));
        }
        let all = self.state.catalog.products.lock().unwrap();
        let items: Vec<RemoteProduct> = all
            .get(&self.state.account())
            .map(|c| {
                c.iter()
                    .map(|(id, p)| RemoteProduct {
                        id: id.clone(),
                        retailer_id: Some(p.retailer_id.clone()),
                        name: Some(p.name.clone()),
                        hidden: p.hidden,
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Two pages, to exercise paging.
        let half = items.len() / 2;
        Ok(match cursor {
            None if half > 0 => (items[..half].to_vec(), Some("page-2".into())),
            None => (items, None),
            Some(_) => (items[half..].to_vec(), None),
        })
    }
    async fn mark_read(&self, chat: &str, ids: &[String]) -> Result<(), AdapterError> {
        self.state.read_marks.lock().unwrap().push((chat.to_string(), ids.to_vec()));
        Ok(())
    }
    async fn download(&self, media_ref: &str) -> Result<Vec<u8>, AdapterError> {
        self.state.media.lock().unwrap().get(media_ref).cloned().ok_or_else(|| AdapterError::permanent("media expired"))
    }
    async fn stop(&self) {
        self.state.connected.store(false, Ordering::SeqCst);
        self.state.stop.notify_one();
    }
    async fn logout(&self) {
        self.state.logged_out.store(true, Ordering::SeqCst);
        self.state.connected.store(false, Ordering::SeqCst);
        let sink = self.state.sink.lock().unwrap().clone();
        if let Some(s) = sink {
            s.event(AdapterEvent::LoggedOut("logged out from AMWAPOS".into()));
        }
        self.state.stop.notify_one();
    }
}
