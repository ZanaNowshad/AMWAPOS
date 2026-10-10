import { useEffect, useState, type ReactNode } from "react";
import { Link, useNavigate } from "react-router-dom";
import { AlertTriangle, CalendarClock, Inbox } from "lucide-react";
import { api } from "../../api";
import type { AiPlaybookResult, AiProposal, AiProvider, AiSettings, AiTestResult, WaStatus } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { ApprovalCancelled, useApproval } from "../../components/approval";
import { WaPairingPanel } from "../../components/WaQr";
import { ApiError } from "../../api/transport";
import { Banner, Button, Checkbox, Chip, Field, Skeleton, TextInput } from "../../components/ui";
import { Confirm, useAction, useLoad } from "./common";
import { formatMoney, formatQty } from "../../lib/money";
import { formatDateTime } from "../../lib/time";
import { t, tb } from "../../i18n";

export const TOOL_LABEL: Record<string, () => string> = {
  library_search: () => t("Searched the documents"),
  memory_search: () => t("Checked Business Memory"),
  propose_memory: () => t("Suggested a fact for Business Memory"),
  library_document: () => t("Opened a document"),
  library_page_text: () => t("Read a document page"),
  propose_document_link: () => t("Proposed linking a document"),
  propose_document_details: () => t("Proposed correcting a document's details"),
  propose_expense_draft: () => t("Proposed a draft expense"),
  list_reports: () => t("Listed reports"),
  run_report: () => t("Ran a report"),
  search_products: () => t("Searched products"),
  product_details: () => t("Opened a product"),
  low_stock: () => t("Checked low stock"),
  recent_whatsapp_messages: () => t("Read WhatsApp messages"),
  invoice_scan_text: () => t("Read an invoice scan"),
  propose_price_change: () => t("Proposed a price change"),
  propose_stock_adjustment: () => t("Proposed a stock correction"),
  propose_purchase_order: () => t("Proposed a purchase order"),
};

const n = (v: unknown) => (typeof v === "number" ? v : null);

