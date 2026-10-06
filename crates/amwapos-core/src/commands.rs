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
            opt::<String>(&args, "binding")?.as_deref(),
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
        "barcodes.set_kind" => out(core.barcode_set_kind(tk()?, &req::<String>(&args, "barcode_id")?, opt(&args, "kind")?)),
        "products.set_plu" => out(core.product_set_plu(tk()?, &req::<String>(&args, "product_id")?, opt(&args, "plu")?)),
        "scale_rules.list" => out(core.scale_rules_list(tk()?)),
        "scale_rules.save" => out(core.scale_rule_save(tk()?, req(&args, "rule")?)),
        "scale_rules.test" => out(core.scale_rule_test(tk()?, &req::<String>(&args, "code")?)),
        "barcodes.set_primary" => out(core.barcode_set_primary(tk()?, &req::<String>(&args, "barcode_id")?)),
        "barcodes.unknown_list" => out(core.unknown_barcodes_list(tk()?, opt(&args, "status")?)),
        "barcodes.unknown_merge" => {
            out(core.unknown_barcodes_merge(tk()?, &req::<String>(&args, "product_id")?, req::<Vec<String>>(&args, "barcodes")?))
        }
        "barcodes.unknown_reopen" => out(core.unknown_barcode_reopen(tk()?, &req::<String>(&args, "barcode")?)),
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
        "pos.loyalty_redeem" => out(core.pos_loyalty_redeem(tk()?, req(&args, "points")?)),
        "loyalty.customer" => out(core.loyalty_customer(tk()?, &req::<String>(&args, "customer_id")?)),
        "loyalty.adjust" => out(core.loyalty_adjust(
            tk()?,
            &req::<String>(&args, "customer_id")?,
            req(&args, "points")?,
            &req::<String>(&args, "note")?,
            opt::<String>(&args, "operation_id")?.as_deref(),
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
        "sales.void_check" => out(core.sale_void_check(tk()?, &req::<String>(&args, "sale_id")?)),
        "sales.void" => out(core.sale_void(tk()?, req(&args, "void")?)),
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
        "customers.account" => out(core.customer_account(tk()?, &req::<String>(&args, "customer_id")?)),
        "customers.account_set" => out(core.customer_account_set(
            tk()?,
            &req::<String>(&args, "customer_id")?,
            req(&args, "enabled")?,
            req(&args, "credit_limit_minor")?,
        )),
        "customers.account_payment" => out(core.customer_account_payment(tk()?, all(&args)?)),
        "customers.account_adjust" => out(core.customer_account_adjust(
            tk()?,
            &req::<String>(&args, "customer_id")?,
            req(&args, "amount_minor")?,
            &req::<String>(&args, "note")?,
            &req::<String>(&args, "operation_id")?,
        )),
        "customers.address_save" => out(core.customer_address_save(tk()?, all(&args)?)),
        "customers.address_delete" => out(core.customer_address_delete(tk()?, &req::<String>(&args, "address_id")?)),
        "deliveries.list" => out(core.deliveries_list(tk()?, opt(&args, "status")?, opt(&args, "include_closed")?.unwrap_or(false))),
        "deliveries.get" => out(core.delivery_get(tk()?, &req::<String>(&args, "delivery_id")?)),
        "deliveries.create" => out(core.delivery_create(tk()?, all(&args)?)),
        "deliveries.update" if opt::<bool>(&args, "revert")?.unwrap_or(false) => {
            out(core.delivery_revert(tk()?, &req::<String>(&args, "delivery_id")?, &req::<String>(&args, "status")?, opt(&args, "note")?))
        }
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
        "reports.eod" => out(core.eod_pack(tk()?, opt(&args, "date")?, opt(&args, "branch_id")?)),
        "reports.eod_zip" => out(core.eod_zip(tk()?, opt(&args, "date")?, opt(&args, "branch_id")?)),
        "reports.presets" => out(core.report_presets(tk()?)),
        "companion.issue" => out(core.companion_issue(tk()?, opt(&args, "label")?, opt(&args, "hours")?)),
        "companion.tokens" => out(core.companion_tokens(tk()?)),
        "companion.revoke" => out(core.companion_revoke(tk()?, &req::<String>(&args, "id")?)),
        "reports.preset_save" => out(core.report_preset_save(tk()?, req(&args, "preset")?)),
        "reports.preset_delete" => out(core.report_preset_delete(tk()?, &req::<String>(&args, "preset_id")?)),
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
        // AI assistant (the question/answer loop itself is "ai.ask" in the runtime)
        "ai.status" => out(core.ai_status(tk()?)),
        "ai.configure" => out(core.ai_configure_full(
            tk()?,
            req(&args, "settings")?,
            opt(&args, "api_key")?,
            opt(&args, "extra_header_value")?,
            opt(&args, "fallback_api_key")?,
        )),
        "ai.conversations" => out(core.ai_conversations(tk()?)),
        "ai.conversation" => out(core.ai_conversation(tk()?, &req::<String>(&args, "conversation_id")?)),
        "ai.proposals" => out(core.ai_proposals(tk()?, opt(&args, "status")?)),
        "ai.proposal_confirm" => Ok(core.ai_proposal_confirm_with(
            tk()?,
            &req::<String>(&args, "proposal_id")?,
            opt(&args, "approval_token")?,
            &args.get("inputs").cloned().unwrap_or(Value::Null),
        )?),
        "ai.digest" => Ok(core.ai_digest(tk()?, opt(&args, "date")?)?),
        "ai.attach_image" => Ok(core.ai_attach_image(tk()?, &req::<String>(&args, "media_type")?, &req::<String>(&args, "data")?)?),
        "ai.attachment" => Ok(core.ai_attachment(tk()?, &req::<String>(&args, "attachment_id")?)?),
        "ai.conversation_rename" => {
            Ok(core.ai_conversation_rename(tk()?, &req::<String>(&args, "conversation_id")?, &req::<String>(&args, "title")?)?)
        }
        "ai.pin" => Ok(core.ai_pin(
            tk()?,
            &req::<String>(&args, "conversation_id")?,
            &req::<String>(&args, "kind")?,
            &req::<String>(&args, "id")?,
        )?),
        "ai.unpin" => Ok(core.ai_unpin(
            tk()?,
            &req::<String>(&args, "conversation_id")?,
            &req::<String>(&args, "kind")?,
            &req::<String>(&args, "id")?,
        )?),
        "ai.briefings" => out(core.ai_briefings(tk()?)),
        "ai.briefing_save" => out(core.ai_briefing_save(tk()?, opt(&args, "briefing_id")?, req(&args, "briefing")?)),
        "ai.briefing_delete" => out(core.ai_briefing_delete(tk()?, &req::<String>(&args, "briefing_id")?)),
        "ai.notes" => out(core.ai_notes(tk()?, opt(&args, "limit")?)),
        "ai.alerts" => out(core.ai_alerts(tk()?, opt(&args, "include_dismissed")?.unwrap_or(false))),
        "ai.alert_dismiss" => out(core.ai_alert_dismiss(tk()?, &req::<String>(&args, "alert_id")?)),
        "ai.reorder_suggestions" => Ok(core.reorder_suggestions(tk()?, opt::<String>(&args, "supplier_id")?.as_deref())?),
        "ai.margin_price" => Ok(core.margin_price(tk()?, &req::<String>(&args, "product_id")?, opt(&args, "margin_bp")?)?),
        "ai.branch_compare" => Ok(core.branch_compare(tk()?, &req::<String>(&args, "product_id")?)?),
        "ai.note_read" => out(core.ai_note_read(tk()?, &req::<String>(&args, "note_id")?)),
        "ai.slash" => Ok(core.ai_slash(tk()?, &req::<String>(&args, "command")?, &opt::<String>(&args, "arg")?.unwrap_or_default())?),
        "whatsapp.triage" => Ok(core.wa_triage(tk()?, opt(&args, "limit")?)?),
        "whatsapp.triage_set" => Ok(core.wa_triage_set(tk()?, req(&args, "seq")?, &req::<String>(&args, "category")?)?),
        "ai.playbook" => Ok(core.ai_playbook(tk()?, &req::<String>(&args, "name")?)?),
        "ai.proposal_reject" => out(core.ai_proposal_reject(tk()?, &req::<String>(&args, "proposal_id")?)),
        "ai.proposal_undo" => out(core.ai_proposal_undo(tk()?, &req::<String>(&args, "proposal_id")?)),
        // migration from another system
        "migration.read" => out(core.migration_read(tk()?, req(&args, "files")?)),
        "migration.preview" => out(core.migration_preview(tk()?, all(&args)?)),
        "migration.apply" => out(core.migration_apply(tk()?, all(&args)?)),
        // receipts as PDF
        "receipts.pdf" => out(core.receipt_pdf(tk()?, &req::<String>(&args, "kind")?, &req::<String>(&args, "ref_id")?)),
        // WhatsApp records (link state and sending are handled by the WhatsApp service in the hub crate)
        "whatsapp.queue" => out(core.wa_queue(tk()?, all(&args)?)),
        "whatsapp.outbox" => out(core.wa_outbox_list(tk()?, opt(&args, "status")?, opt(&args, "limit")?)),
        "whatsapp.outbox_action" => {
            out(core.wa_outbox_action(tk()?, &req::<String>(&args, "message_id")?, &req::<String>(&args, "action")?))
        }
        "whatsapp.conversations" => out(core.wa_conversations(tk()?)),
        "whatsapp.thread" => out(core.wa_thread(tk()?, &req::<String>(&args, "chat")?)),
        "whatsapp.media" => out(core.wa_media(tk()?, req(&args, "seq")?)),
        "whatsapp.mark_read" => out(core.wa_mark_read(tk()?, &req::<String>(&args, "chat")?)),
        "whatsapp.summary" => out(core.wa_summary(tk()?)),
        "whatsapp.recent" => out(core.wa_recent(tk()?, opt(&args, "limit")?)),
        "whatsapp.import_contacts" => out(core.wa_import_contacts(tk()?, opt(&args, "chats")?)),
        "whatsapp.phone_contacts" => out(core.wa_contacts_preview(tk()?)),
        "whatsapp.link_customer" => out(core.wa_link_customer(tk()?, &req::<String>(&args, "chat")?, opt(&args, "customer_id")?)),
        "whatsapp.thread_context" => out(core.wa_thread_context(tk()?, &req::<String>(&args, "chat")?)),
        // the Send loop (tickets and drops)
        "tickets.list" => out(core.tickets_list(tk()?, all(&args)?)),
        "tickets.counts" => out(core.tickets_counts(tk()?)),
        "orders.flow" => out(core.orders_flow(tk()?)),
        "tickets.get" => out(core.ticket_get(tk()?, &req::<String>(&args, "ticket_id")?)),
        "tickets.record_payment" => out(core.ticket_record_payment(tk()?, all(&args)?)),
        "tickets.unable" => out(core.ticket_unable(tk()?, &req::<String>(&args, "delivery_id")?, &req::<String>(&args, "reason")?)),
        "tickets.not_delivered" => out(core.ticket_not_delivered(tk()?, all(&args)?)),
        "customers.block_area" => out(core.block_area(tk()?, &req::<String>(&args, "block")?)),
        "riders.cash" => out(core.rider_cash_list(tk()?)),
        // product images
        "products.images" => out(core.product_images_get(tk()?, req(&args, "hashes")?)),
        "products.image_state" => out(core.product_image_state(tk()?, &req::<String>(&args, "product_id")?)),
        "products.image_upload" => {
            out(core.product_image_upload(tk()?, &req::<String>(&args, "product_id")?, &req::<String>(&args, "data")?))
        }
        "products.image_remove" => out(core.product_image_remove(tk()?, &req::<String>(&args, "product_id")?)),
        "products.image_find" => out(core.product_image_find(tk()?, &req::<String>(&args, "product_id")?)),
        "products.image_backfill" => out(core.product_image_backfill(tk()?, opt(&args, "limit")?)),
        "products.image_overview" => out(core.product_image_overview(tk()?)),
        "products.image_configure" => out(core.product_image_configure(tk()?, req(&args, "settings")?, opt(&args, "google_key")?)),
        "riders.handover" => out(core.rider_handover(tk()?, all(&args)?)),
        "whatsapp.phone_contacts_import" => out(core.wa_contacts_import(tk()?, all(&args)?)),
        // payment screenshot reviews
        "payreviews.list" => out(core.pr_list(tk()?, opt(&args, "status")?)),
        "payreviews.get" => out(core.pr_get(tk()?, &req::<String>(&args, "review_id")?)),
        "payreviews.upload" => out(core.pr_upload(
            tk()?,
            &req::<String>(&args, "file_name")?,
            &req::<String>(&args, "data")?,
            opt(&args, "expected_minor")?,
            opt(&args, "delivery_id")?,
        )),
        "payreviews.set_expected" => {
            out(core.pr_set_expected(tk()?, &req::<String>(&args, "review_id")?, opt(&args, "expected_minor")?, opt(&args, "delivery_id")?))
        }
        "payreviews.decide" => out(core.pr_decide(tk()?, all(&args)?)),
        // invoice scans (OCR → review → draft purchase order)
        "invoicescan.import" => {
            out(core.inv_import(tk()?, &req::<String>(&args, "file_name")?, &req::<String>(&args, "data")?, opt(&args, "supplier_id")?))
        }
        "invoicescan.list" => out(core.inv_list(tk()?, opt(&args, "status")?)),
        "invoicescan.get" => out(core.inv_get(tk()?, &req::<String>(&args, "scan_id")?)),
        "invoicescan.update_line" => out(core.inv_update_line(tk()?, all(&args)?)),
        "invoicescan.confirm" => out(core.inv_confirm(
            tk()?,
            &req::<String>(&args, "scan_id")?,
            &req::<String>(&args, "supplier_id")?,
            opt(&args, "receive")?.unwrap_or(false),
        )),
        "invoicescan.reject" => out(core.inv_reject(tk()?, &req::<String>(&args, "scan_id")?, &req::<String>(&args, "reason")?)),
        // Document Intelligence (supplier documents → review → drafts)
        "docs.import" => {
            out(core.doc_import(tk()?, &req::<String>(&args, "file_name")?, &req::<String>(&args, "data")?, opt(&args, "supplier_id")?))
        }
        "docs.from_inbox" => out(core.doc_from_inbox(tk()?, req(&args, "seq")?)),
        "docs.get" => out(core.doc_get(tk()?, &req::<String>(&args, "scan_id")?)),
        "docs.page" => out(core.doc_page(tk()?, &req::<String>(&args, "scan_id")?, req(&args, "page")?)),
        "docs.update" => out(core.doc_update(tk()?, &req::<String>(&args, "scan_id")?, all(&args)?)),
        "docs.update_line" => out(core.doc_update_line(tk()?, &req::<String>(&args, "scan_id")?, all(&args)?)),
        "docs.new_product" => out(core.doc_new_product_draft(tk()?, &req::<String>(&args, "scan_id")?, req(&args, "line_no")?)),
        "docs.create_supplier_invoice" => {
            out(core.doc_create_supplier_invoice(tk()?, &req::<String>(&args, "scan_id")?, req(&args, "revision")?))
        }
        "docs.create_receiving" => out(core.doc_create_receiving(tk()?, &req::<String>(&args, "scan_id")?, req(&args, "revision")?)),
        "docs.metrics" => out(core.doc_metrics(tk()?)),
        "receiving.drafts" => out(core.receiving_drafts_list(tk()?, opt(&args, "status")?)),
        "receiving.draft_get" => out(core.receiving_draft_get(tk()?, &req::<String>(&args, "draft_id")?)),
        "receiving.draft_update_line" => out(core.receiving_draft_update_line(
            tk()?,
            &req::<String>(&args, "draft_id")?,
            req(&args, "line_no")?,
            opt(&args, "qty_milli")?,
            opt(&args, "unit_cost_minor")?,
            opt(&args, "remove")?.unwrap_or(false),
        )),
        "receiving.draft_cancel" => out(core.receiving_draft_cancel(tk()?, &req::<String>(&args, "draft_id")?)),
        "receiving.draft_post" => {
            out(core.receiving_draft_post(tk()?, &req::<String>(&args, "draft_id")?, &req::<String>(&args, "operation_id")?))
        }
        "supplier_invoices.list" => out(core.supplier_invoices_list(tk()?, opt(&args, "status")?)),
        "supplier_invoices.get" => out(core.supplier_invoice_get(tk()?, &req::<String>(&args, "invoice_id")?)),
        "supplier_invoices.set_status" => {
            out(core.supplier_invoice_set_status(tk()?, &req::<String>(&args, "invoice_id")?, &req::<String>(&args, "status")?))
        }
        // Customer statements and receivables
        "customers.statement" => {
            out(core.customer_statement(tk()?, &req::<String>(&args, "customer_id")?, opt(&args, "from")?, opt(&args, "to")?))
        }
        "customers.statement_pdf" => {
            out(core.customer_statement_pdf(tk()?, &req::<String>(&args, "customer_id")?, opt(&args, "from")?, opt(&args, "to")?))
        }
        "customers.receivables" => out(core.receivables(tk()?, opt(&args, "as_of")?)),
        "customers.terms_set" => out(core.customer_terms_set(tk()?, &req::<String>(&args, "customer_id")?, req(&args, "terms_days")?)),
        // Trading day: X, checks, Z, opening; registers and drawers; cases
        "day.x" => out(core.day_x(tk()?, opt(&args, "date")?, opt(&args, "branch_id")?)),
        "day.x_pdf" => out(core.day_x_pdf(tk()?, opt(&args, "date")?, opt(&args, "branch_id")?)),
        "day.checks" => out(core.day_checks(tk()?, opt(&args, "date")?, opt(&args, "branch_id")?)),
        "day.close" => out(core.day_close(tk()?, all(&args)?)),
        "day.closes" => out(core.day_closes_list(tk()?, opt(&args, "branch_id")?, opt(&args, "limit")?)),
        "day.close_get" => out(core.day_close_get(tk()?, &req::<String>(&args, "close_id")?)),
        "day.close_pdf" => out(core.day_close_pdf(tk()?, &req::<String>(&args, "close_id")?)),
        "day.opening" => out(core.day_opening(tk()?)),
        // Batches, expiry, waste, days of stock left
        "lots.product" => out(core.product_lots(tk()?, &req::<String>(&args, "product_id")?)),
        "lots.get" => out(core.lot_get(tk()?, &req::<String>(&args, "lot_id")?)),
        "lots.count_in" => out(core.lot_count_in(tk()?, all(&args)?)),
        "lots.correct" => out(core.lot_correct(tk()?, all(&args)?)),
        "lots.product_settings" => out(core.product_lot_settings(
            tk()?,
            &req::<String>(&args, "product_id")?,
            req(&args, "track_lots")?,
            opt(&args, "expiry_kind")?,
        )),
        "receiving.draft_set_lot" => out(core.receiving_draft_set_lot(tk()?, all(&args)?)),
        "expiry.overview" => out(core.expiry_overview(tk()?, all(&args)?)),
        "stock.cover" => out(core.stock_cover(tk()?, all(&args)?)),
        "waste.record" => out(core.waste_record(tk()?, all(&args)?)),
        "waste.reverse" => out(core.waste_reverse(
            tk()?,
            &req::<String>(&args, "waste_id")?,
            &req::<String>(&args, "reason")?,
            &req::<String>(&args, "operation_id")?,
        )),
        "waste.get" => out(core.waste_get(tk()?, &req::<String>(&args, "waste_id")?)),
        "waste.list" => out(core.waste_list(tk()?, opt(&args, "from")?, opt(&args, "to")?, opt(&args, "reason")?)),
        "waste.summary" => out(core.waste_summary(tk()?, opt(&args, "from")?, opt(&args, "to")?)),
        // Procurement (Wave 4): supplier catalogue, suggested orders,
        // requisitions, approvals, receiving differences, match, returns
        "supplier.catalogue" => out(core.supplier_catalogue(tk()?, opt(&args, "supplier_id")?, opt(&args, "product_id")?)),
        "supplier.terms_save" => out(core.supplier_terms_save(tk()?, all(&args)?)),
        "products.max_stock" => out(core.product_max_stock(tk()?, &req::<String>(&args, "product_id")?, opt(&args, "max_stock_milli")?)),
        "replenish.list" => out(core.replenishment(tk()?, all(&args)?)),
        "requisitions.list" => out(core.requisitions_list(tk()?, opt(&args, "status")?)),
        "requisitions.get" => out(core.requisition_get(tk()?, &req::<String>(&args, "requisition_id")?)),
        "requisitions.create" => out(core.requisition_create(tk()?, all(&args)?)),
        "requisitions.from_suggestions" => out(core.requisition_from_suggestions(tk()?, all(&args)?)),
        "requisitions.save" => out(core.requisition_save(tk()?, &req::<String>(&args, "requisition_id")?, req(&args, "requisition")?)),
        "requisitions.set_status" => out(core.requisition_set_status(
            tk()?,
            &req::<String>(&args, "requisition_id")?,
            &req::<String>(&args, "action")?,
            opt(&args, "note")?,
        )),
        "requisitions.convert" => {
            out(core.requisition_convert(tk()?, &req::<String>(&args, "requisition_id")?, &req::<String>(&args, "operation_id")?))
        }
        "po.approve" => out(core.purchase_order_approve(
            tk()?,
            &req::<String>(&args, "po_id")?,
            opt(&args, "note")?,
            &req::<String>(&args, "operation_id")?,
        )),
        "receiving.shortage_decide" => {
            out(core.receipt_shortage_decide(tk()?, &req::<String>(&args, "discrepancy_id")?, &req::<String>(&args, "decision")?))
        }
        "receiving.open_shortages" => out(core.receiving_open_shortages(tk()?)),
        "supplier_invoices.match" => out(core.supplier_invoice_match(tk()?, &req::<String>(&args, "invoice_id")?)),
        "supplier_invoices.accept_match" => {
            out(core.supplier_invoice_accept_match(tk()?, &req::<String>(&args, "invoice_id")?, &req::<String>(&args, "note")?))
        }
        "supplier_returns.list" => out(core.supplier_returns_list(tk()?, opt(&args, "status")?, opt(&args, "supplier_id")?)),
        "supplier_returns.get" => out(core.supplier_return_get(tk()?, &req::<String>(&args, "return_id")?)),
        "supplier_returns.save" => out(core.supplier_return_save(tk()?, opt(&args, "return_id")?, req(&args, "return")?)),
        "supplier_returns.cancel" => out(core.supplier_return_cancel(tk()?, &req::<String>(&args, "return_id")?)),
        "supplier_returns.confirm" => {
            out(core.supplier_return_confirm(tk()?, &req::<String>(&args, "return_id")?, &req::<String>(&args, "operation_id")?))
        }
        "supplier_returns.reverse" => out(core.supplier_return_reverse(
            tk()?,
            &req::<String>(&args, "return_id")?,
            &req::<String>(&args, "reason")?,
            &req::<String>(&args, "operation_id")?,
        )),
        "supplier_returns.draft_credit" => out(core.supplier_return_draft_credit(
            tk()?,
            &req::<String>(&args, "return_id")?,
            &req::<String>(&args, "supplier_number")?,
            &req::<String>(&args, "date")?,
        )),
        "supplier_returns.link_credit" => {
            out(core.supplier_return_link_credit(tk()?, &req::<String>(&args, "return_id")?, &req::<String>(&args, "invoice_id")?))
        }
        "registers.list" => out(core.registers_list(tk()?)),
        "registers.devices" => out(core.register_devices(tk()?)),
        "registers.save" => out(core.register_save(tk()?, opt(&args, "register_id")?, req(&args, "register")?)),
        "drawers.save" => out(core.drawer_save(tk()?, opt(&args, "drawer_id")?, req(&args, "drawer")?)),
        "cases.list" => out(core.cases_list(tk()?, opt(&args, "status")?)),
        "cases.get" => out(core.case_get(tk()?, &req::<String>(&args, "case_id")?)),
        "cases.open_for_shift" => out(core.case_open_for_shift(tk()?, &req::<String>(&args, "shift_id")?, opt(&args, "note")?)),
        "cases.act" => out(core.case_act(tk()?, all(&args)?)),
        "cases.attach" => out(core.case_attach(
            tk()?,
            &req::<String>(&args, "case_id")?,
            &req::<String>(&args, "file_name")?,
            &req::<String>(&args, "data_b64")?,
        )),
        "cases.evidence" => out(core.case_evidence(tk()?, &req::<String>(&args, "case_id")?, &req::<String>(&args, "file_id")?)),
        // Expenses and petty cash (back office)
        "expenses.list" => out(core.expenses_list(tk()?, opt(&args, "from")?, opt(&args, "to")?, opt(&args, "status")?)),
        "expenses.get" => out(core.expense_get(tk()?, &req::<String>(&args, "expense_id")?)),
        "expenses.save" => out(core.expense_save(tk()?, opt(&args, "expense_id")?, req(&args, "expense")?)),
        "expenses.delete_draft" => out(core.expense_delete_draft(tk()?, &req::<String>(&args, "expense_id")?)),
        "expenses.submit" => out(core.expense_submit(tk()?, &req::<String>(&args, "expense_id")?)),
        "expenses.decide" => {
            out(core.expense_decide(tk()?, &req::<String>(&args, "expense_id")?, req(&args, "approve")?, opt(&args, "note")?))
        }
        "expenses.pay" => out(core.expense_pay(tk()?, &req::<String>(&args, "expense_id")?, req(&args, "payment")?)),
        "expenses.void" => out(core.expense_void(
            tk()?,
            &req::<String>(&args, "expense_id")?,
            &req::<String>(&args, "reason")?,
            &req::<String>(&args, "operation_id")?,
        )),
        "expenses.attach" => out(core.expense_attach(
            tk()?,
            &req::<String>(&args, "expense_id")?,
            &req::<String>(&args, "file_name")?,
            &req::<String>(&args, "data")?,
        )),
        "expenses.attachment" => out(core.expense_attachment(tk()?, &req::<String>(&args, "attachment_id")?)),
        "expenses.categories" => out(core.expense_categories(tk()?)),
        "expenses.category_save" => out(core.expense_category_save(
            tk()?,
            opt(&args, "category_id")?,
            &req::<String>(&args, "name")?,
            opt(&args, "name_ar")?,
            opt(&args, "active")?.unwrap_or(true),
        )),
        "expenses.recurring" => out(core.expense_recurring_list(tk()?)),
        "expenses.recurring_save" => out(core.expense_recurring_save(tk()?, opt(&args, "recurring_id")?, req(&args, "recurring")?)),
        "expenses.unlinked_paid_outs" => out(core.expense_unlinked_paid_outs(tk()?)),
        "petty.funds" => out(core.petty_funds(tk()?)),
        "petty.fund_save" => out(core.petty_fund_save(
            tk()?,
            opt(&args, "fund_id")?,
            &req::<String>(&args, "name")?,
            opt(&args, "custodian_user_id")?,
            opt(&args, "active")?.unwrap_or(true),
        )),
        "petty.entry" => out(core.petty_entry(
            tk()?,
            &req::<String>(&args, "fund_id")?,
            &req::<String>(&args, "kind")?,
            req(&args, "amount_minor")?,
            opt(&args, "note")?,
            &req::<String>(&args, "operation_id")?,
        )),
        "petty.count" => out(core.petty_count(
            tk()?,
            &req::<String>(&args, "fund_id")?,
            req(&args, "counted_minor")?,
            opt(&args, "note")?,
            &req::<String>(&args, "operation_id")?,
        )),
        "petty.entries" => out(core.petty_entries(tk()?, &req::<String>(&args, "fund_id")?)),
        // Accounts Payable (posting, payments, allocations, balances)
        "ap.overview" => out(core.ap_overview(tk()?)),
        "ap.supplier" => out(core.ap_supplier(tk()?, &req::<String>(&args, "supplier_id")?)),
        "ap.invoice_get" => out(core.ap_invoice_get(tk()?, &req::<String>(&args, "invoice_id")?)),
        "ap.invoice_create" => out(core.ap_invoice_create_manual(tk()?, req(&args, "invoice")?)),
        "ap.invoice_approve" => out(core.ap_invoice_approve(tk()?, &req::<String>(&args, "invoice_id")?)),
        "ap.invoice_post" => out(core.ap_invoice_post(tk()?, &req::<String>(&args, "invoice_id")?, &req::<String>(&args, "operation_id")?)),
        "ap.invoice_reverse" => out(core.ap_invoice_reverse(tk()?, &req::<String>(&args, "invoice_id")?, &req::<String>(&args, "reason")?)),
        "ap.payment_record" => out(core.ap_payment_record(tk()?, req(&args, "payment")?)),
        "ap.payment_get" => out(core.ap_payment_get(tk()?, &req::<String>(&args, "payment_id")?)),
        "ap.payment_reverse" => out(core.ap_payment_reverse(tk()?, &req::<String>(&args, "payment_id")?, &req::<String>(&args, "reason")?)),
        "ap.allocate" => out(core.ap_allocate(tk()?, opt(&args, "payment_id")?, opt(&args, "credit_id")?, req(&args, "allocations")?)),
        "ap.allocation_reverse" => out(core.ap_allocation_reverse(tk()?, &req::<String>(&args, "allocation_id")?)),
        "products.purchase_costs" => out(core.purchase_costs(tk()?, &req::<String>(&args, "product_id")?)),
        // WhatsApp AI orders (conversation → structured draft → staff review)
        "waorders.list" => out(core.wa_orders_list(tk()?, opt(&args, "filter")?)),
        "waorders.get" => out(core.wa_order_get(tk()?, &req::<String>(&args, "session_id")?)),
        "waorders.line" => out(core.wa_order_line(
            tk()?,
            &req::<String>(&args, "session_id")?,
            req(&args, "revision")?,
            opt(&args, "line_no")?,
            opt(&args, "product_id")?,
            opt(&args, "qty_milli")?,
            opt(&args, "remove")?.unwrap_or(false),
            opt(&args, "learn")?.unwrap_or(false),
        )),
        "waorders.delivery" => out(core.wa_order_delivery(
            tk()?,
            &req::<String>(&args, "session_id")?,
            req(&args, "revision")?,
            &req::<String>(&args, "mode")?,
            opt(&args, "address_parts")?,
            opt(&args, "zone_id")?,
        )),
        "waorders.customer" => out(core.wa_order_customer(
            tk()?,
            &req::<String>(&args, "session_id")?,
            req(&args, "revision")?,
            &req::<String>(&args, "customer_id")?,
        )),
        "waorders.flags" => out(core.wa_order_flags(
            tk()?,
            &req::<String>(&args, "session_id")?,
            opt(&args, "takeover")?,
            opt(&args, "handled")?,
            opt(&args, "assign_to_me")?,
        )),
        "waorders.confirm" => out(core.wa_order_confirm_checked(
            tk()?,
            &req::<String>(&args, "session_id")?,
            req(&args, "revision")?,
            opt::<bool>(&args, "acknowledge_shortage")?.unwrap_or(false),
            opt::<bool>(&args, "acknowledge_price_change")?.unwrap_or(false),
        )),
        "waorders.cancel" => out(core.wa_order_cancel(tk()?, &req::<String>(&args, "session_id")?)),
        "waorders.payment" => out(core.wa_order_payment(
            tk()?,
            &req::<String>(&args, "session_id")?,
            &req::<String>(&args, "review_id")?,
            &req::<String>(&args, "decision")?,
            opt(&args, "note")?,
        )),
        "waorders.send" => out(core.wa_order_send(tk()?, &req::<String>(&args, "session_id")?, opt(&args, "text")?)),
        "waorders.metrics" => out(core.wa_orders_metrics(tk()?)),
        // stock locations and transfers (inventory.locations)
        "locations.list" => out(core.locations_list(tk()?)),
        "locations.save" => out(core.location_save(tk()?, opt(&args, "location_id")?, req(&args, "location")?)),
        "locations.stock" => out(core.location_stock(tk()?, &req::<String>(&args, "location_id")?)),
        "transfers.create" => out(core.transfer_create(tk()?, all(&args)?)),
        "transfers.ship" => out(core.transfer_ship(tk()?, &req::<String>(&args, "transfer_id")?, &req::<String>(&args, "operation_id")?)),
        "transfers.receive" => {
            out(core.transfer_receive(tk()?, &req::<String>(&args, "transfer_id")?, &req::<String>(&args, "operation_id")?))
        }
        "transfers.cancel" => out(core.transfer_cancel(tk()?, &req::<String>(&args, "transfer_id")?)),
        "transfers.list" => out(core.transfers_list(tk()?, opt(&args, "status")?)),
        "transfers.get" => out(core.transfer_get(tk()?, &req::<String>(&args, "transfer_id")?)),
        "transfers.in_transit" => out(core.transfers_in_transit(tk()?)),
        // digital orders (orders.digital)
        "orders.list" => out(core.orders_list(tk()?, opt(&args, "status")?)),
        "orders.get" => out(core.order_get(tk()?, &req::<String>(&args, "order_id")?)),
        "orders.products" => out(core.orders_product_search(tk()?, &req::<String>(&args, "q")?)),
        "orders.save" => out(core.order_save(tk()?, opt(&args, "order_id")?, req(&args, "order")?)),
        "orders.from_inbox" => out(core.order_from_inbox(tk()?, req(&args, "seq")?)),
        "orders.confirm" => out(core.order_confirm_checked(
            tk()?,
            &req::<String>(&args, "order_id")?,
            opt::<bool>(&args, "acknowledge_shortage")?.unwrap_or(false),
        )),
        "orders.cancel" => out(core.order_cancel(tk()?, &req::<String>(&args, "order_id")?, opt(&args, "reason")?)),
        "orders.set_payment" => {
            out(core.order_set_payment(tk()?, &req::<String>(&args, "order_id")?, &req::<String>(&args, "payment_state")?))
        }
        "orders.convert" => out(core.order_convert(tk()?, &req::<String>(&args, "order_id")?, &req::<String>(&args, "operation_id")?)),
        // branches (org.multi_branch)
        "branches.list" => out(core.branches_list(tk()?)),
        "branches.save" => out(core.branch_save(tk()?, opt(&args, "branch_id")?, req(&args, "branch")?)),
        "branches.user_get" => out(core.user_branches_get(tk()?, &req::<String>(&args, "user_id")?)),
        "branches.user_set" => out(core.user_branches_set(tk()?, &req::<String>(&args, "user_id")?, req(&args, "branch_ids")?)),
        "branches.switch" => out(core.session_switch_branch(tk()?, &req::<String>(&args, "branch_id")?)),
        "branches.prices" => out(core.branch_prices_get(tk()?, &req::<String>(&args, "product_id")?)),
        "branches.set_price" => out(core.branch_price_set(
            tk()?,
            &req::<String>(&args, "product_id")?,
            &req::<String>(&args, "branch_id")?,
            opt(&args, "amount_minor")?,
        )),
        "ocr.retry" => out(core.ocr_retry(tk()?, &req::<String>(&args, "kind")?, &req::<String>(&args, "id")?)),
        "backup.create" => out(core.backup_create(tk()?, opt(&args, "directory")?)),
        "backup.inspect" => out(core.backup_inspect(tk()?, &req::<String>(&args, "path")?)),
        "backup.restore" => {
            out(core.backup_restore(tk()?, &req::<String>(&args, "path")?, opt(&args, "acknowledge_different_business")?.unwrap_or(false)))
        }
        // sync
        "sync.status" => out(core.sync_status(tk()?)),
        "sync.pairing_code" => out(core.sync_issue_pairing_code(tk()?, opt(&args, "device_name")?, opt(&args, "branch_id")?)),
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
