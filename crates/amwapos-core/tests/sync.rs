mod common;

use std::sync::Arc;

use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::FinalizeRequest;
use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_core::sync::{self, NonceCache, PairRequest, PullRequest, PushRequest};
use amwapos_core::ErrorCode;
use common::*;

struct Term {
    _dir: tempfile::TempDir,
    core: AppCore,
    token: String,
}

fn count(core: &AppCore, sql: &str) -> i64 {
    core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn pair(hub: &Env, name: &str, code: &str) -> Term {
    let t = &hub.owner_token;
    let pc = hub.core.sync_issue_pairing_code(t, Some(name.into())).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let core = AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap();
    let resp = hub
        .core
        .hub_pair(PairRequest {
            code: pc["code"].as_str().unwrap().into(),
            device_name: name.into(),
            device_code: code.into(),
            app_version: "test".into(),
            schema_version: amwapos_core::db::latest_schema_version(),
            os_info: None,
        })
        .unwrap();
    core.terminal_bootstrap("http://hub.local:47800", resp).unwrap();
    let token = core.login(&hub.owner_id, OWNER_PIN).unwrap().token;
    Term { _dir: dir, core, token }
}

/// One sync cycle: push, then pull until caught up. `lose_response` simulates
/// the hub committing a push whose response never reaches the terminal.
fn sync_once(hub: &AppCore, term: &AppCore, lose_response: bool) {
    let dev = term.device().unwrap().device_id;
    let (changes, scanned) = term.terminal_collect_push(1000).unwrap();
    if !changes.is_empty() {
        let resp = hub.hub_apply_push(&dev, PushRequest { device_id: dev.clone(), changes: changes.clone() }).unwrap();
        assert!(resp.rejected.is_empty(), "rejected: {:?}", resp.rejected);
        if lose_response {
            return;
        }
        term.terminal_record_push(scanned, &resp, &changes).unwrap();
    } else {
        term.terminal_record_push(scanned, &amwapos_core::sync::PushResponse { accepted: 0, rejected: vec![], up_to_seq: 0 }, &[]).unwrap();
    }
    loop {
        let since = term.terminal_sync_settings().unwrap().pull_cursor;
        let resp = hub.hub_pull(&dev, PullRequest { device_id: dev.clone(), since_seq: since, limit: Some(50) }).unwrap();
        let (_a, failed) = term.terminal_apply_pull(&resp).unwrap();
        assert_eq!(failed, 0);
        if !resp.has_more {
            break;
        }
    }
}

fn sell(core: &AppCore, token: &str, barcode: &str, qty: i64) -> String {
    let cart = core.pos_scan(token, barcode, Some(qty)).unwrap().cart;
    let total = cart.totals.total_minor;
    core.pos_finalize(
        token,
        FinalizeRequest {
            cart_id: cart.cart_id.unwrap(),
            operation_id: op(),
            tenders: vec![TenderInput { method: "cash".into(), amount_minor: total, reference: None }],
            approval_token: None,
            expected_total_minor: None,
        },
    )
    .unwrap()
    .sale_id
}

fn stock(core: &AppCore, barcode: &str) -> i64 {
    count(core, &format!("SELECT COALESCE((SELECT qty_milli FROM stock_levels s JOIN product_barcodes b ON b.product_id=s.product_id WHERE b.barcode='{barcode}'),0)"))
}

#[test]
fn hub_and_terminals_converge_without_duplicates() {
    let hub = env();
    let ht = hub.owner_token.clone();
    hub.core.sync_enable_hub(&ht).unwrap();
    let pid = hub.product("Laban 200ml", "4001", 150, 90, 100_000);
    let t1 = pair(&hub, "Till 2", "T02");
    let t2 = pair(&hub, "Till 3", "T03");
    // Bootstrap carried catalogue, price, stock and staff.
    assert_eq!(stock(&t1.core, "4001"), 100_000);
    assert_eq!(t1.core.pos_scan(&t1.token, "4001", None).unwrap().cart.totals.total_minor, 150);
    t1.core.pos_cancel_sale(&t1.token, None).unwrap();
    // Catalogue edits are refused on terminals.
    let err = t1.core.product_price_update(&t1.token, &pid, 999, None, None).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);

    // Offline selling on both terminals and the hub.
    t1.core.shift_open(&t1.token, 0, &op()).unwrap();
    t2.core.shift_open(&t2.token, 0, &op()).unwrap();
    hub.core.shift_open(&ht, 0, &op()).unwrap();
    for _ in 0..3 {
        sell(&t1.core, &t1.token, "4001", 2_000);
    }
    let t2_sale = sell(&t2.core, &t2.token, "4001", 5_000);
    sell(&hub.core, &ht, "4001", 1_000);
    // Hub raises the price meanwhile.
    hub.core.product_price_update(&ht, &pid, 175, Some("Supplier".into()), None).unwrap();

    // T1's first push is committed on the hub but the response is lost; retry.
    sync_once(&hub.core, &t1.core, true);
    sync_once(&hub.core, &t1.core, false);
    sync_once(&hub.core, &t2.core, false);
    sync_once(&hub.core, &t1.core, false);

    // No duplicates anywhere.
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM sales"), 5);
    assert_eq!(count(&t1.core, "SELECT COUNT(*) FROM sales"), 5);
    assert_eq!(count(&t2.core, "SELECT COUNT(*) FROM sales"), 5, "t2 pulled t1's and the hub's sales");
    sync_once(&hub.core, &t2.core, false);
    assert_eq!(count(&t2.core, "SELECT COUNT(*) FROM sales"), 5, "repeated sync is a no-op");
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM stock_movements WHERE type='sale'"), 5);
    // Stock converges: 100 - 6 - 5 - 1 = 88 on every node.
    for core in [&hub.core, &t1.core, &t2.core] {
        assert_eq!(stock(core, "4001"), 88_000);
        assert_eq!(count(core, "SELECT SUM(total_minor) FROM sales"), 3 * 300 + 750 + 150);
    }
    // Price change reached the terminals.
    assert_eq!(t2.core.pos_scan(&t2.token, "4001", None).unwrap().cart.totals.total_minor, 175);
    t2.core.pos_cancel_sale(&t2.token, None).unwrap();

    // A terminal can refund a sale made on another terminal once synced.
    let d = t1.core.sale_get(&t1.token, &t2_sale).unwrap();
    t1.core
        .refund_create(
            &t1.token,
            serde_json::from_value(serde_json::json!({
                "sale_id": t2_sale, "reason": "Expired", "operation_id": op(),
                "lines": [{ "sale_item_id": d.items[0].sale_item_id, "qty_milli": 1000 }]
            }))
            .unwrap(),
        )
        .unwrap();
    sync_once(&hub.core, &t1.core, false);
    sync_once(&hub.core, &t2.core, false);
    for core in [&hub.core, &t1.core, &t2.core] {
        assert_eq!(count(core, "SELECT COUNT(*) FROM refunds"), 1);
        assert_eq!(stock(core, "4001"), 89_000);
    }
    // Hub heartbeat and status.
    hub.core.hub_heartbeat(&t1.core.device().unwrap().device_id, t1.core.terminal_heartbeat().unwrap()).unwrap();
    let st = hub.core.sync_status(&ht).unwrap();
    assert_eq!(st["devices"].as_array().unwrap().len(), 3);
    assert_eq!(t1.core.terminal_pending_count().unwrap(), 0);
}

