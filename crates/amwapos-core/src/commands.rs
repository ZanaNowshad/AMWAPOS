//! Transport-neutral command dispatch.
//!
//! Every UI operation is a named command with typed JSON arguments. The same
//! dispatcher serves the Tauri IPC bridge and the development HTTP bridge, so
//! authentication, authorization and validation are identical everywhere.
//! There is no command that executes SQL or touches arbitrary files.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::error::{AppError, AppResult, ErrorCode};
use crate::service::AppCore;

fn req<T: DeserializeOwned>(args: &Value, key: &str) -> AppResult<T> {
    let v = args.get(key).cloned().unwrap_or(Value::Null);
    if v.is_null() {
        return Err(AppError::validation(format!("Missing argument '{key}'.")));
    }
    serde_json::from_value(v).map_err(|e| AppError::validation(format!("Invalid argument '{key}': {e}")))
}

fn opt<T: DeserializeOwned>(args: &Value, key: &str) -> AppResult<Option<T>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone()).map(Some).map_err(|e| AppError::validation(format!("Invalid argument '{key}': {e}"))),
    }
}

fn all<T: DeserializeOwned>(args: &Value) -> AppResult<T> {
    serde_json::from_value(args.clone()).map_err(|e| AppError::validation(format!("Invalid request: {e}")))
}

fn out<T: Serialize>(r: AppResult<T>) -> AppResult<Value> {
    r.and_then(|v| serde_json::to_value(v).map_err(|e| AppError::internal(e.to_string())))
}

/// Commands that do not require a session.
pub const PUBLIC: &[&str] =
    &["app.ping", "setup.status", "setup.initialize", "auth.users", "auth.login", "auth.unlock", "auth.session", "auth.logout"];

