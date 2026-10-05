//! Wave 2 of the merchant operating system: the trading day (X, checks, Z),
//! late records, registers and drawers, cash differences as cases, and the
//! failure cases around closing. Every test checks an invariant.

mod common;

use amwapos_core::dayclose::CloseRequest;
use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::{FinalizeRequest, Fulfilment, SaleResult};
use amwapos_core::ErrorCode;
use common::*;
use serde_json::{json, Value};

fn sell_as(e: &Env, t: &str, barcode: &str, qty: i64, tender: &str) -> SaleResult {
    let cart = e.core.pos_scan(t, barcode, Some(qty)).unwrap().cart;
    let total = cart.totals.total_minor;
    e.core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![TenderInput { method: tender.into(), amount_minor: total, reference: None }],
                approval_token: None,
                expected_total_minor: Some(total),
                fulfilment: None,
            },
        )
        .unwrap()
}

fn one<T: rusqlite::types::FromSql>(e: &Env, sql: &str) -> T {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn today(e: &Env) -> String {
    e.core.db.read(|c| amwapos_core::time::business_date(amwapos_core::time::now(), &amwapos_core::time::day(c)?)).unwrap()
}

fn days_before(date: &str, n: i64) -> String {
    (chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap() - chrono::Duration::days(n)).to_string()
}

fn close_req(date: &str, op_id: &str) -> CloseRequest {
    CloseRequest { business_date: date.into(), branch_id: None, operation_id: op_id.into(), acknowledge_warnings: true }
}

/// Copy a row with some columns replaced (a record as another computer, or
/// another day, would have written it).
fn clone_row(c: &rusqlite::Connection, table: &str, pk: &str, id: &str, set: &[(&str, Value)]) -> rusqlite::Result<()> {
    let mut st = c.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
    let cols: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let exprs: Vec<String> = cols
        .iter()
        .map(|col| match set.iter().find(|(k, _)| k == col) {
            Some((_, Value::String(s))) => format!("'{}'", s.replace('\'', "''")),
            Some((_, Value::Number(n))) => n.to_string(),
            Some((_, _)) => "NULL".into(),
            None => format!("\"{col}\""),
        })
        .collect();
    c.execute(
        &format!(
            "INSERT INTO {table} ({}) SELECT {} FROM {table} WHERE {pk}=?1",
            cols.iter().map(|x| format!("\"{x}\"")).collect::<Vec<_>>().join(","),
            exprs.join(",")
        ),
        [id],
    )?;
    Ok(())
}

/// The sale `sale_id` as if it had been made on `date` at `at` and reached
/// this computer now (sale, lines and payments, new ids). Returns its id.
fn arrive(e: &Env, sale_id: &str, date: &str, at: &str) -> String {
    let n = ulid::Ulid::new().to_string();
    let new_id = format!("S{n}");
    e.core
        .db
        .write(|tx| {
            clone_row(
                tx,
                "sales",
                "sale_id",
                sale_id,
                &[
                    ("sale_id", json!(new_id)),
                    ("receipt_number", json!(format!("T09-{}", &n[18..]))),
                    ("operation_id", json!(format!("op{n}"))),
                    ("business_date", json!(date)),
                    ("completed_at", json!(at)),
                    ("created_at", json!(at)),
                    ("cart_id", Value::Null),
                ],
            )?;
            let items: Vec<String> = tx
                .prepare("SELECT sale_item_id FROM sale_items WHERE sale_id=?1")?
                .query_map([sale_id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for i in items {
                clone_row(tx, "sale_items", "sale_item_id", &i, &[("sale_item_id", json!(format!("{i}{n}"))), ("sale_id", json!(new_id))])?;
            }
            let pays: Vec<String> = tx
                .prepare("SELECT payment_id FROM payments WHERE sale_id=?1")?
                .query_map([sale_id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for p in pays {
                clone_row(tx, "payments", "payment_id", &p, &[("payment_id", json!(format!("{p}{n}"))), ("sale_id", json!(new_id))])?;
            }
            Ok(())
        })
        .unwrap();
    new_id
}

/// Everything a write would change: row counts of every table, the change
/// log and the audit chain.
fn fingerprint(e: &Env) -> Vec<(String, i64)> {
    e.core
        .db
        .read(|c| {
            let names: Vec<String> = c
                .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            let mut out = vec![];
            for n in names {
                out.push((n.clone(), c.query_row(&format!("SELECT COUNT(*) FROM \"{n}\""), [], |r| r.get(0))?));
            }
            out.push(("outbox_seq".into(), c.query_row("SELECT COALESCE(MAX(seq),0) FROM sync_outbox", [], |r| r.get(0))?));
            out.push(("data_version".into(), c.query_row("SELECT total_changes()", [], |r| r.get(0))?));
            Ok(out)
        })
        .unwrap()
}

fn close_shift(e: &Env, t: &str, counted: i64) {
    let id = e.core.shift_current(t).unwrap().unwrap().shift_id;
    e.core.shift_close(t, &id, serde_json::from_value(json!({ "counted_cash_minor": counted, "operation_id": op() })).unwrap()).unwrap();
}

#[test]
fn x_report_shows_the_day_and_changes_nothing() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    e.product("Laban 1L", "7001", 1_100, 600, 100_000);
    let a = sell_as(&e, t, "7001", 2000, "cash");
    sell_as(&e, t, "7001", 1000, "card");
    let v = sell_as(&e, t, "7001", 1000, "cash");
    e.core
        .sale_void(
            t,
            amwapos_core::voids::VoidRequest {
                sale_id: v.sale_id.clone(),
                reason: "Rang up twice".into(),
                operation_id: op(),
                approval_token: None,
            },
        )
        .unwrap();
    let item: String = one(&e, &format!("SELECT sale_item_id FROM sale_items WHERE sale_id='{}'", a.sale_id));
    e.core
        .refund_create(
            t,
            serde_json::from_value(json!({ "sale_id": a.sale_id, "reason": "Damaged", "operation_id": op(),
                "lines": [{ "sale_item_id": item, "qty_milli": 1000, "restock": true }] }))
            .unwrap(),
        )
        .unwrap();

    let before = fingerprint(&e);
    let x = e.core.day_x(t, None, None).unwrap();
    for _ in 0..5 {
        assert_eq!(e.core.day_x(t, None, None).unwrap().total, x.total, "X is repeatable");
        e.core.day_checks(t, None, None).unwrap();
        e.core.day_opening(t).unwrap();
    }
    e.core.day_x_pdf(t, None, None).unwrap();
    assert_eq!(fingerprint(&e), before, "X, the checks and the opening list write nothing");

    let d = &x.day;
    assert_eq!((d.sale_count, d.sales_minor), (3, 4_400));
    assert_eq!((d.refund_count, d.refunds_minor, d.void_count, d.voids_minor), (1, 1_100, 1, 1_100));
    assert_eq!(d.net_sales_minor, 2_200);
    assert_eq!(d.tax_minor, 200, "10% VAT included in 2.200");
    assert_eq!(d.net_ex_vat_minor, 2_000);
    let cash = d.tenders.iter().find(|t| t.method == "cash").unwrap();
    assert_eq!((cash.sales_minor, cash.refunds_minor, cash.net_minor), (3_300, 2_200, 1_100));
    assert_eq!(d.cash_sales_minor + d.non_cash_sales_minor, d.net_sales_minor, "tenders add up to net sales");
    assert_eq!(x.after_close.sale_count, 0);
    // The open drawer is in X with its expected cash; X never counts it.
    assert_eq!(x.drawers.len(), 1);
    assert_eq!(x.drawers[0].status, "open");
    assert_eq!(x.cash.expected_cash_minor, 10_000 + 3_300 - 2_200);
    assert_eq!(x.cash.open_drawers, 1);
}

#[test]
fn a_z_close_is_made_once_and_retries_return_it() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    e.product("Bread", "7002", 300, 150, 100_000);
    sell_as(&e, t, "7002", 1000, "cash");
    let d = today(&e);

    // An open shift on this computer blocks; nothing is written.
    let err = e.core.day_close(t, close_req(&d, &op())).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.details.unwrap()["checks"].as_array().unwrap().iter().any(|c| c["code"] == "open_shift" && c["level"] == "blocking"));
    close_shift(&e, t, 10_300);

    // Warnings need a person to confirm.
    let mut unconfirmed = close_req(&d, &op());
    unconfirmed.acknowledge_warnings = false;
    let checks = e.core.day_checks(t, Some(d.clone()), None).unwrap();
    assert!(checks.can_close);
    if checks.needs_acknowledgement {
        assert_eq!(e.core.day_close(t, unconfirmed).unwrap_err().code, ErrorCode::Conflict);
    }

    let op_id = op();
    let z = e.core.day_close(t, close_req(&d, &op_id)).unwrap();
    assert!(z.verified);
    assert_eq!((z.report.total.sale_count, z.report.total.net_sales_minor), (1, 300));
    assert_eq!(z.report.cash.counted_cash_minor, 10_300);
    // Response lost / double click: the same operation returns the same close.
    let again = e.core.day_close(t, close_req(&d, &op_id)).unwrap();
    assert_eq!((again.close_id.as_str(), again.sha256.as_str()), (z.close_id.as_str(), z.sha256.as_str()));
    // The same operation id with another day is refused.
    let other = e.core.day_close(t, close_req(&days_before(&d, 1), &op_id)).unwrap_err();
    assert_eq!(other.code, ErrorCode::IdempotencyMismatch);
    // A second close of the same day (new operation) is refused.
    let dup = e.core.day_close(t, close_req(&d, &op())).unwrap_err();
    assert_eq!(dup.code, ErrorCode::Conflict);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM day_closes"), 1);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM day_close_items WHERE ref_kind='sale'"), 1);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type='day.closed'"), 1);
}

