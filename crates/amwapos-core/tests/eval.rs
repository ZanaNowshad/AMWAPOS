//! Evaluation harness (synthetic dataset in `tests/fixtures/eval/`).
//!
//! Runs every supplier document and every WhatsApp message through the real
//! pipeline (OCR text → extraction → matching → checks; message → reading →
//! catalogue resolution → draft) and compares the structured result with what
//! a reviewer expects. It prints per-field accuracy and fails on any safety
//! violation: a product chosen that is not the expected one (a wrong product
//! is worse than none), or anything committed automatically. Quality
//! thresholds keep regressions visible; the printed table is the report.
//!
//! `AMWAPOS_EVAL_REPORT=path` also writes the report as JSON.

mod common;

use std::collections::BTreeMap;

use amwapos_core::messaging::Inbound;
use common::*;
use serde_json::{json, Value};

fn fixture(name: &str) -> Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eval").join(name);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[derive(Default)]
struct Score {
    right: BTreeMap<&'static str, u32>,
    total: BTreeMap<&'static str, u32>,
    misses: Vec<String>,
    unsafe_: Vec<String>,
}

impl Score {
    fn check(&mut self, field: &'static str, case: &str, want: &Value, got: &Value) {
        *self.total.entry(field).or_default() += 1;
        if want == got {
            *self.right.entry(field).or_default() += 1;
        } else {
            self.misses.push(format!("[{case}] {field}: expected {want}, got {got}"));
        }
    }
    fn pct(&self, field: &str) -> f64 {
        let t = *self.total.get(field).unwrap_or(&0);
        if t == 0 {
            return 100.0;
        }
        *self.right.get(field).unwrap_or(&0) as f64 * 100.0 / t as f64
    }
    fn report(&self, title: &str) -> Value {
        println!("\n== {title}");
        let mut out = serde_json::Map::new();
        for (f, t) in &self.total {
            let p = self.pct(f);
            println!("  {f:<22} {:>3}/{:<3} {p:6.1}%", self.right.get(f).unwrap_or(&0), t);
            out.insert(f.to_string(), json!({ "right": self.right.get(f).unwrap_or(&0), "total": t, "pct": (p * 10.0).round() / 10.0 }));
        }
        for m in &self.misses {
            println!("  miss: {m}");
        }
        for u in &self.unsafe_ {
            println!("  UNSAFE: {u}");
        }
        json!({ "fields": out, "misses": self.misses, "unsafe": self.unsafe_ })
    }
}