#[test]
fn forged_and_invalid_pushes_are_rejected() {
    let hub = env();
    hub.core.sync_enable_hub(&hub.owner_token).unwrap();
    hub.product("Tissue", "4101", 500, 200, 10_000);
    let t1 = pair(&hub, "Till 2", "T02");
    let t2 = pair(&hub, "Till 3", "T03");
    t1.core.shift_open(&t1.token, 0, &op()).unwrap();
    sell(&t1.core, &t1.token, "4101", 1_000);
    let (changes, _) = t1.core.terminal_collect_push(1000).unwrap();
    // T2 tries to push T1's sale as its own.
    let d2 = t2.core.device().unwrap().device_id;
    let resp = hub.core.hub_apply_push(&d2, PushRequest { device_id: d2.clone(), changes: changes.clone() }).unwrap();
    assert!(resp.rejected.iter().any(|r| r.table == "sales"));
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM sales"), 0);
    assert!(count(&hub.core, "SELECT COUNT(*) FROM sync_dead_letters") > 0);
    // Terminals may not push catalogue changes.
    let mut bad = changes[0].clone();
    bad.table = "products".into();
    let d1 = t1.core.device().unwrap().device_id;
    let resp = hub.core.hub_apply_push(&d1, PushRequest { device_id: d1.clone(), changes: vec![bad] }).unwrap();
    assert_eq!(resp.rejected.len(), 1);
    // Genuine push is accepted.
    let resp = hub.core.hub_apply_push(&d1, PushRequest { device_id: d1.clone(), changes }).unwrap();
    assert!(resp.rejected.is_empty(), "{:?}", resp.rejected);
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM sales"), 1);
}