#[test]
fn a_closed_day_never_changes_when_settings_do() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 5_000);
    e.product("Dates 1kg", "7003", 2_200, 1_500, 100_000);
    sell_as(&e, t, "7003", 1000, "cash");
    close_shift(&e, t, 7_200);
    let d = today(&e);
    let z = e.core.day_close(t, close_req(&d, &op())).unwrap();
    let pdf = e.core.day_close_pdf(t, &z.close_id).unwrap();

    // Change everything a report could pick up.
    let mut biz = serde_json::to_value(e.core.business_get(t).unwrap()).unwrap();
    biz["name"] = json!("Renamed Market");
    biz["vat_number"] = json!("299999999999993");
    e.core.business_update(t, serde_json::from_value(biz).unwrap()).unwrap();
    let mut shift = e.core.settings_get(t, "shift").unwrap();
    shift["day_cutoff_minutes"] = json!(180);
    e.core.settings_save(t, "shift", shift).unwrap();
    let mut rc = e.core.settings_get(t, "receipt").unwrap();
    rc["language"] = json!("en");
    e.core.settings_save(t, "receipt", rc).unwrap();

    let later = e.core.day_close_get(t, &z.close_id).unwrap();
    assert!(later.verified);
    assert_eq!(later.report, z.report);
    assert_eq!(later.doc, z.doc);
    assert_eq!(later.report.business_name, "Al Noor Supermarket");
    assert_eq!(later.report.cutoff_minutes, 0);
    assert_eq!(e.core.day_close_pdf(t, &z.close_id).unwrap()["base64"], pdf["base64"], "the PDF is the same document");
    // The record itself refuses to change.
    let tamper = e.core.db.write(|tx| Ok(tx.execute("UPDATE day_closes SET net_sales_minor=0", [])?));
    assert!(tamper.is_err());
    let tamper = e.core.db.write(|tx| Ok(tx.execute("DELETE FROM day_close_items", [])?));
    assert!(tamper.is_err());
}

