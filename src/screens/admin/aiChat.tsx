import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import {
  AlertTriangle,
  Bot,
  Brain,
  CalendarClock,
  CheckCircle2,
  Cpu,
  FileScan,
  Image as ImageIcon,
  Inbox,
  Keyboard,
  Languages,
  MessageSquare,
  Paperclip,
  Pencil,
  Pin,
  Plus,
  Receipt,
  RotateCw,
  Send,
  Settings2,
  ShoppingCart,
  Slash,
  X,
  XCircle,
} from "lucide-react";
import { api } from "../../api";
import type {
  AiSource,
  AiBriefing,
  AiContext,
  AiConversation,
  AiPin,
  AiPlaybookResult,
  AiSlashResult,
  AiStatus,
  AiStreamEvent,
} from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { useFeature } from "../../components/FeatureGate";
import { Banner, Button, Chip, Modal, Skeleton, TextInput } from "../../components/ui";
import { Confirm, useAction, useLoad } from "./common";
import { formatMoney, formatQty } from "../../lib/money";
import { formatDateTime, relative } from "../../lib/time";
import { getLang, t, tb } from "../../i18n";
import { ActionInbox, Linkified, PLAYBOOKS, PlaybookResult, ProposalCard, TOOL_LABEL, providerLabel } from "./ai";
import { fileToBase64 } from "./automation";

// ---------------------------------------------------------------- live view (C8)

interface LiveStep {
  id: string;
  name: string;
  input: unknown;
  result?: string;
  is_error?: boolean;
  truncated?: boolean;
}
interface Live {
  text: string;
  thinking: string;
  steps: LiveStep[];
  round: number;
  provider: string;
  model: string;
  fallback: { from: string; to: string; reason: string } | null;
  nudged: boolean;
  unverified: boolean;
  tokensIn: number;
  tokensOut: number;
}
const emptyLive = (): Live => ({
  text: "",
  thinking: "",
  steps: [],
  round: 0,
  provider: "",
  model: "",
  fallback: null,
  nudged: false,
  unverified: false,
  tokensIn: 0,
  tokensOut: 0,
});

function applyEvents(l: Live, events: AiStreamEvent[]): Live {
  const next: Live = { ...l, steps: [...l.steps] };
  for (const e of events) {
    switch (e.type) {
      case "start":
        next.provider = e.provider;
        next.model = e.model;
        break;
      case "round":
        next.round = e.n;
        // A new round starts a new answer; keep what was said so far visible.
        if (next.text && !next.text.endsWith("\n\n")) next.text += "\n\n";
        break;
      case "text":
        next.text += e.delta;
        break;
      case "thinking":
        next.thinking += e.delta;
        break;
      case "tool_call":
        next.steps.push({ id: e.id, name: e.name, input: e.input });
        break;
      case "tool_result": {
        const i = next.steps.findIndex((s) => s.id === e.id);
        if (i >= 0)
          next.steps[i] = { ...next.steps[i], result: e.content, is_error: e.is_error, truncated: e.truncated };
        break;
      }
      case "usage":
        next.tokensIn += e.input_tokens;
        next.tokensOut += e.output_tokens;
        break;
      case "fallback":
        next.fallback = { from: e.from, to: e.to, reason: e.reason };
        break;
      case "nudge":
        next.nudged = true;
        break;
      case "unverified":
        next.unverified = true;
        break;
      default:
        break;
    }
  }
  return next;
}

const toolLabel = (name: string) => TOOL_LABEL[name]?.() ?? name;

function pretty(v: unknown): string {
  if (typeof v === "string") {
    try {
      return JSON.stringify(JSON.parse(v), null, 1);
    } catch {
      return v;
    }
  }
  return JSON.stringify(v, null, 1);
}

/** One tool call as a timeline chip: name → running → done. Tap opens its input and output. */
function ToolStep({
  name,
  input,
  result,
  isError,
  running,
}: {
  name: string;
  input: unknown;
  result?: string;
  isError?: boolean;
  running?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const state = running ? "running" : isError ? "error" : "done";
  return (
    <>
      <button
        type="button"
        className={`tool-chip ${state}`}
        data-testid="ai-tool-step"
        aria-label={`${toolLabel(name)} (${name}): ${running ? t("Running…") : isError ? t("Error") : t("Done")}`}
        onClick={() => setOpen(true)}
      >
        {running ? (
          <span className="spinner" aria-hidden />
        ) : isError ? (
          <XCircle size={16} aria-hidden />
        ) : (
          <CheckCircle2 size={16} aria-hidden />
        )}
        <span className="tc-label">{toolLabel(name)}</span>
        <code dir="ltr">{name}</code>
      </button>
      {open ? (
        <Modal title={toolLabel(name)} size="sheet narrow" onClose={() => setOpen(false)}>
          <div className="col gap-8">
            <code dir="ltr" className="tiny">
              {name}
            </code>
            <div className="field-label">{t("Input")}</div>
            <pre className="tool-io" dir="ltr">
              {pretty(input)}
            </pre>
            <div className="field-label">{t("Result the assistant received")}</div>
            <pre className="tool-io" dir="ltr">
              {result === undefined ? t("Running…") : pretty(result)}
            </pre>
          </div>
        </Modal>
      ) : null}
    </>
  );
}

function Thinking({ text, open }: { text: string; open?: boolean }) {
  if (!text.trim()) return null;
  return (
    <details open={open} className="ai-thinking" data-testid="ai-thinking">
      <summary>
        <Brain size={16} aria-hidden /> {t("Thinking")}
      </summary>
      <div className="think-text">{text}</div>
    </details>
  );
}

const SOURCE_TYPE: Record<string, () => string> = {
  sale: () => t("Sale"),
  refund: () => t("Refund"),
  expense: () => t("Expense"),
  supplier_invoice: () => t("Supplier invoice"),
  purchase_order: () => t("Purchase order"),
  requisition: () => t("Requisition"),
  supplier_return: () => t("Supplier return"),
  case: () => t("Case"),
  day_close: () => t("Z close"),
  shift: () => t("Shift"),
  batch: () => t("Batch"),
  waste: () => t("Waste"),
  offer: () => t("Offer"),
  bundle: () => t("Bundle"),
  sync_problem: () => t("Sync problem"),
  document: () => t("Document"),
  memory: () => t("Business memory"),
  supplier: () => t("Supplier"),
  product: () => t("Product"),
  customer: () => t("Customer"),
  read: () => t("Read"),
};

const BASIS: Record<string, () => string> = {
  fact: () => t("Fact"),
  derived: () => t("Derived"),
  estimate: () => t("Estimate"),
};

/** The records the reads behind an answer returned, from the backend's record of them. */
function SourcesUsed({ sources }: { sources: AiSource[] }) {
  const [open, setOpen] = useState(false);
  const shown = open ? sources : sources.slice(0, 6);
  return (
    <div className="evidence" data-testid="ai-sources">
      <span className="tiny muted">{t("Sources used")}:</span>
      {shown.map((s, j) => {
        const kind = SOURCE_TYPE[s.type]?.() ?? s.type;
        const body = (
          <>
            <span>
              {kind}
              {s.label ? (
                <>
                  {" "}
                  <span dir="auto">{s.type === "read" ? (TOOL_LABEL[s.label]?.() ?? s.label) : s.label}</span>
                </>
              ) : null}
            </span>
            {s.basis && s.basis !== "fact" ? <span className="ev-time">{BASIS[s.basis]?.()}</span> : null}
          </>
        );
        return s.link ? (
          <Link key={j} to={s.link} className="ev-chip" data-testid="ai-source">
            {body}
          </Link>
        ) : (
          <span key={j} className="ev-chip" data-testid="ai-source">
            {body}
          </span>
        );
      })}
      {sources.length > 6 && !open ? (
        <button className="link tiny" onClick={() => setOpen(true)}>
          {t("{0} more", sources.length - 6)}
        </button>
      ) : null}
    </div>
  );
}

/** Where a piece of evidence lives in the admin, when it names one record. */
function evidenceLink(tool: string, ids: string[]): string | null {
  const id = ids.length === 1 ? ids[0] : null;
  if (/product|price|stock|barcode|margin/.test(tool)) return id ? `/admin/products/${id}` : "/admin/products";
  if (/customer|loyalty|credit/.test(tool)) return id ? `/admin/customers/${id}` : "/admin/customers";
  if (/supplier/.test(tool)) return id ? `/admin/suppliers/${id}` : "/admin/suppliers";
  if (/purchase|_po|reorder/.test(tool)) return id ? `/admin/purchase-orders/${id}` : "/admin/purchase-orders";
  if (/deliver/.test(tool)) return "/admin/deliveries";
  if (/sale|refund|receipt/.test(tool)) return "/admin/sales";
  if (/shift|cash/.test(tool)) return "/admin/shifts";
  if (/audit/.test(tool)) return "/admin/audit";
  if (/report|kpi|dashboard/.test(tool)) return "/admin/reports";
  return null;
}

const MONEY_RE = /((?:BHD|د\.ب\.?)\s?-?[\d٠-٩,]+(?:[.٫][\d٠-٩]{1,3})?|-?[\d,]+\.\d{3}\b)/g;

/** Answers render bold, bullet lists and money spans styled as totals; links stay links. */
function RichText({ text }: { text: string }) {
  const inline = (line: string, key: string): ReactNode[] =>
    line.split(/(\*\*[^*]+\*\*)/g).flatMap((part, i) => {
      if (/^\*\*[^*]+\*\*$/.test(part)) return [<strong key={`${key}b${i}`}>{part.slice(2, -2)}</strong>];
      return part.split(MONEY_RE).map((seg, j) =>
        j % 2 === 1 ? (
          <span key={`${key}m${i}.${j}`} className="ai-money">
            {seg}
          </span>
        ) : seg ? (
          <Linkified key={`${key}t${i}.${j}`} text={seg} />
        ) : null,
      );
    });
  const blocks: ReactNode[] = [];
  let list: ReactNode[] = [];
  const flush = () => {
    if (list.length) blocks.push(<ul key={`ul${blocks.length}`}>{list}</ul>);
    list = [];
  };
  text.split("\n").forEach((raw, i) => {
    // Raw tool data (JSON) folds away instead of filling the screen.
    const tr = raw.trim();
    if (tr.length > 80 && (tr.startsWith("{") || tr.startsWith("["))) {
      flush();
      blocks.push(
        <details key={i} className="raw-data">
          <summary>{t("Data ({0} characters)", tr.length)}</summary>
          <pre dir="ltr">{tr}</pre>
        </details>,
      );
      return;
    }
    const m = /^\s*(?:[-*•]|\d+[.)])\s+(.*)$/.exec(raw);
    if (m) {
      list.push(<li key={i}>{inline(m[1], `l${i}`)}</li>);
      return;
    }
    flush();
    if (raw.trim()) blocks.push(<p key={i}>{inline(raw, `p${i}`)}</p>);
  });
  flush();
  return <div className="ai-rich">{blocks}</div>;
}

