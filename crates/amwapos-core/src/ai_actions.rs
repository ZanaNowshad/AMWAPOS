//! AI action-risk model: every command the AI can reach is classified here,
//! once, and the class — not prompt wording — decides what may happen.
//!
//! | Class | May the AI run it? |
//! |---|---|
//! | `Read`, `Analyze`, `Suggest` | Yes, immediately, as the signed-in user (normal permissions). |
//! | `Draft` | Only as a proposal a person confirms (chat), or by an automated workflow that is allowed drafts. |
//! | `ReversibleWrite`, `CommitRecord` | Only as a proposal a person confirms. |
//! | `CommitFinancial`, `CommitInventory`, `ExternalCommunication` | Only as a proposal a person with the command's own permission confirms; the command's approval / step-up checks run on Confirm. |
//!
//! Enforcement points (all in the backend):
//! - `ai.rs` refuses to run a non-automatic class as a read, including every
//!   command a composed ("virtual") read dispatches internally: composing
//!   reads can never reach a write.
//! - A proposal stores its class; Confirm re-derives the class from the
//!   command and arguments and refuses when it differs (a stored proposal
//!   cannot be downgraded) or when the command is unknown.
//! - Automated workflows (WhatsApp orders, documents, briefings) declare the
//!   highest class they may reach (`AUTOMATED`); none reaches beyond `Draft`.
//! - Unknown commands classify as `None` and are refused (fail closed).

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    /// Look something up.
    Read,
    /// Compute over data already readable (reorder suggestions, comparisons).
    Analyze,
    /// Produce a candidate that changes nothing authoritative.
    Suggest,
    /// Create or edit an incomplete, reviewable record (draft PO / order / document line).
    Draft,
    /// Small, undoable state (a note, a triage flag, a retry).
    ReversibleWrite,
    /// Authoritative non-monetary records (products, suppliers, users, settings, devices).
    CommitRecord,
    /// Money: prices, costs, tax, payments, refunds, credit, cash, supplier payables.
    CommitFinancial,
    /// Stock: receiving, adjustments, transfers, stock-take finalization.
    CommitInventory,
    /// Anything that leaves the store (WhatsApp messages).
    ExternalCommunication,
}

impl ActionClass {
    /// May run without a person confirming it.
    pub fn automatic(self) -> bool {
        matches!(self, ActionClass::Read | ActionClass::Analyze | ActionClass::Suggest)
    }

    /// Money, stock or outside communication: the strongest confirmation rules.
    pub fn consequential(self) -> bool {
        matches!(self, ActionClass::CommitFinancial | ActionClass::CommitInventory | ActionClass::ExternalCommunication)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ActionClass::Read => "read",
            ActionClass::Analyze => "analyze",
            ActionClass::Suggest => "suggest",
            ActionClass::Draft => "draft",
            ActionClass::ReversibleWrite => "reversible_write",
            ActionClass::CommitRecord => "commit_record",
            ActionClass::CommitFinancial => "commit_financial",
            ActionClass::CommitInventory => "commit_inventory",
            ActionClass::ExternalCommunication => "external_communication",
        }
    }

    /// The minimum proposal risk a class carries.
    pub fn min_risk(self) -> &'static str {
        match self {
            ActionClass::CommitFinancial | ActionClass::CommitInventory | ActionClass::ExternalCommunication => "medium",
            _ => "low",
        }
    }
}

/// Automated (not chat) AI workflows and the highest class each may reach.
/// Their mutations are drafts a person reviews; nothing else is reachable
/// from their code paths.
pub const AUTOMATED: &[(&str, ActionClass)] = &[
    // Reads a WhatsApp message, resolves against the real catalogue, edits a draft digital order.
    ("whatsapp_orders.interpret", ActionClass::Draft),
    // Reads a supplier document, suggests fields and product matches on the review record.
    ("documents.ai_assist", ActionClass::Suggest),
    // Sorts the WhatsApp inbox (a triage label).
    ("whatsapp.triage_ai", ActionClass::Suggest),
    // Drafts a reply a person edits and sends.
    ("whatsapp.draft_reply", ActionClass::Draft),
    // Scheduled briefings: reads and writes a note.
    ("ai.briefing_run", ActionClass::Analyze),
];

/// May an automated workflow perform an action of this class?
pub fn automated_may(workflow: &str, class: ActionClass) -> bool {
    AUTOMATED.iter().any(|(w, max)| *w == workflow && class <= *max && class <= ActionClass::Draft)
}

