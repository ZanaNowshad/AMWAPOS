//! Performance acceptance (spec §10.2). Run in release mode:
//!   cargo test -p amwapos-core --release --test perf -- --ignored --nocapture
//! Imports 100,000 products through the real CSV importer, then measures P95
//! latency of the user-facing operations.

mod common;

use std::time::{Duration, Instant};

use amwapos_core::importer::ImportRequest;
use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::FinalizeRequest;
use common::*;

fn p95(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[(v.len() as f64 * 0.95) as usize - 1]
}

struct Rng(u64);
impl Rng {
    fn next(&mut self, m: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % m
    }
}

const WORDS: &[&str] = &[
    "Almarai",
    "Nadec",
    "Coca-Cola",
    "Pepsi",
    "Lays",
    "Pringles",
    "Nestle",
    "Kinder",
    "Galaxy",
    "Tiffany",
    "Americana",
    "Sadia",
    "Puck",
    "Kraft",
    "Heinz",
    "Lipton",
    "Nescafe",
    "Tang",
    "Vimto",
    "Rani",
    "Aquafina",
    "Masafi",
    "Barakat",
    "Oman",
    "Bayara",
    "Tilda",
    "Abu",
    "Kas",
    "Fine",
    "Dettol",
];
const KINDS: &[&str] = &[
    "Milk", "Juice", "Water", "Chips", "Biscuits", "Rice", "Tea", "Coffee", "Cheese", "Yoghurt", "Chicken", "Tissue", "Soap", "Shampoo",
    "Bread",
];

