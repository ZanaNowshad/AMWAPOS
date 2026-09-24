//! Migration from another system: read CSV / XLSX / ZIP / folder uploads into
//! tables, detect what each table holds (products, customers, suppliers,
//! opening stock), map columns, validate, preview, then apply on request.
//!
//! Nothing is written until `migration_apply`. Products go through the
//! existing product importer (same validation and duplicate rules). Customers,
//! suppliers and stock are validated here and written through the normal
//! commands, so permissions and audit are identical to manual entry. Every
//! cell is read as text: barcodes keep their leading zeros, and numeric
//! spreadsheet cells in code columns are reported because Excel may already
//! have dropped digits.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use calamine::{Data, Reader};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};
use crate::service::AppCore;
use crate::validate;

const MAX_ROWS: usize = 200_000;
const MAX_FILE_BYTES: usize = 60 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UploadFile {
    pub name: String,
    /// base64 content
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    /// "file.xlsx › Sheet1"
    pub source: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Columns that held numeric spreadsheet cells (possible lost leading zeros).
    pub numeric_columns: Vec<String>,
    /// products | customers | suppliers | stock | unknown
    pub entity: String,
    pub mapping: HashMap<String, String>,
}

pub const ENTITIES: &[&str] = &["products", "customers", "suppliers", "stock"];

fn fields(entity: &str) -> Vec<(&'static str, &'static str, &'static [&'static str])> {
    match entity {
        "customers" => vec![
            ("name", "Name", &["name", "customer", "customer name", "client", "full name"]),
            ("phone", "Phone", &["phone", "mobile", "tel", "telephone", "phone number", "contact number"]),
            ("whatsapp", "WhatsApp", &["whatsapp", "whatsapp number", "wa"]),
            ("email", "Email", &["email", "e-mail", "mail"]),
            ("area", "Area", &["area", "block", "city", "region", "district"]),
            ("address", "Address", &["address", "street", "building", "flat", "location"]),
        ],
        "suppliers" => vec![
            ("name", "Name", &["supplier", "supplier name", "vendor", "vendor name", "name", "company"]),
            ("cr_number", "CR number", &["cr", "cr number", "cr no", "commercial registration"]),
            ("vat_number", "VAT number", &["vat", "vat number", "vat no", "trn", "tax number"]),
            ("contact_name", "Contact", &["contact", "contact name", "contact person"]),
            ("phone", "Phone", &["phone", "mobile", "tel", "telephone"]),
            ("email", "Email", &["email", "e-mail"]),
            ("address", "Address", &["address"]),
            ("payment_terms", "Payment terms", &["terms", "payment terms", "credit terms"]),
        ],
        "stock" => vec![
            ("barcode", "Barcode", &["barcode", "ean", "upc", "gtin", "bar code"]),
            ("sku", "SKU", &["sku", "code", "item code", "product code", "ref"]),
            ("qty", "Quantity on hand", &["qty", "quantity", "stock", "on hand", "qty on hand", "stock qty", "balance", "count"]),
        ],
        _ => crate::importer::FIELDS.to_vec(),
    }
}

fn norm(h: &str) -> String {
    h.trim().to_lowercase().replace(['_', '-'], " ")
}

fn auto_map(entity: &str, headers: &[String]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for (field, _, aliases) in fields(entity) {
        if let Some(h) = headers.iter().find(|h| aliases.iter().any(|a| norm(h) == *a)) {
            m.insert(field.to_string(), h.clone());
        }
    }
    m
}

