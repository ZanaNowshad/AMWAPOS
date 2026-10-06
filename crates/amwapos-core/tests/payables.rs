//! Accounts Payable foundation: supplier invoice lifecycle, posting,
//! credits, payments, allocations, derived balances, ageing, reversal,
//! immutability, idempotency and permissions. Exact BHD fils throughout.

mod common;

use amwapos_core::payables::{AllocationInput, ManualInvoice, PaymentInput};
use amwapos_core::ErrorCode;
use common::*;
use serde_json::{json, Value};

fn supplier(e: &Env, name: &str, terms: Option<&str>) -> String {
    e.core
        .supplier_save(&e.owner_token, None, serde_json::from_value(json!({ "name": name, "payment_terms": terms })).unwrap())
        .unwrap()
        .supplier_id
}

fn days_ago(n: i64) -> String {
    (chrono::Utc::now().date_naive() - chrono::Duration::days(n)).to_string()
}

fn invoice(e: &Env, sup: &str, number: &str, date: &str, due: Option<&str>, total: i64) -> String {
    let vat = total / 11; // 10% VAT inside the total, exact
    let v = e
        .core
        .ap_invoice_create_manual(
            &e.owner_token,
            ManualInvoice {
                supplier_id: sup.into(),
                doc_type: "invoice".into(),
                invoice_number: number.into(),
                invoice_date: date.into(),
                due_date: due.map(|d| d.to_string()),
                subtotal_minor: total - vat,
                vat_minor: vat,
                total_minor: total,
                applies_to_invoice_id: None,
                po_id: None,
                notes: None,
                lines: vec![],
            },
        )
        .unwrap();
    v["invoice_id"].as_str().unwrap().to_string()
}

fn approve_post(e: &Env, id: &str) -> Value {
    e.core.ap_invoice_approve(&e.owner_token, id).unwrap();
    e.core.ap_invoice_post(&e.owner_token, id, &op()).unwrap()
}

fn pay(e: &Env, sup: &str, amount: i64, allocs: &[(&str, i64)], oldest_first: bool) -> Value {
    e.core
        .ap_payment_record(
            &e.owner_token,
            PaymentInput {
                supplier_id: sup.into(),
                paid_on: days_ago(0),
                amount_minor: amount,
                method: "bank_transfer".into(),
                reference: Some("TRF-1".into()),
                notes: None,
                allocations: allocs.iter().map(|(i, a)| AllocationInput { invoice_id: i.to_string(), amount_minor: *a }).collect(),
                oldest_first,
                operation_id: op(),
            },
        )
        .unwrap()
}

fn balance(e: &Env, sup: &str) -> i64 {
    e.core.ap_supplier(&e.owner_token, sup).unwrap()["balance_minor"].as_i64().unwrap()
}

