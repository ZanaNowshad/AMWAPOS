//! Document Intelligence end to end with the real OCR worker and Tesseract:
//! invoices are rendered to images here (English, Arabic, rotated, blurred,
//! a two-page PDF with a text page and a scanned page), uploaded through the
//! runtime, read, classified, matched and checked — and nothing is posted.
//! Skips when Tesseract is not installed unless AMWAPOS_REQUIRE_OCR=1 (CI).

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::docintel::image::{self, GrayImage, Luma};
use amwapos_core::raster::{self, Place};
use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_hub::ocr_worker::OcrPaths;
use amwapos_hub::Runtime;
use serde_json::{json, Value};

async fn call(rt: &Arc<Runtime>, cmd: &str, token: Option<&str>, args: Value) -> Value {
    match rt.dispatch(cmd, token.map(|t| t.to_string()), args).await {
        Ok(v) => v,
        Err(e) => panic!("{cmd} failed: {} ({:?})", e.message, e.code),
    }
}

/// One text row: cells at x positions (px at 1×), rendered with the app's own
/// text shaper (Arabic included).
fn row(width: usize, cells: &[(&str, usize)]) -> raster::Bitmap {
    let mut line = raster::Bitmap::new(width, raster::metrics(false).1);
    for (i, (text, x)) in cells.iter().enumerate() {
        let end = cells.get(i + 1).map(|c| c.1).unwrap_or(width);
        let bm = raster::render(end.saturating_sub(*x).max(16), &[(text, Place::Left)], false, false);
        for y in 0..bm.height.min(line.height) {
            for xx in 0..bm.width {
                if bm.get(xx, y) && x + xx < line.width {
                    line.set(x + xx, y);
                }
            }
        }
    }
    line
}

/// A white page with the rows, scaled 2× (about 200 dpi for OCR).
fn page(rows: &[Vec<(&str, usize)>]) -> GrayImage {
    let width = 1000;
    let mut parts = vec![raster::Bitmap::new(width, 60)];
    for r in rows {
        parts.push(row(width, r));
        parts.push(raster::Bitmap::new(width, 8));
    }
    parts.push(raster::Bitmap::new(width, 60));
    let bm = raster::Bitmap::stack(&parts);
    let small = GrayImage::from_fn(bm.width as u32 + 80, bm.height as u32, |x, y| {
        let x = x as usize;
        if (40..40 + bm.width).contains(&x) && bm.get(x - 40, y as usize) {
            Luma([15])
        } else {
            Luma([250])
        }
    });
    image::imageops::resize(&small, small.width() * 2, small.height() * 2, image::imageops::FilterType::Triangle)
}