/// Guess the entity from the headers (and the file name as a tie-breaker).
pub fn detect_entity(source: &str, headers: &[String]) -> String {
    let src = source.to_lowercase();
    let score = |e: &str| -> usize {
        let m = auto_map(e, headers);
        let base = m.len() * 2;
        let bonus = match e {
            "products" => {
                (m.contains_key("price") as usize) * 3
                    + (m.contains_key("barcodes") as usize) * 2
                    + src.contains("product") as usize * 3
                    + src.contains("item") as usize * 2
            }
            "customers" => {
                (m.contains_key("phone") && !m.contains_key("cr_number")) as usize * 2
                    + src.contains("customer") as usize * 4
                    + src.contains("client") as usize * 4
            }
            "suppliers" => {
                (m.contains_key("cr_number") || m.contains_key("vat_number")) as usize * 3
                    + src.contains("supplier") as usize * 4
                    + src.contains("vendor") as usize * 4
            }
            "stock" => {
                (m.contains_key("qty") && m.len() <= 3) as usize * 2
                    + src.contains("stock") as usize * 4
                    + src.contains("inventory") as usize * 4
            }
            _ => 0,
        };
        // A table that needs "name" must have one.
        let needs_name = matches!(e, "products" | "customers" | "suppliers");
        if needs_name && !m.contains_key("name") {
            return 0;
        }
        if e == "stock" && (!m.contains_key("qty") || !(m.contains_key("barcode") || m.contains_key("sku"))) {
            return 0;
        }
        base + bonus
    };
    ENTITIES
        .iter()
        .map(|e| (score(e), *e))
        .max_by_key(|x| x.0)
        .filter(|x| x.0 > 0)
        .map(|x| x.1.to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn cell_text(d: &Data) -> (String, bool) {
    match d {
        Data::Empty => (String::new(), false),
        Data::String(s) => (s.trim().to_string(), false),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                (format!("{}", *f as i64), true)
            } else {
                (format!("{f}"), true)
            }
        }
        Data::Int(i) => (i.to_string(), true),
        Data::Bool(b) => (b.to_string(), false),
        Data::DateTime(dt) => {
            // Excel serial day number (1900 system).
            let base = chrono::NaiveDate::from_ymd_opt(1899, 12, 30).unwrap_or_default();
            let d = base + chrono::Duration::days(dt.as_f64().floor() as i64);
            (d.format("%Y-%m-%d").to_string(), false)
        }
        Data::DateTimeIso(s) | Data::DurationIso(s) => (s.clone(), false),
        Data::Error(e) => (format!("#{e:?}"), false),
    }
}

fn table_from_grid(source: String, grid: Vec<Vec<(String, bool)>>) -> Option<Table> {
    let mut it = grid.into_iter().skip_while(|r| r.iter().all(|c| c.0.is_empty()));
    let headers: Vec<String> = it.next()?.into_iter().map(|c| c.0).collect();
    if headers.iter().all(|h| h.is_empty()) {
        return None;
    }
    let headers: Vec<String> =
        headers.iter().enumerate().map(|(i, h)| if h.is_empty() { format!("Column {}", i + 1) } else { h.clone() }).collect();
    let mut numeric = vec![false; headers.len()];
    let mut rows = vec![];
    for r in it {
        if r.iter().all(|c| c.0.is_empty()) {
            continue;
        }
        let mut row = vec![String::new(); headers.len()];
        for (i, (v, num)) in r.into_iter().enumerate().take(headers.len()) {
            numeric[i] |= num;
            row[i] = v;
        }
        rows.push(row);
    }
    let entity = detect_entity(&source, &headers);
    let mapping = auto_map(&entity, &headers);
    Some(Table {
        numeric_columns: headers.iter().zip(&numeric).filter(|(_, n)| **n).map(|(h, _)| h.clone()).collect(),
        source,
        headers,
        rows,
        entity,
        mapping,
    })
}

fn read_csv(source: &str, bytes: &[u8]) -> AppResult<Option<Table>> {
    let text = String::from_utf8_lossy(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)).to_string();
    let delim = {
        let first = text.lines().next().unwrap_or("");
        b",;\t".iter().copied().max_by_key(|d| first.matches(*d as char).count()).unwrap_or(b',')
    };
    let mut r = csv::ReaderBuilder::new().has_headers(false).flexible(true).delimiter(delim).from_reader(text.as_bytes());
    let mut grid = vec![];
    for rec in r.records() {
        let rec = rec.map_err(|e| AppError::validation(format!("{source}: {e}")))?;
        grid.push(rec.iter().map(|c| (validate::csv_unescape_cell(c.trim()).to_string(), false)).collect());
    }
    Ok(table_from_grid(source.to_string(), grid))
}

fn read_xlsx(source: &str, bytes: Vec<u8>) -> AppResult<Vec<Table>> {
    let mut wb = calamine::open_workbook_auto_from_rs(Cursor::new(bytes))
        .map_err(|e| AppError::validation(format!("{source}: not a readable spreadsheet ({e}).")))?;
    let mut out = vec![];
    for name in wb.sheet_names().to_owned() {
        let range = wb.worksheet_range(&name).map_err(|e| AppError::validation(format!("{source} › {name}: {e}")))?;
        let grid: Vec<Vec<(String, bool)>> = range.rows().map(|r| r.iter().map(cell_text).collect()).collect();
        if let Some(t) = table_from_grid(format!("{source} › {name}"), grid) {
            out.push(t);
        }
    }
    Ok(out)
}

