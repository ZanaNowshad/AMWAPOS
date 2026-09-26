//! Full admin tool map for the AI assistant.
//!
//! Every tool here is an existing command (`commands::dispatch`, or the hub
//! runtime for network-backed commands). There is no SQL, shell or free-form
//! command tool. Rules:
//! - Reads run immediately, as the signed-in user, and are capped at 50 rows.
//! - Writes only record a proposal. A person confirms it on the AI page; the
//!   same command the admin page uses then runs with the confirmer's session,
//!   so its permission, manager-approval and Windows Hello checks still apply.
//! - A tool is offered only if the user has `admin.access`, one of its
//!   permissions, the owner role when it is owner-only, and its feature flag.
//! - Anything that would expose secrets (keys, PINs, QR/pairing codes, WA
//!   session, hub credentials) or bypass the model's limits has no tool.

use serde_json::{json, Map, Value};

use crate::auth::Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Read,
    Propose,
}

#[derive(Debug, Clone, Copy)]
pub struct ToolSpec {
    pub name: &'static str,
    /// The command it runs (`virtual.*` = assembled from other reads).
    pub cmd: &'static str,
    pub kind: Kind,
    /// read | low | medium | high (writes); raised to high after DATA.
    pub risk: &'static str,
    /// Any one of these permissions (plus `admin.access`).
    pub perms: &'static [&'static str],
    pub flag: Option<&'static str>,
    pub owner_only: bool,
    /// Runs in the hub runtime (network, WhatsApp, step-up).
    pub runtime: bool,
    /// Output contains text from outside the store (wrapped as DATA).
    pub untrusted: bool,
    /// Generate `operation_id` when the proposal is created.
    pub op_id: bool,
    /// Values the Confirm card collects from the person (never from the model).
    pub confirm_inputs: &'static [&'static str],
    /// The command's result holds a one-time secret: shown on Confirm only.
    pub secret_result: bool,
    /// Parameters: "name:type" with type s|i|b|a|o and a trailing '!' when required.
    pub params: &'static str,
    pub desc: &'static str,
}

const fn read(name: &'static str, cmd: &'static str, perms: &'static [&'static str], params: &'static str, desc: &'static str) -> ToolSpec {
    ToolSpec {
        name,
        cmd,
        kind: Kind::Read,
        risk: "read",
        perms,
        flag: None,
        owner_only: false,
        runtime: false,
        untrusted: false,
        op_id: false,
        confirm_inputs: &[],
        secret_result: false,
        params,
        desc,
    }
}

const fn write(
    name: &'static str,
    cmd: &'static str,
    risk: &'static str,
    perms: &'static [&'static str],
    params: &'static str,
    desc: &'static str,
) -> ToolSpec {
    ToolSpec { kind: Kind::Propose, risk, ..read(name, cmd, perms, params, desc) }
}

impl ToolSpec {
    const fn flag(mut self, f: &'static str) -> Self {
        self.flag = Some(f);
        self
    }
    const fn owner(mut self) -> Self {
        self.owner_only = true;
        self
    }
    const fn rt(mut self) -> Self {
        self.runtime = true;
        self
    }
    const fn data(mut self) -> Self {
        self.untrusted = true;
        self
    }
    const fn op(mut self) -> Self {
        self.op_id = true;
        self
    }
    const fn inputs(mut self, i: &'static [&'static str]) -> Self {
        self.confirm_inputs = i;
        self
    }
    const fn secret(mut self) -> Self {
        self.secret_result = true;
        self
    }
}

const SALES: &[&str] = &["sales.view", "reports.sales"];
const CATALOG: &[&str] = &["products.view"];
const PRODUCTS: &[&str] = &["products.manage"];
const PRICES: &[&str] = &["prices.manage"];
const INV: &[&str] = &["inventory.view"];
const PURCH: &[&str] = &["purchasing.manage", "suppliers.manage", "inventory.receive"];
const CUST: &[&str] = &["customers.view"];
const CUSTM: &[&str] = &["customers.manage"];
const DELIV: &[&str] = &["deliveries.view", "deliveries.manage"];
const USERS: &[&str] = &["users.manage"];
const SETTINGS: &[&str] = &["settings.manage"];
const DIAG: &[&str] = &["diagnostics.view", "settings.manage"];
const WA: &[&str] = &["whatsapp.manage"];

