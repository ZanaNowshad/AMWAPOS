//! Settings, audit trail, business profile and diagnostics.

use rusqlite::{params, params_from_iter, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit::{self, ChainReport};
use crate::catalog::Page;
use crate::error::{AppError, AppResult};
use crate::service::AppCore;
use crate::settings;
use crate::setup::{clean, clean_opt};
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Serialize)]
pub struct AuditRow {
    pub seq: i64,
    pub audit_id: String,
    pub created_at: String,
    pub user_name: Option<String>,
    pub approver_name: Option<String>,
    pub device_name: Option<String>,
    pub event_type: String,
    pub entity_type: String,
    pub entity_id: Option<String>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub previous_hash: String,
    pub audit_hash: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AuditQuery {
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub entity_type: Option<String>,
    #[serde(default)]
    pub entity_id: Option<String>,
    #[serde(default)]
    pub event_type: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusinessProfile {
    pub name: String,
    #[serde(default)]
    pub name_ar: Option<String>,
    #[serde(default)]
    pub cr_number: Option<String>,
    #[serde(default)]
    pub vat_number: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub currency_digits: i64,
    #[serde(default)]
    pub timezone: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticItem {
    pub component: String,
    /// ok | warning | error | info
    pub state: String,
    pub summary: String,
    pub details: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceRow {
    pub device_id: String,
    pub name: String,
    pub device_code: String,
    pub mode: String,
    pub branch_name: Option<String>,
    pub active: bool,
    pub activated_at: String,
    pub revoked_at: Option<String>,
    pub app_version: Option<String>,
    pub last_seen_at: Option<String>,
    pub pending_count: Option<i64>,
    pub last_error: Option<String>,
    pub is_this_device: bool,
}

impl AppCore {
    pub fn settings_get(&self, token: &str, key: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !settings::EDITABLE_KEYS.contains(&key) {
            return Err(AppError::validation("Unknown setting."));
        }
        // Operational settings are readable by any signed-in user (the POS needs them).
        let _ = s;
        self.db.read(|c| {
            let v = match key {
                settings::KEY_POS => serde_json::to_value(settings::get::<settings::PosSettings>(c, key)?)?,
                settings::KEY_SHIFT => serde_json::to_value(settings::get::<settings::ShiftSettings>(c, key)?)?,
                settings::KEY_PAYMENTS => serde_json::to_value(settings::payments(c)?)?,
                settings::KEY_RECEIPT => serde_json::to_value(settings::get::<settings::ReceiptSettings>(c, key)?)?,
                settings::KEY_SECURITY => serde_json::to_value(settings::get::<settings::SecuritySettings>(c, key)?)?,
                settings::KEY_INVENTORY => serde_json::to_value(settings::get::<settings::InventorySettings>(c, key)?)?,
                settings::KEY_PRINTER => serde_json::to_value(settings::get::<settings::PrinterSettings>(c, key)?)?,
                settings::KEY_BACKUP => serde_json::to_value(settings::get::<settings::BackupSettings>(c, key)?)?,
                settings::KEY_APPEARANCE => serde_json::to_value(settings::get::<settings::AppearanceSettings>(c, key)?)?,
                settings::KEY_FEATURES => serde_json::to_value(settings::get::<settings::FeatureFlags>(c, key)?)?,
                settings::KEY_WHATSAPP => serde_json::to_value(settings::get::<settings::WhatsAppSettings>(c, key)?)?,
                settings::KEY_LOYALTY => serde_json::to_value(settings::get::<settings::LoyaltySettings>(c, key)?)?,
                settings::KEY_UPDATES => serde_json::to_value(settings::get::<settings::UpdateSettings>(c, key)?)?,
                _ => Value::Null,
            };
            Ok(v)
        })
    }

    pub fn settings_save(&self, token: &str, key: &str, value: Value) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("settings.manage")?;
        if !key.starts_with("local.") {
            self.require_back_office_writable()?;
        }
        let v = settings::validate(key, value)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let before = settings::get_raw(tx, key)?;
            settings::put(tx, key, &v, Some(&s.user_id))?;
            audit::record(tx, &actor, "settings.changed", "settings", Some(key), before.as_ref(), Some(&v))?;
            Ok(())
        })?;
        Ok(v)
    }

    /// Record the outcome of an OS step-up (Windows Hello) check.
    pub fn audit_step_up(&self, token: &str, command: &str, outcome: &str, detail: &str) -> AppResult<()> {
        let s = self.session(token)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            audit::record(
                tx,
                &actor,
                "security.step_up",
                "command",
                Some(command),
                None,
                Some(&json!({ "outcome": outcome, "detail": detail })),
            )?;
            Ok(())
        })
    }

    /// Permission and module check for an update action; returns the feed settings.
    pub fn updates_authorize(&self, token: &str, action: &str) -> AppResult<settings::UpdateSettings> {
        let s = self.session(token)?;
        s.require("settings.manage")?;
        if action != "status" {
            self.require_feature("updates")?;
        }
        if action == "install" {
            s.require("backup.manage")?;
            let actor = self.actor(&s, None);
            self.db.write(|tx| {
                audit::record(tx, &actor, "update.install_started", "system", None, None, None)?;
                Ok(())
            })?;
        }
        self.db.read(|c| settings::get(c, settings::KEY_UPDATES))
    }

    /// Public, non-sensitive settings the POS needs before full login.
    pub fn pos_config(&self, token: &str) -> AppResult<Value> {
        self.session(token)?;
        self.db.read(|c| {
            let pos: settings::PosSettings = settings::get(c, settings::KEY_POS)?;
            let pay: settings::PaymentSettings = settings::payments(c)?;
            let shift: settings::ShiftSettings = settings::get(c, settings::KEY_SHIFT)?;
            let appearance: settings::AppearanceSettings = settings::get(c, settings::KEY_APPEARANCE)?;
            let printer: settings::PrinterSettings = settings::get(c, settings::KEY_PRINTER)?;
            let features: settings::FeatureFlags = settings::get(c, settings::KEY_FEATURES)?;
            let (currency, digits) = self.currency(c)?;
            let (name, tz): (String, String) =
                c.query_row("SELECT name, timezone FROM business LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(json!({
                "pos": pos, "payments": pay.tenders.into_iter().filter(|t| t.enabled).collect::<Vec<_>>(),
                "shift": { "blind_close": shift.blind_close }, "appearance": appearance,
                "printer_configured": printer.mode != "none",
                "currency": currency, "currency_digits": digits, "business_name": name, "timezone": tz,
                "device": self.device(), "features": features,
            }))
        })
    }

    pub fn business_get(&self, token: &str) -> AppResult<BusinessProfile> {
        self.session(token)?;
        self.db.read(|c| {
            Ok(c.query_row(
                "SELECT name, name_ar, cr_number, vat_number, phone, address, currency, currency_digits, timezone FROM business LIMIT 1",
                [],
                |r| {
                    Ok(BusinessProfile {
                        name: r.get(0)?,
                        name_ar: r.get(1)?,
                        cr_number: r.get(2)?,
                        vat_number: r.get(3)?,
                        phone: r.get(4)?,
                        address: r.get(5)?,
                        currency: r.get(6)?,
                        currency_digits: r.get(7)?,
                        timezone: r.get(8)?,
                    })
                },
            )?)
        })
    }

    /// Update business identity. Currency cannot change once sales exist.
    pub fn business_update(&self, token: &str, p: BusinessProfile) -> AppResult<BusinessProfile> {
        let s = self.session(token)?;
        s.require("settings.manage")?;
        self.require_back_office_writable()?;
        let actor = self.actor(&s, None);
        let name = clean(&p.name, "Business name", 120, true)?;
        time::tz(&p.timezone)?;
        self.db.write(|tx| {
            let (cur, digits): (String, i64) = tx.query_row("SELECT currency, currency_digits FROM business LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let sales: i64 = tx.query_row("SELECT COUNT(*) FROM sales", [], |r| r.get(0))?;
            if (p.currency != cur || p.currency_digits != digits) && sales > 0 {
                return Err(AppError::conflict("The currency cannot be changed after sales have been recorded."));
            }
            let before = serde_json::to_value(self.business_get(token)?)?;
            tx.execute(
                "UPDATE business SET name=?1, name_ar=?2, cr_number=?3, vat_number=?4, phone=?5, address=?6, currency=?7, currency_digits=?8, timezone=?9, updated_at=?10",
                params![
                    name,
                    clean_opt(&p.name_ar, "Arabic name", 120)?,
                    clean_opt(&p.cr_number, "CR number", 40)?,
                    clean_opt(&p.vat_number, "VAT number", 40)?,
                    clean_opt(&p.phone, "Phone", 40)?,
                    clean_opt(&p.address, "Address", 300)?,
                    p.currency,
                    p.currency_digits,
                    p.timezone,
                    time::now_str()
                ],
            )?;
            tx.execute(
                "UPDATE branches SET cr_number=?1, vat_number=?2, phone=COALESCE(?3, phone), address=COALESCE(?4, address), updated_at=?5",
                params![clean_opt(&p.cr_number, "CR", 40)?, clean_opt(&p.vat_number, "VAT", 40)?, clean_opt(&p.phone, "Phone", 40)?, clean_opt(&p.address, "Address", 300)?, time::now_str()],
            )?;
            audit::record(tx, &actor, "business.updated", "business", None, Some(&before), Some(&serde_json::to_value(&p)?))?;
            Ok(())
        })?;
        self.business_get(token)
    }

    pub fn audit_list(&self, token: &str, q: AuditQuery) -> AppResult<Page<AuditRow>> {
        let s = self.session(token)?;
        s.require("audit.view")?;
        let limit = validate::limit(q.limit, 100, 1000);
        let offset = validate::offset(q.offset);
        self.db.read(|c| {
            let tz = self.store_timezone(c)?;
            let mut w = vec!["1=1".to_string()];
            let mut args: Vec<rusqlite::types::Value> = vec![];
            for (v, col) in
                [(&q.user_id, "a.user_id"), (&q.entity_type, "a.entity_type"), (&q.entity_id, "a.entity_id"), (&q.device_id, "a.device_id")]
            {
                if let Some(v) = v.as_ref().filter(|x| !x.is_empty()) {
                    args.push(v.clone().into());
                    w.push(format!("{col}=?{}", args.len()));
                }
            }
            if let Some(ev) = q.event_type.as_ref().filter(|x| !x.is_empty()) {
                args.push(format!("{}%", ev.replace('%', "")).into());
                w.push(format!("a.event_type LIKE ?{}", args.len()));
            }
            if q.from.is_some() || q.to.is_some() {
                let (a, b) =
                    time::local_date_range_utc(q.from.as_deref().unwrap_or("2000-01-01"), q.to.as_deref().unwrap_or("2999-12-31"), &tz)?;
                args.push(a.into());
                w.push(format!("a.created_at>=?{}", args.len()));
                args.push(b.into());
                w.push(format!("a.created_at<?{}", args.len()));
            }
            let ws = w.join(" AND ");
            let total: i64 =
                c.query_row(&format!("SELECT COUNT(*) FROM audit_logs a WHERE {ws}"), params_from_iter(args.iter()), |r| r.get(0))?;
            let mut st = c.prepare(&format!(
                "SELECT a.seq, a.audit_id, a.created_at, u.display_name, ap.display_name, d.name, a.event_type, a.entity_type, a.entity_id,
                        a.before_json, a.after_json, a.previous_hash, a.audit_hash
                 FROM audit_logs a LEFT JOIN users u ON u.user_id=a.user_id LEFT JOIN users ap ON ap.user_id=a.approved_by
                 LEFT JOIN devices d ON d.device_id=a.device_id WHERE {ws} ORDER BY a.seq DESC LIMIT {limit} OFFSET {offset}"
            ))?;
            let rows = st
                .query_map(params_from_iter(args.iter()), |r| {
                    let b: Option<String> = r.get(9)?;
                    let a: Option<String> = r.get(10)?;
                    Ok(AuditRow {
                        seq: r.get(0)?,
                        audit_id: r.get(1)?,
                        created_at: r.get(2)?,
                        user_name: r.get(3)?,
                        approver_name: r.get(4)?,
                        device_name: r.get(5)?,
                        event_type: r.get(6)?,
                        entity_type: r.get(7)?,
                        entity_id: r.get(8)?,
                        before: b.and_then(|s| serde_json::from_str(&s).ok()),
                        after: a.and_then(|s| serde_json::from_str(&s).ok()),
                        previous_hash: r.get(11)?,
                        audit_hash: r.get(12)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Page { rows, total, limit, offset })
        })
    }

    pub fn audit_verify(&self, token: &str) -> AppResult<ChainReport> {
        let s = self.session(token)?;
        s.require("audit.view")?;
        self.db.read(audit::verify_chain)
    }

    pub fn devices_list(&self, token: &str) -> AppResult<Vec<DeviceRow>> {
        let s = self.session(token)?;
        if !s.has("devices.manage") && !s.has("sync.manage") && !s.has("diagnostics.view") {
            return Err(AppError::forbidden("devices.manage"));
        }
        let me = self.device().map(|d| d.device_id).unwrap_or_default();
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT d.device_id, d.name, d.device_code, d.operating_mode, b.name, d.active, d.activated_at, d.revoked_at,
                        COALESCE(h.app_version, d.app_version), h.last_seen_at, h.pending_count, h.last_error
                 FROM devices d LEFT JOIN branches b ON b.branch_id=d.branch_id LEFT JOIN device_heartbeats h ON h.device_id=d.device_id
                 ORDER BY d.device_code",
            )?;
            let rows = st
                .query_map([], |r| {
                    let id: String = r.get(0)?;
                    Ok(DeviceRow {
                        is_this_device: id == me,
                        device_id: id,
                        name: r.get(1)?,
                        device_code: r.get(2)?,
                        mode: r.get(3)?,
                        branch_name: r.get(4)?,
                        active: r.get::<_, i64>(5)? == 1,
                        activated_at: r.get(6)?,
                        revoked_at: r.get(7)?,
                        app_version: r.get(8)?,
                        last_seen_at: r.get(9)?,
                        pending_count: r.get(10)?,
                        last_error: r.get(11)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn device_rename(&self, token: &str, device_id: &str, name: &str) -> AppResult<Vec<DeviceRow>> {
        let s = self.session(token)?;
        s.require("devices.manage")?;
        self.require_back_office_writable()?;
        let id = validate::id(device_id, "Device")?;
        let name = clean(name, "Terminal name", 60, true)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let n = tx.execute("UPDATE devices SET name=?2 WHERE device_id=?1", params![id, name])?;
            if n == 0 {
                return Err(AppError::not_found("Device"));
            }
            audit::record(tx, &actor, "device.renamed", "device", Some(&id), None, Some(&json!({ "name": name })))?;
            Ok(())
        })?;
        if let Some(mut d) = self.device() {
            if d.device_id == id {
                d.name = name.clone();
                self.db.write(|tx| settings::put(tx, settings::KEY_DEVICE, &d, Some(&s.user_id)))?;
                self.set_device(Some(d));
            }
        }
        self.devices_list(token)
    }

    /// Revoke a terminal: its hub credential stops working immediately.
    pub fn device_set_active(&self, token: &str, device_id: &str, active: bool) -> AppResult<Vec<DeviceRow>> {
        let s = self.session(token)?;
        s.require("devices.manage")?;
        self.require_back_office_writable()?;
        let id = validate::id(device_id, "Device")?;
        if self.device().map(|d| d.device_id == id).unwrap_or(false) && !active {
            return Err(AppError::conflict("You cannot deactivate the computer you are using."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let now = time::now_str();
            let n = tx.execute(
                "UPDATE devices SET active=?2, revoked_at=CASE WHEN ?2=1 THEN NULL ELSE ?3 END WHERE device_id=?1",
                params![id, active as i64, now],
            )?;
            if n == 0 {
                return Err(AppError::not_found("Device"));
            }
            audit::record(tx, &actor, if active { "device.reactivated" } else { "device.revoked" }, "device", Some(&id), None, None)?;
            Ok(())
        })?;
        self.devices_list(token)
    }

    /// Structured health report for the Diagnostics screen. Contains no secrets.
    pub fn diagnostics(&self, token: &str, full_integrity: bool) -> AppResult<Vec<DiagnosticItem>> {
        let s = self.session(token)?;
        s.require("diagnostics.view")?;
        let mut out = vec![];
        let dev = self.device();
        out.push(DiagnosticItem {
            component: "Application".into(),
            state: "ok".into(),
            summary: format!("AMWAPOS {}", audit::APP_VERSION),
            details: json!({
                "version": audit::APP_VERSION,
                "build_sha": option_env!("AMWAPOS_BUILD_SHA").unwrap_or("dev"),
                "started_at": time::fmt(self.started_at),
                "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
                "data_dir": self.data_dir.to_string_lossy(),
            }),
        });
        let integrity = if full_integrity {
            self.db.integrity_check()?
        } else {
            self.db.read(|c| Ok(vec![c.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))?]))?
        };
        let ok = integrity.len() == 1 && integrity[0] == "ok";
        let (mode, page_count, page_size, freelist): (String, i64, i64, i64) = self.db.read(|c| {
            Ok((
                c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?,
                c.query_row("PRAGMA page_count", [], |r| r.get(0))?,
                c.query_row("PRAGMA page_size", [], |r| r.get(0))?,
                c.query_row("PRAGMA freelist_count", [], |r| r.get(0))?,
            ))
        })?;
        let counts = self.db.read(|c| {
            let mut m = serde_json::Map::new();
            for t in
                ["products", "product_barcodes", "sales", "sale_items", "refunds", "stock_movements", "customers", "audit_logs", "shifts"]
            {
                let n: i64 = c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))?;
                m.insert(t.into(), n.into());
            }
            Ok(Value::Object(m))
        })?;
        out.push(DiagnosticItem {
            component: "Database".into(),
            state: if ok { "ok".into() } else { "error".into() },
            summary: if ok {
                format!("SQLite integrity: OK · Schema {}", self.db.schema_version()?)
            } else {
                "Integrity check reported problems".into()
            },
            details: json!({
                "path": self.db.path().to_string_lossy(), "journal_mode": mode, "schema_version": self.db.schema_version()?,
                "size_bytes": page_count * page_size, "free_pages": freelist, "integrity": integrity, "records": counts,
                "full_check": full_integrity,
            }),
        });
        let chain = self.db.read(audit::verify_chain)?;
        out.push(DiagnosticItem {
            component: "Audit trail".into(),
            state: if chain.valid { "ok".into() } else { "error".into() },
            summary: chain.message.clone(),
            details: serde_json::to_value(&chain)?,
        });
        out.push(DiagnosticItem {
            component: "Terminal".into(),
            state: if dev.is_some() { "ok".into() } else { "warning".into() },
            summary: dev.as_ref().map(|d| format!("{} ({}) · {}", d.name, d.device_code, d.mode)).unwrap_or_else(|| "Not set up".into()),
            details: serde_json::to_value(&dev)?,
        });
        let printer: settings::PrinterSettings = self.db.read(|c| settings::get(c, settings::KEY_PRINTER))?;
        let (failed, last_err): (i64, Option<String>) = self.db.read(|c| {
            Ok((
                c.query_row("SELECT COUNT(*) FROM print_jobs WHERE status='failed'", [], |r| r.get(0))?,
                c.query_row("SELECT last_error FROM print_jobs WHERE status='failed' ORDER BY updated_at DESC LIMIT 1", [], |r| r.get(0))
                    .optional()?
                    .flatten(),
            ))
        })?;
        out.push(DiagnosticItem {
            component: "Printer".into(),
            state: if printer.mode == "none" || failed > 0 { "warning".into() } else { "ok".into() },
            summary: if printer.mode == "none" { "No receipt printer configured".into() } else { format!("{} printer · {} failed job(s)", printer.mode, failed) },
            details: json!({ "mode": printer.mode, "target": printer.target, "paper_width_mm": printer.paper_width_mm, "failed_jobs": failed, "last_error": last_err }),
        });
        out.push(self.sync_diagnostic()?);
        out.push(self.backup_diagnostic()?);
        out.push(DiagnosticItem {
            component: "WhatsApp".into(),
            state: "info".into(),
            summary: "Optional module (status filled in by the running app)".into(),
            details: json!({ "note": "WhatsApp runs in-process under its own supervisor; checkout never depends on it." }),
        });
        out.push(DiagnosticItem {
            component: "OCR".into(),
            state: "info".into(),
            summary: "Optional module (status filled in by the running app)".into(),
            details: json!({}),
        });
        let free = crate::backup::free_space(&self.data_dir);
        out.push(DiagnosticItem {
            component: "Storage".into(),
            state: match free {
                Some(f) if f < 2 * 1024 * 1024 * 1024 => "warning".into(),
                Some(_) => "ok".into(),
                None => "info".into(),
            },
            summary: match free {
                Some(f) => format!("{:.1} GB free", f as f64 / 1_073_741_824.0),
                None => "Free space unknown".into(),
            },
            details: json!({ "free_bytes": free }),
        });
        Ok(out)
    }

    /// Diagnostics bundle (JSON). Secrets are never included: PIN hashes,
    /// credentials and API keys are not part of any diagnostic item.
    pub fn diagnostics_export(&self, token: &str) -> AppResult<Value> {
        let items = self.diagnostics(token, true)?;
        let recent_errors = self.db.read(|c| {
            let mut st =
                c.prepare("SELECT kind, last_error, updated_at FROM print_jobs WHERE status='failed' ORDER BY updated_at DESC LIMIT 20")?;
            let rows = st
                .query_map([], |r| {
                    Ok(json!({ "kind": r.get::<_, String>(0)?, "error": r.get::<_, Option<String>>(1)?, "at": r.get::<_, String>(2)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        let migrations = self.db.read(|c| {
            let mut st = c.prepare("SELECT version, name, applied_at FROM schema_migrations ORDER BY version")?;
            let rows = st
                .query_map([], |r| {
                    Ok(json!({ "version": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "applied_at": r.get::<_, String>(2)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        let out = json!({
            "generated_at": time::now_str(),
            "items": items,
            "migrations": migrations,
            "recent_print_errors": recent_errors,
            "redaction": "PIN hashes, device credentials, API keys, extra AI header values, QR and pairing codes, WhatsApp session data and customer details are excluded.",
        });
        Ok(redact_secrets(out, &self.ai_secret_values()))
    }
}

/// Replace every known secret value anywhere in an export (defence in depth:
/// secrets are never collected, but an error text could echo one).
pub fn redact_secrets(v: Value, secrets: &[String]) -> Value {
    let secrets: Vec<&String> = secrets.iter().filter(|s| s.len() >= 6).collect();
    if secrets.is_empty() {
        return v;
    }
    let mut text = v.to_string();
    for s in secrets {
        // Match the JSON-escaped form too.
        let escaped = serde_json::to_string(s).unwrap_or_default();
        let inner = escaped.trim_matches('"');
        text = text.replace(inner, "[redacted]");
    }
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

#[cfg(test)]
mod redaction_tests {
    #[test]
    fn keys_are_replaced() {
        let v = serde_json::json!({ "a": "error: bad key sk-live-abcdef123 here", "b": ["sk-live-abcdef123"] });
        let r = super::redact_secrets(v, &["sk-live-abcdef123".into()]);
        assert!(!r.to_string().contains("sk-live-abcdef123"));
        assert_eq!(r["b"][0], "[redacted]");
    }
}