fn read_any(name: &str, bytes: Vec<u8>, depth: usize) -> AppResult<Vec<Table>> {
    let lower = name.to_lowercase();
    if lower.ends_with(".csv") || lower.ends_with(".txt") || lower.ends_with(".tsv") {
        Ok(read_csv(name, &bytes)?.into_iter().collect())
    } else if lower.ends_with(".xlsx") || lower.ends_with(".xlsm") || lower.ends_with(".xls") || lower.ends_with(".ods") {
        read_xlsx(name, bytes)
    } else if lower.ends_with(".zip") && depth == 0 {
        let mut z = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| AppError::validation(format!("{name}: not a readable ZIP file ({e}).")))?;
        let mut out = vec![];
        let mut total = 0usize;
        for i in 0..z.len() {
            let mut f = z.by_index(i).map_err(|e| AppError::validation(format!("{name}: {e}")))?;
            if f.is_dir() || f.name().starts_with("__MACOSX") {
                continue;
            }
            let inner = format!("{name} › {}", f.name());
            // Guard against ZIP bombs: cap the expanded size.
            let mut buf = vec![];
            f.by_ref().take((MAX_FILE_BYTES + 1) as u64).read_to_end(&mut buf)?;
            total += buf.len();
            if buf.len() > MAX_FILE_BYTES || total > MAX_FILE_BYTES * 3 {
                return Err(AppError::validation(format!("{name}: the archive expands to more than the allowed size.")));
            }
            out.extend(read_any(&inner, buf, depth + 1)?);
        }
        Ok(out)
    } else {
        Ok(vec![]) // images, PDFs and other files in a folder are ignored
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RowCheck {
    pub row: usize,
    /// create | update | skip | error
    pub action: String,
    pub label: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TableRequest {
    pub table: Table,
    #[serde(default)]
    pub update_existing: bool,
    #[serde(default)]
    pub skip_errors: bool,
    #[serde(default)]
    pub operation_id: Option<String>,
}

fn table_csv(t: &Table) -> AppResult<String> {
    let mut w = csv::Writer::from_writer(vec![]);
    w.write_record(&t.headers).map_err(|e| AppError::internal(e.to_string()))?;
    for r in &t.rows {
        w.write_record(r).map_err(|e| AppError::internal(e.to_string()))?;
    }
    String::from_utf8(w.into_inner().map_err(|e| AppError::internal(e.to_string()))?).map_err(|e| AppError::internal(e.to_string()))
}

fn get<'a>(t: &Table, row: &'a [String], field: &str) -> Option<&'a str> {
    let h = t.mapping.get(field)?;
    let i = t.headers.iter().position(|x| x == h)?;
    row.get(i).map(|s| s.trim()).filter(|s| !s.is_empty())
}

fn opt(v: Option<&str>) -> Option<String> {
    v.map(|s| s.to_string())
}

fn code_warning(t: &Table, fields: &[&str]) -> Option<String> {
    let cols: Vec<&String> = fields.iter().filter_map(|f| t.mapping.get(*f)).filter(|h| t.numeric_columns.contains(h)).collect();
    if cols.is_empty() {
        None
    } else {
        Some(format!(
            "Column {} was stored as numbers in the spreadsheet. Leading zeros may already be lost; check barcodes before applying.",
            cols.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", ")
        ))
    }
}