function LiveView({ live }: { live: Live }) {
  return (
    <div className="msg assistant" data-testid="ai-live">
      <div className="avatar-ai" aria-hidden>
        <Bot size={18} />
      </div>
      <div className="bubble in">
        <div className="bubble-meta">
          {live.provider
            ? `${providerLabel(live.provider as AiStatus["active_provider"])} · ${live.model}`
            : t("Starting…")}
          {live.round ? ` · ${t("Step {0}", live.round)}` : ""}
          {live.tokensIn || live.tokensOut ? ` · ${t("{0} in / {1} out tokens", live.tokensIn, live.tokensOut)}` : ""}
        </div>
        {live.fallback ? (
          <span className="chip warning" data-testid="ai-fallback-chip">
            {t("Answered via OpenRouter fallback")} ·{" "}
            <span dir="ltr">
              {live.fallback.from} → {live.fallback.to}
            </span>
          </span>
        ) : null}
        <Thinking text={live.thinking} open />
        {live.steps.length ? (
          <div className="tool-timeline">
            {live.steps.map((s) => (
              <ToolStep
                key={s.id}
                name={s.name}
                input={s.input}
                result={s.result}
                isError={s.is_error}
                running={s.result === undefined}
              />
            ))}
          </div>
        ) : null}
        {live.nudged ? (
          <div className="tiny">{t("AMWAPOS asked the assistant to back its figures with a tool.")}</div>
        ) : null}
        <div className="ai-rich streaming">
          {live.text ? <span style={{ whiteSpace: "pre-wrap" }}>{live.text}</span> : null}
          <span className="caret" aria-hidden />
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- stored messages

function Attachment({ id }: { id: string }) {
  const [src, setSrc] = useState<string | null>(null);
  useEffect(() => {
    let on = true;
    api.ai.attachment(id).then(
      (a) => on && setSrc(`data:${a.media_type};base64,${a.data}`),
      () => undefined,
    );
    return () => {
      on = false;
    };
  }, [id]);
  return src ? (
    <img src={src} alt={t("Attached photo")} style={{ maxWidth: 180, maxHeight: 140, borderRadius: 8 }} />
  ) : (
    <Chip>{t("Photo")}</Chip>
  );
}

type Message = AiConversation["messages"][number];

function MessageView({ m, fallback }: { m: Message; fallback?: { from: string; to: string } | null }) {
  if (m.kind === "nudge") {
    return (
      <div className="tiny muted msg-note">{t("AMWAPOS asked the assistant to back its figures with a tool.")}</div>
    );
  }
  const mine = m.role === "user";
  return (
    <div className={`msg ${mine ? "user" : "assistant"}`}>
      {!mine ? (
        <div className="avatar-ai" aria-hidden>
          <Bot size={18} />
        </div>
      ) : null}
      <div className={`bubble ${mine ? "out" : "in"}`}>
        {fallback ? (
          <span className="chip warning" data-testid="ai-fallback-chip">
            {t("Answered via OpenRouter fallback")}
          </span>
        ) : null}
        {m.thinking ? <Thinking text={m.thinking} /> : null}
        {m.calls?.length ? (
          <div className="tool-timeline">
            {m.calls.map((c) => (
              <ToolStep key={c.id} name={c.name} input={c.input} result={c.result} isError={c.is_error} />
            ))}
          </div>
        ) : null}
        {m.attachments?.length ? (
          <div className="row wrap">
            {m.attachments.map((a) => (
              <Attachment key={a.attachment_id} id={a.attachment_id} />
            ))}
          </div>
        ) : null}
        {m.has_context ? <Chip tone="brand">{t("Till cart attached")}</Chip> : null}
        {m.unverified ? (
          <div className="ai-unverified" data-testid="ai-unverified" role="note">
            <AlertTriangle size={16} aria-hidden />
            {t("Unverified: no tool result backs these figures")}
          </div>
        ) : null}
        {m.text ? mine ? <div style={{ whiteSpace: "pre-wrap" }}>{m.text}</div> : <RichText text={m.text} /> : null}
        {m.role === "assistant" && m.text && m.sources?.length ? <SourcesUsed sources={m.sources} /> : null}
        {m.role === "assistant" && m.text && m.evidence?.length ? (
          <div className="evidence" data-testid="ai-evidence">
            {m.evidence.slice(0, 8).map((ev, j) => {
              const to = evidenceLink(ev.tool, ev.ids);
              const body = (
                <>
                  <span dir="ltr">
                    {ev.tool}
                    {ev.ids.length ? ` · ${ev.ids.slice(0, 2).join(", ")}${ev.ids.length > 2 ? "…" : ""}` : ""}
                  </span>
                  <span className="ev-time">{relative(ev.at)}</span>
                </>
              );
              return to ? (
                <Link key={j} to={to} className="ev-chip">
                  {body}
                </Link>
              ) : (
                <span key={j} className="ev-chip">
                  {body}
                </span>
              );
            })}
          </div>
        ) : null}
        {m.stop_reason === "refusal" ? (
          <div className="tiny">{t("The provider declined to answer this request.")}</div>
        ) : null}
        {m.stop_reason === "max_tokens" ? <div className="tiny">{t("The answer was cut short.")}</div> : null}
        <div className="bubble-time">{formatDateTime(m.at)}</div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- slash commands (F1)

interface SlashDef {
  name: string;
  args?: string;
  read?: boolean;
  desc: () => string;
}

export const SLASH: SlashDef[] = [
  { name: "help", desc: () => t("List every command") },
  { name: "new", desc: () => t("Start a new conversation") },
  { name: "rename", args: "<name>", desc: () => t("Name this conversation") },
  {
    name: "pin",
    args: "<kind> <name or id>",
    desc: () => t("Pin a product, customer, supplier, order, shift, PO, sale or delivery"),
  },
  { name: "unpin", args: "<kind> <id>", desc: () => t("Remove a pinned record") },
  { name: "price", args: "<product> <amount>", desc: () => t("Propose a new price (you confirm it)") },
  {
    name: "explain",
    args: "<command> [text]",
    desc: () => t("Let the assistant look into a command's data and explain it"),
  },
  { name: "ask", args: "<question>", desc: () => t("Ask the assistant (same as typing)") },
  { name: "attach", desc: () => t("Attach a photo, invoice or payment screenshot") },
  { name: "inbox", desc: () => t("Open the action inbox") },
  { name: "briefings", desc: () => t("Scheduled briefings and notes") },
  { name: "goto", args: "<page>", desc: () => t("Open an admin page") },
  { name: "model", desc: () => t("Show the provider, model and today's tokens") },
  { name: "shortcuts", desc: () => t("Keyboard shortcuts") },
  { name: "clear", desc: () => t("Clear command results") },
  { name: "kpi", read: true, desc: () => t("Today's dashboard figures") },
  { name: "stock", args: "<product>", read: true, desc: () => t("Find products and their stock") },
  { name: "low", read: true, desc: () => t("Products low on stock") },
  { name: "sale", args: "<receipt>", read: true, desc: () => t("Find a sale by receipt number") },
  { name: "refund", args: "<receipt>", read: true, desc: () => t("What can be refunded on a receipt") },
  { name: "customer", args: "<name or phone>", read: true, desc: () => t("Find customers") },
  { name: "supplier", args: "[name]", read: true, desc: () => t("Suppliers") },
  { name: "po", args: "[status]", read: true, desc: () => t("Purchase orders") },
  { name: "shift", read: true, desc: () => t("The open shift") },
  { name: "shifts", read: true, desc: () => t("Today's shifts") },
  { name: "cash", read: true, desc: () => t("Today's cash in / out / drops") },
  { name: "deliveries", read: true, desc: () => t("Open deliveries") },
  { name: "orders", read: true, desc: () => t("Orders") },
  { name: "transfers", read: true, desc: () => t("Stock transfers") },
  { name: "stocktakes", read: true, desc: () => t("Stocktakes") },
  { name: "ghost", read: true, desc: () => t("Unknown barcodes scanned at the till") },
  { name: "payments", read: true, desc: () => t("Payment screenshots to review") },
  { name: "invoices", read: true, desc: () => t("Invoice scans") },
  { name: "triage", read: true, desc: () => t("Sorted WhatsApp messages") },
  { name: "audit", args: "[event]", read: true, desc: () => t("Audit log") },
  { name: "backup", read: true, desc: () => t("Backup health") },
  { name: "diagnostics", read: true, desc: () => t("Health checks") },
  { name: "users", read: true, desc: () => t("Staff accounts") },
  { name: "devices", read: true, desc: () => t("Tills and hub") },
  { name: "sync", read: true, desc: () => t("Sync status") },
  { name: "categories", read: true, desc: () => t("Product categories") },
  { name: "notes", read: true, desc: () => t("Briefing notes") },
  { name: "eod", read: true, desc: () => t("End-of-day playbook") },
  { name: "cash_short", read: true, desc: () => t("Cash-short playbook") },
  { name: "reorder", read: true, desc: () => t("Reorder playbook") },
  { name: "refund_spike", read: true, desc: () => t("Refund-spike playbook") },
];

/** Palette groups: read-only lookups, writes (proposals only) and chat controls. */
const slashGroup = (d: SlashDef): "read" | "write" | "chat" =>
  d.read ? "read" : d.name === "price" ? "write" : "chat";
const SLASH_GROUP_LABEL = { read: () => t("Read"), write: () => t("Write"), chat: () => t("Chat") };

const PAGES = [
  "dashboard",
  "sales",
  "refunds",
  "shifts",
  "products",
  "inventory",
  "suppliers",
  "purchase-orders",
  "customers",
  "deliveries",
  "orders",
  "reports",
  "end-of-day",
  "whatsapp",
  "payment-reviews",
  "invoice-scan",
  "users",
  "settings",
  "backups",
  "audit",
  "diagnostics",
  "sync",
];

const PIN_KINDS: AiPin["kind"][] = ["product", "customer", "supplier", "order", "shift", "po", "sale", "delivery"];
const PIN_LABEL: Record<AiPin["kind"], () => string> = {
  product: () => t("Product"),
  customer: () => t("Customer"),
  supplier: () => t("Supplier"),
  order: () => t("Order"),
  shift: () => t("Shift"),
  po: () => t("Purchase order"),
  sale: () => t("Sale"),
  delivery: () => t("Delivery"),
};

/** Money fields named without the _minor suffix (dashboard figures are in fils too). */
const MONEY_KEY =
  /(^|\.)(sales|refunds|gross_profit|average_basket|revenue|expected_cash|counted_cash|variance)(_prev)?$/;

function cellValue(k: string, v: unknown): ReactNode {
  if (v === null || v === undefined) return "—";
  if (typeof v === "number" && (k.endsWith("_minor") || MONEY_KEY.test(k))) return formatMoney(v);
  if (typeof v === "number" && k.endsWith("_milli")) return formatQty(v);
  if (typeof v === "boolean") return v ? t("Yes") : t("No");
  if (typeof v === "object") return <code className="tiny">{JSON.stringify(v).slice(0, 60)}</code>;
  return String(v);
}

function rowsOf(v: unknown): Record<string, unknown>[] | null {
  if (Array.isArray(v)) return v.filter((x) => x && typeof x === "object") as Record<string, unknown>[];
  if (v && typeof v === "object") {
    for (const k of ["rows", "items", "data", "steps"]) {
      const x = (v as Record<string, unknown>)[k];
      if (Array.isArray(x)) return x.filter((y) => y && typeof y === "object") as Record<string, unknown>[];
    }
  }
  return null;
}

/** Any read result as a table (lists) or key/value list (records). */
export function ResultView({ value }: { value: unknown }) {
  const rows = rowsOf(value);
  if (rows && rows.length) {
    const cols = Object.keys(rows[0])
      .filter((k) => rows.some((r) => r[k] === null || typeof r[k] !== "object"))
      .slice(0, 8);
    return (
      <div style={{ overflowX: "auto" }}>
        <table className="table">
          <thead>
            <tr>
              {cols.map((c) => (
                <th key={c}>
                  <code className="tiny" dir="ltr">
                    {c}
                  </code>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.slice(0, 50).map((r, i) => (
              <tr key={i}>
                {cols.map((c) => (
                  <td key={c}>{cellValue(c, r[c])}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    );
  }
  if (rows) return <div className="small muted">{t("Nothing found.")}</div>;
  if (value && typeof value === "object") {
    // One level of nesting is shown as "group.field" (e.g. kpis.sales_minor).
    const entries: [string, unknown][] = [];
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      if (v === null || typeof v !== "object") entries.push([k, v]);
      else if (!Array.isArray(v))
        for (const [k2, v2] of Object.entries(v as Record<string, unknown>)) {
          if (v2 === null || typeof v2 !== "object") entries.push([`${k}.${k2}`, v2]);
        }
    }
    return (
      <dl className="kv">
        {entries.slice(0, 40).map(([k, v]) => (
          <div key={k} style={{ display: "contents" }}>
            <dt>
              <code className="tiny" dir="ltr">
                {k}
              </code>
            </dt>
            <dd>{cellValue(k, v)}</dd>
          </div>
        ))}
      </dl>
    );
  }
  return <div>{String(value)}</div>;
}

function SlashResultCard({ r, onClose, onExplain }: { r: AiSlashResult; onClose: () => void; onExplain: () => void }) {
  const def = SLASH.find((s) => s.name === r.command);
  return (
    <div className="card card-pad col gap-8" data-testid="ai-slash-result">
      <div className="row">
        <strong className="grow">
          /{r.command} · {def?.desc()}
        </strong>
        <Button variant="ghost" onClick={onExplain}>
          {t("Explain with the assistant")}
        </Button>
        <button className="icon-btn" aria-label={t("Close")} onClick={onClose}>
          <X size={16} />
        </button>
      </div>
      <div className="tiny muted">
        {t("Read directly with your permissions; the assistant did not see this.")} <code dir="ltr">{r.ran}</code>
        {r.truncated ? ` · ${t("First 50 rows")}` : ""}
      </div>
      <ResultView value={r.result} />
      <details>
        <summary className="tiny">{t("Raw data")}</summary>
        <pre className="tiny" dir="ltr" style={{ whiteSpace: "pre-wrap", maxHeight: 300, overflow: "auto" }}>
          {JSON.stringify(r.result, null, 1)}
        </pre>
      </details>
    </div>
  );
}

// ---------------------------------------------------------------- briefings (A8)

const DAY_LABEL = (): string[] => [t("Mon"), t("Tue"), t("Wed"), t("Thu"), t("Fri"), t("Sat"), t("Sun")];

const TIME_PRESETS = ["07:00", "09:00", "13:00", "18:00", "22:00"];

/** Briefings: playbook tiles, time presets and day toggles (all 48 px, tap only). */
function BriefingForm({ b, onSaved, onCancel }: { b: AiBriefing | null; onSaved: () => void; onCancel: () => void }) {
  const act = useAction();
  const [playbook, setPlaybook] = useState<AiBriefing["playbook"]>(b?.playbook ?? "eod");
  const [name, setName] = useState(b?.name ?? "");
  const [at, setAt] = useState(b?.at_time ?? "22:00");
  const [custom, setCustom] = useState(!!b && !TIME_PRESETS.includes(b.at_time));
  const [days, setDays] = useState(b?.days ?? "1234567");
  const [withAi, setWithAi] = useState(b?.with_ai ?? false);
  const [enabled, setEnabled] = useState(b?.enabled ?? true);
  const label = PLAYBOOKS.find((x) => x.name === playbook)?.label() ?? playbook;
  const toggleDay = (k: string) =>
    setDays(days.includes(k) ? days.replace(k, "") : [...new Set((days + k).split(""))].sort().join(""));
  return (
    <Confirm
      title={b ? t("Edit briefing") : t("New briefing")}
      confirmLabel={t("Save")}
      busy={act.busy}
      error={act.error}
      onCancel={onCancel}
      onConfirm={async () => {
        const r = await act.run(() =>
          api.ai.briefingSave(b?.briefing_id ?? null, {
            name: name.trim() || label,
            playbook,
            at_time: at,
            days: days || "1234567",
            with_ai: withAi,
            enabled,
          }),
        );
        if (r) onSaved();
      }}
    >
      <div className="col gap-16 briefing-form">
        <div className="toggle-group" role="radiogroup" aria-label={t("Playbook")}>
          {PLAYBOOKS.map((p) => (
            <button
              key={p.name}
              type="button"
              role="radio"
              aria-checked={playbook === p.name}
              className={`toggle ${playbook === p.name ? "on" : ""}`}
              onClick={() => setPlaybook(p.name)}
            >
              {p.label()}
            </button>
          ))}
        </div>
        <div>
          <div className="field-label">{t("Time")}</div>
          <div className="toggle-group" role="radiogroup" aria-label={t("Time")}>
            {TIME_PRESETS.map((x) => (
              <button
                key={x}
                type="button"
                role="radio"
                aria-checked={!custom && at === x}
                className={`toggle num ${!custom && at === x ? "on" : ""}`}
                onClick={() => (setCustom(false), setAt(x))}
              >
                {x}
              </button>
            ))}
            <button
              type="button"
              role="radio"
              aria-checked={custom}
              className={`toggle ${custom ? "on" : ""}`}
              onClick={() => setCustom(true)}
            >
              {t("Other time")}
            </button>
          </div>
          {custom ? (
            <input
              className="input"
              style={{ marginTop: 8, maxWidth: 200 }}
              type="time"
              aria-label={t("Time")}
              value={at}
              onChange={(e) => setAt(e.target.value)}
            />
          ) : null}
        </div>
        <div>
          <div className="field-label">{t("Days")}</div>
          <div className="toggle-group days" role="group" aria-label={t("Days")}>
            {DAY_LABEL().map((d, i) => {
              const k = String(i + 1);
              return (
                <button
                  key={k}
                  type="button"
                  aria-pressed={days.includes(k)}
                  className={`toggle ${days.includes(k) ? "on" : ""}`}
                  onClick={() => toggleDay(k)}
                >
                  {d}
                </button>
              );
            })}
          </div>
        </div>
        <div className="toggle-group">
          <button
            type="button"
            aria-pressed={withAi}
            className={`toggle ${withAi ? "on" : ""}`}
            onClick={() => setWithAi(!withAi)}
          >
            {t("Add an AI summary (needs a real provider)")}
          </button>
          <button
            type="button"
            aria-pressed={enabled}
            className={`toggle ${enabled ? "on" : ""}`}
            onClick={() => setEnabled(!enabled)}
          >
            {t("Enabled")}
          </button>
        </div>
        <TextInput
          label={t("Name (optional)")}
          placeholder={label}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <div className="tiny">
          {t(
            "Runs while AMWAPOS is open on this computer, with the permissions of the person who saves it. It only reads.",
          )}
        </div>
      </div>
    </Confirm>
  );
}

function BriefingsPanel() {
  const toast = useToast();
  const list = useLoad(() => api.ai.briefings(), []);
  const notes = useLoad(() => api.ai.notes(30), []);
  const act = useAction();
  const [edit, setEdit] = useState<AiBriefing | "new" | null>(null);
  const label = (p: string) => PLAYBOOKS.find((x) => x.name === p)?.label() ?? p;
  return (
    <div className="col gap-16" data-testid="ai-briefings">
      <div className="card card-pad col gap-8">
        <div className="row">
          <strong className="grow">{t("Scheduled briefings")}</strong>
          <Button icon={<Plus size={16} />} onClick={() => setEdit("new")}>
            {t("New briefing")}
          </Button>
        </div>
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        {!list.data ? (
          <Skeleton />
        ) : list.data.length ? (
          <table className="table">
            <tbody>
              {list.data.map((b) => (
                <tr key={b.briefing_id}>
                  <td>
                    <strong>{b.name}</strong>
                    <div className="tiny">
                      {label(b.playbook)} · {b.at_time} ·{" "}
                      {b.days
                        .split("")
                        .map((d) => DAY_LABEL()[Number(d) - 1])
                        .join(" ")}
                      {b.with_ai ? ` · ${t("AI summary")}` : ""}
                    </div>
                  </td>
                  <td>{b.enabled ? <Chip tone="success">{t("On")}</Chip> : <Chip>{t("Off")}</Chip>}</td>
                  <td className="tiny">{b.last_run_on ? t("Last run {0}", b.last_run_on) : t("Not run yet")}</td>
                  <td className="row">
                    <Button
                      variant="ghost"
                      loading={act.busy}
                      onClick={async () => {
                        if (await act.run(() => api.ai.briefingRun(b.briefing_id))) {
                          toast("success", t("Briefing ran; see the note below."));
                          void notes.reload();
                          void list.reload();
                        }
                      }}
                    >
                      {t("Run now")}
                    </Button>
                    <Button variant="ghost" onClick={() => setEdit(b)}>
                      {t("Edit")}
                    </Button>
                    <Button
                      variant="ghost"
                      onClick={async () => {
                        if (await act.run(() => api.ai.briefingDelete(b.briefing_id))) void list.reload();
                      }}
                    >
                      {t("Delete")}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <div className="small muted">
            {t("No briefings yet. For example: the end-of-day playbook every night at 22:00.")}
          </div>
        )}
      </div>
      <div className="card card-pad col gap-8">
        <strong>{t("Notes")}</strong>
        {!notes.data ? (
          <Skeleton />
        ) : notes.data.length ? (
          notes.data.map((n) => (
            <details
              key={n.note_id}
              onToggle={(e) => {
                if ((e.target as HTMLDetailsElement).open && !n.read_at)
                  void api.ai.noteRead(n.note_id).then(() => notes.reload());
              }}
            >
              <summary className="row">
                <strong className="grow">{n.title}</strong>
                {!n.read_at ? <Chip tone="info">{t("New")}</Chip> : null}
                <Chip tone={n.status === "ok" ? "success" : "warning"}>
                  {n.status === "ok" ? t("Complete") : t("Partial")}
                </Chip>
                <span className="tiny">{formatDateTime(n.created_at)}</span>
              </summary>
              <div className="col gap-8" style={{ marginTop: 8 }}>
                {n.summary ? <div style={{ whiteSpace: "pre-wrap" }}>{n.summary}</div> : null}
                {n.error ? <Banner tone="warning">{tb(n.error)}</Banner> : null}
                <PlaybookResult r={n.data as AiPlaybookResult} onAsk={() => undefined} />
              </div>
            </details>
          ))
        ) : (
          <div className="small muted">{t("No notes yet.")}</div>
        )}
      </div>
      {edit ? (
        <BriefingForm
          b={edit === "new" ? null : edit}
          onCancel={() => setEdit(null)}
          onSaved={() => {
            setEdit(null);
            void list.reload();
          }}
        />
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------- shortcuts (F5)

function ShortcutsHelp({ onClose }: { onClose: () => void }) {
  const rows: [string, string][] = [
    ["Ctrl+K", t("Focus the question box")],
    ["/", t("Start a slash command")],
    ["Enter", t("Send")],
    ["Shift+Enter", t("New line")],
    ["↑", t("Recall the last question (empty box)")],
    ["Tab", t("Complete the highlighted command")],
    ["Esc", t("Close the command list or clear the box")],
    ["Alt+N", t("New conversation")],
    ["Alt+I", t("Action inbox")],
    ["Alt+B", t("Briefings and notes")],
    ["Ctrl+Enter", t("On a focused proposal card: review and confirm")],
    ["Ctrl+Backspace", t("On a focused proposal card: reject")],
    ["?", t("This list")],
  ];
  return (
    <Modal title={t("Keyboard shortcuts")} onClose={onClose} closeOnBackdrop>
      <table className="table">
        <tbody>
          {rows.map(([k, d]) => (
            <tr key={k}>
              <td>
                <kbd dir="ltr">{k}</kbd>
              </td>
              <td>{d}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </Modal>
  );
}

// ---------------------------------------------------------------- the chat

type View = "chat" | "inbox" | "briefings";

const newStreamId = () => `s${Date.now().toString(36)}${Math.random().toString(36).slice(2, 12)}`;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

interface PendingPhoto {
  id: string;
  name: string;
}

/**
 * The assistant, usable on the AI page and in the till (compact). Everything
 * the model does is shown as it happens: thinking, each tool call with its
 * input and result, provider fallbacks and checks.
 */
export function AiChat({
  status,
  compact = false,
  context,
  initialText = "",
}: {
  status: AiStatus;
  compact?: boolean;
  /** Called at send time; the till passes its cart (F6). */
  context?: () => AiContext | null;
  initialText?: string;
}) {
  const { has } = useSession();
  const toast = useToast();
  const nav = useNavigate();
  const list = useLoad(() => api.ai.conversations(), []);
  const [cid, setCid] = useState<string | null>(null);
  const [conv, setConv] = useState<AiConversation | null>(null);
  const [text, setText] = useState(initialText);
  const [view, setView] = useState<View>("chat");
  const [live, setLive] = useState<Live | null>(null);
  const [playbook, setPlaybook] = useState<AiPlaybookResult | null>(null);
  const [slashResults, setSlashResults] = useState<AiSlashResult[]>([]);
  const [photos, setPhotos] = useState<PendingPhoto[]>([]);
  const [attachKind, setAttachKind] = useState<"photo" | "invoice" | "payment">("photo");
  const [renaming, setRenaming] = useState<string | null>(null);
  const [shortcuts, setShortcuts] = useState(false);
  const [paletteIdx, setPaletteIdx] = useState(0);
  const [sendContext, setSendContext] = useState(true);
  // A9: answer language for this viewer (the store setting wins when it is not "ui").
  const [lang, setLang] = useState<"ui" | "en" | "ar">(() => {
    try {
      const v = localStorage.getItem("amwapos.ai.lang");
      return v === "en" || v === "ar" ? v : "ui";
    } catch {
      return "ui";
    }
  });
  const [sheet, setSheet] = useState<null | "model" | "attach" | "history" | "side">(null);
  // C4: the answer at this index came from the free fallback model.
  const [fallbackIdx, setFallbackIdx] = useState<number | null>(null);
  const liveRef = useRef<Live | null>(null);
  const lastQuestion = useRef("");
  const act = useAction();
  const pb = useAction();
  const box = useRef<HTMLTextAreaElement>(null);
  const file = useRef<HTMLInputElement>(null);
  const end = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!cid) return setConv(null);
    void api.ai.conversation(cid).then(setConv, () => setConv(null));
  }, [cid]);
  useEffect(() => {
    // Block body: newer browsers return a Promise here, which is not an effect cleanup.
    void end.current?.scrollIntoView({ block: "end" });
  }, [conv, live, slashResults]);

  const reloadConv = useCallback(async () => {
    if (conv) setConv(await api.ai.conversation(conv.conversation_id));
    void list.reload();
  }, [conv, list]);

  // F1 palette: commands matching what was typed after "/".
  const palette = useMemo(() => {
    if (!text.startsWith("/") || text.includes(" ")) return [];
    const q = text.slice(1).toLowerCase();
    return SLASH.filter((s) => s.name.startsWith(q)).slice(0, 12);
  }, [text]);
  useEffect(() => {
    setPaletteIdx(0);
  }, [palette.length]);

  const ask = useCallback(
    async (question: string) => {
      const q = question.trim();
      if (!q) return;
      lastQuestion.current = q;
      const streamId = newStreamId();
      setLive(emptyLive());
      let stop = false;
      const poll = (async () => {
        let after = 0;
        while (!stop) {
          try {
            const r = await api.ai.stream(streamId, after);
            if (r.events.length) {
              setLive((l) => {
                const n = l ? applyEvents(l, r.events) : l;
                liveRef.current = n;
                return n;
              });
              after = r.next;
            }
            if (r.done) break;
          } catch {
            // The question itself reports errors.
          }
          await sleep(200);
        }
      })();
      const ctx = context && sendContext ? context() : null;
      const r = await act.run(() =>
        api.ai.ask(q, conv?.conversation_id ?? null, lang === "ui" ? getLang() : lang, {
          stream_id: streamId,
          images: photos.map((p) => p.id),
          context: ctx,
        }),
      );
      stop = true;
      await poll;
      const usedFallback = !!liveRef.current?.fallback;
      liveRef.current = null;
      setLive(null);
      if (r) {
        setText("");
        setPhotos([]);
        setFallbackIdx(usedFallback ? r.messages.length - 1 : null);
        setConv(r);
        setCid(r.conversation_id);
        void list.reload();
      }
    },
    [act, context, conv, list, photos, sendContext, lang],
  );

  const resolvePin = async (kind: AiPin["kind"], q: string): Promise<string | null> => {
    if (/^[0-9A-Z]{26}$/.test(q)) return q;
    if (kind === "product") {
      const r = await api.products.search({ q, limit: 1 });
      return r.rows[0]?.product_id ?? null;
    }
    if (kind === "customer") {
      const r = await api.customers.search(q, false, 1);
      return r[0]?.customer_id ?? null;
    }
    return null;
  };

  const runSlash = async (line: string) => {
    const [head, ...rest] = line.slice(1).trim().split(/\s+/);
    const name = (head ?? "").toLowerCase();
    const arg = rest.join(" ");
    const def = SLASH.find((s) => s.name === name);
    setText("");
    if (!def) {
      toast("error", t("Unknown command /{0}. Type /help for the list.", name));
      return;
    }
    if (def.read) {
      const r = await act.run(() => api.ai.slash(name, arg));
      if (r) setSlashResults((x) => [...x.slice(-4), r]);
      return;
    }
    switch (name) {
      case "help":
        setText("/");
        box.current?.focus();
        return;
      case "new":
        setCid(null);
        setConv(null);
        setView("chat");
        return;
      case "rename":
        if (!conv) return toast("info", t("Ask a question first; then name the conversation."));
        if (!arg) return setRenaming(conv.title);
        if (await act.run(() => api.ai.rename(conv.conversation_id, arg))) void reloadConv();
        return;
      case "pin":
      case "unpin": {
        if (!conv) return toast("info", t("Ask a question first; then pin records to it."));
        const [kind, ...q] = arg.split(/\s+/);
        if (!PIN_KINDS.includes(kind as AiPin["kind"]) || !q.length) {
          return toast("info", t("Use /{0} <kind> <name or id>. Kinds: {1}", name, PIN_KINDS.join(", ")));
        }
        const id = name === "pin" ? await resolvePin(kind as AiPin["kind"], q.join(" ")) : q.join(" ");
        if (!id) return toast("error", t("Nothing found to pin."));
        const r = await act.run(() =>
          name === "pin"
            ? api.ai.pin(conv.conversation_id, kind as AiPin["kind"], id)
            : api.ai.unpin(conv.conversation_id, kind as AiPin["kind"], id),
        );
        if (r) void reloadConv();
        return;
      }
      case "price": {
        const m = arg.match(/^(.+)\s+([\d.,٠-٩]+)$/);
        if (!m) return toast("info", t("Use /price <product> <amount>, e.g. /price Tea 100g 0.450"));
        return void ask(t("Set the price of {0} to {1}", m[1], m[2]));
      }
      case "explain": {
        if (!arg) return toast("info", t("Use /explain <command>, e.g. /explain low"));
        return void ask(
          t("Use your tools to look at: {0}. Explain what needs attention and what I should do next.", arg),
        );
      }
      case "ask":
        return void ask(arg);
      case "attach":
        file.current?.click();
        return;
      case "inbox":
        setView("inbox");
        return;
      case "briefings":
        setView("briefings");
        return;
      case "goto": {
        const page = PAGES.find((p) => p === arg.trim().toLowerCase());
        if (!page) return toast("info", t("Pages: {0}", PAGES.join(", ")));
        nav(`/admin/${page}`);
        return;
      }
      case "model":
        toast(
          "info",
          `${providerLabel(status.active_provider)} · ${status.model_id}` +
            (status.daily_token_cap
              ? ` · ${t("Tokens today: {0} of {1}", status.tokens_today ?? 0, status.daily_token_cap)}`
              : ` · ${t("Tokens today: {0}", status.tokens_today ?? 0)}`) +
            (status.fallback_ready ? ` · ${t("Fallback ready")}` : ""),
        );
        return;
      case "shortcuts":
        setShortcuts(true);
        return;
      case "clear":
        setSlashResults([]);
        setPlaybook(null);
        return;
    }
  };

  const submit = () => {
    const v = text.trim();
    if (!v) return;
    if (v.startsWith("/")) void runSlash(v);
    else void ask(v);
  };

  // F5: page-level shortcuts.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const typing = ["INPUT", "TEXTAREA", "SELECT"].includes((e.target as HTMLElement)?.tagName ?? "");
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        box.current?.focus();
      } else if (e.altKey && e.key.toLowerCase() === "n") {
        e.preventDefault();
        setCid(null);
        setConv(null);
        setView("chat");
        box.current?.focus();
      } else if (e.altKey && e.key.toLowerCase() === "i") {
        e.preventDefault();
        setView((v) => (v === "inbox" ? "chat" : "inbox"));
      } else if (e.altKey && e.key.toLowerCase() === "b") {
        e.preventDefault();
        setView((v) => (v === "briefings" ? "chat" : "briefings"));
      } else if (!typing && e.key === "/") {
        e.preventDefault();
        setText("/");
        box.current?.focus();
      } else if (!typing && e.key === "?") {
        setShortcuts(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const attach = async (f: File) => {
    const data = await fileToBase64(f);
    if (attachKind === "photo") {
      const r = await act.run(() => api.ai.attachImage(f.type, data));
      if (r) setPhotos((p) => [...p, { id: r.attachment_id, name: f.name }]);
    } else if (attachKind === "invoice") {
      const r = await act.run(() => api.invoiceScan.import({ file_name: f.name, data }));
      if (r) {
        toast("success", t("Invoice {0} is being read; the assistant can open it when OCR finishes.", r.scan_number));
        setText((x) => `${x}${x ? " " : ""}${t("Check invoice scan {0} and propose receiving it.", r.scan_number)}`);
      }
    } else {
      const r = await act.run(() => api.payreviews.upload({ file_name: f.name, data }));
      if (r) {
        toast("success", t("Payment screenshot {0} added for review.", r.review_number));
        setText((x) => `${x}${x ? " " : ""}${t("Compare payment review {0} with what is owed.", r.review_number)}`);
      }
    }
  };

  const openChat = (id: string | null) => {
    setCid(id);
    if (!id) setConv(null);
    setView("chat");
    setSheet(null);
  };
  const cycleLang = () => {
    const next = lang === "ui" ? "en" : lang === "en" ? "ar" : "ui";
    setLang(next);
    try {
      localStorage.setItem("amwapos.ai.lang", next);
    } catch {
      /* per-viewer convenience only */
    }
  };
  const storeLang = status.settings?.answer_language ?? "ui";
  // Language names a person recognises, not codes: "Same as screen", "English", "العربية".
  const langName = (l: string) => (l === "ar" ? "العربية" : l === "en" ? "English" : t("Same as screen"));
  const langLabel = langName(storeLang !== "ui" ? storeLang : lang);
  const modelName = status.active_provider === "fake" ? t("Offline test model") : providerLabel(status.active_provider);
  const cartCtx = context ? context() : null;
  const openProposals = (conv?.proposals ?? []).filter((p) => p.status === "proposed");
  const otherProposals = (conv?.proposals ?? []).filter((p) => p.status !== "proposed");
  const lastAnswer = [...(conv?.messages ?? [])].reverse().find((m) => m.role === "assistant" && m.text);
  const lastAssistantIdx = (conv?.messages ?? []).reduce((a, m, i) => (m.role === "assistant" ? i : a), -1);

  const rail = (
    <nav className="ai-rail" aria-label={t("Conversations")}>
      <Button
        variant="primary"
        block
        icon={<Plus size={20} />}
        data-testid="ai-new-chat"
        onClick={() => openChat(null)}
      >
        {t("New conversation")}
      </Button>
      <div className="ai-views" role="tablist">
        <button
          type="button"
          role="tab"
          aria-selected={view === "chat"}
          className={`ai-view ${view === "chat" ? "on" : ""}`}
          onClick={() => setView("chat")}
        >
          <MessageSquare size={20} aria-hidden /> {t("Chat")}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={view === "inbox"}
          className={`ai-view ${view === "inbox" ? "on" : ""}`}
          data-testid="ai-inbox-button"
          onClick={() => (setView("inbox"), setSheet(null))}
        >
          <Inbox size={20} aria-hidden /> {t("Action inbox")}
        </button>
        {has("admin.access") ? (
          <button
            type="button"
            role="tab"
            aria-selected={view === "briefings"}
            className={`ai-view ${view === "briefings" ? "on" : ""}`}
            data-testid="ai-briefings-button"
            onClick={() => (setView("briefings"), setSheet(null))}
          >
            <CalendarClock size={20} aria-hidden /> {t("Briefings")}
          </button>
        ) : null}
      </div>
      <div className="ai-convs">
        {(list.data ?? []).map((c) => (
          <button
            key={c.conversation_id}
            type="button"
            className={`ai-conv ${cid === c.conversation_id ? "on" : ""}`}
            onClick={() => openChat(c.conversation_id)}
          >
            <span className="ellipsis c-title" dir="auto">
              {c.title}
            </span>
            <span className="tiny">
              {relative(c.updated_at)}
              {c.open_proposals ? (
                <>
                  {" · "}
                  <span className="c-open">{t("{0} to review", c.open_proposals)}</span>
                </>
              ) : null}
            </span>
          </button>
        ))}
        {list.data && !list.data.length ? (
          <div className="tiny muted ai-convs-empty">{t("No conversations yet.")}</div>
        ) : null}
      </div>
    </nav>
  );

  const proposalsPanel = (inline: boolean) => (
    <>
      {openProposals.map((p) => (
        <ProposalCard key={p.proposal_id} p={p} onChanged={() => void reloadConv()} />
      ))}
      {otherProposals.map((p) => (
        <ProposalCard key={p.proposal_id} p={p} onChanged={() => void reloadConv()} />
      ))}
      {!inline && !conv?.proposals.length ? (
        <div className="side-empty">
          <strong>{t("Proposals")}</strong>
          <p>{t("Changes the assistant prepares appear here. Nothing changes until a person confirms.")}</p>
        </div>
      ) : null}
    </>
  );

  const side = (
    <aside className="ai-side" aria-label={t("Proposals and evidence")}>
      {proposalsPanel(false)}
      {lastAnswer?.evidence?.length ? (
        <section className="side-card">
          <h3>{t("Evidence")}</h3>
          <ul className="ev-list">
            {lastAnswer.evidence.slice(0, 10).map((ev, j) => {
              const to = evidenceLink(ev.tool, ev.ids);
              const body = (
                <>
                  <code dir="ltr">{ev.tool}</code>
                  <span className="tiny">
                    {ev.ids.length ? `${ev.ids.length} · ` : ""}
                    {relative(ev.at)}
                  </span>
                </>
              );
              return <li key={j}>{to ? <Link to={to}>{body}</Link> : <span>{body}</span>}</li>;
            })}
          </ul>
        </section>
      ) : null}
      <section className="side-card" data-testid="ai-playbooks">
        <h3>{t("Playbooks")}</h3>
        <div className="toggle-group">
          {PLAYBOOKS.map((b) => (
            <button
              key={b.name}
              type="button"
              className="toggle"
              disabled={pb.busy}
              onClick={async () => {
                const r = await pb.run(() => api.ai.playbook(b.name));
                if (r) setPlaybook(r);
              }}
            >
              {b.label()}
            </button>
          ))}
        </div>
        <p className="tiny">
          {status.mutations && status.can_mutate
            ? t(
                "The assistant may propose any admin change you are allowed to make. Nothing changes until a person confirms.",
              )
            : t("Read-only: the assistant cannot change anything.")}
        </p>
      </section>
    </aside>
  );

  const suggestions = compact
    ? [t("Which products are running low?"), t("Find a product")]
    : [t("Which products are running low?"), t("How are sales today?"), t("Show the purchase orders")];

  const transcript = (
    <div className="ai-transcript" aria-live="polite">
      {pb.error ? <Banner tone="danger">{pb.error}</Banner> : null}
      {playbook ? <PlaybookResult r={playbook} onAsk={(q) => (setText(q), setPlaybook(null))} /> : null}
      {conv?.untrusted_seen ? (
        <Banner tone="warning">
          {t("This conversation read customer messages, photos or scanned text. Check any proposal carefully.")}
        </Banner>
      ) : null}
      {!conv && !live ? (
        <div className="ai-empty" data-testid="ai-empty">
          <div className="ai-empty-art" aria-hidden>
            <Bot size={40} />
          </div>
          <h2>{compact ? t("Ask about this sale or the store") : t("Ask about sales, stock and purchasing")}</h2>
          <p>{t("The assistant reads with your permissions. Type / for commands.")}</p>
          <div className="toggle-group center">
            {suggestions.map((q) => (
              <button key={q} type="button" className="toggle" onClick={() => void ask(q)}>
                {q}
              </button>
            ))}
          </div>
        </div>
      ) : null}
      {conv?.messages.map((m, i) => (
        <MessageView
          key={i}
          m={m}
          fallback={i === fallbackIdx && i === lastAssistantIdx ? { from: "", to: "" } : null}
        />
      ))}
      {live ? <LiveView live={live} /> : null}
      {compact ? proposalsPanel(true) : null}
      {slashResults.map((r, i) => (
        <SlashResultCard
          key={i}
          r={r}
          onClose={() => setSlashResults((x) => x.filter((_, j) => j !== i))}
          onExplain={() =>
            void ask(
              t(
                "Use your tools to look at: {0}. Explain what needs attention and what I should do next.",
                `/${r.command}`,
              ),
            )
          }
        />
      ))}
      {act.error && !live ? (
        <div className="ai-error" role="alert">
          <XCircle size={20} aria-hidden />
          <span className="grow">{act.error}</span>
          {lastQuestion.current ? (
            <Button icon={<RotateCw size={18} />} onClick={() => void ask(lastQuestion.current)}>
              {t("Retry")}
            </Button>
          ) : null}
        </div>
      ) : null}
      <div ref={end} />
    </div>
  );

  const groups = (["read", "write", "chat"] as const)
    .map((g) => ({ g, items: palette.filter((d) => slashGroup(d) === g) }))
    .filter((x) => x.items.length);
  const flatPalette = groups.flatMap((x) => x.items);

  const composer = (
    <div className="ai-composer">
      {groups.length ? (
        <div className="slash-palette" role="listbox" data-testid="ai-slash-palette" aria-label={t("Commands")}>
          {groups.map(({ g, items }) => (
            <div key={g} role="group" aria-label={SLASH_GROUP_LABEL[g]()}>
              <div className="sp-group">{SLASH_GROUP_LABEL[g]()}</div>
              {items.map((sd) => {
                const i = flatPalette.indexOf(sd);
                return (
                  <button
                    key={sd.name}
                    type="button"
                    role="option"
                    aria-selected={i === paletteIdx}
                    className={`sp-row ${i === paletteIdx ? "on" : ""}`}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      if (sd.args) {
                        setText(`/${sd.name} `);
                        box.current?.focus();
                      } else void runSlash(`/${sd.name}`);
                    }}
                  >
                    <code dir="ltr">
                      /{sd.name}
                      {sd.args ? ` ${sd.args}` : ""}
                    </code>
                    <span className="sp-desc">{sd.desc()}</span>
                  </button>
                );
              })}
            </div>
          ))}
        </div>
      ) : null}
      {conv?.pins?.length || photos.length ? (
        <div className="pin-row">
          {(conv?.pins ?? []).map((p) => (
            <span key={`${p.kind}-${p.id}`} className="pin-chip">
              <Pin size={14} aria-hidden />
              <span className="ellipsis">
                {PIN_LABEL[p.kind]()}: {p.label}
              </span>
              <button
                type="button"
                aria-label={t("Unpin {0}", p.label)}
                onClick={async () => {
                  if (conv && (await act.run(() => api.ai.unpin(conv.conversation_id, p.kind, p.id))))
                    void reloadConv();
                }}
              >
                <X size={16} aria-hidden />
              </button>
            </span>
          ))}
          {photos.map((p) => (
            <span key={p.id} className="pin-chip photo">
              <ImageIcon size={14} aria-hidden />
              <span className="ellipsis">{p.name}</span>
              <button
                type="button"
                aria-label={t("Remove {0}", p.name)}
                onClick={() => setPhotos((x) => x.filter((y) => y.id !== p.id))}
              >
                <X size={16} aria-hidden />
              </button>
            </span>
          ))}
        </div>
      ) : null}
      <textarea
        ref={box}
        className="ai-input"
        rows={1}
        maxLength={4000}
        value={text}
        aria-label={t("Question")}
        placeholder={t("Ask a question, or type / for commands")}
        data-testid="ai-composer"
        onChange={(e) => {
          setText(e.target.value);
          const el = e.target;
          el.style.height = "auto";
          el.style.height = `${Math.min(el.scrollHeight + 2, 124)}px`;
        }}
        onKeyDown={(e) => {
          if (flatPalette.length && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
            e.preventDefault();
            setPaletteIdx((i) => (i + (e.key === "ArrowDown" ? 1 : flatPalette.length - 1)) % flatPalette.length);
          } else if (flatPalette.length && e.key === "Tab") {
            e.preventDefault();
            const sd = flatPalette[paletteIdx];
            setText(`/${sd.name}${sd.args ? " " : ""}`);
          } else if (e.key === "Escape") {
            if (text) {
              e.stopPropagation();
              setText("");
            }
          } else if (e.key === "ArrowUp" && !text && lastQuestion.current) {
            e.preventDefault();
            setText(lastQuestion.current);
          } else if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            if (flatPalette.length) {
              // Complete the highlighted command; run it when it needs nothing more.
              const sd = flatPalette[paletteIdx];
              if (sd.args) return setText(`/${sd.name} `);
              return void runSlash(`/${sd.name}`);
            }
            submit();
          }
        }}
      />
      <div className="ai-actions">
        <input
          ref={file}
          type="file"
          accept="image/png,image/jpeg,image/webp,image/gif"
          hidden
          onChange={(e) => {
            const f = e.target.files?.[0];
            e.target.value = "";
            if (f) void attach(f);
          }}
        />
        <button
          type="button"
          className="ai-act"
          aria-label={t("Attach")}
          onClick={() =>
            has("ocr.scan") || has("payments.review")
              ? setSheet("attach")
              : (setAttachKind("photo"), file.current?.click())
          }
        >
          <Paperclip size={20} aria-hidden />
        </button>
        <button
          type="button"
          className="ai-act"
          aria-label={t("Commands")}
          onClick={() => (setText("/"), box.current?.focus())}
        >
          <Slash size={20} aria-hidden />
        </button>
        <span className="grow" />
        <button
          type="button"
          className="ai-chip"
          data-testid="ai-lang-chip"
          aria-label={t("Answer language: {0}", langLabel)}
          disabled={storeLang !== "ui"}
          onClick={cycleLang}
        >
          <Languages size={16} aria-hidden />
          {langLabel}
        </button>
        <button
          type="button"
          className="ai-chip model"
          data-testid="ai-model-chip"
          aria-label={t("Model: {0} · {1}", modelName, status.model_id)}
          onClick={() => setSheet("model")}
        >
          <Cpu size={16} aria-hidden />
          <span className="ellipsis">{status.active_provider === "fake" ? t("Test model") : status.model_id}</span>
        </button>
        <button
          type="button"
          id="ai-send"
          className="ai-send"
          aria-label={t("Send")}
          disabled={!text.trim() || act.busy}
          onClick={submit}
        >
          {act.busy ? <span className="spinner" aria-hidden /> : <Send size={22} aria-hidden />}
        </button>
      </div>
    </div>
  );

  const sheets = (
    <>
      {sheet === "model" ? (
        <Modal title={t("Model")} size="sheet narrow" onClose={() => setSheet(null)}>
          <div className="col gap-16">
            <dl className="kv">
              <dt>{t("Provider")}</dt>
              <dd>{modelName}</dd>
              <dt>{t("Model")}</dt>
              <dd>
                <code dir="ltr">{status.model_id}</code>
              </dd>
              <dt>{t("Tokens today")}</dt>
              <dd className="num">
                {status.daily_token_cap
                  ? t("{0} of {1}", status.tokens_today ?? 0, status.daily_token_cap)
                  : String(status.tokens_today ?? 0)}
              </dd>
              <dt>{t("Fallback")}</dt>
              <dd>{status.fallback_ready ? t("Ready (OpenRouter free model)") : t("Off")}</dd>
              <dt>{t("Answer language")}</dt>
              <dd>{storeLang !== "ui" ? t("Set by the store: {0}", langName(storeLang)) : langLabel}</dd>
            </dl>
            <Button size="lg" icon={<Keyboard size={20} />} onClick={() => (setSheet(null), setShortcuts(true))}>
              {t("Keyboard shortcuts")}
            </Button>
            {has("settings.manage") ? (
              <Link className="btn lg" to="/admin/settings?section=ai" onClick={() => setSheet(null)}>
                <Settings2 size={20} aria-hidden /> {t("Open Settings → AI")}
              </Link>
            ) : null}
          </div>
        </Modal>
      ) : null}
      {sheet === "attach" ? (
        <Modal title={t("Attach")} size="sheet narrow" onClose={() => setSheet(null)}>
          <div className="more-grid one">
            {[
              { k: "photo" as const, label: t("Photo for the assistant"), icon: <ImageIcon size={20} />, show: true },
              {
                k: "invoice" as const,
                label: t("Supplier invoice (OCR)"),
                icon: <FileScan size={20} />,
                show: has("ocr.scan"),
              },
              {
                k: "payment" as const,
                label: t("Payment screenshot (OCR)"),
                icon: <Receipt size={20} />,
                show: has("payments.review"),
              },
            ]
              .filter((x) => x.show)
              .map((x) => (
                <button
                  key={x.k}
                  type="button"
                  className="more-row"
                  onClick={() => {
                    setAttachKind(x.k);
                    setSheet(null);
                    setTimeout(() => file.current?.click(), 0);
                  }}
                >
                  {x.icon}
                  <span>{x.label}</span>
                </button>
              ))}
          </div>
          <p className="tiny" style={{ marginTop: 12 }}>
            {t("Photos are sent to the AI provider and treated as outside text.")}
          </p>
        </Modal>
      ) : null}
      {sheet === "history" ? (
        <Modal title={t("Conversations")} size="sheet narrow" onClose={() => setSheet(null)}>
          {rail}
        </Modal>
      ) : null}
      {sheet === "side" ? (
        <Modal title={t("Proposals and evidence")} size="sheet narrow" onClose={() => setSheet(null)}>
          {side}
        </Modal>
      ) : null}
      {shortcuts ? <ShortcutsHelp onClose={() => setShortcuts(false)} /> : null}
    </>
  );

  const head = (
    <div className="ai-center-head">
      {conv && renaming !== null ? (
        <form
          className="row grow"
          onSubmit={async (e) => {
            e.preventDefault();
            if (await act.run(() => api.ai.rename(conv.conversation_id, renaming))) {
              setRenaming(null);
              void reloadConv();
            }
          }}
        >
          <input
            className="input"
            autoFocus
            aria-label={t("Conversation name")}
            value={renaming}
            onChange={(e) => setRenaming(e.target.value)}
          />
          <Button type="submit" variant="primary">
            {t("Save")}
          </Button>
          <Button variant="ghost" onClick={() => setRenaming(null)}>
            {t("Cancel")}
          </Button>
        </form>
      ) : (
        <>
          <h2 className="ellipsis grow">
            {view === "inbox"
              ? t("Action inbox")
              : view === "briefings"
                ? t("Briefings")
                : (conv?.title ?? t("New conversation"))}
          </h2>
          {!compact ? (
            <button
              type="button"
              className="ai-act history-toggle"
              aria-label={t("Conversations")}
              onClick={() => setSheet("history")}
            >
              <MessageSquare size={18} aria-hidden />
            </button>
          ) : null}
          {conv && view === "chat" ? (
            <button type="button" className="ai-act" aria-label={t("Rename")} onClick={() => setRenaming(conv.title)}>
              <Pencil size={18} aria-hidden />
            </button>
          ) : null}
          {!compact ? (
            <button
              type="button"
              className="ai-act side-toggle"
              aria-label={t("Proposals and evidence")}
              onClick={() => setSheet("side")}
            >
              <Inbox size={18} aria-hidden />
              {openProposals.length ? <span className="badge">{openProposals.length}</span> : null}
            </button>
          ) : null}
        </>
      )}
    </div>
  );

  if (compact) {
    return (
      <div className="ai-compact">
        <div className="ai-compact-bar">
          {context ? (
            <button
              type="button"
              className={`ctx-chip ${sendContext && cartCtx ? "on" : ""}`}
              aria-pressed={sendContext}
              data-testid="ai-cart-chip"
              onClick={() => setSendContext(!sendContext)}
            >
              <ShoppingCart size={16} aria-hidden />
              {cartCtx
                ? t("Cart: {0} lines · {1}", cartCtx.lines.length, formatMoney(cartCtx.total_minor))
                : t("Cart is empty")}
              <span className="ctx-state">{sendContext ? t("included") : t("not sent")}</span>
            </button>
          ) : null}
          <span className="grow" />
          <button type="button" className="ai-act" aria-label={t("Conversations")} onClick={() => setSheet("history")}>
            <MessageSquare size={20} aria-hidden />
          </button>
        </div>
        {view === "inbox" ? (
          <div className="ai-transcript">
            <ActionInbox onOpen={(id) => openChat(id)} />
          </div>
        ) : view === "briefings" ? (
          <div className="ai-transcript">
            <BriefingsPanel />
          </div>
        ) : (
          transcript
        )}
        {view === "chat" ? composer : null}
        {sheets}
      </div>
    );
  }
  return (
    <div className="ai-page" data-testid="ai-page">
      {rail}
      <section className="ai-center">
        {head}
        {view === "inbox" ? (
          <div className="ai-transcript">
            <ActionInbox onOpen={(id) => openChat(id)} />
          </div>
        ) : view === "briefings" ? (
          <div className="ai-transcript">
            <BriefingsPanel />
          </div>
        ) : (
          transcript
        )}
        {view === "chat" ? composer : null}
      </section>
      {side}
      {sheets}
    </div>
  );
}

/** Setup state shared by the page and the till widget. */
export function AiReady({ children }: { children: (st: AiStatus) => ReactNode }) {
  const { has } = useSession();
  const status = useLoad(() => api.ai.status(), []);
  const st = status.data;
  if (status.error)
    return (
      <div className="ai-state">
        <XCircle size={36} aria-hidden />
        <h2>{t("The assistant could not be reached")}</h2>
        <p>{status.error}</p>
        <Button size="lg" icon={<RotateCw size={20} />} onClick={() => void status.reload()}>
          {t("Retry")}
        </Button>
      </div>
    );
  if (!st) return <Skeleton />;
  if (!st.ready) {
    return (
      <div className="ai-state" data-testid="ai-not-ready">
        <div className="ai-empty-art" aria-hidden>
          <Bot size={40} />
        </div>
        <h2>{!st.key_configured ? t("Add a key in Settings → AI") : t("The assistant is not set up yet")}</h2>
        <p>
          {!st.key_configured
            ? t("No AI provider key is stored.")
            : st.settings.consent === false
              ? t("An owner has not agreed to send store data to the provider.")
              : t("Finish the setup in Settings → AI.")}
        </p>
        {has("settings.manage") ? (
          <Link className="btn primary lg" to="/admin/settings?section=ai">
            <Settings2 size={20} aria-hidden /> {t("Open Settings → AI")}
          </Link>
        ) : (
          <p className="tiny">{t("Ask the owner to finish the setup in Settings → AI.")}</p>
        )}
      </div>
    );
  }
  return <>{children(st)}</>;
}

/** ai.enabled is off: one sentence and the way to turn it on. */
function AiOff() {
  const { has } = useSession();
  return (
    <div className="ai-state" data-testid="ai-off">
      <div className="ai-empty-art" aria-hidden>
        <Bot size={40} />
      </div>
      <h2>{t("The AI assistant is switched off")}</h2>
      <p>{t("Selling, cash, refunds and reports work fully without it.")}</p>
      {has("settings.manage") ? (
        <Link className="btn primary lg" to="/admin/settings?section=features">
          <Settings2 size={20} aria-hidden /> {t("Open Features")}
        </Link>
      ) : (
        <p className="tiny">{t("Ask the owner to enable it in Settings → Features.")}</p>
      )}
    </div>
  );
}

export function AiAssistantPage() {
  const [search] = useSearchParams();
  const on = useFeature("ai.enabled");
  if (!on) return <AiOff />;
  return (
    <>
      {/* The workspace fills the page; the heading is for screen readers and the page outline. */}
      <h1 className="sr-only">{t("AI Assistant")}</h1>
      <AiReady>{(s) => <AiChat status={s} initialText={search.get("q") ?? ""} />}</AiReady>
    </>
  );
}
