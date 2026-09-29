//! WhatsApp catalogue publishing state in the core: nothing before an
//! administrator starts it, account-scoped mappings, fingerprints (no
//! rewrites when unchanged), the claim / outcome state machine with bounded
//! retries, deletion and remote-missing semantics, and permissions.
mod common;

use amwapos_core::auth::ROLE_CASHIER;
use amwapos_core::wa_catalog::{CatalogAction, CatalogOutcome, MAX_ATTEMPTS};
use amwapos_core::ErrorCode;
use common::*;
use serde_json::json;

const ACC: &str = "97330000000@s.whatsapp.net";
const ACC2: &str = "97339999999@s.whatsapp.net";

fn count(e: &Env, sql: &str) -> i64 {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn published(e: &Env, pid: &str, remote: &str) {
    let job = e.core.wa_catalog_claim(ACC, 10).unwrap().into_iter().find(|j| j.product_id == pid).unwrap();
    let item = job.item.clone().unwrap();
    let st = e
        .core
        .wa_catalog_complete(
            ACC,
            pid,
            CatalogOutcome::Published { remote_id: remote.into(), item, image_url: None, adopted: false, image_rejected: None },
        )
        .unwrap();
    assert_eq!(st, "synced");
}

#[test]
fn nothing_is_published_until_an_administrator_starts_it() {
    let e = env();
    let pid = e.product("Laban", "7001", 450, 300, 0);
    assert!(e.core.wa_catalog_claim(ACC, 10).unwrap().is_empty());
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 0, "the automatic scan needs the first sync");
    assert!(!e.core.wa_catalog_auto(ACC).unwrap());
    // A cashier can neither start nor see it.
    let (_u, ct) = e.user("Cashier", ROLE_CASHIER, "2580");
    assert_eq!(e.core.wa_catalog_start(&ct, ACC).unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(e.core.wa_catalog_overview(&ct, Some(ACC)).unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(e.core.wa_catalog_configure(&ct, false).unwrap_err().code, ErrorCode::Forbidden);
    // Start: queued, the job carries the exact price in thousandths.
    let o = e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    assert_eq!((o["published"].as_bool(), o["counts"]["queued"].as_i64()), (Some(true), Some(1)));
    let jobs = e.core.wa_catalog_claim(ACC, 10).unwrap();
    assert_eq!(jobs.len(), 1);
    let j = &jobs[0];
    assert_eq!((j.product_id.as_str(), &j.action), (pid.as_str(), &CatalogAction::Upsert));
    assert_eq!(j.item.as_ref().unwrap().price_1000, Some(450));
    assert_eq!(j.item.as_ref().unwrap().currency, "BHD");
    assert!(e.core.wa_catalog_claim(ACC, 10).unwrap().is_empty(), "claimed once");
    assert!(count(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type='whatsapp.catalog_sync'") == 1);
}

#[test]
fn unchanged_products_are_not_rewritten_and_changes_are_detected() {
    let e = env();
    let pid = e.product("Laban", "7001", 450, 300, 0);
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    published(&e, &pid, "5001");
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 0, "unchanged: nothing queued");
    e.core.product_price_update(&e.owner_token, &pid, 500, None, None).unwrap();
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 1);
    let j = e.core.wa_catalog_claim(ACC, 10).unwrap().remove(0);
    assert_eq!((j.remote_id.as_deref(), j.item.as_ref().unwrap().price_1000), (Some("5001"), Some(500)), "the same remote product");
    // Archive → hide; a hidden product is not re-written on later edits.
    let item = j.item.clone().unwrap();
    e.core
        .wa_catalog_complete(
            ACC,
            &pid,
            CatalogOutcome::Published { remote_id: "5001".into(), item, image_url: None, adopted: false, image_rejected: None },
        )
        .unwrap();
    e.core.product_set_active(&e.owner_token, &pid, false).unwrap();
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 1);
    let j = e.core.wa_catalog_claim(ACC, 10).unwrap().remove(0);
    assert_eq!(j.action, CatalogAction::Hide);
    assert!(j.item.as_ref().unwrap().hidden);
    let item = j.item.clone().unwrap();
    assert_eq!(
        e.core
            .wa_catalog_complete(
                ACC,
                &pid,
                CatalogOutcome::Published { remote_id: "5001".into(), item, image_url: None, adopted: false, image_rejected: None }
            )
            .unwrap(),
        "hidden"
    );
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 0);
}

