//! Wave 7: the Sync Reconciliation Centre between a real hub and real
//! terminals (docs/OPERATIONAL_CONTROL.md).

mod common;

use std::sync::Arc;

use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::FinalizeRequest;
use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_core::sync::{Change, PairRequest, PullRequest, PushRequest, PushResponse};
use amwapos_core::sync_recon::{BulkRetry, CloseRequest, DeadQuery};
use amwapos_core::ErrorCode;
use common::*;
use serde_json::{json, Value};

struct Term {
    _dir: tempfile::TempDir,
    core: AppCore,
    token: String,
}

fn enable_hub(core: &AppCore, token: &str) {
    core.settings_save(token, "features", json!({ "hub": true })).unwrap();
    core.sync_enable_hub(token).unwrap();
}

fn count(core: &AppCore, sql: &str) -> i64 {
    core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn text(core: &AppCore, sql: &str) -> String {
    core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn pair(hub: &Env, name: &str, code: &str) -> Term {
    let t = &hub.owner_token;
    let pc = hub.core.sync_issue_pairing_code(t, Some(name.into()), None).unwrap();
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

fn pull_all(hub: &AppCore, term: &AppCore) {
    let dev = term.device().unwrap().device_id;
    loop {
        let since = term.terminal_sync_settings().unwrap().pull_cursor;
        let resp = hub.hub_pull(&dev, PullRequest { device_id: dev.clone(), since_seq: since, limit: Some(200) }).unwrap();
        term.terminal_apply_pull(&resp).unwrap();
        if !resp.has_more {
            break;
        }
    }
}

/// Send a chosen part of the terminal's pending changes, record the hub's
/// answer on the terminal, and return it.
fn push_some(hub: &AppCore, term: &AppCore, keep: impl Fn(&Change) -> bool) -> (PushResponse, Vec<Change>) {
    let dev = term.device().unwrap().device_id;
    let (changes, scanned) = term.terminal_collect_push(1000).unwrap();
    let sent: Vec<Change> = changes.into_iter().filter(|c| keep(c)).collect();
    let resp = hub.hub_apply_push(&dev, PushRequest { device_id: dev.clone(), changes: sent.clone() }).unwrap();
    term.terminal_record_push(scanned, &resp, &sent).unwrap();
    (resp, sent)
}

fn sell(core: &AppCore, token: &str, barcode: &str) -> String {
    let cart = core.pos_scan(token, barcode, Some(1000)).unwrap().cart;
    let total = cart.totals.total_minor;
    core.pos_finalize(
        token,
        FinalizeRequest {
            cart_id: cart.cart_id.unwrap(),
            operation_id: op(),
            tenders: vec![TenderInput { method: "cash".into(), amount_minor: total, reference: None }],
            approval_token: None,
            expected_total_minor: None,
            fulfilment: None,
        },
    )
    .unwrap()
    .sale_id
}

fn page(core: &AppCore, t: &str, q: Value) -> Value {
    core.sync_dead_letters(t, serde_json::from_value(q).unwrap()).unwrap()
}

#[test]
fn a_sale_whose_parts_arrive_first_waits_and_is_saved_exactly_once_on_retry() {
    let hub = env();
    let ht = hub.owner_token.clone();
    enable_hub(&hub.core, &ht);
    hub.product("Rice", "7001", 1_500, 900, 50_000);
    let t1 = pair(&hub, "Till 2", "T02");
    pull_all(&hub.core, &t1.core);
    t1.core.shift_open(&t1.token, 0, &op()).unwrap();
    let sale = sell(&t1.core, &t1.token, "7001");

    // Everything but the sale itself reaches the hub: its lines and payment
    // wait for it. Stock and the shift are saved.
    let (resp, _) = push_some(&hub.core, &t1.core, |c| c.table != "sales");
    assert!(!resp.rejected.is_empty());
    assert!(resp.rejected.iter().all(|r| r.reason.as_deref() == Some("missing_dependency")), "{:?}", resp.rejected);
    let waiting = count(&hub.core, "SELECT COUNT(*) FROM sync_dead_letters WHERE status='open'");
    assert!(waiting >= 2, "lines and payment");
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM sync_dead_letters WHERE retryable=1 AND reason_code='missing_dependency'"), waiting);
    // The terminal keeps its own copy, read-only.
    assert_eq!(count(&t1.core, "SELECT COUNT(*) FROM sync_dead_letters WHERE status='open' AND direction='push'"), waiting);
    let tp = page(&t1.core, &t1.token, json!({}));
    assert_eq!(tp["can_act"], false);
    let some = tp["rows"][0]["dead_id"].as_str().unwrap().to_string();
    assert_eq!(t1.core.sync_retry_dead_letter(&t1.token, &some, None).unwrap_err().code, ErrorCode::Forbidden);

    // One case for the till and reason, linked to the reconciliation screen.
    hub.core.ops_evaluate().unwrap();
    let key = format!("sync_failures:{}:missing_dependency", t1.core.device().unwrap().device_id);
    assert_eq!(count(&hub.core, &format!("SELECT COUNT(*) FROM cases WHERE dedupe_key='{key}' AND status='new'")), 1);

    // Trying again too early changes nothing and says why.
    let p = page(&hub.core, &ht, json!({ "reason_code": "missing_dependency" }));
    assert_eq!(p["total"], waiting);
    assert_eq!(p["groups"][0]["count"], waiting);
    assert!(p["rows"].as_array().unwrap().iter().all(|r| r.get("payload").is_none() && r.get("payload_json").is_none()));
    let first = p["rows"][0]["dead_id"].as_str().unwrap().to_string();
    let r = hub.core.sync_retry_dead_letter(&ht, &first, Some(&op())).unwrap();
    assert_eq!(r["outcome"], "failed");
    assert_eq!(text(&hub.core, &format!("SELECT status FROM sync_dead_letters WHERE dead_id='{first}'")), "open");

    // The sale arrives (the till sends it again), then one bulk retry saves
    // the rest through the normal path.
    let dev = t1.core.device().unwrap().device_id;
    let sale_change = {
        let (all, _) = (t1.core.terminal_collect_push(1000).unwrap().0, 0);
        assert!(all.is_empty(), "the till moved past what it sent");
        let row = t1
            .core
            .db
            .read(|c| Ok(c.query_row("SELECT json_object('sale_id', sale_id) FROM sales", [], |r| r.get::<_, String>(0))?))
            .unwrap();
        let _ = row;
        // Rebuild the sale's change exactly as the till would send it.
        t1.core
            .db
            .write(|c| {
                Ok(c.execute(
                    "INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('sales', json_object('sale_id', ?1), 'upsert', NULL)",
                    [&sale],
                )?)
            })
            .unwrap();
        t1.core.terminal_collect_push(1000).unwrap().0
    };
    assert_eq!(sale_change.len(), 1);
    let resp = hub.core.hub_apply_push(&dev, PushRequest { device_id: dev.clone(), changes: sale_change }).unwrap();
    assert!(resp.rejected.is_empty(), "{:?}", resp.rejected);
    let bulk_op = op();
    let req = BulkRetry { dead_ids: vec![], origin: Some(dev.clone()), reason_code: None, table: None, operation_id: bulk_op.clone() };
    let out = hub.core.sync_retry_dead_letters(&ht, req.clone()).unwrap();
    assert_eq!(out["eligible"], waiting);
    assert_eq!(out["applied"], waiting, "{out}");
    assert_eq!(out["ineligible"], 0);
    // The same request again is answered from its first result.
    assert_eq!(hub.core.sync_retry_dead_letters(&ht, req).unwrap(), out);
    // A new request finds nothing left to do; a single retry of a finished
    // record reports it and applies nothing twice.
    let again = hub
        .core
        .sync_retry_dead_letters(
            &ht,
            BulkRetry { dead_ids: vec![first.clone()], origin: None, reason_code: None, table: None, operation_id: op() },
        )
        .unwrap();
    assert_eq!((again["eligible"].as_i64(), again["ineligible"].as_i64()), (Some(0), Some(1)));
    assert_eq!(hub.core.sync_retry_dead_letter(&ht, &first, None).unwrap()["outcome"], "already_finished");
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM sales"), 1);
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM sale_items"), 1);
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM payments"), 1);
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM stock_movements WHERE qty_delta_milli < 0"), 1);
    assert_eq!(
        count(
            &hub.core,
            &format!(
                "SELECT COUNT(*) FROM sync_dead_letters WHERE status='resolved' AND resolution='applied' AND resolved_by='{}'",
                hub.owner_id
            )
        ),
        waiting
    );
    // Two requests, two audit records (the replay is not a new request).
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM audit_logs WHERE event_type='sync.dead_letters_bulk_retried'"), 2);

    // The case shows what the person did, then resolves itself.
    let case_id = text(&hub.core, &format!("SELECT case_id FROM cases WHERE dedupe_key='{key}'"));
    assert_eq!(
        count(
            &hub.core,
            &format!("SELECT COUNT(*) FROM case_events WHERE case_id='{case_id}' AND kind='evidence' AND user_id='{}'", hub.owner_id)
        ),
        1
    );
    assert_eq!(hub.core.ops_evaluate().unwrap().resolved, 1);
    assert_eq!(text(&hub.core, &format!("SELECT resolution_code FROM cases WHERE case_id='{case_id}'")), "recovered");

    // The till learns at its next heartbeat that the hub saved them.
    let hb = t1.core.terminal_heartbeat().unwrap();
    assert_eq!(hb.open_letters.len() as i64, waiting);
    let ans = hub.core.hub_heartbeat(&dev, hb).unwrap();
    assert_eq!(ans["settled"].as_array().unwrap().len() as i64, waiting);
    assert_eq!(t1.core.terminal_after_heartbeat(&ans).unwrap() as i64, waiting);
    assert_eq!(count(&t1.core, "SELECT COUNT(*) FROM sync_dead_letters WHERE status='open'"), 0);
    assert_eq!(count(&t1.core, "SELECT COUNT(*) FROM sync_dead_letters WHERE resolution='settled_on_hub'"), waiting);
}

#[test]
fn a_refused_record_is_closed_only_with_a_reason_and_is_kept() {
    let hub = env();
    let ht = hub.owner_token.clone();
    enable_hub(&hub.core, &ht);
    hub.product("Tea", "7101", 800, 400, 20_000);
    let t1 = pair(&hub, "Till 2", "T02");
    let t2 = pair(&hub, "Till 3", "T03");
    pull_all(&hub.core, &t1.core);
    t1.core.shift_open(&t1.token, 0, &op()).unwrap();
    sell(&t1.core, &t1.token, "7101");
    // Till 3 sends Till 2's sale as its own: refused, and no retry can fix it.
    let (changes, _) = t1.core.terminal_collect_push(1000).unwrap();
    let d2 = t2.core.device().unwrap().device_id;
    let sale_only: Vec<Change> = changes.iter().filter(|c| c.table == "sales").cloned().collect();
    let resp = hub.core.hub_apply_push(&d2, PushRequest { device_id: d2.clone(), changes: sale_only }).unwrap();
    assert_eq!(resp.rejected[0].reason.as_deref(), Some("not_permitted"));
    let dead = text(&hub.core, "SELECT dead_id FROM sync_dead_letters WHERE table_name='sales'");
    assert_eq!(count(&hub.core, &format!("SELECT retryable FROM sync_dead_letters WHERE dead_id='{dead}'")), 0);
    let bulk = hub
        .core
        .sync_retry_dead_letters(
            &ht,
            BulkRetry { dead_ids: vec![], origin: Some(d2.clone()), reason_code: None, table: None, operation_id: op() },
        )
        .unwrap();
    assert_eq!((bulk["eligible"].as_i64(), bulk["ineligible"].as_i64(), bulk["applied"].as_i64()), (Some(0), Some(1), Some(0)));
    assert_eq!(hub.core.sync_retry_dead_letter(&ht, &dead, None).unwrap()["outcome"], "not_retryable");
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM sales"), 0);

    let close = |t: &str, note: &str, confirm: bool| {
        hub.core.sync_close_dead_letter(
            t,
            CloseRequest { dead_id: dead.clone(), note: note.into(), confirm_financial: confirm, operation_id: op() },
        )
    };
    // A cashier cannot; a reason is required; a sale needs confirmation.
    let (_, cashier) = hub.user("Cashier", "role_cashier", "5937");
    assert_eq!(close(&cashier, "Wrong till", true).unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(close(&ht, "no", true).unwrap_err().code, ErrorCode::Validation);
    assert_eq!(close(&ht, "Sent by the wrong till", false).unwrap_err().code, ErrorCode::ApprovalRequired);
    close(&ht, "Sent by the wrong till; Till 2 sends it itself", true).unwrap();
    assert_eq!(close(&ht, "Sent by the wrong till again", true).unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(
        text(&hub.core, &format!("SELECT status || '/' || resolution FROM sync_dead_letters WHERE dead_id='{dead}'")),
        "closed/closed_without_applying"
    );
    // Kept and fixed: no delete, no edit of the record, no reopening.
    for sql in [
        format!("DELETE FROM sync_dead_letters WHERE dead_id='{dead}'"),
        format!("UPDATE sync_dead_letters SET payload_json='{{}}' WHERE dead_id='{dead}'"),
        format!("UPDATE sync_dead_letters SET status='open' WHERE dead_id='{dead}'"),
    ] {
        assert!(hub.core.db.write(|c| Ok(c.execute(&sql, [])?)).is_err(), "{sql}");
    }
    // The audit says who, why and what kind of record, never its contents.
    let audit = text(&hub.core, "SELECT after_json FROM audit_logs WHERE event_type='sync.dead_letter_closed'");
    assert!(audit.contains("Sent by the wrong till") && audit.contains("\"sales\""), "{audit}");
    assert!(!audit.contains("total_minor") && !audit.contains("receipt_number"), "{audit}");
    let closed = page(&hub.core, &ht, json!({ "status": "closed" }));
    assert_eq!(closed["total"], 1);
    assert_eq!(closed["rows"][0]["resolved_by"], "Owner");
}

#[test]
fn a_record_refused_for_a_newer_app_is_settled_by_a_later_send() {
    let hub = env();
    let ht = hub.owner_token.clone();
    enable_hub(&hub.core, &ht);
    let t1 = pair(&hub, "Till 2", "T02");
    pull_all(&hub.core, &t1.core);
    let cust = t1
        .core
        .customer_save(&t1.token, None, serde_json::from_value(json!({ "name": "Layla", "phone": "+97333112233" })).unwrap())
        .unwrap()
        .customer_id;
    let dev = t1.core.device().unwrap().device_id;
    let (changes, scanned) = t1.core.terminal_collect_push(1000).unwrap();
    // The first send carries a column this hub does not know (a newer till).
    let mut newer = changes.clone();
    for c in newer.iter_mut().filter(|c| c.table == "customers") {
        c.row.as_mut().unwrap().insert("loyalty_tier_v99".into(), json!("gold"));
    }
    let resp = hub.core.hub_apply_push(&dev, PushRequest { device_id: dev.clone(), changes: newer }).unwrap();
    assert_eq!(resp.rejected[0].reason.as_deref(), Some("version_mismatch"));
    assert_eq!(count(&hub.core, "SELECT retryable FROM sync_dead_letters WHERE reason_code='version_mismatch'"), 1);
    // The till is brought in line and sends the record again: saved, and
    // the problem settles by itself on both sides.
    let resp = hub.core.hub_apply_push(&dev, PushRequest { device_id: dev.clone(), changes: changes.clone() }).unwrap();
    assert!(resp.rejected.is_empty());
    t1.core.terminal_record_push(scanned, &resp, &changes).unwrap();
    assert_eq!(
        count(
            &hub.core,
            "SELECT COUNT(*) FROM sync_dead_letters WHERE status='resolved' AND resolution='recovered' AND resolved_by IS NULL"
        ),
        1
    );
    assert_eq!(count(&hub.core, &format!("SELECT COUNT(*) FROM customers WHERE customer_id='{cust}'")), 1);
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM audit_logs WHERE event_type='sync.dead_letters_recovered'"), 1);
}

#[test]
fn a_hub_change_a_till_could_not_save_is_retried_by_the_till_itself() {
    let hub = env();
    let ht = hub.owner_token.clone();
    enable_hub(&hub.core, &ht);
    let t1 = pair(&hub, "Till 2", "T02");
    pull_all(&hub.core, &t1.core);
    hub.core.customer_save(&ht, None, serde_json::from_value(json!({ "name": "Huda", "phone": "+97333445566" })).unwrap()).unwrap();
    let dev = t1.core.device().unwrap().device_id;
    let since = t1.core.terminal_sync_settings().unwrap().pull_cursor;
    let resp = hub.core.hub_pull(&dev, PullRequest { device_id: dev.clone(), since_seq: since, limit: Some(200) }).unwrap();
    // A copy of that change for a customer no later pull will carry, held as
    // a problem from five minutes ago (e.g. its dependency arrived since).
    let mut ch = resp.changes.iter().find(|c| c.table == "customers").unwrap().clone();
    let id = amwapos_core::ids::new_id();
    ch.pk.insert("customer_id".into(), json!(id));
    let row = ch.row.as_mut().unwrap();
    row.insert("customer_id".into(), json!(id));
    row.insert("phone".into(), json!("+97333999000"));
    t1.core
        .db
        .write(|c| {
            Ok(c.execute(
                "INSERT INTO sync_dead_letters(dead_id, direction, origin, table_name, row_pk, op, payload_json, error, attempts, status, created_at,
                    last_attempt_at, reason_code, retryable)
                 VALUES ('pull-1','pull',NULL,'customers',?1,'upsert',?2,'FOREIGN KEY constraint failed',1,'open','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z',
                    'missing_dependency',1)",
                rusqlite::params![serde_json::to_string(&ch.pk).unwrap(), serde_json::to_string(&ch).unwrap()],
            )?)
        })
        .unwrap();
    t1.core.terminal_apply_pull(&resp).unwrap();
    assert_eq!(text(&t1.core, "SELECT status || '/' || resolution FROM sync_dead_letters WHERE dead_id='pull-1'"), "resolved/recovered");
    assert_eq!(count(&t1.core, &format!("SELECT COUNT(*) FROM customers WHERE customer_id='{id}'")), 1);
}

#[test]
fn two_people_retrying_the_same_record_save_it_once() {
    let hub = env();
    let ht = hub.owner_token.clone();
    enable_hub(&hub.core, &ht);
    hub.product("Milk", "7201", 600, 300, 20_000);
    let t1 = pair(&hub, "Till 2", "T02");
    pull_all(&hub.core, &t1.core);
    t1.core.shift_open(&t1.token, 0, &op()).unwrap();
    sell(&t1.core, &t1.token, "7201");
    let (changes, scanned) = t1.core.terminal_collect_push(1000).unwrap();
    let dev = t1.core.device().unwrap().device_id;
    let (sales, rest): (Vec<Change>, Vec<Change>) = changes.into_iter().partition(|c| c.table == "sales");
    let resp = hub.core.hub_apply_push(&dev, PushRequest { device_id: dev.clone(), changes: rest.clone() }).unwrap();
    t1.core.terminal_record_push(scanned, &resp, &rest).unwrap();
    hub.core.hub_apply_push(&dev, PushRequest { device_id: dev.clone(), changes: sales }).unwrap();
    let ids: Vec<String> =
        page(&hub.core, &ht, json!({}))["rows"].as_array().unwrap().iter().map(|r| r["dead_id"].as_str().unwrap().into()).collect();
    assert!(!ids.is_empty());
    // Two people with sync.manage at two screens.
    let manager = hub.core.login(&hub.owner_id, OWNER_PIN).unwrap().token;
    let outcomes: Vec<String> = std::thread::scope(|sc| {
        let hs: Vec<_> = [ht.clone(), manager.clone()]
            .into_iter()
            .map(|t| {
                let core = &hub.core;
                let ids = ids.clone();
                sc.spawn(move || {
                    ids.iter()
                        .map(|id| core.sync_retry_dead_letter(&t, id, Some(&op())).unwrap()["outcome"].as_str().unwrap().to_string())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(outcomes.iter().filter(|o| *o == "applied").count(), ids.len());
    assert_eq!(outcomes.iter().filter(|o| *o == "already_finished").count(), ids.len());
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM sale_items"), 1);
    assert_eq!(count(&hub.core, "SELECT COUNT(*) FROM payments"), 1);
}

#[test]
fn old_problem_records_are_kept_unclassified_by_the_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("amwapos.db");
    let c = rusqlite::Connection::open(&path).unwrap();
    amwapos_core::db::migrate_until(&c, &path, 33).unwrap();
    c.execute_batch(
        "INSERT INTO sync_dead_letters(dead_id, direction, origin, table_name, row_pk, op, payload_json, error, attempts, status, created_at, last_attempt_at)
           VALUES ('d1','apply','dev','sales','{\"sale_id\":\"s1\"}','upsert','{}','Missing row data.',3,'open','2026-01-01','2026-01-02'),
                  ('d2','apply','dev','customers','{\"customer_id\":\"c1\"}','upsert','{}','Unknown column',1,'resolved','2026-01-01','2026-01-03');",
    )
    .unwrap();
    amwapos_core::db::migrate_until(&c, &path, amwapos_core::db::latest_schema_version()).unwrap();
    type Row = (String, String, i64, Option<String>, Option<String>, String, i64);
    let rows: Vec<Row> = c
        .prepare(
            "SELECT dead_id, reason_code, retryable, resolution, resolved_at, status, attempts FROM sync_dead_letters ORDER BY dead_id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("d1".into(), "legacy_unclassified".into(), 1, None, None, "open".into(), 3),
            ("d2".into(), "legacy_unclassified".into(), 1, None, None, "resolved".into(), 1),
        ]
    );
}

#[test]
fn many_problem_records_are_paged_and_grouped() {
    let e = env();
    let t = &e.owner_token;
    e.core
        .db
        .write(|c| {
            for i in 0..120 {
                c.execute(
                    "INSERT INTO sync_dead_letters(dead_id, direction, origin, table_name, row_pk, op, payload_json, error, attempts, status, created_at,
                        last_attempt_at, reason_code, retryable)
                     VALUES (?1,'apply','devA','sales',?2,'upsert',?3,'x',1,'open',?4,?4,?5,1)",
                    rusqlite::params![
                        format!("d{i:03}"),
                        format!("{{\"sale_id\":\"s{i}\"}}"),
                        json!({ "seq": i, "table": "sales", "pk": {"sale_id": format!("s{i}")}, "op": "upsert",
                            "row": { "sale_id": format!("s{i}"), "receipt_number": format!("R-{i:04}"), "total_minor": 5000 } }).to_string(),
                        format!("2026-01-01T00:{:02}:{:02}Z", i / 60, i % 60),
                        if i % 3 == 0 { "missing_dependency" } else { "storage_error" },
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let p = page(&e.core, t, json!({ "limit": 50, "offset": 100 }));
    assert_eq!(p["total"], 120);
    assert_eq!(p["rows"].as_array().unwrap().len(), 20);
    assert_eq!(p["groups"].as_array().unwrap().len(), 2);
    let r0 = &p["rows"][0];
    assert!(r0["record_ref"].as_str().unwrap().starts_with("R-"), "{r0}");
    assert!(!r0.to_string().contains("total_minor"));
    let q: DeadQuery = serde_json::from_value(json!({ "reason_code": "missing_dependency", "limit": 500 })).unwrap();
    assert_eq!(e.core.sync_dead_letters(t, q).unwrap()["rows"].as_array().unwrap().len(), 40);
    assert_eq!(
        e.core.sync_dead_letters(t, serde_json::from_value(json!({ "status": "bogus" })).unwrap()).unwrap_err().code,
        ErrorCode::Validation
    );
}

#[test]
fn the_assistant_reads_sync_problems_but_cannot_act_on_them() {
    for cmd in ["sync.retry_dead_letter", "sync.retry_dead_letters", "sync.close_dead_letter"] {
        let reason = amwapos_core::ai_tools::NO_TOOL.iter().find(|(c, _)| *c == cmd).map(|(_, r)| *r).unwrap_or("");
        assert!(reason.starts_with("forbidden"), "{cmd}: {reason}");
    }
}

// ------------------------------------------------------------------ terminal health

fn health_of(hub: &Env, dev: &str) -> Value {
    let h = hub.core.terminals_health(&hub.owner_token).unwrap();
    h["terminals"].as_array().unwrap().iter().find(|t| t["device_id"] == dev).unwrap().clone()
}

#[test]
fn terminal_health_shows_only_what_was_observed() {
    let hub = env();
    let ht = hub.owner_token.clone();
    enable_hub(&hub.core, &ht);
    hub.product("Bread", "7301", 300, 150, 20_000);
    let t1 = pair(&hub, "Till 2", "T02");
    let dev = t1.core.device().unwrap().device_id;

    // Paired, never reported: unknown, nothing filled in.
    let h = health_of(&hub, &dev);
    assert_eq!((h["health"].as_str(), h["connection"].as_str()), (Some("unknown"), Some("never_reported")));
    assert!(h["versions"]["app"]["reported"].is_null() && h["versions"]["protocol"]["reported"].is_null());
    assert!(h["versions"]["schema"]["matches"].is_null());
    assert!(h["sending"]["pending"].is_null());

    // A shift is opened and a sale made; the till sends, then reports.
    pull_all(&hub.core, &t1.core);
    t1.core.shift_open(&t1.token, 0, &op()).unwrap();
    sell(&t1.core, &t1.token, "7301");
    let (resp, _) = push_some(&hub.core, &t1.core, |_| true);
    assert!(resp.rejected.is_empty());
    hub.core.hub_heartbeat(&dev, t1.core.terminal_heartbeat().unwrap()).unwrap();
    let h = health_of(&hub, &dev);
    assert_eq!((h["health"].as_str(), h["connection"].as_str()), (Some("healthy"), Some("online")), "{h}");
    assert_eq!(h["versions"]["protocol"]["reported"], amwapos_core::sync::PROTOCOL_VERSION);
    assert_eq!(h["versions"]["protocol"]["matches"], true);
    assert_eq!(h["versions"]["schema"]["matches"], true);
    assert_eq!(h["versions"]["app"]["reported"], amwapos_core::audit::APP_VERSION);
    assert_eq!(h["sending"]["pending"], 0);
    assert_eq!(h["refused"]["reported_by_till"], 0);
    assert!(h["unknown"].as_array().unwrap().is_empty(), "{h}");
    // The shift comes from the shifts the till sent.
    let shift_no = text(&t1.core, "SELECT shift_number FROM shifts WHERE status='open'");
    assert_eq!(h["shift"]["shift_number"], shift_no);

    // Silent for 20 minutes during that shift: attention, and a case.
    hub.core
        .db
        .write(|c| Ok(c.execute("UPDATE device_heartbeats SET last_seen_at=strftime('%Y-%m-%dT%H:%M:%fZ','now','-20 minutes')", [])?))
        .unwrap();
    let h = health_of(&hub, &dev);
    assert_eq!(h["connection"], "not_seen_recently");
    assert_eq!(h["reasons"], json!(["not_seen_during_shift"]));
    hub.core.ops_evaluate().unwrap();
    assert_eq!(count(&hub.core, &format!("SELECT COUNT(*) FROM cases WHERE kind='terminal_not_seen' AND device_id='{dev}'")), 1);
    assert_eq!(health_of(&hub, &dev)["open_cases"], 1);

    // An old terminal reporting a different database version.
    hub.core
        .db
        .write(|c| {
            Ok(c.execute("UPDATE device_heartbeats SET schema_version=schema_version-1, last_seen_at=?1", [amwapos_core::time::now_str()])?)
        })
        .unwrap();
    let h = health_of(&hub, &dev);
    assert_eq!(h["versions"]["schema"]["matches"], false);
    assert!(h["reasons"].as_array().unwrap().contains(&json!("schema_mismatch")));

    // Store facts sit beside the terminals; a cashier sees none of it.
    let all = hub.core.terminals_health(&ht).unwrap();
    assert!(all["store"]["printing_here"]["failed_24h"].is_number());
    assert_eq!(all["thresholds"]["not_seen_minutes"], 5);
    let (_, cashier) = hub.user("Cashier", "role_cashier", "5937");
    assert_eq!(hub.core.terminals_health(&cashier).unwrap_err().code, ErrorCode::Forbidden);

    // Revoked is shown as revoked, whatever it reported before.
    hub.core.device_set_active(&ht, &dev, false).unwrap();
    assert_eq!(health_of(&hub, &dev)["health"], "revoked");
}

#[test]
fn new_heartbeat_fields_stay_empty_after_the_upgrade_until_reported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("amwapos.db");
    let c = rusqlite::Connection::open(&path).unwrap();
    amwapos_core::db::migrate_until(&c, &path, 34).unwrap();
    c.execute(
        "INSERT INTO device_heartbeats(device_id, last_seen_at, app_version, schema_version, pending_count) VALUES ('d1','2026-01-01T00:00:00Z','1.0',34,7)",
        [],
    )
    .unwrap();
    amwapos_core::db::migrate_until(&c, &path, amwapos_core::db::latest_schema_version()).unwrap();
    let row: (Option<i64>, Option<String>, Option<String>, Option<i64>, Option<String>, i64) = c
        .query_row(
            "SELECT protocol_version, oldest_pending_at, last_sync_ok_at, problem_count, last_heartbeat_at, pending_count FROM device_heartbeats",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap();
    assert_eq!(row, (None, None, None, None, None, 7));
}
