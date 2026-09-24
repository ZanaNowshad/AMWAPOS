//! Migration: read → detect → preview (no writes) → apply through normal commands.
mod common;

use amwapos_core::migration::{TableRequest, UploadFile};
use common::*;
use serde_json::{json, Value};

fn count(e: &Env, sql: &str) -> i64 {
    e.core.db.read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?)).unwrap()
}

fn upload(name: &str, text: &str) -> UploadFile {
    UploadFile { name: name.into(), data: amwapos_core::ids::b64(text.as_bytes()) }
}

fn req(table: &Value, op_id: &str) -> TableRequest {
    serde_json::from_value(json!({ "table": table, "operation_id": op_id, "skip_errors": true })).unwrap()
}

#[test]
fn migration_of_customers_suppliers_stock_and_products() {
    let e = env();
    let t = &e.owner_token;
    e.product("Tea 100s", "0001112223334", 1_900, 1_200, 5_000);
    let read = e
        .core
        .migration_read(
            t,
            vec![
                upload("clients.csv", "Name,Mobile,Area\nFatima,33334444,Riffa\nAli,33334444,Isa Town\n,39990000,\n"),
                upload("vendors.csv", "Supplier Name,CR No,VAT Number\nGulf Foods,12345-1,200000000000003\n"),
                upload("stock count.csv", "Barcode,Qty On Hand\n0001112223334,12\n9999999999999,3\n"),
                upload("items.csv", "Barcode,Product Name,Price,Cost\n0009990001112,Rice 5kg,4.000,3.000\n"),
                upload("photo.jpg", "x"),
            ],
        )
        .unwrap();
    let ops: Vec<String> = (0..6).map(|_| op()).collect();
    let tables = read["tables"].as_array().unwrap();
    let ent: Vec<&str> = tables.iter().map(|x| x["entity"].as_str().unwrap()).collect();
    assert_eq!(ent, vec!["customers", "suppliers", "stock", "products"]);
    assert_eq!(read["ignored"][0], "photo.jpg");

    // Preview writes nothing.
    let before = count(&e, "SELECT COUNT(*) FROM audit_logs");
    let p = e.core.migration_preview(t, req(&tables[0], &ops[1])).unwrap();
    assert_eq!(p["preview"]["summary"]["creates"], 1);
    assert_eq!(p["preview"]["summary"]["errors"], 2, "duplicate phone and empty name");
    let sp = e.core.migration_preview(t, req(&tables[2], &ops[3])).unwrap();
    assert_eq!(sp["preview"]["summary"]["updates"], 1);
    assert_eq!(sp["preview"]["summary"]["errors"], 1, "unknown barcode");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM audit_logs"), before);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM customers"), 0);

    // Apply.
    let r = e.core.migration_apply(t, req(&tables[0], &ops[1])).unwrap();
    assert_eq!(r["result"]["applied"], 1);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM customers WHERE phone='+97333334444'"), 1);
    e.core.migration_apply(t, req(&tables[1], &ops[2])).unwrap();
    assert_eq!(count(&e, "SELECT COUNT(*) FROM suppliers WHERE vat_number='200000000000003'"), 1);
    e.core.migration_apply(t, req(&tables[2], &ops[3])).unwrap();
    assert_eq!(count(&e, "SELECT SUM(qty_milli) FROM stock_levels"), 12_000);
    // Re-applying the same stock migration does not double count.
    e.core.migration_apply(t, req(&tables[2], &ops[3])).unwrap();
    assert_eq!(count(&e, "SELECT SUM(qty_milli) FROM stock_levels"), 12_000);
    let pr = e.core.migration_apply(t, req(&tables[3], &ops[4])).unwrap();
    assert_eq!(pr["result"]["created"], 1, "{pr}");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM product_barcodes WHERE barcode='0009990001112'"), 1, "leading zeros kept");
    // Customers already present are skipped on a second run.
    let again = e.core.migration_preview(t, req(&tables[0], &ops[5])).unwrap();
    assert_eq!(again["preview"]["summary"]["skipped"], 1);

    // A cashier cannot migrate.
    let (_, cashier) = e.user("Sara", "role_cashier", "2468");
    assert!(e.core.migration_read(&cashier, vec![upload("a.csv", "Name\nx\n")]).is_err());
}