/// The class of a command as the AI would call it (`None`: not reachable by
/// the AI at all). Arguments matter for a few commands.
pub fn class_of(cmd: &str, args: &Value) -> Option<ActionClass> {
    use ActionClass::*;
    let key = args.get("key").and_then(|k| k.as_str());
    Some(match cmd {
        // ---- money --------------------------------------------------------
        "products.price_update"
        | "products.bulk_price"
        | "products.cost_update"
        | "branches.set_price"
        | "tax.create"
        | "tax.set_active"
        | "loyalty.adjust"
        | "customers.account_set"
        | "customers.account_payment"
        | "customers.account_adjust"
        | "orders.set_payment"
        | "orders.convert"
        | "payreviews.decide"
        | "refunds.create"
        | "cash.event"
        | "ap.invoice_post"
        | "ap.invoice_reverse"
        | "ap.payment_record"
        | "ap.payment_reverse"
        | "ap.allocate"
        | "ap.allocation_reverse"
        | "legacy.price_change" => CommitFinancial,
        "ap.invoice_create" | "ap.invoice_approve" | "supplier_invoices.set_status" => Draft,
        // ---- stock --------------------------------------------------------
        "inventory.receive"
        | "inventory.adjust"
        | "stocktake.finalize"
        | "transfers.ship"
        | "transfers.receive"
        | "po.receive"
        | "barcodes.unknown_merge"
        | "receiving.draft_post"
        | "legacy.stock_adjustment" => CommitInventory,
        "invoicescan.confirm" if args.get("receive").and_then(|r| r.as_bool()) == Some(true) => CommitInventory,
        // ---- leaves the store ---------------------------------------------
        "whatsapp.queue" | "whatsapp.outbox_action" | "waorders.reply" => ExternalCommunication,
        // Settings that change what is sent or who may do what.
        "settings.save" if matches!(key, Some("features") | Some("security") | Some("inventory")) => CommitRecord,
        // ---- drafts -------------------------------------------------------
        "po.save"
        | "orders.save"
        | "orders.from_inbox"
        | "stocktake.create"
        | "stocktake.count"
        | "transfers.create"
        | "invoicescan.update_line"
        | "invoicescan.confirm"
        | "invoicescan.ai_parse"
        | "docs.update_line"
        | "waorders.line"
        | "reports.preset_save"
        | "ai.briefing_save"
        | "legacy.purchase_order" => Draft,
        // ---- small, undoable ----------------------------------------------
        "barcodes.set_primary"
        | "barcodes.unknown_dismiss"
        | "barcodes.unknown_reopen"
        | "categories.save"
        | "invoicescan.reject"
        | "ocr.retry"
        | "customers.add_note"
        | "sales.reprint"
        | "print.retry"
        | "print.test"
        | "print.drawer_test"
        | "devices.rename"
        | "reports.preset_delete"
        | "backup.create"
        | "sync.run_now"
        | "updates.check"
        | "ai.briefing_delete"
        | "whatsapp.triage_set"
        | "companion.revoke"
        | "whatsapp.mark_read"
        | "transfers.cancel"
        | "locations.save"
        | "payreviews.set_expected"
        | "stocktake.set_status" => ReversibleWrite,
        // ---- authoritative records ----------------------------------------
        "products.create"
        | "products.update"
        | "products.set_active"
        | "products.bulk_set_active"
        | "products.import_apply"
        | "barcodes.add"
        | "barcodes.remove"
        | "categories.archive"
        | "suppliers.save"
        | "po.set_status"
        | "customers.save"
        | "customers.address_save"
        | "customers.address_delete"
        | "deliveries.create"
        | "deliveries.update"
        | "orders.confirm"
        | "orders.cancel"
        | "users.create"
        | "users.update"
        | "users.unlock"
        | "roles.save"
        | "branches.user_set"
        | "branches.save"
        | "branches.switch"
        | "devices.set_active"
        | "settings.save"
        | "business.update"
        | "backup.restore"
        | "sync.set_bind_address"
        | "sync.enable_hub"
        | "sync.unblock"
        | "sync.retry_dead_letter"
        | "companion.issue"
        | "whatsapp.import_contacts"
        | "whatsapp.start"
        | "whatsapp.stop"
        | "whatsapp.logout"
        | "whatsapp.session_backup" => CommitRecord,
        // ---- computed over readable data ----------------------------------
        "ai.alerts"
        | "ai.reorder_suggestions"
        | "ai.margin_price"
        | "ai.branch_compare"
        | "whatsapp.triage"
        | "refunds.preview"
        | "receipts.preview"
        | "virtual.supplier_performance"
        | "reports.run"
        | "legacy.run_report" => Analyze,
        // ---- reads --------------------------------------------------------
        c if is_read_command(c) => Read,
        _ => return None,
    })
}