fn eval_documents() -> (Score, Value) {
    let data = fixture("invoices.json");
    let e = env();
    let t = &e.owner_token;
    e.core.settings_save(t, "features", json!({ "ocr.enabled": true, "ocr.supplier_invoices": true })).unwrap();
    for s in data["suppliers"].as_array().unwrap() {
        e.core.supplier_save(t, None, serde_json::from_value(s.clone()).unwrap()).unwrap();
    }
    for p in data["products"].as_array().unwrap() {
        e.product(
            p["name"].as_str().unwrap(),
            p["barcode"].as_str().unwrap(),
            p["price_minor"].as_i64().unwrap(),
            p["cost_minor"].as_i64().unwrap(),
            0,
        );
    }
    let stock = |e: &Env| -> i64 {
        e.core.db.read(|c| Ok(c.query_row("SELECT COALESCE(SUM(qty_milli),0) FROM stock_levels", [], |r| r.get(0))?)).unwrap()
    };
    let stock_before = stock(&e);
    let mut sc = Score::default();
    for (i, case) in data["cases"].as_array().unwrap().iter().enumerate() {
        let name = case["name"].as_str().unwrap();
        let s = e.core.doc_import(t, &format!("eval-{i}.jpg"), &amwapos_core::ids::b64(format!("eval-{i}").as_bytes()), None).unwrap();
        e.core.ocr_result("invoice", &s.scan_id, Ok((case["text"].as_str().unwrap().into(), 90))).unwrap();
        let v = e.core.doc_get(t, &s.scan_id).unwrap();
        let x = &case["expect"];
        let field = |k: &str| v["fields"][k]["value"].clone();
        if !x["doc_type"].is_null() {
            sc.check("doc_type", name, &x["doc_type"], &v["classification"]["doc_type"]);
        }
        if x.get("supplier").is_some() {
            let got = if v["scan"]["supplier_id"].is_null() { Value::Null } else { v["scan"]["supplier_name"].clone() };
            sc.check("supplier", name, &x["supplier"], &got);
            if !x["supplier"].is_null() && !got.is_null() && got != x["supplier"] {
                sc.unsafe_.push(format!("[{name}] wrong supplier chosen: {got}"));
            }
        }
        for k in ["invoice_number", "invoice_date", "subtotal_minor", "vat_minor", "total_minor"] {
            if let Some(w) = x.get(k) {
                let f: &'static str = match k {
                    "invoice_number" => "invoice_number",
                    "invoice_date" => "invoice_date",
                    "subtotal_minor" => "subtotal",
                    "vat_minor" => "vat",
                    _ => "total",
                };
                sc.check(f, name, w, &field(k));
            }
        }
        for k in ["arithmetic_ok", "vat_ok"] {
            if let Some(w) = x.get(k) {
                sc.check(if k == "arithmetic_ok" { "arithmetic_check" } else { "vat_check" }, name, w, &v["validation"][k]);
            }
        }
        if let Some(lines) = x["lines"].as_array() {
            let got = v["lines"].as_array().unwrap();
            sc.check("line_count", name, &json!(lines.len()), &json!(got.len()));
            for (j, w) in lines.iter().enumerate() {
                let Some(g) = got.get(j) else { continue };
                for k in ["qty_milli", "unit_cost_minor", "line_total_minor"] {
                    let f: &'static str = match k {
                        "qty_milli" => "line_qty",
                        "unit_cost_minor" => "line_unit_cost",
                        _ => "line_total",
                    };
                    sc.check(f, name, &w[k], &g[k]);
                }
                sc.check("line_product", name, &w["product"], &g["product_name"]);
                if !g["product_name"].is_null() && g["product_name"] != w["product"] {
                    sc.unsafe_.push(format!("[{name}] line {} matched to the wrong product {}", j + 1, g["product_name"]));
                }
            }
        }
    }
    // Reading documents never touches stock, costs or supplier liabilities.
    assert_eq!(stock(&e), stock_before, "reading documents moved stock");
    let n: i64 = e.core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM goods_receipts", [], |r| r.get(0))?)).unwrap();
    assert_eq!(n, 0, "a goods receipt was created without a person");
    let r = sc.report("Supplier documents");
    (sc, r)
}