#[test]
fn transient_errors_are_bounded_and_permanent_ones_wait_for_a_change_or_retry() {
    let e = env();
    let pid = e.product("Flaky", "7002", 300, 100, 0);
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    let due = || e.core.db.write(|c| Ok(c.execute("UPDATE wa_catalog_products SET next_at=NULL", [])?)).unwrap();
    for i in 1..=MAX_ATTEMPTS {
        let j = e.core.wa_catalog_claim(ACC, 10).unwrap();
        assert_eq!(j.len(), 1, "attempt {i}");
        let st =
            e.core.wa_catalog_complete(ACC, &pid, CatalogOutcome::Transient { error: "timeout".into(), retry_after_s: Some(120) }).unwrap();
        assert_eq!(st, if i < MAX_ATTEMPTS { "retry" } else { "failed" });
        if i == 1 {
            assert!(e.core.wa_catalog_claim(ACC, 10).unwrap().is_empty(), "waits for its delay");
        }
        due();
    }
    assert!(e.core.wa_catalog_claim(ACC, 10).unwrap().is_empty(), "failed is terminal");
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 0, "no loop while nothing changed");
    // A change re-queues it; so does an explicit retry.
    e.core.product_price_update(&e.owner_token, &pid, 350, None, None).unwrap();
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 1);
    let _ = e.core.wa_catalog_claim(ACC, 10).unwrap();
    e.core.wa_catalog_complete(ACC, &pid, CatalogOutcome::Failed { error: "400".into() }).unwrap();
    assert_eq!(e.core.wa_catalog_retry(&e.owner_token, ACC, None).unwrap()["queued"], 1);
    assert_eq!(e.core.wa_catalog_claim(ACC, 10).unwrap().len(), 1);
}

#[test]
fn a_stale_claim_is_recovered_and_a_superseded_outcome_is_dropped() {
    let e = env();
    let pid = e.product("Stale", "7003", 300, 100, 0);
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    let j = e.core.wa_catalog_claim(ACC, 10).unwrap().remove(0);
    e.core.db.write(|c| Ok(c.execute("UPDATE wa_catalog_products SET claimed_at='2000-01-01T00:00:00.000Z'", [])?)).unwrap();
    assert_eq!(e.core.wa_catalog_claim(ACC, 10).unwrap().len(), 1, "taken back after the claim timeout");
    let _ = j;
    // An outcome for a row that is not claimed any more is ignored.
    e.core.wa_catalog_complete(ACC, &pid, CatalogOutcome::Deleted).unwrap();
    assert_eq!(e.core.wa_catalog_complete(ACC, &pid, CatalogOutcome::Deleted).unwrap(), "superseded");
}

#[test]
fn remote_missing_and_pos_deletion_semantics() {
    let e = env();
    let pid = e.product("Gone there", "7004", 300, 100, 0);
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    published(&e, &pid, "5004");
    // Deleted on WhatsApp: never silently re-created.
    e.core.product_price_update(&e.owner_token, &pid, 320, None, None).unwrap();
    e.core.wa_catalog_scan(ACC).unwrap();
    let _ = e.core.wa_catalog_claim(ACC, 10).unwrap();
    e.core.wa_catalog_complete(ACC, &pid, CatalogOutcome::RemoteMissing { error: "deleted on WhatsApp".into() }).unwrap();
    e.core.product_price_update(&e.owner_token, &pid, 330, None, None).unwrap();
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 0, "remote_missing waits for Retry");
    // Retry creates it again (the stale remote id is dropped).
    e.core.wa_catalog_retry(&e.owner_token, ACC, Some(&pid)).unwrap();
    let j = e.core.wa_catalog_claim(ACC, 10).unwrap().remove(0);
    assert_eq!((j.action, j.remote_id), (CatalogAction::Upsert, None));
    // A mapping whose POS product no longer exists deletes only its own remote product.
    e.core
        .db
        .write(|c| {
            Ok(c.execute(
                "INSERT INTO wa_catalog_products(account, product_id, remote_id, status, created_at, updated_at)
                 VALUES ('97330000000','01ARZ3NDEKTSV4RRFFQ69G5FAV','5099','synced','x','x')",
                [],
            )?)
        })
        .unwrap();
    e.core.wa_catalog_scan(ACC).unwrap();
    let j = e.core.wa_catalog_claim(ACC, 10).unwrap().into_iter().find(|j| j.remote_id.as_deref() == Some("5099")).unwrap();
    assert_eq!(j.action, CatalogAction::Delete);
    assert_eq!(e.core.wa_catalog_complete(ACC, &j.product_id, CatalogOutcome::Deleted).unwrap(), "removed");
}

