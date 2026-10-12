//! Order event feed and sync targets (docs/WHATSAPP_AUTOPILOT.md, "Sync"):
//! every order and delivery change is one event, in order, with a per-order
//! version; each target reads it with its own cursor; failures retry, then
//! park without stopping the rest; parked events can be sent again.

mod common;

use amwapos_core::order_sync::{SinkInput, SyncOutcome, MAX_ATTEMPTS};
use amwapos_core::ErrorCode;
use common::*;
use serde_json::json;

fn setup() -> (Env, String, String) {
    let e = env();
    let t = &e.owner_token;
    let mut f = e.core.settings_get(t, "features").unwrap();
    f["orders.digital"] = json!(true);
    e.core.settings_save(t, "features", f).unwrap();
    let milk = e.product("Fresh Milk", "9201", 450, 300, 50_000);
    let bread = e.product("Brown Bread", "9202", 300, 200, 50_000);
    e.open_shift(t, 0);
    (e, milk, bread)
}

fn events(e: &Env) -> Vec<(String, i64, String)> {
    e.core
        .db
        .read(|c| {
            let mut st = c.prepare("SELECT kind, version, state_key FROM order_events ORDER BY seq")?;
            let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(r)
        })
        .unwrap()
}

fn new_order(e: &Env, milk: &str, bread: &str, delivery: bool) -> String {
    e.core
        .order_save(
            &e.owner_token,
            None,
            serde_json::from_value(json!({
                "channel": "phone", "phone": "33334444", "delivery_wanted": delivery, "address": "Road 12, Manama",
                "lines": [{ "product_id": milk, "qty_milli": 2000 }, { "product_id": bread, "qty_milli": 1000 }]
            }))
            .unwrap(),
        )
        .unwrap()
        .order_id
}

fn sell(e: &Env, order: &str) -> String {
    let t = &e.owner_token;
    let cart = e.core.order_convert(t, order, &op()).unwrap();
    let total = cart.totals.total_minor;
    e.core
        .pos_finalize(
            t,
            amwapos_core::sales::FinalizeRequest {
                cart_id: cart.cart_id.clone().unwrap(),
                operation_id: op(),
                tenders: vec![amwapos_core::pricing::TenderInput { method: "cash".into(), amount_minor: total, reference: None }],
                approval_token: None,
                expected_total_minor: Some(total),
                fulfilment: None,
            },
        )
        .unwrap();
    e.core.order_get(t, order).unwrap().delivery_id.unwrap()
}

fn webhook(e: &Env) -> String {
    let r = e
        .core
        .order_sync_create(
            &e.owner_token,
            SinkInput { name: "ERP".into(), kind: "webhook".into(), url: "http://127.0.0.1:9/hook".into(), ..Default::default() },
        )
        .unwrap();
    r["target"]["sink_id"].as_str().unwrap().to_string()
}

#[test]
fn every_state_change_is_one_event_in_order_with_a_version_per_order() {
    let (e, milk, bread) = setup();
    let t = &e.owner_token;
    let o = new_order(&e, &milk, &bread, true);
    e.core.order_confirm(t, &o).unwrap();
    // An update that changes nothing the feed tracks adds nothing.
    e.core.db.write(|c| Ok(c.execute("UPDATE digital_orders SET status=status, note='x' WHERE order_id=?1", [&o])?)).unwrap();
    let d = sell(&e, &o);
    e.core.delivery_update(t, &d, Some("dispatched".into()), None, None, None).unwrap();
    e.core.delivery_update(t, &d, Some("delivered".into()), None, None, None).unwrap();
    let ev = events(&e);
    let kinds: Vec<&str> = ev.iter().map(|x| x.0.as_str()).collect();
    assert_eq!(
        kinds,
        vec!["order.draft", "order.confirmed", "delivery.pending", "order.converted", "delivery.dispatched", "delivery.delivered"],
        "{ev:?}"
    );
    let versions: Vec<i64> = ev.iter().map(|x| x.1).collect();
    assert_eq!(versions, vec![1, 2, 3, 4, 5, 6], "one counter per order across order and delivery events");
    // A payment-only change is its own event.
    let o2 = new_order(&e, &milk, &bread, false);
    e.core.order_set_payment(t, &o2, "recorded").unwrap();
    assert_eq!(events(&e).last().unwrap().0, "order.payment_recorded");
    assert_eq!(events(&e).last().unwrap().1, 2);
}