fn png(img: &GrayImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(vec![]);
    image::DynamicImage::ImageLuma8(img.clone()).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn jpeg(img: &GrayImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(vec![]);
    image::DynamicImage::ImageLuma8(img.clone()).write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

fn english_invoice() -> GrayImage {
    page(&[
        vec![("Al Waha Trading Co. W.L.L.", 0)],
        vec![("Tel: 17712345", 0), ("CR No: 45678-2", 420)],
        vec![("VAT No: 200011122233344", 0)],
        vec![("TAX INVOICE", 0)],
        vec![("Invoice No: INV-00123", 0), ("Date: 28/09/2026", 520)],
        vec![("Description", 0), ("Qty", 470), ("Price", 600), ("Amount", 780)],
        vec![("Milk Full Cream 1L", 0), ("10", 470), ("0.450", 600), ("4.500", 780)],
        vec![("Basmati Rice 5kg", 0), ("2", 470), ("3.100", 600), ("6.200", 780)],
        vec![("Subtotal", 560), ("10.700", 780)],
        vec![("VAT 10%", 560), ("1.070", 780)],
        vec![("Grand Total", 560), ("11.770", 780)],
    ])
}

/// A minimal PDF: page 1 with a text layer, page 2 a scanned JPEG.
fn two_page_pdf(text: &[&str], scan: &GrayImage) -> Vec<u8> {
    let jpg = jpeg(scan);
    let mut objs: Vec<Vec<u8>> = vec![];
    let mut content = String::from("BT /F1 12 Tf 50 780 Td ");
    for (i, l) in text.iter().enumerate() {
        if i > 0 {
            content.push_str("0 -18 Td ");
        }
        content.push_str(&format!("({}) Tj ", l.replace('(', "\\(").replace(')', "\\)")));
    }
    content.push_str("ET");
    objs.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objs.push(b"<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".to_vec());
    objs.push(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_vec());
    objs.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    let mut c5 = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    c5.extend_from_slice(content.as_bytes());
    c5.extend_from_slice(b"\nendstream");
    objs.push(c5);
    objs.push(
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /XObject << /Im1 7 0 R >> >> /Contents 8 0 R >>".to_vec(),
    );
    let mut c7 = format!(
        "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
        scan.width(),
        scan.height(),
        jpg.len()
    )
    .into_bytes();
    c7.extend_from_slice(&jpg);
    c7.extend_from_slice(b"\nendstream");
    objs.push(c7);
    let draw = b"q 595 0 0 842 0 0 cm /Im1 Do Q";
    let mut c8 = format!("<< /Length {} >>\nstream\n", draw.len()).into_bytes();
    c8.extend_from_slice(draw);
    c8.extend_from_slice(b"\nendstream");
    objs.push(c8);
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![];
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

async fn upload(rt: &Arc<Runtime>, t: &str, name: &str, bytes: &[u8]) -> Value {
    let s = call(rt, "docs.import", Some(t), json!({ "file_name": name, "data": amwapos_core::ids::b64(bytes) })).await;
    let id = s["scan_id"].as_str().unwrap().to_string();
    let mut v = Value::Null;
    for _ in 0..1200 {
        v = call(rt, "docs.get", Some(t), json!({ "scan_id": id })).await;
        if v["scan"]["status"] != "imported" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn documents_are_read_checked_and_never_posted() {
    let exe = OcrPaths::discover(None).tesseract.unwrap();
    let have = std::process::Command::new(&exe).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    if !have {
        assert!(std::env::var("AMWAPOS_REQUIRE_OCR").is_err(), "Tesseract is required in CI");
        eprintln!("skipped: tesseract not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let core = Arc::new(AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap());
    let rt = Runtime::with_bind(core.clone(), Ipv4Addr::LOCALHOST, Duration::from_millis(500));
    call(
        &rt,
        "setup.initialize",
        None,
        json!({ "business_name": "Al Noor Supermarket", "vat_number": "200000000000003", "branch_name": "Main", "vat_rate_bp": 1000,
                "owner_name": "Owner", "owner_pin": "4826", "device_name": "Till", "device_code": "T01" }),
    )
    .await;
    let owner = call(&rt, "auth.users", None, json!({})).await[0]["user_id"].as_str().unwrap().to_string();
    let t = call(&rt, "auth.login", None, json!({ "user_id": owner, "pin": "4826" })).await["token"].as_str().unwrap().to_string();
    rt.ocr.set_paths(OcrPaths::discover(None));
    call(&rt, "settings.save", Some(&t), json!({ "key": "features", "value": { "ocr.enabled": true, "ocr.supplier_invoices": true } }))
        .await;
    let tax = call(&rt, "tax.list", Some(&t), json!({})).await.as_array().unwrap().iter().find(|x| x["rate_bp"] == 1000).unwrap()
        ["tax_rule_id"]
        .clone();
    for (name, bc) in [("Milk Full Cream 1L", "6291041500213"), ("Basmati Rice 5kg", "6290000000011")] {
        call(
            &rt,
            "products.create",
            Some(&t),
            json!({ "name": name, "tax_rule_id": tax, "unit": "pcs", "price_minor": 1000, "cost_minor": 400, "barcodes": [bc], "opening_stock_milli": 0, "track_inventory": true }),
        )
        .await;
    }
    call(&rt, "suppliers.save", Some(&t), json!({ "supplier": { "name": "Al Waha Trading", "vat_number": "200011122233344" } })).await;
    let movements = || {
        core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM stock_movements WHERE type='receive'", [], |r| r.get::<_, i64>(0))?)).unwrap()
    };

    // ---- clean English invoice (PNG)
    let v = upload(&rt, &t, "invoice.png", &png(&english_invoice())).await;
    assert_eq!(v["scan"]["status"], "review", "{v}");
    assert_eq!(v["classification"]["doc_type"], "invoice", "{}", v["classification"]);
    assert_eq!(v["fields"]["invoice_number"]["value"], "INV-00123", "{}", v["fields"]);
    assert_eq!(v["fields"]["invoice_date"]["value"], "2026-09-28");
    assert_eq!(v["fields"]["supplier_vat"]["value"], "200011122233344");
    assert_eq!(v["supplier_match"]["kind"], "vat", "{}", v["supplier_match"]);
    assert_eq!(v["fields"]["total_minor"]["value"], 11_770);
    let lines = v["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().all(|l| l["product_id"].is_string()), "{lines:?}");
    assert!(lines[0]["evidence"]["bbox"].is_array(), "boxes kept for the review screen");
    assert_eq!(v["validation"]["arithmetic_ok"], true, "{}", v["validation"]);
    assert_eq!(v["validation"]["vat_ok"], true, "{}", v["validation"]);
    assert!(matches!(v["quality"]["status"].as_str(), Some("good" | "usable")), "{}", v["quality"]);
    assert!(v["summary"].as_str().unwrap().contains("Invoice total arithmetic is valid"), "{}", v["summary"]);

    // ---- the same invoice turned 90°: orientation found by OCR signal
    let rotated = image::imageops::rotate90(&english_invoice());
    if let Ok(d) = std::env::var("DI_DUMP") {
        std::fs::write(format!("{d}/rotated.png"), png(&rotated)).unwrap();
        std::fs::write(format!("{d}/english.png"), png(&english_invoice())).unwrap();
    }
    let v = upload(&rt, &t, "rotated.png", &png(&rotated)).await;
    assert_eq!(v["scan"]["status"], "review", "{v}");
    assert_eq!(v["fields"]["invoice_number"]["value"], "INV-00123", "{}", v["fields"]);
    assert!(v["quality"]["messages"].to_string().contains("turned"), "{}", v["quality"]);
    // Same supplier + number: flagged, not silently accepted.
    assert!(
        v["duplicates"].to_string().contains("same_invoice") || v["duplicates"].to_string().contains("same_scanned"),
        "{}",
        v["duplicates"]
    );

    // ---- blurred: quality says so; values are not invented
    let blurred = image::imageops::blur(&english_invoice(), 5.0);
    let v = upload(&rt, &t, "blurred.jpg", &jpeg(&blurred)).await;
    assert_eq!(v["scan"]["status"], "review", "{v}");
    assert_eq!(v["quality"]["status"], "poor", "{}", v["quality"]);
    assert!(v["quality"]["messages"].to_string().contains("blurred"), "{}", v["quality"]);

    // ---- Arabic invoice
    let ar = page(&[
        vec![("شركة الواحة للتجارة", 0)],
        vec![("الرقم الضريبي: 200011122233344", 0)],
        vec![("فاتورة ضريبية", 0)],
        vec![("رقم الفاتورة: 456", 0)],
        vec![("التاريخ: 28/09/2026", 0)],
        vec![("حليب كامل الدسم", 0), ("10", 470), ("0.450", 600), ("4.500", 780)],
        vec![("الإجمالي", 560), ("4.500", 780)],
    ]);
    let v = upload(&rt, &t, "arabic.png", &png(&ar)).await;
    assert_eq!(v["scan"]["status"], "review", "{v}");
    assert_eq!(v["classification"]["doc_type"], "invoice", "{} / {}", v["classification"], v["scan"]);
    assert_eq!(v["fields"]["supplier_vat"]["value"], "200011122233344", "{}", v["fields"]);
    assert_eq!(v["supplier_match"]["kind"], "vat");

    // ---- two-page PDF: text layer + scanned page
    let scan = page(&[
        vec![("Description", 0), ("Qty", 470), ("Price", 600), ("Amount", 780)],
        vec![("Basmati Rice 5kg", 0), ("4", 470), ("3.100", 600), ("12.400", 780)],
        vec![("Grand Total", 560), ("12.400", 780)],
    ]);
    let pdf =
        two_page_pdf(&["Al Waha Trading", "VAT No: 200011122233344", "TAX INVOICE", "Invoice No: INV-00991", "Date: 01/10/2026"], &scan);
    let v = upload(&rt, &t, "invoice.pdf", &pdf).await;
    assert_eq!(v["scan"]["status"], "review", "{v}");
    assert_eq!(v["page_count"], 2);
    assert_eq!(v["fields"]["invoice_number"]["value"], "INV-00991", "{}", v["fields"]);
    assert_eq!(v["supplier_match"]["kind"], "vat");
    let lines = v["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["qty_milli"], 4000);
    let page2 = call(&rt, "docs.page", Some(&t), json!({ "scan_id": v["scan"]["scan_id"], "page": 2 })).await;
    assert!(page2["image"].is_object() || page2["image"].is_string(), "the scanned page is viewable: {page2}");
    // A password-less damaged PDF is reported, not guessed.
    let e = rt
        .dispatch("docs.import", Some(t.clone()), json!({ "file_name": "x.pdf", "data": amwapos_core::ids::b64(b"hello") }))
        .await
        .unwrap_err();
    assert!(e.message.contains("not a PDF"), "{}", e.message);

    // Nothing was received or posted by reading documents.
    assert_eq!(movements(), 0);
    let si = core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM supplier_invoices", [], |r| r.get::<_, i64>(0))?)).unwrap();
    assert_eq!(si, 0);
}
