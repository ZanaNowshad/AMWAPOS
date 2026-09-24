//! WhatsApp outbox, payment-screenshot reviews and invoice scans: feature
//! gates, permissions, send-once semantics and "OCR never posts stock".
mod common;

use amwapos_core::messaging::QueueRequest;
use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::FinalizeRequest;
use amwapos_core::ErrorCode;
use common::*;
use serde_json::json;

fn count(e: &Env, sql: &str) -> i64 {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn features(e: &Env, v: serde_json::Value) {
    e.core.settings_save(&e.owner_token, "features", v).unwrap();
}

fn sale(e: &Env) -> String {
    let t = &e.owner_token;
    e.open_shift(t, 0);
    e.product("Laban 1L", "7001", 450, 300, 10_000);
    let cart = e.core.pos_scan(t, "7001", Some(2000)).unwrap().cart;
    let total = cart.totals.total_minor;
    e.core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![TenderInput { method: "cash".into(), amount_minor: total, reference: None }],
                approval_token: None,
                expected_total_minor: Some(total),
            },
        )
        .unwrap()
        .sale_id
}

fn receipt_req(sale_id: &str, op_id: &str) -> QueueRequest {
    serde_json::from_value(json!({ "operation_id": op_id, "kind": "receipt", "sale_id": sale_id, "to_phone": "33334444", "lang": "ar" }))
        .unwrap()
}

#[test]
fn whatsapp_receipt_is_gated_queued_once_and_retried() {
    let e = env();
    let t = &e.owner_token;
    let sale_id = sale(&e);
    let op_id = op();
    // Module off: refused in the backend.
    let err = e.core.wa_queue(t, receipt_req(&sale_id, &op_id)).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(err.details.unwrap()["kind"], "feature_disabled");
    features(&e, json!({ "whatsapp": true }));
    let m = e.core.wa_queue(t, receipt_req(&sale_id, &op_id)).unwrap();
    assert_eq!(m.status, "queued");
    assert_eq!(m.to_phone, "+97333334444");
    assert!(m.body.contains("الإيصال"), "{}", m.body);
    assert!(m.document_name.as_deref().unwrap().ends_with(".pdf"));
    // Same operation id → same message.
    assert_eq!(e.core.wa_queue(t, receipt_req(&sale_id, &op_id)).unwrap().message_id, m.message_id);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_outbox"), 1);
    // The PDF exists and is a PDF.
    let jobs = e.core.wa_claim_due(10).unwrap();
    assert_eq!(jobs.len(), 1);
    let pdf = std::fs::read(jobs[0].document_path.as_ref().unwrap()).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4"));
    // Claimed messages are not claimed twice.
    assert!(e.core.wa_claim_due(10).unwrap().is_empty());
    // A transient failure re-queues with back-off; a permanent one fails.
    e.core.wa_send_result(&m.message_id, Err(("not ready".into(), false))).unwrap();
    assert_eq!(count(&e, "SELECT COUNT(*) FROM wa_outbox WHERE status='queued' AND next_attempt_at > created_at"), 1);
    e.core.wa_send_result(&m.message_id, Err(("This number is not on WhatsApp.".into(), true))).unwrap();
    let row = &e.core.wa_outbox_list(t, None, None).unwrap()[0];
    assert_eq!(row.status, "failed");
    // Retry puts it back; success marks it sent.
    e.core.wa_outbox_action(t, &m.message_id, "retry").unwrap();
    e.core.wa_send_result(&m.message_id, Ok(Some("WAMID1".into()))).unwrap();
    assert_eq!(e.core.wa_outbox_list(t, None, None).unwrap()[0].status, "sent");
    assert_eq!(e.core.wa_outbox_action(t, &m.message_id, "cancel").unwrap_err().code, ErrorCode::Conflict);
    // A cashier may send receipts but not free text.
    let (_, cashier) = e.user("Sara", "role_cashier", "2468");
    e.core.wa_queue(&cashier, receipt_req(&sale_id, &op())).unwrap();
    let text: QueueRequest =
        serde_json::from_value(json!({ "operation_id": op(), "kind": "text", "to_phone": "33334444", "text": "hi" })).unwrap();
    assert_eq!(e.core.wa_queue(&cashier, text).unwrap_err().code, ErrorCode::Forbidden);
}

#[test]
fn pdf_receipt_failure_never_affects_the_sale() {
    let e = env();
    features(&e, json!({ "pdf_receipts": true }));
    // Make the receipts folder unusable: a file where the folder should be.
    std::fs::write(e.dir.path().join("receipts"), b"not a folder").unwrap();
    let sale_id = sale(&e);
    assert_eq!(count(&e, &format!("SELECT COUNT(*) FROM sales WHERE sale_id='{sale_id}'")), 1);
}