/// Read-only commands the AI may run (the names of commands that only look
/// things up). Anything not listed is not a read.
fn is_read_command(c: &str) -> bool {
    const READS: &[&str] = &[
        "dashboard.get",
        "sales.get",
        "sales.find_receipt",
        "sales.list",
        "refunds.lookup",
        "refunds.list",
        "virtual.refund_get",
        "print.queue",
        "print.printers",
        "shift.get",
        "shift.current",
        "shift.list",
        "cash.list",
        "categories.list",
        "virtual.category_get",
        "tax.list",
        "barcodes.unknown_list",
        "virtual.inventory_get",
        "inventory.movements",
        "stocktake.list",
        "stocktake.get",
        "locations.list",
        "locations.stock",
        "transfers.list",
        "transfers.get",
        "transfers.in_transit",
        "suppliers.list",
        "suppliers.get",
        "po.list",
        "po.get",
        "invoicescan.list",
        "customers.search",
        "customers.get",
        "virtual.customer_history",
        "customers.account",
        "deliveries.list",
        "deliveries.get",
        "tickets.list",
        "tickets.get",
        "orders.list",
        "payreviews.list",
        "payreviews.get",
        "whatsapp.conversations",
        "whatsapp.thread",
        "whatsapp.outbox",
        "whatsapp.summary",
        "whatsapp.status",
        "docs.get",
        "docs.metrics",
        "receiving.drafts",
        "receiving.draft_get",
        "supplier_invoices.list",
        "supplier_invoices.get",
        "waorders.list",
        "waorders.get",
        "waorders.metrics",
        "ocr.status",
        "users.list",
        "virtual.role_permissions",
        "roles.list",
        "roles.permissions",
        "devices.list",
        "sync.status",
        "sync.dead_letters",
        "sync.hub_addresses",
        "diagnostics.get",
        "audit.verify",
        "virtual.feature_flags",
        "backup.health",
        "backup.list",
        "backup.inspect",
        "audit.list",
        "virtual.settings_public",
        "settings.get",
        "business.get",
        "branches.list",
        "branches.prices",
        "branches.user_get",
        "reports.presets",
        "reports.catalog",
        "companion.tokens",
        "ai.proposals",
        "ai.briefings",
        "ai.notes",
        "updates.status",
        "products.get",
        "products.search",
        "ap.overview",
        "ap.supplier",
        "ap.invoice_get",
        "ap.payment_get",
        "products.purchase_costs",
        "attention.list",
        "search.global",
        "legacy.search_products",
        "legacy.low_stock",
        "legacy.product_details",
        "legacy.recent_whatsapp_messages",
        "legacy.invoice_scan_text",
        "legacy.eod_pack",
        "legacy.branch_context",
        "legacy.loyalty_balance",
        "legacy.digital_order_get",
        "legacy.list_reports",
    ];
    READS.contains(&c)
}

/// The pseudo-command of a legacy assistant tool (defined in `ai.rs`).
pub fn legacy_command(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "list_reports" => "legacy.list_reports",
        "run_report" => "legacy.run_report",
        "search_products" => "legacy.search_products",
        "low_stock" => "legacy.low_stock",
        "product_details" => "legacy.product_details",
        "recent_whatsapp_messages" => "legacy.recent_whatsapp_messages",
        "invoice_scan_text" => "legacy.invoice_scan_text",
        "eod_pack" => "legacy.eod_pack",
        "branch_context" => "legacy.branch_context",
        "loyalty_balance" => "legacy.loyalty_balance",
        "digital_order_get" => "legacy.digital_order_get",
        "propose_price_change" => "legacy.price_change",
        "propose_stock_adjustment" => "legacy.stock_adjustment",
        "propose_purchase_order" => "legacy.purchase_order",
        _ => return None,
    })
}