#[test]
fn a_sale_that_arrives_after_its_day_was_closed_counts_once_in_the_next_close() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    e.product("Rice 5kg", "7004", 3_500, 2_400, 100_000);
    let base = sell_as(&e, t, "7004", 1000, "cash");
    let d = today(&e);
    let d2 = days_before(&d, 2);
    let d1 = days_before(&d, 1);
    let noon = |date: &str| format!("{date}T09:00:00.000Z");

    let a = arrive(&e, &base.sale_id, &d2, &noon(&d2));
    let z2 = e.core.day_close(t, close_req(&d2, &op())).unwrap();
    assert_eq!(z2.report.day.sale_count, 1);

    // A till that was offline sends a sale of D-2 after D-2 was closed.
    let late = arrive(&e, &base.sale_id, &d2, &format!("{d2}T10:30:00.000Z"));
    let b = arrive(&e, &base.sale_id, &d1, &noon(&d1));
    let x = e.core.day_x(t, Some(d1.clone()), None).unwrap();
    assert_eq!(x.day.sale_count, 1, "D-1's own sale");
    assert_eq!(x.after_close.sale_count, 1, "the late sale is an after-close adjustment");
    assert_eq!(x.late.len(), 1);
    assert_eq!((x.late[0].business_date.as_str(), x.late[0].kind.as_str()), (d2.as_str(), "sale"));
    let checks = e.core.day_checks(t, Some(d1.clone()), None).unwrap();
    assert!(checks.checks.iter().any(|c| c.code == "late_records" && c.level == "info"));

    let z1 = e.core.day_close(t, close_req(&d1, &op())).unwrap();
    assert_eq!((z1.report.day.sale_count, z1.report.after_close.sale_count, z1.report.total.sale_count), (1, 1, 2));
    assert_eq!(z1.report.total.net_sales_minor, 7_000);
    // The original close is exactly as it was; the late sale keeps its own date and time.
    let z2_again = e.core.day_close_get(t, &z2.close_id).unwrap();
    assert!(z2_again.verified);
    assert_eq!(z2_again.report, z2.report);
    let (date, at): (String, String) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT business_date, completed_at FROM sales WHERE sale_id=?1", [&late], |r| Ok((r.get(0)?, r.get(1)?)))?)
        })
        .unwrap();
    assert_eq!((date.as_str(), at.as_str()), (d2.as_str(), format!("{d2}T10:30:00.000Z").as_str()));
    let (close, flag): (String, i64) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT close_id, late FROM day_close_items WHERE ref_kind='sale' AND ref_id=?1", [&late], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?)
        })
        .unwrap();
    assert_eq!((close.as_str(), flag), (z1.close_id.as_str(), 1));
    // Each sale is counted by exactly one close; today's sale by none yet.
    for (id, n) in [(&a, 1), (&late, 1), (&b, 1), (&base.sale_id, 0)] {
        assert_eq!(one::<i64>(&e, &format!("SELECT COUNT(*) FROM day_close_items WHERE ref_id='{id}'")), n);
    }
    // Days close in order: an older open day blocks a newer one.
    let gap = e.core.day_close(t, close_req(&days_before(&d, 3), &op())).unwrap_err();
    assert_eq!(gap.code, ErrorCode::Conflict, "a day before the last close cannot be closed");
}

