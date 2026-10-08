//! Wave 7 of the merchant operating system: operational control. Alert
//! Centre, sync reconciliation, terminal health and per-device credentials
//! (docs/OPERATIONAL_CONTROL.md).

mod common;

use amwapos_core::ops::{self, Condition};
use amwapos_core::ErrorCode;
use common::*;
use serde_json::json;

fn one<T: rusqlite::types::FromSql>(e: &Env, sql: &str) -> T {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn act(
    e: &Env,
    t: &str,
    case_id: &str,
    action: &str,
    note: Option<&str>,
) -> Result<amwapos_core::cases::CaseDetail, amwapos_core::AppError> {
    e.core.case_act(t, serde_json::from_value(json!({ "case_id": case_id, "action": action, "note": note, "operation_id": op() })).unwrap())
}

fn backup_condition(branch: &str) -> Condition {
    Condition {
        key: "backup_overdue".into(),
        kind: "backup_overdue",
        severity: "high",
        title: "The last backup is overdue".into(),
        branch_id: branch.into(),
        entity_type: "store",
        entity_id: "backup".into(),
        device_id: None,
        facts: json!({ "state": "overdue" }),
    }
}

// ------------------------------------------------------------------ migration

#[test]
fn upgrade_keeps_cash_cases_and_copies_the_old_inbox_without_inventing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("amwapos.db");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        amwapos_core::db::migrate_until(&c, &path, 32).unwrap();
        c.execute_batch(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO branches(branch_id, code, name, created_at, updated_at) VALUES ('br','B1','Main','2026-01-01','2026-01-01');
             INSERT INTO cases(case_id, case_number, kind, severity, status, branch_id, entity_type, entity_id, title, facts_json, created_by, created_at, updated_at)
               VALUES ('k1','C-00001','cash_variance','medium','acknowledged','br','shift','s1','Drawer is BHD 6.250 short','{\"variance_minor\":-6250}',NULL,'2026-01-02','2026-01-03');
             INSERT INTO case_events(event_id, case_id, seq, kind, from_status, to_status, note, at) VALUES ('e1','k1',1,'created',NULL,'new',NULL,'2026-01-02'),
               ('e2','k1',2,'status','new','acknowledged','Looking','2026-01-03');
             INSERT INTO ai_alerts(alert_id, kind, day_key, severity, title, detail_json, created_at) VALUES
               ('a1','hub_lag','2026-01-04','danger','Tills are not synchronising','{\"tills_silent\":1}','2026-01-04T08:00:00Z');
             INSERT INTO ai_alerts(alert_id, kind, day_key, severity, title, detail_json, created_at, dismissed_by, dismissed_at) VALUES
               ('a2','refund_spike','2026-01-05','warning','4 refunds today','{\"count\":4}','2026-01-05T09:00:00Z','u1','2026-01-05T10:00:00Z');",
        )
        .unwrap();
    }
    let core =
        amwapos_core::service::AppCore::open(dir.path(), std::sync::Arc::new(amwapos_core::service::MemorySecretStore::default())).unwrap();
    let n = |sql: &str| core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get::<_, i64>(0))?)).unwrap();
    // The cash case is exactly as it was; its history too.
    assert_eq!(
        n("SELECT COUNT(*) FROM cases WHERE case_id='k1' AND kind='cash_variance' AND status='acknowledged' AND severity='medium'
            AND facts_json='{\"variance_minor\":-6250}' AND title='Drawer is BHD 6.250 short' AND source='system'"),
        1
    );
    assert_eq!(n("SELECT COUNT(*) FROM case_events WHERE case_id='k1'"), 2);
    // Each old inbox row is one legacy case with its own words and state.
    assert_eq!(n("SELECT COUNT(*) FROM cases WHERE source='legacy'"), 2);
    assert_eq!(
        n("SELECT COUNT(*) FROM cases WHERE legacy_ref='a1' AND status='new' AND severity='high' AND created_at='2026-01-04T08:00:00Z'"),
        1
    );
    assert_eq!(n("SELECT COUNT(*) FROM cases WHERE legacy_ref='a2' AND status='dismissed' AND resolved_by='u1' AND resolved_at='2026-01-05T10:00:00Z'"), 1);
    // No system case was invented from today's data.
    assert_eq!(n("SELECT COUNT(*) FROM cases WHERE source='system' AND kind<>'cash_variance'"), 0);
    assert_eq!(n("SELECT COUNT(*) FROM alert_conditions"), 0);
    // The old inbox is read-only.
    let w = core.db.write(|c| {
        Ok(c.execute(
            "INSERT INTO ai_alerts(alert_id, kind, day_key, severity, title, detail_json, created_at) VALUES ('x','hub_lag','d','info','t','{}','x')",
            [],
        )?)
    });
    assert!(w.is_err());
    // The rebuilt history still points at the cases table, and every key holds.
    let sql: String =
        core.db.read(|c| Ok(c.query_row("SELECT sql FROM sqlite_master WHERE name='case_events'", [], |r| r.get(0))?)).unwrap();
    assert!(sql.contains("REFERENCES \"cases\"") || sql.contains("REFERENCES cases("), "{sql}");
    assert!(!sql.contains("cases_new"));
    let fk: i64 =
        core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check('case_events')", [], |r| r.get(0))?)).unwrap();
    assert_eq!(fk, 0);
    // Finished cases stay finished.
    assert!(core.db.write(|c| Ok(c.execute("UPDATE cases SET status='new' WHERE legacy_ref='a2'", [])?)).is_err());
}