fn count(e: &Env, sql: &str) -> i64 {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

#[test]
fn only_an_approved_invoice_posts_and_it_posts_once_with_an_exact_liability() {
    let e = env();
    let s = supplier(&e, "Gulf Foods", Some("30 days"));
    let inv = invoice(&e, &s, "GF-1001", &days_ago(5), None, 12_345);
    // A draft (unreviewed) record cannot be posted.
    let err = e.core.ap_invoice_post(&e.owner_token, &inv, &op()).unwrap_err();
    assert_eq!(err.details.unwrap()["kind"], "not_approved");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM ap_liabilities"), 0);
    e.core.ap_invoice_approve(&e.owner_token, &inv).unwrap();
    // Double-click / network retry: the same operation id is one posting.
    let op1 = op();
    let v = e.core.ap_invoice_post(&e.owner_token, &inv, &op1).unwrap();
    let again = e.core.ap_invoice_post(&e.owner_token, &inv, &op1).unwrap();
    assert_eq!(v["liability"]["liability_id"], again["liability"]["liability_id"]);
    // A second posting with another operation id is refused.
    let err = e.core.ap_invoice_post(&e.owner_token, &inv, &op()).unwrap_err();
    assert_eq!(err.details.unwrap()["kind"], "already_posted");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM ap_liabilities"), 1);
    assert_eq!(v["liability"]["amount_minor"], 12_345, "exact fils");
    assert_eq!(v["liability"]["due_rule"], "terms");
    assert_eq!(v["liability"]["due_date"], json!(days_ago(5 - 30)), "invoice date + 30 days of terms");
    assert_eq!(v["lifecycle"], "unpaid");
    assert_eq!(balance(&e, &s), 12_345);
    // Posting changed no stock.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_movements"), 0);
}

#[test]
fn a_posted_invoice_is_immutable_and_only_a_reversal_undoes_it() {
    let e = env();
    let s = supplier(&e, "Delta Trading", None);
    let inv = invoice(&e, &s, "DT-7", &days_ago(1), Some(&days_ago(-14)), 5_000);
    approve_post(&e, &inv);
    // The database itself refuses edits and deletes of posted records.
    let edit = e.core.db.write(|c| Ok(c.execute("UPDATE supplier_invoices SET total_minor=1 WHERE invoice_id=?1", [&inv])?));
    assert!(edit.is_err());
    let del = e.core.db.write(|c| Ok(c.execute("DELETE FROM ap_liabilities", [])?));
    assert!(del.is_err());
    let amt = e.core.db.write(|c| Ok(c.execute("UPDATE ap_liabilities SET amount_minor=1", [])?));
    assert!(amt.is_err());
    // Voiding a posted record is refused; reversing needs a reason.
    assert_eq!(e.core.supplier_invoice_set_status(&e.owner_token, &inv, "void").unwrap_err().code, ErrorCode::Conflict);
    assert!(e.core.ap_invoice_reverse(&e.owner_token, &inv, " ").is_err());
    // A paid invoice cannot be reversed until its payment is taken off it.
    let p = pay(&e, &s, 2_000, &[(&inv, 2_000)], false);
    let err = e.core.ap_invoice_reverse(&e.owner_token, &inv, "wrong supplier").unwrap_err();
    assert_eq!(err.details.unwrap()["kind"], "allocated");
    let alloc = p["allocations"][0]["allocation_id"].as_str().unwrap().to_string();
    e.core.ap_allocation_reverse(&e.owner_token, &alloc).unwrap();
    let v = e.core.ap_invoice_reverse(&e.owner_token, &inv, "wrong supplier").unwrap();
    assert_eq!(v["lifecycle"], "reversed");
    assert_eq!(v["liability"]["status"], "reversed");
    assert_eq!(v["liability"]["reversal_reason"], "wrong supplier");
    // The balance returns to what the payment left: the supplier owes us 2.000.
    assert_eq!(balance(&e, &s), -2_000);
    // A reversed record is never reopened, and the history is kept.
    assert!(e.core.db.write(|c| Ok(c.execute("UPDATE ap_liabilities SET status='open'", [])?)).is_err());
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type IN ('ap.invoice_posted','ap.reversed','ap.allocation_reversed')"),
        3
    );
}