#[test]
fn mappings_are_scoped_to_the_linked_account() {
    let e = env();
    let pid = e.product("Scoped", "7005", 300, 100, 0);
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    published(&e, &pid, "5005");
    // Another linked number: not published, no jobs, no reuse of 5005.
    assert!(!e.core.wa_catalog_published(ACC2).unwrap());
    assert!(e.core.wa_catalog_claim(ACC2, 10).unwrap().is_empty());
    e.core.wa_catalog_start(&e.owner_token, ACC2).unwrap();
    let j = e.core.wa_catalog_claim(ACC2, 10).unwrap().remove(0);
    assert_eq!(j.remote_id, None, "a new account starts without remote ids");
    // A remote id already linked to another product is never accepted twice.
    let other = e.product("Other", "7006", 300, 100, 0);
    e.core.wa_catalog_scan(ACC).unwrap();
    let k = e.core.wa_catalog_claim(ACC, 10).unwrap().into_iter().find(|j| j.product_id == other).unwrap();
    let item = k.item.clone().unwrap();
    let st = e
        .core
        .wa_catalog_complete(
            ACC,
            &other,
            CatalogOutcome::Published { remote_id: "5005".into(), item, image_url: None, adopted: true, image_rejected: None },
        )
        .unwrap();
    assert_eq!(st, "failed");
    let ov = e.core.wa_catalog_overview(&e.owner_token, Some(ACC)).unwrap();
    assert_eq!(ov["counts"]["synced"], 1);
    assert_eq!(ov["failures"][0]["product_id"], json!(other));
}

// ---------------------------------------------------------------------------
// Hardening pass (audit 2026-09-29): one test per finding / invariant.

use std::collections::HashSet;

fn claim_one(e: &Env, acc: &str, pid: &str) -> amwapos_core::wa_catalog::CatalogJob {
    e.core.wa_catalog_claim(acc, 50).unwrap().into_iter().find(|j| j.product_id == pid).expect("claimed")
}

fn publish(e: &Env, acc: &str, j: &amwapos_core::wa_catalog::CatalogJob, remote: &str) -> String {
    e.core
        .wa_catalog_complete(
            acc,
            &j.product_id,
            CatalogOutcome::Published {
                remote_id: remote.into(),
                item: j.item.clone().unwrap(),
                image_url: None,
                adopted: false,
                image_rejected: None,
            },
        )
        .unwrap()
}

fn status(e: &Env, pid: &str) -> String {
    e.core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT status FROM wa_catalog_products WHERE account='97330000000' AND product_id=?1", [pid], |r| r.get(0))?)
        })
        .unwrap()
}

/// Close the open run's remote check as if the worker listed `listed`.
fn verify(e: &Env, acc: &str, listed: &[&str]) -> usize {
    let run = e.core.wa_catalog_verify_due(acc).unwrap().expect("a run waits for its check");
    let set: HashSet<String> = listed.iter().map(|s| s.to_string()).collect();
    e.core.wa_catalog_verify(acc, &run, Some(&set)).unwrap()
}

fn sql(e: &Env, q: &str) {
    e.core.db.write(|c| Ok(c.execute_batch(q)?)).unwrap();
}