/** Field | before | after rows (the proposal card's diff). */
function DiffRows({ rows }: { rows: [string, ReactNode, ReactNode, boolean][] }) {
  return (
    <table className="diff">
      <thead>
        <tr>
          <th>{t("Field")}</th>
          <th>{t("Before")}</th>
          <th>{t("After")}</th>
        </tr>
      </thead>
      <tbody>
        {rows.map(([label, before, after, changed]) => (
          <tr key={label} className={changed ? "changed" : ""}>
            <th scope="row">{label}</th>
            <td className="before">{before}</td>
            <td className="after">{after}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function ProposalPreview({ p }: { p: AiProposal }) {
  const pv = p.preview;
  const money = (v: unknown) => <span className="money">{formatMoney(n(v))}</span>;
  if (p.kind === "price_change") {
    return (
      <div className="col gap-4">
        <div className="p-subject">{String(pv.product)}</div>
        <DiffRows
          rows={[
            [t("Price"), money(pv.old_price_minor), money(pv.new_price_minor), true],
            [t("Cost"), money(pv.cost_minor), money(pv.cost_minor), false],
          ]}
        />
      </div>
    );
  }
  if (p.kind === "stock_adjustment") {
    return (
      <div className="col gap-4">
        <div className="p-subject">{String(pv.product)}</div>
        <DiffRows
          rows={[
            [
              t("Stock"),
              <span className="num">{formatQty(n(pv.old_stock_milli) ?? 0)}</span>,
              <span className="num">{formatQty(n(pv.new_stock_milli) ?? 0)}</span>,
              true,
            ],
            [t("Value change"), "—", money(pv.value_change_minor), true],
          ]}
        />
      </div>
    );
  }
  const lines =
    (pv.lines as { product: string; qty_milli: number; unit_cost_minor: number; line_total_minor: number }[]) ?? [];
  return (
    <div className="col gap-8">
      <div>
        {t("Supplier")}: <strong>{String(pv.supplier)}</strong>
      </div>
      <table className="table">
        <tbody>
          {lines.map((l, i) => (
            <tr key={i}>
              <td>{l.product}</td>
              <td className="num">{formatQty(l.qty_milli)}</td>
              <td className="num">{formatMoney(l.unit_cost_minor)}</td>
              <td className="num">{formatMoney(l.line_total_minor)}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <div>
        {t("Total")}: <strong>{formatMoney(n(pv.total_minor))}</strong>
      </div>
    </div>
  );
}

/** One value in a diff: fils as money, milli-units as quantities, the rest as text. */
function DiffValue({ k, v }: { k: string; v: unknown }) {
  if (v === null || v === undefined || v === "") return <span className="muted">—</span>;
  if (typeof v === "number" && k.endsWith("_minor")) return <span className="money">{formatMoney(v)}</span>;
  if (typeof v === "number" && k.endsWith("_milli")) return <span className="num">{formatQty(v)}</span>;
  if (typeof v === "boolean") return <>{v ? t("Yes") : t("No")}</>;
  if (typeof v === "object") {
    const text = JSON.stringify(v);
    return (
      <code dir="ltr" className="tiny" style={{ wordBreak: "break-all" }}>
        {text.length > 240 ? `${text.slice(0, 240)}…` : text}
      </code>
    );
  }
  return <>{String(v)}</>;
}

/** Flatten one level of nested objects ("product.name") so a diff lines up field by field. */
function flat(v: unknown, prefix = ""): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  if (!v || typeof v !== "object" || Array.isArray(v)) return prefix ? { [prefix]: v } : {};
  for (const [k, x] of Object.entries(v as Record<string, unknown>)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (x && typeof x === "object" && !Array.isArray(x) && !prefix) Object.assign(out, flat(x, key));
    else out[key] = x;
  }
  return out;
}

/** "product.price_minor" → "Price" (money/qty/percent suffixes dropped). */
function fieldLabel(k: string): string {
  const last = k.split(".").pop() ?? k;
  const base = last.replace(/_(minor|milli|bp|id)$/, "").replace(/_/g, " ");
  const known: Record<string, () => string> = {
    price: () => t("Price"),
    "old price": () => t("Current price"),
    "new price": () => t("New price"),
    cost: () => t("Cost"),
    name: () => t("Name"),
    status: () => t("Status"),
    active: () => t("Active"),
    stock: () => t("Stock"),
    points: () => t("Points"),
    amount: () => t("Amount"),
    balance: () => t("Balance"),
    note: () => t("Note"),
    reason: () => t("Reason"),
  };
  return known[base]?.() ?? base.charAt(0).toUpperCase() + base.slice(1);
}

/** F3: field | before | after for a command proposal; changed rows are highlighted. */
export function DiffView({ preview }: { preview: Record<string, unknown> }) {
  const before = flat(preview.before);
  const after = flat(preview.after ?? preview.changes);
  const keys = Array.from(new Set([...Object.keys(after), ...Object.keys(before)])).filter(
    (k) => !["approval_token", "operation_id", "pin"].includes(k.split(".").pop() ?? ""),
  );
  if (!keys.length) return <div className="small muted">{t("No preview is available for this change.")}</div>;
  // Changed fields first, so the important rows are on screen.
  const changed = (k: string) => k in after && JSON.stringify(before[k]) !== JSON.stringify(after[k]);
  const ordered = [...keys.filter(changed), ...keys.filter((k) => !changed(k))].slice(0, 40);
  return (
    <table className="diff" data-testid="ai-diff">
      <thead>
        <tr>
          <th>{t("Field")}</th>
          <th>{t("Before")}</th>
          <th>{t("After")}</th>
        </tr>
      </thead>
      <tbody>
        {ordered.map((k) => (
          <tr key={k} className={changed(k) ? "changed" : ""}>
            <th scope="row">{fieldLabel(k)}</th>
            <td className="before">
              {k in before ? <DiffValue k={k} v={before[k]} /> : <span className="muted">—</span>}
            </td>
            <td className="after">
              {k in after ? <DiffValue k={k} v={after[k]} /> : <span className="muted">{t("unchanged")}</span>}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** Mirrors `ai::WRITE_PERMISSIONS` in the backend. */
export const WRITE_PERMISSIONS = [
  "products.manage",
  "prices.manage",
  "barcodes.resolve",
  "inventory.adjust",
  "inventory.receive",
  "inventory.transfer",
  "stocktake.manage",
  "suppliers.manage",
  "purchasing.manage",
  "customers.manage",
  "customers.credit",
  "deliveries.manage",
  "users.manage",
  "roles.manage",
  "devices.manage",
  "sync.manage",
  "settings.manage",
  "backup.manage",
  "backup.restore",
  "import.run",
  "whatsapp.manage",
  "whatsapp.send",
  "payments.review",
  "ocr.scan",
  "loyalty.adjust",
  "orders.manage",
  "branches.manage",
  "refund.create",
  "cash.paid_in",
  "cash.paid_out",
  "cash.safe_drop",
];

/** WhatsApp status fields: shown through the pairing panel, not as text. */
const WA_STATUS_KEYS = new Set([
  "enabled",
  "process",
  "session",
  "connected",
  "ready",
  "account",
  "adapter",
  "session_file",
  "restarts",
  "inbox_rev",
  "last_error",
  "next_retry_at",
  "banned_until",
  "last_send_at",
  "last_send_error",
]);

const isCommand = (p: AiProposal) => p.kind.startsWith("command:");

const LINK_RE = /\/admin\/(?:products|customers|suppliers|purchase-orders|stocktake)\/[A-Za-z0-9]{6,40}|\/admin\/ai\b/g;

/** F2: record paths in an answer become links. */
export function Linkified({ text }: { text: string }) {
  const parts: ReactNode[] = [];
  let last = 0;
  for (const m of text.matchAll(LINK_RE)) {
    const at = m.index ?? 0;
    if (at > last) parts.push(text.slice(last, at));
    parts.push(
      <Link key={at} to={m[0]} dir="ltr">
        {m[0]}
      </Link>,
    );
    last = at + m[0].length;
  }
  parts.push(text.slice(last));
  return <div style={{ whiteSpace: "pre-wrap" }}>{parts}</div>;
}

const STATUS_LABEL: Record<string, () => string> = {
  proposed: () => t("Waiting for your decision"),
  executing: () => t("Running"),
  executed: () => t("Done"),
  rejected: () => t("Rejected"),
  failed: () => t("Failed"),
  undone: () => t("Undone"),
  expired: () => t("Expired"),
};

const KIND_LABEL: Record<string, () => string> = {
  price_change: () => t("Price change"),
  stock_adjustment: () => t("Stock correction"),
  purchase_order: () => t("Draft purchase order"),
};

/** Human title by proposing tool (never the raw command name). */
const TOOL_TITLE: Record<string, () => string> = {
  propose_price_change: () => t("Change a selling price"),
  propose_bulk_price: () => t("Change selling prices"),
  propose_margin_price: () => t("Set a price from a margin"),
  propose_cost_update: () => t("Update a cost"),
  propose_product_active: () => t("Archive or restore a product"),
  propose_products_bulk_active: () => t("Archive or restore products"),
  propose_stock_adjustment: () => t("Correct stock"),
  propose_po_save: () => t("Draft a purchase order"),
  propose_reorder: () => t("Reorder low stock"),
  propose_loyalty_adjust: () => t("Adjust loyalty points"),
  propose_credit_adjust: () => t("Adjust customer credit"),
  propose_delivery_create: () => t("Create a delivery"),
  propose_delivery_update: () => t("Update a delivery"),
  propose_whatsapp_send: () => t("Send a WhatsApp message"),
  propose_whatsapp_connect: () => t("Connect WhatsApp"),
  propose_setting: () => t("Change a setting"),
  propose_device_rename: () => t("Rename a device"),
  propose_user_reset_pin: () => t("Reset a PIN"),
};

const PAGE_LABEL: Record<string, () => string> = {
  products: () => t("Products"),
  customers: () => t("Customers"),
  suppliers: () => t("Suppliers"),
  "purchase-orders": () => t("Purchase orders"),
  inventory: () => t("Inventory"),
  sales: () => t("Sales"),
  cash: () => t("Cash Events"),
  users: () => t("Users"),
  branches: () => t("Branches"),
  devices: () => t("Devices"),
  settings: () => t("Settings"),
  backups: () => t("Backups"),
  sync: () => t("Sync"),
  updates: () => t("Updates"),
  whatsapp: () => t("WhatsApp"),
  "phone-view": () => t("Phone view"),
  ai: () => t("AI Assistant"),
  dashboard: () => t("Dashboard"),
};

function humanCommand(cmd: string): string {
  const [area, action] = cmd.split(".");
  const a = (area ?? "").replace(/_/g, " ");
  return `${a.charAt(0).toUpperCase()}${a.slice(1)} · ${(action ?? "").replace(/_/g, " ")}`;
}

export function ProposalCard({ p, onChanged }: { p: AiProposal; onChanged: () => void }) {
  const { has } = useSession();
  const toast = useToast();
  const approve = useApproval();
  const act = useAction();
  const [confirm, setConfirm] = useState(false);
  const [pin, setPin] = useState("");
  const [once, setOnce] = useState<Record<string, unknown> | null>(null);
  const command = isCommand(p);
  const inputs = (p.params.confirm_inputs as string[] | undefined) ?? [];
  const needsPin = inputs.includes("user.pin");
  // Decided by permissions (never the role's name): admin access plus at least
  // one write permission; the command itself re-checks on Confirm.
  const canDecide = (has("ai.mutate") || has("admin.access")) && WRITE_PERMISSIONS.some((x) => has(x));
  const riskLabel = p.risk === "high" ? t("High risk") : p.risk === "medium" ? t("Medium risk") : t("Low risk");
  const tool = typeof p.params.tool === "string" ? p.params.tool : "";
  const title = command
    ? (TOOL_TITLE[tool]?.() ?? humanCommand(p.kind.slice("command:".length)))
    : (KIND_LABEL[p.kind]?.() ?? p.kind);
  const page = typeof p.preview.page === "string" ? p.preview.page : null;
  const pageName = page ? (PAGE_LABEL[page.split("/")[2] ?? ""]?.() ?? page) : null;
  const doConfirm = async () => {
    const r = await act.run(async () => {
      try {
        return await approve((tok) => api.ai.confirm(p.proposal_id, tok, needsPin ? { pin } : undefined));
      } catch (e) {
        if (e instanceof ApprovalCancelled) return null;
        // D1: the first of two confirmations is recorded; a second person must confirm.
        if (e instanceof ApiError && e.details?.kind === "second_confirmation_required") {
          toast("info", t("Your confirmation is recorded. A second person must also confirm this change."));
          setConfirm(false);
          onChanged();
          return null;
        }
        throw e;
      }
    });
    if (r) {
      setConfirm(false);
      setPin("");
      toast("success", t("Change made and recorded in the audit trail"));
      if (r.once) setOnce(r.once);
      else onChanged();
    }
  };
  // High-risk changes and PIN resets get one review dialog; the rest confirm from the card (its diff is the review).
  const onConfirm = () => (p.risk === "high" || needsPin ? setConfirm(true) : void doConfirm());
  return (
    <article
      className={`proposal risk-${p.risk} status-${p.status}`}
      data-testid="ai-proposal-card"
      tabIndex={0}
      aria-label={`${p.proposal_number} · ${title} · ${riskLabel}`}
      aria-keyshortcuts="Control+Enter Control+Backspace"
      onKeyDown={(e) => {
        // F5: Ctrl+Enter confirms, Ctrl+Backspace rejects (focused card only).
        if (!canDecide || p.status !== "proposed" || e.target !== e.currentTarget) return;
        if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
          e.preventDefault();
          onConfirm();
        } else if ((e.ctrlKey || e.metaKey) && e.key === "Backspace") {
          e.preventDefault();
          void act.run(() => api.ai.reject(p.proposal_id)).then((r) => r && onChanged());
        }
      }}
    >
      <div className="risk-stripe" data-testid="risk-stripe" aria-hidden />
      <div className="p-body" data-testid="ai-proposal">
        <header className="p-head">
          <span className={`risk-chip ${p.risk}`}>{riskLabel}</span>
          <span className="p-status">{STATUS_LABEL[p.status]?.() ?? p.status}</span>
        </header>
        <h3 className="p-title">{title}</h3>
        <div className="p-number tiny">{p.proposal_number}</div>
        <div data-testid="proposal-diff">{command ? <DiffView preview={p.preview} /> : <ProposalPreview p={p} />}</div>
        {p.first_confirmed_by_name && p.status === "proposed" ? (
          <div className="dual-line" role="status">
            {t("Confirmed by {0}; waiting for a second person", p.first_confirmed_by_name)}
          </div>
        ) : null}
        {p.risk_reasons.length ? (
          <details className="p-reasons">
            <summary>{t("Why this risk level")}</summary>
            <ul>
              {p.risk_reasons.map((r) => (
                <li key={r}>{tb(r)}</li>
              ))}
            </ul>
          </details>
        ) : null}
        {p.error ? <Banner tone="danger">{tb(p.error)}</Banner> : null}
        {act.error && !confirm ? <Banner tone="danger">{act.error}</Banner> : null}
        {canDecide ? (
          <div className="p-actions">
            {p.status === "proposed" ? (
              <>
                <Button
                  variant="primary"
                  size="lg"
                  block
                  loading={act.busy}
                  onClick={onConfirm}
                  data-testid="ai-confirm"
                >
                  {t("Confirm")}
                </Button>
                <Button
                  variant="ghost"
                  block
                  disabled={act.busy}
                  onClick={async () => {
                    if (await act.run(() => api.ai.reject(p.proposal_id))) onChanged();
                  }}
                >
                  {t("Reject")}
                </Button>
              </>
            ) : null}
            {p.status === "executed" && (!command || p.preview.undo === true) ? (
              <Button
                block
                data-testid="ai-undo"
                loading={act.busy}
                onClick={async () => {
                  if (await act.run(() => api.ai.undo(p.proposal_id))) {
                    toast("info", t("Undone with a correcting record"));
                    onChanged();
                  }
                }}
              >
                {t("Undo")}
              </Button>
            ) : null}
            {p.status === "executed" && command && p.preview.undo !== true ? (
              page ? (
                <Link className="btn block" to={page}>
                  {t("Fix on {0}", pageName ?? page)}
                </Link>
              ) : (
                <span className="tiny">
                  {t("This change cannot be undone from here. Correct it on the matching admin page.")}
                </span>
              )
            ) : null}
            {p.status === "executed" && p.kind === "purchase_order" && p.result?.po_id ? (
              <Link className="btn block" to={`/admin/purchase-orders/${String(p.result.po_id)}`}>
                {t("Open order")}
              </Link>
            ) : null}
          </div>
        ) : (
          <div className="tiny">{t("A manager with permission to approve AI changes must confirm this.")}</div>
        )}
      </div>
      {confirm ? (
        <Confirm
          title={t("Confirm {0}", p.proposal_number)}
          confirmLabel={t("Confirm change")}
          danger={p.risk === "high"}
          busy={act.busy}
          error={act.error}
          onCancel={() => (setConfirm(false), setPin(""))}
          onConfirm={() => void doConfirm()}
        >
          <div className="col gap-16">
            {command ? <DiffView preview={p.preview} /> : <ProposalPreview p={p} />}
            {needsPin ? (
              <TextInput
                label={t("New PIN for this user")}
                type="password"
                inputMode="numeric"
                autoComplete="off"
                value={pin}
                data-testid="ai-confirm-pin"
                hint={t("Typed here only. It is never sent to the assistant or stored with the proposal.")}
                onChange={(e) => setPin(e.target.value.replace(/[^\d]/g, "").slice(0, 8))}
              />
            ) : null}
            <div className="small">
              {command
                ? t(
                    "AMWAPOS runs the same command as the admin page, with your permissions. Manager approval or Windows Hello is asked for if that command needs it. A wrong PIN changes nothing.",
                  )
                : t(
                    "AMWAPOS runs this through the normal command with your permissions. It can be undone later with a correcting record; nothing is deleted.",
                  )}
            </div>
          </div>
        </Confirm>
      ) : null}
      {once ? (
        <Confirm
          title={t("Shown once")}
          confirmLabel={t("I have saved it")}
          onCancel={() => (setOnce(null), onChanged())}
          onConfirm={() => (setOnce(null), onChanged())}
        >
          <div className="col gap-8" data-testid="ai-once">
            {"session" in once && "qr" in once ? <WaPairingPanel initial={once as unknown as WaStatus} /> : null}
            <Banner tone="warning">
              {t("Copy this now. It is not stored with the proposal and is never sent to the assistant.")}
            </Banner>
            <dl className="kv">
              {Object.entries(once)
                .filter(([k, v]) => (typeof v === "string" || typeof v === "number") && !WA_STATUS_KEYS.has(k))
                .map(([k, v]) => (
                  <div key={k} style={{ display: "contents" }}>
                    <dt>
                      <code dir="ltr">{k}</code>
                    </dt>
                    <dd>
                      <code dir="ltr" style={{ wordBreak: "break-all", userSelect: "all" }}>
                        {String(v)}
                      </code>
                    </dd>
                  </div>
                ))}
            </dl>
          </div>
        </Confirm>
      ) : null}
    </article>
  );
}

export const PLAYBOOKS: { name: "eod" | "cash_short" | "reorder" | "refund_spike"; label: () => string }[] = [
  { name: "eod", label: () => t("End of day") },
  { name: "cash_short", label: () => t("Cash short") },
  { name: "reorder", label: () => t("Reorder") },
  { name: "refund_spike", label: () => t("Refund spike") },
];

function rowsIn(v: unknown): number | null {
  if (Array.isArray(v)) return v.length;
  if (v && typeof v === "object") {
    for (const k of ["rows", "items", "products", "data"]) {
      const x = (v as Record<string, unknown>)[k];
      if (Array.isArray(x)) return x.length;
    }
  }
  return null;
}

/** B2: a playbook is a fixed set of reads; no model is involved. */
export function PlaybookResult({ r, onAsk }: { r: AiPlaybookResult; onAsk: (q: string) => void }) {
  const label = PLAYBOOKS.find((p) => p.name === r.playbook)?.label() ?? r.playbook;
  return (
    <div className="card card-pad col gap-8" data-testid="ai-playbook">
      <div className="row">
        <strong className="grow">
          {label} · {r.date}
        </strong>
        <Button variant="ghost" onClick={() => onAsk(t("Explain the {0} playbook results for {1}", label, r.date))}>
          {t("Ask the assistant")}
        </Button>
      </div>
      {r.steps.map((st, i) => {
        const n = rowsIn(st.result);
        return (
          <details key={i}>
            <summary className="row">
              <Chip tone={st.ok ? "success" : "danger"}>{TOOL_LABEL[st.tool]?.() ?? st.tool}</Chip>
              <span className="tiny">{st.ok ? (n !== null ? t("{0} rows", n) : t("Done")) : tb(st.error ?? "")}</span>
            </summary>
            {st.ok ? (
              <pre className="tiny" dir="ltr" style={{ maxHeight: 240, overflow: "auto", whiteSpace: "pre-wrap" }}>
                {JSON.stringify(st.result, null, 1).slice(0, 6000)}
              </pre>
            ) : null}
          </details>
        );
      })}
    </div>
  );
}

/** A7 + D2: every open proposal in one place, with today's digest. */
export function ActionInbox({ onOpen }: { onOpen: (cid: string) => void }) {
  const open = useLoad(() => api.ai.proposals("proposed"), []);
  const digest = useLoad(() => api.ai.digest(), []);
  const notes = useLoad(() => api.ai.notes(10), []);
  const nav = useNavigate();
  const reload = () => (void open.reload(), void digest.reload(), void notes.reload());
  const unread = (notes.data ?? []).filter((n) => !n.read_at);
  return (
    <div className="col gap-16" data-testid="ai-inbox">
      <section className="side-card" data-testid="ai-alerts-moved">
        <div className="row gap-8">
          <AlertTriangle size={18} aria-hidden />
          <span className="grow small">{t("Operational alerts are cases in the Alert Centre.")}</span>
          <Button variant="ghost" onClick={() => nav("/admin/cases")}>
            {t("Open the Alert Centre")}
          </Button>
        </div>
      </section>
      {unread.length ? (
        <section className="inbox-list">
          {unread.map((n) => (
            <details
              key={n.note_id}
              className="inbox-row note"
              onToggle={(e) => {
                if ((e.target as HTMLDetailsElement).open) void api.ai.noteRead(n.note_id);
              }}
            >
              <summary>
                <CalendarClock size={20} className="ir-icon" aria-hidden />
                <div className="ir-main">
                  <div className="ir-title ellipsis">{n.title}</div>
                  <div className="tiny">
                    {t("Briefing note")} · {formatDateTime(n.created_at)}
                  </div>
                </div>
                <span className="chip info">{t("New")}</span>
              </summary>
              {n.summary ? <div className="ir-body">{n.summary}</div> : null}
            </details>
          ))}
        </section>
      ) : null}
      <section className="side-card">
        <h3>{t("Today's AI changes")}</h3>
        {digest.data ? (
          <div className="row wrap">
            {digest.data.counts.length ? (
              digest.data.counts.map((c) => (
                <Chip
                  key={`${c.status}-${c.risk}`}
                  tone={c.risk === "high" ? "danger" : c.risk === "medium" ? "warning" : "default"}
                >
                  {STATUS_LABEL[c.status]?.() ?? c.status} ·{" "}
                  {c.risk === "high" ? t("High risk") : c.risk === "medium" ? t("Medium risk") : t("Low risk")}:{" "}
                  {c.count}
                </Chip>
              ))
            ) : (
              <span className="small muted">{t("No proposals today.")}</span>
            )}
          </div>
        ) : (
          <Skeleton />
        )}
        <div className="tiny">{t("Open proposals expire after 60 minutes.")}</div>
      </section>
      {open.error ? <Banner tone="danger">{open.error}</Banner> : null}
      {!open.data ? (
        <Skeleton />
      ) : open.data.length ? (
        <div className="inbox-proposals">
          {open.data.map((p) => (
            <div key={p.proposal_id} className="col gap-8">
              <ProposalCard p={p} onChanged={reload} />
              <Button variant="ghost" onClick={() => onOpen(p.conversation_id)}>
                {t("Open the conversation")}
              </Button>
            </div>
          ))}
        </div>
      ) : (
        <div className="ai-state small-state">
          <Inbox size={28} aria-hidden />
          <p>{t("Nothing is waiting for a decision.")}</p>
        </div>
      )}
    </div>
  );
}

export function providerLabel(p: AiProvider): string {
  switch (p) {
    case "openai":
      return "OpenAI";
    case "anthropic":
      return "Anthropic";
    case "google":
      return "Google (Gemini)";
    case "openrouter":
      return "OpenRouter";
    case "custom":
      return t("Custom (OpenAI-compatible)");
    default:
      return t("Offline test model");
  }
}

const BASE_HINT: Record<AiProvider, string> = {
  fake: "",
  openai: "https://api.openai.com/v1",
  anthropic: "https://api.anthropic.com",
  google: "",
  openrouter: "https://openrouter.ai/api/v1",
  custom: "https://…/v1",
};

/** Settings → AI (owner only). Bring your own API key; no subscription sign-in. */
export function AiSettingsSection() {
  const toast = useToast();
  const { session } = useSession();
  const { data, setData, error } = useLoad(() => api.ai.status(), []);
  const [s, setS] = useState<AiSettings | null>(null);
  const [key, setKey] = useState("");
  const [header, setHeader] = useState("");
  const [fbKey, setFbKey] = useState("");
  const [test, setTest] = useState<AiTestResult | null>(null);
  const [consentReset, setConsentReset] = useState(false);
  const act = useAction();
  useEffect(() => {
    if (data && !s && data.is_owner) setS(data.settings as AiSettings);
  }, [data, s]);
  if (session?.role_id !== "role_owner") {
    return <Banner tone="info">{t("Only the owner can change the AI provider and keys.")}</Banner>;
  }
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data || !s) return <Skeleton />;
  const save = async (
    apiKey: string | null,
    headerValue: string | null,
    fallbackKey: string | null = fbKey || null,
  ) => {
    const r = await act.run(() => api.ai.configure(s, apiKey, headerValue, fallbackKey));
    if (r) {
      setData(r);
      setS(r.settings as AiSettings);
      setKey("");
      setHeader("");
      setFbKey("");
      setConsentReset(!!r.consent_reset);
      toast("success", t("Settings saved"));
    }
  };
  const real = s.provider !== "fake";
  const models = s.list_models_cache ?? [];
  return (
    <div className="card card-pad col gap-16">
      <Banner tone="info" title={t("Use an API key, not a chat subscription")}>
        {t(
          "ChatGPT Plus, Claude Pro, Gemini Advanced, and Codex logins do not work here. Create an API key at platform.openai.com, console.anthropic.com, aistudio.google.com, or openrouter.ai. For other servers, choose Custom and enter a base URL that speaks OpenAI Chat Completions.",
        )}
      </Banner>
      {!data.enabled ? (
        <Banner tone="info">
          {t("The AI assistant module is switched off.")}{" "}
          <Link to="/admin/settings?section=features">{t("Settings → Features")}</Link>
        </Banner>
      ) : null}
      <div className="row">
        <span className="small muted">{t("Active now")}:</span>
        <Chip tone={data.active_provider === "fake" ? "default" : "info"}>
          {providerLabel(data.active_provider)} · {data.model_id}
        </Chip>
        {real && !data.key_configured ? (
          <span className="tiny">{t("No key stored: the offline test model answers.")}</span>
        ) : null}
      </div>
      <div className="form-grid">
        <Field label={t("Provider")}>
          <select
            className="select"
            value={s.provider}
            data-testid="ai-provider"
            onChange={(e) => {
              const provider = e.target.value as AiProvider;
              setTest(null);
              setS({
                ...s,
                provider,
                model_id: provider === "fake" ? "fake-local" : provider === s.provider ? s.model_id : "",
                base_url: provider === s.provider ? s.base_url : "",
                list_models_cache: provider === s.provider ? s.list_models_cache : [],
              });
            }}
          >
            {(["fake", "openai", "anthropic", "google", "openrouter", "custom"] as AiProvider[]).map((p) => (
              <option key={p} value={p}>
                {p === "fake" ? t("Offline test model (no key, nothing sent)") : providerLabel(p)}
              </option>
            ))}
          </select>
        </Field>
        {real ? (
          <Field label={t("Model")} hint={t("Pick from the list or type any model id.")}>
            <div className="col gap-8">
              {models.length ? (
                <select
                  className="select"
                  aria-label={t("Models from the provider")}
                  value={models.includes(s.model_id) ? s.model_id : ""}
                  onChange={(e) => e.target.value && setS({ ...s, model_id: e.target.value })}
                >
                  <option value="">{t("Choose a model…")}</option>
                  {models.map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
                </select>
              ) : null}
              <input
                className="input"
                dir="ltr"
                aria-label={t("Model id")}
                placeholder={t("Model id")}
                value={s.model_id}
                onChange={(e) => setS({ ...s, model_id: e.target.value })}
              />
            </div>
          </Field>
        ) : null}
        {real && s.provider !== "google" ? (
          <TextInput
            label={s.provider === "custom" ? t("Base URL (required)") : t("Base URL (optional override)")}
            value={s.base_url}
            dir="ltr"
            placeholder={BASE_HINT[s.provider]}
            hint={
              s.provider === "custom"
                ? t("A server that speaks OpenAI Chat Completions; /v1 is added if missing.")
                : t("Leave empty for the provider's public API.")
            }
            onChange={(e) => setS({ ...s, base_url: e.target.value })}
          />
        ) : null}
        {real ? (
          <TextInput
            label={t("Maximum output tokens")}
            className="num"
            value={String(s.max_output_tokens)}
            hint={t("256 to 32000.")}
            onChange={(e) => setS({ ...s, max_output_tokens: Number(e.target.value.replace(/[^\d]/g, "")) || 0 })}
          />
        ) : null}
        <TextInput
          label={t("Daily token limit")}
          className="num"
          value={String(s.daily_token_cap ?? 0)}
          data-testid="ai-daily-cap"
          hint={t(
            "Input and output tokens per business day for all users. 0 = no limit. Used today: {0}.",
            data.tokens_today ?? 0,
          )}
          onChange={(e) => setS({ ...s, daily_token_cap: Number(e.target.value.replace(/[^\d]/g, "")) || 0 })}
        />
        <Field label={t("Answer language")} hint={t("Follow the screen, or always answer in one language.")}>
          <select
            className="select"
            data-testid="ai-answer-language"
            value={s.answer_language ?? "ui"}
            onChange={(e) => setS({ ...s, answer_language: e.target.value as AiSettings["answer_language"] })}
          >
            <option value="ui">{t("Same as the screen")}</option>
            <option value="en">English</option>
            <option value="ar">العربية</option>
          </select>
        </Field>
        {real ? (
          <TextInput
            label={t("Fast model (optional)")}
            dir="ltr"
            value={s.model_id_fast ?? ""}
            hint={t("Used for sorting WhatsApp messages and short reply drafts. Empty = the main model.")}
            onChange={(e) => setS({ ...s, model_id_fast: e.target.value })}
          />
        ) : null}
        {real ? (
          <TextInput
            label={t("Timeout (ms)")}
            className="num"
            value={String(s.timeout_ms)}
            hint={t("Up to 120000.")}
            onChange={(e) => setS({ ...s, timeout_ms: Number(e.target.value.replace(/[^\d]/g, "")) || 0 })}
          />
        ) : null}
      </div>
      {real ? (
        <>
          <TextInput
            label={t("API key")}
            type="password"
            autoComplete="off"
            value={key}
            data-testid="ai-key"
            placeholder={data.key_configured ? t("Stored — leave empty to keep") : ""}
            hint={t(
              "Stored in Windows Credential Manager on this computer, never in the database. It is never shown again.",
            )}
            onChange={(e) => setKey(e.target.value)}
          />
          <div className="form-grid">
            <TextInput
              label={t("Extra header name (optional)")}
              value={s.extra_header_name}
              dir="ltr"
              placeholder="X-Org-Id"
              onChange={(e) => setS({ ...s, extra_header_name: e.target.value })}
            />
            <TextInput
              label={t("Extra header value")}
              type="password"
              autoComplete="off"
              value={header}
              placeholder={data.extra_header_configured ? t("Stored — leave empty to keep") : ""}
              hint={t("Also kept in Credential Manager.")}
              onChange={(e) => setHeader(e.target.value)}
            />
          </div>
          {s.provider === "anthropic" ? (
            <Checkbox
              label={t("Retry declined requests on Anthropic's fallback model")}
              checked={s.fallbacks}
              onChange={(x) => setS({ ...s, fallbacks: x })}
            />
          ) : null}
          {consentReset ? (
            <Banner tone="warning" title={t("Agree again for the new provider")}>
              {t(
                "You changed to a different AI provider, so the earlier agreement no longer applies. Tick the box below and save.",
              )}
            </Banner>
          ) : null}
          <div className="col gap-8">
            <Checkbox
              label={t("I agree to send store data to this AI provider")}
              checked={s.consent}
              onChange={(x) => setS({ ...s, consent: x })}
            />
            <div className="tiny" style={{ marginInlineStart: 24 }}>
              {t(
                "When someone asks a question, AMWAPOS sends the question and the results of the lookups the assistant makes (product names, prices, stock, report totals) to the provider. PINs, keys and full customer lists are never sent. Customer messages are sent only if WhatsApp is on and the assistant reads them.",
              )}
              {` ${t("Changing to another provider clears this tick; agree again for the new one.")}`}
              {s.consent_at ? ` ${t("Agreed on {0}.", formatDateTime(s.consent_at))}` : ""}
            </div>
          </div>
        </>
      ) : null}
      <div className="card card-pad col gap-8" data-testid="ai-helpers">
        <strong>{t("Helpers and alerts")}</strong>
        <div className="small muted">
          {t(
            "Fixed rules, not guesses. Price and reorder suggestions become proposals a person confirms. Alerts only appear in the inbox.",
          )}
        </div>
        <div className="form-grid">
          <TextInput
            label={t("Target margin (%)")}
            className="num"
            value={String((s.target_margin_bp ?? 2500) / 100)}
            hint={t("Suggested price = cost ÷ (1 − margin), plus VAT when prices include it.")}
            onChange={(e) =>
              setS({ ...s, target_margin_bp: Math.round((Number(e.target.value.replace(/[^\d.]/g, "")) || 0) * 100) })
            }
          />
          <TextInput
            label={t("Round prices up to (fils)")}
            className="num"
            value={String(s.price_round_minor ?? 5)}
            onChange={(e) => setS({ ...s, price_round_minor: Number(e.target.value.replace(/[^\d]/g, "")) || 0 })}
          />
          <TextInput
            label={t("Refunds per day before an alert")}
            className="num"
            value={String(s.anomaly_refund_count ?? 5)}
            onChange={(e) => setS({ ...s, anomaly_refund_count: Number(e.target.value.replace(/[^\d]/g, "")) || 0 })}
          />
          <TextInput
            label={t("Refund total per day before an alert (fils)")}
            className="num"
            value={String(s.anomaly_refund_minor ?? 20000)}
            onChange={(e) => setS({ ...s, anomaly_refund_minor: Number(e.target.value.replace(/[^\d]/g, "")) || 0 })}
          />
          <TextInput
            label={t("Discount total per day before an alert (fils)")}
            className="num"
            value={String(s.anomaly_discount_minor ?? 20000)}
            onChange={(e) => setS({ ...s, anomaly_discount_minor: Number(e.target.value.replace(/[^\d]/g, "")) || 0 })}
          />
          <TextInput
            label={t("Minutes without a till heartbeat before an alert")}
            className="num"
            value={String(s.anomaly_hub_lag_minutes ?? 30)}
            onChange={(e) => setS({ ...s, anomaly_hub_lag_minutes: Number(e.target.value.replace(/[^\d]/g, "")) || 0 })}
          />
        </div>
      </div>
      <div className="card card-pad col gap-8" data-testid="ai-fallback">
        <strong>{t("Free fallback when the provider is unavailable")}</strong>
        <div className="small muted">
          {t(
            "If the chosen provider times out, is rate-limited, has a server error or does not know the model, AMWAPOS asks OpenRouter instead, using the model below (openrouter/free picks a free model). The page shows when this happens. It never happens for a wrong key (401) or a refusal. It is off until you tick the box.",
          )}
        </div>
        <Checkbox
          label={t("Use the OpenRouter fallback, and I agree to send the same store data to OpenRouter")}
          checked={!!s.fallback_free}
          onChange={(x) => setS({ ...s, fallback_free: x })}
        />
        <div className="form-grid">
          <TextInput
            label={t("Fallback model")}
            dir="ltr"
            value={s.fallback_model ?? "openrouter/free"}
            placeholder="openrouter/free"
            onChange={(e) => setS({ ...s, fallback_model: e.target.value })}
          />
          <TextInput
            label={t("OpenRouter API key")}
            type="password"
            autoComplete="off"
            value={fbKey}
            placeholder={data.fallback_key_configured ? t("Stored — leave empty to keep") : ""}
            hint={t("Stored in Windows Credential Manager, like the main key.")}
            onChange={(e) => setFbKey(e.target.value)}
          />
        </div>
        <div className="row">
          <Chip tone={data.fallback_ready ? "success" : "default"}>
            {data.fallback_ready ? t("Fallback ready") : t("Fallback off")}
          </Chip>
          {s.fallback_consent_at ? (
            <span className="tiny">{t("Agreed on {0}.", formatDateTime(s.fallback_consent_at))}</span>
          ) : null}
          {data.fallback_key_configured ? (
            <Button variant="ghost" onClick={() => void save(null, null, "")}>
              {t("Remove OpenRouter key")}
            </Button>
          ) : null}
        </div>
      </div>
      {test ? (
        <Banner tone={test.ok ? "success" : "danger"} title={test.ok ? t("Connection works") : t("Connection failed")}>
          {test.ok
            ? t("{0} models available.", test.models ?? 0) +
              (test.model_listed === false ? ` ${t("The chosen model is not in the list.")}` : "")
            : `${test.status ? `HTTP ${test.status} · ` : ""}${tb(test.error ?? "")}`}
        </Banner>
      ) : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <div className="row wrap">
        {real && data.key_configured ? (
          <Button variant="ghost" onClick={() => void save("", null)}>
            {t("Remove key")}
          </Button>
        ) : null}
        {real ? (
          <>
            <Button
              disabled={!data.key_configured || act.busy}
              onClick={async () => {
                const r = await act.run(() => api.ai.test());
                if (r) setTest(r);
              }}
            >
              {t("Test connection")}
            </Button>
            <Button
              disabled={!data.key_configured || act.busy}
              onClick={async () => {
                const r = await act.run(() => api.ai.models());
                if (r) {
                  setS({ ...s, list_models_cache: r.models });
                  toast("success", t("{0} models loaded", r.models.length));
                }
              }}
            >
              {t("Refresh models")}
            </Button>
          </>
        ) : null}
        <Button
          variant="primary"
          className="right"
          loading={act.busy}
          onClick={() => void save(key || null, header || null)}
        >
          {t("Save")}
        </Button>
      </div>
      <div className="tiny">{t("Changes apply to the next question; no restart is needed.")}</div>
    </div>
  );
}