/// The admin tool map. Existing assistant tools (list_reports, run_report,
/// search_products, product_details, low_stock, eod_pack, branch_context,
/// loyalty_balance, digital_order_get, recent_whatsapp_messages,
/// invoice_scan_text, propose_price_change, propose_stock_adjustment,
/// propose_purchase_order) stay in `ai.rs`.
pub const TOOLS: &[ToolSpec] = &[
    // ---- reads -----------------------------------------------------------
    read("dashboard_kpis", "dashboard.get", &["reports.sales", "admin.access"], "", "Today's KPIs, hourly sales, top products and attention items (the dashboard)."),
    read("sale_get", "sales.get", SALES, "sale_id:s!", "One sale with items, payments and refunds."),
    read("sale_by_receipt", "sales.find_receipt", SALES, "receipt_number:s!", "Find a sale by its receipt number."),
    read("search_sales", "sales.list", SALES, "from:s,to:s,receipt:s,cashier_id:s,method:s,customer_id:s,device_id:s,shift_id:s,limit:i", "Search sales by date (YYYY-MM-DD), receipt number, cashier, payment method, customer, terminal or shift."),
    read("refund_lookup", "refunds.lookup", &["refund.create", "sales.view"], "receipt_number:s!", "What can still be refunded on a receipt."),
    read("refund_preview", "refunds.preview", &["refund.create"], "sale_id:s!,lines:a!", "Preview the amounts of a refund without recording it. lines: [{sale_item_id, qty_milli, restock}]."),
    read("list_refunds", "refunds.list", SALES, "from:s,to:s,limit:i", "Refunds in a date range."),
    read("refund_get", "virtual.refund_get", SALES, "refund:s!,from:s,to:s", "One refund by id or refund number (searches the date range, default last 90 days)."),
    read("receipt_preview", "receipts.preview", SALES, "kind:s!,ref_id:s!", "Receipt text for a sale, refund or shift (kind: sale | refund | shift)."),
    read("print_queue", "print.queue", &["admin.access"], "", "Print jobs waiting or failed."),
    read("list_printers", "print.printers", SETTINGS, "", "Printers this computer can see."),
    read("shift_get", "shift.get", SALES, "shift_id:s!", "One shift: float, cash sales, refunds, paid in/out, safe drops, expected cash, counted cash and variance."),
    read("current_shift", "shift.current", &["admin.access"], "", "The open shift on this till, if any."),
    read("list_shifts", "shift.list", SALES, "from:s,to:s,limit:i", "Shifts in a date range with expected cash and variance."),
    read("cash_events_list", "cash.list", SALES, "shift_id:s,from:s,to:s", "Paid in, paid out, safe drops and no-sale drawer opens."),
    read("list_categories", "categories.list", CATALOG, "include_inactive:b", "Product categories."),
    read("category_get", "virtual.category_get", CATALOG, "category_id:s!", "One category."),
    read("list_tax_rules", "tax.list", CATALOG, "", "VAT rules."),
    read("unknown_barcodes_list", "barcodes.unknown_list", &["barcodes.resolve", "products.manage"], "status:s", "Barcodes scanned at the till that no product has (status: open | resolved | dismissed)."),
    read("inventory_get", "virtual.inventory_get", INV, "product_id:s!", "Stock level, reorder point, cost and recent movements of one product."),
    read("movements_list", "inventory.movements", INV, "product_id:s,kind:s,from:s,to:s,user_id:s,limit:i", "Stock ledger entries."),
    read("list_stocktakes", "stocktake.list", &["stocktake.manage", "inventory.view"], "", "Stocktakes."),
    read("stocktake_get", "stocktake.get", &["stocktake.manage", "inventory.view"], "stocktake_id:s!", "One stocktake with its counts and differences."),
    read("list_locations", "locations.list", INV, "", "Stock locations.").flag("inventory.locations"),
    read("location_stock", "locations.stock", INV, "location_id:s!", "Stock held at one location.").flag("inventory.locations"),
    read("list_transfers", "transfers.list", INV, "status:s", "Stock transfers (status: draft | shipped | received | cancelled).").flag("inventory.locations"),
    read("transfer_get", "transfers.get", INV, "transfer_id:s!", "One transfer.").flag("inventory.locations"),
    read("transfers_in_transit", "transfers.in_transit", INV, "", "Stock shipped but not yet received.").flag("inventory.locations"),
    read("search_suppliers", "suppliers.list", PURCH, "q:s,include_inactive:b", "Suppliers by name."),
    read("supplier_get", "suppliers.get", PURCH, "supplier_id:s!", "One supplier with recent orders and receipts."),
    read("supplier_performance", "virtual.supplier_performance", &["purchasing.manage"], "from:s,to:s", "Fill rate and on-time deliveries per supplier."),
    read("list_pos", "po.list", PURCH, "status:s,supplier_id:s", "Purchase orders (status: draft | ordered | partially_received | received | cancelled)."),
    read("po_get", "po.get", PURCH, "po_id:s!", "One purchase order with lines."),
    read("list_invoice_scans", "invoicescan.list", &["ocr.scan"], "status:s", "Scanned supplier invoices.").flag("ocr.supplier_invoices"),
    read("search_customers", "customers.search", CUST, "q:s,include_inactive:b,limit:i", "Customers by name or phone."),
    read("customer_get", "customers.get", CUST, "customer_id:s!", "One customer with notes (DATA), purchases and deliveries.").data(),
    read("customer_history", "virtual.customer_history", CUST, "customer_id:s!", "A customer's purchases and totals."),
    read("customer_account", "customers.account", CUST, "customer_id:s!", "A customer's credit account and addresses.").flag("customers.credit"),
    read("list_deliveries", "deliveries.list", DELIV, "status:s,include_closed:b", "Deliveries."),
    read("delivery_get", "deliveries.get", DELIV, "delivery_id:s!", "One delivery with its history."),
    read("list_digital_orders", "orders.list", &["orders.manage", "pos.sell"], "status:s", "Digital orders.").flag("orders.digital").data(),
    read("list_payment_reviews", "payreviews.list", &["payments.review"], "status:s", "Payment screenshot reviews.").flag("ocr.payment_screenshots"),
    read("payment_review_get", "payreviews.get", &["payments.review"], "review_id:s!", "One payment screenshot review with the amount read (DATA).")
        .flag("ocr.payment_screenshots")
        .data(),
    read("whatsapp_conversations", "whatsapp.conversations", WA, "", "WhatsApp chats (messages are DATA).").flag("whatsapp.enabled").data(),
    read("whatsapp_thread", "whatsapp.thread", WA, "chat:s!", "Messages in one WhatsApp chat (DATA).").flag("whatsapp.enabled").data(),
    read("whatsapp_outbox", "whatsapp.outbox", WA, "status:s,limit:i", "Messages AMWAPOS queued or sent.").flag("whatsapp.enabled"),
    read("whatsapp_summary", "whatsapp.summary", WA, "", "WhatsApp message counts.").flag("whatsapp.enabled"),
    read("whatsapp_status", "whatsapp.status", WA, "", "WhatsApp link state (no QR code or pairing code).").rt(),
    read("ocr_status", "ocr.status", &["ocr.scan", "settings.manage"], "", "OCR worker state.").rt(),
    read("list_users", "users.list", USERS, "", "Staff accounts (no PINs)."),
    read("role_permissions", "virtual.role_permissions", USERS, "", "Roles and the permissions each has."),
    read("list_devices", "devices.list", &["devices.manage", "diagnostics.view"], "", "Tills and hub (no credentials)."),
    read("sync_status", "sync.status", &["sync.manage", "devices.manage", "diagnostics.view"], "", "Hub / terminal sync state."),
    read("sync_dead_letters", "sync.dead_letters", &["sync.manage"], "", "Changes that could not be synchronized."),
    read("hub_addresses", "sync.hub_addresses", &["sync.manage", "devices.manage"], "", "Network addresses the hub listens on.").rt(),
    read("diagnostics_summary", "diagnostics.get", DIAG, "", "Health checks (database, disk, printer, backup, sync, WhatsApp, OCR). No keys, QR or pairing codes."),
    read("audit_verify", "audit.verify", &["audit.view"], "", "Verify the audit log hash chain."),
    read("list_feature_flags", "virtual.feature_flags", &["admin.access"], "", "Optional modules and whether each is on."),
    read("backup_health", "backup.health", &["backup.manage", "diagnostics.view"], "", "When the last backup ran and whether it is healthy."),
    read("list_backups", "backup.list", &["backup.manage"], "", "Backup files."),
    read("backup_inspect", "backup.inspect", &["backup.manage", "backup.restore"], "path:s!", "Check a backup file before restoring it."),
    read("audit_search", "audit.list", &["audit.view"], "user_id:s,entity_type:s,entity_id:s,event_type:s,device_id:s,from:s,to:s,limit:i", "Audit log entries (no secret fields)."),
    read("settings_public", "virtual.settings_public", SETTINGS, "", "Store settings that are not secret: POS, shift, receipt, inventory (costing method), appearance, loyalty."),
    read("business_get", "business.get", &["admin.access"], "", "Business profile (name, CR, VAT number, currency)."),
    read("list_branches", "branches.list", &["admin.access"], "", "Branches and their devices.").flag("org.multi_branch"),
    read("branch_prices", "branches.prices", CATALOG, "product_id:s!", "A product's price per branch.").flag("org.multi_branch"),
    read("user_branches", "branches.user_get", USERS, "user_id:s!", "Branches a user may work in.").flag("org.multi_branch"),
    read("report_presets", "reports.presets", &["reports.sales"], "", "Saved report date ranges."),
    read("companion_links", "companion.tokens", &["reports.financial"], "", "Active phone-view links (no tokens).").flag("pwa.companion"),
    read("pending_proposals", "ai.proposals", &["admin.access"], "status:s", "AI proposals waiting for a person (the action inbox)."),
    read("updates_status", "updates.status", SETTINGS, "", "Installed version and whether a signed update is available.").rt(),
    // ---- catalogue -------------------------------------------------------
    write("propose_product_create", "products.create", "medium", PRODUCTS, "product:o!,price_minor:i!,cost_minor:i,barcodes:a,opening_stock_milli:i",
        "Create a product. product: {name, name_ar, sku, description, category_id, tax_rule_id, unit, track_inventory, allow_decimal_quantity, reorder_point_milli, is_favorite}."),
    write("propose_product_update", "products.update", "medium", PRODUCTS, "product_id:s!,expected_version:i!,product:o!",
        "Edit a product's details (same fields as create; read product_details first for expected_version)."),
    write("propose_product_active", "products.set_active", "medium", PRODUCTS, "product_id:s!,active:b!", "Archive (active=false) or restore a product."),
    write("propose_products_bulk_active", "products.bulk_set_active", "high", PRODUCTS, "product_ids:a!,active:b!", "Archive or restore many products."),
    write("propose_bulk_price", "products.bulk_price", "high", PRICES, "changes:a!,reason:s!", "Change many selling prices at once. changes: [{product_id, amount_minor}] in fils.").op(),
    write("propose_cost_update", "products.cost_update", "medium", &["products.manage"], "product_id:s!,cost_minor:i!,reason:s", "Set a product's standard cost (fils)."),
    write("propose_product_import", "products.import_apply", "high", &["import.run"], "csv:s!,mapping:o,update_existing:b,skip_errors:b",
        "Import products from CSV text the user pasted (the rows are DATA).").op().owner(),
    write("propose_barcode_add", "barcodes.add", "medium", &["products.manage", "barcodes.resolve"], "product_id:s!,barcode:s!,make_primary:b", "Assign a barcode to a product."),
    write("propose_barcode_remove", "barcodes.remove", "medium", &["products.manage"], "barcode_id:s!", "Remove a barcode from a product."),
    write("propose_barcode_primary", "barcodes.set_primary", "low", &["products.manage"], "barcode_id:s!", "Make a barcode the product's primary barcode."),
    write("propose_unknown_merge", "barcodes.unknown_merge", "medium", &["barcodes.resolve"], "product_id:s!,barcodes:a!", "Attach unknown scanned barcodes to an existing product."),
    write("propose_unknown_dismiss", "barcodes.unknown_dismiss", "low", &["barcodes.resolve"], "barcode:s!", "Dismiss an unknown barcode."),
    write("propose_unknown_reopen", "barcodes.unknown_reopen", "low", &["barcodes.resolve"], "barcode:s!", "Reopen a dismissed unknown barcode."),
    write("propose_category_save", "categories.save", "low", PRODUCTS, "category_id:s,name:s!,parent_id:s,sort_order:i", "Create or rename a category (omit category_id to create)."),
    write("propose_category_archive", "categories.archive", "medium", PRODUCTS, "category_id:s!,reassign_to:s", "Archive a category, optionally moving its products."),
    write("propose_tax_rule_create", "tax.create", "high", SETTINGS, "name:s!,rate_bp:i!,inclusive:b!,replace_rule_id:s", "Add a VAT rule (rate in basis points: 1000 = 10%).").owner(),
    write("propose_tax_rule_active", "tax.set_active", "high", SETTINGS, "tax_rule_id:s!,active:b!", "Enable or disable a VAT rule.").owner(),
    write("propose_branch_price", "branches.set_price", "medium", PRICES, "product_id:s!,branch_id:s!,amount_minor:i",
        "Set (or clear with null) a branch price override in fils.").flag("org.multi_branch"),
    // ---- inventory -------------------------------------------------------
    write("propose_receive_stock", "inventory.receive", "medium", &["inventory.receive"], "supplier_id:s,reference:s,lines:a!",
        "Receive goods without a PO. lines: [{product_id, qty_milli, unit_cost_minor}].").op(),
    write("propose_stocktake_create", "stocktake.create", "medium", &["stocktake.manage"], "name:s!,scope_type:s!,category_id:s,product_ids:a,blind:b",
        "Start a stocktake (scope_type: all | category | products)."),
    write("propose_stocktake_count", "stocktake.count", "low", &["stocktake.manage"], "stocktake_id:s!,product_id:s,barcode:s,qty_milli:i!,mode:s",
        "Record a count (mode: set | add)."),
    write("propose_stocktake_status", "stocktake.set_status", "medium", &["stocktake.manage"], "stocktake_id:s!,status:s!", "Move a stocktake to review / counting / cancelled."),
    write("propose_stocktake_finalize", "stocktake.finalize", "high", &["stocktake.manage"], "stocktake_id:s!", "Approve a stocktake: posts the stock differences.").op(),
    write("propose_location_save", "locations.save", "low", &["inventory.transfer"], "location_id:s,location:o!", "Create or edit a stock location. location: {code, name, active}.")
        .flag("inventory.locations"),
    write("propose_transfer_create", "transfers.create", "medium", &["inventory.transfer"], "to_branch_id:s,from_location_id:s,to_location_id:s,note:s,lines:a!",
        "Draft a stock transfer. lines: [{product_id, qty_milli}].").flag("inventory.locations"),
    write("propose_transfer_ship", "transfers.ship", "medium", &["inventory.transfer"], "transfer_id:s!", "Ship a draft transfer (stock leaves the source).")
        .flag("inventory.locations")
        .op(),
    write("propose_transfer_receive", "transfers.receive", "medium", &["inventory.transfer"], "transfer_id:s!", "Receive a shipped transfer.")
        .flag("inventory.locations")
        .op(),
    write("propose_transfer_cancel", "transfers.cancel", "low", &["inventory.transfer"], "transfer_id:s!", "Cancel a draft transfer.").flag("inventory.locations"),
    // ---- purchasing ------------------------------------------------------
    write("propose_supplier_save", "suppliers.save", "medium", &["suppliers.manage"], "supplier_id:s,supplier:o!",
        "Create or edit a supplier. supplier: {name, cr_number, vat_number, contact_name, phone, whatsapp, email, address, payment_terms, notes, active}."),
    write("propose_po_save", "po.save", "medium", &["purchasing.manage"], "po_id:s,po:o!",
        "Create or edit a draft PO. po: {supplier_id, reference, expected_at, notes, lines:[{product_id, qty_milli, unit_cost_minor, tax_rate_bp}]}."),
    write("propose_po_status", "po.set_status", "medium", &["purchasing.manage"], "po_id:s!,status:s!", "Mark a PO ordered or cancelled."),
    write("propose_po_receive", "po.receive", "medium", &["inventory.receive"], "po_id:s!,reference:s,lines:a!",
        "Receive against a PO. lines: [{po_item_id, qty_milli, unit_cost_minor}].").op(),
    write("propose_invoice_line", "invoicescan.update_line", "low", &["ocr.scan"], "scan_id:s!,line_no:i!,product_id:s,clear_product:b,qty_milli:i,unit_cost_minor:i,include:b",
        "Correct one line of a scanned invoice.").flag("ocr.supplier_invoices"),
    write("propose_invoice_confirm", "invoicescan.confirm", "medium", &["ocr.scan"], "scan_id:s!,supplier_id:s!,receive:b",
        "Turn a reviewed invoice scan into a PO (receive=true also receives the stock: high risk).").flag("ocr.supplier_invoices"),
    write("propose_invoice_reject", "invoicescan.reject", "low", &["ocr.scan"], "scan_id:s!,reason:s!", "Reject an invoice scan.").flag("ocr.supplier_invoices"),
    write("propose_invoice_ai_parse", "invoicescan.ai_parse", "low", &["ocr.scan"], "scan_id:s!",
        "Ask the AI provider to re-read an invoice scan's lines (the review screen still applies).").flag("ocr.ai_parse").rt(),
    write("propose_ocr_retry", "ocr.retry", "low", &["ocr.scan", "payments.review"], "kind:s!,id:s!", "Run OCR again (kind: invoice | payment)."),
    // ---- customers -------------------------------------------------------
    write("propose_customer_save", "customers.save", "low", CUSTM, "customer_id:s,customer:o!",
        "Create or edit a customer. customer: {name, phone, whatsapp, email, area, address, active}."),
    write("propose_customer_note", "customers.add_note", "low", CUSTM, "customer_id:s!,note:s!", "Add a note to a customer."),
    write("propose_customer_address", "customers.address_save", "low", CUSTM, "address_id:s,customer_id:s!,label:s!,area:s,address:s!,notes:s,is_default:b",
        "Add or edit a customer's delivery address."),
    write("propose_customer_address_delete", "customers.address_delete", "low", CUSTM, "address_id:s!", "Delete a customer address."),
    write("propose_loyalty_adjust", "loyalty.adjust", "medium", &["loyalty.adjust"], "customer_id:s!,points:i!,note:s!", "Add (+) or remove (−) loyalty points.")
        .flag("loyalty.enabled"),
    write("propose_credit_account", "customers.account_set", "high", &["customers.credit"], "customer_id:s!,enabled:b!,credit_limit_minor:i!",
        "Open/close a customer's credit account and set its limit (fils).").flag("customers.credit"),
    write("propose_credit_payment", "customers.account_payment", "high", &["customers.credit"], "customer_id:s!,amount_minor:i!,method:s!,reference:s",
        "Record a payment against a customer's credit balance.").flag("customers.credit").op(),
    write("propose_credit_adjust", "customers.account_adjust", "high", &["customers.credit_override"], "customer_id:s!,amount_minor:i!,note:s!",
        "Adjust a customer's credit balance.").flag("customers.credit").op(),
    // ---- deliveries / orders ---------------------------------------------
    write("propose_delivery_create", "deliveries.create", "medium", &["deliveries.manage"], "sale_id:s,customer_id:s,address:s,area:s,phone:s,payment_status:s,amount_minor:i,notes:s",
        "Create a delivery."),
    write("propose_delivery_update", "deliveries.update", "medium", &["deliveries.manage", "deliveries.view"], "delivery_id:s!,status:s,assigned_user_id:s,payment_status:s,note:s",
        "Change a delivery's status, driver or payment status."),
    write("propose_order_save", "orders.save", "medium", &["orders.manage"], "order_id:s,order:o!",
        "Create or edit a draft digital order. order: {channel, external_ref, customer_id, phone, payment_state, note, address, delivery_wanted, lines:[{product_id, description, qty_milli}]}.")
        .flag("orders.digital"),
    write("propose_order_confirm", "orders.confirm", "medium", &["orders.manage"], "order_id:s!", "Confirm a digital order (every line matched).").flag("orders.digital"),
    write("propose_order_cancel", "orders.cancel", "medium", &["orders.manage"], "order_id:s!,reason:s", "Cancel a digital order.").flag("orders.digital"),
    write("propose_order_payment", "orders.set_payment", "medium", &["orders.manage"], "order_id:s!,payment_state:s!", "Set an order's payment state (unpaid | recorded | screenshot_pending).")
        .flag("orders.digital"),
    write("propose_order_convert", "orders.convert", "medium", &["pos.sell"], "order_id:s!",
        "Load a confirmed order into this till's sale. The cashier still takes payment; the AI never finalizes a sale.").flag("orders.digital").op(),
    write("propose_order_from_message", "orders.from_inbox", "medium", &["orders.manage"], "seq:i!", "Draft an order from a WhatsApp message (a person reviews it).")
        .flag("orders.digital"),
    write("propose_payment_review_expected", "payreviews.set_expected", "low", &["payments.review"], "review_id:s!,expected_minor:i,delivery_id:s",
        "Set the amount a payment screenshot should show.").flag("ocr.payment_screenshots"),
    write("propose_payment_review_decide", "payreviews.decide", "high", &["payments.review"], "review_id:s!,decision:s!,note:s,delivery_id:s",
        "Accept or reject a payment screenshot (decision: accepted | rejected).").flag("ocr.payment_screenshots"),
    // ---- cash / sales ----------------------------------------------------
    write("propose_refund", "refunds.create", "high", &["refund.create"], "sale_id:s!,lines:a!,reason:s!,tenders:a",
        "Refund items of a sale (full refund flow). lines: [{sale_item_id, qty_milli, restock}]; tenders: [{method, amount_minor, reference}].")
        .op()
        .inputs(&["approval_token"]),
    write("propose_cash_event", "cash.event", "high", &["cash.paid_in", "cash.paid_out", "cash.safe_drop", "shift.open"], "kind:s!,amount_minor:i,reason:s!",
        "Record paid in, paid out, safe drop or a no-sale drawer open on this till's open shift (kind: paid_in | paid_out | safe_drop | no_sale).")
        .op()
        .inputs(&["approval_token"]),
    write("propose_reprint", "sales.reprint", "low", &["pos.reprint", "sales.view"], "sale_id:s!", "Reprint a receipt."),
    write("propose_print_retry", "print.retry", "low", &["admin.access"], "job_id:s!", "Retry a failed print job."),
    write("propose_print_test", "print.test", "low", SETTINGS, "", "Print a test page."),
    write("propose_drawer_test", "print.drawer_test", "medium", SETTINGS, "", "Open the cash drawer as a test (recorded)."),
    // ---- people / devices ------------------------------------------------
    write("propose_user_create", "users.create", "high", USERS, "user:o!",
        "Add a staff account. user: {display_name, role_id, active}. The PIN is typed on the Confirm card, never in chat.").owner().inputs(&["user.pin"]),
    write("propose_user_update", "users.update", "high", USERS, "user_id:s!,user:o!",
        "Change a staff account's name, role or active flag (deactivate = lock). user: {display_name, role_id, active}.").owner(),
    write("propose_user_reset_pin", "users.update", "high", USERS, "user_id:s!",
        "Reset a staff PIN. The new PIN is typed on the Confirm card; it is never sent to the assistant.").owner().inputs(&["user.pin"]),
    write("propose_user_unlock", "users.unlock", "medium", USERS, "user_id:s!", "Unlock an account locked by wrong PINs."),
    write("propose_role_save", "roles.save", "high", &["roles.manage"], "role_id:s,name:s!,description:s,permissions:a!", "Create or edit a role's permissions.").owner(),
    write("propose_user_branches", "branches.user_set", "high", &["branches.manage"], "user_id:s!,branch_ids:a!", "Set the extra branches a user may work in.")
        .flag("org.multi_branch")
        .owner(),
    write("propose_branch_save", "branches.save", "high", &["branches.manage"], "branch_id:s,branch:o!", "Create or edit a branch. branch: {code, name, address, phone, active}.")
        .flag("org.multi_branch")
        .owner(),
    write("propose_switch_branch", "branches.switch", "medium", &["admin.access"], "branch_id:s!", "Work in another branch for back-office tasks.").flag("org.multi_branch"),
    write("propose_device_rename", "devices.rename", "low", &["devices.manage"], "device_id:s!,name:s!", "Rename a till or hub."),
    write("propose_device_active", "devices.set_active", "high", &["devices.manage"], "device_id:s!,active:b!", "Enable or disable (revoke) a device.").owner(),
    // ---- system ----------------------------------------------------------
    write("propose_setting", "settings.save", "medium", SETTINGS, "key:s!,value:o!",
        "Save one settings section (keys: pos, shift, payments, receipt, inventory, appearance, security, loyalty, features). features, security and inventory (costing method) are owner-only and high risk. Read settings_public first; send the whole section."),
    write("propose_business_update", "business.update", "high", SETTINGS, "name:s!,name_ar:s,cr_number:s,vat_number:s,phone:s,address:s,currency:s!,currency_digits:i!,timezone:s!",
        "Change the business profile.").owner(),
    write("propose_preset_save", "reports.preset_save", "low", &["reports.sales"], "preset:o!", "Save a report date range. preset: {name, range_kind, from_date, to_date}."),
    write("propose_preset_delete", "reports.preset_delete", "low", &["reports.sales"], "preset_id:s!", "Delete a saved date range."),
    write("propose_backup_now", "backup.create", "low", &["backup.manage"], "", "Run a backup now."),
    write("propose_backup_restore", "backup.restore", "high", &["backup.restore"], "path:s!,acknowledge_different_business:b",
        "Restore a backup (replaces the live data after a safety copy; the app restarts its services).").owner().rt(),
    write("propose_bind_address", "sync.set_bind_address", "high", &["sync.manage"], "address:s!", "Choose the network card the hub listens on.").owner().rt(),
    write("propose_enable_hub", "sync.enable_hub", "high", &["sync.manage"], "", "Turn this computer into the hub.").owner().rt(),
    write("propose_sync_now", "sync.run_now", "low", &["sync.manage", "devices.manage"], "", "Synchronize with the hub now.").rt(),
    write("propose_sync_unblock", "sync.unblock", "high", &["sync.manage"], "accept_new_hub:b", "Resume sync after it was paused for safety.").owner().rt(),
    write("propose_sync_retry", "sync.retry_dead_letter", "medium", &["sync.manage"], "dead_id:s!", "Retry one change that could not be synchronized."),
    write("propose_update_check", "updates.check", "low", SETTINGS, "", "Check for a signed update (does not install).").rt(),
    write("propose_companion_link", "companion.issue", "medium", &["reports.financial"], "label:s,hours:i",
        "Create a phone-view link. The link is shown on the Confirm card only.").flag("pwa.companion").secret(),
    write("propose_companion_revoke", "companion.revoke", "low", &["reports.financial"], "id:s!", "Revoke a phone-view link.").flag("pwa.companion"),
    // ---- WhatsApp --------------------------------------------------------
    write("propose_whatsapp_send", "whatsapp.queue", "medium", &["whatsapp.send", "whatsapp.manage"], "kind:s!,to_phone:s,customer_id:s,sale_id:s,delivery_id:s,review_id:s,lang:s,text:s,document_name:s",
        "Queue a WhatsApp message (kind: receipt | delivery_update | payment_request | text). Nothing is sent until a person confirms.").flag("whatsapp.enabled").op(),
    write("propose_whatsapp_outbox", "whatsapp.outbox_action", "low", WA, "message_id:s!,action:s!", "Retry or cancel a queued WhatsApp message (action: retry | cancel).")
        .flag("whatsapp.enabled"),
    write("propose_whatsapp_mark_read", "whatsapp.mark_read", "low", WA, "chat:s!", "Mark a chat read.").flag("whatsapp.enabled"),
    write("propose_whatsapp_import_contacts", "whatsapp.import_contacts", "medium", WA, "chats:a", "Create customers from WhatsApp chats.").flag("whatsapp.enabled"),
    write("propose_whatsapp_connect", "whatsapp.start", "medium", WA, "", "Start the WhatsApp link (the QR code shows on the WhatsApp page, never in chat).")
        .flag("whatsapp.enabled")
        .rt()
        .secret(),
    write("propose_whatsapp_disconnect", "whatsapp.stop", "medium", WA, "", "Stop the WhatsApp link (keeps the session).").flag("whatsapp.enabled").rt(),
    write("propose_whatsapp_logout", "whatsapp.logout", "high", WA, "", "Log this shop's WhatsApp out (the phone must be linked again).")
        .flag("whatsapp.enabled")
        .rt()
        .owner(),
    write("propose_whatsapp_session_backup", "whatsapp.session_backup", "high", SETTINGS, "acknowledge_risk:b!",
        "Back up the WhatsApp session file (anyone with the file can use this WhatsApp).").flag("whatsapp.enabled").rt().owner(),
];