/// Remove a product and everything that points at it (the POS has no delete
/// command; this models a product that disappeared, e.g. an older backup).
fn delete_product(e: &Env, pid: &str) {
    e.core
        .db
        .write(|c| {
            let tables: Vec<String> = {
                let mut st = c.prepare(
                    "SELECT m.name FROM sqlite_master m WHERE m.type='table' AND m.name NOT IN ('products','wa_catalog_products')
                     AND EXISTS (SELECT 1 FROM pragma_table_info(m.name) WHERE name='product_id')",
                )?;
                let r = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
                r
            };
            // Test database only: history tables refuse deletes by trigger.
            let guards: Vec<String> = {
                let mut st = c.prepare("SELECT name FROM sqlite_master WHERE type='trigger' AND sql LIKE '%BEFORE DELETE%'")?;
                let r = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
                r
            };
            for g in guards {
                c.execute_batch(&format!("DROP TRIGGER {g}"))?;
            }
            for t in tables {
                c.execute(&format!("DELETE FROM {t} WHERE product_id=?1"), [pid])?;
            }
            c.execute("DELETE FROM products WHERE product_id=?1", [pid])?;
            Ok(())
        })
        .unwrap();
}

fn set_picture(e: &Env, pid: &str, hash: &str, bytes: Option<&[u8]>) {
    e.core
        .db
        .write(|c| {
            if let Some(b) = bytes {
                c.execute(
                    "INSERT OR REPLACE INTO product_images(image_hash, mime, width, height, bytes, data_b64, created_at) VALUES (?1,'image/jpeg',10,10,?2,?3,'x')",
                    rusqlite::params![hash, b.len() as i64, amwapos_core::ids::b64(b)],
                )?;
            }
            c.execute("UPDATE products SET image_hash=?2 WHERE product_id=?1", rusqlite::params![pid, hash])?;
            Ok(())
        })
        .unwrap();
}

const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4];

#[test]
fn a_full_sync_is_a_run_with_progress_and_one_remote_check() {
    let e = env();
    let a = e.product("Run A", "8001", 300, 100, 0);
    let b = e.product("Run B", "8002", 400, 100, 0);
    let o = e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    assert_eq!((o["run"]["total"].as_i64(), o["run"]["processed"].as_i64()), (Some(2), Some(0)));
    // Pressing Sync again joins the same run: no duplicate rows, same total.
    let o = e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    assert_eq!(o["run"]["total"], 2);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_catalog_runs"), 1);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_catalog_products"), 2);
    let js = e.core.wa_catalog_claim(ACC, 50).unwrap();
    assert_eq!(js.len(), 2);
    let ja = js.iter().find(|j| j.product_id == a).unwrap();
    let jb = js.iter().find(|j| j.product_id == b).unwrap();
    publish(&e, ACC, ja, "9001");
    publish(&e, ACC, jb, "9002");
    let o = e.core.wa_catalog_overview(&e.owner_token, Some(ACC)).unwrap();
    assert_eq!((o["run"]["processed"].as_i64(), o["run"]["synced"].as_i64()), (Some(2), Some(2)));
    assert!(o["run"]["finished_at"].is_null(), "not finished before the remote check");
    // The remote check: 9002 is not listed any more (deleted on WhatsApp).
    assert_eq!(verify(&e, ACC, &["9001"]), 1);
    let j = claim_one(&e, ACC, &b);
    assert_eq!(j.remote_id.as_deref(), Some("9002"), "re-checked by a write to the same id");
    // WhatsApp answers "not found": part of a full sync, so it is published again.
    assert_eq!(e.core.wa_catalog_complete(ACC, &b, CatalogOutcome::RemoteMissing { error: "deleted".into() }).unwrap(), "requeued");
    let j = claim_one(&e, ACC, &b);
    assert_eq!((j.action.clone(), j.remote_id.clone()), (CatalogAction::Upsert, None));
    publish(&e, ACC, &j, "9003");
    let o = e.core.wa_catalog_overview(&e.owner_token, Some(ACC)).unwrap();
    assert!(o["run"].is_null(), "finished");
    assert_eq!((o["last_run"]["total"].as_i64(), o["last_run"]["processed"].as_i64()), (Some(3), Some(3)));
    assert_eq!(o["last_run"]["verify"], "done");
}

