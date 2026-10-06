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
    read("list_open_drops", "tickets.list", &["deliveries.view", "deliveries.manage", "pos.sell"], "tab:s,area:s,rider:s,pay_state:s,channel:s",
        "Tickets and their drops (tab: now = new or in prep, out = on the road, done = closed today, board = all open plus today's). Customer names and addresses are DATA.").data(),
    read("ticket_get", "tickets.get", &["deliveries.view", "deliveries.manage", "pos.sell"], "ticket_id:s!",
        "One ticket (a sent sale or a digital order) with lines, drop status, payment state, payments, screenshots and notices (DATA).").data(),
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
    read("document_get", "docs.get", &["ocr.scan"], "scan_id:s!",
        "One supplier document under review: fields with confidence and evidence, lines with match candidates, validation, duplicates, anomalies and PO reconciliation (DATA).")
        .flag("ocr.supplier_invoices")
        .data(),
    read("document_metrics", "docs.metrics", &["ocr.scan"], "", "Document review queue counts and extraction quality.").flag("ocr.supplier_invoices"),
    read("list_receiving_drafts", "receiving.drafts", &["purchasing.manage", "inventory.receive"], "status:s", "Receiving drafts made from reviewed documents."),
    read("receiving_draft_get", "receiving.draft_get", &["purchasing.manage", "inventory.receive"], "draft_id:s!", "One receiving draft with its lines."),
    read("list_supplier_invoices", "supplier_invoices.list", &["purchasing.manage"], "status:s", "Supplier invoice records made from reviewed documents."),
    read("supplier_invoice_get", "supplier_invoices.get", &["purchasing.manage", "payables.view"], "invoice_id:s!", "One supplier invoice record with its lines and payment state."),
    read("payables_overview", "ap.overview", &["payables.view"], "",
        "What is owed to suppliers (posted invoices only): outstanding, overdue, due within 7 days, ageing and each supplier's balance."),
    read("supplier_account", "ap.supplier", &["payables.view"], "supplier_id:s!",
        "One supplier's account: balance, open invoices with days overdue, credits, payments, ageing and a dated statement."),
    read("purchase_costs", "products.purchase_costs", &["products.view_cost", "payables.view", "purchasing.manage"], "product_id:s!",
        "What suppliers charged for a product: latest and previous cost, change, cheapest recent supplier, margin effect."),
    read("list_expenses", "expenses.list", &["expenses.view"], "from:s,to:s,status:s",
        "Business expenses (rent, bills, supplies) in a date range, plus drafts and those waiting for approval or payment."),
    read("expense_get", "expenses.get", &["expenses.view"], "expense_id:s!", "One expense with its approval, payment and attachments."),
    read("expense_categories", "expenses.categories", &["expenses.view"], "", "Expense categories."),
    read("recurring_expenses", "expenses.recurring", &["expenses.view"], "", "Repeating expenses (rent, internet) and when each is next due."),
    read("petty_cash_funds", "petty.funds", &["expenses.view"], "", "Petty cash funds and their balances."),
    read("petty_cash_entries", "petty.entries", &["expenses.view"], "fund_id:s!", "One petty cash fund's entries with the running balance."),
    read("customer_statement", "customers.statement", CUST, "customer_id:s!,from:s,to:s",
        "A customer's account statement: opening balance, charges, payments, closing balance and ageing.").flag("customers.credit"),
    read("receivables", "customers.receivables", CUST, "as_of:s", "What customers owe on account, aged by days past due.").flag("customers.credit"),
    read("sale_void_check", "sales.void_check", &["refund.create", "pos.void_sale", "sales.view"], "sale_id:s!", "Whether a sale can still be voided, and why not."),
    read("day_current_totals", "day.x", &["day.x_report"], "date:s,branch_id:s",
        "X report: the current totals of a trading day (sales, refunds, voids, VAT, tenders, drawers, expected cash, after-close records). Changes nothing."),
    read("day_close_checks", "day.checks", &["day.x_report"], "date:s,branch_id:s",
        "Why a trading day can or cannot be closed: blocking items, warnings and information."),
    read("day_closes", "day.closes", &["day.x_report"], "branch_id:s,limit:i", "Closed trading days (Z closes), newest first."),
    read("day_close_get", "day.close_get", &["day.x_report"], "close_id:s!", "One Z close exactly as it was closed."),
    read("day_opening", "day.opening", &["day.x_report"], "", "The opening checklist for today: last close, backups, sync, register, float, printer."),
    read("list_cases", "cases.list", &["cases.view"], "status:s", "Cases to look into, such as cash differences (status: open, closed, all)."),
    read("case_get", "cases.get", &["cases.view"], "case_id:s!", "One case with its facts and full history."),
    read("list_registers", "registers.list", &["registers.manage", "day.x_report"], "", "Registers, their drawers, computers and open shifts."),
    read("product_batches", "lots.product", INV, "product_id:s!",
        "A product's batches: received, removed by recorded waste or counts, estimated sold (first-expiring-first-out, an estimate, not observed), balance, expiry state; plus stock in no batch."),
    read("batch_get", "lots.get", INV, "lot_id:s!", "One batch with its recorded movements and corrections."),
    read("expiring_stock", "expiry.overview", INV, "category_id:s,supplier_id:s,product_id:s",
        "Batches by expiry state (expired, urgent, soon, later) with quantity, value, daily sales, likely left at expiry and price-reduction scenarios (suggestions only)."),
    read("days_of_stock_left", "stock.cover", INV, "window_days:i,category_id:s,product_id:s,search:s",
        "Days of stock left per product: available stock ÷ daily net sales over 7/30/60/90 days, stock-out date, and why when it cannot be said."),
    read("list_waste", "waste.list", INV, "from:s,to:s,reason:s", "Waste recorded in a date range (reversed records marked)."),
    read("waste_get", "waste.get", INV, "waste_id:s!", "One waste record."),
    read("waste_summary", "waste.summary", &["products.view_cost"], "from:s,to:s",
        "Waste at cost by reason, product, category, supplier and day, with % of net sales and % of goods received (defined in the answer)."),
    read("suggested_orders", "replenish.list", &["inventory.view", "requisitions.create", "purchasing.manage"], "states:a,supplier_id:s,product_ids:a,search:s",
        "Suggested orders from the replenishment engine: per product the stock position (usable, on order, transfers, requested), demand, reorder point, order-up-to, supplier chosen (with alternatives), packs and quantity, with reason codes and states (order, covered, insufficient_history, no_demand, no_supplier, no_lead_time, inactive, invalid_pack). Computed by the app; nothing is ordered."),
    read("supplier_catalogue", "supplier.catalogue", &["suppliers.manage", "purchasing.manage", "requisitions.create"], "supplier_id:s,product_id:s",
        "A supplier's products (or a product's suppliers) with the terms: pack size and who confirmed it, minimum order, lead time, preferred, document evidence and cost baselines."),
    read("list_requisitions", "requisitions.list", &["requisitions.create", "purchasing.approve", "purchasing.manage"], "status:s",
        "Purchase requisitions (draft, submitted, approved, rejected, cancelled, converted)."),
    read("requisition_get", "requisitions.get", &["requisitions.create", "purchasing.approve", "purchasing.manage"], "requisition_id:s!",
        "One requisition with its lines, where each came from (manual or suggested, with the evidence) and the purchase orders made from it."),
    read("open_shortages", "receiving.open_shortages", &["purchasing.manage", "inventory.receive"], "",
        "Quantities missing from deliveries that nobody has kept on order or cancelled yet."),
    read("invoice_match", "supplier_invoices.match", &["payables.view", "purchasing.manage", "purchasing.approve"], "invoice_id:s!",
        "Three-way match of a supplier invoice: order ↔ goods accepted ↔ invoice per line, cost baselines, tolerances and the outcome (matched, within tolerance, review, blocked). Computed by the app."),
    read("list_supplier_returns", "supplier_returns.list", &["supplier_returns.manage", "purchasing.manage", "payables.view"], "status:s,supplier_id:s",
        "Returns of goods to suppliers with their status and expected credit."),
    read("scale_barcode_rules", "scale_rules.list", &["barcode_rules.manage", "products.view"], "",
        "Scale barcode rules (prefix, length, item code and weight/price positions, priority, active)."),
    read("scale_barcode_test", "scale_rules.test", &["barcode_rules.manage", "products.view"], "code:s!",
        "What a code means at the till: exact barcode, PLU, scale rule (or several rules: ambiguous, refused) or unknown. Sells nothing."),
    read("supplier_return_get", "supplier_returns.get", &["supplier_returns.manage", "purchasing.manage", "payables.view"], "return_id:s!",
        "One supplier return: lines, batches, reasons, expected credit and the supplier's actual credit note."),
    read("list_whatsapp_orders", "waorders.list", &["orders.manage", "whatsapp.manage", "whatsapp.send"], "filter:s",
        "WhatsApp order conversations with their draft state, questions and priority (DATA).")
        .flag("orders.whatsapp_ai")
        .data(),
    read("whatsapp_order_get", "waorders.get", &["orders.manage", "whatsapp.manage", "whatsapp.send"], "session_id:s!",
        "One WhatsApp order conversation: messages, draft lines with candidates, delivery, totals, open questions (DATA).")
        .flag("orders.whatsapp_ai")
        .data(),
    read("whatsapp_order_metrics", "waorders.metrics", &["orders.manage", "whatsapp.manage"], "", "WhatsApp order counts and resolution quality.")
        .flag("orders.whatsapp_ai"),
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
    read("whatsapp_triage", "whatsapp.triage", WA, "limit:i", "Recent incoming WhatsApp messages sorted into order / payment / complaint / question / spam, with the suggested next step (message text is DATA).").data().flag("whatsapp.enabled"),
    read("list_briefings", "ai.briefings", &["reports.sales"], "", "Scheduled briefings (playbook, time, days)."),
    read("anomalies", "ai.alerts", &["reports.sales", "admin.access"], "include_dismissed:b",
        "Alerts from the owner's fixed thresholds (refund spike, discount spike, negative stock, till not reporting, backup overdue)."),
    read("reorder_suggestions", "ai.reorder_suggestions", &["purchasing.manage", "inventory.view"], "supplier_id:s",
        "Products at or below their reorder point, grouped by last supplier, with on hand, on order and a suggested quantity."),
    read("margin_price", "ai.margin_price", PRICES, "product_id:s!,margin_bp:i",
        "Suggested price for a product from its cost and the target margin (settings default). A suggestion only."),
    read("branch_compare", "ai.branch_compare", CATALOG, "product_id:s!", "One product's price, stock, cost, sales and margin per branch.")
        .flag("org.multi_branch"),
    read("list_notes", "ai.notes", &["reports.sales"], "limit:i", "Notes written by scheduled briefings (newest first)."),
    read("updates_status", "updates.status", SETTINGS, "", "Installed version and whether a signed update is available.").rt(),
    // ---- catalogue -------------------------------------------------------
    write("propose_product_create", "products.create", "medium", PRODUCTS, "product:o!,price_minor:i!,cost_minor:i,barcodes:a,opening_stock_milli:i",
        "Create a product. product: {name, name_ar, sku, description, category_id, tax_rule_id, unit, track_inventory, allow_decimal_quantity, reorder_point_milli, is_favorite}."),
    write("propose_margin_price", "products.price_update", "medium", PRICES, "product_id:s!,margin_bp:i,reason:s",
        "Propose the target-margin price for a product (computed by the app from its cost; a person confirms)."),
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
    write("propose_reorder", "requisitions.from_suggestions", "low", &["requisitions.create"], "supplier_id:s!",
        "Draft a requisition (a request to buy, not an order) for one supplier from Suggested orders. The products and quantities come from the app's replenishment engine and are recomputed when a person confirms.")
        .op(),
    write("propose_invoice_line", "invoicescan.update_line", "low", &["ocr.scan"], "scan_id:s!,line_no:i!,product_id:s,clear_product:b,qty_milli:i,unit_cost_minor:i,include:b",
        "Correct one line of a scanned invoice.").flag("ocr.supplier_invoices"),
    write("propose_invoice_confirm", "invoicescan.confirm", "medium", &["ocr.scan"], "scan_id:s!,supplier_id:s!,receive:b",
        "Turn a reviewed invoice scan into a PO (receive=true also receives the stock: high risk).").flag("ocr.supplier_invoices"),
    write("propose_invoice_reject", "invoicescan.reject", "low", &["ocr.scan"], "scan_id:s!,reason:s!", "Reject an invoice scan.").flag("ocr.supplier_invoices"),
    write("propose_invoice_ai_parse", "invoicescan.ai_parse", "low", &["ocr.scan"], "scan_id:s!",
        "Ask the AI provider to re-read an invoice scan's lines (the review screen still applies).").flag("ocr.ai_parse").rt(),
    write("propose_document_line", "docs.update_line", "low", &["ocr.scan"],
        "scan_id:s!,revision:i!,line_no:i!,product_id:s,clear_product:b,qty_milli:i,unit_cost_minor:i,units_per_case:i,include:b",
        "Correct one line of a document under review (nothing is received or posted).").flag("ocr.supplier_invoices"),
    write("propose_whatsapp_order_line", "waorders.line", "low", &["orders.manage"], "session_id:s!,revision:i!,line_no:i,product_id:s,qty_milli:i,remove:b",
        "Correct one line of a WhatsApp order draft (the order is not confirmed).").flag("orders.whatsapp_ai"),
    write("propose_ocr_retry", "ocr.retry", "low", &["ocr.scan", "payments.review"], "kind:s!,id:s!", "Run OCR again (kind: invoice | payment)."),
    // ---- customers -------------------------------------------------------
    write("propose_customer_save", "customers.save", "low", CUSTM, "customer_id:s,customer:o!",
        "Create or edit a customer. customer: {name, phone, whatsapp, email, area, address, active}."),
    write("propose_customer_note", "customers.add_note", "low", CUSTM, "customer_id:s!,note:s!", "Add a note to a customer."),
    write("propose_customer_address", "customers.address_save", "low", CUSTM, "address_id:s,customer_id:s!,label:s!,area:s,address:s!,notes:s,is_default:b",
        "Add or edit a customer's delivery address."),
    write("propose_customer_address_delete", "customers.address_delete", "low", CUSTM, "address_id:s!", "Delete a customer address."),
    write("propose_loyalty_adjust", "loyalty.adjust", "medium", &["loyalty.adjust"], "customer_id:s!,points:i!,note:s!", "Add (+) or remove (−) loyalty points.").op()
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
    write("propose_briefing_save", "ai.briefing_save", "low", &["reports.sales"], "briefing_id:s,briefing:o!",
        "Schedule a briefing. briefing: {name, playbook: eod|cash_short|reorder|refund_spike, at_time: HH:MM, days: e.g. 1234567, with_ai, enabled}. Runs while the app is open."),
    write("propose_briefing_delete", "ai.briefing_delete", "low", &["reports.sales"], "briefing_id:s!", "Delete a scheduled briefing."),
    write("propose_triage_set", "whatsapp.triage_set", "low", WA, "seq:i!,category:s!", "Correct the category of an incoming WhatsApp message.").flag("whatsapp.enabled"),
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
/// Fields a proposal may never carry, per command: a person sets these
/// (Wave 5: PLUs, structured address details, the sale's channel).
pub fn forbidden_keys(cmd: &str) -> &'static [&'static str] {
    match cmd {
        "products.update" | "products.create" => &["plu"],
        "customers.save" | "customers.address_save" => &["governorate", "directions"],
        "orders.save" => &["channel"],
        _ => &[],
    }
}