#[test]
#[ignore]
fn perf_100k_products() {
    let e = env();
    let t = &e.owner_token;
    let n = 100_000usize;
    let mut rng = Rng(7);
    let mut csv = String::from("sku,name,barcode,price,cost,category,stock\n");
    for i in 0..n {
        let name = format!(
            "{} {} {}g #{i}",
            WORDS[rng.next(WORDS.len() as u64) as usize],
            KINDS[rng.next(KINDS.len() as u64) as usize],
            50 + rng.next(950)
        );
        let bc = format!("{:013}", 6_290_000_000_000u64 + i as u64);
        let price = 100 + rng.next(20_000);
        csv.push_str(&format!(
            "S{i:06},{name},{bc},{}.{:03},{}.{:03},{},{}\n",
            price / 1000,
            price % 1000,
            price * 6 / 10000,
            (price * 6 / 10) % 1000,
            KINDS[i % KINDS.len()],
            100
        ));
    }
    let started = Instant::now();
    let r = e
        .core
        .products_import_apply(
            t,
            ImportRequest { csv, mapping: None, update_existing: false, skip_errors: false, operation_id: Some(op()) },
        )
        .unwrap();
    let import = started.elapsed();
    assert_eq!(r["created"], n as i64);
    println!("import of {n} products: {:.1}s", import.as_secs_f64());

    e.open_shift(t, 0);
    // Barcode scan (lookup + add to persisted cart + authoritative re-pricing).
    let mut scans = vec![];
    for i in 0..2000 {
        if i % 10 == 0 {
            let _ = e.core.pos_cancel_sale(t, None);
        }
        let bc = format!("{:013}", 6_290_000_000_000u64 + rng.next(n as u64));
        let s = Instant::now();
        let res = e.core.pos_scan(t, &bc, None).unwrap();
        scans.push(s.elapsed());
        assert_eq!(res.outcome, "added");
    }
    let _ = e.core.pos_cancel_sale(t, None);
    // Name search.
    let mut searches = vec![];
    for _ in 0..500 {
        let q = format!("{} {}", WORDS[rng.next(WORDS.len() as u64) as usize], &KINDS[rng.next(KINDS.len() as u64) as usize][..3]);
        let s = Instant::now();
        let r = e.core.pos_search(t, &q, None, false, Some(40)).unwrap();
        searches.push(s.elapsed());
        assert!(!r.is_empty(), "no results for {q}");
    }
    // Cart mutation (quantity change on a 10-line cart).
    for _ in 0..10 {
        let bc = format!("{:013}", 6_290_000_000_000u64 + rng.next(n as u64));
        e.core.pos_scan(t, &bc, None).unwrap();
    }
    let cart = e.core.pos_get_cart(t).unwrap();
    let mut muts = vec![];
    for i in 0..500 {
        let line = &cart.lines[i % cart.lines.len()];
        let s = Instant::now();
        e.core.pos_set_quantity(t, &line.line_id, 1000 + (i as i64 % 5) * 1000, None).unwrap();
        muts.push(s.elapsed());
    }
    let _ = e.core.pos_cancel_sale(t, None);
    // Sale commit (5 lines, cash, stock movements, audit, idempotency).
    let mut commits = vec![];
    for _ in 0..300 {
        let mut cart = None;
        for _ in 0..5 {
            let bc = format!("{:013}", 6_290_000_000_000u64 + rng.next(n as u64));
            cart = Some(e.core.pos_scan(t, &bc, None).unwrap().cart);
        }
        let c = cart.unwrap();
        let s = Instant::now();
        e.core
            .pos_finalize(
                t,
                FinalizeRequest {
                    cart_id: c.cart_id.unwrap(),
                    operation_id: op(),
                    tenders: vec![TenderInput { method: "cash".into(), amount_minor: c.totals.total_minor, reference: None }],
                    approval_token: None,
                    expected_total_minor: None,
                    fulfilment: None,
                },
            )
            .unwrap();
        commits.push(s.elapsed());
    }
    let (a, b, c, d) = (p95(scans), p95(searches), p95(muts), p95(commits));
    println!("P95 barcode scan:   {:.2} ms (target 50)", a.as_secs_f64() * 1e3);
    println!("P95 product search: {:.2} ms (target 150)", b.as_secs_f64() * 1e3);
    println!("P95 cart mutation:  {:.2} ms (target 100)", c.as_secs_f64() * 1e3);
    println!("P95 sale commit:    {:.2} ms (target 500)", d.as_secs_f64() * 1e3);
    // Reports over the resulting data stay interactive.
    let s = Instant::now();
    e.core.report_run(t, "products", Default::default()).unwrap();
    println!("product report (300 sales): {:.1} ms", s.elapsed().as_secs_f64() * 1e3);
    let s = Instant::now();
    e.core.dashboard(t).unwrap();
    println!("dashboard: {:.1} ms", s.elapsed().as_secs_f64() * 1e3);
    // Replenishment at scale (Wave 4): 100,000 products, three suppliers
    // each, half of them below a reorder point. The engine reads every
    // product in a fixed number of grouped queries (no query per product).
    e.core
        .db
        .write(|tx| {
            for (id, name) in [("SUPA", "Alpha"), ("SUPB", "Bravo"), ("SUPC", "Charlie")] {
                tx.execute(
                    "INSERT INTO suppliers(supplier_id, name, active, created_at, updated_at) VALUES (?1,?2,1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                    [id, name],
                )?;
            }
            tx.execute_batch(
                "INSERT INTO supplier_products(supplier_id, product_id, units_per_case, pack_source, moq_packs, lead_time_days, preferred, created_at, updated_at)
                   SELECT s.supplier_id, p.product_id, 12, 'person', 1, 2 + (rowid % 5), CASE WHEN s.supplier_id='SUPA' THEN 1 ELSE 0 END, 'x', 'x'
                   FROM products p CROSS JOIN (SELECT 'SUPA' AS supplier_id UNION ALL SELECT 'SUPB' UNION ALL SELECT 'SUPC') s;
                 UPDATE products SET reorder_point_milli = 150000 WHERE rowid % 2 = 0;",
            )?;
            Ok(())
        })
        .unwrap();
    let mut runs = vec![];
    let mut to_order = 0;
    for _ in 0..3 {
        let s = Instant::now();
        let v =
            e.core.replenishment(t, serde_json::from_value(serde_json::json!({ "states": ["order"], "limit": 5000 })).unwrap()).unwrap();
        runs.push(s.elapsed());
        to_order = v["counts"]["order"].as_i64().unwrap_or(0);
    }
    let rep = runs.iter().min().copied().unwrap();
    println!("replenishment over {n} products ({to_order} to order): {:.0} ms", rep.as_secs_f64() * 1e3);
    assert!(to_order >= (n as i64) / 2 - 1000, "half the products are below their reorder point: {to_order}");
    let s = Instant::now();
    e.core.dashboard(t).unwrap();
    println!("dashboard with suggested orders: {:.1} ms", s.elapsed().as_secs_f64() * 1e3);
    // Wave 5 at scale: PLUs on 10,000 products, 200 scale rules (looked up
    // by length and prefix, never scanned one by one), the duplicate review
    // and the pricing review over all 100,000 products.
    e.core
        .db
        .write(|tx| {
            tx.execute_batch("UPDATE products SET plu = CAST(rowid AS TEXT) WHERE rowid <= 10000;")?;
            for i in 0..200 {
                let prefix = format!("{}", 20 + (i % 10));
                let len = 13 - (i / 100) as i64; // 13 and 12 digits
                tx.execute(
                    "INSERT INTO scale_barcode_rules(rule_id, name, prefix, length, item_start, item_length, value_kind, value_start, value_length,
                        decimals, check_digit, active, priority, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, 3, 5, 'weight', 8, 4, 3, 'none', 1, ?5, 'x', 'x')",
                    rusqlite::params![format!("R{i:03}"), format!("Rule {i}"), prefix, len, i as i64],
                )?;
            }
            // 500 near-duplicates to find (same words, different case and spacing).
            tx.execute_batch(
                "INSERT INTO products(product_id, sku, name, tax_rule_id, unit, created_at, updated_at, allow_decimal_quantity)
                   SELECT 'dup' || rowid, 'D' || rowid, upper(name), tax_rule_id, unit, created_at, updated_at, 1 FROM products WHERE rowid <= 500;
                 UPDATE products SET allow_decimal_quantity = 1 WHERE rowid <= 10000;",
            )?;
            Ok(())
        })
        .unwrap();
    let _ = e.core.pos_cancel_sale(t, None);
    let mut scale = vec![];
    let mut plus = vec![];
    for i in 0..1000 {
        if i % 10 == 0 {
            let _ = e.core.pos_cancel_sale(t, None);
        }
        // Prefix 29, 13 digits: the highest-priority rule for it wins.
        let item = 1 + rng.next(9_999);
        let code = format!("29{item:05}{:04}00", 100 + rng.next(900));
        let s = Instant::now();
        let r = e.core.pos_scan(t, &code, None).unwrap();
        scale.push(s.elapsed());
        assert_eq!(r.outcome, "added");
        let s = Instant::now();
        e.core.pos_scan(t, &format!("{}", 1 + rng.next(9_999)), Some(1000)).unwrap();
        plus.push(s.elapsed());
    }
    let _ = e.core.pos_cancel_sale(t, None);
    let (sc, pl) = (p95(scale), p95(plus));
    println!("P95 scale-label scan (200 rules): {:.2} ms (target 50)", sc.as_secs_f64() * 1e3);
    println!("P95 PLU scan:                     {:.2} ms (target 50)", pl.as_secs_f64() * 1e3);
    let s = Instant::now();
    let dups = e.core.duplicates_list(t, false, Some(500)).unwrap();
    let dup_t = s.elapsed();
    let found = dups["pairs"].as_array().unwrap().len();
    println!("duplicate review over {} products: {:.0} ms ({found} pairs)", n + 500, dup_t.as_secs_f64() * 1e3);
    assert!(found >= 400, "the near-duplicates are found: {found}");
    e.core
        .pricing_policy_save(
            t,
            serde_json::from_value(serde_json::json!({ "name": "Global", "scope": "global", "target_margin_bp": 3000, "min_margin_bp": 1500, "rounding_step_minor": 50 }))
                .unwrap(),
        )
        .unwrap();
    let s = Instant::now();
    let rv = e.core.pricing_review(t, Some("below_min_margin".into()), Some(100), None).unwrap();
    let rv_t = s.elapsed();
    println!(
        "pricing review over {} products: {:.0} ms ({} below minimum, {} recommendations)",
        n + 500,
        rv_t.as_secs_f64() * 1e3,
        rv["counts"]["below_min_margin"],
        rv["counts"]["recommendation"]
    );
    let s = Instant::now();
    e.core.commercial_summary(t).unwrap();
    println!("commercial summary (dashboard cards): {:.0} ms", s.elapsed().as_secs_f64() * 1e3);
    assert!(sc < Duration::from_millis(50), "scale scan {sc:?}");
    assert!(pl < Duration::from_millis(50), "PLU scan {pl:?}");
    assert!(dup_t < Duration::from_secs(10), "duplicates {dup_t:?}");
    assert!(rv_t < Duration::from_secs(10), "pricing review {rv_t:?}");
    assert!(rep < Duration::from_secs(10), "replenishment {rep:?}");
    assert!(a < Duration::from_millis(50));
    assert!(b < Duration::from_millis(150));
    assert!(c < Duration::from_millis(100));
    assert!(d < Duration::from_millis(500));
}
