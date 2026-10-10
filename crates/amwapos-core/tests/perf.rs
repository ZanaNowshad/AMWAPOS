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

    // ---------------------------------------------------------------- Wave 6: 1000 live offers
    let (pids, cats): (Vec<String>, Vec<String>) = e
        .core
        .db
        .read(|c| {
            let mut st = c.prepare("SELECT product_id FROM products ORDER BY product_id")?;
            let p = st.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            let mut st = c.prepare("SELECT category_id FROM categories ORDER BY category_id")?;
            let k = st.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            Ok((p, k))
        })
        .unwrap();
    let kinds = ["percent", "amount", "fixed_price", "quantity", "bxgy"];
    e.core
        .db
        .write(|c| {
            for i in 0..1000usize {
                let id = format!("perfpromo{i:05}");
                let (kind, target) = if i < 20 { ("basket", "all") } else { (kinds[i % kinds.len()], "items") };
                c.execute(
                    "INSERT INTO promotions(promotion_id, name, status, kind, target, priority, stackable, percent_bp, amount_minor, price_minor,
                        buy_qty, get_qty, threshold_minor, created_by, created_at, updated_at)
                     VALUES (?1,?2,'active',?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'perf','2026-01-01','2026-01-01')",
                    rusqlite::params![
                        id, format!("Offer {i}"), kind, target, (i % 7) as i64, (i % 3 == 0) as i64,
                        if kind == "amount" || (kind == "basket" && i % 2 == 0) { None } else { Some(500 + (i as i64 % 20) * 100) },
                        if kind == "amount" || (kind == "basket" && i % 2 == 0) { Some(50 + i as i64 % 200) } else { None },
                        if kind == "fixed_price" || kind == "quantity" { Some(500 + i as i64 % 3000) } else { None },
                        if kind == "quantity" || kind == "bxgy" { Some(2 + i as i64 % 3) } else { None },
                        if kind == "bxgy" { Some(1) } else { None },
                        if kind == "basket" { Some(5_000 + i as i64 * 100) } else { None }
                    ],
                )?;
                if target == "items" {
                    for k in 0..3 {
                        let pid = &pids[(i * 97 + k * 31_337) % pids.len()];
                        c.execute("INSERT OR IGNORE INTO promotion_targets VALUES (?1,'buy','product',?2)", [&id, pid])?;
                    }
                    if i % 50 == 0 {
                        c.execute("INSERT OR IGNORE INTO promotion_targets VALUES (?1,'buy','category',?2)", [&id, &cats[i % cats.len()]])?;
                    }
                }
            }
            // 200 bundles of 3 items each, on products 0..600.
            for b in 0..200usize {
                let parent = &pids[pids.len() - 1 - b];
                c.execute("UPDATE products SET track_inventory=0 WHERE product_id=?1", [parent])?;
                c.execute(
                    "INSERT INTO bundles(bundle_product_id, version, active, created_by, created_at, updated_by, updated_at) VALUES (?1,1,1,'perf','x','perf','x')",
                    [parent],
                )?;
                for k in 0..3 {
                    c.execute(
                        "INSERT INTO bundle_components VALUES (?1,1,?2,?3)",
                        rusqlite::params![parent, pids[b * 3 + k], 1000 * (1 + k as i64)],
                    )?;
                }
            }
            Ok(())
        })
        .unwrap();
    let _ = e.core.pos_cancel_sale(t, None);
    // Offer-covered products are scanned too, so the engine has work to do.
    let covered: Vec<String> = (0..1000usize).map(|i| pids[(i * 97) % pids.len()].clone()).collect();
    let mut promo_scans = vec![];
    for i in 0..1000 {
        if i % 20 == 0 {
            let _ = e.core.pos_cancel_sale(t, None);
        }
        let pid = &covered[rng.next(covered.len() as u64) as usize];
        let s = Instant::now();
        e.core.pos_add_product(t, pid, Some(1000 + (i as i64 % 3) * 1000)).unwrap();
        promo_scans.push(s.elapsed());
    }
    let _ = e.core.pos_cancel_sale(t, None);
    for k in 0..20 {
        e.core.pos_add_product(t, &covered[k * 13], Some(3000)).unwrap();
    }
    let cart = e.core.pos_get_cart(t).unwrap();
    assert_eq!(cart.lines.len(), 20);
    let mut promo_muts = vec![];
    for i in 0..300 {
        let line = &cart.lines[i % 20];
        let s = Instant::now();
        e.core.pos_set_quantity(t, &line.line_id, 1000 + (i as i64 % 4) * 1000, None).unwrap();
        promo_muts.push(s.elapsed());
    }
    let mut promo_commits = vec![];
    for i in 0..100 {
        let s0 = if i == 0 { None } else { Some(()) };
        if s0.is_some() {
            for k in 0..20 {
                e.core.pos_add_product(t, &covered[(i * 20 + k) % covered.len()], Some(2000)).unwrap();
            }
        }
        let c = e.core.pos_get_cart(t).unwrap();
        let s = Instant::now();
        e.core
            .pos_finalize(
                t,
                FinalizeRequest {
                    cart_id: c.cart_id.unwrap(),
                    operation_id: op(),
                    tenders: vec![TenderInput { method: "cash".into(), amount_minor: c.totals.total_minor, reference: None }],
                    approval_token: None,
                    expected_total_minor: Some(c.totals.total_minor),
                    fulfilment: None,
                },
            )
            .unwrap();
        promo_commits.push(s.elapsed());
    }
    let sip: i64 = e.core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM sale_item_promotions", [], |r| r.get(0))?)).unwrap();
    let s = Instant::now();
    let bl = e.core.bundles_list(t).unwrap();
    let bundles_t = s.elapsed();
    assert_eq!(bl["rows"].as_array().unwrap().len(), 200);
    let s = Instant::now();
    e.core.promotions_attention(t).unwrap();
    let att_t = s.elapsed();
    let (ps, pm, pc) = (p95(promo_scans), p95(promo_muts), p95(promo_commits));
    println!("with 1000 live offers ({sip} offer lines recorded):");
    println!("P95 add covered item:          {:.2} ms (target 50)", ps.as_secs_f64() * 1e3);
    println!("P95 20-line cart change:       {:.2} ms (target 100)", pm.as_secs_f64() * 1e3);
    println!("P95 20-line sale commit:       {:.2} ms (target 500)", pc.as_secs_f64() * 1e3);
    println!("bundle availability (200):     {:.1} ms", bundles_t.as_secs_f64() * 1e3);
    println!("offer attention (dashboard):   {:.0} ms", att_t.as_secs_f64() * 1e3);
    assert!(sip > 0, "offers applied during the benchmark");
    assert!(ps < Duration::from_millis(50), "add {ps:?}");
    assert!(pm < Duration::from_millis(100), "cart change {pm:?}");
    assert!(pc < Duration::from_millis(500), "commit {pc:?}");
    assert!(att_t < Duration::from_secs(5), "offer attention {att_t:?}");
    assert!(bundles_t < Duration::from_secs(1), "bundle availability {bundles_t:?}");

    // ---- Wave 7: operational control at scale --------------------------
    // 40 paired terminals with heartbeats, 10,000 earlier cases (30% still
    // open) and 10,000 refused records from those terminals, on top of the
    // 100,000 products and the sales above. These rows stand for history the
    // store already has; every number below is measured through the real
    // functions a person or the minute check calls.
    let branch: String = e.core.db.read(|c| Ok(c.query_row("SELECT branch_id FROM branches LIMIT 1", [], |r| r.get(0))?)).unwrap();
    let reasons = ["missing_dependency", "version_mismatch", "conflict", "not_permitted", "storage_error"];
    let tables = ["sales", "sale_items", "payments", "customers", "stock_movements"];
    e.core
        .db
        .write(|c| {
            for d in 0..40 {
                let id = format!("01BENCHTERM{d:02}00000000000000");
                c.execute(
                    "INSERT INTO devices(device_id, branch_id, name, device_code, operating_mode, active, activated_at)
                     VALUES (?1,?2,?3,?4,'terminal',1,'2026-01-01T00:00:00Z')",
                    rusqlite::params![id, branch, format!("Till {}", d + 2), format!("B{d:02}")],
                )?;
                c.execute(
                    "INSERT INTO device_heartbeats(device_id, last_seen_at, app_version, schema_version, pending_count, protocol_version,
                        oldest_pending_at, last_heartbeat_at, problem_count)
                     VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ','now', ?2), '1.0', ?3, ?4, 2, strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 minutes'),
                        strftime('%Y-%m-%dT%H:%M:%fZ','now'), 0)",
                    rusqlite::params![id, format!("-{} minutes", d % 7), amwapos_core::db::latest_schema_version(), d % 5],
                )?;
            }
            for i in 0..10_000 {
                let status = if i % 10 < 3 { "new" } else if i % 2 == 0 { "resolved" } else { "dismissed" };
                c.execute(
                    "INSERT INTO cases(case_id, case_number, kind, severity, status, branch_id, entity_type, entity_id, title, facts_json, created_at,
                        updated_at, source, dedupe_key, device_id, condition_active, first_seen_at, last_seen_at, resolution_code, resolved_at)
                     VALUES (?1,?2,?3,?4,?5,?6,'device',?7,?8,'{}',?9,?9,'system',?10,?7,?11,?9,?9,?12,?13)",
                    rusqlite::params![
                        format!("bench-case-{i:05}"),
                        format!("B-{i:05}"),
                        ["print_failures", "terminal_backlog", "rider_cash_held", "payment_review_backlog"][i % 4],
                        ["low", "medium", "high"][i % 3],
                        status,
                        branch,
                        format!("01BENCHTERM{:02}00000000000000", i % 40),
                        format!("Earlier problem {i}"),
                        format!("2026-0{}-{:02}T08:00:00Z", 1 + i % 9, 1 + i % 28),
                        (status == "new").then(|| format!("bench:{i}")),
                        (status == "new") as i64,
                        (status == "resolved").then_some("fixed"),
                        (status != "new").then_some("2026-10-01T00:00:00Z"),
                    ],
                )?;
            }
            for i in 0..10_000 {
                let origin = format!("01BENCHTERM{:02}00000000000000", i % 40);
                let table = tables[i % tables.len()];
                let reason = reasons[(i / 40) % reasons.len()];
                let payload = serde_json::json!({ "seq": i, "table": table, "pk": { "id": format!("r{i}") }, "op": "upsert",
                    "row": { "id": format!("r{i}"), "receipt_number": format!("R-{i:06}") } });
                c.execute(
                    "INSERT INTO sync_dead_letters(dead_id, direction, origin, table_name, row_pk, op, payload_json, error, attempts, status, created_at,
                        last_attempt_at, reason_code, retryable)
                     VALUES (?1,'apply',?2,?3,?4,'upsert',?5,'refused',1,?6,?7,?7,?8,?9)",
                    rusqlite::params![
                        format!("bench-dead-{i:05}"),
                        origin,
                        table,
                        format!("{{\"id\":\"r{i}\"}}"),
                        payload.to_string(),
                        if (i / 7) % 4 == 0 { "resolved" } else { "open" },
                        format!("2026-10-0{}T{:02}:00:00Z", 1 + i % 7, i % 24),
                        reason,
                        (reason != "conflict" && reason != "not_permitted") as i64,
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let time = |f: &mut dyn FnMut()| {
        let s = Instant::now();
        f();
        s.elapsed()
    };
    let eval_first = time(&mut || {
        e.core.ops_evaluate().unwrap();
    });
    let eval_again = time(&mut || {
        e.core.ops_evaluate().unwrap();
    });
    let open_sync: i64 = e
        .core
        .db
        .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM cases WHERE kind='sync_failures' AND status='new'", [], |r| r.get(0))?))
        .unwrap();
    let q = |status: &str, offset: i64| -> amwapos_core::cases::CaseQuery {
        serde_json::from_value(serde_json::json!({ "status": status, "limit": 50, "offset": offset })).unwrap()
    };
    let mut page_first = Duration::ZERO;
    let mut page_deep = Duration::ZERO;
    let mut total = 0;
    for _ in 0..5 {
        page_first = page_first.max(time(&mut || total = e.core.cases_query(t, q("needs_attention", 0)).unwrap().total));
        page_deep = page_deep.max(time(&mut || {
            e.core.cases_query(t, q("all", 9_900)).unwrap();
        }));
    }
    let dq = |v: serde_json::Value| -> amwapos_core::sync_recon::DeadQuery { serde_json::from_value(v).unwrap() };
    let mut dead_first = Duration::ZERO;
    let mut dead_deep = Duration::ZERO;
    let mut dead_filtered = Duration::ZERO;
    let mut dead_total = 0;
    for _ in 0..5 {
        dead_first = dead_first.max(time(&mut || {
            dead_total = e.core.sync_dead_letters(t, dq(serde_json::json!({ "limit": 50 }))).unwrap()["total"].as_i64().unwrap();
        }));
        dead_deep = dead_deep.max(time(&mut || {
            e.core.sync_dead_letters(t, dq(serde_json::json!({ "limit": 50, "offset": 7_400 }))).unwrap();
        }));
        dead_filtered = dead_filtered.max(time(&mut || {
            e.core
                .sync_dead_letters(
                    t,
                    dq(serde_json::json!({ "origin": "01BENCHTERM0700000000000000", "reason_code": "missing_dependency" })),
                )
                .unwrap();
        }));
    }
    let health = time(&mut || {
        let h = e.core.terminals_health(t).unwrap();
        assert_eq!(h["terminals"].as_array().unwrap().len(), 40);
    });
    let dash = time(&mut || {
        e.core.dashboard(t).unwrap();
    });
    let bulk = time(&mut || {
        let r = e
            .core
            .sync_retry_dead_letters(
                t,
                serde_json::from_value(serde_json::json!({ "reason_code": "storage_error", "operation_id": op() })).unwrap(),
            )
            .unwrap();
        assert_eq!(r["tried"], 500, "bounded to 500 per request");
    });
    println!("operational control (40 tills, 10,000 earlier cases, 10,000 refused records):");
    println!("minute check, first run ({open_sync} sync cases opened): {:.0} ms", eval_first.as_secs_f64() * 1e3);
    println!("minute check, steady state:        {:.0} ms", eval_again.as_secs_f64() * 1e3);
    println!("Alert Centre, needs attention ({total}): {:.1} ms", page_first.as_secs_f64() * 1e3);
    println!("Alert Centre, page at 9,900:       {:.1} ms", page_deep.as_secs_f64() * 1e3);
    println!("Sync problems, first page ({dead_total}): {:.1} ms", dead_first.as_secs_f64() * 1e3);
    println!("Sync problems, page at 7,400:      {:.1} ms", dead_deep.as_secs_f64() * 1e3);
    println!("Sync problems, one till + reason:  {:.1} ms", dead_filtered.as_secs_f64() * 1e3);
    println!("Terminals (40):                    {:.1} ms", health.as_secs_f64() * 1e3);
    println!("Dashboard with open cases:         {:.1} ms", dash.as_secs_f64() * 1e3);
    println!("bulk retry of 500 records:         {:.0} ms", bulk.as_secs_f64() * 1e3);
    assert_eq!(open_sync, 40 * 5, "one case per till and reason");
    assert!(eval_again < Duration::from_secs(2), "minute check {eval_again:?}");
    assert!(page_first < Duration::from_millis(300) && page_deep < Duration::from_millis(300), "case pages {page_first:?} {page_deep:?}");
    assert!(dead_first < Duration::from_millis(300) && dead_deep < Duration::from_millis(500), "sync pages {dead_first:?} {dead_deep:?}");
    assert!(health < Duration::from_millis(500), "terminals {health:?}");
    assert!(dash < Duration::from_secs(1), "dashboard {dash:?}");
}

/// Wave 8: Document Library search over 50,000 documents, Business Memory
/// search over 5,000 memories, and the Cash-flow Radar over thousands of
/// records. Documents and memories go in through the same indexing code
/// the app uses (rows written directly for speed; the index is built by
/// `library::index_document` and `memory` itself).
#[test]
#[ignore]
fn perf_wave8_intelligence() {
    let e = env();
    let t = &e.owner_token;
    let mut rng = Rng(11);
    let n_docs = 50_000usize;
    let started = Instant::now();
    e.core
        .db
        .write(|c| {
            let now = amwapos_core::time::now_str();
            for i in 0..n_docs {
                let sha = format!("{:064x}", i + 1);
                let id = format!("01BENCHDOC{i:016}");
                c.execute(
                    "INSERT INTO library_files(sha256, path, mime, bytes, page_count, text_status, text_source, created_at)
                     VALUES (?1, ?2, 'application/pdf', 1000, 2, 'extracted', 'pdf_text', ?3)",
                    rusqlite::params![sha, format!("library/{}/{}.pdf", &sha[..2], sha), now],
                )?;
                for p in 1..=2 {
                    let words: Vec<&str> = (0..40).map(|_| WORDS[rng.next(WORDS.len() as u64) as usize]).collect();
                    let text = format!("{} invoice {} page {p} {}", KINDS[i % KINDS.len()], i, words.join(" "));
                    c.execute("INSERT INTO library_file_pages(sha256, page, text) VALUES (?1,?2,?3)", rusqlite::params![sha, p, text])?;
                }
                let category = ["invoice", "receipt", "contract", "delivery_note", "bank"][i % 5];
                c.execute(
                    "INSERT INTO library_documents(document_id, seq, number, title, category, sha256, original_name, version, status, source,
                       added_by, added_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,1,'active','upload','bench',?8,?8)",
                    rusqlite::params![id, (i + 1) as i64, format!("DOC-{:06}", i + 1), format!("{} document {i}", KINDS[i % KINDS.len()]),
                        category, sha, format!("doc-{i}.pdf"), now],
                )?;
                amwapos_core::library::index_document(c, &id)?;
            }
            c.execute("INSERT OR REPLACE INTO sequences(name, value) VALUES ('library_document', ?1)", [n_docs as i64])?;
            Ok(())
        })
        .unwrap();
    println!("library: {n_docs} documents indexed in {:.1}s", started.elapsed().as_secs_f64());
    let (_, inv) = e.user("Ali", "role_inventory", "2580");
    let mut searches = vec![];
    let mut scoped = vec![];
    for i in 0..200 {
        let q = format!("{} {}", WORDS[i % WORDS.len()], KINDS[i % KINDS.len()]);
        let s = Instant::now();
        let r = e.core.library_search(t, &q, false, None).unwrap();
        searches.push(s.elapsed());
        assert!(r["results"].as_array().unwrap().len() <= 20);
        let s = Instant::now();
        e.core.library_search(&inv, &q, false, None).unwrap();
        scoped.push(s.elapsed());
    }
    let mut lists = vec![];
    for _ in 0..50 {
        let s = Instant::now();
        e.core.library_list(t, serde_json::from_value(serde_json::json!({ "limit": 50 })).unwrap()).unwrap();
        lists.push(s.elapsed());
    }
    let (ls, lsc, ll) = (p95(searches), p95(scoped), p95(lists));

    // Business Memory: 5,000 confirmed memories through the real path.
    let started = Instant::now();
    for i in 0..5_000 {
        e.core
            .memory_add(
                t,
                amwapos_core::memory::MemoryInput {
                    statement: format!(
                        "{} {} arrives on weekday {} for order group {i}",
                        WORDS[i % WORDS.len()],
                        KINDS[i % KINDS.len()],
                        i % 7
                    ),
                    ..Default::default()
                },
                true,
            )
            .unwrap();
    }
    println!("memory: 5,000 memories in {:.1}s", started.elapsed().as_secs_f64());
    let mut msearch = vec![];
    for i in 0..200 {
        let s = Instant::now();
        let r = e.core.memory_search(t, &format!("{} arrives", WORDS[i % WORDS.len()]), None, None).unwrap();
        msearch.push(s.elapsed());
        assert!(r["memories"].as_array().unwrap().len() <= 10);
    }
    let ms = p95(msearch);

    // Cash-flow Radar: 2,000 posted supplier invoices, 1,000 expenses, 300 orders, 50 repeating expenses.
    e.core
        .db
        .write(|c| {
            let now = amwapos_core::time::now_str();
            let today = chrono::Utc::now().date_naive();
            let branch: String = c.query_row("SELECT branch_id FROM branches LIMIT 1", [], |r| r.get(0))?;
            let cat: String = c.query_row("SELECT category_id FROM expense_categories LIMIT 1", [], |r| r.get(0))?;
            for s in 0..50 {
                c.execute(
                    "INSERT INTO suppliers(supplier_id, name, active, created_at, updated_at) VALUES (?1, ?2, 1, ?3, ?3)",
                    rusqlite::params![format!("01BENCHSUP{s:016}"), format!("Supplier {s}"), now],
                )?;
            }
            for i in 0..2_000 {
                let sup = format!("01BENCHSUP{:016}", i % 50);
                let inv = format!("01BENCHINV{i:016}");
                let due = (today + chrono::Duration::days((i % 120) as i64 - 20)).to_string();
                c.execute(
                    "INSERT INTO supplier_invoices(invoice_id, number, doc_type, supplier_id, invoice_number, invoice_date, due_date, subtotal_minor,
                       vat_minor, total_minor, status, posting, created_by, created_at, updated_at, revision)
                     VALUES (?1, ?2, 'invoice', ?3, ?2, ?4, ?5, 9000, 900, 9900, 'approved', 'posted', 'bench', ?6, ?6, 1)",
                    rusqlite::params![inv, format!("SI-{i:06}"), sup, (today - chrono::Duration::days(30)).to_string(), due, now],
                )?;
                c.execute(
                    "INSERT INTO ap_liabilities(liability_id, supplier_id, invoice_id, doc_date, due_date, due_rule, amount_minor, currency, status,
                       operation_id, posted_by, posted_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'invoice', 9900, 'BHD', 'open', ?1, 'bench', ?6)",
                    rusqlite::params![format!("01BENCHLIA{i:016}"), sup, inv, (today - chrono::Duration::days(30)).to_string(), due, now],
                )?;
            }
            for i in 0..1_000 {
                c.execute(
                    "INSERT INTO expenses(expense_id, number, branch_id, business_date, category_id, description, net_minor, vat_minor, total_minor,
                       status, created_by, revision, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'Bench expense', 5000, 0, 5000, ?6, 'bench', 1, ?7, ?7)",
                    rusqlite::params![format!("01BENCHEXP{i:016}"), format!("EX-{i:06}"), branch,
                        (today + chrono::Duration::days((i % 60) as i64)).to_string(), cat, if i % 2 == 0 { "approved" } else { "submitted" }, now],
                )?;
            }
            for i in 0..300 {
                c.execute(
                    "INSERT INTO purchase_orders(po_id, po_number, supplier_id, branch_id, status, expected_at, subtotal_minor, tax_minor, total_minor,
                       created_by, created_at, updated_at, version)
                     VALUES (?1, ?2, ?3, ?4, 'ordered', ?5, 50000, 0, 50000, 'bench', ?6, ?6, 1)",
                    rusqlite::params![format!("01BENCHPO{i:017}"), format!("PO-B{i:05}"), format!("01BENCHSUP{:016}", i % 50), branch,
                        (today + chrono::Duration::days((i % 90) as i64)).to_string(), now],
                )?;
            }
            for i in 0..50 {
                c.execute(
                    "INSERT INTO expense_recurring(recurring_id, name, category_id, description, net_minor, vat_minor, cadence, day, next_date,
                       branch_id, active, created_by, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'Bench repeating', 2000, 0, ?4, ?5, ?6, ?7, 1, 'bench', ?8, ?8)",
                    rusqlite::params![format!("01BENCHREC{i:016}"), format!("Repeating {i}"), cat, if i % 2 == 0 { "weekly" } else { "monthly" },
                        1 + i % 7, (today + chrono::Duration::days((i % 7) as i64)).to_string(), branch, now],
                )?;
            }
            Ok(())
        })
        .unwrap();
    let mut radar = vec![];
    for h in [7, 14, 30, 60, 90, 90, 30, 90, 60, 90] {
        let s = Instant::now();
        let r = e.core.cashflow_radar(t, Some(h)).unwrap();
        radar.push(s.elapsed());
        assert!(r["horizons"].as_array().unwrap().len() == 5);
    }
    let rd = radar.iter().max().copied().unwrap();
    println!("Document Library search, 50,000 documents, P95: {:.1} ms", ls.as_secs_f64() * 1e3);
    println!("  same, scoped to inventory staff, P95:         {:.1} ms", lsc.as_secs_f64() * 1e3);
    println!("  first list page, P95:                          {:.1} ms", ll.as_secs_f64() * 1e3);
    println!("Business Memory search, 5,000 memories, P95:     {:.1} ms", ms.as_secs_f64() * 1e3);
    println!("Cash-flow Radar (2,000 invoices, 1,000 expenses, 300 orders, 50 repeating), slowest: {:.0} ms", rd.as_secs_f64() * 1e3);
    assert!(ls < Duration::from_millis(500) && lsc < Duration::from_millis(500), "library search {ls:?} {lsc:?}");
    assert!(ms < Duration::from_millis(100), "memory search {ms:?}");
    assert!(rd < Duration::from_secs(1), "radar {rd:?}");
}
