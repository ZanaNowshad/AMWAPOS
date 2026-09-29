//! WhatsApp Business catalogue publishing through the existing WhatsApp
//! service, end to end with the fake adapter (the same `AdapterSession`
//! interface the real client implements): runtime command → core queue →
//! catalogue worker → adapter → simulated WhatsApp catalogue → mappings →
//! later changes update the same remote products.

use std::net::Ipv4Addr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::auth::ROLE_CASHIER;
use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::whatsapp::{CatalogCapability, CatalogProduct, FakeAdapter};
use amwapos_hub::Runtime;
use base64::Engine;
use serde_json::{json, Value};

const ACC: &str = "97330000000@s.whatsapp.net";
const ACC2: &str = "97339999999@s.whatsapp.net";

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Value {
    match rt.dispatch(cmd, token.map(|t| t.to_string()), args).await {
        Ok(v) => v,
        Err(e) => panic!("{cmd} failed: {} ({:?})", e.message, e.code),
    }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..600 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn count(core: &AppCore, sql: &str) -> i64 {
    core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

struct Env {
    _dir: tempfile::TempDir,
    core: Arc<AppCore>,
    rt: Arc<Runtime>,
    fake: FakeAdapter,
    t: String,
    tax: String,
}

/// Set up, turn WhatsApp on and link the (fake) phone.
async fn env(business: Option<CatalogCapability>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let core = Arc::new(AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
    let rt = Runtime::with_bind(core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(500));
    let fake = FakeAdapter::new();
    if let Some(cap) = business {
        fake.make_business(ACC, cap);
    }
    rt.set_whatsapp_adapter(Arc::new(fake.clone()));
    call(
        &rt,
        "setup.initialize",
        None,
        json!({ "business_name": "Test Mart", "branch_name": "Main", "vat_rate_bp": 1000, "owner_name": "Owner",
                "owner_pin": "4826", "device_name": "Till", "device_code": "T01" }),
    )
    .await;
    let owner = call(&rt, "auth.users", None, json!({})).await[0]["user_id"].as_str().unwrap().to_string();
    let t = call(&rt, "auth.login", None, json!({ "user_id": owner, "pin": "4826" })).await["token"].as_str().unwrap().to_string();
    call(&rt, "settings.save", Some(&t), json!({ "key": "features", "value": { "whatsapp.enabled": true } })).await;
    for _ in 0..200 {
        if rt.whatsapp.status().process != "disabled" {
            break;
        }
        rt.whatsapp.ensure();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    call(&rt, "whatsapp.start", Some(&t), json!({})).await;
    fake.scan();
    let tax = call(&rt, "tax.list", Some(&t), json!({})).await[0]["tax_rule_id"].as_str().unwrap().to_string();
    Env { _dir: dir, core, rt, fake, t, tax }
}

async fn capability(e: &Env, want: &str) -> Value {
    let mut st = json!(null);
    for _ in 0..600 {
        e.rt.whatsapp.catalog_poke.notify_one();
        st = call(&e.rt, "whatsapp.catalog_status", Some(&e.t), json!({})).await;
        let linked = e.rt.whatsapp.status().account.as_deref().and_then(amwapos_core::wa_catalog::account_key);
        let same = want == "disconnected" || st["capability"]["account"].as_str() == linked.as_deref();
        if st["capability"]["capability"] == want && same {
            return st;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("capability never became {want}: {st}");
}

fn png(rgb: [u8; 3]) -> String {
    let mut img = image::RgbaImage::from_pixel(300, 300, image::Rgba([255, 255, 255, 255]));
    for y in 80..220 {
        for x in 80..220 {
            img.put_pixel(x, y, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
        }
    }
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    base64::engine::general_purpose::STANDARD.encode(out)
}

async fn product(e: &Env, name: &str, price: i64, extra: Value) -> Value {
    let mut body = json!({ "name": name, "tax_rule_id": e.tax, "unit": "pcs", "price_minor": price, "barcodes": [] });
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    call(&e.rt, "products.create", Some(&e.t), body).await
}

fn status_of(e: &Env, pid: &str) -> String {
    e.core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT status FROM wa_catalog_products WHERE product_id=?1", [pid], |r| r.get::<_, String>(0))
                .unwrap_or_else(|_| "none".into()))
        })
        .unwrap()
}

fn remote_id(e: &Env, pid: &str) -> Option<String> {
    e.core.db.read(|c| Ok(c.query_row("SELECT remote_id FROM wa_catalog_products WHERE product_id=?1", [pid], |r| r.get(0))?)).unwrap()
}

fn writes(f: &FakeAdapter) -> (u32, u32, u32) {
    let c = &f.state.catalog;
    (c.creates.load(Ordering::SeqCst), c.updates.load(Ordering::SeqCst), c.deletes.load(Ordering::SeqCst))
}

/// Let the worker run a few passes.
async fn settle(e: &Env) {
    for _ in 0..6 {
        e.rt.whatsapp.catalog_changed();
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_personal_account_gets_no_catalogue_sync() {
    let e = env(None).await;
    let st = capability(&e, "personal").await;
    assert_eq!(st["capability"]["collections"], false);
    product(&e, "Laban", 450, json!({})).await;
    let err = e.rt.dispatch("whatsapp.catalog_sync", Some(e.t.clone()), json!({})).await.unwrap_err();
    assert_eq!(err.details.unwrap()["kind"], "catalog_unavailable");
    settle(&e).await;
    assert_eq!(writes(&e.fake), (0, 0, 0));
    assert_eq!(count(&e.core, "SELECT COUNT(*) FROM wa_catalog_products"), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_business_account_publishes_the_pos_catalogue_and_keeps_it_in_step() {
    let e = env(Some(CatalogCapability::Supported)).await;
    // Something the merchant made in the WhatsApp Business app: never touched.
    let theirs = e.fake.add_remote(
        ACC,
        CatalogProduct {
            name: "Gift wrapping".into(),
            description: None,
            price_1000: Some(500),
            currency: "BHD".into(),
            retailer_id: "GIFT".into(),
            image_url: None,
            hidden: false,
        },
    );
    let cat = call(&e.rt, "categories.save", Some(&e.t), json!({ "name": "Dairy" })).await;
    let milk = product(
        &e,
        "Almarai Fresh Milk 1L",
        1250,
        json!({ "category_id": cat["category_id"], "description": "Full fat, 1 litre", "image_b64": png([200, 30, 30]) }),
    )
    .await;
    let laban = product(&e, "Laban Up", 100, json!({ "name_ar": "لبن أب" })).await;
    let rice = product(&e, "Basmati Rice 10kg", 100_000, json!({})).await;
    let free = product(&e, "Carrier bag", 0, json!({})).await;
    let old = product(&e, "Old stock", 990, json!({})).await;
    call(&e.rt, "products.set_active", Some(&e.t), json!({ "product_id": old["product_id"], "active": false })).await;
    let st = capability(&e, "supported").await;
    assert_eq!(st["capability"]["collections"], false, "collections cannot be written through this client");
    // Nothing is published until an administrator starts it.
    settle(&e).await;
    assert_eq!(writes(&e.fake), (0, 0, 0));
    // A cashier cannot publish.
    let cashier =
        call(&e.rt, "users.create", Some(&e.t), json!({ "user": { "display_name": "Sara", "pin": "7391", "role_id": ROLE_CASHIER } }))
            .await;
    let ct = call(&e.rt, "auth.login", None, json!({ "user_id": cashier["user_id"], "pin": "7391" })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let err = e.rt.dispatch("whatsapp.catalog_sync", Some(ct.clone()), json!({})).await.unwrap_err();
    assert_eq!(err.code, amwapos_core::ErrorCode::Forbidden);
    assert!(e.rt.dispatch("whatsapp.catalog_configure", Some(ct), json!({ "auto_sync": false })).await.is_err());

    // Initial sync.
    let r = call(&e.rt, "whatsapp.catalog_sync", Some(&e.t), json!({})).await;
    assert_eq!(r["published"], true);
    let (m, l, rc) = (milk["product_id"].as_str().unwrap(), laban["product_id"].as_str().unwrap(), rice["product_id"].as_str().unwrap());
    until("three products synced", || [m, l, rc].iter().all(|p| status_of(&e, p) == "synced")).await;
    assert_eq!(status_of(&e, free["product_id"].as_str().unwrap()), "none", "no price: not published");
    assert_eq!(status_of(&e, old["product_id"].as_str().unwrap()), "none", "archived: not published");
    assert_eq!(writes(&e.fake), (3, 0, 0));
    let remote = e.fake.remote(ACC);
    let milk_r = &remote[&remote_id(&e, m).unwrap()];
    assert_eq!((milk_r.price_1000, milk_r.currency.as_str()), (Some(1250), "BHD"), "1.250 BHD exactly");
    assert_eq!(milk_r.description.as_deref(), Some("Full fat, 1 litre"));
    assert_eq!(milk_r.retailer_id, milk["sku"].as_str().unwrap(), "the POS product code");
    assert!(milk_r.image_url.as_deref().unwrap().starts_with("https://mmg.whatsapp.net/"));
    let laban_r = &remote[&remote_id(&e, l).unwrap()];
    assert_eq!((laban_r.price_1000, laban_r.description.as_deref(), laban_r.image_url.as_ref()), (Some(100), Some("لبن أب"), None));
    assert_eq!(remote[&remote_id(&e, rc).unwrap()].price_1000, Some(100_000));
    // The picture sent is the POS-managed stored JPEG, byte for byte.
    let hash = milk["image_hash"].as_str().unwrap().to_string();
    let stored = e
        .core
        .db
        .read(|c| Ok(c.query_row("SELECT data_b64 FROM product_images WHERE image_hash=?1", [&hash], |r| r.get::<_, String>(0))?))
        .unwrap();
    let uploads = e.fake.state.catalog.uploads.lock().unwrap().clone();
    assert_eq!(uploads, vec![base64::engine::general_purpose::STANDARD.decode(stored).unwrap()]);
    // Categories are recorded as unsupported collections, never faked.
    assert_eq!(
        count(&e.core, "SELECT COUNT(*) FROM wa_catalog_collections WHERE status='unsupported' AND remote_id IS NULL"),
        count(&e.core, "SELECT COUNT(*) FROM categories")
    );

    // Running the sync again changes nothing remotely.
    call(&e.rt, "whatsapp.catalog_sync", Some(&e.t), json!({})).await;
    settle(&e).await;
    assert_eq!(writes(&e.fake), (3, 0, 0), "unchanged products are not written again");

    // Rename, price and description changes update the same remote products.
    let d = call(&e.rt, "products.get", Some(&e.t), json!({ "product_id": m })).await;
    call(
        &e.rt,
        "products.update",
        Some(&e.t),
        json!({ "product_id": m, "expected_version": d["version"], "name": "Almarai Milk 1L", "description": "Full fat",
                "tax_rule_id": e.tax, "unit": "pcs", "track_inventory": true, "allow_decimal_quantity": false,
                "reorder_point_milli": 0, "is_favorite": false }),
    )
    .await;
    call(&e.rt, "products.price_update", Some(&e.t), json!({ "product_id": l, "amount_minor": 9990 })).await;
    until("updates written", || writes(&e.fake).1 == 2).await;
    let remote = e.fake.remote(ACC);
    assert_eq!(remote[&remote_id(&e, m).unwrap()].name, "Almarai Milk 1L");
    assert_eq!(remote[&remote_id(&e, m).unwrap()].description.as_deref(), Some("Full fat"));
    assert_eq!(remote[&remote_id(&e, l).unwrap()].price_1000, Some(9990), "9.990 BHD");
    assert_eq!(writes(&e.fake).0, 3, "no duplicates");
    // The picture is re-uploaded only when it changes.
    assert_eq!(e.fake.state.catalog.uploads.lock().unwrap().len(), 1);
    call(&e.rt, "products.image_upload", Some(&e.t), json!({ "product_id": m, "data": png([20, 20, 200]) })).await;
    until("new picture sent", || e.fake.state.catalog.uploads.lock().unwrap().len() == 2 && writes(&e.fake).1 == 3).await;

    // Archiving hides the remote product (not deleted).
    call(&e.rt, "products.set_active", Some(&e.t), json!({ "product_id": rc, "active": false })).await;
    until("hidden", || status_of(&e, rc) == "hidden").await;
    assert!(e.fake.remote(ACC)[&remote_id(&e, rc).unwrap()].hidden);
    assert_eq!(writes(&e.fake).2, 0, "nothing deleted");
    // The merchant's own WhatsApp product is untouched.
    assert_eq!(e.fake.remote(ACC)[&theirs].name, "Gift wrapping");

    // Product-level state for the editor.
    let ps = call(&e.rt, "whatsapp.catalog_product", Some(&e.t), json!({ "product_id": m })).await;
    assert_eq!((ps["status"].as_str(), ps["on_whatsapp"].as_bool()), (Some("synced"), Some(true)));
    let ov = call(&e.rt, "whatsapp.catalog_status", Some(&e.t), json!({})).await;
    assert_eq!(ov["catalog"]["counts"]["synced"], 2);
    assert_eq!(ov["catalog"]["counts"]["hidden"], 1);

    // Auto-sync off: POS changes stay local until the next manual sync.
    call(&e.rt, "whatsapp.catalog_configure", Some(&e.t), json!({ "auto_sync": false })).await;
    call(&e.rt, "products.price_update", Some(&e.t), json!({ "product_id": l, "amount_minor": 1000 })).await;
    settle(&e).await;
    assert_eq!(e.fake.remote(ACC)[&remote_id(&e, l).unwrap()].price_1000, Some(9990));
    call(&e.rt, "whatsapp.catalog_sync", Some(&e.t), json!({})).await;
    until("manual sync", || e.fake.remote(ACC)[&remote_id(&e, l).unwrap()].price_1000 == Some(1000)).await;

    // Receipts and messages still go out over the same session.
    call(
        &e.rt,
        "whatsapp.queue",
        Some(&e.t),
        json!({ "operation_id": "op-cat-1", "kind": "text", "to_phone": "33334444", "text": "Hello" }),
    )
    .await;
    until("message sent", || e.fake.sent().iter().any(|s| s.text == "Hello")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnects_resume_and_a_different_account_never_reuses_mappings() {
    let e = env(Some(CatalogCapability::Supported)).await;
    let p = product(&e, "Kiri 12 portions", 1450, json!({})).await;
    let pid = p["product_id"].as_str().unwrap().to_string();
    capability(&e, "supported").await;
    call(&e.rt, "whatsapp.catalog_sync", Some(&e.t), json!({})).await;
    until("synced", || status_of(&e, &pid) == "synced").await;
    let rid = remote_id(&e, &pid).unwrap();

    // WhatsApp down: POS writes still succeed; nothing is marked synchronised.
    call(&e.rt, "whatsapp.stop", Some(&e.t), json!({})).await;
    capability(&e, "disconnected").await;
    call(&e.rt, "products.price_update", Some(&e.t), json!({ "product_id": pid, "amount_minor": 1500 })).await;
    settle(&e).await;
    assert_eq!(e.fake.remote(ACC)[&rid].price_1000, Some(1450));
    // Back: the same remote product is updated, not duplicated.
    call(&e.rt, "whatsapp.start", Some(&e.t), json!({})).await;
    until("resumed", || e.fake.remote(ACC).get(&rid).and_then(|r| r.price_1000) == Some(1500)).await;
    assert_eq!(writes(&e.fake).0, 1);

    // A different phone links: its catalogue starts from nothing; the old
    // mappings are kept but never used with the new account.
    call(&e.rt, "whatsapp.stop", Some(&e.t), json!({})).await;
    capability(&e, "disconnected").await;
    e.fake.set_account(ACC2);
    e.fake.make_business(ACC2, CatalogCapability::Supported);
    call(&e.rt, "whatsapp.start", Some(&e.t), json!({})).await;
    let st = capability(&e, "supported").await;
    assert_eq!(st["capability"]["account"], "97339999999");
    assert_eq!(st["catalog"]["published"], false, "the new account needs its own first sync");
    settle(&e).await;
    assert!(e.fake.remote(ACC2).is_empty());
    call(&e.rt, "whatsapp.catalog_sync", Some(&e.t), json!({})).await;
    until("new account synced", || e.fake.remote(ACC2).len() == 1).await;
    assert_eq!(count(&e.core, "SELECT COUNT(*) FROM wa_catalog_products"), 2, "one mapping per account");
    let new_rid: String = e
        .core
        .db
        .read(|c| Ok(c.query_row("SELECT remote_id FROM wa_catalog_products WHERE account='97339999999'", [], |r| r.get(0))?))
        .unwrap();
    assert_ne!(new_rid, rid);
    assert_eq!(e.fake.remote(ACC)[&rid].price_1000, Some(1500), "the old account's product is untouched");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failures_are_isolated_bounded_and_a_resumed_sync_never_duplicates() {
    let e = env(Some(CatalogCapability::Supported)).await;
    let bad = product(&e, "Bad Product", 500, json!({ "sku": "BAD-1" })).await;
    let good = product(&e, "Good Product", 600, json!({})).await;
    // A remote product created by an earlier, interrupted run (created on
    // WhatsApp, never recorded): matched by the POS product code, not
    // re-created.
    let orphan = product(&e, "Interrupted", 700, json!({ "sku": "INT-7" })).await;
    let existing = e.fake.add_remote(
        ACC,
        CatalogProduct {
            name: "Interrupted".into(),
            description: None,
            price_1000: Some(700),
            currency: "BHD".into(),
            retailer_id: "INT-7".into(),
            image_url: None,
            hidden: false,
        },
    );
    *e.fake.state.catalog.reject_retailer.lock().unwrap() = Some("BAD-1".into());
    capability(&e, "supported").await;
    call(&e.rt, "whatsapp.catalog_sync", Some(&e.t), json!({})).await;
    let (b, g, o) = (bad["product_id"].as_str().unwrap(), good["product_id"].as_str().unwrap(), orphan["product_id"].as_str().unwrap());
    until("settled", || status_of(&e, g) == "synced" && status_of(&e, b) == "failed" && status_of(&e, o) == "synced").await;
    assert_eq!(remote_id(&e, o).as_deref(), Some(existing.as_str()), "adopted by product code");
    assert_eq!(writes(&e.fake).0, 1, "only the good product was created");
    // A permanent error does not loop.
    settle(&e).await;
    assert_eq!(status_of(&e, b), "failed");
    // Retry after the cause is fixed.
    *e.fake.state.catalog.reject_retailer.lock().unwrap() = None;
    call(&e.rt, "whatsapp.catalog_retry", Some(&e.t), json!({})).await;
    until("retried", || status_of(&e, b) == "synced").await;

    // A temporary error is retried later (bounded, with a delay).
    e.fake.state.catalog.fail_writes.store(1, Ordering::SeqCst);
    call(&e.rt, "products.price_update", Some(&e.t), json!({ "product_id": g, "amount_minor": 650 })).await;
    until("retry scheduled", || {
        count(&e.core, "SELECT COUNT(*) FROM wa_catalog_products WHERE status='queued' AND next_at IS NOT NULL AND last_error IS NOT NULL")
            == 1
    })
    .await;
    e.core.db.write(|c| Ok(c.execute("UPDATE wa_catalog_products SET next_at=NULL", [])?)).unwrap();
    e.rt.whatsapp.catalog_changed();
    until("retried ok", || e.fake.remote(ACC).values().any(|p| p.name == "Good Product" && p.price_1000 == Some(650))).await;

    // A create whose reply is lost (stored on WhatsApp, error here), and then
    // an unreadable catalogue: the product is not created again blind; once
    // the catalogue can be read, the copy is adopted, never duplicated.
    let creates_before = writes(&e.fake).0;
    e.fake.state.catalog.fail_after_create.store(1, Ordering::SeqCst);
    e.fake.state.catalog.fail_list.store(true, Ordering::SeqCst);
    let late = product(&e, "Late Product", 800, json!({})).await;
    let lp = late["product_id"].as_str().unwrap().to_string();
    until("reply lost", || writes(&e.fake).0 == creates_before + 1).await;
    for _ in 0..3 {
        e.core.db.write(|c| Ok(c.execute("UPDATE wa_catalog_products SET next_at=NULL WHERE status='queued'", [])?)).unwrap();
        settle(&e).await;
    }
    assert_eq!(writes(&e.fake).0, creates_before + 1, "not created again while the catalogue cannot be read");
    assert_eq!(status_of(&e, &lp), "queued");
    e.fake.state.catalog.fail_list.store(false, Ordering::SeqCst);
    e.core.db.write(|c| Ok(c.execute("UPDATE wa_catalog_products SET next_at=NULL WHERE status='queued'", [])?)).unwrap();
    until("adopted", || status_of(&e, &lp) == "synced").await;
    assert_eq!(writes(&e.fake).0, creates_before + 1, "the copy is adopted, not duplicated");
    assert_eq!(e.fake.remote(ACC).values().filter(|p| p.name == "Late Product").count(), 1);
}