/// The first forbidden field found in `input` (top level or one object down).
pub fn forbidden_key_in(cmd: &str, input: &serde_json::Value) -> Option<&'static str> {
    let keys = forbidden_keys(cmd);
    let has = |v: &serde_json::Value, k: &str| v.get(k).map(|x| !x.is_null()).unwrap_or(false);
    keys.iter()
        .find(|k| has(input, k) || input.as_object().map(|o| o.values().any(|v| v.is_object() && has(v, k))).unwrap_or(false))
        .copied()
}

pub const NO_TOOL: &[(&str, &str)] = &[
    ("app.ping", "internal"),
    ("setup.status", "setup wizard only"),
    ("setup.initialize", "setup wizard only"),
    ("auth.users", "forbidden: sign-in"),
    ("ai.alert_dismiss", "a person dismisses alerts in the inbox"),
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
    ("tickets.counts", "the till's Send badge"),
    ("orders.flow", "the order pages' guide bar"),
    ("tickets.record_payment", "money is recorded by a person on the ticket sheet, at the till or the door"),
    ("tickets.unable", "a rider or cashier says why a drop failed, on the ticket sheet"),
    ("tickets.not_delivered", "refunds a sale and moves stock; a manager closes it on the ticket sheet"),
    ("riders.cash", "cash custody is counted in person at the till"),
    ("riders.handover", "cash is counted into a drawer by a person at the till"),
    ("products.images", "binary picture data for screens; not useful to the assistant"),
    ("products.image_state", "picture state is shown on the product page"),
    ("products.image_upload", "a person chooses and uploads a product picture"),
    ("products.image_remove", "a person removes a product picture on the product page"),
    ("products.image_find", "a person asks for the one-time picture search on the product page"),
    ("products.image_backfill", "starts outside lookups; a person runs it from settings"),
    ("products.image_overview", "picture search settings and counts for the settings page"),
    ("products.image_configure", "search sources and the key are set by a person in settings"),
    ("customers.block_area", "a lookup for the address form; the assistant reads areas from drops"),
    ("whatsapp.link_customer", "a person links a chat to a customer from the chat header"),
    ("whatsapp.thread_context", "the WhatsApp chat header; the assistant uses whatsapp_thread and ticket reads"),
    ("whatsapp.phone_contacts", "personal names and numbers from the shop's phone; reviewed on the Customers page"),
    ("whatsapp.phone_contacts_import", "a person imports on the Customers page after reviewing the list"),
    ("whatsapp.phone_contacts_refresh", "the Customers page asks WhatsApp to resend the phone's contacts"),
    ("whatsapp.catalog_status", "the WhatsApp Catalogue tab reads the catalogue connection and publishing counts"),
    ("whatsapp.catalog_sync", "publishing the catalogue to WhatsApp is an owner decision taken on the WhatsApp page"),
    ("whatsapp.catalog_retry", "retrying catalogue publishing is done on the WhatsApp page"),
    ("whatsapp.catalog_configure", "keeping the WhatsApp catalogue synchronised is set on the WhatsApp page"),
    ("whatsapp.catalog_recheck", "re-checking the WhatsApp catalogue capability is a button on the WhatsApp page"),
    ("whatsapp.catalog_product", "the product editor shows one product's WhatsApp catalogue state"),
    ("migration.read", "needs files chosen in the page"),
    ("migration.preview", "needs files chosen in the page"),
    ("migration.apply", "needs files chosen in the page"),
    ("payreviews.upload", "needs an image chosen in the page"),
    ("invoicescan.import", "needs an image chosen in the page"),
    ("docs.import", "needs a file chosen in the page"),
    ("docs.from_inbox", "a person picks the WhatsApp attachment on the Documents page"),
    ("docs.page", "page images are shown on the review screen, not sent to the model"),
    ("docs.update", "document header corrections are made on the review screen"),
    ("docs.new_product", "a new catalogue product from a document is a person's decision on the review screen"),
    ("docs.create_supplier_invoice", "creating a supplier invoice record is a person's action on the review screen"),
    ("docs.create_receiving", "creating a receiving draft is a person's action on the review screen"),
    ("receiving.draft_update_line", "receiving quantities are checked by a person on the receiving screen"),
    ("receiving.draft_cancel", "a person cancels a receiving draft on the receiving screen"),
    ("receiving.draft_post", "forbidden: receiving stock is a person's action"),
    ("supplier_invoices.set_status", "forbidden: approving a supplier invoice is a person's action"),
    ("waorders.delivery", "delivery mode, address and zone are set on the WhatsApp Orders screen"),
    ("waorders.customer", "linking a customer is done on the WhatsApp Orders screen"),
    ("waorders.flags", "takeover and handled flags are a person's call on the WhatsApp Orders screen"),
    ("ap.invoice_create", "a person enters supplier invoices on the Payables page"),
    ("ap.invoice_approve", "a person reviews supplier invoices on the Payables page"),
    ("ap.invoice_post", "forbidden: only a person posts what is owed to a supplier"),
    ("ap.invoice_reverse", "forbidden: only a person reverses posted supplier records"),
    ("ap.payment_record", "forbidden: only a person records supplier payments"),
    ("ap.payment_reverse", "forbidden: only a person reverses supplier payments"),
    ("ap.allocate", "forbidden: only a person allocates supplier payments"),
    ("ap.allocation_reverse", "forbidden: only a person changes payment allocations"),
    ("sales.void", "forbidden: only a person at the till voids a sale, with a manager when needed"),
    ("customers.statement_pdf", "a person downloads or sends statements from the customer page"),
    ("customers.terms_set", "forbidden: only a person sets a customer's payment terms"),
    ("expenses.save", "a person enters expenses on the Expenses page"),
    ("expenses.delete_draft", "a person deletes draft expenses"),
    ("expenses.submit", "a person submits expenses for approval"),
    ("expenses.decide", "forbidden: only a person approves or rejects expenses"),
    ("expenses.pay", "forbidden: only a person records expense payments"),
    ("expenses.void", "forbidden: only a person voids expenses"),
    ("expenses.attach", "a person attaches bills on the Expenses page"),
    ("expenses.attachment", "file download in the Expenses page"),
    ("expenses.category_save", "a person edits expense categories"),
    ("expenses.recurring_save", "a person sets up repeating expenses"),
    ("expenses.unlinked_paid_outs", "covered by the Expenses pay dialog"),
    ("petty.fund_save", "forbidden: only a person opens or closes petty cash funds"),
    ("petty.entry", "forbidden: only a person moves petty cash"),
    ("petty.count", "forbidden: only a person counts petty cash"),
    ("day.close", "forbidden: only a person closes a trading day"),
    ("day.close_pdf", "a person downloads closes from End of day"),
    ("day.x_pdf", "a person downloads the X report from End of day"),
    ("cases.act", "forbidden: only a person acknowledges, resolves or dismisses a case"),
    ("cases.open_for_shift", "a person opens a case from the drawer"),
    ("cases.attach", "a person adds evidence to a case"),
    ("cases.evidence", "file download in the case drawer"),
    ("registers.save", "forbidden: only a person changes registers"),
    ("registers.devices", "covered by list_registers"),
    ("drawers.save", "forbidden: only a person changes cash drawers"),
    ("waste.record", "forbidden: only a person records waste"),
    ("waste.reverse", "forbidden: only a person reverses waste"),
    ("lots.count_in", "forbidden: only a person counts stock into a batch"),
    ("lots.correct", "forbidden: only a person changes batch details"),
    ("lots.product_settings", "a person sets batch tracking on the product"),
    ("supplier.terms_save", "forbidden: only a person changes supplier terms"),
    ("products.max_stock", "forbidden: only a person sets stock levels"),
    ("requisitions.create", "drafted only through propose_reorder (from Suggested orders)"),
    ("requisitions.save", "a person edits requisitions"),
    ("requisitions.set_status", "forbidden: only a person submits, approves, rejects or cancels a requisition"),
    ("requisitions.convert", "forbidden: only a person turns a requisition into purchase orders"),
    ("po.approve", "forbidden: only a person approves a purchase order"),
    ("po.set_status", "forbidden: only a person places or cancels a purchase order"),
    ("po.receive", "forbidden: only a person receives goods"),
    ("receiving.shortage_decide", "forbidden: only a person keeps a shortage on order or cancels it"),
    ("supplier_invoices.accept_match", "forbidden: only a person accepts invoice differences"),
    ("barcodes.set_kind", "forbidden: only a person records a barcode's type"),
    ("products.set_plu", "forbidden: only a person sets a PLU"),
    ("scale_rules.save", "forbidden: only a person sets up scale barcode rules"),
    ("supplier_returns.save", "forbidden: only a person prepares a supplier return"),
    ("supplier_returns.cancel", "forbidden: only a person cancels a supplier return"),
    ("supplier_returns.confirm", "forbidden: only a person confirms a supplier return"),
    ("supplier_returns.reverse", "forbidden: only a person reverses a supplier return"),
    ("supplier_returns.draft_credit", "forbidden: only a person creates supplier credits"),
    ("supplier_returns.link_credit", "forbidden: only a person links supplier credits"),
    ("receiving.draft_set_lot", "forbidden: only a person confirms expiry dates"),
    ("ap.payment_get", "covered by supplier_account"),
    ("ap.invoice_get", "covered by supplier_invoice_get"),
    ("waorders.confirm", "forbidden: only a person confirms a WhatsApp order"),
    ("waorders.cancel", "a person cancels a WhatsApp order on the WhatsApp Orders screen"),
    ("waorders.payment", "forbidden: payment evidence is decided by a person"),
    ("waorders.send", "forbidden: only a person sends WhatsApp replies"),
    ("roles.list", "covered by role_permissions"),
    ("roles.permissions", "covered by role_permissions"),
    ("settings.get", "covered by settings_public (secrets excluded)"),
    ("customers.add_note", "covered by propose_customer_note"),
    ("ai.digest", "the AI page's action inbox (pending_proposals is the read)"),
    ("ai.attach_image", "the person attaches photos on the AI page"),
    ("ai.attachment", "the AI page shows the person's own photos"),
    ("ai.conversation_rename", "the person names conversations on the AI page"),
    ("ai.pin", "the person pins records on the AI page"),
    ("ai.unpin", "the person pins records on the AI page"),
    ("ai.note_read", "the AI page marks notes read"),
    ("ai.slash", "slash commands are typed by the person, not the model"),
    ("ai.stream", "the AI page's live view of a question"),
    ("ai.briefing_run", "Run now on the AI page (list_notes reads the result)"),
    ("whatsapp.triage_ai", "the WhatsApp page asks the provider to re-sort messages"),
    ("whatsapp.draft_reply", "the WhatsApp page drafts a reply for a person to edit and send"),
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

// ---- C6: customer details stay in the store --------------------------------

/// Fields that identify a person (not the store): replaced before any text
/// reaches the AI provider. Ids stay, so the model can still call tools.
const PII_KEYS: &[&str] = &[
    "phone",
    "whatsapp",
    "mobile",
    "email",
    "address",
    "address_line",
    "area",
    "landmark",
    "push_name",
    "customer_name",
    "to_phone",
    "customer_phone",
    "contact_phone",
    "contact_name",
    "delivery_address",
    "chat",
    "account",
];

/// Replace phone-number-like runs in free text ("+973 3300 1122", "0097333001122",
/// "97333001122") with "[phone]". Barcodes (no "+", no 00/973 prefix) are kept.
pub fn redact_phones(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        let plus = chars[i] == '+';
        let mut j = if plus { i + 1 } else { i };
        let mut digits = String::new();
        while j < chars.len() && (chars[j].is_ascii_digit() || ((chars[j] == ' ' || chars[j] == '-') && !digits.is_empty())) {
            if chars[j].is_ascii_digit() {
                digits.push(chars[j]);
            }
            j += 1;
        }
        let prev_ok = start == 0 || !chars[start - 1].is_ascii_alphanumeric();
        let phone = prev_ok
            && ((plus && (8..=15).contains(&digits.len()))
                || (digits.starts_with("00") && (10..=16).contains(&digits.len()))
                || (digits.starts_with("973") && digits.len() == 11));
        if phone {
            // Keep a trailing separator that was not part of the number.
            let mut end = j;
            while end > start && !chars[end - 1].is_ascii_digit() {
                end -= 1;
            }
            out.push_str("[phone]");
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn is_person_object(m: &Map<String, Value>, parent: &str) -> bool {
    matches!(parent, "customer" | "customers" | "contact" | "recipient")
        || (m.contains_key("customer_id")
            && !m.contains_key("product_id")
            && !m.contains_key("supplier_id")
            && (m.contains_key("phone") || m.contains_key("whatsapp") || m.contains_key("email")))
}

/// C6: replace customer names, phones and addresses with ids (or "[redacted]")
/// in anything headed for the AI provider.
pub fn redact_customer_pii(v: &mut Value) {
    redact_walk(v, "");
}

fn redact_walk(v: &mut Value, parent: &str) {
    match v {
        Value::Object(m) => {
            let person = is_person_object(m, parent);
            let cid = m.get("customer_id").and_then(|x| x.as_str()).map(str::to_string);
            let label = || match &cid {
                Some(id) => format!("customer:{id}"),
                None => "[redacted]".to_string(),
            };
            for (k, x) in m.iter_mut() {
                let key = k.as_str();
                if (PII_KEYS.contains(&key) || (person && key == "name")) && x.as_str().is_some_and(|s| !s.is_empty()) {
                    *x = Value::String(if key == "name" || key == "customer_name" { label() } else { "[redacted]".into() });
                } else {
                    redact_walk(x, key);
                }
            }
        }
        Value::Array(a) => {
            for x in a {
                redact_walk(x, parent);
            }
        }
        Value::String(s) => {
            // Tool results are often JSON inside a string; phones in free text too.
            if s.len() > 1 && (s.starts_with('{') || s.starts_with('[')) {
                if let Ok(mut inner) = serde_json::from_str::<Value>(s) {
                    redact_walk(&mut inner, parent);
                    *s = inner.to_string();
                    return;
                }
            }
            let r = redact_phones(s);
            if r != *s {
                *s = r;
            }
        }
        _ => {}
    }
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
    fn customer_details_are_replaced_with_ids() {
        let mut v = json!({ "customer": { "customer_id": "C1", "name": "Ali Hasan", "phone": "+97333001122", "address": "Road 12" },
                            "notes": [{ "text": "call me on +973 3300 1122 or 0097333001122" }],
                            "product": { "product_id": "P1", "name": "Tea", "barcode": "6291234567890" } });
        redact_customer_pii(&mut v);
        let s = v.to_string();
        assert!(!s.contains("Ali Hasan") && !s.contains("33001122") && !s.contains("Road 12"), "{s}");
        assert!(s.contains("customer:C1"));
        assert!(s.contains("Tea") && s.contains("6291234567890"), "product data and barcodes stay: {s}");
    }

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