/// Commands that deliberately have no tool, with the reason.
pub const NO_TOOL: &[(&str, &str)] = &[
    ("app.ping", "internal"),
    ("setup.status", "setup wizard only"),
    ("setup.initialize", "setup wizard only"),
    ("auth.users", "forbidden: sign-in"),
    ("auth.login", "forbidden: sign-in"),
    ("auth.logout", "forbidden: sign-in"),
    ("auth.lock", "forbidden: sign-in"),
    ("auth.unlock", "forbidden: PIN entry"),
    ("auth.session", "internal"),
    ("auth.touch", "internal"),
    ("auth.approve", "forbidden: manager PIN approval happens on the Confirm card"),
    ("auth.approvers", "internal"),
    ("auth.change_pin", "forbidden: PINs"),
    ("ai.configure", "forbidden: API keys"),
    ("ai.test", "forbidden: uses API keys"),
    ("ai.models", "forbidden: uses API keys"),
    ("ai.ask", "the assistant itself"),
    ("ai.status", "internal"),
    ("ai.conversations", "internal"),
    ("ai.conversation", "internal"),
    ("ai.proposal_confirm", "forbidden: only a person confirms"),
    ("ai.proposal_reject", "a person decides on the AI page"),
    ("ai.proposal_undo", "a person decides on the AI page"),
    ("sync.pairing_code", "forbidden: pairing codes"),
    ("sync.reset_hub_credentials", "forbidden: hub credentials"),
    ("sync.join", "device setup only"),
    ("sync.discover", "device setup only"),
    ("sync.probe", "device setup only"),
    ("whatsapp.pair_code", "forbidden: pairing codes"),
    ("updates.download", "forbidden: installs replace the program (Updates page)"),
    ("updates.install", "forbidden: installs replace the program (Updates page)"),
    ("pos.config", "till only"),
    ("pos.cart", "till only"),
    ("pos.scan", "till only"),
    ("pos.search", "till only"),
    ("pos.add_product", "till only"),
    ("pos.add_custom", "till only"),
    ("pos.set_qty", "till only"),
    ("pos.remove_line", "till only"),
    ("pos.line_discount", "till only"),
    ("pos.cart_discount", "till only"),
    ("pos.price_override", "till only"),
    ("pos.loyalty_redeem", "till only"),
    ("pos.set_customer", "till only"),
    ("pos.hold", "till only"),
    ("pos.held", "till only"),
    ("pos.restore", "till only"),
    ("pos.held_delete", "till only"),
    ("pos.cancel", "till only"),
    ("pos.finalize", "forbidden: sales finalize only at the till"),
    ("shift.open", "till only (counting the float)"),
    ("shift.close", "till only (counting the drawer)"),
    ("products.search", "covered by search_products"),
    ("products.get", "covered by product_details"),
    ("products.price_update", "covered by propose_price_change"),
    ("inventory.adjust", "covered by propose_stock_adjustment"),
    ("reports.catalog", "covered by list_reports"),
    ("reports.run", "covered by run_report"),
    ("reports.eod", "covered by eod_pack"),
    ("loyalty.customer", "covered by loyalty_balance"),
    ("orders.get", "covered by digital_order_get"),
    ("whatsapp.recent", "covered by recent_whatsapp_messages"),
    ("invoicescan.get", "covered by invoice_scan_text"),
    ("branches.list", "covered by list_branches"),
    ("orders.products", "covered by search_products"),
    ("products.export_csv", "file export for the page"),
    ("products.import_preview", "file preview for the page"),
    ("reports.csv", "file export for the page"),
    ("reports.eod_zip", "file export for the page"),
    ("diagnostics.export", "file export for the page (diagnostics_summary is the read)"),
    ("receipts.pdf", "file export for the page"),
    ("whatsapp.media", "binary attachment"),
    ("migration.read", "needs files chosen in the page"),
    ("migration.preview", "needs files chosen in the page"),
    ("migration.apply", "needs files chosen in the page"),
    ("payreviews.upload", "needs an image chosen in the page"),
    ("invoicescan.import", "needs an image chosen in the page"),
    ("roles.list", "covered by role_permissions"),
    ("roles.permissions", "covered by role_permissions"),
    ("settings.get", "covered by settings_public (secrets excluded)"),
    ("customers.add_note", "covered by propose_customer_note"),
    ("ai.digest", "the AI page's action inbox (pending_proposals is the read)"),
    ("ai.playbook", "the AI page's playbook buttons (each step is a read tool)"),
];

