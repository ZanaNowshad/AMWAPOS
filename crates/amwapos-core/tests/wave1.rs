//! Wave 1 of the merchant operating system: trading day, receipt snapshots,
//! bound approvals, sale void, expenses and petty cash, operating profit and
//! customer statements. Every test checks an invariant, not only a reply.

mod common;

use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::FinalizeRequest;
use amwapos_core::time;
use amwapos_core::ErrorCode;
use common::*;
use serde_json::json;

fn sell(e: &Env, barcode: &str, qty: i64, tender: &str) -> amwapos_core::sales::SaleResult {
    let t = &e.owner_token;
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

fn one<T: rusqlite::types::FromSql>(e: &Env, sql: &str, p: &str) -> T {
    e.core.db.read(|c| Ok(c.query_row(sql, [p], |r| r.get(0))?)).unwrap()
}

#[test]
fn trading_day_cutoff_decides_the_business_date_of_new_records() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    e.product("Laban 1L", "7001", 450, 300, 50_000);

    // A cutoff after 06:00 is refused; the default stays midnight.
    let mut shift = e.core.settings_get(t, "shift").unwrap();
    shift["day_cutoff_minutes"] = json!(420);
    let err = e.core.settings_save(t, "shift", shift.clone()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Validation);

    // With the latest cutoff (06:00), the sale's stored date is exactly what
    // the one trading-day rule gives for its own timestamp.
    shift["day_cutoff_minutes"] = json!(360);
    e.core.settings_save(t, "shift", shift).unwrap();
    let sale = sell(&e, "7001", 1000, "cash");
    let (stored, at): (String, String) = e
        .core
        .db
        .read(|c| {
            Ok(c.query_row("SELECT business_date, completed_at FROM sales WHERE sale_id=?1", [&sale.sale_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?)
        })
        .unwrap();
    let day = time::Day { tz: "Asia/Bahrain".into(), cutoff_minutes: 360 };
    assert_eq!(stored, time::business_date(time::parse(&at).unwrap(), &day).unwrap());
    // …and the report range for that date contains the sale.
    let (a, b) = time::local_date_range_utc(&stored, &stored, &day).unwrap();
    assert!(a <= at && at < b, "{a} <= {at} < {b}");
    // The setting reaches the stored trading-day helper.
    let read = e.core.db.read(time::day).unwrap();
    assert_eq!(read.cutoff_minutes, 360);
    let _: String = one(&e, "SELECT sale_id FROM sales WHERE sale_id=?1", &sale.sale_id);
}

#[test]
fn every_table_has_a_replication_decision() {
    let e = env();
    let names: Vec<String> = e
        .core
        .db
        .read(|c| {
            let mut st = c.prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '%_fts_%' AND name NOT LIKE 'products_fts%' OR name IN ('products_fts','products_fts_map')",
            )?;
            let v = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            Ok(v)
        })
        .unwrap();
    let mut undecided = vec![];
    for n in &names {
        let replicated = amwapos_core::sync::policy(n).is_some();
        let local = amwapos_core::sync::LOCAL_TABLES.contains(&n.as_str());
        assert!(!(replicated && local), "{n} is listed as both replicated and local");
        if !replicated && !local {
            undecided.push(n.clone());
        }
    }
    assert!(undecided.is_empty(), "tables without a replication decision: {undecided:?}");
}

#[test]
fn issued_receipts_never_change_when_settings_change() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    e.product("Dates 500g", "7101", 1_250, 800, 50_000);
    let mut rcfg = e.core.settings_get(t, "receipt").unwrap();
    rcfg["footer_lines"] = json!(["Thank you — first footer"]);
    e.core.settings_save(t, "receipt", rcfg.clone()).unwrap();
    let sale = sell(&e, "7101", 2000, "cash");

    let before = e.core.db.read(|c| amwapos_core::receipt::issued(c, "sale", &sale.sale_id)).unwrap();
    assert!(before.exact, "a new sale has its receipt frozen at commit");
    assert!(before.doc.to_text().contains("first footer"));

    // The owner later changes the footer and the business name.
    rcfg["footer_lines"] = json!(["A different footer"]);
    e.core.settings_save(t, "receipt", rcfg).unwrap();
    e.core.db.write(|tx| Ok(tx.execute("UPDATE business SET name='Renamed Store'", [])?)).unwrap();

    let after = e.core.db.read(|c| amwapos_core::receipt::issued(c, "sale", &sale.sale_id)).unwrap();
    assert_eq!(after.doc, before.doc, "the issued receipt is unchanged");
    assert_eq!(after.sha256, before.sha256);
    assert!(!after.doc.to_text().contains("Renamed Store"));

    // A reprint says COPY, outside the fingerprinted body.
    let copy = e.core.db.read(|c| amwapos_core::receipt::sale_receipt(c, &sale.sale_id, Some("COPY"))).unwrap();
    assert!(copy.to_text().contains("*** COPY ***"));
    assert_eq!(copy.blocks.len(), before.doc.blocks.len() + 1);

    // The stored receipt cannot be edited or deleted.
    let edit = e.core.db.write(|tx| Ok(tx.execute("UPDATE receipt_snapshots SET doc_json='{}'", [])?));
    assert!(edit.is_err());
    let del = e.core.db.write(|tx| Ok(tx.execute("DELETE FROM receipt_snapshots", [])?));
    assert!(del.is_err());
}

#[test]
fn records_before_snapshots_reprint_as_reconstructed() {
    let e = env();
    let t = &e.owner_token;
    e.open_shift(t, 10_000);
    e.product("Tea 100", "7102", 900, 500, 50_000);
    let sale = sell(&e, "7102", 1000, "cash");
    // Simulate a sale made before migration 27: no snapshot row.
    e.core
        .db
        .write(|tx| {
            tx.execute_batch("DROP TRIGGER trg_receipt_snapshots_no_delete")?;
            Ok(tx.execute("DELETE FROM receipt_snapshots", [])?)
        })
        .unwrap();
    let r = e.core.db.read(|c| amwapos_core::receipt::issued(c, "sale", &sale.sale_id)).unwrap();
    assert!(!r.exact);
    assert!(r.doc.to_text().contains("Tea 100"));
}