#[test]
fn payment_screenshot_review_flow() {
    let e = env();
    let t = &e.owner_token;
    features(&e, json!({ "whatsapp": true, "ocr": true, "payment_reviews": true }));
    let d = e
        .core
        .delivery_create(
            t,
            serde_json::from_value(json!({ "phone": "33334444", "area": "Riffa", "amount_minor": 12_500, "payment_status": "pending" }))
                .unwrap(),
        )
        .unwrap();
    let img = e.dir.path().join("wa-shot.png");
    std::fs::write(&img, b"fake png bytes").unwrap();
    let msg = |id: &str| {
        json!({ "id": id, "chat": "97333334444@s.whatsapp.net", "ts": 1_790_000_000, "type": "image", "caption": "paid",
                "media": { "path": img.to_string_lossy(), "mime": "image/png", "sha256": "abc123" } })
    };
    let imgs = e.core.wa_ingest(&[msg("M1"), msg("M1")]).unwrap();
    assert_eq!(imgs.len(), 1, "same WhatsApp message is stored once");
    let ids = e.core.pr_from_inbox(&imgs).unwrap();
    let r = &e.core.pr_list(t, Some("open".into())).unwrap()[0];
    assert_eq!(r.review_id, ids[0]);
    assert_eq!(r.expected_minor, Some(12_500), "expected amount comes from the open delivery for that phone");
    assert_eq!(r.delivery_id.as_deref(), Some(d.delivery_id.as_str()));
    let jobs = e.core.ocr_pending(5).unwrap();
    assert_eq!(jobs.iter().filter(|j| j.kind == "payment").count(), 1);
    e.core.ocr_result("payment", &ids[0], Ok(("BenefitPay\nAmount BHD 12.500\nRef 99887766".into(), 91))).unwrap();
    let r = &e.core.pr_list(t, None).unwrap()[0];
    assert_eq!((r.status.as_str(), r.detected_minor), ("matched", Some(12_500)));
    // The same image again is flagged as a duplicate, never auto-matched.
    let imgs = e.core.wa_ingest(&[msg("M2")]).unwrap();
    let dup = e.core.pr_from_inbox(&imgs).unwrap();
    e.core.ocr_result("payment", &dup[0], Ok(("Amount BHD 12.500".into(), 95))).unwrap();
    let r2 = e.core.pr_list(t, None).unwrap().into_iter().find(|x| x.review_id == dup[0]).unwrap();
    assert_eq!((r2.status.as_str(), r2.reason.as_deref()), ("needs_review", Some("duplicate_image")));
    // Confirming a non-matched review needs a note; a cashier cannot review.
    let decide =
        |id: &str, note: Option<&str>| serde_json::from_value(json!({ "review_id": id, "decision": "confirm", "note": note })).unwrap();
    assert_eq!(e.core.pr_decide(t, decide(&dup[0], None)).unwrap_err().code, ErrorCode::Validation);
    let (_, cashier) = e.user("Sara", "role_cashier", "2468");
    assert_eq!(e.core.pr_decide(&cashier, decide(&ids[0], None)).unwrap_err().code, ErrorCode::Forbidden);
    // Confirm the matched one: delivery becomes paid, with an event and audit row.
    e.core.pr_decide(t, decide(&ids[0], None)).unwrap();
    assert_eq!(
        count(&e, &format!("SELECT COUNT(*) FROM delivery_orders WHERE delivery_id='{}' AND payment_status='paid'", d.delivery_id)),
        1
    );
    assert_eq!(count(&e, "SELECT COUNT(*) FROM audit_logs WHERE event_type='payment_review.confirmed'"), 1);
    assert_eq!(e.core.pr_decide(t, decide(&ids[0], None)).unwrap_err().code, ErrorCode::Conflict);
}

#[test]
fn invoice_scan_creates_only_a_draft_order() {
    let e = env();
    let t = &e.owner_token;
    let milk = e.product("Milk Full Cream 1L", "6291041500213", 600, 400, 0);
    let rice = e.product("Basmati Rice 5kg", "6290000000011", 4_000, 3_000, 0);
    let sup = e.core.supplier_save(t, None, serde_json::from_value(json!({ "name": "ACME" })).unwrap()).unwrap();
    let img = e.dir.path().join("invoice.jpg");
    std::fs::write(&img, b"jpeg").unwrap();
    assert_eq!(e.core.inv_import(t, &img.to_string_lossy(), None).unwrap_err().details.unwrap()["kind"], "feature_disabled");
    features(&e, json!({ "ocr": true }));
    let scan = e.core.inv_import(t, &img.to_string_lossy(), Some(sup.supplier_id.clone())).unwrap();
    assert_eq!(scan.status, "imported");
    let text = "Invoice No: INV-7\n6291041500213 Milk 12 x 0.420 5.040\nRice Basmati 5kg 2 3.100 6.200\nMystery item 1.000\nTotal 12.240";
    e.core.ocr_result("invoice", &scan.scan_id, Ok((text.into(), 88))).unwrap();
    let v = e.core.inv_get(t, &scan.scan_id).unwrap();
    assert_eq!(v["scan"]["status"], "review");
    assert_eq!(v["scan"]["invoice_number"], "INV-7");
    let lines = v["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3);
    assert_eq!((lines[0]["match_kind"].as_str(), lines[0]["product_id"].as_str()), (Some("barcode"), Some(milk.as_str())));
    assert_eq!((lines[1]["match_kind"].as_str(), lines[1]["product_id"].as_str()), (Some("name"), Some(rice.as_str())));
    assert_eq!(lines[2]["match_kind"], "none");
    // Unmatched line blocks confirmation until excluded or matched.
    assert_eq!(e.core.inv_confirm(t, &scan.scan_id, &sup.supplier_id).unwrap_err().code, ErrorCode::Validation);
    e.core.inv_update_line(t, serde_json::from_value(json!({ "scan_id": scan.scan_id, "line_no": 3, "include": false })).unwrap()).unwrap();
    let v = e.core.inv_confirm(t, &scan.scan_id, &sup.supplier_id).unwrap();
    assert_eq!(v["scan"]["status"], "confirmed");
    let po = e.core.purchase_order_get(t, v["scan"]["po_id"].as_str().unwrap()).unwrap();
    assert_eq!(po.header.status, "draft");
    assert_eq!(po.lines.len(), 2);
    assert_eq!(po.header.total_minor, 5_040 + 6_200);
    // Never auto-posts stock.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_movements"), 0);
    assert_eq!(e.core.inv_confirm(t, &scan.scan_id, &sup.supplier_id).unwrap_err().code, ErrorCode::Conflict);
}