pub fn find(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|t| t.name == name)
}

/// Is this tool offered to the session (before flags)?
pub fn allowed(t: &ToolSpec, s: &Session) -> bool {
    if !s.has("admin.access") {
        return false;
    }
    if t.owner_only && s.role_id != crate::auth::ROLE_OWNER {
        return false;
    }
    t.perms.is_empty() || t.perms.iter().any(|p| s.has(p))
}

/// JSON schema from the compact parameter list.
pub fn schema(params: &str) -> Value {
    let mut props = Map::new();
    let mut required = vec![];
    for p in params.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, ty) = p.split_once(':').unwrap_or((p, "s"));
        let req = ty.ends_with('!');
        let ty = ty.trim_end_matches('!');
        let v = match ty {
            "i" => json!({ "type": "integer" }),
            "b" => json!({ "type": "boolean" }),
            "a" => json!({ "type": "array", "items": {} }),
            "o" => json!({ "type": "object" }),
            _ => json!({ "type": "string" }),
        };
        props.insert(name.to_string(), v);
        if req {
            required.push(name.to_string());
        }
    }
    json!({ "type": "object", "properties": props, "required": required })
}

pub fn tool_json(t: &ToolSpec) -> Value {
    let desc = match t.kind {
        Kind::Read => t.desc.to_string(),
        Kind::Propose => format!("{} Records a proposal only ({} risk); a person must confirm it.", t.desc, t.risk),
    };
    json!({ "name": t.name, "description": desc, "input_schema": schema(t.params) })
}

