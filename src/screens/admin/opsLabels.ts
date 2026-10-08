// Plain-word labels shared by the Alert Centre, Sync problems and Terminals
// screens (docs/OPERATIONAL_CONTROL.md). Codes come from the backend; people
// see these words.
import type { CaseKind, DeadLetterReason, TerminalHealthState } from "../../api/types";
import { t } from "../../i18n";

export function kindLabel(k: CaseKind | string): string {
  switch (k) {
    case "cash_variance":
      return t("Drawer difference");
    case "sync_failures":
      return t("Records not saved");
    case "terminal_not_seen":
      return t("Till not seen");
    case "terminal_backlog":
      return t("Till behind with sending");
    case "terminal_incompatible":
      return t("Till on a different version");
    case "credential_rotation_stale":
      return t("New credential not picked up");
    case "backup_overdue":
      return t("Backup overdue");
    case "print_failures":
      return t("Printing failed");
    case "payment_review_backlog":
      return t("Payment screenshots waiting");
    case "rider_cash_held":
      return t("Rider holding cash");
    case "legacy_alert":
      return t("From the old AI inbox");
    default:
      return k;
  }
}

export const CASE_KINDS: CaseKind[] = [
  "sync_failures",
  "terminal_not_seen",
  "terminal_backlog",
  "terminal_incompatible",
  "credential_rotation_stale",
  "backup_overdue",
  "print_failures",
  "payment_review_backlog",
  "rider_cash_held",
  "cash_variance",
  "legacy_alert",
];

/** Why a record could not be saved, and what to do about it. */
export function reasonLabel(r: DeadLetterReason | string): { label: string; help: string } {
  switch (r) {
    case "version_mismatch":
      return {
        label: t("App versions differ"),
        help: t("Update the hub and this till to the same AMWAPOS version, then try again."),
      };
    case "not_permitted":
      return {
        label: t("Not allowed from this till"),
        help: t("This till may not change these records. Trying again will not help; close it with a reason."),
      };
    case "invalid_record":
      return { label: t("Incomplete record"), help: t("The record arrived incomplete. Trying again will not help.") };
    case "missing_dependency":
      return {
        label: t("Waiting for a related record"),
        help: t("A record it depends on has not arrived yet. Try again once the till has sent everything."),
      };
    case "conflict":
      return {
        label: t("Conflicts with a saved record"),
        help: t("It clashes with a record already saved. Check both, then close it with a reason."),
      };
    case "superseded":
      return {
        label: t("A newer version was saved"),
        help: t("Applying it would overwrite newer information. Close it with a reason."),
      };
    case "storage_error":
      return { label: t("Database busy or full"), help: t("Free disk space if needed, then try again.") };
    case "legacy_unclassified":
      return {
        label: t("Reason not recorded"),
        help: t("This record is from before this update; its reason was not recorded. Trying again is safe."),
      };
    default:
      return { label: t("Unexpected error"), help: t("Try again. If it keeps failing, contact support.") };
  }
}

export const REASONS: DeadLetterReason[] = [
  "missing_dependency",
  "version_mismatch",
  "storage_error",
  "conflict",
  "superseded",
  "not_permitted",
  "invalid_record",
  "unknown",
  "legacy_unclassified",
];

/** A replicated table as a person would call it. */
export function recordLabel(table: string): string {
  const m: Record<string, () => string> = {
    sales: () => t("Sale"),
    sale_items: () => t("Sale line"),
    payments: () => t("Payment"),
    refunds: () => t("Refund"),
    refund_items: () => t("Refund line"),
    refund_tenders: () => t("Refund payment"),
    cash_events: () => t("Cash movement"),
    stock_movements: () => t("Stock movement"),
    shifts: () => t("Shift"),
    customers: () => t("Customer"),
    customer_ledger: () => t("Customer account entry"),
    loyalty_ledger: () => t("Loyalty points entry"),
    sale_collections: () => t("Delivery cash collection"),
    rider_handovers: () => t("Rider hand-over"),
    receipt_snapshots: () => t("Receipt copy"),
    sale_voids: () => t("Voided sale"),
    coupon_redemptions: () => t("Coupon use"),
  };
  return m[table]?.() ?? table;
}

export function healthLabel(h: TerminalHealthState): {
  label: string;
  tone: "success" | "warning" | "danger" | "default" | "info";
} {
  switch (h) {
    case "healthy":
      return { label: t("Healthy"), tone: "success" };
    case "attention":
      return { label: t("Needs attention"), tone: "warning" };
    case "offline":
      return { label: t("Offline"), tone: "default" };
    case "revoked":
      return { label: t("Revoked"), tone: "danger" };
    default:
      return { label: t("Unknown"), tone: "info" };
  }
}

export function healthReason(r: string): string {
  switch (r) {
    case "not_seen_during_shift":
      return t("Not seen during an open shift");
    case "protocol_mismatch":
      return t("Different sync protocol");
    case "schema_mismatch":
      return t("Different database version");
    case "backlog":
      return t("Behind with sending");
    case "refused_records":
      return t("Has records that could not be saved");
    case "sync_error":
      return t("Last sync failed");
    case "never_reported":
      return t("Has not reported yet");
    default:
      return r;
  }
}

/** Labels for the facts a system case records. */
export function factLabel(k: string): string {
  const m: Record<string, () => string> = {
    count: () => t("How many"),
    money_or_stock: () => t("Money or stock records"),
    oldest: () => t("Oldest"),
    reason: () => t("Reason"),
    can_retry: () => t("Can be tried again"),
    tables: () => t("Kinds of record"),
    last_seen_at: () => t("Last seen"),
    minutes: () => t("Minutes"),
    open_shift: () => t("Open shift"),
    pending: () => t("Waiting to send"),
    oldest_pending_at: () => t("Oldest waiting since"),
    last_push_at: () => t("Last sent"),
    schema_version: () => t("Database version"),
    hub_schema_version: () => t("Hub database version"),
    protocol_version: () => t("Sync protocol"),
    hub_protocol_version: () => t("Hub sync protocol"),
    app_version: () => t("App version"),
    state: () => t("State"),
    summary: () => t("Summary"),
    staged_at: () => t("Started"),
    version: () => t("Credential version"),
    next_version: () => t("New credential version"),
    legacy_kind: () => t("Old alert type"),
    day: () => t("Day"),
    detail: () => t("Details"),
    source: () => t("Source"),
    episode: () => t("Episode"),
    failed_24h: () => t("Failed in the last 24 hours"),
    waiting: () => t("Waiting"),
    collections: () => t("Orders"),
    last_failed_at: () => t("Last failure"),
    last_printed_at: () => t("Last printed"),
    amount_minor: () => t("Amount"),
    rider: () => t("Rider"),
  };
  return m[k]?.() ?? k;
}