#[test]
fn days_close_in_order() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 1_000);
    e.product("Milk", "7005", 500, 300, 100_000);
    let base = sell_as(&e, t, "7005", 1000, "cash");
    let d = today(&e);
    let (d3, d2) = (days_before(&d, 3), days_before(&d, 2));
    arrive(&e, &base.sale_id, &d3, &format!("{d3}T09:00:00.000Z"));
    e.core.day_close(t, close_req(&d3, &op())).unwrap();
    arrive(&e, &base.sale_id, &d2, &format!("{d2}T09:00:00.000Z"));
    let err = e.core.day_close(t, close_req(&days_before(&d, 1), &op())).unwrap_err();
    assert!(err.message.contains(&d2), "{}", err.message);
    e.core.day_close(t, close_req(&d2, &op())).unwrap();
    // A future day cannot be closed.
    let tomorrow = (chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d").unwrap() + chrono::Duration::days(1)).to_string();
    assert_eq!(e.core.day_close(t, close_req(&tomorrow, &op())).unwrap_err().code, ErrorCode::Conflict);
}

#[test]
fn the_close_uses_the_stored_trading_day_at_the_cutoff() {
    let e = env();
    let t = &e.owner_token;
    let mut shift = e.core.settings_get(t, "shift").unwrap();
    shift["day_cutoff_minutes"] = json!(120);
    e.core.settings_save(t, "shift", shift).unwrap();
    e.open_shift(t, 1_000);
    e.product("Tea", "7006", 800, 500, 100_000);
    let base = sell_as(&e, t, "7006", 1000, "cash");
    let d = today(&e);
    let x = days_before(&d, 5);
    let day = e.core.db.read(amwapos_core::time::day).unwrap();
    // 01:15 Bahrain on X+1 is still day X; 02:30 is day X+1 (the Wave 1 rule).
    let before_cut = format!("{x}T22:15:00.000Z");
    let after_cut = format!("{x}T23:30:00.000Z");
    let bd = |ts: &str| amwapos_core::time::business_date(amwapos_core::time::parse(ts).unwrap(), &day).unwrap();
    assert_eq!(bd(&before_cut), x);
    assert_eq!(bd(&after_cut), days_before(&d, 4));
    let early = arrive(&e, &base.sale_id, &bd(&before_cut), &before_cut);
    let late = arrive(&e, &base.sale_id, &bd(&after_cut), &after_cut);
    let zx = e.core.day_close(t, close_req(&x, &op())).unwrap();
    assert_eq!(zx.report.total.sale_count, 1);
    assert_eq!(zx.report.cutoff_minutes, 120);
    assert_eq!(one::<String>(&e, &format!("SELECT close_id FROM day_close_items WHERE ref_id='{early}'")), zx.close_id);
    let zn = e.core.day_close(t, close_req(&days_before(&d, 4), &op())).unwrap();
    assert_eq!(one::<String>(&e, &format!("SELECT close_id FROM day_close_items WHERE ref_id='{late}'")), zn.close_id);
}

