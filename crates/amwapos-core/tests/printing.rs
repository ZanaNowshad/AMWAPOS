mod common;

use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::FinalizeRequest;
use common::*;
use serde_json::json;

fn sell(e: &Env, barcode: &str) -> amwapos_core::sales::SaleResult {
    let t = &e.owner_token;
    let cart = e.core.pos_scan(t, barcode, None).unwrap().cart;
    let total = cart.totals.total_minor;
    e.core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![TenderInput { method: "cash".into(), amount_minor: total, reference: None }],
                approval_token: None,
                expected_total_minor: None,
            },
        )
        .unwrap()
}

#[test]
fn receipt_prints_after_commit_and_reprint_is_marked_copy() {
    let e = env();
    let t = &e.owner_token;
    e.product("Almarai Fresh Milk 1L", "6281007031126", 850, 500, 10_000);
    e.open_shift(t, 20_000);
    let out = e.dir.path().join("printer.txt");
    e.core
        .settings_save(
            t,
            "local.printer",
            json!({ "mode": "file", "target": out.to_string_lossy(), "paper_width_mm": 80, "cut": true, "drawer_pulse": false }),
        )
        .unwrap();

    let sale = sell(&e, "6281007031126");
    assert_eq!(sale.print.as_ref().unwrap().status, "printed");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains(&sale.receipt_number), "{text}");
    assert!(text.contains("TAX INVOICE"));
    assert!(text.contains("Almarai Fresh Milk 1L"));
    assert!(text.contains("0.850"));
    assert!(text.lines().all(|l| l.chars().count() <= 48), "line wider than 80 mm paper:\n{text}");

    let re = e.core.sale_reprint(t, &sale.sale_id).unwrap();
    assert_eq!(re.status, "printed");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("COPY"));
}

#[test]
fn printer_failure_never_undoes_the_sale() {
    let e = env();
    let t = &e.owner_token;
    e.product("Lays Salted 40g", "6281006511339", 300, 150, 10_000);
    e.open_shift(t, 0);
    // Nothing listens on the loopback discard port, so the connection is refused.
    e.core.settings_save(t, "local.printer", json!({ "mode": "network", "target": "127.0.0.1:9" })).unwrap();

    let sale = sell(&e, "6281006511339");
    assert_eq!(sale.print.as_ref().unwrap().status, "failed");
    let stored = e.core.sale_get(t, &sale.sale_id).unwrap();
    assert_eq!(stored.receipt_number, sale.receipt_number);
    let failed: i64 = e
        .core
        .db
        .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM print_jobs WHERE status='failed' AND kind='sale'", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(failed, 1);

    // Fix the printer and retry from the queue.
    let out = e.dir.path().join("printer.txt");
    e.core.settings_save(t, "local.printer", json!({ "mode": "file", "target": out.to_string_lossy() })).unwrap();
    let job = e.core.print_queue(t).unwrap().into_iter().find(|j| j.status == "failed" && j.kind == "sale").unwrap();
    assert_eq!(e.core.print_retry(t, &job.job_id).unwrap().status, "printed");
    assert!(std::fs::read_to_string(&out).unwrap().contains(&sale.receipt_number));
}

#[test]
fn escpos_output_has_init_and_cut() {
    let e = env();
    let t = &e.owner_token;
    e.product("Coca-Cola 330ml", "06291100001234", 250, 100, 10_000);
    e.open_shift(t, 0);
    let sale = sell(&e, "06291100001234");
    let doc = e.core.db.read(|c| amwapos_core::receipt::sale_receipt(c, &sale.sale_id, None)).unwrap();
    let bytes = doc.to_escpos(true);
    assert_eq!(&bytes[..2], b"\x1b@", "ESC @ initialise");
    assert!(bytes.windows(2).any(|w| w == b"\x1dV"), "GS V cut");
    assert!(!doc.to_escpos(false).windows(2).any(|w| w == b"\x1dV"));
}

/// Split an ESC/POS stream into (text-mode bytes, raster images).
fn split_escpos(bytes: &[u8]) -> (Vec<u8>, usize) {
    let mut text = vec![];
    let mut rasters = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(&[0x1D, 0x76, 0x30, 0x00]) {
            let bx = bytes[i + 4] as usize | (bytes[i + 5] as usize) << 8;
            let rows = bytes[i + 6] as usize | (bytes[i + 7] as usize) << 8;
            i += 8 + bx * rows;
            rasters += 1;
        } else {
            text.push(bytes[i]);
            i += 1;
        }
    }
    (text, rasters)
}

fn set_name_ar(e: &Env, product_id: &str, name_ar: &str) {
    e.core.db.write(|tx| Ok(tx.execute("UPDATE products SET name_ar=?2 WHERE product_id=?1", [product_id, name_ar])?)).unwrap();
}