#[test]
fn credits_payments_and_allocations_settle_invoices_and_balances_derive() {
    let e = env();
    let s = supplier(&e, "Al Watan", Some("Net 45"));
    let a = invoice(&e, &s, "AW-1", &days_ago(40), None, 10_000);
    let b = invoice(&e, &s, "AW-2", &days_ago(20), None, 4_500);
    let c = invoice(&e, &s, "AW-3", &days_ago(2), None, 1_250);
    for i in [&a, &b, &c] {
        approve_post(&e, i);
    }
    assert_eq!(balance(&e, &s), 15_750);
    // A credit note for invoice A reduces what is owed.
    let cn = e
        .core
        .ap_invoice_create_manual(
            &e.owner_token,
            ManualInvoice {
                supplier_id: s.clone(),
                doc_type: "credit_note".into(),
                invoice_number: "AW-CN-1".into(),
                invoice_date: days_ago(1),
                due_date: None,
                subtotal_minor: 1_000,
                vat_minor: 100,
                total_minor: 1_100,
                applies_to_invoice_id: Some(a.clone()),
                po_id: None,
                notes: None,
                lines: vec![],
            },
        )
        .unwrap()["invoice_id"]
        .as_str()
        .unwrap()
        .to_string();
    let v = approve_post(&e, &cn);
    assert_eq!(v["lifecycle"], "credit_open");
    assert_eq!(balance(&e, &s), 14_650);
    let credit = v["credit"]["credit_id"].as_str().unwrap().to_string();
    e.core
        .ap_allocate(&e.owner_token, None, Some(credit.clone()), vec![AllocationInput { invoice_id: a.clone(), amount_minor: 1_100 }])
        .unwrap();
    // A credit cannot be applied beyond its amount.
    let err = e
        .core
        .ap_allocate(&e.owner_token, None, Some(credit), vec![AllocationInput { invoice_id: b.clone(), amount_minor: 1 }])
        .unwrap_err();
    assert_eq!(err.details.unwrap()["kind"], "over_allocation");
    // One payment across two invoices, partially; another invoice untouched.
    let p = pay(&e, &s, 9_000, &[(&a, 8_000), (&b, 1_000)], false);
    assert_eq!(p["unallocated_minor"], 0);
    let inv_a = e.core.ap_invoice_get(&e.owner_token, &a).unwrap();
    assert_eq!((inv_a["lifecycle"].as_str(), inv_a["outstanding_minor"].as_i64()), (Some("partially_paid"), Some(900)));
    // Two payments against one invoice: the second finishes it.
    let p2 = pay(&e, &s, 900, &[(&a, 900)], false);
    assert_eq!(e.core.ap_invoice_get(&e.owner_token, &a).unwrap()["lifecycle"], "paid");
    // Allocating more than an invoice's outstanding is refused, nothing written.
    let err = e
        .core
        .ap_payment_record(
            &e.owner_token,
            PaymentInput {
                supplier_id: s.clone(),
                paid_on: days_ago(0),
                amount_minor: 5_000,
                method: "cash".into(),
                reference: None,
                notes: None,
                allocations: vec![AllocationInput { invoice_id: c.clone(), amount_minor: 2_000 }],
                oldest_first: false,
                operation_id: op(),
            },
        )
        .unwrap_err();
    assert_eq!(err.details.unwrap()["kind"], "over_invoice");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM ap_payments"), 2, "the refused payment was not recorded");
    // Balance = invoices − credits − payments = 15.750 − 1.100 − 9.000 − 0.900.
    assert_eq!(balance(&e, &s), 4_750);
    // The same figure from outstanding amounts (nothing unallocated left).
    let acct = e.core.ap_supplier(&e.owner_token, &s).unwrap();
    let outstanding: i64 = acct["open_invoices"].as_array().unwrap().iter().map(|i| i["outstanding_minor"].as_i64().unwrap()).sum();
    assert_eq!(outstanding, 4_750);
    // The statement's running balance ends at the balance.
    assert_eq!(acct["statement"].as_array().unwrap().last().unwrap()["balance_minor"], 4_750);
    // Reversing the second payment reopens invoice A.
    e.core.ap_payment_reverse(&e.owner_token, p2["payment_id"].as_str().unwrap(), "bounced").unwrap();
    assert_eq!(e.core.ap_invoice_get(&e.owner_token, &a).unwrap()["outstanding_minor"], 900);
    assert_eq!(balance(&e, &s), 5_650);
}