#[test]
fn automatic_sync_never_recreates_a_product_deleted_on_whatsapp() {
    let e = env();
    let p = e.product("Auto only", "8003", 300, 100, 0);
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    let j = claim_one(&e, ACC, &p);
    publish(&e, ACC, &j, "9101");
    verify(&e, ACC, &["9101"]);
    assert!(e.core.wa_catalog_verify_due(ACC).unwrap().is_none(), "run finished");
    e.core.product_price_update(&e.owner_token, &p, 320, None, None).unwrap();
    e.core.wa_catalog_scan(ACC).unwrap();
    claim_one(&e, ACC, &p);
    assert_eq!(e.core.wa_catalog_complete(ACC, &p, CatalogOutcome::RemoteMissing { error: "deleted".into() }).unwrap(), "remote_missing");
    e.core.product_price_update(&e.owner_token, &p, 330, None, None).unwrap();
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 0, "the automatic sync leaves it for a person");
    // An explicit full sync publishes it again, as a new remote product.
    let o = e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    assert_eq!(o["run"]["total"], 1);
    let j = claim_one(&e, ACC, &p);
    assert_eq!(j.remote_id, None);
}

#[test]
fn an_edit_made_while_a_write_is_in_flight_is_never_reported_as_published() {
    let e = env();
    let p = e.product("In flight", "8004", 300, 100, 0);
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    let j = claim_one(&e, ACC, &p);
    publish(&e, ACC, &j, "9201");
    verify(&e, ACC, &["9201"]);
    // Worker takes version A; the user saves version B; A's reply arrives.
    e.core.product_price_update(&e.owner_token, &p, 350, None, None).unwrap();
    e.core.wa_catalog_scan(ACC).unwrap();
    let a = claim_one(&e, ACC, &p);
    assert_eq!(a.item.as_ref().unwrap().price_1000, Some(350));
    e.core.product_price_update(&e.owner_token, &p, 360, None, None).unwrap();
    assert_eq!(publish(&e, ACC, &a, "9201"), "requeued", "auto-sync: B is queued at once");
    let b = claim_one(&e, ACC, &p);
    assert_eq!(b.item.as_ref().unwrap().price_1000, Some(360));
    assert_eq!(publish(&e, ACC, &b, "9201"), "synced");
    // With automatic sync off, the stale version is recorded as what WhatsApp
    // has, and the screen says the product is out of date.
    e.core.wa_catalog_configure(&e.owner_token, false).unwrap();
    e.core.product_price_update(&e.owner_token, &p, 370, None, None).unwrap();
    e.core.db.write(|c| Ok(c.execute("UPDATE wa_catalog_products SET status='queued'", [])?)).unwrap();
    let c = claim_one(&e, ACC, &p);
    e.core.product_price_update(&e.owner_token, &p, 380, None, None).unwrap();
    assert_eq!(publish(&e, ACC, &c, "9201"), "synced");
    let o = e.core.wa_catalog_overview(&e.owner_token, Some(ACC)).unwrap();
    assert_eq!(o["out_of_date"], 1);
    let ps = e.core.wa_catalog_product_state(&e.owner_token, Some(ACC), &p).unwrap();
    assert_eq!(ps["out_of_date"], true);
}