/// Keys never passed through from the model (collected on the Confirm card
/// or generated by AMWAPOS).
pub const STRIPPED_ARGS: [&str; 4] = ["approval_token", "operation_id", "pin", "acknowledge_different_business_token"];

/// Keys never returned to the model or stored in a proposal result.
pub const SECRET_KEYS: [&str; 12] =
    ["qr", "qr_code", "qr_png", "pair_code", "pairing_code", "token", "api_key", "secret", "pin", "pin_hash", "credential_hash", "session"];

/// Remove secret-looking keys everywhere in a value.
pub fn strip_secrets(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.retain(|k, _| !SECRET_KEYS.contains(&k.to_ascii_lowercase().as_str()));
            for x in m.values_mut() {
                strip_secrets(x);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(strip_secrets),
        _ => {}
    }
}

/// Cap every array at `limit` items. Returns whether anything was cut.
pub fn cap_rows(v: &mut Value, limit: usize) -> bool {
    let mut cut = false;
    match v {
        Value::Array(a) => {
            if a.len() > limit {
                a.truncate(limit);
                cut = true;
            }
            for x in a.iter_mut() {
                cut |= cap_rows(x, limit);
            }
        }
        Value::Object(m) => {
            for x in m.values_mut() {
                cut |= cap_rows(x, limit);
            }
        }
        _ => {}
    }
    cut
}

