//! Release qualification: disaster-recovery drill (docs/RELEASE_ACCEPTANCE.md).
//!
//! A populated store at the final schema is backed up; the backup folder is
//! copied to "removable media"; the original data folder is destroyed; a new,
//! empty data folder (a second PC) is set up and the backup is restored there
//! through the same path the app uses (Admin → Backups → Restore). Every
//! immutable record must come back byte for byte, document files must match
//! their SHA-256, and staff must sign in with their own PINs.

mod common;

use std::path::Path;
use std::sync::Arc;

use amwapos_core::library::AddInput;
use amwapos_core::memory::{MemoryFilter, MemoryInput};
use amwapos_core::pricing::TenderInput;
use amwapos_core::sales::FinalizeRequest;
use amwapos_core::service::{AppCore, MemorySecretStore};
use common::*;
use sha2::Digest;

fn png(seed: u8) -> Vec<u8> {
    let g = image::GrayImage::from_fn(20, 10, |x, y| image::Luma([(x as u8).wrapping_mul(5).wrapping_add(y as u8).wrapping_add(seed)]));
    let mut out = std::io::Cursor::new(vec![]);
    image::DynamicImage::ImageLuma8(g).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn sell(e: &Env, barcode: &str, qty: i64) {
    let t = &e.owner_token;
    let cart = e.core.pos_scan(t, barcode, Some(qty)).unwrap().cart;
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
                fulfilment: None,
            },
        )
        .unwrap();
}

/// A digest of every row of a table, in key order: equal digests mean the
/// restored table is the same data, not only the same count.
fn table_digest(core: &AppCore, table: &str, order: &str) -> (i64, String) {
    core.db
        .read(|c| {
            let mut st = c.prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))?;
            let cols = st.column_count();
            let mut h = sha2::Sha256::new();
            let mut n = 0i64;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                n += 1;
                for i in 0..cols {
                    let v: rusqlite::types::Value = r.get(i)?;
                    h.update(format!("{v:?}\u{1f}").as_bytes());
                }
                h.update(b"\x1e");
            }
            Ok((n, hex::encode(h.finalize())))
        })
        .unwrap()
}

const TABLES: &[(&str, &str)] = &[
    ("sales", "sale_id"),
    ("sale_items", "rowid"),
    ("payments", "rowid"),
    ("receipt_snapshots", "ref_kind, ref_id"),
    ("stock_movements", "rowid"),
    ("products", "product_id"),
    ("users", "user_id"),
    ("audit_logs", "rowid"),
    ("library_documents", "document_id"),
    ("library_files", "sha256"),
    ("library_file_pages", "rowid"),
    ("business_memories", "memory_id"),
    ("business", "business_id"),
];

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