#[test]
fn a_picture_that_is_missing_or_refused_never_blocks_the_product_or_loops() {
    let e = env();
    let p = e.product("Pictured", "8005", 300, 100, 0);
    // The product points at a picture whose bytes are not stored (yet).
    set_picture(&e, &p, &"a".repeat(64), None);
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    let j = claim_one(&e, ACC, &p);
    assert_eq!(j.item.as_ref().unwrap().image_hash, None, "published without a picture it cannot send");
    assert!(j.image_jpeg.is_none());
    publish(&e, ACC, &j, "9301");
    verify(&e, ACC, &["9301"]);
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 0, "no loop while the picture is missing");
    // The bytes arrive: the product is queued once, with the picture.
    set_picture(&e, &p, &"a".repeat(64), Some(JPEG));
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 1);
    let j = claim_one(&e, ACC, &p);
    assert_eq!(j.image_jpeg.as_deref(), Some(JPEG));
    // WhatsApp refuses the picture: published without it, remembered.
    let mut item = j.item.clone().unwrap();
    let rejected = item.image_hash.take();
    assert_eq!(
        e.core
            .wa_catalog_complete(
                ACC,
                &p,
                CatalogOutcome::Published { remote_id: "9301".into(), item, image_url: None, adopted: false, image_rejected: rejected }
            )
            .unwrap(),
        "synced"
    );
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 0, "a refused picture does not loop");
    assert_eq!(e.core.wa_catalog_product_state(&e.owner_token, Some(ACC), &p).unwrap()["picture_refused"], true);
    // A different picture is tried; a corrupt stored one is left out locally.
    set_picture(&e, &p, &"b".repeat(64), Some(b"not a jpeg"));
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 1);
    let j = claim_one(&e, ACC, &p);
    assert!(j.image_jpeg.is_none() && j.item.as_ref().unwrap().image_hash.is_none());
    publish(&e, ACC, &j, "9301");
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 0);
    // Retry forgets the refusal (the merchant may have fixed the picture).
    set_picture(&e, &p, &"b".repeat(64), Some(JPEG));
    e.core.wa_catalog_retry(&e.owner_token, ACC, Some(&p)).unwrap();
    let j = claim_one(&e, ACC, &p);
    assert_eq!(j.image_jpeg.as_deref(), Some(JPEG));
}

#[test]
fn an_uploaded_picture_is_reused_by_the_retry_of_a_failed_write() {
    let e = env();
    let p = e.product("Upload once", "8006", 300, 100, 0);
    set_picture(&e, &p, &"c".repeat(64), Some(JPEG));
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    let j = claim_one(&e, ACC, &p);
    assert!(j.image_jpeg.is_some());
    e.core.wa_catalog_note_upload(ACC, &p, &"c".repeat(64), "https://mmg.whatsapp.net/x").unwrap();
    e.core.wa_catalog_complete(ACC, &p, CatalogOutcome::Transient { error: "timeout".into(), retry_after_s: None }).unwrap();
    sql(&e, "UPDATE wa_catalog_products SET next_at=NULL");
    let j = claim_one(&e, ACC, &p);
    assert_eq!((j.image_url.as_deref(), j.image_jpeg.is_none()), (Some("https://mmg.whatsapp.net/x"), true));
}

#[test]
fn hiding_a_product_that_lost_its_price_keeps_the_last_published_price() {
    let e = env();
    let p = e.product("Priced", "8007", 450, 100, 0);
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    let j = claim_one(&e, ACC, &p);
    publish(&e, ACC, &j, "9401");
    verify(&e, ACC, &["9401"]);
    // Its price is cleared to zero in the POS: not publishable, so it is hidden.
    e.core.product_price_update(&e.owner_token, &p, 0, None, None).unwrap();
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 1);
    let j = claim_one(&e, ACC, &p);
    assert_eq!(j.action, CatalogAction::Hide);
    assert_eq!(j.item.as_ref().unwrap().price_1000, Some(450));
    assert_eq!(publish(&e, ACC, &j, "9401"), "hidden");
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 0);
}

#[test]
fn names_and_descriptions_are_cleaned_and_bounded() {
    let e = env();
    let p = e.product("x", "8008", 300, 100, 0);
    let desc = format!("Line one\r\n\r\n\r\nLine\ttwo 🥛{}", "د".repeat(2000));
    e.core
        .db
        .write(|c| {
            Ok(c.execute(
                "UPDATE products SET name=?2, description=?3 WHERE product_id=?1",
                rusqlite::params![p, "  حليب\tالمراعي\n Fresh\u{0} Milk  ", desc],
            )?)
        })
        .unwrap();
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    let j = claim_one(&e, ACC, &p);
    let i = j.item.unwrap();
    assert_eq!(i.name, "حليب المراعي Fresh Milk");
    let d = i.description.unwrap();
    assert!(d.starts_with("Line one\n\nLine two 🥛") && d.ends_with('…'), "{d:.40}");
    assert_eq!(d.chars().count(), amwapos_core::wa_catalog::DESCRIPTION_MAX_CHARS);
}

