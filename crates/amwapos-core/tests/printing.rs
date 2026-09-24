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