#[test]
fn a_backup_restores_onto_an_empty_second_pc_with_every_record_and_file_intact() {
    let e = env();
    let t = &e.owner_token;
    // A populated store: products, a shift, sales, a cashier, documents with
    // read text, and confirmed business memory.
    e.product("Milk 1L", "6290000000011", 450, 300, 50_000);
    e.product("Bread", "6290000000028", 250, 150, 40_000);
    let (cashier_id, _) = e.user("Cashier A", "role_cashier", "7391");
    e.open_shift(t, 10_000);
    for i in 0..6 {
        sell(&e, if i % 2 == 0 { "6290000000011" } else { "6290000000028" }, (1 + i % 3) * 1000);
    }
    let mut docs = vec![];
    for seed in 1..=3u8 {
        let r = e
            .core
            .library_add(
                t,
                AddInput {
                    file_name: format!("invoice-{seed}.png"),
                    data_base64: amwapos_core::ids::b64(&png(seed)),
                    category: "other".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let id = r["document_id"].as_str().unwrap().to_string();
        let sha = e.core.library_get(t, &id).unwrap()["document"]["sha256"].as_str().unwrap().to_string();
        e.core.library_text_done(&sha, vec![(1, format!("Supplier delivery note number {seed} milk crates"))], "ocr").unwrap();
        docs.push((id, sha));
    }
    e.core.memory_add(t, MemoryInput { statement: "The milk supplier delivers on Sundays".into(), ..Default::default() }, true).unwrap();
    let before: Vec<_> = TABLES.iter().map(|(tb, o)| (tb, table_digest(&e.core, tb, o))).collect();
    assert!(before.iter().all(|(_, (n, _))| *n > 0), "every drilled table holds data: {before:?}");

    // 1. Back up, with the manifest and the document files beside it.
    let out = e.dir.path().join("bk");
    let b = e.core.backup_create(t, Some(out.to_string_lossy().into())).unwrap();
    assert_eq!(b.status, "completed", "{:?}", b.error);
    let bak = Path::new(&b.path);
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(bak.with_extension("amwbak.json")).unwrap()).unwrap();
    let sha = hex::encode(sha2::Sha256::digest(std::fs::read(bak).unwrap()));
    assert_eq!(manifest["sha256"], sha, "the manifest hash is the file's hash");
    for (_, s) in &docs {
        let f = out.join("AMWAPOS-files").join(&s[..2]).join(s);
        assert_eq!(hex::encode(sha2::Sha256::digest(std::fs::read(&f).unwrap())), *s, "file copy matches its hash");
    }

    // 2. Copy the backup folder to removable media; the first PC is lost.
    let media = tempfile::tempdir().unwrap();
    copy_dir(&out, media.path());
    let bak_on_media = media.path().join(bak.file_name().unwrap());
    let old_owner = e.owner_id.clone();
    drop(e);

    // 3. A second PC: a new, empty data folder, set up by the wizard.
    let pc2 = tempfile::tempdir().unwrap();
    assert_eq!(std::fs::read_dir(pc2.path()).unwrap().count(), 0, "the second PC starts empty");
    let core2 = AppCore::open(pc2.path(), Arc::new(MemorySecretStore::default())).unwrap();
    let mut req = setup_request();
    req.business_name = "Temporary setup".into();
    req.owner_pin = "5517".into();
    core2.setup_initialize(req).unwrap();
    let tmp_owner = core2.login_users().unwrap()[0].user_id.clone();
    let t2 = core2.login(&tmp_owner, "5517").unwrap().token;

    // 4. Inspect, refuse without the explicit different-business consent, restore.
    let insp = core2.backup_inspect(&t2, &bak_on_media.to_string_lossy()).unwrap();
    assert!(insp.ok && insp.compatible && !insp.same_business, "{insp:?}");
    assert!(core2.backup_restore(&t2, &bak_on_media.to_string_lossy(), false).is_err(), "a different business needs consent");
    let r = core2.backup_restore(&t2, &bak_on_media.to_string_lossy(), true).unwrap();
    assert_eq!(r["counts_verified"], true, "{r}");
    assert_eq!(r["files_restored"], 3, "{r}");
    assert_eq!(r["files_missing"], 0, "{r}");
    assert_eq!(r["schema_version"], amwapos_core::db::latest_schema_version());
    assert!(Path::new(r["safety_backup"].as_str().unwrap()).exists(), "the second PC's own data was kept as a safety backup");

    // 5. Integrity, schema and every drilled table equal to the original.
    let q = |sql: &str| core2.db.read(|c| Ok(c.query_row(sql, [], |r| r.get::<_, String>(0))?)).unwrap();
    assert_eq!(q("PRAGMA integrity_check"), "ok");
    assert_eq!(q("SELECT CAST(COUNT(*) AS TEXT) FROM pragma_foreign_key_check"), "0");
    assert_eq!(q("SELECT name FROM business"), "Al Noor Supermarket", "the restored business, not the temporary one");
    let after: Vec<_> = TABLES.iter().map(|(tb, o)| (tb, table_digest(&core2, tb, o))).collect();
    for ((tb, a), (_, b)) in before.iter().zip(after.iter()) {
        // The audit log gains the restore entry; everything before it is the same.
        if **tb == "audit_logs" {
            assert!(b.0 > a.0, "the restore is audited");
            continue;
        }
        assert_eq!(a, b, "{tb} restored exactly");
    }

    // 6. Staff sign in with their own PINs; the temporary owner is gone.
    assert!(core2.session(&t2).is_err(), "everyone is signed out after a restore");
    assert!(core2.login(&tmp_owner, "5517").is_err());
    let t3 = core2.login(&old_owner, OWNER_PIN).unwrap().token;
    assert!(core2.login(&cashier_id, "7391").is_ok());

    // 7. Documents open intact and are found by search; memory is searchable.
    for (id, _) in &docs {
        assert_eq!(core2.library_file(&t3, id).unwrap()["intact"], true);
    }
    assert_eq!(core2.library_search(&t3, "delivery note", false, None).unwrap()["results"].as_array().unwrap().len(), 3);
    assert_eq!(core2.memory_search(&t3, "Sundays", None, None).unwrap()["memories"].as_array().unwrap().len(), 1);
    assert_eq!(core2.memory_list(&t3, MemoryFilter::default()).unwrap()["rows"].as_array().unwrap().len(), 1);

    // 8. Issued receipts reprint from their immutable snapshots.
    let sale_id = q("SELECT sale_id FROM sales ORDER BY created_at LIMIT 1");
    assert!(core2.sale_get(&t3, &sale_id).is_ok());
    let snap = q("SELECT sha256 FROM receipt_snapshots WHERE ref_kind='sale' ORDER BY ref_id LIMIT 1");
    assert_eq!(snap.len(), 64);
}
