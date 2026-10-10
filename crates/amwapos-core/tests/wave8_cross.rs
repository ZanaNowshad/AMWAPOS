//! Wave 8 cross-wave verification: upgrades from older schemas, the role
//! matrix for the new screens, working without an AI provider, backup and
//! restore, and failure injection (docs/INTELLIGENCE_AND_EVIDENCE.md).

mod common;

use std::sync::Arc;

use amwapos_core::library::{AddInput, ListFilter};
use amwapos_core::memory::{MemoryFilter, MemoryInput};
use amwapos_core::service::{AppCore, MemorySecretStore};
use amwapos_core::ErrorCode;
use common::*;
use serde_json::json;

fn png(seed: u8) -> Vec<u8> {
    let g = image::GrayImage::from_fn(24, 12, |x, y| image::Luma([(x as u8).wrapping_mul(7).wrapping_add(y as u8).wrapping_add(seed)]));
    let mut out = std::io::Cursor::new(vec![]);
    image::DynamicImage::ImageLuma8(g).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn add_doc(e: &Env, t: &str, seed: u8) -> String {
    e.core
        .library_add(
            t,
            AddInput {
                file_name: format!("doc-{seed}.png"),
                data_base64: amwapos_core::ids::b64(&png(seed)),
                category: "other".into(),
                ..Default::default()
            },
        )
        .unwrap()["document_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn fact(s: &str) -> MemoryInput {
    MemoryInput { statement: s.into(), ..Default::default() }
}

#[test]
fn every_older_schema_upgrades_to_this_one_with_empty_memory_and_a_clean_database() {
    for from in [1, 9, 22, 27, 30, 33, 36] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("amwapos.db");
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            amwapos_core::db::migrate_until(&c, &path, from).unwrap();
        }
        let core = AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap();
        let n = |sql: &str| core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get::<_, i64>(0))?)).unwrap();
        assert_eq!(n("SELECT MAX(version) FROM schema_migrations"), amwapos_core::db::latest_schema_version(), "from schema {from}");
        assert_eq!(n("SELECT COUNT(*) FROM business_memories"), 0, "memory starts empty (from {from})");
        assert_eq!(n("SELECT COUNT(*) FROM library_documents"), 0, "no document invented (from {from})");
        assert_eq!(n("SELECT COUNT(*) FROM pragma_foreign_key_check"), 0, "from {from}");
        assert_eq!(core.db.read(|c| Ok(c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?)).unwrap(), "ok");
    }
}