#[test]
fn one_branch_close_never_closes_another() {
    let e = env();
    let t = &e.owner_token;
    e.core.settings_save(t, "features", json!({ "org.multi_branch": true })).unwrap();
    let rows = e.core.branch_save(t, None, serde_json::from_value(json!({ "code": "B2", "name": "Riffa" })).unwrap()).unwrap();
    let b2 = rows.iter().find(|b| b.code == "B2").unwrap().branch_id.clone();
    e.open_shift(t, 1_000);
    e.product("Water", "7007", 100, 50, 100_000);
    let base = sell_as(&e, t, "7007", 1000, "cash");
    close_shift(&e, t, 1_100);
    let d = today(&e);
    // A sale made in branch 2 (as its till wrote it).
    let n = ulid::Ulid::new().to_string();
    let other = format!("B{n}");
    e.core
        .db
        .write(|tx| {
            clone_row(
                tx,
                "sales",
                "sale_id",
                &base.sale_id,
                &[
                    ("sale_id", json!(other)),
                    ("receipt_number", json!(format!("B2-{}", &n[18..]))),
                    ("operation_id", json!(format!("opb{n}"))),
                    ("branch_id", json!(b2)),
                    ("cart_id", Value::Null),
                ],
            )?;
            Ok(())
        })
        .unwrap();
    let main = e.core.day_close(t, close_req(&d, &op())).unwrap();
    assert_eq!(main.report.total.sale_count, 1, "branch 1: its own sale only");
    assert_eq!(one::<i64>(&e, &format!("SELECT COUNT(*) FROM day_close_items WHERE ref_id='{other}'")), 0);
    let mut r2 = close_req(&d, &op());
    r2.branch_id = Some(b2.clone());
    let z2 = e.core.day_close(t, r2).unwrap();
    assert_eq!((z2.report.branch_name.as_str(), z2.report.total.sale_count), ("Riffa", 1));
    assert_ne!(z2.close_number, main.close_number);
}