pub fn dispatch(core: &AppCore, cmd: &str, token: Option<&str>, args: Value) -> AppResult<Value> {
    let started = std::time::Instant::now();
    let tk = || -> AppResult<&str> { token.ok_or_else(|| AppError::new(ErrorCode::Unauthenticated, "Please log in.")) };
    let result = match cmd {
        "app.ping" => Ok(serde_json::json!({ "ok": true, "version": crate::audit::APP_VERSION })),
        // setup & auth
        "setup.status" => out(core.setup_status()),
        "setup.initialize" => out(core.setup_initialize(all(&args)?)),
        "auth.users" => out(core.login_users()),
        "auth.login" => out(core.login(&req::<String>(&args, "user_id")?, &req::<String>(&args, "pin")?)),
        "auth.logout" => out(core.logout(tk()?)),
        "auth.lock" => out(core.lock(tk()?)),
        "auth.unlock" => out(core.unlock(tk()?, &req::<String>(&args, "pin")?)),
        "auth.session" => out(core.session_info(tk()?)),
        "auth.touch" => out(core.session(tk()?)),
        "auth.approve" => out(core.approve(
            tk()?,
            &req::<String>(&args, "approver_user_id")?,
            &req::<String>(&args, "pin")?,
            &req::<String>(&args, "permission")?,
            &opt::<String>(&args, "summary")?.unwrap_or_default(),
        )),
        "auth.approvers" => out(core.approvers(tk()?, &req::<String>(&args, "permission")?)),
        "auth.change_pin" => out(core.user_change_own_pin(tk()?, &req::<String>(&args, "current_pin")?, &req::<String>(&args, "new_pin")?)),
        // catalogue
        "products.search" => out(core.products_search(tk()?, all(&args)?)),
        "products.get" => out(core.product_get(tk()?, &req::<String>(&args, "product_id")?)),
        "products.create" => out(core.product_create(tk()?, all(&args)?)),
        "products.update" => out(core.product_update(tk()?, all(&args)?)),
        "products.set_active" => out(core.product_set_active(tk()?, &req::<String>(&args, "product_id")?, req(&args, "active")?)),
        "products.bulk_set_active" => out(core.product_bulk_archive(tk()?, req(&args, "product_ids")?, req(&args, "active")?)),
        "products.price_update" => out(core.product_price_update(
            tk()?,
            &req::<String>(&args, "product_id")?,
            req(&args, "amount_minor")?,
            opt(&args, "reason")?,
            opt(&args, "effective_from")?,
        )),
        "products.bulk_price" => {
            out(core.product_bulk_price(tk()?, req(&args, "changes")?, opt(&args, "reason")?, &req::<String>(&args, "operation_id")?))
        }
        "products.cost_update" => {
            out(core.product_cost_update(tk()?, &req::<String>(&args, "product_id")?, req(&args, "cost_minor")?, opt(&args, "reason")?))
        }
        "products.export_csv" => out(core.products_export_csv(tk()?, opt(&args, "include_archived")?.unwrap_or(false))),
        "products.import_preview" => out(core.products_import_preview(tk()?, all(&args)?)),
        "products.import_apply" => out(core.products_import_apply(tk()?, all(&args)?)),
        "barcodes.add" => out(core.barcode_add(
            tk()?,
            &req::<String>(&args, "product_id")?,
            &req::<String>(&args, "barcode")?,
            opt(&args, "make_primary")?.unwrap_or(false),
        )),
        "barcodes.remove" => out(core.barcode_remove(tk()?, &req::<String>(&args, "barcode_id")?)),
        "barcodes.set_primary" => out(core.barcode_set_primary(tk()?, &req::<String>(&args, "barcode_id")?)),
        "barcodes.unknown_list" => out(core.unknown_barcodes_list(tk()?, opt(&args, "status")?)),
        "barcodes.unknown_dismiss" => out(core.unknown_barcode_dismiss(tk()?, &req::<String>(&args, "barcode")?)),
        "categories.list" => out(core.categories_list(tk()?, opt(&args, "include_inactive")?.unwrap_or(false))),
        "categories.save" => out(core.category_save(
            tk()?,
            opt(&args, "category_id")?,
            &req::<String>(&args, "name")?,
            opt(&args, "parent_id")?,
            opt(&args, "sort_order")?.unwrap_or(0),
        )),
        "categories.archive" => out(core.category_archive(tk()?, &req::<String>(&args, "category_id")?, opt(&args, "reassign_to")?)),
        "tax.list" => out(core.tax_rules_list(tk()?)),
        "tax.create" => out(core.tax_rule_create(
            tk()?,
            &req::<String>(&args, "name")?,
            req(&args, "rate_bp")?,
            req(&args, "inclusive")?,
            opt(&args, "replace_rule_id")?,
        )),
        "tax.set_active" => out(core.tax_rule_set_active(tk()?, &req::<String>(&args, "tax_rule_id")?, req(&args, "active")?)),
        // POS
        "pos.config" => out(core.pos_config(tk()?)),
        "pos.cart" => out(core.pos_get_cart(tk()?)),
        "pos.scan" => out(core.pos_scan(tk()?, &req::<String>(&args, "barcode")?, opt(&args, "qty_milli")?)),
        "pos.search" => out(core.pos_search(
            tk()?,
            &opt::<String>(&args, "q")?.unwrap_or_default(),
            opt(&args, "category_id")?,
            opt(&args, "favorites")?.unwrap_or(false),
            opt(&args, "limit")?,
        )),
        "pos.add_product" => out(core.pos_add_product(tk()?, &req::<String>(&args, "product_id")?, opt(&args, "qty_milli")?)),
        "pos.add_custom" => out(core.pos_add_custom_item(tk()?, all(&args)?)),
        "pos.set_qty" => {
            out(core.pos_set_quantity(tk()?, &req::<String>(&args, "line_id")?, req(&args, "qty_milli")?, opt(&args, "approval_token")?))
        }
        "pos.remove_line" => out(core.pos_remove_line(tk()?, &req::<String>(&args, "line_id")?, opt(&args, "approval_token")?)),
        "pos.line_discount" => out(core.pos_line_discount(
            tk()?,
            &req::<String>(&args, "line_id")?,
            opt(&args, "discount_minor")?.unwrap_or(0),
            opt(&args, "discount_bp")?.unwrap_or(0),
            opt(&args, "approval_token")?,
        )),
        "pos.cart_discount" => out(core.pos_cart_discount(
            tk()?,
            opt(&args, "discount_minor")?.unwrap_or(0),
            opt(&args, "discount_bp")?.unwrap_or(0),
            opt(&args, "approval_token")?,
        )),
        "pos.price_override" => out(core.pos_price_override(
            tk()?,
            &req::<String>(&args, "line_id")?,
            req(&args, "unit_price_minor")?,
            opt(&args, "reason")?,
            opt(&args, "approval_token")?,
        )),
        "pos.set_customer" => out(core.pos_set_customer(tk()?, opt(&args, "customer_id")?)),
        "pos.hold" => out(core.pos_hold(tk()?, opt(&args, "note")?)),
        "pos.held" => out(core.pos_held_list(tk()?)),
        "pos.restore" => out(core.pos_restore(tk()?, &req::<String>(&args, "cart_id")?)),
        "pos.held_delete" => out(core.pos_held_delete(tk()?, &req::<String>(&args, "cart_id")?, opt(&args, "approval_token")?)),
        "pos.cancel" => out(core.pos_cancel_sale(tk()?, opt(&args, "approval_token")?)),
        "pos.finalize" => out(core.pos_finalize(tk()?, all(&args)?)),
        // sales / refunds / receipts
        "sales.list" => out(core.sales_list(tk()?, all(&args)?)),
        "sales.get" => out(core.sale_get(tk()?, &req::<String>(&args, "sale_id")?)),
        "sales.find_receipt" => out(core.sale_find_by_receipt(tk()?, &req::<String>(&args, "receipt_number")?)),
        "sales.reprint" => out(core.sale_reprint(tk()?, &req::<String>(&args, "sale_id")?)),
        "refunds.lookup" => out(core.refund_lookup(tk()?, &req::<String>(&args, "receipt_number")?)),
        "refunds.preview" => out(core.refund_preview(tk()?, all(&args)?)),
        "refunds.create" => out(core.refund_create(tk()?, all(&args)?)),
        "refunds.list" => out(core.refunds_list(tk()?, opt(&args, "from")?, opt(&args, "to")?, opt(&args, "limit")?)),
        "receipts.preview" => out(core.receipt_preview(tk()?, &req::<String>(&args, "kind")?, &req::<String>(&args, "ref_id")?)),
        "print.retry" => out(core.print_retry(tk()?, &req::<String>(&args, "job_id")?)),
        "print.queue" => out(core.print_queue(tk()?)),
        "print.test" => out(core.printer_test(tk()?)),
        "print.drawer_test" => out(core.printer_drawer_test(tk()?)),
        "print.printers" => out(core.printers_available(tk()?)),
        // shifts & cash
        "shift.current" => out(core.shift_current(tk()?)),
        "shift.open" => out(core.shift_open(tk()?, req(&args, "opening_float_minor")?, &req::<String>(&args, "operation_id")?)),
        "shift.get" => out(core.shift_get(tk()?, &req::<String>(&args, "shift_id")?)),
        "shift.close" => out(core
            .shift_close(tk()?, &req::<String>(&args, "shift_id")?, all(&args)?)
            .map(|(s, p)| serde_json::json!({ "summary": s, "print": p }))),
        "shift.list" => out(core.shifts_list(tk()?, opt(&args, "from")?, opt(&args, "to")?, opt(&args, "limit")?)),
        "cash.event" => out(core.cash_event(tk()?, all(&args)?)),
        "cash.list" => out(core.cash_events_list(tk()?, opt(&args, "shift_id")?, opt(&args, "from")?, opt(&args, "to")?)),
        // inventory
        "inventory.movements" => out(core.inventory_movements(tk()?, all(&args)?)),
        "inventory.adjust" => out(core.inventory_adjust(tk()?, all(&args)?)),
        "inventory.receive" => out(core.inventory_receive(tk()?, all(&args)?)),
        "stocktake.list" => out(core.stocktakes_list(tk()?)),
        "stocktake.create" => out(core.stocktake_create(tk()?, all(&args)?)),
        "stocktake.get" => out(core.stocktake_get(tk()?, &req::<String>(&args, "stocktake_id")?)),
        "stocktake.count" => out(core.stocktake_count(
            tk()?,
            &req::<String>(&args, "stocktake_id")?,
            opt(&args, "product_id")?,
            opt(&args, "barcode")?,
            req(&args, "qty_milli")?,
            &opt::<String>(&args, "mode")?.unwrap_or_else(|| "set".into()),
        )),
        "stocktake.set_status" => {
            out(core.stocktake_set_status(tk()?, &req::<String>(&args, "stocktake_id")?, &req::<String>(&args, "status")?))
        }
        "stocktake.finalize" => {
            out(core.stocktake_finalize(tk()?, &req::<String>(&args, "stocktake_id")?, &req::<String>(&args, "operation_id")?))
        }
        // purchasing
        "suppliers.list" => out(core.suppliers_list(tk()?, opt(&args, "q")?, opt(&args, "include_inactive")?.unwrap_or(false))),
        "suppliers.get" => out(core.supplier_get(tk()?, &req::<String>(&args, "supplier_id")?)),
        "suppliers.save" => out(core.supplier_save(tk()?, opt(&args, "supplier_id")?, req(&args, "supplier")?)),
        "po.list" => out(core.purchase_orders_list(tk()?, opt(&args, "status")?, opt(&args, "supplier_id")?)),
        "po.get" => out(core.purchase_order_get(tk()?, &req::<String>(&args, "po_id")?)),
        "po.save" => out(core.purchase_order_save(tk()?, opt(&args, "po_id")?, req(&args, "po")?)),
        "po.set_status" => out(core.purchase_order_set_status(tk()?, &req::<String>(&args, "po_id")?, &req::<String>(&args, "status")?)),
        "po.receive" => out(core.purchase_order_receive(tk()?, all(&args)?)),
        // customers & deliveries
        "customers.search" => {
            out(core.customers_search(tk()?, opt(&args, "q")?, opt(&args, "include_inactive")?.unwrap_or(false), opt(&args, "limit")?))
        }
        "customers.get" => out(core.customer_get(tk()?, &req::<String>(&args, "customer_id")?)),
        "customers.save" => out(core.customer_save(tk()?, opt(&args, "customer_id")?, req(&args, "customer")?)),
        "customers.add_note" => out(core.customer_add_note(tk()?, &req::<String>(&args, "customer_id")?, &req::<String>(&args, "note")?)),
        "deliveries.list" => out(core.deliveries_list(tk()?, opt(&args, "status")?, opt(&args, "include_closed")?.unwrap_or(false))),
        "deliveries.get" => out(core.delivery_get(tk()?, &req::<String>(&args, "delivery_id")?)),
        "deliveries.create" => out(core.delivery_create(tk()?, all(&args)?)),
        "deliveries.update" => out(core.delivery_update(
            tk()?,
            &req::<String>(&args, "delivery_id")?,
            opt(&args, "status")?,
            opt(&args, "assigned_user_id")?,
            opt(&args, "payment_status")?,
            opt(&args, "note")?,
        )),
        // reports
        "reports.catalog" => out(core.reports_catalog(tk()?)),
        "reports.run" => out(core.report_run(tk()?, &req::<String>(&args, "key")?, opt(&args, "params")?.unwrap_or_default())),
        "reports.csv" => out(core.report_csv(tk()?, &req::<String>(&args, "key")?, opt(&args, "params")?.unwrap_or_default())),
        "dashboard.get" => out(core.dashboard(tk()?)),
        // staff
        "users.list" => out(core.users_list(tk()?)),
        "users.create" => out(core.user_create(tk()?, req(&args, "user")?)),
        "users.update" => out(core.user_update(tk()?, &req::<String>(&args, "user_id")?, req(&args, "user")?)),
        "users.unlock" => out(core.user_unlock(tk()?, &req::<String>(&args, "user_id")?)),
        "roles.list" => out(core.roles_list(tk()?)),
        "roles.permissions" => out(core.permissions_catalog(tk()?)),
        "roles.save" => out(core.role_save(
            tk()?,
            opt(&args, "role_id")?,
            &req::<String>(&args, "name")?,
            opt(&args, "description")?,
            req(&args, "permissions")?,
        )),
        // system
        "settings.get" => out(core.settings_get(tk()?, &req::<String>(&args, "key")?)),
        "settings.save" => out(core.settings_save(tk()?, &req::<String>(&args, "key")?, req(&args, "value")?)),
        "business.get" => out(core.business_get(tk()?)),
        "business.update" => out(core.business_update(tk()?, all(&args)?)),
        "audit.list" => out(core.audit_list(tk()?, all(&args)?)),
        "audit.verify" => out(core.audit_verify(tk()?)),
        "devices.list" => out(core.devices_list(tk()?)),
        "devices.rename" => out(core.device_rename(tk()?, &req::<String>(&args, "device_id")?, &req::<String>(&args, "name")?)),
        "devices.set_active" => out(core.device_set_active(tk()?, &req::<String>(&args, "device_id")?, req(&args, "active")?)),
        "diagnostics.get" => out(core.diagnostics(tk()?, opt(&args, "full")?.unwrap_or(false))),
        "diagnostics.export" => out(core.diagnostics_export(tk()?)),
        "backup.list" => out(core.backups_list(tk()?)),
        "backup.health" => out(core.backup_health(tk()?)),
        "backup.create" => out(core.backup_create(tk()?, opt(&args, "directory")?)),
        "backup.inspect" => out(core.backup_inspect(tk()?, &req::<String>(&args, "path")?)),
        "backup.restore" => {
            out(core.backup_restore(tk()?, &req::<String>(&args, "path")?, opt(&args, "acknowledge_different_business")?.unwrap_or(false)))
        }
        // sync
        "sync.status" => out(core.sync_status(tk()?)),
        "sync.pairing_code" => out(core.sync_issue_pairing_code(tk()?, opt(&args, "device_name")?)),
        "sync.reset_hub_credentials" => out(core.sync_reset_hub_credentials(tk()?)),
        "sync.dead_letters" => out(core.sync_dead_letters(tk()?)),
        "sync.retry_dead_letter" => out(core.sync_retry_dead_letter(tk()?, &req::<String>(&args, "dead_id")?)),
        _ => Err(AppError::new(ErrorCode::NotFound, format!("Unknown command '{cmd}'."))),
    };
    let ms = started.elapsed().as_millis();
    match &result {
        Ok(_) => tracing::debug!(command = cmd, duration_ms = ms as u64, "command ok"),
        Err(e) => tracing::info!(command = cmd, duration_ms = ms as u64, code = ?e.code, "command failed"),
    }
    result
}