#[test]
fn arabic_receipts_print_as_raster_never_as_question_marks() {
    let e = env();
    let t = &e.owner_token;
    // Arabic store name, Arabic product name, Arabic footer, bilingual labels.
    e.core.db.write(|tx| Ok(tx.execute("UPDATE business SET name='سوبرماركت النور'", [])?)).unwrap();
    let pid = e.product("Almarai Fresh Milk 1L", "6281007031126", 850, 500, 10_000);
    set_name_ar(&e, &pid, "حليب المراعي طازج");
    e.product("لبن كامل الدسم", "6281007000009", 650, 300, 10_000);
    e.core
        .settings_save(
            t,
            "receipt",
            json!({ "language": "bilingual", "footer_lines": ["شكراً لتسوقكم معنا"], "paper_width_mm": 80, "title": "TAX INVOICE" }),
        )
        .unwrap();
    e.open_shift(t, 0);
    e.core.pos_scan(t, "6281007031126", None).unwrap();
    let cart = e.core.pos_scan(t, "6281007000009", None).unwrap().cart;
    let total = cart.totals.total_minor;
    let sale = e
        .core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![TenderInput { method: "cash".into(), amount_minor: total, reference: None }],
                approval_token: None,
                expected_total_minor: None,
            },
        )
        .unwrap();

    let doc = e.core.db.read(|c| amwapos_core::receipt::sale_receipt(c, &sale.sale_id, None)).unwrap();
    let text = doc.to_text();
    assert!(text.contains("حليب المراعي طازج"), "Arabic name snapshot printed under the English name:\n{text}");
    assert!(text.contains("TOTAL / الإجمالي"));
    assert!(text.contains("TAX INVOICE / فاتورة ضريبية"));
    assert!(text.contains("شكراً لتسوقكم معنا"));

    let (text_mode, rasters) = split_escpos(&doc.to_escpos(true));
    assert!(rasters >= 8, "every Arabic line is a raster image ({rasters})");
    assert!(!text_mode.contains(&b'?'), "no substitution characters in text mode");
    assert!(text_mode.windows(11).any(|w| w == b"  1 x 0.850"), "ASCII lines still use text mode");

    // Snapshot: changing the catalogue later does not change the reprint.
    set_name_ar(&e, &pid, "اسم جديد");
    let doc = e.core.db.read(|c| amwapos_core::receipt::sale_receipt(c, &sale.sale_id, Some("COPY"))).unwrap();
    assert!(doc.to_text().contains("حليب المراعي طازج"));
    assert!(!doc.to_text().contains("اسم جديد"));
}

#[test]
fn english_receipts_stay_in_fast_text_mode() {
    let e = env();
    let t = &e.owner_token;
    e.product("Coca-Cola 330ml", "06291100001234", 250, 100, 10_000);
    e.open_shift(t, 0);
    let sale = sell(&e, "06291100001234");
    let doc = e.core.db.read(|c| amwapos_core::receipt::sale_receipt(c, &sale.sale_id, None)).unwrap();
    let (text_mode, rasters) = split_escpos(&doc.to_escpos(true));
    assert_eq!(rasters, 0);
    assert!(!text_mode.contains(&b'?'));
    assert!(e.core.settings_save(t, "receipt", json!({ "language": "fr" })).is_err());
}

fn jobs(e: &Env, sale_id: &str) -> Vec<(String, String)> {
    e.core
        .db
        .read(|c| {
            let mut st = c.prepare("SELECT kind, status FROM print_jobs WHERE ref_id=?1 ORDER BY created_at, kind DESC")?;
            let rows = st.query_map([sale_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .unwrap()
}

#[test]
fn cash_drawer_pulses_on_cash_sales_only() {
    let e = env();
    let t = &e.owner_token;
    e.product("Coca-Cola 330ml", "06291100001234", 250, 100, 10_000);
    e.open_shift(t, 0);
    let out = e.dir.path().join("printer.txt");
    e.core.settings_save(t, "local.printer", json!({ "mode": "file", "target": out.to_string_lossy(), "drawer_pulse": true })).unwrap();

    // Cash sale: receipt and drawer pulse are both queued in the sale's transaction and printed.
    let cash = sell(&e, "06291100001234");
    assert_eq!(jobs(&e, &cash.sale_id), vec![("sale".into(), "printed".into()), ("drawer".into(), "printed".into())]);
    assert!(std::fs::read_to_string(&out).unwrap().contains("[drawer pulse]"));

    // Card sale: receipt only, the drawer stays shut.
    let cart = e.core.pos_scan(t, "06291100001234", None).unwrap().cart;
    let card = e
        .core
        .pos_finalize(
            t,
            FinalizeRequest {
                cart_id: cart.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![TenderInput { method: "card".into(), amount_minor: cart.totals.total_minor, reference: Some("4421".into()) }],
                approval_token: None,
                expected_total_minor: None,
            },
        )
        .unwrap();
    assert_eq!(jobs(&e, &card.sale_id), vec![("sale".into(), "printed".into())]);

    // A reprint never opens the drawer.
    e.core.sale_reprint(t, &cash.sale_id).unwrap();
    let kinds: Vec<String> = jobs(&e, &cash.sale_id).into_iter().map(|j| j.0).collect();
    assert_eq!(kinds.iter().filter(|k| *k == "drawer").count(), 1);
    assert_eq!(kinds.iter().filter(|k| *k == "sale").count(), 2);

    // Drawer pulse turned off: no drawer job even for cash.
    e.core.settings_save(t, "local.printer", json!({ "mode": "file", "target": out.to_string_lossy(), "drawer_pulse": false })).unwrap();
    let cash2 = sell(&e, "06291100001234");
    assert_eq!(jobs(&e, &cash2.sale_id), vec![("sale".into(), "printed".into())]);
    // ESC p (kick pin 2) is what the printer receives for a pulse.
    assert_eq!(amwapos_core::receipt::drawer_pulse_bytes()[2..4], [0x1B, 0x70]);
}