#[test]
fn exact_prices_from_one_fils_up() {
    let e = env();
    for (sku, fils) in [("P1", 1), ("P2", 10), ("P3", 999), ("P4", 10_005), ("P5", 99_999), ("P6", 100_000), ("P7", 999_999_999)] {
        e.product(&format!("Price {sku}"), sku, fils, 1, 0);
    }
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    let mut got: Vec<i64> =
        e.core.wa_catalog_claim(ACC, 50).unwrap().iter().map(|j| j.item.as_ref().unwrap().price_1000.unwrap()).collect();
    got.sort();
    assert_eq!(got, vec![1, 10, 999, 10_005, 99_999, 100_000, 999_999_999]);
}

#[test]
fn a_restarted_worker_releases_its_claims_at_once() {
    let e = env();
    let p = e.product("Released", "8009", 300, 100, 0);
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    let old = claim_one(&e, ACC, &p);
    assert_eq!(e.core.wa_catalog_release_claims().unwrap(), 1);
    assert_eq!(status(&e, &p), "queued");
    let new = claim_one(&e, ACC, &p);
    publish(&e, ACC, &new, "9501");
    // The old worker's late reply for the same row is dropped.
    assert_eq!(publish(&e, ACC, &old, "9599"), "superseded");
    assert_eq!(e.core.wa_catalog_remote_owner(ACC, "9501").unwrap().as_deref(), Some(p.as_str()));
}

#[test]
fn account_a_then_b_then_a_again_never_mixes_remote_ids() {
    let e = env();
    let p = e.product("Two phones", "8010", 300, 100, 0);
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    let j = claim_one(&e, ACC, &p);
    publish(&e, ACC, &j, "A-1");
    verify(&e, ACC, &["A-1"]);
    // Account B: its own run and mapping, never A-1.
    e.core.wa_catalog_start(&e.owner_token, ACC2).unwrap();
    let j = claim_one(&e, ACC2, &p);
    assert_eq!(j.remote_id, None);
    assert_eq!(publish(&e, ACC2, &j, "B-1"), "synced");
    // An outcome for A while B is linked lands on A's row only.
    e.core.product_price_update(&e.owner_token, &p, 310, None, None).unwrap();
    e.core.wa_catalog_scan(ACC).unwrap();
    let a = claim_one(&e, ACC, &p);
    assert_eq!(a.remote_id.as_deref(), Some("A-1"), "back on A: A's own mapping");
    publish(&e, ACC, &a, "A-1");
    let ids: Vec<(String, String)> = e
        .core
        .db
        .read(|c| {
            let mut st = c.prepare("SELECT account, remote_id FROM wa_catalog_products ORDER BY account")?;
            let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(r)
        })
        .unwrap();
    assert_eq!(ids, vec![("97330000000".into(), "A-1".into()), ("97339999999".into(), "B-1".into())]);
    // The same remote id may exist in two accounts, but never twice in one.
    let dup = e.core.db.write(|c| Ok(c.execute("UPDATE wa_catalog_products SET remote_id='A-1' WHERE account='97339999999'", [])?));
    assert!(dup.is_ok(), "ids are scoped per account");
    let other = e.product("Other", "8011", 300, 100, 0);
    e.core.wa_catalog_scan(ACC).unwrap();
    let k = claim_one(&e, ACC, &other);
    assert_eq!(publish(&e, ACC, &k, "A-1"), "failed", "a remote id already owned in this account is refused");
}