#[test]
fn payments_are_never_allocated_first_in_first_out_unless_chosen_and_never_twice() {
    let e = env();
    let s = supplier(&e, "Bahrain Dairy", None);
    let old = invoice(&e, &s, "BD-1", &days_ago(60), None, 3_000);
    let new = invoice(&e, &s, "BD-2", &days_ago(10), None, 2_000);
    approve_post(&e, &old);
    approve_post(&e, &new);
    // Without allocations the payment stays unallocated (it still lowers the balance).
    let p = pay(&e, &s, 1_000, &[], false);
    assert_eq!(p["unallocated_minor"], 1_000);
    assert_eq!(e.core.ap_invoice_get(&e.owner_token, &old).unwrap()["lifecycle"], "unpaid");
    assert_eq!(balance(&e, &s), 4_000);
    // "Oldest first" only when chosen.
    let p = pay(&e, &s, 3_500, &[], true);
    assert_eq!(p["unallocated_minor"], 0);
    assert_eq!(e.core.ap_invoice_get(&e.owner_token, &old).unwrap()["lifecycle"], "paid");
    assert_eq!(e.core.ap_invoice_get(&e.owner_token, &new).unwrap()["outstanding_minor"], 1_500);
    // Double submit of a payment: one payment.
    let input = || PaymentInput {
        supplier_id: s.clone(),
        paid_on: days_ago(0),
        amount_minor: 100,
        method: "cash".into(),
        reference: None,
        notes: None,
        allocations: vec![],
        oldest_first: false,
        operation_id: "pay-0123456789abcdef-once".into(),
    };
    let a = e.core.ap_payment_record(&e.owner_token, input()).unwrap();
    let b = e.core.ap_payment_record(&e.owner_token, input()).unwrap();
    assert_eq!(a["payment_id"], b["payment_id"]);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM ap_payments"), 3);
    // The same operation id for a different payment is refused.
    let mut other = input();
    other.amount_minor = 101;
    assert_eq!(e.core.ap_payment_record(&e.owner_token, other).unwrap_err().code, ErrorCode::IdempotencyMismatch);
}

