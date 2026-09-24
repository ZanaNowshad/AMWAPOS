//! Catalogue import from CSV with explicit preview → apply.
//!
//! Barcodes are always text. Rows with problems are reported individually and
//! never silently applied. Apply re-validates everything inside one
//! transaction, so a failure leaves the catalogue untouched.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::auth::Session;
use crate::catalog::{self, ProductCreate, ProductInput};
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::new_id;
use crate::money::parse_decimal;
use crate::service::AppCore;
use crate::time;
use crate::validate;

pub const FIELDS: &[(&str, &str, &[&str])] = &[
    ("sku", "SKU", &["sku", "code", "item code", "product code", "item_code", "itemcode", "ref"]),
    ("name", "Name", &["name", "product name", "product", "description", "item name", "item", "title", "product_name"]),
    ("name_ar", "Arabic name", &["name_ar", "arabic name", "arabic", "name (arabic)", "الاسم"]),
    ("category", "Category", &["category", "department", "group", "category name"]),
    ("barcodes", "Barcodes", &["barcode", "barcodes", "ean", "upc", "gtin", "bar code"]),
    ("price", "Selling price", &["price", "selling price", "retail price", "sell price", "rsp", "sale price", "unit price"]),
    ("cost", "Cost", &["cost", "cost price", "purchase price", "unit cost", "buy price"]),
    ("tax_rate", "VAT %", &["vat", "vat %", "tax", "tax rate", "vat rate", "tax %"]),
    ("unit", "Unit", &["unit", "uom", "unit of measure"]),
    ("track_inventory", "Track stock", &["track", "track stock", "track_inventory", "stocked"]),
    ("allow_decimal", "Sold by weight", &["weighted", "decimal", "allow_decimal", "by weight", "sold by weight"]),
    ("reorder_point", "Reorder point", &["reorder", "reorder point", "reorder level", "min stock", "minimum"]),
    ("stock", "Opening stock", &["stock", "qty", "quantity", "on hand", "opening stock", "stock qty"]),
];

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImportRequest {
    pub csv: String,
    /// field -> source column header
    #[serde(default)]
    pub mapping: Option<HashMap<String, String>>,
    /// When true, rows whose SKU already exists update that product.
    #[serde(default)]
    pub update_existing: bool,
    /// When true, apply valid rows and skip rows with errors.
    #[serde(default)]
    pub skip_errors: bool,
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportRow {
    pub row: usize,
    /// create | update | error
    pub action: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub sku: Option<String>,
    pub name: String,
    pub barcodes: Vec<String>,
    pub price_minor: Option<i64>,
    pub cost_minor: Option<i64>,
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportPreview {
    pub columns: Vec<String>,
    pub mapping: HashMap<String, String>,
    pub fields: Vec<serde_json::Value>,
    pub total_rows: usize,
    pub creates: usize,
    pub updates: usize,
    pub errors: usize,
    pub warnings: usize,
    pub barcodes_added: usize,
    pub new_categories: Vec<String>,
    pub rows: Vec<ImportRow>,
    pub spreadsheet_warning: Option<String>,
}

struct Parsed {
    row: ImportRow,
    create: Option<ProductCreate>,
    update_id: Option<String>,
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "" => None,
        "1" | "y" | "yes" | "true" | "t" => Some(true),
        "0" | "n" | "no" | "false" | "f" => Some(false),
        _ => None,
    }
}

fn detect(headers: &[String]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for (field, _, aliases) in FIELDS {
        if let Some(h) = headers.iter().find(|h| aliases.iter().any(|a| h.trim().eq_ignore_ascii_case(a))) {
            m.insert(field.to_string(), h.clone());
        }
    }
    m
}

fn split_barcodes(s: &str) -> Vec<String> {
    s.split(['|', ';', ',']).map(|b| b.trim().to_string()).filter(|b| !b.is_empty()).collect()
}

fn looks_scientific(s: &str) -> bool {
    let t = s.trim().to_ascii_uppercase();
    t.contains("E+") && t.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
}

fn analyse(c: &Connection, s: &Session, req: &ImportRequest, digits: u32) -> AppResult<(ImportPreview, Vec<Parsed>)> {
    if req.csv.len() > 60 * 1024 * 1024 {
        return Err(AppError::validation("The file is larger than 60 MB. Split it into smaller files."));
    }
    let text = req.csv.trim_start_matches('\u{feff}');
    let delim = {
        let first = text.lines().next().unwrap_or("");
        if first.matches(';').count() > first.matches(',').count() && first.matches(';').count() > first.matches('\t').count() {
            b';'
        } else if first.matches('\t').count() > first.matches(',').count() {
            b'\t'
        } else {
            b','
        }
    };
    let mut rdr = csv::ReaderBuilder::new().delimiter(delim).flexible(true).trim(csv::Trim::None).from_reader(text.as_bytes());
    let headers: Vec<String> = rdr
        .headers()
        .map_err(|e| AppError::validation(format!("The file could not be read as CSV: {e}")))?
        .iter()
        .map(|h| h.trim().to_string())
        .collect();
    if headers.is_empty() || headers.iter().all(|h| h.is_empty()) {
        return Err(AppError::validation("The first row must contain column names."));
    }
    let mapping = req.mapping.clone().filter(|m| !m.is_empty()).unwrap_or_else(|| detect(&headers));
    for (f, col) in &mapping {
        if !FIELDS.iter().any(|x| x.0 == f) {
            return Err(AppError::validation(format!("Unknown field {f}.")));
        }
        if !col.is_empty() && !headers.contains(col) {
            return Err(AppError::validation(format!("Column '{col}' is not in the file.")));
        }
    }
    if !mapping.contains_key("name") {
        return Err(AppError::validation("Map a column to the product Name."));
    }
    let idx: HashMap<&str, usize> =
        mapping.iter().filter_map(|(f, col)| headers.iter().position(|h| h == col).map(|i| (f.as_str(), i))).collect();
    let tax_rules: Vec<(String, i64)> = {
        let mut st = c.prepare("SELECT tax_rule_id, rate_bp FROM tax_rules WHERE active=1 ORDER BY created_at")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let default_tax =
        tax_rules.iter().max_by_key(|t| t.1).map(|t| t.0.clone()).ok_or_else(|| AppError::validation("Create a tax rule first."))?;
    let categories: HashMap<String, String> = {
        let mut st = c.prepare("SELECT lower(name), category_id FROM categories WHERE active=1")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<HashMap<_, _>, _>>()?;
        rows
    };
    let mut parsed: Vec<Parsed> = vec![];
    let mut new_cats: HashSet<String> = HashSet::new();
    let mut sci = 0usize;
    for (i, rec) in rdr.records().enumerate() {
        let row_no = i + 2;
        if i >= 200_000 {
            return Err(AppError::validation("Import at most 200,000 rows at a time."));
        }
        let rec = rec.map_err(|e| AppError::validation(format!("Row {row_no}: {e}")))?;
        let get = |f: &str| idx.get(f).and_then(|i| rec.get(*i)).map(|v| v.trim().to_string()).unwrap_or_default();
        if rec.iter().all(|v| v.trim().is_empty()) {
            continue;
        }
        let mut errors = vec![];
        let mut warnings = vec![];
        let name = get("name");
        if name.is_empty() {
            errors.push("Name is empty.".to_string());
        }
        let sku = Some(get("sku")).filter(|s| !s.is_empty());
        let raw_bc = get("barcodes");
        let mut barcodes = vec![];
        for b in split_barcodes(&raw_bc) {
            if looks_scientific(&b) {
                sci += 1;
                errors.push(format!("Barcode '{b}' was converted to scientific notation by a spreadsheet. Export barcodes as text."));
                continue;
            }
            match validate::barcode(&b) {
                Ok(v) => {
                    if v.chars().all(|c| c.is_ascii_digit()) && ![8, 12, 13, 14].contains(&v.len()) {
                        warnings.push(format!(
                            "Barcode {v} has {} digits (EAN/UPC codes have 8, 12, 13 or 14). Leading zeros may have been lost.",
                            v.len()
                        ));
                    }
                    if !barcodes.contains(&v) {
                        barcodes.push(v)
                    }
                }
                Err(e) => errors.push(format!("Barcode '{b}': {}", e.message)),
            }
        }
        let money = |f: &str, label: &str, errors: &mut Vec<String>| -> Option<i64> {
            let v = get(f);
            if v.is_empty() {
                return None;
            }
            let cleaned = v.replace("BHD", "").replace("BD", "").trim().to_string();
            match parse_decimal(&cleaned, digits) {
                Ok(x) if x >= 0 => Some(x),
                Ok(_) => {
                    errors.push(format!("{label} cannot be negative."));
                    None
                }
                Err(_) => {
                    errors.push(format!("{label} '{v}' is not a valid amount (max {digits} decimals)."));
                    None
                }
            }
        };
        let price = money("price", "Price", &mut errors);
        let cost = money("cost", "Cost", &mut errors);
        let qty = |f: &str, label: &str, errors: &mut Vec<String>| -> Option<i64> {
            let v = get(f);
            if v.is_empty() {
                return None;
            }
            match parse_decimal(&v, 3) {
                Ok(x) => Some(x),
                Err(_) => {
                    errors.push(format!("{label} '{v}' is not a valid quantity."));
                    None
                }
            }
        };
        let reorder = qty("reorder_point", "Reorder point", &mut errors).unwrap_or(0);
        let stock = qty("stock", "Stock", &mut errors);
        let allow_dec = parse_bool(&get("allow_decimal")).unwrap_or(false);
        let track = parse_bool(&get("track_inventory")).unwrap_or(true);
        if !allow_dec {
            if let Some(q) = stock {
                if q % 1000 != 0 {
                    errors.push("Stock must be a whole number unless the product is sold by weight.".into());
                }
            }
        }
        let tax_rate_bp = {
            let v = get("tax_rate").replace('%', "");
            if v.is_empty() {
                None
            } else {
                match parse_decimal(&v, 2) {
                    Ok(bp) => {
                        // "10" means 10% -> 1000 bp.
                        if !tax_rules.iter().any(|t| t.1 == bp) {
                            errors.push(format!("No active tax rule has rate {v}%. Create it first."));
                        }
                        Some(bp)
                    }
                    Err(_) => {
                        errors.push(format!("VAT '{v}' is not a valid percentage."));
                        None
                    }
                }
            }
        };
        let category = Some(get("category")).filter(|c| !c.is_empty());
        let category_id = match &category {
            Some(cn) => match categories.get(&cn.to_lowercase()) {
                Some(id) => Some(id.clone()),
                None => {
                    new_cats.insert(cn.clone());
                    None
                }
            },
            None => None,
        };
        // Existing data
        let existing: Option<(String, String)> = match &sku {
            Some(sk) => c
                .query_row("SELECT product_id, name FROM products WHERE sku=?1 COLLATE NOCASE", [sk], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?,
            None => None,
        };
        let mut action = "create".to_string();
        let mut update_id = None;
        if let Some((pid, pname)) = &existing {
            if req.update_existing {
                action = "update".into();
                update_id = Some(pid.clone());
                if stock.is_some() {
                    warnings.push("Stock is ignored for existing products. Use a stock adjustment or stocktake.".into());
                }
            } else {
                errors.push(format!(
                    "SKU {} already exists ({pname}). Enable 'update existing products' to update it.",
                    sku.clone().unwrap_or_default()
                ));
            }
        } else if price.is_none() {
            errors.push("Selling price is required for new products.".into());
        }
        for b in &barcodes {
            if let Some((owner, oname)) = catalog::barcode_owner(c, b)? {
                if Some(&owner) != update_id.as_ref() {
                    errors.push(format!("Barcode {b} already belongs to {oname}."));
                }
            }
        }
        if !errors.is_empty() {
            action = "error".into();
        }
        let tax_id =
            tax_rate_bp.and_then(|bp| tax_rules.iter().find(|t| t.1 == bp).map(|t| t.0.clone())).unwrap_or_else(|| default_tax.clone());
        let unit = Some(get("unit")).filter(|u| !u.is_empty()).unwrap_or_else(|| if allow_dec { "kg".into() } else { "pcs".into() });
        let create = if action == "create" || action == "update" {
            Some(ProductCreate {
                product: ProductInput {
                    sku: sku.clone(),
                    name: name.clone(),
                    name_ar: Some(get("name_ar")).filter(|x| !x.is_empty()),
                    description: None,
                    category_id: category_id.clone(),
                    tax_rule_id: tax_id,
                    unit,
                    track_inventory: track,
                    allow_decimal_quantity: allow_dec,
                    reorder_point_milli: reorder,
                    is_favorite: false,
                },
                price_minor: price.unwrap_or(0),
                cost_minor: if s.has("products.view_cost") { cost } else { None },
                barcodes: barcodes.clone(),
                opening_stock_milli: if action == "create" && track { stock } else { None },
            })
        } else {
            None
        };
        parsed.push(Parsed {
            row: ImportRow { row: row_no, action, errors, warnings, sku, name, barcodes, price_minor: price, cost_minor: cost, category },
            create,
            update_id,
        });
    }
    // Isolate every row involved in an in-file duplicate, including the first occurrence.
    let mut bc_rows: HashMap<String, Vec<usize>> = HashMap::new();
    let mut sku_rows: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, p) in parsed.iter().enumerate() {
        for b in &p.row.barcodes {
            bc_rows.entry(b.clone()).or_default().push(i);
        }
        if let Some(sk) = &p.row.sku {
            sku_rows.entry(sk.to_lowercase()).or_default().push(i);
        }
    }
    for (label, groups) in [("Barcode", &bc_rows), ("SKU", &sku_rows)] {
        for (value, idxs) in groups.iter().filter(|(_, v)| v.len() > 1) {
            let rows_txt = idxs.iter().map(|i| parsed[*i].row.row.to_string()).collect::<Vec<_>>().join(", ");
            for i in idxs {
                let p = &mut parsed[*i];
                let msg = format!("{label} {value} appears on rows {rows_txt}.");
                if !p.row.errors.contains(&msg) {
                    p.row.errors.push(msg);
                }
                p.row.action = "error".into();
                p.create = None;
            }
        }
    }
    let rows: Vec<ImportRow> = parsed.iter().map(|p| p.row.clone()).collect();
    let mut new_categories: Vec<String> = new_cats.into_iter().collect();
    new_categories.sort();
    let preview = ImportPreview {
        columns: headers,
        mapping,
        fields: FIELDS.iter().map(|f| json!({ "key": f.0, "label": f.1 })).collect(),
        total_rows: rows.len(),
        creates: rows.iter().filter(|r| r.action == "create").count(),
        updates: rows.iter().filter(|r| r.action == "update").count(),
        errors: rows.iter().filter(|r| r.action == "error").count(),
        warnings: rows.iter().filter(|r| !r.warnings.is_empty()).count(),
        barcodes_added: rows.iter().filter(|r| r.action != "error").map(|r| r.barcodes.len()).sum(),
        new_categories,
        rows,
        spreadsheet_warning: if sci > 0 {
            Some(format!("{sci} barcode(s) look like they were converted by a spreadsheet (e.g. 6.29E+12). Re-export with the barcode column formatted as Text."))
        } else {
            None
        },
    };
    Ok((preview, parsed))
}

impl AppCore {
    pub fn products_import_preview(&self, token: &str, req: ImportRequest) -> AppResult<ImportPreview> {
        let s = self.session(token)?;
        s.require("import.run")?;
        s.require("products.manage")?;
        let digits = self.db.read(|c| self.currency(c))?.1;
        let (mut p, _) = self.db.read(|c| analyse(c, &s, &req, digits))?;
        // Keep the response small: errors first, then a sample.
        p.rows.sort_by_key(|r| (r.action != "error", r.warnings.is_empty(), r.row));
        p.rows.truncate(1000);
        Ok(p)
    }

    pub fn products_import_apply(&self, token: &str, req: ImportRequest) -> AppResult<serde_json::Value> {
        let s = self.session(token)?;
        s.require("import.run")?;
        s.require("products.manage")?;
        self.require_back_office_writable()?;
        let op = req.operation_id.clone().ok_or_else(|| AppError::validation("An operation id is required."))?;
        let digits = self.db.read(|c| self.currency(c))?.1;
        let actor = self.actor(&s, None);
        let started = std::time::Instant::now();
        self.db.write(|tx| {
            let payload = json!({ "sha": crate::auth::sha256_hex(&req.csv), "mapping": req.mapping, "update": req.update_existing, "skip": req.skip_errors });
            if let Check::Replay { result } = idempotency::check(tx, &op, "products.import", &payload)? {
                return Ok(result);
            }
            let hash = idempotency::payload_hash("products.import", &payload)?;
            let (preview, parsed) = analyse(tx, &s, &req, digits)?;
            if preview.errors > 0 && !req.skip_errors {
                return Err(AppError::validation(format!("{} row(s) have errors. Fix them or choose to skip rows with errors.", preview.errors)));
            }
            // Create missing categories.
            let now = time::now_str();
            let mut cat_ids: HashMap<String, String> = HashMap::new();
            for name in &preview.new_categories {
                let id = new_id();
                tx.execute(
                    "INSERT INTO categories(category_id, parent_id, name, sort_order, active, created_at, updated_at) VALUES (?1,NULL,?2,0,1,?3,?3)",
                    params![id, name, now],
                )?;
                cat_ids.insert(name.to_lowercase(), id);
            }
            let mut created = 0;
            let mut updated = 0;
            let mut barcodes = 0;
            let mut skipped = 0;
            for p in parsed {
                let Some(mut create) = p.create else {
                    skipped += 1;
                    continue;
                };
                if create.product.category_id.is_none() {
                    if let Some(cn) = &p.row.category {
                        create.product.category_id = cat_ids.get(&cn.to_lowercase()).cloned();
                    }
                }
                match p.update_id {
                    None => {
                        catalog::insert_product(tx, &s, &create, "import")?;
                        created += 1;
                        barcodes += create.barcodes.len();
                    }
                    Some(pid) => {
                        let pi = &create.product;
                        tx.execute(
                            "UPDATE products SET name=?2, name_ar=COALESCE(?3,name_ar), category_id=COALESCE(?4,category_id), tax_rule_id=?5, unit=?6,
                                track_inventory=?7, allow_decimal_quantity=?8, reorder_point_milli=?9, updated_at=?10, version=version+1 WHERE product_id=?1",
                            params![pid, pi.name, pi.name_ar, pi.category_id, pi.tax_rule_id, pi.unit, pi.track_inventory as i64, pi.allow_decimal_quantity as i64, pi.reorder_point_milli, now],
                        )?;
                        if let Some(price) = p.row.price_minor {
                            if catalog::current_price(tx, &pid)? != Some(price) {
                                catalog::set_price(tx, &s, &pid, price, Some("Import"), &now, &actor)?;
                            }
                        }
                        for b in &create.barcodes {
                            if catalog::barcode_owner(tx, b)?.is_none() {
                                catalog::add_barcode(tx, &s, &pid, b, false, "import")?;
                                barcodes += 1;
                            }
                        }
                        if let (Some(cost), true) = (create.cost_minor, s.has("products.view_cost")) {
                            tx.execute(
                                "INSERT INTO product_cost_history(cost_id, product_id, supplier_id, cost_minor, source, effective_at, created_by) VALUES (?1,?2,NULL,?3,'import',?4,?5)",
                                params![new_id(), pid, cost, now, s.user_id],
                            )?;
                            tx.execute(
                                "INSERT INTO product_costs(product_id, branch_id, avg_cost_minor, last_cost_minor, updated_at) VALUES (?1,?2,?3,?3,?4)
                                 ON CONFLICT(product_id, branch_id) DO UPDATE SET last_cost_minor=?3, updated_at=?4",
                                params![pid, s.branch_id, cost, now],
                            )?;
                        }
                        updated += 1;
                    }
                }
            }
            let result = json!({ "created": created, "updated": updated, "skipped": skipped, "barcodes_added": barcodes,
                "categories_created": preview.new_categories.len(), "duration_ms": started.elapsed().as_millis() as i64 });
            audit::record(tx, &actor, "products.imported", "product", None, None, Some(&result))?;
            idempotency::complete(tx, &op, "products.import", Some(&s.user_id), Some(&s.device_id), &hash, None, &result)?;
            Ok(result)
        })
    }
}
