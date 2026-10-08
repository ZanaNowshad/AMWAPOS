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