#[test]
fn ageing_uses_due_dates_and_the_overview_counts_posted_records_only() {
    let e = env();
    let s = supplier(&e, "Ageing Co", None);
    let cur = invoice(&e, &s, "AG-1", &days_ago(3), Some(&days_ago(-10)), 1_000);
    let late15 = invoice(&e, &s, "AG-2", &days_ago(45), Some(&days_ago(15)), 2_000);
    let late75 = invoice(&e, &s, "AG-3", &days_ago(100), Some(&days_ago(75)), 3_000);
    let late120 = invoice(&e, &s, "AG-4", &days_ago(150), Some(&days_ago(120)), 4_000);
    for i in [&cur, &late15, &late75, &late120] {
        approve_post(&e, i);
    }
    // A draft and an approved-but-unposted invoice are not owed yet.
    invoice(&e, &s, "AG-5", &days_ago(1), None, 9_999);
    let unposted = invoice(&e, &s, "AG-6", &days_ago(1), None, 8_888);
    e.core.ap_invoice_approve(&e.owner_token, &unposted).unwrap();
    let o = e.core.ap_overview(&e.owner_token).unwrap();
    assert_eq!(o["outstanding_minor"], 10_000);
    assert_eq!(o["overdue_minor"], 9_000);
    assert_eq!(o["to_review"], 1);
    assert_eq!(o["to_post"], 1);
    assert_eq!(o["overdue_invoices"], 3);
    let ageing: Vec<(String, i64)> = o["ageing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["bucket"].as_str().unwrap().to_string(), b["amount_minor"].as_i64().unwrap()))
        .collect();
    assert_eq!(
        ageing,
        vec![("current".into(), 1_000), ("1_30".into(), 2_000), ("31_60".into(), 0), ("61_90".into(), 3_000), ("90_plus".into(), 4_000)]
    );
    assert_eq!(o["suppliers"][0]["balance_minor"], 10_000);
}

#[test]
fn duplicate_supplier_numbers_and_foreign_allocations_are_refused() {
    let e = env();
    let s = supplier(&e, "Dup Supplier", None);
    let other = supplier(&e, "Other Supplier", None);
    let a = invoice(&e, &s, "INV-9", &days_ago(3), None, 1_000);
    approve_post(&e, &a);
    let b = invoice(&e, &s, "inv-9", &days_ago(2), None, 1_000);
    e.core.ap_invoice_approve(&e.owner_token, &b).unwrap();
    let err = e.core.ap_invoice_post(&e.owner_token, &b, &op()).unwrap_err();
    assert_eq!(err.details.unwrap()["kind"], "duplicate_number");
    // The same number at another supplier is fine.
    let c = invoice(&e, &other, "INV-9", &days_ago(2), None, 700);
    approve_post(&e, &c);
    // A payment to one supplier cannot settle another supplier's invoice.
    let err = e
        .core
        .ap_payment_record(
            &e.owner_token,
            PaymentInput {
                supplier_id: s.clone(),
                paid_on: days_ago(0),
                amount_minor: 700,
                method: "cash".into(),
                reference: None,
                notes: None,
                allocations: vec![AllocationInput { invoice_id: c.clone(), amount_minor: 700 }],
                oldest_first: false,
                operation_id: op(),
            },
        )
        .unwrap_err();
    assert!(err.message.contains("another supplier"), "{}", err.message);
    // Exact arithmetic is required when entering an invoice.
    let bad = e.core.ap_invoice_create_manual(
        &e.owner_token,
        ManualInvoice {
            supplier_id: s,
            doc_type: "invoice".into(),
            invoice_number: "X".into(),
            invoice_date: days_ago(1),
            due_date: None,
            subtotal_minor: 1_000,
            vat_minor: 100,
            total_minor: 1_099,
            applies_to_invoice_id: None,
            po_id: None,
            notes: None,
            lines: vec![],
        },
    );
    assert!(bad.is_err());
}

#[test]
fn cashiers_and_managers_have_only_the_powers_they_are_given() {
    let e = env();
    let s = supplier(&e, "Perm Supplier", None);
    let inv = invoice(&e, &s, "P-1", &days_ago(1), None, 1_000);
    let (_, cashier) = e.user("Cash", "role_cashier", "1357");
    let (_, manager) = e.user("Mgr", "role_manager", "2468");
    assert_eq!(e.core.ap_overview(&cashier).unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(e.core.ap_invoice_approve(&cashier, &inv).unwrap_err().code, ErrorCode::Forbidden);
    // Managers review and see payables, but posting and paying are the owner's by default.
    assert!(e.core.ap_overview(&manager).is_ok());
    e.core.ap_invoice_approve(&manager, &inv).unwrap();
    assert_eq!(e.core.ap_invoice_post(&manager, &inv, &op()).unwrap_err().code, ErrorCode::Forbidden);
    let err = e
        .core
        .ap_payment_record(
            &manager,
            PaymentInput {
                supplier_id: s,
                paid_on: days_ago(0),
                amount_minor: 100,
                method: "cash".into(),
                reference: None,
                notes: None,
                allocations: vec![],
                oldest_first: false,
                operation_id: op(),
            },
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);
}

#[test]
fn a_posted_invoice_joins_the_canonical_purchase_cost_history() {
    let e = env();
    let s = supplier(&e, "Cost Supplier", None);
    let milk = e.product("Milk 1L", "6281007000013", 1_000, 850, 0);
    let v = e
        .core
        .ap_invoice_create_manual(
            &e.owner_token,
            ManualInvoice {
                supplier_id: s.clone(),
                doc_type: "invoice".into(),
                invoice_number: "CS-1".into(),
                invoice_date: days_ago(1),
                due_date: None,
                subtotal_minor: 9_250,
                vat_minor: 925,
                total_minor: 10_175,
                applies_to_invoice_id: None,
                po_id: None,
                notes: None,
                lines: vec![serde_json::from_value(
                    json!({ "product_id": milk, "description": "Milk 1L", "qty_milli": 10_000, "unit_cost_minor": 925 }),
                )
                .unwrap()],
            },
        )
        .unwrap();
    let inv = v["invoice_id"].as_str().unwrap().to_string();
    approve_post(&e, &inv);
    let pc = e.core.purchase_costs(&e.owner_token, &milk).unwrap();
    assert_eq!(pc["latest_cost_minor"], 925, "{pc}");
    assert_eq!(pc["selling_price_minor"], 1_000);
    assert_eq!(pc["margin_now"]["margin_minor"], 75);
    assert_eq!(pc["cheapest_recent_supplier"]["supplier_id"], json!(s));
    // The selling price is not changed by a cost.
    assert_eq!(e.core.product_get(&e.owner_token, &milk).unwrap().row.price_minor, Some(1_000));
    // A reversed invoice leaves the history.
    e.core.ap_invoice_reverse(&e.owner_token, &inv, "entered twice").unwrap();
    assert!(e.core.purchase_costs(&e.owner_token, &milk).unwrap()["history"]
        .as_array()
        .unwrap()
        .iter()
        .all(|h| h["source"] != "supplier_invoice"));
}

#[test]
fn a_reused_number_a_year_later_posts_and_a_credit_note_quoting_the_invoice_is_not_its_duplicate() {
    let e = env();
    let s = supplier(&e, "Reuse Co", None);
    let old = invoice(&e, &s, "R-100", &days_ago(500), None, 1_000);
    approve_post(&e, &old);
    let new = invoice(&e, &s, "R-100", &days_ago(3), None, 2_000);
    approve_post(&e, &new);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM ap_liabilities"), 2, "numbers reused more than a year apart are separate invoices");
    // Document duplicate signals follow the same rules.
    e.core
        .db
        .write(|c| {
            c.execute_batch(&format!(
                "INSERT INTO invoice_scans(scan_id,scan_number,supplier_id,image_path,image_sha256,status,created_by,created_at,updated_at,invoice_number_norm,invoice_date,doc_type)
                   VALUES ('SC-OLD','IS-1','{s}','x','sha-old','confirmed','u','x','x','INV55','2024-01-10','invoice'),
                          ('SC-INV','IS-2','{s}','x','sha-inv','confirmed','u','x','x','INV77','2026-09-01','invoice');"
            ))?;
            Ok(())
        })
        .unwrap();
    let dups = |num: &str, date: &str, kind: &str| {
        e.core
            .db
            .read(|c| amwapos_core::docintel::checks::duplicates(c, "SC-NEW", "sha-new", Some(&s), Some(num), Some(date), None, None, kind))
            .unwrap()
    };
    let far = dups("INV55", "2026-09-20", "invoice");
    assert_eq!(far[0].kind, "possible_duplicate", "{far:?}");
    let near = dups("INV77", "2026-09-20", "invoice");
    assert_eq!(near[0].kind, "same_invoice");
    // A credit note that prints the invoice number it corrects is not that invoice.
    assert!(dups("INV77", "2026-09-20", "credit_note").is_empty());
}

#[test]
fn overdue_and_ready_to_post_invoices_reach_the_dashboard_for_the_right_people() {
    let e = env();
    let sup = supplier(&e, "Late Supplies", None);
    let id = invoice(&e, &sup, "L-1", &days_ago(60), Some(&days_ago(30)), 11_000);
    approve_post(&e, &id);
    let ready = invoice(&e, &sup, "L-2", &days_ago(1), None, 1_100);
    e.core.ap_invoice_approve(&e.owner_token, &ready).unwrap();
    let d = e.core.dashboard(&e.owner_token).unwrap();
    let kinds: Vec<&str> = d["attention"].as_array().unwrap().iter().filter_map(|a| a["kind"].as_str()).collect();
    assert!(kinds.contains(&"payables") && kinds.contains(&"payables_post"), "{kinds:?}");
    // A manager without payables rights sees neither.
    let (_, mgr) = e.user("Floor Manager", "role_manager", "7391");
    let m = e.core.dashboard(&mgr).unwrap();
    let kinds: Vec<&str> = m["attention"].as_array().unwrap().iter().filter_map(|a| a["kind"].as_str()).collect();
    assert!(!kinds.contains(&"payables_post"), "{kinds:?}");
    // Paying it clears the overdue line.
    e.core
        .ap_payment_record(
            &e.owner_token,
            PaymentInput {
                supplier_id: sup.clone(),
                paid_on: days_ago(0),
                amount_minor: 11_000,
                method: "bank_transfer".into(),
                reference: None,
                notes: None,
                allocations: vec![AllocationInput { invoice_id: id, amount_minor: 11_000 }],
                oldest_first: false,
                operation_id: op(),
            },
        )
        .unwrap();
    let d = e.core.dashboard(&e.owner_token).unwrap();
    assert!(!d["attention"].to_string().contains("\"payables\""), "{}", d["attention"]);
}
