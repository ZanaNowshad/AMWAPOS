//! Document Intelligence workflows on the core (OCR text supplied directly,
//! deterministic): matching, packs, new-product candidates, drafts that post
//! nothing until a person with receiving rights posts them, corrections and
//! learned mappings, duplicates, arithmetic/VAT discrepancies, PO and
//! three-way reconciliation, credit notes, permissions and AI validation.

mod common;

use amwapos_core::ErrorCode;
use common::*;
use serde_json::{json, Value};

fn features(e: &Env) {
    e.core.settings_save(&e.owner_token, "features", json!({ "ocr.enabled": true, "ocr.supplier_invoices": true })).unwrap();
}

fn count(e: &Env, sql: &str) -> i64 {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

/// Upload bytes and feed OCR text (as the worker would).
fn read_doc(e: &Env, token: &str, name: &str, bytes: &[u8], text: &str) -> Value {
    let s = e.core.doc_import(token, name, &amwapos_core::ids::b64(bytes), None).unwrap();
    e.core.ocr_result("invoice", &s.scan_id, Ok((text.into(), 90))).unwrap();
    e.core.doc_get(token, &s.scan_id).unwrap()
}

fn line(v: &Value, no: i64) -> &Value {
    v["lines"].as_array().unwrap().iter().find(|l| l["line_no"] == no).unwrap()
}

fn supplier(e: &Env, name: &str, vat: &str) -> String {
    e.core
        .supplier_save(&e.owner_token, None, serde_json::from_value(json!({ "name": name, "vat_number": vat })).unwrap())
        .unwrap()
        .supplier_id
}

const INVOICE: &str = "Al Waha Trading Co. W.L.L.\nVAT No: 200011122233344\nTAX INVOICE\nInvoice No: INV-00123   Date: 28/09/2026\n\
    6291041500213 Milk Full Cream 1L 10 PCS 0.450 4.500\nCOKE REG 24x330ML 2 CTN 7.200 14.400\nDragon Fruit Premium 3 PCS 1.000 3.000\n\
    Subtotal 21.900\nVAT 10% 2.190\nGrand Total 24.090";

#[test]
fn invoice_to_drafts_without_posting() {
    let e = env();
    features(&e);
    let t = &e.owner_token;
    let sup = supplier(&e, "Al Waha Trading", "200011122233344");
    let milk = e.product("Milk Full Cream 1L", "6291041500213", 600, 400, 0);
    let coke = e.product("Coca-Cola Original 330 ml", "5449000000996", 150, 100, 0);
    let _big = e.product("Coca-Cola Original 1.5 L", "5449000054227", 600, 450, 0);

    let v = read_doc(&e, t, "inv.jpg", b"img-1", INVOICE);
    assert_eq!(v["scan"]["status"], "review");
    assert_eq!(v["classification"]["doc_type"], "invoice");
    assert_eq!(v["supplier_match"]["id"], json!(sup));
    assert_eq!(v["supplier_match"]["kind"], "vat");
    // Barcode: high; fuzzy name with the same size: medium; unknown: a new-product candidate.
    let (l1, l2, l3) = (line(&v, 1), line(&v, 2), line(&v, 3));
    assert_eq!((l1["product_id"].as_str(), l1["match_band"].as_str()), (Some(milk.as_str()), Some("high")));
    assert_eq!(l2["product_id"].as_str(), Some(coke.as_str()), "{l2}");
    assert_eq!(l2["match_band"], "medium", "{l2}");
    assert!(l2["reasons"].to_string().contains("size matches"), "{l2}");
    assert_eq!((l2["units_per_case"].as_i64(), l2["base_qty_milli"].as_i64()), (Some(24), Some(48_000)));
    assert!(l3["product_id"].is_null() && l3["new_product"] == true, "{l3}");
    // Receive cost per single unit: 14.400 / 48 = 0.300.
    let r2 = v["receive"].as_array().unwrap().iter().find(|r| r["line_no"] == 2).unwrap();
    assert_eq!((r2["receive_qty_milli"].as_i64(), r2["receive_unit_cost_minor"].as_i64()), (Some(48_000), Some(300)));
    assert_eq!(v["validation"]["arithmetic_ok"], true, "{}", v["validation"]);
    assert_eq!(v["validation"]["vat_ok"], true, "{}", v["validation"]);
    let summary = v["summary"].as_str().unwrap();
    assert!(
        summary.starts_with(
            "Al Waha Trading invoice INV-00123 dated 28 Sep 2026. 3 lines, subtotal 21.900 BHD, VAT 2.190 BHD, total 24.090 BHD."
        ),
        "{summary}"
    );
    assert!(summary.contains("1 new product candidate") && summary.contains("Invoice total arithmetic is valid"), "{summary}");

    // A proposed product uses only the document's evidence and creates nothing.
    let np = e.core.doc_new_product_draft(t, v["scan"]["scan_id"].as_str().unwrap(), 3).unwrap();
    assert_eq!(np["name"], "Dragon Fruit Premium");
    assert_eq!(np["purchase_cost_minor"], 1_000);
    assert!(np["selling_price"].is_null() && np["created"] == false);
    let products_before = count(&e, "SELECT COUNT(*) FROM products");

    let id = v["scan"]["scan_id"].as_str().unwrap().to_string();
    let rev = v["revision"].as_i64().unwrap();
    // The unmatched line blocks a receiving draft until it is matched or excluded.
    assert_eq!(e.core.doc_create_receiving(t, &id, rev).unwrap_err().code, ErrorCode::Validation);
    // Stale revision: someone else changed the document.
    let stale =
        e.core.doc_update_line(t, &id, serde_json::from_value(json!({ "revision": rev - 1, "line_no": 3, "include": false })).unwrap());
    assert_eq!(stale.unwrap_err().code, ErrorCode::Conflict);
    let v = e
        .core
        .doc_update_line(t, &id, serde_json::from_value(json!({ "revision": rev, "line_no": 3, "include": false })).unwrap())
        .unwrap();
    let rev = v["revision"].as_i64().unwrap();
    let d = e.core.doc_create_receiving(t, &id, rev).unwrap();
    assert_eq!(d["status"], "draft");
    let dl = d["lines"].as_array().unwrap();
    assert_eq!(dl.len(), 2);
    assert_eq!((dl[1]["qty_milli"].as_i64(), dl[1]["unit_cost_minor"].as_i64()), (Some(48_000), Some(300)));
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_movements WHERE type='receive'"), 0, "a draft posts nothing");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM products"), products_before, "no product was created");
    // A second draft for the same document is refused.
    let rev = e.core.doc_get(t, &id).unwrap()["revision"].as_i64().unwrap();
    assert_eq!(e.core.doc_create_receiving(t, &id, rev).unwrap_err().code, ErrorCode::Conflict);

    // Supplier invoice draft: a record for review; approving creates no payable, payment or stock.
    let si = e.core.doc_create_supplier_invoice(t, &id, rev).unwrap();
    assert_eq!(
        (si["status"].as_str(), si["doc_type"].as_str(), si["posting"].as_str()),
        (Some("draft"), Some("invoice"), Some("not_posted"))
    );
    assert_eq!(si["total_minor"], 24_090);
    assert_eq!(si["receiving_draft_id"], d["draft_id"]);
    let si = e.core.supplier_invoice_set_status(t, si["invoice_id"].as_str().unwrap(), "approved").unwrap();
    assert_eq!(si["status"], "approved");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_movements WHERE type='receive'"), 0);

    // Posting needs receiving rights: a cashier cannot, the inventory role can.
    let (_, cashier) = e.user("Cash", "role_cashier", "1357");
    let draft = d["draft_id"].as_str().unwrap();
    assert_eq!(e.core.receiving_draft_post(&cashier, draft, &op()).unwrap_err().code, ErrorCode::Forbidden);
    let (_, stock) = e.user("Stock", "role_inventory", "2468");
    let post_op = op();
    let posted = e.core.receiving_draft_post(&stock, draft, &post_op).unwrap();
    assert_eq!(posted["status"], "posted");
    assert_eq!(
        count(&e, &format!("SELECT COALESCE(SUM(qty_delta_milli),0) FROM stock_movements WHERE type='receive' AND product_id='{coke}'")),
        48_000
    );
    // Retrying is harmless.
    assert_eq!(e.core.receiving_draft_post(&stock, draft, &post_op).unwrap()["status"], "posted");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_movements WHERE type='receive'"), 2);
}