#[test]
fn the_payload_carries_lines_with_pos_prices_the_sale_and_the_delivery() {
    let (e, milk, bread) = setup();
    let o = new_order(&e, &milk, &bread, true);
    e.core.order_confirm(&e.owner_token, &o).unwrap();
    sell(&e, &o);
    let seq: i64 = e.core.db.read(|c| Ok(c.query_row("SELECT MAX(seq) FROM order_events", [], |r| r.get(0))?)).unwrap();
    let p = e.core.db.read(|c| amwapos_core::order_sync::event_payload(c, seq)).unwrap();
    assert_eq!(p["kind"], "order.converted");
    let lines = p["order"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["unit_price_minor"], 450);
    assert_eq!(lines[0]["line_total_minor"], 900);
    assert_eq!(p["order"]["total_minor"], 1200);
    assert_eq!(p["order"]["sale"]["total_minor"], 1200, "the sale is the authoritative amount");
    assert!(p["order"]["sale"]["receipt_number"].is_string());
    assert_eq!(p["delivery"]["status"], "pending");
    assert!(p["event_id"].as_str().unwrap().len() == 32 && p["version"] == 4);
}

#[test]
fn a_target_starts_at_the_end_retries_then_parks_and_moves_on() {
    let (e, milk, bread) = setup();
    new_order(&e, &milk, &bread, false);
    let sink = webhook(&e);
    // No history: the order made before the target existed is not sent.
    assert!(e.core.order_sync_next(&sink).unwrap().is_none());
    let o1 = new_order(&e, &milk, &bread, false);
    new_order(&e, &milk, &bread, false);
    let job = e.core.order_sync_next(&sink).unwrap().unwrap();
    assert_eq!(job.payload["order"]["order_id"], o1);
    // A temporary failure waits before the same event is tried again.
    e.core.order_sync_complete(&job, SyncOutcome::Retry { error: "HTTP 503".into(), after_s: None }).unwrap();
    assert!(e.core.order_sync_next(&sink).unwrap().is_none(), "waiting");
    let unwait = || e.core.db.write(|c| Ok(c.execute("UPDATE sync_sinks SET next_at=NULL", [])?)).unwrap();
    for _ in 1..MAX_ATTEMPTS {
        unwait();
        let again = e.core.order_sync_next(&sink).unwrap().unwrap();
        assert_eq!(again.event_seq, job.event_seq, "the same event, in order");
        e.core.order_sync_complete(&again, SyncOutcome::Retry { error: "HTTP 503".into(), after_s: None }).unwrap();
    }
    // After the last attempt it is parked and the target moves on.
    unwait();
    let next = e.core.order_sync_next(&sink).unwrap().unwrap();
    assert!(next.event_seq > job.event_seq);
    let failures = e.core.order_sync_failures(&e.owner_token, &sink).unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["event_seq"], job.event_seq);
    e.core.order_sync_complete(&next, SyncOutcome::Delivered).unwrap();
    // A refusal parks at once.
    new_order(&e, &milk, &bread, false);
    let refused = e.core.order_sync_next(&sink).unwrap().unwrap();
    e.core.order_sync_complete(&refused, SyncOutcome::Refused { error: "HTTP 400".into() }).unwrap();
    assert_eq!(e.core.order_sync_failures(&e.owner_token, &sink).unwrap().len(), 2);
    // Sent again on request: parked events first, then the cursor.
    assert_eq!(e.core.order_sync_failure_action(&e.owner_token, &sink, None, "retry").unwrap(), 2);
    let r1 = e.core.order_sync_next(&sink).unwrap().unwrap();
    assert!(r1.from_parked && r1.event_seq == job.event_seq);
    e.core.order_sync_complete(&r1, SyncOutcome::Delivered).unwrap();
    let r2 = e.core.order_sync_next(&sink).unwrap().unwrap();
    assert!(r2.from_parked && r2.event_seq == refused.event_seq);
    e.core.order_sync_failure_action(&e.owner_token, &sink, Some(r2.event_seq), "dismiss").unwrap();
    assert!(e.core.order_sync_failures(&e.owner_token, &sink).unwrap().is_empty());
    let row = &e.core.order_sync_list(&e.owner_token).unwrap()["targets"][0];
    assert_eq!(row["behind"], 0, "{row}");
    assert_eq!(row["delivered"], 2);
}