#[test]
fn expected_cash_follows_every_kind_of_cash_movement() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 20_000);
    e.product("Laban 1L", "7001", 1_000, 600, 100_000);
    let a = sell_as(&e, t, "7001", 3000, "cash"); // +3.000
    sell_as(&e, t, "7001", 1000, "card"); // drawer unchanged
    let v = sell_as(&e, t, "7001", 1000, "cash"); // +1.000, voided −1.000
    e.core
        .sale_void(
            t,
            amwapos_core::voids::VoidRequest {
                sale_id: v.sale_id,
                reason: "Rang up twice".into(),
                operation_id: op(),
                approval_token: None,
            },
        )
        .unwrap();
    let item: String = one(&e, &format!("SELECT sale_item_id FROM sale_items WHERE sale_id='{}'", a.sale_id));
    e.core
        .refund_create(
            t,
            serde_json::from_value(json!({ "sale_id": a.sale_id, "reason": "Damaged", "operation_id": op(),
                "lines": [{ "sale_item_id": item, "qty_milli": 1000, "restock": true }] }))
            .unwrap(),
        )
        .unwrap(); // −1.000
    let ev = |kind: &str, amt: i64| {
        e.core
            .cash_event(
                t,
                serde_json::from_value(json!({ "kind": kind, "amount_minor": amt, "reason": "Test", "operation_id": op() })).unwrap(),
            )
            .unwrap()
    };
    ev("paid_in", 5_000);
    let po = ev("paid_out", 2_000);
    ev("safe_drop", 10_000);
    // The paid-out backs an expense (counted once, as the paid-out).
    let x = e
        .core
        .expense_save(
            t,
            None,
            serde_json::from_value(json!({ "category_id": "exp_cleaning", "description": "Mop", "total_minor": 2_000, "payee": "Shop" }))
                .unwrap(),
        )
        .unwrap();
    e.core.expense_submit(t, &x.expense_id).unwrap();
    e.core
        .expense_pay(
            t,
            &x.expense_id,
            serde_json::from_value(json!({ "method": "till_paid_out", "cash_event_id": po["cash_event_id"], "operation_id": op() }))
                .unwrap(),
        )
        .unwrap();
    // Pay on delivery, then the cash collected at the door.
    let cu = e
        .core
        .customer_save(t, None, serde_json::from_value(json!({ "name": "Ali", "phone": "33332222", "address": "Villa 7" })).unwrap())
        .unwrap()
        .customer_id;
    e.core.pos_scan(t, "7001", Some(2000)).unwrap();
    let cart = e.core.pos_set_customer(t, Some(cu)).unwrap();
    let total = cart.totals.total_minor;
    let s = e
        .core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![TenderInput { method: "pay_on_delivery".into(), amount_minor: total, reference: None }],
                approval_token: None,
                expected_total_minor: Some(total),
                fulfilment: Some(Fulfilment {
                    mode: "send".into(),
                    address_parts: amwapos_core::address::AddressParts {
                        building: Some("12".into()),
                        block: Some("905".into()),
                        landmark: Some("Villa 7".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            },
        )
        .unwrap();
    e.core
        .ticket_record_payment(
            t,
            amwapos_core::tickets::RecordPayment {
                delivery_id: s.delivery_id.unwrap(),
                method: "cash".into(),
                amount_minor: None,
                reference: None,
                operation_id: op(),
            },
        )
        .unwrap(); // +2.000

    let expected = 20_000 + 3_000 + 1_000 - 1_000 - 1_000 + 5_000 - 2_000 - 10_000 + 2_000;
    let xr = e.core.day_x(t, None, None).unwrap();
    assert_eq!(xr.cash.expected_cash_minor, expected);
    assert_eq!(xr.cash.expenses_from_till_minor, 2_000, "shown as part of the paid-outs, not again");
    assert_eq!(xr.cash.delivery_collections_minor, 2_000);
    assert_eq!(xr.day.pay_on_delivery_minor, 2_000);
    assert_eq!(xr.drawers[0].expected_cash_minor, expected);
    let c = &xr.cash;
    assert_eq!(
        c.opening_float_minor + c.cash_sales_minor - c.cash_refunds_minor + c.paid_in_minor - c.paid_out_minor - c.safe_drop_minor
            + c.delivery_collections_minor
            + c.rider_handover_minor,
        c.expected_cash_minor,
        "the X cash lines add up"
    );
    // Counted short: the Z keeps the difference.
    close_shift(&e, t, expected - 250);
    let z = e.core.day_close(t, close_req(&today(&e), &op())).unwrap();
    assert_eq!((z.report.cash.expected_cash_minor, z.report.cash.variance_minor), (expected, -250));
}

#[test]
fn a_shift_records_its_register_and_drawer_and_registers_move_between_computers() {
    let e = env();
    let t = &e.owner_token;
    let dev = e.core.device().unwrap().device_id;
    let regs = e.core.registers_list(t).unwrap();
    assert_eq!(regs.len(), 1);
    assert_eq!(regs[0].register_id, format!("reg_{dev}"));
    assert_eq!(regs[0].device_id.as_deref(), Some(dev.as_str()));
    assert!(regs[0].drawers[0].is_default);
    e.open_shift(t, 1_000);
    let s = e.core.shift_current(t).unwrap().unwrap();
    assert_eq!(s.register_id.as_deref(), Some(format!("reg_{dev}").as_str()));
    assert_eq!(s.drawer_id.as_deref(), Some(format!("drw_{dev}").as_str()));
    assert!(e.core.registers_list(t).unwrap()[0].open_shift.is_some());
    // A new computer gets its own register; the old register can move to it.
    e.core
        .db
        .write(|tx| {
            tx.execute(
                "INSERT INTO devices(device_id, branch_id, name, device_code, operating_mode, active, activated_at) SELECT 'dev2', branch_id, 'New PC', 'T02', 'terminal', 1, 'x' FROM devices LIMIT 1",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(e.core.registers_list(t).unwrap().len(), 2);
    let moved = e
        .core
        .register_save(t, Some(format!("reg_{dev}")), serde_json::from_value(json!({ "name": "Till 1", "device_id": "dev2" })).unwrap())
        .unwrap();
    let r1 = moved.iter().find(|r| r.register_id == format!("reg_{dev}")).unwrap();
    let r2 = moved.iter().find(|r| r.register_id == "reg_dev2").unwrap();
    assert_eq!(r1.device_id.as_deref(), Some("dev2"));
    assert_eq!(r2.device_id, None, "the new computer left its own register");
    // A register with an open shift cannot be switched off.
    let off =
        e.core.register_save(t, Some(format!("reg_{dev}")), serde_json::from_value(json!({ "name": "Till 1", "active": false })).unwrap());
    assert_eq!(off.unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type='register.updated'"), 1);
}

#[test]
fn upgrading_keeps_every_shift_and_invents_no_register_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("amwapos.db");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        amwapos_core::db::migrate_until(&c, &path, 27).unwrap();
        c.execute_batch(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO business(business_id, name, created_at, updated_at) VALUES ('b','Old Shop','x','x');
             INSERT INTO branches(branch_id, code, name, created_at, updated_at) VALUES ('br','B1','Main','x','x');
             INSERT INTO devices(device_id, branch_id, name, device_code, operating_mode, active, activated_at) VALUES ('d1','br','Till A','T01','standalone',1,'x');
             INSERT INTO devices(device_id, branch_id, name, device_code, operating_mode, active, activated_at) VALUES ('d2','br','Old PC','T02','terminal',0,'x');
             INSERT INTO shifts(shift_id, shift_number, user_id, branch_id, device_id, business_date, opening_float_minor, opened_at, closed_at, status,
                                expected_cash_minor, counted_cash_minor, variance_minor)
               VALUES ('s1','T01-S00001','u','br','d1','2026-09-01',10000,'2026-09-01T05:00:00Z','2026-09-01T15:00:00Z','closed',12000,11500,-500);",
        )
        .unwrap();
    }
    let core =
        amwapos_core::service::AppCore::open(dir.path(), std::sync::Arc::new(amwapos_core::service::MemorySecretStore::default())).unwrap();
    let (shift, reg, drawer, variance): (String, Option<String>, Option<String>, i64) = core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT shift_id, register_id, drawer_id, variance_minor FROM shifts", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?)
        })
        .unwrap();
    assert_eq!((shift.as_str(), reg, drawer, variance), ("s1", None, None, -500), "history is kept as it was recorded");
    let regs: Vec<(String, Option<String>, i64)> = core
        .db
        .read(|c| {
            Ok(c.prepare("SELECT register_id, device_id, active FROM registers ORDER BY register_id")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?)
        })
        .unwrap();
    assert_eq!(regs, vec![("reg_d1".into(), Some("d1".into()), 1), ("reg_d2".into(), None, 0)]);
    // An old difference does not become a case at upgrade.
    assert_eq!(core.db.write(amwapos_core::cases::sweep_cash_variances).unwrap(), 0);
    assert!(
        core.db
            .read(|c| Ok(
                c.query_row("SELECT COUNT(*) FROM pragma_table_info('shifts') WHERE name='register_id'", [], |r| r.get::<_, i64>(0))?
            ))
            .unwrap()
            == 1
    );
}

#[test]
fn a_cash_difference_becomes_a_case_with_facts_and_a_permanent_history() {
    let e = env();
    let t = &e.owner_token;
    let (_m, mt) = e.user("Mona", amwapos_core::auth::ROLE_MANAGER, "5917");
    let (_c, ct) = e.user("Cashier C", amwapos_core::auth::ROLE_CASHIER, "4682");
    e.product("Rice", "7008", 6_250, 4_000, 100_000);
    e.open_shift(t, 10_000);
    sell_as(&e, t, "7008", 1000, "cash");
    close_shift(&e, t, 10_000); // 6.250 short
    let list = e.core.cases_list(t, None).unwrap();
    assert_eq!(list.len(), 1);
    let k = &list[0];
    assert_eq!((k.kind.as_str(), k.status.as_str(), k.title.as_str()), ("cash_variance", "new", "Drawer is BHD 6.250 short"));
    assert_eq!(k.facts["variance_minor"], -6_250);
    assert_eq!(k.facts["expected_cash_minor"], 16_250);
    assert!(k.facts["register_name"].is_string());
    assert_eq!(k.severity, "high", "more than five times the threshold");
    // Opening it again, or sweeping again, never makes a second case.
    let sid = k.entity_id.clone();
    assert_eq!(e.core.case_open_for_shift(t, &sid, None).unwrap().case.case_id, k.case_id);
    assert_eq!(e.core.db.write(amwapos_core::cases::sweep_cash_variances).unwrap(), 0);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM cases"), 1);

    let act = |tok: &str, action: &str, note: Option<&str>, op_id: &str| {
        e.core.case_act(
            tok,
            serde_json::from_value(
                json!({ "case_id": k.case_id, "action": action, "note": note, "resolution_code": "counting_error", "operation_id": op_id }),
            )
            .unwrap(),
        )
    };
    assert_eq!(act(&ct, "acknowledge", None, &op()).unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(act(&mt, "acknowledge", None, &op()).unwrap().case.status, "acknowledged");
    assert_eq!(act(&mt, "start", None, &op()).unwrap().case.status, "in_progress");
    assert_eq!(act(&mt, "resolve", None, &op()).unwrap_err().code, ErrorCode::Validation, "say what was found");
    let resolve_op = op();
    let done = act(&mt, "resolve", Some("Recounted: a 5 and a 1.250 were in the coin tray."), &resolve_op).unwrap();
    assert_eq!((done.case.status.as_str(), done.case.resolution_code.as_deref()), ("resolved", Some("counting_error")));
    // Retrying the same step changes nothing; a new step on a finished case is refused.
    let again = act(&mt, "resolve", Some("Recounted: a 5 and a 1.250 were in the coin tray."), &resolve_op).unwrap();
    assert_eq!(again.events.len(), done.events.len());
    assert_eq!(act(&mt, "dismiss", Some("x"), &op()).unwrap_err().code, ErrorCode::Conflict);
    let kinds: Vec<String> = done.events.iter().map(|e| e.to_status.clone().unwrap_or(e.kind.clone())).collect();
    assert_eq!(kinds, vec!["new", "acknowledged", "in_progress", "resolved"]);
    // History and facts cannot be rewritten.
    assert!(e.core.db.write(|tx| Ok(tx.execute("UPDATE case_events SET note='x'", [])?)).is_err());
    assert!(e.core.db.write(|tx| Ok(tx.execute("UPDATE cases SET status='new'", [])?)).is_err());
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type LIKE 'case.%'"), 4);
}

#[test]
fn closing_and_registers_and_cases_need_their_permissions() {
    let e = env();
    let (_c, ct) = e.user("Cashier D", amwapos_core::auth::ROLE_CASHIER, "4682");
    let d = today(&e);
    assert_eq!(e.core.day_x(&ct, None, None).unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(e.core.day_close(&ct, close_req(&d, &op())).unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(e.core.day_opening(&ct).unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(e.core.cases_list(&ct, None).unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(
        e.core.register_save(&ct, None, serde_json::from_value(json!({ "name": "Till 9" })).unwrap()).unwrap_err().code,
        ErrorCode::Forbidden
    );
    let (_a, at) = e.user("Accountant", amwapos_core::auth::ROLE_ACCOUNTANT, "7351");
    e.core.day_x(&at, None, None).unwrap();
    assert_eq!(
        e.core.day_close(&at, close_req(&d, &op())).unwrap_err().code,
        ErrorCode::Forbidden,
        "an accountant reads, the manager closes"
    );
}

#[test]
fn a_close_that_fails_halfway_leaves_nothing_and_can_be_retried() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 1_000);
    e.product("Ghee", "7009", 2_000, 1_200, 100_000);
    sell_as(&e, t, "7009", 1000, "cash");
    close_shift(&e, t, 3_000);
    let d = today(&e);
    // The computer fails while writing the list of counted records.
    e.core
        .db
        .write(|tx| {
            Ok(tx.execute_batch(
                "CREATE TRIGGER fail_close BEFORE INSERT ON day_close_items BEGIN SELECT RAISE(ABORT, 'disk failure'); END;",
            )?)
        })
        .unwrap();
    let op_id = op();
    assert!(e.core.day_close(t, close_req(&d, &op_id)).is_err());
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM day_closes"), 0);
    assert_eq!(one::<i64>(&e, &format!("SELECT COUNT(*) FROM operation_idempotency WHERE operation_id='{op_id}'")), 0);
    e.core.db.write(|tx| Ok(tx.execute_batch("DROP TRIGGER fail_close;")?)).unwrap();
    // The same request, retried, closes the day once.
    let z = e.core.day_close(t, close_req(&d, &op_id)).unwrap();
    assert_eq!(z.report.total.sale_count, 1);
    assert_eq!(one::<i64>(&e, "SELECT COUNT(*) FROM day_closes"), 1);
    // A cash count sent twice is one count.
    e.open_shift(t, 1_000);
    let id = e.core.shift_current(t).unwrap().unwrap().shift_id;
    let req: amwapos_core::shifts::ShiftCloseRequest =
        serde_json::from_value(json!({ "counted_cash_minor": 1_000, "operation_id": op() })).unwrap();
    e.core.shift_close(t, &id, req.clone()).unwrap();
    e.core.shift_close(t, &id, req).unwrap();
    assert_eq!(one::<i64>(&e, &format!("SELECT COUNT(*) FROM audit_logs WHERE event_type='shift.closed' AND entity_id='{id}'")), 1);
}

#[test]
fn opening_says_ready_or_what_needs_attention() {
    let e = env();
    let t = &e.owner_token;
    let o = e.core.day_opening(t).unwrap();
    assert!(o.checks.iter().any(|c| c.code == "register" && c.level == "ok"));
    assert!(o.checks.iter().any(|c| c.code == "float" && c.level == "info"));
    assert!(o.checks.iter().all(|c| c.level != "blocking"), "opening never blocks");
    e.product("Salt", "7010", 200, 100, 100_000);
    e.open_shift(t, 1_000);
    let base = sell_as(&e, t, "7010", 1000, "cash");
    let d = today(&e);
    let (d2, d1) = (days_before(&d, 2), days_before(&d, 1));
    arrive(&e, &base.sale_id, &d2, &format!("{d2}T09:00:00.000Z"));
    e.core.day_close(t, close_req(&d2, &op())).unwrap();
    arrive(&e, &base.sale_id, &d1, &format!("{d1}T09:00:00.000Z"));
    let o = e.core.day_opening(t).unwrap();
    assert_eq!(o.verdict, "attention");
    let prior = o.checks.iter().find(|c| c.code == "prior_day").unwrap();
    assert_eq!(prior.level, "warning");
    assert!(prior.message.contains(&d1));
}