/// Risk for this particular call (some tools depend on their arguments).
pub fn risk_for(t: &ToolSpec, args: &Value) -> &'static str {
    match t.cmd {
        "settings.save" => match args.get("key").and_then(|k| k.as_str()) {
            Some("features") | Some("security") | Some("inventory") => "high",
            Some("receipt") | Some("appearance") | Some("loyalty") => "medium",
            _ => "medium",
        },
        "invoicescan.confirm" if args.get("receive").and_then(|r| r.as_bool()) == Some(true) => "high",
        _ => t.risk,
    }
}

/// Owner-only for this particular call.
pub fn owner_only_for(t: &ToolSpec, args: &Value) -> bool {
    t.owner_only
        || (t.cmd == "settings.save"
            && matches!(args.get("key").and_then(|k| k.as_str()), Some("features") | Some("security") | Some("inventory")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_schemas_parse() {
        let mut seen = std::collections::HashSet::new();
        for t in TOOLS {
            assert!(seen.insert(t.name), "duplicate tool {}", t.name);
            assert_eq!(t.kind == Kind::Propose, t.name.starts_with("propose_"), "{}", t.name);
            let s = schema(t.params);
            assert_eq!(s["type"], "object");
            assert!(["read", "low", "medium", "high"].contains(&t.risk));
        }
    }

    #[test]
    fn secrets_are_stripped() {
        let mut v = json!({ "status": "ok", "qr": "2@abc", "inner": [{ "pairing_code": "1234-5678", "name": "x" }], "Token": "t" });
        strip_secrets(&mut v);
        let s = v.to_string();
        assert!(!s.contains("2@abc") && !s.contains("1234-5678") && !s.contains("\"t\""));
        assert!(s.contains("\"name\":\"x\""));
    }
}