fn eval_whatsapp() -> (Score, Value) {
    let data = fixture("whatsapp.json");
    let e = env();
    let t = &e.owner_token;
    e.core.settings_save(t, "features", json!({ "whatsapp.enabled": true, "orders.digital": true, "orders.whatsapp_ai": true })).unwrap();
    for p in data["products"].as_array().unwrap() {
        let price = p["price_minor"].as_i64().unwrap();
        e.product(p["name"].as_str().unwrap(), p["barcode"].as_str().unwrap(), price, price / 2, p["stock_milli"].as_i64().unwrap());
    }
    let mut sc = Score::default();
    for (i, case) in data["cases"].as_array().unwrap().iter().enumerate() {
        let name = case["name"].as_str().unwrap();
        let chat = format!("9733300{:04}@s.whatsapp.net", i);
        e.core
            .wa_ingest(&[Inbound {
                wa_id: format!("eval-{i}"),
                chat: chat.clone(),
                sender_pn: None,
                push_name: Some("Eval".into()),
                ts: chrono::Utc::now().timestamp(),
                kind: "text".into(),
                text: Some(case["text"].as_str().unwrap().into()),
                caption: None,
                media_mime: None,
                media_ref: None,
            }])
            .unwrap();
        e.core.wa_orders_process(50).unwrap();
        let x = &case["expect"];
        let intent: Option<String> = e
            .core
            .db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT p.intent FROM wa_inbox_processing p JOIN wa_inbox i ON i.seq=p.inbox_seq WHERE i.chat=?1 ORDER BY p.inbox_seq DESC LIMIT 1",
                    [&chat],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        sc.check("intent", name, &x["intent"], &json!(intent));
        let list = e.core.wa_orders_list(t, Some("all".into())).unwrap();
        let session = list.iter().find(|r| r["chat"] == json!(chat));
        if x["session"] == json!(false) {
            sc.check("no_session_for_non_order", name, &json!(false), &json!(session.is_some()));
            continue;
        }
        let Some(row) = session else {
            sc.check("session_created", name, &json!(true), &json!(false));
            continue;
        };
        let v = e.core.wa_order_get(t, row["session_id"].as_str().unwrap()).unwrap();
        if let Some(m) = x.get("mode") {
            sc.check("delivery_mode", name, m, &v["session"]["delivery_mode"]);
        }
        if let Some(b) = x.get("block") {
            sc.check("address_block", name, b, &v["session"]["address"]["parts"]["block"]);
        }
        if let Some(p) = x.get("priority") {
            let has = v["session"]["priority_reasons"].as_array().is_some_and(|a| a.contains(p));
            sc.check("priority", name, &json!(true), &json!(has));
        }
        if let Some(lines) = x["lines"].as_array() {
            let got: Vec<Value> = v["order"]["lines"].as_array().cloned().unwrap_or_default();
            sc.check("line_count", name, &json!(lines.len()), &json!(got.len()));
            for (j, w) in lines.iter().enumerate() {
                let Some(g) = got.get(j) else { continue };
                let chosen = if g["product_id"].is_null() { Value::Null } else { g["name"].clone() };
                sc.check("line_product", name, &w["product"], &chosen);
                sc.check("line_qty", name, &w["qty_milli"], &g["qty_milli"]);
                if let Some(r) = w.get("resolution") {
                    sc.check("line_resolution", name, r, &g["resolution"]);
                }
                if !chosen.is_null() && chosen != w["product"] {
                    sc.unsafe_.push(format!("[{name}] line {} resolved to the wrong product {chosen}", j + 1));
                }
            }
        }
        // Nothing is ever confirmed, paid or sent by reading a message.
        assert_eq!(v["order"]["status"].as_str().unwrap_or("draft"), "draft", "[{name}] order left draft by itself");
    }
    let sent: i64 = e.core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM wa_outbox", [], |r| r.get(0))?)).unwrap();
    assert_eq!(sent, 0, "a WhatsApp message was queued without a person");
    let sales: i64 = e.core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM sales", [], |r| r.get(0))?)).unwrap();
    assert_eq!(sales, 0);
    let r = sc.report("WhatsApp orders");
    (sc, r)
}

#[test]
fn evaluation_dataset() {
    let (d, dr) = eval_documents();
    let (w, wr) = eval_whatsapp();
    if let Ok(p) = std::env::var("AMWAPOS_EVAL_REPORT") {
        std::fs::write(p, serde_json::to_string_pretty(&json!({ "documents": dr, "whatsapp": wr })).unwrap()).unwrap();
    }
    // Safety first: never a wrong product or supplier.
    assert!(d.unsafe_.is_empty() && w.unsafe_.is_empty(), "unsafe results: {:?} {:?}", d.unsafe_, w.unsafe_);
    // Quality floors (the synthetic set is small: these guard regressions).
    for f in ["doc_type", "supplier", "invoice_number", "invoice_date", "total", "line_qty", "line_unit_cost", "line_total", "line_product"]
    {
        assert!(d.pct(f) >= 85.0, "documents {f} accuracy {:.1}% < 85%", d.pct(f));
    }
    for f in ["intent", "line_product", "line_qty"] {
        assert!(w.pct(f) >= 85.0, "whatsapp {f} accuracy {:.1}% < 85%", w.pct(f));
    }
}