#[test]
fn a_product_that_keeps_failing_never_starves_the_others() {
    let e = env();
    let bad = e.product("Always fails", "8012", 300, 100, 0);
    let mut others = vec![];
    for i in 0..20 {
        others.push(e.product(&format!("Healthy {i}"), &format!("H{i:03}"), 300, 100, 0));
    }
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    for _ in 0..10 {
        for j in e.core.wa_catalog_claim(ACC, 5).unwrap() {
            if j.product_id == bad {
                e.core.wa_catalog_complete(ACC, &bad, CatalogOutcome::Transient { error: "timeout".into(), retry_after_s: None }).unwrap();
            } else {
                publish(&e, ACC, &j, &format!("R{}", j.product_id));
            }
        }
    }
    assert!(others.iter().all(|p| status(&e, p) == "synced"));
    let o = e.core.wa_catalog_overview(&e.owner_token, Some(ACC)).unwrap();
    assert_eq!(o["retrying"], 1);
}

#[test]
fn a_product_gone_from_the_pos_deletes_only_its_own_remote_copy() {
    let e = env();
    let p = e.product("Goes away", "8013", 300, 100, 0);
    let keep = e.product("Stays", "8014", 300, 100, 0);
    e.core.wa_catalog_start(&e.owner_token, ACC).unwrap();
    let js = e.core.wa_catalog_claim(ACC, 50).unwrap();
    publish(&e, ACC, js.iter().find(|j| j.product_id == p).unwrap(), "9601");
    publish(&e, ACC, js.iter().find(|j| j.product_id == keep).unwrap(), "9602");
    verify(&e, ACC, &["9601", "9602"]);
    // Deleted while an update was in flight: the late reply does not revive it.
    e.core.product_price_update(&e.owner_token, &p, 350, None, None).unwrap();
    e.core.wa_catalog_scan(ACC).unwrap();
    let j = claim_one(&e, ACC, &p);
    delete_product(&e, &p);
    assert_eq!(publish(&e, ACC, &j, "9601"), "requeued");
    let j = claim_one(&e, ACC, &p);
    assert_eq!((j.action.clone(), j.remote_id.as_deref()), (CatalogAction::Delete, Some("9601")));
    assert_eq!(e.core.wa_catalog_complete(ACC, &p, CatalogOutcome::Deleted).unwrap(), "removed");
    assert_eq!(status(&e, &keep), "synced");
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 0, "removed stays removed");
}

#[test]
fn catalogue_management_needs_both_whatsapp_and_product_permissions() {
    let e = env();
    let roles = |name: &str, perms: &[&str]| -> String {
        let r = e.core.role_save(&e.owner_token, None, name, None, perms.iter().map(|p| p.to_string()).collect()).unwrap();
        r.into_iter().find(|x| x.name == name).unwrap().role_id
    };
    let wa_only = roles("WA only", &["whatsapp.manage"]);
    let prod_only = roles("Products only", &["products.manage", "products.view"]);
    let (_a, wt) = e.user("Wafa", &wa_only, "3571");
    let (_b, pt) = e.user("Pavel", &prod_only, "3572");
    for t in [&wt, &pt] {
        assert_eq!(e.core.wa_catalog_start(t, ACC).unwrap_err().code, ErrorCode::Forbidden);
        assert_eq!(e.core.wa_catalog_retry(t, ACC, None).unwrap_err().code, ErrorCode::Forbidden);
        assert_eq!(e.core.wa_catalog_configure(t, false).unwrap_err().code, ErrorCode::Forbidden);
    }
    assert!(e.core.wa_catalog_overview(&wt, Some(ACC)).is_ok(), "WhatsApp managers can see the status");
    assert_eq!(e.core.wa_catalog_overview(&pt, Some(ACC)).unwrap_err().code, ErrorCode::Forbidden);
}

#[test]
fn an_upgrade_publishes_nothing() {
    let e = env();
    e.product("Existing", "8015", 300, 100, 0);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_catalog_products"), 0);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_catalog_runs"), 0);
    assert_eq!(e.core.wa_catalog_scan(ACC).unwrap(), 0);
    assert!(e.core.wa_catalog_claim(ACC, 10).unwrap().is_empty());
    assert_eq!(e.core.wa_catalog_release_claims().unwrap(), 0);
    assert!(e.core.wa_catalog_verify_due(ACC).unwrap().is_none());
}