#[test]
fn nothing_writes_to_the_old_inbox_any_more() {
    let e = env();
    assert_eq!(e.core.ai_anomaly_scan().unwrap(), 0);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM ai_alerts"), 0);
    let err = e.core.ai_alert_dismiss(&e.owner_token, "01J0000000000000000000000A").unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
}

// ------------------------------------------------------------------ system cases

#[test]
fn a_system_case_moves_through_the_same_lifecycle_and_a_person_s_decision_sticks() {
    let e = env();
    let t = &e.owner_token;
    let branch: String = one(&e, "SELECT branch_id FROM branches LIMIT 1");
    let run = |min: i64, on: bool| {
        let now = amwapos_core::time::now() + chrono::Duration::minutes(min);
        let conds = if on { vec![backup_condition(&branch)] } else { vec![] };
        e.core.db.write(|c| ops::reconcile(c, now, &["backup_overdue"], &conds)).unwrap()
    };
    assert_eq!(run(0, true).opened, 1);
    assert_eq!(run(1, true).opened, 0);
    let page = e.core.cases_query(t, serde_json::from_value(json!({ "status": "needs_attention", "source": "system" })).unwrap()).unwrap();
    assert_eq!(page.total, 1);
    let case = &page.rows[0];
    assert_eq!((case.kind.as_str(), case.severity.as_str(), case.source.as_str()), ("backup_overdue", "high", "system"));
    assert_eq!(case.link.as_deref(), Some("/admin/backups"));
    assert_eq!(case.condition_active, Some(true));
    assert!(case.created_by_name.is_none(), "a system case names no person as its author");
    let d = act(&e, t, &case.case_id, "acknowledge", None).unwrap();
    assert_eq!(d.case.status, "acknowledged");
    // Dismissed by a person while the backup is still overdue: no new case.
    assert_eq!(act(&e, t, &case.case_id, "dismiss", None).unwrap_err().code, ErrorCode::Validation, "a reason is required");
    act(&e, t, &case.case_id, "dismiss", Some("Backup drive is at the shop tomorrow")).unwrap();
    for m in 2..20 {
        assert_eq!(run(m, true).opened, 0);
    }
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM cases WHERE kind='backup_overdue'"), 1);
    // A backup is made, then it is overdue again later: a new case.
    run(30, false);
    assert_eq!(run(90, true).opened, 1);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM cases WHERE kind='backup_overdue'"), 2);
    // The auto-resolution of the second one is a system event, attributed to no person.
    run(120, false);
    let id: String = one(&e, "SELECT case_id FROM cases WHERE kind='backup_overdue' AND status='resolved'");
    let d = e.core.case_get(t, &id).unwrap();
    let last = d.events.last().unwrap();
    assert_eq!((last.to_status.as_deref(), last.user_name.as_deref()), (Some("resolved"), None));
    assert_eq!(last.evidence.as_ref().unwrap()["actor"], "system");
    assert!(one::<i64>(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type='case.auto_resolved'") >= 1);
}

#[test]
fn cashiers_cannot_see_or_handle_the_alert_centre() {
    let e = env();
    let (_, cashier) = e.user("Sara", "role_cashier", "1357");
    let q = serde_json::from_value(json!({})).unwrap();
    assert_eq!(e.core.cases_query(&cashier, q).unwrap_err().code, ErrorCode::Forbidden);
}

// ------------------------------------------------------------------ the checks

fn terminal(e: &Env, code: &str) -> String {
    let branch: String = one(e, "SELECT branch_id FROM branches LIMIT 1");
    let id = format!("01TERM{code}000000000000000");
    e.core
        .db
        .write(|c| {
            Ok(c.execute(
                "INSERT INTO devices(device_id, branch_id, name, device_code, operating_mode, active, activated_at) VALUES (?1,?2,?3,?4,'terminal',1,'2026-01-01')",
                rusqlite::params![id, branch, format!("Till {code}"), code],
            )?)
        })
        .unwrap();
    id
}

fn heartbeat(e: &Env, device: &str, minutes_ago: i64, pending: i64, schema: i64) {
    let seen = amwapos_core::time::fmt(amwapos_core::time::now() - chrono::Duration::minutes(minutes_ago));
    e.core
        .db
        .write(|c| {
            Ok(c.execute(
                "INSERT INTO device_heartbeats(device_id, last_seen_at, schema_version, pending_count, app_version) VALUES (?1,?2,?3,?4,'0.1.0')
                 ON CONFLICT(device_id) DO UPDATE SET last_seen_at=?2, schema_version=?3, pending_count=?4",
                rusqlite::params![device, seen, schema, pending],
            )?)
        })
        .unwrap();
}

fn dead_letters(e: &Env, origin: &str, table: &str, n: usize) {
    e.core
        .db
        .write(|c| {
            for i in 0..n {
                c.execute(
                    "INSERT INTO sync_dead_letters(dead_id, direction, origin, table_name, row_pk, op, payload_json, error, attempts, status, created_at, last_attempt_at)
                     VALUES (?1,'apply',?2,?3,?4,'upsert','{}','FOREIGN KEY constraint failed',1,'open','2026-10-08T08:00:00Z','2026-10-08T08:00:00Z')",
                    rusqlite::params![format!("{origin}-{table}-{i}"), origin, table, format!("{{\"id\":\"{i}\"}}")],
                )?;
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn four_hundred_bad_records_from_one_till_are_one_case_that_recovers() {
    let e = env();
    let t = &e.owner_token;
    let till = terminal(&e, "T02");
    dead_letters(&e, &till, "sales", 400);
    dead_letters(&e, &till, "customers", 3);
    for _ in 0..5 {
        e.core.ops_evaluate().unwrap();
    }
    let n: i64 = one(&e, "SELECT COUNT(*) FROM cases WHERE kind='sync_failures'");
    assert_eq!(n, 1);
    let (sev, title, link): (String, String, String) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT severity, title, link FROM cases WHERE kind='sync_failures'", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?)
        })
        .unwrap();
    assert_eq!(sev, "high", "sales are money");
    assert_eq!(title, "403 records from Till T02 could not be saved: the reason was not recorded before this update");
    assert_eq!(link, "/admin/sync-reconciliation");
    // The Dashboard shows the case and does not compute its own sync alert.
    let d = e.core.dashboard(t).unwrap();
    let att = d["attention"].as_array().unwrap();
    assert!(att.iter().any(|a| a["kind"] == "case"
        && a["case_kind"] == "sync_failures"
        && a["link"].as_str().unwrap().starts_with("/admin/cases?case=")));
    assert!(!att.iter().any(|a| a["kind"] == "sync"));
    // The records are dealt with: the case resolves itself, with a system event.
    e.core.db.write(|c| Ok(c.execute("UPDATE sync_dead_letters SET status='resolved'", [])?)).unwrap();
    let r = e.core.ops_evaluate().unwrap();
    assert_eq!(r.resolved, 1);
    assert_eq!(
        one::<i64>(&e, "SELECT COUNT(*) FROM cases WHERE kind='sync_failures' AND status='resolved' AND resolution_code='recovered'"),
        1
    );
}

#[test]
fn a_till_switched_off_after_closing_is_not_an_incident_but_one_silent_during_a_shift_is() {
    let e = env();
    let t = &e.owner_token;
    let till = terminal(&e, "T03");
    let branch: String = one(&e, "SELECT branch_id FROM branches LIMIT 1");
    let hub: String = one(&e, "SELECT device_id FROM devices WHERE operating_mode<>'terminal' LIMIT 1");
    // Never reported: unknown, not "offline", no case.
    let measure = |min: i64| {
        let now = amwapos_core::time::now() + chrono::Duration::minutes(min);
        e.core.db.read(|c| ops::measure(c, now, &hub, None)).unwrap()
    };
    assert!(measure(0).iter().all(|c| c.kind != "terminal_not_seen"));
    heartbeat(&e, &till, 40, 0, amwapos_core::db::latest_schema_version());
    assert!(measure(0).iter().all(|c| c.kind != "terminal_not_seen"), "no open shift: switched off after closing");
    // An open shift on that till: 40 minutes of silence is an incident.
    e.open_shift(t, 0);
    e.core.db.write(|c| Ok(c.execute("UPDATE shifts SET device_id=?1 WHERE status='open'", [&till])?)).unwrap();
    let conds = measure(0);
    let c = conds.iter().find(|c| c.kind == "terminal_not_seen").unwrap();
    assert_eq!((c.severity, c.device_id.as_deref()), ("medium", Some(till.as_str())));
    assert!(c.title.contains("Till T03 has not been seen for 40 minutes"), "{}", c.title);
    assert_eq!(c.branch_id, branch);
    // After 2 hours it is high.
    assert_eq!(measure(90).iter().find(|c| c.kind == "terminal_not_seen").unwrap().severity, "high");
    // A different database version is reported as such, never guessed from the app version.
    heartbeat(&e, &till, 1, 0, amwapos_core::db::latest_schema_version() - 1);
    let conds = measure(0);
    assert!(conds.iter().any(|c| c.kind == "terminal_incompatible"));
    assert!(conds.iter().all(|c| c.kind != "terminal_not_seen"));
    // A backlog while alive and not sending.
    heartbeat(&e, &till, 1, 120, amwapos_core::db::latest_schema_version());
    assert!(measure(0).iter().any(|c| c.kind == "terminal_backlog" && c.title == "Till T03 has 120 records waiting to send"));
    heartbeat(&e, &till, 1, 3, amwapos_core::db::latest_schema_version());
    assert!(measure(0).iter().all(|c| c.kind != "terminal_backlog"), "a few records waiting is normal");
}

#[test]
fn backups_prints_payments_and_rider_cash_open_cases_from_evidence_only() {
    let e = env();
    // A new store has no backup yet: overdue, one case.
    e.core.ops_evaluate().unwrap();
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM cases WHERE kind='backup_overdue' AND status='new'"), 1);
    // A failed print job is evidence; a printed one after it clears it.
    let now = amwapos_core::time::now_str();
    e.core
        .db
        .write(|c| {
            c.execute(
                "INSERT INTO print_jobs(job_id, kind, status, attempts, last_error, created_at, updated_at) VALUES ('p1','sale','failed',3,'paper out',?1,?1)",
                [&now],
            )?;
            Ok(())
        })
        .unwrap();
    e.core.ops_evaluate().unwrap();
    let title: String = one(&e, "SELECT title FROM cases WHERE kind='print_failures'");
    assert_eq!(title, "1 print jobs failed in the last 24 hours");
    assert!(!title.to_lowercase().contains("offline"), "no printer state is invented");
    let later = amwapos_core::time::fmt(amwapos_core::time::now() + chrono::Duration::seconds(5));
    e.core
        .db
        .write(|c| {
            Ok(c.execute(
                "INSERT INTO print_jobs(job_id, kind, status, attempts, created_at, updated_at) VALUES ('p2','sale','printed',1,?1,?1)",
                [&later],
            )?)
        })
        .unwrap();
    e.core.ops_evaluate().unwrap();
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM cases WHERE kind='print_failures' AND status='resolved'"), 1);
    // Two runs at the same time still make one case per incident.
    std::thread::scope(|sc| {
        let a = sc.spawn(|| e.core.ops_evaluate().unwrap());
        let b = sc.spawn(|| e.core.ops_evaluate().unwrap());
        a.join().unwrap();
        b.join().unwrap();
    });
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM cases WHERE kind='backup_overdue' AND status NOT IN ('resolved','dismissed')"), 1);
}

#[test]
fn a_terminal_never_writes_cases() {
    let hub = env();
    let _ = hub;
    // A terminal-mode core: ops_evaluate is a no-op (cases live on the hub).
    let e = env();
    e.core
        .db
        .write(|c| {
            c.execute("UPDATE settings SET value_json=json_set(value_json,'$.mode','terminal') WHERE key='local.device'", [])?;
            Ok(())
        })
        .unwrap();
    let r = amwapos_core::service::AppCore::open(e.dir.path(), std::sync::Arc::new(amwapos_core::service::MemorySecretStore::default()))
        .unwrap()
        .ops_evaluate()
        .unwrap();
    assert_eq!(r, ops::Reconciled::default());
}