#[test]
fn an_upgrade_brings_earlier_receipts_into_the_library_without_copying_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("amwapos.db");
    let file = dir.path().join("expenses").join("old.png");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let bytes = png(3);
    std::fs::write(&file, &bytes).unwrap();
    let sha = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(&bytes))
    };
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        amwapos_core::db::migrate_until(&c, &path, 36).unwrap();
        c.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        c.execute(
            "INSERT INTO expense_attachments(attachment_id, expense_id, file_name, path, mime, sha256, added_by, added_at)
             VALUES ('att1','exp1','old.png',?1,'image/png',?2,'u1','2026-03-01T10:00:00Z')",
            rusqlite::params![file.to_string_lossy(), sha],
        )
        .unwrap();
    }
    let core = AppCore::open(dir.path(), Arc::new(MemorySecretStore::default())).unwrap();
    core.db.write(|c| amwapos_core::library::adopt_existing(c, dir.path())).unwrap();
    let (src, path_kept, added_at): (String, String, String) = core
        .db
        .read(|c| {
            Ok(c.query_row(
                "SELECT d.source, f.path, d.added_at FROM library_documents d JOIN library_files f ON f.sha256=d.sha256",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .unwrap();
    assert_eq!(src, "expense_attachment");
    assert_eq!(path_kept, "expenses/old.png", "the file stays where it was");
    assert_eq!(added_at, "2026-03-01T10:00:00Z", "the original time, not the upgrade time");
    // Adopted once.
    core.db.write(|c| amwapos_core::library::adopt_existing(c, dir.path())).unwrap();
    assert_eq!(core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM library_documents", [], |r| r.get::<_, i64>(0))?)).unwrap(), 1);
}

#[test]
fn each_built_in_role_gets_exactly_the_new_screens_it_should() {
    let e = env();
    let t = &e.owner_token;
    let doc = add_doc(&e, t, 1);
    e.core.memory_add(t, fact("The shop opens at 7 on Fridays"), true).unwrap();
    // (role, library view, library add, memory view, memory add, radar)
    let matrix: &[(&str, bool, bool, bool, bool, bool)] = &[
        ("role_manager", true, true, true, true, true),
        ("role_accountant", true, false, false, false, true),
        ("role_inventory", true, true, false, false, false),
        ("role_cashier", false, false, false, false, false),
        ("role_delivery", false, false, false, false, false),
    ];
    for (i, (role, lv, la, mv, ma, rv)) in matrix.iter().enumerate() {
        let (_, tk) = e.user(&format!("User {i}"), role, &format!("{}", 3571 + i * 1111));
        let ok = |r: bool, res: Result<serde_json::Value, amwapos_core::AppError>, what: &str| match (r, res) {
            (true, Ok(_)) => {}
            (false, Err(err)) => assert_eq!(err.code, ErrorCode::Forbidden, "{role} {what}"),
            (want, got) => panic!("{role} {what}: wanted allowed={want}, got {got:?}"),
        };
        ok(*lv, e.core.library_list(&tk, ListFilter::default()), "library list");
        ok(*lv, e.core.library_get(&tk, &doc), "library get");
        ok(
            *la,
            e.core.library_add(
                &tk,
                AddInput {
                    file_name: "x.png".into(),
                    data_base64: amwapos_core::ids::b64(&png(50 + i as u8)),
                    category: "other".into(),
                    ..Default::default()
                },
            ),
            "library add",
        );
        ok(*mv, e.core.memory_list(&tk, MemoryFilter::default()), "memory list");
        ok(*ma, e.core.memory_add(&tk, fact(&format!("Fact number {i} for the role test")), true), "memory add");
        ok(*rv, e.core.cashflow_radar(&tk, None), "radar");
    }
}

#[test]
fn the_library_memory_and_radar_work_with_no_ai_provider() {
    let e = env();
    let t = &e.owner_token;
    // The assistant is off and no provider is configured.
    e.core.settings_save(t, "features", json!({ "ai.enabled": false })).unwrap();
    let doc = add_doc(&e, t, 2);
    e.core
        .library_text_done(
            e.core.library_get(t, &doc).unwrap()["document"]["sha256"].as_str().unwrap(),
            vec![(1, "Rent receipt March".into())],
            "ocr",
        )
        .unwrap();
    assert_eq!(e.core.library_search(t, "rent receipt", false, None).unwrap()["results"].as_array().unwrap().len(), 1);
    e.core.memory_add(t, fact("The landlord prefers bank transfer"), true).unwrap();
    assert_eq!(e.core.memory_search(t, "landlord", None, None).unwrap()["memories"].as_array().unwrap().len(), 1);
    assert!(e.core.cashflow_radar(t, Some(30)).is_ok());
    // The assistant's tools say it is off; nothing else depends on it.
    let (r, err) = e.core.ai_tool(t, "01NOAI0000000000000000000", "library_search", &json!({ "q": "rent" }));
    assert!(err, "{r}");
}

#[test]
fn a_restore_brings_back_documents_memory_and_their_files() {
    let e = env();
    let t = &e.owner_token;
    let doc = add_doc(&e, t, 4);
    e.core.memory_add(t, fact("Deliveries come on Sundays"), true).unwrap();
    let out = e.dir.path().join("bk");
    let b = e.core.backup_create(t, Some(out.to_string_lossy().into())).unwrap();
    // After the backup: one more memory, and the library folder is lost.
    e.core.memory_add(t, fact("Added after the backup"), true).unwrap();
    std::fs::remove_dir_all(e.dir.path().join("library")).unwrap();
    let r = e.core.backup_restore(t, &b.path, false).unwrap();
    assert_eq!(r["files_restored"], 1);
    assert_eq!(r["files_missing"], 0);
    let t = e.core.login(&e.owner_id, OWNER_PIN).unwrap().token;
    let rows = e.core.memory_list(&t, MemoryFilter::default()).unwrap()["rows"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 1, "the memory added after the backup is gone with the restore");
    assert_eq!(e.core.library_file(&t, &doc).unwrap()["intact"], true);
    assert!(
        e.core.memory_search(&t, "Sundays", None, None).unwrap()["memories"].as_array().unwrap().len() == 1,
        "search index came back too"
    );
}

#[test]
fn damaged_or_missing_files_are_reported_never_hidden() {
    let e = env();
    let t = &e.owner_token;
    let doc = add_doc(&e, t, 5);
    let got = e.core.library_get(t, &doc).unwrap();
    let sha = got["document"]["sha256"].as_str().unwrap().to_string();
    let path = e.dir.path().join("library").join(&sha[..2]).join(format!("{sha}.png"));
    // Bytes changed on disk: still served, flagged as not matching.
    std::fs::write(&path, b"tampered").unwrap();
    assert_eq!(e.core.library_file(t, &doc).unwrap()["intact"], false);
    // A backup does not copy a file that no longer matches its hash.
    let out = e.dir.path().join("bk");
    let b = e.core.backup_create(t, Some(out.to_string_lossy().into())).unwrap();
    assert!(b.error.as_deref().unwrap_or_default().contains("could not be copied"), "{:?}", b.error);
    assert!(!out.join("AMWAPOS-files").join(&sha[..2]).join(&sha).exists());
    // The file is gone: the message says where to get it back.
    std::fs::remove_file(&path).unwrap();
    let err = e.core.library_file(t, &doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(err.message.contains("Restore it from a backup"));
    // A broken search index is rebuilt from what is stored.
    e.core.db.write(|c| Ok(c.execute("DELETE FROM library_fts", [])?)).unwrap();
    e.core.library_reindex(t).unwrap();
    assert_eq!(e.core.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM library_fts", [], |r| r.get::<_, i64>(0))?)).unwrap(), 1);
}