#[test]
fn signatures_revocation_and_pairing_codes() {
    let hub = env();
    let ht = hub.owner_token.clone();
    hub.core.sync_enable_hub(&ht).unwrap();
    let t1 = pair(&hub, "Till 2", "T02");
    let dev = t1.core.device().unwrap().device_id;
    let key = t1.core.terminal_device_key().unwrap();
    let nonces = NonceCache::default();
    let body = br#"{"x":1}"#;
    let ts = chrono::Utc::now().timestamp_millis();
    let sig = sync::hmac_hex(&key, sync::signing_string("POST", "/sync/push", ts, "n0nce-0000000000001", body).as_bytes());
    assert_eq!(hub.core.hub_authenticate(&nonces, &dev, ts, "n0nce-0000000000001", &sig, "POST", "/sync/push", body).unwrap(), dev);
    // Replay is refused.
    assert!(hub.core.hub_authenticate(&nonces, &dev, ts, "n0nce-0000000000001", &sig, "POST", "/sync/push", body).is_err());
    // Tampered body is refused.
    let sig2 = sync::hmac_hex(&key, sync::signing_string("POST", "/sync/push", ts, "n0nce-0000000000002", body).as_bytes());
    assert!(hub.core.hub_authenticate(&nonces, &dev, ts, "n0nce-0000000000002", &sig2, "POST", "/sync/push", b"{}").is_err());
    // Stale timestamp is refused.
    let old = ts - 10 * 60 * 1000;
    let sig3 = sync::hmac_hex(&key, sync::signing_string("POST", "/sync/push", old, "n0nce-0000000000003", body).as_bytes());
    assert!(hub.core.hub_authenticate(&nonces, &dev, old, "n0nce-0000000000003", &sig3, "POST", "/sync/push", body).is_err());
    // Revocation takes effect immediately.
    hub.core.device_set_active(&ht, &dev, false).unwrap();
    let sig4 = sync::hmac_hex(&key, sync::signing_string("POST", "/sync/push", ts, "n0nce-0000000000004", body).as_bytes());
    let err = hub.core.hub_authenticate(&nonces, &dev, ts, "n0nce-0000000000004", &sig4, "POST", "/sync/push", body).unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);
    // Pairing codes are single-use.
    let pc = hub.core.sync_issue_pairing_code(&ht, None).unwrap();
    let mk = |code: &str, dc: &str| PairRequest {
        code: code.into(),
        device_name: "X".into(),
        device_code: dc.into(),
        app_version: "t".into(),
        schema_version: amwapos_core::db::latest_schema_version(),
        os_info: None,
    };
    hub.core.hub_pair(mk(pc["code"].as_str().unwrap(), "T09")).unwrap();
    assert!(hub.core.hub_pair(mk(pc["code"].as_str().unwrap(), "T10")).is_err());
    assert!(hub.core.hub_pair(mk("12345678", "T11")).is_err());
    let pc = hub.core.sync_issue_pairing_code(&ht, None).unwrap();
    assert_eq!(hub.core.hub_pair(mk(pc["code"].as_str().unwrap(), "T02")).unwrap_err().code, ErrorCode::Duplicate);
}

#[test]
fn rebuilt_hub_does_not_become_authority() {
    let hub = env();
    hub.core.sync_enable_hub(&hub.owner_token).unwrap();
    hub.product("Salt", "4201", 200, 100, 10_000);
    let t1 = pair(&hub, "Till 2", "T02");
    t1.core.shift_open(&t1.token, 0, &op()).unwrap();
    sell(&t1.core, &t1.token, "4201", 1_000);
    sync_once(&hub.core, &t1.core, false);
    // A brand-new empty hub appears at the same address.
    let hub2 = env();
    hub2.core.sync_enable_hub(&hub2.owner_token).unwrap();
    let dev = t1.core.device().unwrap().device_id;
    let resp = hub2.core.hub_pull(&dev, PullRequest { device_id: dev.clone(), since_seq: 0, limit: None }).unwrap();
    let err = t1.core.terminal_apply_pull(&resp).unwrap_err();
    assert_eq!(err.code, ErrorCode::Sync);
    assert!(t1.core.terminal_sync_settings().unwrap().blocked_reason.is_some());
    // Local records untouched.
    assert_eq!(count(&t1.core, "SELECT COUNT(*) FROM sales"), 1);
    assert_eq!(count(&t1.core, "SELECT COUNT(*) FROM products"), 1);
}

#[test]
fn version_mismatch_refuses_pairing() {
    let hub = env();
    hub.core.sync_enable_hub(&hub.owner_token).unwrap();
    let pc = hub.core.sync_issue_pairing_code(&hub.owner_token, None).unwrap();
    let err = hub
        .core
        .hub_pair(PairRequest {
            code: pc["code"].as_str().unwrap().into(),
            device_name: "Old".into(),
            device_code: "T05".into(),
            app_version: "0.0.1".into(),
            schema_version: 1,
            os_info: None,
        })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
}