impl AppCore {
    /// Step 1-2: read the uploads and return detected tables with suggested mappings.
    pub fn migration_read(&self, token: &str, files: Vec<UploadFile>) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("import.run")?;
        if files.is_empty() {
            return Err(AppError::validation("Choose at least one file."));
        }
        let mut tables = vec![];
        let mut ignored = vec![];
        for f in files {
            let bytes =
                crate::ids::b64_decode(&f.data).ok_or_else(|| AppError::validation(format!("{}: the file could not be read.", f.name)))?;
            if bytes.len() > MAX_FILE_BYTES {
                return Err(AppError::validation(format!("{} is larger than 60 MB.", f.name)));
            }
            let t = read_any(&f.name, bytes, 0)?;
            if t.is_empty() {
                ignored.push(f.name);
            }
            tables.extend(t);
        }
        let rows: usize = tables.iter().map(|t| t.rows.len()).sum();
        if rows > MAX_ROWS {
            return Err(AppError::validation(format!(
                "The upload has {rows} rows; the limit per migration is {MAX_ROWS}. Split it into smaller files."
            )));
        }
        let field_lists: Value = ENTITIES
            .iter()
            .map(|e| (e.to_string(), json!(fields(e).iter().map(|(k, l, _)| json!({ "key": k, "label": l })).collect::<Vec<_>>())))
            .collect::<serde_json::Map<_, _>>()
            .into();
        Ok(json!({ "tables": tables, "ignored": ignored, "fields": field_lists }))
    }

    /// Step 3-5: validate one table and preview what would happen.
    pub fn migration_preview(&self, token: &str, req: TableRequest) -> AppResult<Value> {
        self.migration_run(token, req, false)
    }

    /// Step 6: apply one table.
    pub fn migration_apply(&self, token: &str, req: TableRequest) -> AppResult<Value> {
        self.migration_run(token, req, true)
    }

    fn migration_run(&self, token: &str, req: TableRequest, apply: bool) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("import.run")?;
        let t = &req.table;
        match t.entity.as_str() {
            "products" => {
                let r = crate::importer::ImportRequest {
                    csv: table_csv(t)?,
                    mapping: Some(t.mapping.clone()),
                    update_existing: req.update_existing,
                    skip_errors: req.skip_errors,
                    operation_id: req.operation_id.clone(),
                };
                if apply {
                    Ok(json!({ "entity": "products", "result": self.products_import_apply(token, r)? }))
                } else {
                    let mut p = serde_json::to_value(self.products_import_preview(token, r)?)?;
                    if let Some(w) = code_warning(t, &["barcodes", "sku"]) {
                        p["spreadsheet_warning"] = json!(w);
                    }
                    Ok(json!({ "entity": "products", "preview": p }))
                }
            }
            "customers" | "suppliers" | "stock" => self.migration_simple(token, &req, apply),
            _ => Err(AppError::validation("Choose what this table contains (products, customers, suppliers or stock).")),
        }
    }

    fn migration_simple(&self, token: &str, req: &TableRequest, apply: bool) -> AppResult<Value> {
        let t = &req.table;
        let entity = t.entity.as_str();
        let s = self.session(token)?;
        match entity {
            "customers" => s.require("customers.manage")?,
            "suppliers" => s.require("suppliers.manage")?,
            _ => s.require("inventory.adjust")?,
        }
        if apply && entity != "stock" {
            self.require_back_office_writable()?;
        }
        // Existing records for duplicate checks.
        let (phones, supplier_names): (HashMap<String, String>, HashMap<String, String>) = self.db.read(|c| {
            let mut ph = HashMap::new();
            let mut st = c.prepare("SELECT customer_id, phone FROM customers WHERE phone IS NOT NULL")?;
            for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
                let (id, p) = r?;
                ph.insert(p, id);
            }
            let mut sn = HashMap::new();
            let mut st = c.prepare("SELECT supplier_id, name FROM suppliers")?;
            for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
                let (id, n) = r?;
                sn.insert(n.to_lowercase(), id);
            }
            Ok((ph, sn))
        })?;
        let mut checks = vec![];
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut plan: Vec<(usize, Value)> = vec![];
        for (i, row) in t.rows.iter().enumerate() {
            let n = i + 2; // header is row 1
            let mut errors = vec![];
            let mut warnings = vec![];
            let mut action = "create".to_string();
            let label;
            match entity {
                "customers" => {
                    let name = get(t, row, "name");
                    label = name.unwrap_or("").to_string();
                    if name.is_none() {
                        errors.push("Name is empty.".to_string());
                    }
                    let phone = match get(t, row, "phone").map(crate::customers::normalize_phone) {
                        Some(Ok(p)) => p,
                        Some(Err(_)) => {
                            warnings.push("Phone number is not valid and will be left empty.".into());
                            None
                        }
                        None => None,
                    };
                    if let Some(p) = &phone {
                        if let Some(prev) = seen.insert(format!("p:{p}"), n) {
                            errors.push(format!("Same phone as row {prev}."));
                        }
                        if let Some(id) = phones.get(p) {
                            if req.update_existing {
                                action = "update".into();
                                plan.push((n, json!({ "id": id })));
                            } else {
                                action = "skip".into();
                                warnings.push(format!("A customer with phone {p} already exists."));
                            }
                        }
                    }
                    if errors.is_empty() && action != "skip" {
                        let input = json!({ "name": name, "phone": phone, "whatsapp": opt(get(t, row, "whatsapp")), "email": opt(get(t, row, "email")),
                                            "area": opt(get(t, row, "area")), "address": opt(get(t, row, "address")) });
                        if action == "update" {
                            plan.last_mut().unwrap().1["input"] = input;
                        } else {
                            plan.push((n, json!({ "input": input })));
                        }
                    }
                }
                "suppliers" => {
                    let name = get(t, row, "name");
                    label = name.unwrap_or("").to_string();
                    match name {
                        None => errors.push("Name is empty.".to_string()),
                        Some(nm) => {
                            let key = nm.to_lowercase();
                            if let Some(prev) = seen.insert(key.clone(), n) {
                                errors.push(format!("Same supplier name as row {prev}."));
                            }
                            let existing = supplier_names.get(&key).cloned();
                            if existing.is_some() && !req.update_existing {
                                action = "skip".into();
                                warnings.push(format!("Supplier {nm} already exists."));
                            } else if errors.is_empty() {
                                if existing.is_some() {
                                    action = "update".into();
                                }
                                let input = json!({
                                    "name": nm, "cr_number": opt(get(t, row, "cr_number")), "vat_number": opt(get(t, row, "vat_number")),
                                    "contact_name": opt(get(t, row, "contact_name")), "phone": opt(get(t, row, "phone")), "email": opt(get(t, row, "email")),
                                    "address": opt(get(t, row, "address")), "payment_terms": opt(get(t, row, "payment_terms"))
                                });
                                plan.push((n, json!({ "id": existing, "input": input })));
                            }
                        }
                    }
                }
                _ => {
                    let code = get(t, row, "barcode").map(|b| ("barcode", b)).or_else(|| get(t, row, "sku").map(|s| ("sku", s)));
                    label = code.map(|c| c.1.to_string()).unwrap_or_default();
                    let qty = get(t, row, "qty").map(|q| crate::money::parse_decimal(&q.replace(',', ""), 3));
                    let product: Option<(String, String, i64, bool, bool)> = match code {
                        None => {
                            errors.push("Barcode or SKU is empty.".into());
                            None
                        }
                        Some((kind, v)) => self.db.read(|c| {
                            let sql = if kind == "barcode" {
                                "SELECT p.product_id, p.name, COALESCE((SELECT SUM(qty_milli) FROM stock_levels s WHERE s.product_id=p.product_id),0), p.track_inventory, p.allow_decimal_quantity
                                 FROM product_barcodes b JOIN products p ON p.product_id=b.product_id WHERE b.barcode=?1"
                            } else {
                                "SELECT p.product_id, p.name, COALESCE((SELECT SUM(qty_milli) FROM stock_levels s WHERE s.product_id=p.product_id),0), p.track_inventory, p.allow_decimal_quantity
                                 FROM products p WHERE p.sku=?1 COLLATE NOCASE"
                            };
                            use rusqlite::OptionalExtension;
                            Ok(c.query_row(sql, [v], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0, r.get::<_, i64>(4)? != 0))).optional()?)
                        })?,
                    };
                    match (&product, &qty) {
                        (None, _) if code.is_some() => errors.push(format!("No product with {} {}.", code.unwrap().0, code.unwrap().1)),
                        (_, None) => errors.push("Quantity is empty.".into()),
                        (_, Some(Err(_))) => errors.push("Quantity is not a number.".into()),
                        (Some((pid, name, cur, track, dec)), Some(Ok(q))) => {
                            if !*track {
                                errors.push(format!("{name} does not track stock."));
                            } else if *q < 0 {
                                errors.push("Quantity cannot be negative.".into());
                            } else if !*dec && q % 1000 != 0 {
                                errors.push(format!("{name} is sold in whole units."));
                            } else if let Some(prev) = seen.insert(pid.clone(), n) {
                                errors.push(format!("Same product as row {prev}."));
                            } else if cur == q {
                                action = "skip".into();
                                warnings.push("Stock already matches.".into());
                            } else {
                                action = "update".into();
                                warnings.push(format!("{name}: {} → {}", crate::money::format_qty(*cur), crate::money::format_qty(*q)));
                                plan.push((n, json!({ "product_id": pid, "qty_milli": q })));
                            }
                        }
                        _ => {}
                    }
                }
            }
            if !errors.is_empty() {
                action = "error".into();
            }
            checks.push(RowCheck { row: n, action, label, errors, warnings });
        }
        let count = |a: &str| checks.iter().filter(|c| c.action == a).count();
        let summary = json!({ "total_rows": checks.len(), "creates": count("create"), "updates": count("update"), "skipped": count("skip"), "errors": count("error"),
                              "warnings": checks.iter().filter(|c| !c.warnings.is_empty()).count() });
        if !apply {
            let mut rows: Vec<&RowCheck> = checks.iter().filter(|c| c.action == "error" || !c.warnings.is_empty()).take(500).collect();
            if rows.len() < 50 {
                rows.extend(checks.iter().filter(|c| c.action != "error" && c.warnings.is_empty()).take(50 - rows.len()));
            }
            let warning = if entity == "stock" { code_warning(t, &["barcode", "sku"]) } else { code_warning(t, &["phone"]) };
            return Ok(json!({ "entity": entity, "preview": { "summary": summary, "rows": rows, "spreadsheet_warning": warning } }));
        }
        if count("error") > 0 && !req.skip_errors {
            return Err(AppError::validation("Fix or skip the rows with errors before applying."));
        }
        // Deterministic per-row operation ids make a repeated apply safe for stock.
        let op = req.operation_id.clone().unwrap_or_else(crate::ids::new_id);
        let mut done = 0usize;
        let mut failed = vec![];
        for (n, p) in plan {
            let r: AppResult<()> = match entity {
                "customers" => {
                    let input: crate::customers::CustomerInput =
                        serde_json::from_value(p["input"].clone()).map_err(|e| AppError::validation(e.to_string()))?;
                    self.customer_save(token, p["id"].as_str().map(|x| x.to_string()), input).map(|_| ())
                }
                "suppliers" => {
                    let input: crate::purchasing::SupplierInput =
                        serde_json::from_value(p["input"].clone()).map_err(|e| AppError::validation(e.to_string()))?;
                    self.supplier_save(token, p["id"].as_str().map(|x| x.to_string()), input).map(|_| ())
                }
                _ => {
                    let rop = hex::encode(&Sha256::digest(format!("{op}:{n}"))[..13]).to_uppercase();
                    self.inventory_adjust(
                        token,
                        crate::inventory::AdjustRequest {
                            product_id: p["product_id"].as_str().unwrap_or_default().into(),
                            mode: "set".into(),
                            qty_milli: p["qty_milli"].as_i64().unwrap_or(0),
                            reason: "Opening stock from migration".into(),
                            operation_id: rop,
                        },
                    )
                    .map(|_| ())
                }
            };
            match r {
                Ok(()) => done += 1,
                Err(e) => failed.push(json!({ "row": n, "error": e.message })),
            }
        }
        Ok(json!({ "entity": entity, "result": { "applied": done, "failed": failed, "summary": summary } }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn detects_entities() {
        assert_eq!(detect_entity("items.csv", &h(&["Barcode", "Product Name", "Price", "Cost"])), "products");
        assert_eq!(detect_entity("clients.csv", &h(&["Name", "Mobile", "Area"])), "customers");
        assert_eq!(detect_entity("vendors.csv", &h(&["Supplier Name", "CR No", "VAT Number", "Phone"])), "suppliers");
        assert_eq!(detect_entity("count.csv", &h(&["Barcode", "Qty On Hand"])), "stock");
        assert_eq!(detect_entity("x.csv", &h(&["foo", "bar"])), "unknown");
    }

    #[test]
    fn csv_keeps_leading_zeros_and_semicolons() {
        let t = read_csv("a.csv", "Barcode;Name;Price\n000123;Tea;1.250\n".as_bytes()).unwrap().unwrap();
        assert_eq!(t.rows[0][0], "000123");
        assert_eq!(t.entity, "products");
        assert!(t.numeric_columns.is_empty());
    }

    #[test]
    fn zip_of_csvs() {
        let mut buf = Cursor::new(vec![]);
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("customers.csv", o).unwrap();
            std::io::Write::write_all(&mut z, b"Name,Phone\nAli,33334444\n").unwrap();
            z.start_file("notes.pdf", o).unwrap();
            std::io::Write::write_all(&mut z, b"%PDF").unwrap();
            z.finish().unwrap();
        }
        let t = read_any("export.zip", buf.into_inner(), 0).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].entity, "customers");
        assert_eq!(t[0].source, "export.zip › customers.csv");
    }
}