#[test]
fn a_disabled_target_waits_and_targets_are_independent() {
    let (e, milk, bread) = setup();
    let a = webhook(&e);
    let b = e
        .core
        .order_sync_create(
            &e.owner_token,
            SinkInput { name: "Sheet".into(), kind: "csv".into(), path: "C:\\x\\orders.csv".into(), ..Default::default() },
        )
        .unwrap()["target"]["sink_id"]
        .as_str()
        .unwrap()
        .to_string();
    new_order(&e, &milk, &bread, false);
    e.core
        .order_sync_update(
            &e.owner_token,
            &a,
            SinkInput { name: "ERP".into(), url: "http://127.0.0.1:9/hook".into(), enabled: Some(false), ..Default::default() },
        )
        .unwrap();
    assert!(e.core.order_sync_next(&a).unwrap().is_none(), "off");
    let jb = e.core.order_sync_next(&b).unwrap().unwrap();
    e.core.order_sync_complete(&jb, SyncOutcome::Delivered).unwrap();
    assert_eq!(e.core.order_sync_due().unwrap(), Vec::<String>::new());
    e.core
        .order_sync_update(
            &e.owner_token,
            &a,
            SinkInput { name: "ERP".into(), url: "http://127.0.0.1:9/hook".into(), enabled: Some(true), ..Default::default() },
        )
        .unwrap();
    assert_eq!(e.core.order_sync_due().unwrap(), vec![a.clone()], "back on, it catches up from where it was");
}

#[test]
fn only_settings_managers_configure_targets_and_the_secret_is_never_in_the_database() {
    let (e, _, _) = setup();
    let (_, cashier) = e.user("Cashier", "role_cashier", "7391");
    let input = SinkInput { name: "ERP".into(), kind: "webhook".into(), url: "https://erp.example/hook".into(), ..Default::default() };
    assert_eq!(e.core.order_sync_create(&cashier, input.clone()).unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(e.core.order_sync_list(&cashier).unwrap_err().code, ErrorCode::Forbidden);
    let r = e.core.order_sync_create(&e.owner_token, input).unwrap();
    let secret = r["secret"].as_str().unwrap().to_string();
    assert_eq!(secret.len(), 64);
    let id = r["target"]["sink_id"].as_str().unwrap();
    assert_eq!(e.core.secrets.get(&amwapos_core::order_sync::secret_slot(id)).unwrap().as_deref(), Some(secret.as_str()));
    let db = std::fs::read(e.dir.path().join("amwapos.db")).unwrap();
    let wal = std::fs::read(e.dir.path().join("amwapos.db-wal")).unwrap_or_default();
    let hay = [db, wal].concat();
    assert!(!hay.windows(secret.len()).any(|w| w == secret.as_bytes()), "the signing secret is only in the credential store");
    // Bad addresses and file names are refused.
    for (kind, url, path) in [("webhook", "ftp://x", ""), ("webhook", "", ""), ("csv", "", "C:\\x\\orders.txt"), ("ftp", "", "")] {
        let bad = SinkInput { name: "X".into(), kind: kind.into(), url: url.into(), path: path.into(), ..Default::default() };
        assert_eq!(e.core.order_sync_create(&e.owner_token, bad).unwrap_err().code, ErrorCode::Validation);
    }
    e.core.order_sync_delete(&e.owner_token, id).unwrap();
    assert!(e.core.secrets.get(&amwapos_core::order_sync::secret_slot(id)).unwrap().is_none());
}

#[test]
fn old_delivered_events_are_pruned_but_parked_ones_are_kept() {
    let (e, milk, bread) = setup();
    let sink = webhook(&e);
    new_order(&e, &milk, &bread, false);
    new_order(&e, &milk, &bread, false);
    let j1 = e.core.order_sync_next(&sink).unwrap().unwrap();
    e.core.order_sync_complete(&j1, SyncOutcome::Refused { error: "HTTP 400".into() }).unwrap();
    let j2 = e.core.order_sync_next(&sink).unwrap().unwrap();
    e.core.order_sync_complete(&j2, SyncOutcome::Delivered).unwrap();
    e.core.db.write(|c| Ok(c.execute("UPDATE order_events SET created_at='2020-01-01T00:00:00.000Z'", [])?)).unwrap();
    assert_eq!(e.core.order_events_prune().unwrap(), 1);
    let left: Vec<i64> = e
        .core
        .db
        .read(|c| {
            let mut st = c.prepare("SELECT seq FROM order_events")?;
            let r = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<i64>, _>>()?;
            Ok(r)
        })
        .unwrap();
    assert_eq!(left, vec![j1.event_seq], "the parked event stays until it is sent or dismissed");
}
