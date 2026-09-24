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
    "Almarai", "Nadec", "Coca-Cola", "Pepsi", "Lays", "Pringles", "Nestle", "Kinder", "Galaxy", "Tiffany", "Americana", "Sadia", "Puck", "Kraft", "Heinz",
    "Lipton", "Nescafe", "Tang", "Vimto", "Rani", "Aquafina", "Masafi", "Barakat", "Oman", "Bayara", "Tilda", "Abu", "Kas", "Fine", "Dettol",
];
const KINDS: &[&str] = &["Milk", "Juice", "Water", "Chips", "Biscuits", "Rice", "Tea", "Coffee", "Cheese", "Yoghurt", "Chicken", "Tissue", "Soap", "Shampoo", "Bread"];

#[test]
#[ignore]
fn perf_100k_products() {
    let e = env();
    let t = &e.owner_token;
    let n = 100_000usize;
    let mut rng = Rng(7);
    let mut csv = String::from("sku,name,barcode,price,cost,category,stock\n");
    for i in 0..n {
        let name = format!("{} {} {}g #{i}", WORDS[rng.next(WORDS.len() as u64) as usize], KINDS[rng.next(KINDS.len() as u64) as usize], 50 + rng.next(950));
        let bc = format!("{:013}", 6_290_000_000_000u64 + i as u64);
        let price = 100 + rng.next(20_000);
        csv.push_str(&format!("S{i:06},{name},{bc},{}.{:03},{}.{:03},{},{}\n", price / 1000, price % 1000, price * 6 / 10000, (price * 6 / 10) % 1000, KINDS[i % KINDS.len()], 100));
    }
    let started = Instant::now();
    let r = e
        .core
        .products_import_apply(t, ImportRequest { csv, mapping: None, update_existing: false, skip_errors: false, operation_id: Some(op()) })
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
            .pos_finalize(t, FinalizeRequest {
                cart_id: c.cart_id.unwrap(),
                operation_id: op(),
                tenders: vec![TenderInput { method: "cash".into(), amount_minor: c.totals.total_minor, reference: None }],
                approval_token: None,
                expected_total_minor: None,
            })
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
    assert!(a < Duration::from_millis(50));
    assert!(b < Duration::from_millis(150));
    assert!(c < Duration::from_millis(100));
    assert!(d < Duration::from_millis(500));
}