/// Guard for anything the AI runs without a person: refuses every class that
/// needs a confirmation (and unknown commands).
pub fn require_automatic(cmd: &str, args: &Value) -> crate::AppResult<ActionClass> {
    match class_of(cmd, args) {
        Some(c) if c.automatic() => Ok(c),
        Some(c) => Err(crate::AppError::forbidden("ai.mutate").with_details(serde_json::json!({
            "kind": "ai_action_needs_confirmation", "command": cmd, "class": c.as_str()
        }))),
        None => Err(crate::AppError::forbidden("ai.use").with_details(serde_json::json!({ "kind": "ai_action_unknown", "command": cmd }))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_tools::{Kind, TOOLS};
    use serde_json::json;

    #[test]
    fn every_tool_has_a_class_and_reads_are_automatic() {
        for t in TOOLS {
            let c = class_of(t.cmd, &json!({})).unwrap_or_else(|| panic!("{} ({}) has no action class", t.name, t.cmd));
            match t.kind {
                Kind::Read => assert!(c.automatic(), "{} is offered as a read but is {}", t.name, c.as_str()),
                Kind::Propose => assert!(!c.automatic(), "{} is a proposal but classified {}", t.name, c.as_str()),
            }
        }
    }

    #[test]
    fn money_stock_and_messages_are_never_automatic() {
        for cmd in [
            "products.price_update",
            "refunds.create",
            "payreviews.decide",
            "orders.convert",
            "cash.event",
            "ap.invoice_post",
            "ap.payment_record",
        ] {
            assert_eq!(class_of(cmd, &json!({})), Some(ActionClass::CommitFinancial), "{cmd}");
        }
        for cmd in ["inventory.receive", "inventory.adjust", "po.receive", "stocktake.finalize", "receiving.draft_post"] {
            assert_eq!(class_of(cmd, &json!({})), Some(ActionClass::CommitInventory), "{cmd}");
        }
        assert_eq!(class_of("whatsapp.queue", &json!({})), Some(ActionClass::ExternalCommunication));
        assert_eq!(class_of("invoicescan.confirm", &json!({ "receive": true })), Some(ActionClass::CommitInventory));
        assert_eq!(class_of("invoicescan.confirm", &json!({ "receive": false })), Some(ActionClass::Draft));
        for c in [ActionClass::CommitFinancial, ActionClass::CommitInventory, ActionClass::ExternalCommunication] {
            assert!(!c.automatic() && c.consequential() && c.min_risk() == "medium");
        }
    }

    #[test]
    fn unknown_commands_fail_closed() {
        assert_eq!(class_of("sales.create", &json!({})), None);
        assert_eq!(class_of("db.exec", &json!({})), None);
        assert!(require_automatic("sales.create", &json!({})).is_err());
        assert!(require_automatic("inventory.adjust", &json!({})).is_err());
        assert!(require_automatic("products.get", &json!({})).is_ok());
    }

    #[test]
    fn automated_workflows_never_reach_beyond_drafts() {
        for (w, max) in AUTOMATED {
            assert!(*max <= ActionClass::Draft, "{w}");
            for c in [
                ActionClass::ReversibleWrite,
                ActionClass::CommitRecord,
                ActionClass::CommitFinancial,
                ActionClass::CommitInventory,
                ActionClass::ExternalCommunication,
            ] {
                assert!(!automated_may(w, c), "{w} may {}", c.as_str());
            }
        }
        assert!(automated_may("whatsapp_orders.interpret", ActionClass::Draft));
        assert!(!automated_may("documents.ai_assist", ActionClass::Draft));
        assert!(!automated_may("unknown.workflow", ActionClass::Read));
    }

    #[test]
    fn legacy_tools_are_classified() {
        for t in [
            "list_reports",
            "run_report",
            "search_products",
            "low_stock",
            "product_details",
            "recent_whatsapp_messages",
            "invoice_scan_text",
            "eod_pack",
            "branch_context",
            "loyalty_balance",
            "digital_order_get",
        ] {
            let c = class_of(legacy_command(t).unwrap(), &json!({})).unwrap();
            assert!(c.automatic(), "{t}");
        }
        for t in ["propose_price_change", "propose_stock_adjustment", "propose_purchase_order"] {
            assert!(!class_of(legacy_command(t).unwrap(), &json!({})).unwrap().automatic(), "{t}");
        }
    }
}