#[test]
fn permissions_follow_roles() {
    let e = env();
    features(&e);
    let (_, cashier) = e.user("Cash", "role_cashier", "1357");
    assert_eq!(e.core.doc_import(&cashier, "x.jpg", &amwapos_core::ids::b64(b"x"), None).unwrap_err().code, ErrorCode::Forbidden);
    let (_, acct) = e.user("Acct", "role_accountant", "1368");
    // Accountants read supplier invoices (payables.view) but never approve or post them.
    assert!(e.core.supplier_invoices_list(&acct, None).is_ok());
    assert_eq!(e.core.ap_invoice_approve(&acct, "01XXXXXXXXXXXXXXXXXXXXXXXX").unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(e.core.ap_invoice_post(&acct, "01XXXXXXXXXXXXXXXXXXXXXXXX", "op-0123456789abcdef").unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(e.core.supplier_invoices_list(&cashier, None).unwrap_err().code, ErrorCode::Forbidden);
    let (_, stock) = e.user("Stock", "role_inventory", "2468");
    assert!(e.core.doc_import(&stock, "x.jpg", &amwapos_core::ids::b64(b"x"), None).is_ok());
}

#[test]
fn corrections_are_learned_and_reused() {
    let e = env();
    features(&e);
    let t = &e.owner_token;
    let sup = supplier(&e, "Gulf Foods", "200099988877766");
    let juice = e.product("Orange Juice Fresh 1 L", "6290000000288", 900, 600, 0);
    let text = "AWT Foodstuff\nInvoice No: 77\nDate 28/09/2026\nOJ FRSH 1LTR X6 2 CTN 12.000 24.000\nTotal 24.000";
    let v = read_doc(&e, t, "a.jpg", b"a", text);
    let id = v["scan"]["scan_id"].as_str().unwrap().to_string();
    assert!(v["supplier_match"]["id"].is_null(), "an unknown name is not guessed: {}", v["supplier_match"]);
    let rev = v["revision"].as_i64().unwrap();
    let v = e.core.doc_update(&id.clone(), &id, Default::default()).err().map(|x| x.code);
    assert_eq!(v, Some(ErrorCode::Unauthenticated));
    let v = e.core.doc_update(t, &id, serde_json::from_value(json!({ "revision": rev, "supplier_id": sup })).unwrap()).unwrap();
    let rev = v["revision"].as_i64().unwrap();
    let v = e
        .core
        .doc_update_line(
            t,
            &id,
            serde_json::from_value(json!({ "revision": rev, "line_no": 1, "product_id": juice, "units_per_case": 6 })).unwrap(),
        )
        .unwrap();
    let l = line(&v, 1);
    assert_eq!(
        (l["match_kind"].as_str(), l["base_qty_milli"].as_i64(), l["corrected"].as_bool()),
        (Some("manual"), Some(12_000), Some(true))
    );
    assert_eq!(count(&e, "SELECT COUNT(*) FROM ai_decisions WHERE kind='line_correction' AND source='person'"), 1);
    // Next document: the name alias and the supplier line mapping (with pack size) are remembered.
    let v = read_doc(&e, t, "b.jpg", b"b", &text.replace("77", "78"));
    assert_eq!(v["supplier_match"]["id"], json!(sup), "{}", v["supplier_match"]);
    assert_eq!(v["supplier_match"]["kind"], "alias");
    let l = line(&v, 1);
    assert_eq!(
        (l["product_id"].as_str(), l["match_kind"].as_str(), l["match_band"].as_str()),
        (Some(juice.as_str()), Some("supplier_map"), Some("high"))
    );
    assert_eq!(l["base_qty_milli"], 12_000, "pack size from the confirmed mapping: {l}");
}

#[test]
fn duplicates_discrepancies_and_anomalies() {
    let e = env();
    features(&e);
    let t = &e.owner_token;
    supplier(&e, "Al Waha Trading", "200011122233344");
    e.product("Milk Full Cream 1L", "6291041500213", 600, 400, 0);
    let a = read_doc(&e, t, "a.jpg", b"same-bytes", INVOICE);
    // The same file again: exact duplicate. A rescan: same invoice.
    let b = read_doc(&e, t, "b.jpg", b"same-bytes", INVOICE);
    assert!(b["duplicates"].to_string().contains("exact_document"), "{}", b["duplicates"]);
    let c = read_doc(&e, t, "c.jpg", b"other-scan", INVOICE);
    assert!(c["duplicates"].to_string().contains("same_invoice"), "{}", c["duplicates"]);
    assert!(c["anomalies"].to_string().contains("duplicate_number"), "{}", c["anomalies"]);
    assert!(a["duplicates"].as_array().unwrap().is_empty());
    // A different invoice with the same amount is not a duplicate by amount alone.
    let d = read_doc(&e, t, "d.jpg", b"d", &INVOICE.replace("INV-00123", "INV-00124").replace("28/09/2026", "29/09/2026"));
    assert!(!d["duplicates"].to_string().contains("same_invoice"), "{}", d["duplicates"]);

    // Totals that do not add up, and VAT that does not match the rate.
    let bad =
        INVOICE.replace("Grand Total 24.090", "Grand Total 24.590").replace("VAT 10% 2.190", "VAT 10% 2.090").replace("INV-00123", "INV-5");
    let v = read_doc(&e, t, "e.jpg", b"e", &bad);
    assert_eq!(v["validation"]["arithmetic_ok"], false);
    let issues = v["validation"]["issues"].to_string();
    assert!(
        issues.contains("Printed grand total: 24.590 BHD. Calculated from extracted lines: 23.990 BHD. Difference: 0.600 BHD."),
        "{issues}"
    );
    assert!(issues.contains("vat_mismatch"), "{issues}");
    assert!(
        v["anomalies"].to_string().contains("Document anomaly") || v["anomalies"].to_string().contains("total_arithmetic"),
        "{}",
        v["anomalies"]
    );
    assert!(!v["anomalies"].to_string().to_lowercase().contains("fraud"));
}

#[test]
fn leaving_a_line_out_of_receiving_keeps_the_document_checks() {
    let e = env();
    features(&e);
    let t = &e.owner_token;
    supplier(&e, "Al Waha Trading", "200011122233344");
    let v = read_doc(&e, t, "a.jpg", b"a", &INVOICE.replace("Grand Total 24.090", "Grand Total 24.090\nDelivered by Ahmed 1 0.000 0.000"));
    let id = v["scan"]["scan_id"].as_str().unwrap().to_string();
    assert_eq!(v["validation"]["arithmetic_ok"], true, "{}", v["validation"]);
    // Dragon fruit (line 3) is not received, but it is still on the invoice.
    let rev = v["revision"].as_i64().unwrap();
    let v = e
        .core
        .doc_update_line(t, &id, serde_json::from_value(json!({ "revision": rev, "line_no": 3, "include": false })).unwrap())
        .unwrap();
    assert_eq!(line(&v, 3)["include"], false);
    assert_eq!(v["validation"]["arithmetic_ok"], true, "{}", v["validation"]);
    assert!(!v["anomalies"].to_string().contains("do not add up"), "{}", v["anomalies"]);
    // A misread row marked "not an item" leaves the checks and the drafts.
    let extra = v["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["raw_text"].as_str().unwrap_or("").contains("Delivered"))
        .map(|l| l["line_no"].as_i64().unwrap());
    if let Some(no) = extra {
        let rev = v["revision"].as_i64().unwrap();
        let v = e
            .core
            .doc_update_line(t, &id, serde_json::from_value(json!({ "revision": rev, "line_no": no, "not_item": true })).unwrap())
            .unwrap();
        assert_eq!(line(&v, no)["include"], false);
        assert!(line(&v, no)["flags"].to_string().contains("not_item"));
        assert_eq!(v["validation"]["arithmetic_ok"], true);
    }
}

#[test]
fn purchase_order_and_three_way_match() {
    let e = env();
    features(&e);
    let t = &e.owner_token;
    let sup = supplier(&e, "Al Waha Trading", "200011122233344");
    let milk = e.product("Milk Full Cream 1L", "6291041500213", 600, 400, 0);
    let rice = e.product("Basmati Rice 5kg", "6290000000011", 4_000, 3_000, 0);
    let po = e
        .core
        .purchase_order_save(
            t,
            None,
            serde_json::from_value(json!({ "supplier_id": sup, "lines": [
                { "product_id": milk, "qty_milli": 10_000, "unit_cost_minor": 400, "tax_rate_bp": 0 },
                { "product_id": rice, "qty_milli": 2_000, "unit_cost_minor": 3_000, "tax_rate_bp": 0 } ] }))
            .unwrap(),
        )
        .unwrap();
    e.core.purchase_order_set_status(t, &po.header.po_id, "ordered").unwrap();
    // 8 of 10 milk arrived.
    e.core
        .purchase_order_receive(
            t,
            serde_json::from_value(json!({ "po_id": po.header.po_id, "operation_id": op(), "lines": [{ "po_item_id": po.lines[0].po_item_id, "qty_milli": 8_000 }] })).unwrap(),
        )
        .unwrap();
    let text = format!(
        "Al Waha Trading\nVAT No: 200011122233344\nTAX INVOICE\nInvoice No: 991\nPO No: {}\nDate 28/09/2026\n6291041500213 Milk Full Cream 1L 10 PCS 0.450 4.500\nTotal 4.500",
        po.header.po_number
    );
    let v = read_doc(&e, t, "p.jpg", b"p", &text);
    let r = &v["recon"];
    assert_eq!(r["po_id"], json!(po.header.po_id), "{r}");
    assert_eq!(r["three_way"], true);
    let lines = r["lines"].as_array().unwrap();
    let m = lines.iter().find(|l| l["product_id"] == json!(milk)).unwrap();
    let states = m["states"].to_string();
    assert!(states.contains("cost_variance") && states.contains("invoice_exceeds_received"), "{m}");
    assert_eq!(m["cost_variance_minor"], 50);
    assert_eq!(m["cost_variance_pct"], "+12.50%");
    assert!(m["notes"].to_string().contains("PO: 0.400 BHD  Invoice: 0.450 BHD  Variance: +0.050 BHD (+12.50%)"), "{m}");
    let r2 = lines.iter().find(|l| l["product_id"] == json!(rice)).unwrap();
    assert_eq!(r2["states"], json!(["missing_from_invoice"]));

    // A receiving draft on the PO posts through PO receiving (over-receipt refused there).
    let id = v["scan"]["scan_id"].as_str().unwrap();
    let d = e.core.doc_create_receiving(t, id, v["revision"].as_i64().unwrap()).unwrap();
    assert_eq!(d["po_id"], json!(po.header.po_id));
    assert!(d["lines"][0]["po_item_id"].is_string());
    let err = e.core.receiving_draft_post(t, d["draft_id"].as_str().unwrap(), &op()).unwrap_err();
    assert!(err.message.contains("exceed"), "{}", err.message);
    // A person corrects the draft to what is left on the order, then posts.
    e.core.receiving_draft_update_line(t, d["draft_id"].as_str().unwrap(), 1, Some(2_000), None, false).unwrap();
    assert_eq!(e.core.receiving_draft_post(t, d["draft_id"].as_str().unwrap(), &op()).unwrap()["status"], "posted");
    assert_eq!(e.core.purchase_order_get(t, &po.header.po_id).unwrap().lines[0].qty_received_milli, 10_000);
}

#[test]
fn credit_notes_and_unclear_types() {
    let e = env();
    features(&e);
    let t = &e.owner_token;
    supplier(&e, "Al Waha Trading", "200011122233344");
    e.product("Milk Full Cream 1L", "6291041500213", 600, 400, 0);
    let v = read_doc(&e, t, "cn.jpg", b"cn", "Al Waha Trading\nVAT No: 200011122233344\nCREDIT NOTE\nCredit Note No: CN-7\nDate 28/09/2026\n6291041500213 Milk Full Cream 1L 2 PCS 0.450 0.900\nTotal (0.900)");
    assert_eq!(v["classification"]["doc_type"], "credit_note");
    let id = v["scan"]["scan_id"].as_str().unwrap();
    let rev = v["revision"].as_i64().unwrap();
    let e1 = e.core.doc_create_receiving(t, id, rev).unwrap_err();
    assert!(e1.message.contains("credit note does not receive stock"), "{}", e1.message);
    let si = e.core.doc_create_supplier_invoice(t, id, rev).unwrap();
    assert_eq!((si["doc_type"].as_str(), si["posting"].as_str()), (Some("credit_note"), Some("not_posted")));
    assert_eq!(count(&e, "SELECT COUNT(*) FROM stock_movements"), 0);
    // Unclear type: no financially consequential reading is chosen for the user.
    let v = read_doc(&e, t, "u.jpg", b"u", "Al Waha Trading\nVAT No: 200011122233344\nInvoice No: 5\n6291041500213 Milk Full Cream 1L 2 PCS 0.450 0.900\nTotal 0.900\nThis credit note cancels invoice 4");
    assert_eq!(v["classification"]["doc_type"], "unknown", "{}", v["classification"]);
    let id = v["scan"]["scan_id"].as_str().unwrap();
    let err = e.core.doc_create_supplier_invoice(t, id, v["revision"].as_i64().unwrap()).unwrap_err();
    assert!(err.message.contains("document type"), "{}", err.message);
}

#[test]
fn ai_reading_is_validated_and_never_overrides_people() {
    let e = env();
    features(&e);
    let t = &e.owner_token;
    let milk = e.product("Milk Full Cream 1L", "6291041500213", 600, 400, 0);
    let v = read_doc(&e, t, "g.jpg", b"g", "gar#bled ~~ text\nMlk Fll Crm 10 0.450 4.600\nTotal 4.500");
    let id = v["scan"]["scan_id"].as_str().unwrap().to_string();
    // A fabricated product id and an invalid amount are rejected; valid values apply.
    let reply = json!({ "doc_type": "invoice", "invoice_number": "A-1", "total": "4.500",
        "lines": [{ "src_line": 1, "description": "Milk Full Cream 1L", "qty": "10", "unit_cost": "0.450", "line_total": "4.500", "product_id": "01FAKEPRODUCT0000000000000" }] });
    assert!(e.core.doc_apply_ai(&id, &reply, "test:model").unwrap());
    let v = e.core.doc_get(t, &id).unwrap();
    assert_eq!(v["fields"]["invoice_number"]["value"], "A-1");
    assert_eq!(v["fields"]["invoice_number"]["source"], "ai");
    let l = line(&v, 1);
    assert_eq!(l["product_id"].as_str(), Some(milk.as_str()), "matched by name, not by the fabricated id: {l}");
    assert!(
        count(&e, "SELECT COUNT(*) FROM ai_decisions WHERE kind='ai_extraction' AND data_json LIKE '%not one of the candidates%'") == 1
    );
    assert_eq!(v["ai_model"], "test:model");
    // After a person's correction the AI no longer replaces lines.
    let rev = v["revision"].as_i64().unwrap();
    e.core.doc_update_line(t, &id, serde_json::from_value(json!({ "revision": rev, "line_no": 1, "qty_milli": 12_000 })).unwrap()).unwrap();
    e.core
        .doc_apply_ai(&id, &json!({ "lines": [{ "src_line": 1, "description": "X", "qty": "1", "line_total": "0.100" }] }), "test:model")
        .unwrap();
    assert_eq!(line(&e.core.doc_get(t, &id).unwrap(), 1)["qty_milli"], 12_000);
    assert!(!e.core.doc_apply_ai(&id, &json!("not json object"), "test:model").unwrap());
}

#[test]
fn an_ai_reading_that_started_before_a_person_edited_is_dropped() {
    let e = env();
    features(&e);
    let t = &e.owner_token;
    e.product("Milk Full Cream 1L", "6291041500213", 600, 400, 0);
    let v = read_doc(&e, t, "s.jpg", b"s", "gar#bled\nMlk Fll Crm 10 0.450 4.600\nTotal 4.500");
    let id = v["scan"]["scan_id"].as_str().unwrap().to_string();
    // The AI starts reading at this revision...
    let started = e.core.doc_revision(&id).unwrap();
    // ...a person corrects the document meanwhile...
    e.core
        .doc_update_line(t, &id, serde_json::from_value(json!({ "revision": v["revision"], "line_no": 1, "qty_milli": 12_000 })).unwrap())
        .unwrap();
    // ...so the late result is dropped whole, header included, and recorded.
    let reply = json!({ "doc_type": "invoice", "invoice_number": "LATE-1", "total": "4.500",
        "lines": [{ "src_line": 1, "description": "Milk Full Cream 1L", "qty": "1", "unit_cost": "0.450", "line_total": "0.450" }] });
    assert!(!e.core.doc_apply_ai_at(&id, &reply, "test:model", Some(started)).unwrap());
    let v = e.core.doc_get(t, &id).unwrap();
    assert_ne!(v["fields"]["invoice_number"]["value"], "LATE-1");
    assert_eq!(line(&v, 1)["qty_milli"], 12_000);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM ai_decisions WHERE kind='ai_result_dropped'"), 1);
    // A reading that started at the current revision still applies.
    let now = e.core.doc_revision(&id).unwrap();
    assert!(e.core.doc_apply_ai_at(&id, &reply, "test:model", Some(now)).unwrap());
}

#[test]
fn an_unclear_pack_is_asked_about_not_reported_as_a_price_jump() {
    let e = env();
    features(&e);
    let t = &e.owner_token;
    supplier(&e, "Al Waha Trading", "200011122233344");
    e.product("Coca-Cola Original 330 ml", "5449000000996", 150, 90, 0);
    // "24x330ML 2" with no CTN: 2 cases or 2 cans? The per-can cost is unknown.
    let v = read_doc(
        &e,
        t,
        "p.jpg",
        b"p",
        "Al Waha Trading\nVAT No: 200011122233344\nTAX INVOICE\nInvoice No: 77\nCoca-Cola 24x330ML 2 7.200 14.400\nTotal 14.400",
    );
    let l = line(&v, 1);
    assert_eq!(l["pack_clear"], false, "{l}");
    assert!(!v["anomalies"].to_string().contains("price_deviation"), "{}", v["anomalies"]);
    // Once a person says the 2 are cases of 24, the per-can cost is compared.
    let rev = v["revision"].as_i64().unwrap();
    let v = e
        .core
        .doc_update_line(
            t,
            v["scan"]["scan_id"].as_str().unwrap(),
            serde_json::from_value(json!({ "revision": rev, "line_no": 1, "unit": "ctn", "units_per_case": 24 })).unwrap(),
        )
        .unwrap();
    assert_eq!(line(&v, 1)["base_qty_milli"], 48_000);
    assert_eq!(v["receive"][0]["receive_unit_cost_minor"], 300);
    assert!(v["anomalies"].to_string().contains("0.300 BHD"), "{}", v["anomalies"]);
}
