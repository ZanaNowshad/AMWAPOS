// Procurement (Wave 4): Suggested orders, requisitions, purchase order
// approval, receiving with differences, the three-way match, supplier
// returns and the supplier catalogue terms (docs/PROCUREMENT.md).
import { useEffect, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import {
  ArrowLeft,
  CheckCircle2,
  ClipboardList,
  PackageCheck,
  Plus,
  Send,
  ShieldCheck,
  Trash2,
  Undo2,
} from "lucide-react";
import { api } from "../../api";
import type {
  PoDetail,
  PosSearchRow,
  ReceiptDiscrepancy,
  ReplenishRow,
  Requisition,
  SupplierCatalogueRow,
  SupplierReturn,
} from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { useApproval, ApprovalCancelled } from "../../components/approval";
import { newOperationId } from "../../lib/ids";
import { formatAmount, formatMoney, formatPercent, formatQty, parseMoney, parseQty } from "../../lib/money";
import { formatDate, formatShort } from "../../lib/time";
import {
  Banner,
  Button,
  Checkbox,
  Chip,
  Empty,
  Field,
  Modal,
  PageHeader,
  Skeleton,
  Tabs,
  TextInput,
} from "../../components/ui";
import { Confirm, DataTable, Drawer, useAction, useLoad } from "./common";
import { getLang, t, tb } from "../../i18n";
import { dateWarnings } from "./stockTruth";

const pname = (r: { product_name?: string; name?: string; product_name_ar?: string | null; name_ar?: string | null }) =>
  (getLang() === "ar" ? (r.product_name_ar ?? r.name_ar) : null) || r.product_name || r.name || "";

// ---------------------------------------------------------------- words

export const STATE_WORDS: Record<string, () => string> = {
  order: () => t("Order now"),
  covered: () => t("Enough stock"),
  insufficient_history: () => t("Not enough sales history"),
  no_demand: () => t("No recent sales"),
  no_supplier: () => t("No supplier"),
  no_lead_time: () => t("Lead time not set"),
  inactive: () => t("Inactive"),
  invalid_pack: () => t("Pack size is not valid"),
};

export const REASON_WORDS: Record<string, () => string> = {
  expired_stock_not_counted: () => t("Expired stock not counted"),
  holds_not_counted: () => t("Held for orders, not counted"),
  on_order: () => t("Some already on order"),
  transfer_on_the_way: () => t("A transfer is on the way"),
  already_requested: () => t("Already requested"),
  at_or_below_reorder_point: () => t("At or below the reorder point"),
  rounded_to_packs: () => t("Rounded up to whole packs"),
  minimum_order: () => t("Supplier minimum order"),
  insufficient_history_using_product_levels: () => t("Little history: using the product's levels"),
  no_demand_using_product_levels: () => t("No recent sales: using the product's levels"),
  expiry_risk: () => t("Some stock expires before the next delivery"),
  high_waste: () => t("High waste recently"),
  no_lead_time: () => t("Lead time not set"),
  preferred_supplier: () => t("Preferred supplier"),
  only_supplier: () => t("Only supplier"),
  lowest_last_cost: () => t("Lowest last cost"),
  shortest_lead_time: () => t("Shortest lead time"),
  has_lead_time: () => t("Has a lead time"),
  first_by_name: () => t("First by name"),
  edited: () => t("Edited by a person"),
};
const reasonWord = (r: string) => REASON_WORDS[r]?.() ?? r;

const REQ_TONE: Record<string, "default" | "info" | "warning" | "success" | "danger"> = {
  draft: "default",
  submitted: "info",
  approved: "success",
  rejected: "danger",
  cancelled: "default",
  converted: "success",
};
export const REQ_WORDS: Record<string, () => string> = {
  draft: () => t("Draft"),
  submitted: () => t("Waiting for approval"),
  approved: () => t("Approved"),
  rejected: () => t("Rejected"),
  cancelled: () => t("Cancelled"),
  converted: () => t("Purchase orders created"),
};

const REJECT_REASONS: [string, () => string][] = [
  ["damaged", () => t("Damaged")],
  ["wrong_item", () => t("Wrong item")],
  ["short_dated", () => t("Short-dated")],
  ["expired", () => t("Expired")],
  ["quality", () => t("Quality")],
  ["not_ordered", () => t("Not ordered")],
  ["other", () => t("Other")],
];
export const RETURN_REASONS: [string, () => string][] = [
  ["damaged", () => t("Damaged")],
  ["incorrect_item", () => t("Incorrect item")],
  ["over_delivery", () => t("Over-delivery")],
  ["short_dated", () => t("Short-dated")],
  ["expired", () => t("Expired")],
  ["quality", () => t("Quality")],
  ["recalled", () => t("Recalled")],
  ["commercial", () => t("Commercial agreement")],
  ["other", () => t("Other")],
];
const words = (list: [string, () => string][], k: string | null) => list.find((x) => x[0] === k)?.[1]() ?? k ?? "";

const DISC_WORDS: Record<string, () => string> = {
  shortage: () => t("Short"),
  overage: () => t("Extra"),
  damaged: () => t("Damaged, kept"),
  rejected: () => t("Refused"),
  substitution: () => t("Substitute"),
};
const RESOLUTION_WORDS: Record<string, () => string> = {
  open: () => t("Needs a decision"),
  backorder: () => t("Kept on order"),
  cancelled: () => t("Cancelled"),
  accepted: () => t("Accepted"),
  rejected: () => t("Refused"),
};
const OUTCOME_WORDS: Record<string, () => string> = {
  matched: () => t("Matched"),
  within_tolerance: () => t("Within tolerance"),
  review: () => t("Needs review"),
  blocked: () => t("Blocked"),
};
const OUTCOME_TONE: Record<string, "success" | "info" | "warning" | "danger"> = {
  matched: "success",
  within_tolerance: "info",
  review: "warning",
  blocked: "danger",
};
const RETURN_WORDS: Record<string, () => string> = {
  draft: () => t("Draft"),
  confirmed: () => t("Sent back, credit expected"),
  credited: () => t("Credited"),
  cancelled: () => t("Cancelled"),
  reversed: () => t("Reversed"),
};

function errText(e: unknown) {
  return e instanceof Error ? tb(e.message) : String(e);
}

// ---------------------------------------------------------------- Suggested orders

const VIEWS: { key: string; label: () => string; states: string[] }[] = [
  { key: "order", label: () => t("To order"), states: ["order"] },
  { key: "decide", label: () => t("Needs a decision"), states: ["no_supplier", "no_lead_time", "invalid_pack"] },
  { key: "covered", label: () => t("Enough stock"), states: ["covered"] },
  { key: "unknown", label: () => t("Cannot tell yet"), states: ["insufficient_history", "no_demand"] },
];

export function SuggestedOrdersPage() {
  const nav = useNavigate();
  const toast = useToast();
  const { has } = useSession();
  const [view, setView] = useState("order");
  const [search, setSearch] = useState("");
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [why, setWhy] = useState<ReplenishRow | null>(null);
  const [opId, setOpId] = useState(newOperationId);
  const states = VIEWS.find((v) => v.key === view)!.states;
  const data = useLoad(() => api.procurement.suggestions({ states, search: search || null }), [view, search]);
  const act = useAction();
  const counts = data.data?.counts ?? {};
  const create = async () => {
    const r = await act.run(() => api.requisitions.fromSuggestions([...sel], opId));
    if (r) {
      toast(
        "success",
        t("Requisition {0} created", r.number),
        r.skipped?.length ? t("{0} no longer need ordering", r.skipped.length) : undefined,
      );
      setSel(new Set());
      setOpId(newOperationId());
      nav(`/admin/requisitions/${r.requisition_id}`);
    }
  };
  return (
    <div>
      <PageHeader
        title={t("Suggested orders")}
        subtitle={t(
          "What to order, from whom and why, worked out from sales, stock, orders on the way and each supplier's terms. Nothing is ordered until a person creates a requisition and a purchase order.",
        )}
        actions={
          has("requisitions.create") && view === "order" ? (
            <Button
              variant="primary"
              icon={<ClipboardList size={16} />}
              disabled={sel.size === 0}
              loading={act.busy}
              onClick={create}
            >
              {t("Create requisition ({0})", sel.size)}
            </Button>
          ) : null
        }
      />
      <div className="row wrap gap-8" style={{ marginBottom: 12 }}>
        <Tabs
          tabs={VIEWS.map((v) => ({
            key: v.key,
            label: `${v.label()} (${v.states.reduce((a, s) => a + (counts[s] ?? 0), 0)})`,
          }))}
          value={view}
          onChange={(v) => {
            setView(v);
            setSel(new Set());
          }}
        />
        <TextInput label={t("Search")} value={search} onChange={(e) => setSearch(e.target.value)} />
      </div>
      {data.data ? (
        <div className="tiny" style={{ marginBottom: 8 }}>
          {t(
            "Demand over the last {0} days · safety {1} days · orders every {2} days (Settings → Purchasing).",
            data.data.settings.demand_window_days,
            data.data.settings.safety_days,
            data.data.settings.order_cycle_days,
          )}
        </div>
      ) : null}
      {data.error ? <Banner tone="danger">{data.error}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <DataTable<ReplenishRow>
        rows={data.data?.rows ?? null}
        loading={data.loading}
        rowKey={(r) => r.product_id}
        selectable={view === "order" && has("requisitions.create")}
        selected={sel}
        onSelect={setSel}
        onRowClick={setWhy}
        empty={<Empty title={view === "order" ? t("Nothing needs ordering now") : t("Nothing here")} />}
        columns={[
          { key: "p", label: t("Product"), render: (r) => <span dir="auto">{pname(r)}</span> },
          { key: "u", label: t("Usable"), num: true, render: (r) => formatQty(r.usable_milli) },
          {
            key: "i",
            label: t("On the way"),
            num: true,
            render: (r) => (r.inbound_milli ? formatQty(r.inbound_milli) : "—"),
          },
          {
            key: "d",
            label: t("Sells a day"),
            num: true,
            render: (r) => (r.days_used >= 7 ? formatQty(r.per_day_milli) : "—"),
          },
          {
            key: "rp",
            label: t("Reorder at"),
            num: true,
            render: (r) => (r.reorder_point_milli !== null ? formatQty(r.reorder_point_milli) : "—"),
          },
          {
            key: "s",
            label: t("Supplier"),
            render: (r) =>
              r.supplier ? (
                <span dir="auto">
                  {r.supplier.supplier_name}
                  {r.alternatives.length ? (
                    <span className="tiny"> {t("+{0} more", r.alternatives.length)}</span>
                  ) : null}
                </span>
              ) : (
                "—"
              ),
          },
          {
            key: "q",
            label: t("Suggested"),
            num: true,
            render: (r) =>
              r.state === "order"
                ? r.packs !== null && r.supplier?.units_per_case
                  ? t("{0} × {1} = {2}", r.packs, r.supplier.units_per_case, formatQty(r.suggested_milli))
                  : formatQty(r.suggested_milli)
                : "—",
          },
          {
            key: "c",
            label: t("Estimated cost"),
            num: true,
            render: (r) => (r.estimated_cost_minor !== null ? formatMoney(r.estimated_cost_minor) : "—"),
          },
          {
            key: "st",
            label: t("Why"),
            render: (r) => (
              <div className="row wrap" style={{ gap: 4 }}>
                <Chip tone={r.state === "order" ? "info" : r.state === "covered" ? "success" : "warning"}>
                  {STATE_WORDS[r.state]?.() ?? r.state}
                </Chip>
                {r.warnings.map((w) => (
                  <Chip key={w} tone="warning">
                    {reasonWord(w)}
                  </Chip>
                ))}
              </div>
            ),
          },
        ]}
      />
      {why ? <SuggestionDrawer row={why} onClose={() => setWhy(null)} /> : null}
    </div>
  );
}

function SuggestionDrawer({ row: r, onClose }: { row: ReplenishRow; onClose: () => void }) {
  const f = r.facts;
  const line = (label: string, v: string) => (
    <tr>
      <td>{label}</td>
      <td className="num">{v}</td>
    </tr>
  );
  return (
    <Drawer title={pname(r)} onClose={onClose}>
      <div className="col gap-12">
        <Chip tone={r.state === "order" ? "info" : "default"}>{STATE_WORDS[r.state]?.() ?? r.state}</Chip>
        <table className="table">
          <tbody>
            {line(t("On hand"), formatQty(f.on_hand_milli))}
            {f.held_milli ? line(t("Held for orders"), `− ${formatQty(f.held_milli)}`) : null}
            {f.expired_use_by_milli ? line(t("Expired (use by)"), `− ${formatQty(f.expired_use_by_milli)}`) : null}
            {line(t("Usable"), formatQty(r.usable_milli))}
            {f.open_po_milli ? line(t("On order"), `+ ${formatQty(f.open_po_milli)}`) : null}
            {f.draft_po_milli ? line(t("On draft purchase orders"), `+ ${formatQty(f.draft_po_milli)}`) : null}
            {f.transfers_in_milli ? line(t("Transfers on the way"), `+ ${formatQty(f.transfers_in_milli)}`) : null}
            {f.requisitioned_milli
              ? line(t("Requested, not yet ordered"), `+ ${formatQty(f.requisitioned_milli)}`)
              : null}
            {line(t("Stock position"), formatQty(r.position_milli))}
            {line(t("Sold (net) over {0} days", r.days_used), formatQty(f.net_sold_milli))}
            {r.reorder_point_milli !== null
              ? line(
                  r.reorder_point_source === "product"
                    ? t("Reorder point (set on the product)")
                    : t("Reorder point (demand × lead time and safety)"),
                  formatQty(r.reorder_point_milli),
                )
              : null}
            {r.order_up_to_milli !== null
              ? line(
                  r.order_up_to_source === "max_stock" ? t("Order up to (maximum stock)") : t("Order up to"),
                  formatQty(r.order_up_to_milli),
                )
              : null}
            {r.need_milli ? line(t("Needed"), formatQty(r.need_milli)) : null}
            {r.state === "order" ? line(t("Suggested"), formatQty(r.suggested_milli)) : null}
          </tbody>
        </table>
        {r.reasons.length ? (
          <div className="row wrap" style={{ gap: 4 }}>
            {r.reasons.map((x) => (
              <Chip key={x}>{reasonWord(x)}</Chip>
            ))}
          </div>
        ) : null}
        {r.supplier ? (
          <div className="col gap-4">
            <h4>{t("Supplier")}</h4>
            <div dir="auto">
              {r.supplier.supplier_name} · {r.supplier_reason ? reasonWord(r.supplier_reason) : ""}
            </div>
            <div className="tiny">
              {t(
                "Pack {0} · minimum {1} · lead time {2}",
                r.supplier.units_per_case ?? "—",
                r.supplier.moq_packs ?? "—",
                r.supplier.lead_time_days !== null ? t("{0} days", r.supplier.lead_time_days) : "—",
              )}
            </div>
            {r.alternatives.length ? (
              <>
                <h4>{t("Other suppliers")}</h4>
                {r.alternatives.map((a) => (
                  <div key={a.supplier_id} className="tiny" dir="auto">
                    {a.supplier_name} ·{" "}
                    {t(
                      "Pack {0} · minimum {1} · lead time {2}",
                      a.units_per_case ?? "—",
                      a.moq_packs ?? "—",
                      a.lead_time_days !== null ? t("{0} days", a.lead_time_days) : "—",
                    )}
                    {a.last_cost_minor !== null ? ` · ${formatMoney(a.last_cost_minor)}` : ""}
                  </div>
                ))}
              </>
            ) : null}
          </div>
        ) : null}
      </div>
    </Drawer>
  );
}

// ---------------------------------------------------------------- Requisitions

export function RequisitionsPage() {
  const nav = useNavigate();
  const [params] = useSearchParams();
  const [status, setStatus] = useState(params.get("status") ?? "");
  const data = useLoad(() => api.requisitions.list(status || null), [status]);
  return (
    <div>
      <PageHeader
        title={t("Requisitions")}
        subtitle={t(
          "Requests to buy, before any order. Approved requisitions become draft purchase orders, one per supplier.",
        )}
        actions={
          <Button icon={<Plus size={16} />} onClick={() => nav("/admin/suggested-orders")}>
            {t("From suggested orders")}
          </Button>
        }
      />
      <div className="row wrap gap-8" style={{ marginBottom: 12 }}>
        <select className="select" value={status} onChange={(e) => setStatus(e.target.value)} aria-label={t("Status")}>
          <option value="">{t("All")}</option>
          {Object.keys(REQ_WORDS).map((s) => (
            <option key={s} value={s}>
              {REQ_WORDS[s]()}
            </option>
          ))}
        </select>
      </div>
      {data.error ? <Banner tone="danger">{data.error}</Banner> : null}
      <DataTable
        rows={data.data?.rows ?? null}
        loading={data.loading}
        rowKey={(r) => r.requisition_id}
        onRowClick={(r) => nav(`/admin/requisitions/${r.requisition_id}`)}
        empty={<Empty title={t("No requisitions yet")} />}
        columns={[
          { key: "n", label: t("Number"), render: (r) => <span dir="ltr">{r.number}</span> },
          {
            key: "s",
            label: t("Status"),
            render: (r) => <Chip tone={REQ_TONE[r.status]}>{REQ_WORDS[r.status]()}</Chip>,
          },
          { key: "l", label: t("Lines"), num: true, render: (r) => r.line_count },
          { key: "sp", label: t("Suppliers"), num: true, render: (r) => r.supplier_count },
          { key: "b", label: t("Requested by"), render: (r) => r.created_by_name ?? "—" },
          { key: "d", label: t("Created"), render: (r) => formatShort(r.created_at) },
        ]}
      />
    </div>
  );
}

type ReqEdit = {
  line_id: string | null;
  product_id: string;
  name: string;
  supplier_id: string;
  qty: string;
  cost: string;
};

export function RequisitionDetailPage() {
  const { id } = useParams();
  const nav = useNavigate();
  const toast = useToast();
  const { has } = useSession();
  const data = useLoad(() => api.requisitions.get(id!), [id]);
  const suppliers = useLoad(() => api.suppliers.list(), []);
  const act = useAction();
  const [edit, setEdit] = useState<ReqEdit[] | null>(null);
  const [reject, setReject] = useState<string | null>(null);
  const [convertOp] = useState(newOperationId);
  const [ev, setEv] = useState<Record<string, unknown> | null>(null);
  const [search, setSearch] = useState("");
  const [results, setResults] = useState<PosSearchRow[]>([]);
  const r = data.data;
  if (!r) return data.error ? <Banner tone="danger">{data.error}</Banner> : <Skeleton rows={8} />;
  const done = (x: Requisition | undefined, msg: string) => {
    if (x) {
      toast("success", msg);
      setEdit(null);
      void data.reload();
    }
  };
  const startEdit = () =>
    setEdit(
      r.lines.map((l) => ({
        line_id: l.line_id,
        product_id: l.product_id,
        name: pname(l),
        supplier_id: l.supplier_id ?? "",
        qty: formatQty(l.qty_milli),
        cost: l.unit_cost_minor !== null ? formatAmount(l.unit_cost_minor) : "",
      })),
    );
  const saveEdit = async () => {
    if (!edit) return;
    const x = await act.run(() =>
      api.requisitions.save(r.requisition_id, {
        note: r.note,
        expected_version: r.version,
        lines: edit.map((l) => ({
          line_id: l.line_id,
          product_id: l.product_id,
          supplier_id: l.supplier_id || null,
          qty_milli: parseQty(l.qty) ?? 0,
          unit_cost_minor: l.cost.trim() ? (parseMoney(l.cost) ?? -1) : null,
        })),
      }),
    );
    done(x, t("Requisition saved"));
  };
  const searchProducts = (q: string) => {
    setSearch(q);
    if (!q.trim()) return setResults([]);
    api.pos
      .search(q, { limit: 8 })
      .then(setResults)
      .catch(() => {});
  };
  return (
    <div>
      <div className="page-header">
        <Button
          variant="ghost"
          icon={<ArrowLeft size={18} />}
          aria-label={t("Back")}
          onClick={() => nav("/admin/requisitions")}
        />
        <div className="grow">
          <div className="tiny">{t("Requisition")}</div>
          <h1>
            <span dir="ltr">{r.number}</span> <Chip tone={REQ_TONE[r.status]}>{REQ_WORDS[r.status]()}</Chip>
          </h1>
        </div>
        {r.status === "draft" && has("requisitions.create") && !edit ? (
          <Button onClick={startEdit}>{t("Edit")}</Button>
        ) : null}
        {edit ? (
          <>
            <Button onClick={() => setEdit(null)}>{t("Cancel")}</Button>
            <Button variant="primary" loading={act.busy} onClick={saveEdit}>
              {t("Save")}
            </Button>
          </>
        ) : null}
        {r.status === "draft" && has("requisitions.create") && !edit ? (
          <Button
            variant="primary"
            icon={<Send size={16} />}
            loading={act.busy}
            onClick={async () =>
              done(await act.run(() => api.requisitions.act(r.requisition_id, "submit")), t("Submitted for approval"))
            }
          >
            {t("Submit")}
          </Button>
        ) : null}
        {r.status === "submitted" && has("purchasing.approve") ? (
          <>
            <Button variant="danger-outline" onClick={() => setReject("")}>
              {t("Reject")}
            </Button>
            <Button
              variant="primary"
              icon={<ShieldCheck size={16} />}
              loading={act.busy}
              onClick={async () =>
                done(await act.run(() => api.requisitions.act(r.requisition_id, "approve")), t("Requisition approved"))
              }
            >
              {t("Approve")}
            </Button>
          </>
        ) : null}
        {r.status === "approved" && has("purchasing.manage") ? (
          <Button
            variant="primary"
            icon={<PackageCheck size={16} />}
            loading={act.busy}
            onClick={async () =>
              done(
                await act.run(() => api.requisitions.convert(r.requisition_id, convertOp)),
                t("Purchase orders created"),
              )
            }
          >
            {t("Create purchase orders")}
          </Button>
        ) : null}
        {["draft", "submitted", "approved"].includes(r.status) &&
        (has("requisitions.create") || has("purchasing.approve")) &&
        !edit ? (
          <Button
            variant="ghost"
            onClick={async () =>
              done(await act.run(() => api.requisitions.act(r.requisition_id, "cancel")), t("Requisition cancelled"))
            }
          >
            {t("Cancel requisition")}
          </Button>
        ) : null}
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {r.decision_note ? (
        <Banner tone={r.status === "rejected" ? "danger" : "info"}>
          {t("Decision by {0}: {1}", r.decided_by_name ?? "—", r.decision_note)}
        </Banner>
      ) : null}
      {r.status === "approved" ? (
        <Banner tone="info">
          {t(
            "Every line needs a supplier and a cost. The whole requisition becomes purchase orders at once; to order only part, cancel it and create a new one.",
          )}
        </Banner>
      ) : null}
      <div className="card">
        {edit ? (
          <div className="card-head">
            <div style={{ position: "relative", width: 360 }}>
              <input
                className="input"
                placeholder={t("Add product by name, SKU or barcode…")}
                value={search}
                onChange={(e) => searchProducts(e.target.value)}
                aria-label={t("Add product")}
              />
              {results.length ? (
                <div className="menu" style={{ insetInline: 0 }}>
                  {results.map((p) => (
                    <button
                      key={p.product_id}
                      onClick={() => {
                        if (!edit.some((l) => l.product_id === p.product_id))
                          setEdit([
                            ...edit,
                            {
                              line_id: null,
                              product_id: p.product_id,
                              name: p.name,
                              supplier_id: "",
                              qty: "1",
                              cost: "",
                            },
                          ]);
                        setSearch("");
                        setResults([]);
                      }}
                    >
                      {p.name}
                    </button>
                  ))}
                </div>
              ) : null}
            </div>
          </div>
        ) : null}
        <table className="table">
          <thead>
            <tr>
              <th>{t("Product")}</th>
              <th>{t("Supplier")}</th>
              <th className="num">{t("Quantity")}</th>
              <th className="num">{t("Unit cost")}</th>
              <th>{t("From")}</th>
              <th>{t("Purchase order")}</th>
              {edit ? <th /> : null}
            </tr>
          </thead>
          <tbody>
            {edit
              ? edit.map((l, i) => (
                  <tr key={l.product_id}>
                    <td dir="auto">{l.name}</td>
                    <td>
                      <select
                        className="select"
                        value={l.supplier_id}
                        aria-label={t("Supplier")}
                        onChange={(e) =>
                          setEdit(edit.map((x, j) => (j === i ? { ...x, supplier_id: e.target.value } : x)))
                        }
                      >
                        <option value="">{t("Choose supplier…")}</option>
                        {(suppliers.data ?? []).map((s) => (
                          <option key={s.supplier_id} value={s.supplier_id}>
                            {s.name}
                          </option>
                        ))}
                      </select>
                    </td>
                    <td className="num">
                      <input
                        className="input num"
                        style={{ width: 100 }}
                        value={l.qty}
                        aria-label={t("Quantity")}
                        onChange={(e) => setEdit(edit.map((x, j) => (j === i ? { ...x, qty: e.target.value } : x)))}
                      />
                    </td>
                    <td className="num">
                      <input
                        className="input num"
                        style={{ width: 100 }}
                        value={l.cost}
                        aria-label={t("Unit cost")}
                        onChange={(e) => setEdit(edit.map((x, j) => (j === i ? { ...x, cost: e.target.value } : x)))}
                      />
                    </td>
                    <td />
                    <td />
                    <td>
                      <Button
                        size="sm"
                        variant="ghost"
                        aria-label={t("Remove line")}
                        icon={<Trash2 size={14} />}
                        onClick={() => setEdit(edit.filter((_, j) => j !== i))}
                      />
                    </td>
                  </tr>
                ))
              : r.lines.map((l) => (
                  <tr key={l.line_id}>
                    <td dir="auto">{pname(l)}</td>
                    <td dir="auto">{l.supplier_name ?? <Chip tone="warning">{t("No supplier")}</Chip>}</td>
                    <td className="num">
                      {l.packs !== null && l.units_per_case
                        ? t("{0} × {1} = {2}", l.packs, l.units_per_case, formatQty(l.qty_milli))
                        : formatQty(l.qty_milli)}
                    </td>
                    <td className="num">{l.unit_cost_minor !== null ? formatMoney(l.unit_cost_minor) : "—"}</td>
                    <td>
                      {l.source === "replenishment" ? (
                        <button className="link" onClick={() => setEv(l.evidence)}>
                          {t("Suggested")}
                          {l.evidence && (l.evidence as { edited?: boolean }).edited
                            ? ` · ${reasonWord("edited")}`
                            : ""}
                        </button>
                      ) : (
                        t("Typed by a person")
                      )}
                    </td>
                    <td>
                      {l.po_id ? (
                        <button className="link" onClick={() => nav(`/admin/purchase-orders/${l.po_id}`)}>
                          {l.po_number}
                        </button>
                      ) : (
                        "—"
                      )}
                    </td>
                  </tr>
                ))}
          </tbody>
          {r.estimated_total_minor !== null && !edit ? (
            <tfoot>
              <tr>
                <td colSpan={3}>{t("Estimated total")}</td>
                <td className="num">{formatMoney(r.estimated_total_minor)}</td>
                <td colSpan={2} />
              </tr>
            </tfoot>
          ) : null}
        </table>
      </div>
      {ev ? (
        <Drawer title={t("Why it was suggested")} onClose={() => setEv(null)}>
          <EvidenceView ev={ev} />
        </Drawer>
      ) : null}
      {reject !== null ? (
        <Confirm
          title={t("Reject requisition")}
          confirmLabel={t("Reject")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setReject(null)}
          onConfirm={async () => {
            const x = await act.run(() => api.requisitions.act(r.requisition_id, "reject", reject));
            if (x) setReject(null);
            done(x, t("Requisition rejected"));
          }}
        >
          <TextInput label={t("Why")} value={reject} onChange={(e) => setReject(e.target.value)} />
        </Confirm>
      ) : null}
    </div>
  );
}

function EvidenceView({ ev }: { ev: Record<string, unknown> }) {
  const n = (k: string) => (typeof ev[k] === "number" ? formatQty(ev[k] as number) : "—");
  const reasons = (ev.reasons as string[] | undefined) ?? [];
  return (
    <div className="col gap-8">
      <table className="table">
        <tbody>
          <tr>
            <td>{t("Usable")}</td>
            <td className="num">{n("usable_milli")}</td>
          </tr>
          <tr>
            <td>{t("On the way")}</td>
            <td className="num">{n("inbound_milli")}</td>
          </tr>
          <tr>
            <td>{t("Stock position")}</td>
            <td className="num">{n("position_milli")}</td>
          </tr>
          <tr>
            <td>{t("Sells a day")}</td>
            <td className="num">{n("per_day_milli")}</td>
          </tr>
          <tr>
            <td>{t("Reorder at")}</td>
            <td className="num">{n("reorder_point_milli")}</td>
          </tr>
          <tr>
            <td>{t("Order up to")}</td>
            <td className="num">{n("order_up_to_milli")}</td>
          </tr>
          <tr>
            <td>{t("Suggested")}</td>
            <td className="num">{n("suggested_milli")}</td>
          </tr>
        </tbody>
      </table>
      <div className="row wrap" style={{ gap: 4 }}>
        {reasons.map((x) => (
          <Chip key={x}>{reasonWord(x)}</Chip>
        ))}
      </div>
      <div className="tiny">{t("Worked out on {0}.", String(ev.today ?? ""))}</div>
    </div>
  );
}

// ---------------------------------------------------------------- Purchase order: approval

export function PoApprovalCard({ po, onChange }: { po: PoDetail; onChange: () => void }) {
  const { has } = useSession();
  const act = useAction();
  const [op, setOp] = useState(newOperationId);
  const a = po.approval;
  if (!a || (!a.required && a.history.length === 0)) return null;
  return (
    <div className="card card-pad col gap-8" style={{ marginBottom: 16 }} data-testid="po-approval">
      <div className="row">
        <h3 className="grow">{t("Approval")}</h3>
        {po.status === "draft" ? (
          a.valid ? (
            <Chip tone="success">{t("Approved as it is")}</Chip>
          ) : a.required ? (
            <Chip tone="warning">{t("Needs approval before ordering")}</Chip>
          ) : null
        ) : null}
        {po.status === "draft" && !a.valid && has("purchasing.approve") ? (
          <Button
            variant="primary"
            icon={<ShieldCheck size={16} />}
            loading={act.busy}
            onClick={async () => {
              const r = await act.run(() => api.po.approve(po.po_id, op));
              if (r) {
                setOp(newOperationId());
                onChange();
              }
            }}
          >
            {t("Approve")}
          </Button>
        ) : null}
      </div>
      <div className="tiny">
        {a.mode === "always"
          ? t("Every purchase order needs approval.")
          : a.mode === "above_threshold"
            ? t("Purchase orders above {0} need approval.", formatMoney(a.threshold_minor))
            : t("Approval is off.")}{" "}
        {t("Changing the supplier, lines, quantities, costs or taxes after approval needs a new approval.")}
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {a.history.map((h) => (
        <div key={h.approval_id} className="tiny">
          {t(
            "Approved by {0} on {1} at {2}",
            h.approved_by_name ?? "—",
            formatShort(h.approved_at),
            formatMoney(h.total_minor),
          )}
          {h.invalidated_at ? ` · ${t("no longer valid: the order was changed")}` : ""}
        </div>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------- Purchase order: receiving

type RecvState = {
  accepted: string;
  cost: string;
  rejected: string;
  reason: string;
  damaged: string;
  substitute: string;
  substituteName: string;
  acceptSub: boolean;
  acceptOver: boolean;
  lot: string;
  expiry: string;
  shortage: "backorder" | "cancel";
};

export function PoReceivePanel({ po, onDone, onCancel }: { po: PoDetail; onDone: () => void; onCancel: () => void }) {
  const toast = useToast();
  const approval = useApproval();
  const [ref, setRef] = useState("");
  const [op, setOp] = useState(newOperationId);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [subFor, setSubFor] = useState<string | null>(null);
  // Suspicious batch dates come back as warnings; pressing again keeps them.
  const [dateWarns, setDateWarns] = useState<string[] | null>(null);
  const [st, setSt] = useState<Record<string, RecvState>>(() =>
    Object.fromEntries(
      po.lines.map((l) => [
        l.po_item_id,
        {
          accepted: l.qty_remaining_milli > 0 ? formatQty(l.qty_remaining_milli) : "",
          cost: formatAmount(l.unit_cost_minor),
          rejected: "",
          reason: "damaged",
          damaged: "",
          substitute: "",
          substituteName: "",
          acceptSub: false,
          acceptOver: false,
          lot: "",
          expiry: "",
          shortage: "backorder",
        },
      ]),
    ),
  );
  const set = (id: string, p: Partial<RecvState>) => setSt({ ...st, [id]: { ...st[id], ...p } });
  const submit = async () => {
    const lines = po.lines
      .map((l) => {
        const s = st[l.po_item_id];
        const acc = parseQty(s.accepted) ?? 0;
        const rej = parseQty(s.rejected) ?? 0;
        return {
          po_item_id: l.po_item_id,
          qty_milli: acc,
          unit_cost_minor: parseMoney(s.cost),
          rejected: rej > 0 ? [{ qty_milli: rej, reason: s.reason }] : [],
          damaged_kept_milli: parseQty(s.damaged) ?? 0,
          substitute_product_id: s.substitute || null,
          accept_substitution: s.acceptSub,
          accept_overage: s.acceptOver,
          lot:
            s.lot.trim() || s.expiry
              ? { supplier_lot_code: s.lot.trim() || null, expires_on: s.expiry || null, confirm_warnings: !!dateWarns }
              : null,
        };
      })
      .filter((l) => l.qty_milli > 0 || l.rejected.length > 0);
    const shortages = po.lines
      .filter((l) => {
        const acc = parseQty(st[l.po_item_id].accepted) ?? 0;
        return l.qty_remaining_milli - acc > 0;
      })
      .map((l) => ({ po_item_id: l.po_item_id, decision: st[l.po_item_id].shortage }));
    setBusy(true);
    setError(null);
    try {
      const r = await approval((tok) =>
        api.po.receive({
          po_id: po.po_id,
          reference: ref || null,
          lines,
          shortages,
          operation_id: op,
          approval_token: tok,
        }),
      );
      toast(
        "success",
        t("Goods received"),
        r.status === "received" ? t("Purchase order fully received") : t("Partial delivery recorded"),
      );
      setOp(newOperationId());
      setDateWarns(null);
      onDone();
    } catch (e) {
      const w = dateWarnings(e);
      if (w) setDateWarns(w);
      else if (!(e instanceof ApprovalCancelled)) setError(errText(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="card" data-testid="po-receive">
      <div className="card-head">
        <h3 className="grow">{t("Receive goods")}</h3>
        <input
          className="input"
          style={{ width: 240 }}
          placeholder={t("Delivery note / invoice ref")}
          value={ref}
          onChange={(e) => setRef(e.target.value)}
        />
      </div>
      <div className="card-body tiny">
        {t(
          "Only what you accept becomes stock. Refused goods go back with the driver: they are not stock and not waste.",
        )}
      </div>
      <div className="table-wrap">
        <table className="table">
          <thead>
            <tr>
              <th>{t("Product")}</th>
              <th className="num">{t("Still expected")}</th>
              <th className="num">{t("Accepted")}</th>
              <th className="num">{t("Refused")}</th>
              <th className="num">{t("Damaged, kept")}</th>
              <th className="num">{t("Unit cost")}</th>
              <th>{t("Batch / expiry")}</th>
              <th>{t("If short")}</th>
            </tr>
          </thead>
          <tbody>
            {po.lines.map((l) => {
              const s = st[l.po_item_id];
              const acc = parseQty(s.accepted) ?? 0;
              const over = acc > l.qty_remaining_milli;
              const short = l.qty_remaining_milli - acc > 0;
              const costDiff = parseMoney(s.cost);
              return (
                <tr key={l.po_item_id}>
                  <td>
                    <span dir="auto">{l.product_name}</span>
                    {s.substitute ? (
                      <div className="tiny">
                        {t("Received instead: {0}", s.substituteName)}{" "}
                        <Checkbox
                          label={t("Accept the substitute")}
                          checked={s.acceptSub}
                          onChange={(v) => set(l.po_item_id, { acceptSub: v })}
                        />
                      </div>
                    ) : (
                      <div>
                        <button className="link tiny" onClick={() => setSubFor(l.po_item_id)}>
                          {t("Another product came instead")}
                        </button>
                      </div>
                    )}
                  </td>
                  <td className="num">{formatQty(l.qty_remaining_milli)}</td>
                  <td className="num">
                    <input
                      className="input num"
                      style={{ width: 90 }}
                      value={s.accepted}
                      aria-label={t("Accepted {0}", l.product_name)}
                      onChange={(e) => set(l.po_item_id, { accepted: e.target.value })}
                    />
                    {over ? (
                      <div className="tiny">
                        <Checkbox
                          label={t("Keep the extra")}
                          checked={s.acceptOver}
                          onChange={(v) => set(l.po_item_id, { acceptOver: v })}
                        />
                      </div>
                    ) : null}
                  </td>
                  <td className="num">
                    <input
                      className="input num"
                      style={{ width: 80 }}
                      value={s.rejected}
                      aria-label={t("Refused {0}", l.product_name)}
                      onChange={(e) => set(l.po_item_id, { rejected: e.target.value })}
                    />
                    {parseQty(s.rejected) ? (
                      <select
                        className="select"
                        value={s.reason}
                        aria-label={t("Why refused")}
                        onChange={(e) => set(l.po_item_id, { reason: e.target.value })}
                      >
                        {REJECT_REASONS.map(([k, w]) => (
                          <option key={k} value={k}>
                            {w()}
                          </option>
                        ))}
                      </select>
                    ) : null}
                  </td>
                  <td className="num">
                    <input
                      className="input num"
                      style={{ width: 80 }}
                      value={s.damaged}
                      aria-label={t("Damaged, kept {0}", l.product_name)}
                      onChange={(e) => set(l.po_item_id, { damaged: e.target.value })}
                    />
                  </td>
                  <td className="num">
                    <input
                      className="input num"
                      style={{ width: 100 }}
                      value={s.cost}
                      aria-label={t("Cost {0}", l.product_name)}
                      onChange={(e) => set(l.po_item_id, { cost: e.target.value })}
                    />
                    {costDiff !== null && costDiff !== l.unit_cost_minor ? (
                      <div className="tiny">{t("Order cost {0}", formatMoney(l.unit_cost_minor))}</div>
                    ) : null}
                  </td>
                  <td>
                    <input
                      className="input"
                      style={{ width: 110 }}
                      placeholder={t("Batch")}
                      value={s.lot}
                      aria-label={t("Batch {0}", l.product_name)}
                      onChange={(e) => set(l.po_item_id, { lot: e.target.value })}
                    />
                    <input
                      className="input"
                      type="date"
                      style={{ width: 150 }}
                      value={s.expiry}
                      aria-label={t("Expiry {0}", l.product_name)}
                      onChange={(e) => set(l.po_item_id, { expiry: e.target.value })}
                    />
                  </td>
                  <td>
                    {short ? (
                      <select
                        className="select"
                        value={s.shortage}
                        aria-label={t("If short {0}", l.product_name)}
                        onChange={(e) => set(l.po_item_id, { shortage: e.target.value as "backorder" | "cancel" })}
                      >
                        <option value="backorder">{t("Keep on order")}</option>
                        <option value="cancel">{t("Cancel the rest")}</option>
                      </select>
                    ) : (
                      "—"
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {dateWarns ? (
        <Banner tone="warning" title={t("Check the dates")}>
          {dateWarns.map((w) => (
            <div key={w} dir="auto">
              {tb(w)}
            </div>
          ))}
          <div className="tiny">{t("Press Receive goods again to keep these dates.")}</div>
        </Banner>
      ) : null}
      <div className="card-body row">
        <Button onClick={onCancel}>{t("Cancel")}</Button>
        <Button variant="primary" className="right" onClick={submit} loading={busy}>
          {t("Receive goods")}
        </Button>
      </div>
      {subFor ? (
        <ProductPicker
          title={t("Product received instead")}
          onClose={() => setSubFor(null)}
          onPick={(p) => {
            set(subFor, { substitute: p.product_id, substituteName: p.name, acceptSub: false });
            setSubFor(null);
          }}
        />
      ) : null}
    </div>
  );
}

function ProductPicker({
  title,
  onClose,
  onPick,
}: {
  title: string;
  onClose: () => void;
  onPick: (p: PosSearchRow) => void;
}) {
  const [q, setQ] = useState("");
  const [rows, setRows] = useState<PosSearchRow[]>([]);
  return (
    <Modal title={title} onClose={onClose}>
      <div className="col gap-8">
        <input
          className="input"
          autoFocus
          placeholder={t("Search products…")}
          value={q}
          onChange={(e) => {
            setQ(e.target.value);
            api.pos
              .search(e.target.value, { limit: 10 })
              .then(setRows)
              .catch(() => {});
          }}
          aria-label={t("Search products…")}
        />
        {rows.map((p) => (
          <button key={p.product_id} className="list-row" onClick={() => onPick(p)}>
            {p.name} <span className="tiny">{p.sku}</span>
          </button>
        ))}
      </div>
    </Modal>
  );
}

export function PoDiscrepanciesCard({ po, onChange }: { po: PoDetail; onChange: () => void }) {
  const { has } = useSession();
  const act = useAction();
  if (!po.discrepancies.length) return null;
  return (
    <div className="card" style={{ marginTop: 16 }} data-testid="po-discrepancies">
      <div className="card-head">
        <h3>{t("Delivery differences")}</h3>
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <table className="table">
        <tbody>
          {po.discrepancies.map((d: ReceiptDiscrepancy) => (
            <tr key={d.discrepancy_id}>
              <td>{formatShort(d.created_at)}</td>
              <td>
                <Chip tone={d.kind === "shortage" && d.resolution === "open" ? "warning" : "default"}>
                  {DISC_WORDS[d.kind]()}
                </Chip>
              </td>
              <td dir="auto">
                {pname(d)}
                {d.substitute_name ? ` → ${d.substitute_name}` : ""}
              </td>
              <td className="num">{formatQty(d.qty_milli)}</td>
              <td>{d.reason ? words(REJECT_REASONS, d.reason) : ""}</td>
              <td>{RESOLUTION_WORDS[d.resolution]()}</td>
              <td>
                {d.kind === "shortage" &&
                (d.resolution === "open" || d.resolution === "backorder") &&
                has("purchasing.manage") ? (
                  <div className="row" style={{ gap: 4 }}>
                    {d.resolution === "open" ? (
                      <Button
                        size="sm"
                        onClick={async () =>
                          (await act.run(() => api.po.decideShortage(d.discrepancy_id, "backorder"))) && onChange()
                        }
                      >
                        {t("Keep on order")}
                      </Button>
                    ) : null}
                    <Button
                      size="sm"
                      variant="danger-outline"
                      onClick={async () =>
                        (await act.run(() => api.po.decideShortage(d.discrepancy_id, "cancel"))) && onChange()
                      }
                    >
                      {t("Cancel the rest")}
                    </Button>
                  </div>
                ) : null}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// ---------------------------------------------------------------- Three-way match

export function InvoiceMatchCard({ invoiceId, onChange }: { invoiceId: string; onChange?: () => void }) {
  const { has } = useSession();
  const data = useLoad(() => api.invoiceMatch.get(invoiceId), [invoiceId]);
  const act = useAction();
  const [note, setNote] = useState<string | null>(null);
  const m = data.data?.match;
  if (!data.data) return data.error ? <Banner tone="danger">{data.error}</Banner> : null;
  if (!m) return null;
  const acc = data.data.acceptance;
  return (
    <div className="card card-pad col gap-8" data-testid="invoice-match">
      <div className="row">
        <h3 className="grow">{t("Order, goods received and invoice")}</h3>
        <Chip tone={OUTCOME_TONE[m.outcome]}>{OUTCOME_WORDS[m.outcome]()}</Chip>
      </div>
      <div className="tiny">
        {t("Purchase order {0}.", m.po_number)}{" "}
        {m.tolerance
          ? t(
              "A cost difference is within tolerance when it is at most {0} on the line and {1} on the unit cost.",
              formatMoney(m.tolerance.cost_tolerance_minor),
              formatPercent(m.tolerance.cost_tolerance_bp),
            )
          : ""}
      </div>
      <div className="table-wrap">
        <table className="table">
          <thead>
            <tr>
              <th>{t("Product")}</th>
              <th className="num">{t("Ordered")}</th>
              <th className="num">{t("Received")}</th>
              <th className="num">{t("Invoiced")}</th>
              <th className="num">{t("Order cost")}</th>
              <th className="num">{t("Invoice cost")}</th>
              <th className="num">{t("Last confirmed")}</th>
              <th className="num">{t("Typical")}</th>
              <th>{t("Result")}</th>
            </tr>
          </thead>
          <tbody>
            {m.lines.map((l, i) => (
              <tr key={i}>
                <td dir="auto">{l.product_name ?? t("Not matched to a product")}</td>
                <td className="num">{l.ordered_milli !== null ? formatQty(l.ordered_milli) : "—"}</td>
                <td className="num">{l.received_milli !== null ? formatQty(l.received_milli) : "—"}</td>
                <td className="num">
                  {l.invoiced_milli !== null ? formatQty(l.invoiced_milli) : "—"}
                  {l.invoiced_elsewhere_milli ? (
                    <div className="tiny">{t("+ {0} on other invoices", formatQty(l.invoiced_elsewhere_milli))}</div>
                  ) : null}
                </td>
                <td className="num">{l.po_cost_minor !== null ? formatMoney(l.po_cost_minor) : "—"}</td>
                <td className="num">{l.invoice_cost_minor !== null ? formatMoney(l.invoice_cost_minor) : "—"}</td>
                <td className="num">{l.last_cost_minor !== null ? formatMoney(l.last_cost_minor) : "—"}</td>
                <td className="num">{l.typical_cost_minor !== null ? formatMoney(l.typical_cost_minor) : "—"}</td>
                <td>
                  <Chip tone={OUTCOME_TONE[l.outcome] ?? "default"}>{OUTCOME_WORDS[l.outcome]?.() ?? l.outcome}</Chip>
                  {l.notes.map((n, j) => (
                    <div key={j} className="tiny">
                      {tb(n)}
                    </div>
                  ))}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {m.outcome === "blocked" ? (
        <Banner tone="danger">
          {t(
            "The invoice charges for more than was received. Receive the goods, or ask the supplier for a corrected invoice.",
          )}
        </Banner>
      ) : null}
      {acc ? (
        <div className="tiny">
          {t(
            "Differences accepted by {0} on {1}: {2}",
            acc.by ?? "—",
            acc.at ? formatShort(acc.at) : "—",
            acc.note ?? "",
          )}
          {!acc.still_applies ? ` · ${t("no longer applies: the evidence changed")}` : ""}
        </div>
      ) : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {m.outcome === "review" &&
      (!acc || !acc.still_applies) &&
      data.data.posting === "not_posted" &&
      has("purchasing.approve") ? (
        note === null ? (
          <Button icon={<CheckCircle2 size={16} />} onClick={() => setNote("")}>
            {t("Accept the differences")}
          </Button>
        ) : (
          <div className="row gap-8">
            <TextInput label={t("Why they are accepted")} value={note} onChange={(e) => setNote(e.target.value)} />
            <Button
              variant="primary"
              loading={act.busy}
              disabled={!note.trim()}
              onClick={async () => {
                const r = await act.run(() => api.invoiceMatch.accept(invoiceId, note));
                if (r) {
                  setNote(null);
                  void data.reload();
                  onChange?.();
                }
              }}
            >
              {t("Accept")}
            </Button>
          </div>
        )
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------- Supplier catalogue terms

export function SupplierTermsCard({ supplierId }: { supplierId: string }) {
  const { has } = useSession();
  const data = useLoad(() => api.procurement.catalogue({ supplier_id: supplierId }), [supplierId]);
  const [edit, setEdit] = useState<SupplierCatalogueRow | null>(null);
  return (
    <div className="card" data-testid="supplier-terms">
      <div className="card-head">
        <h3 className="grow">{t("Products and terms")}</h3>
      </div>
      <div className="card-body tiny">
        {t(
          "Pack size, minimum order and lead time used by Suggested orders. Terms a person confirms here win over what documents say.",
        )}
      </div>
      {data.error ? <Banner tone="danger">{data.error}</Banner> : null}
      <DataTable<SupplierCatalogueRow>
        rows={data.data?.rows ?? null}
        loading={data.loading}
        rowKey={(r) => r.terms.product_id}
        onRowClick={has("suppliers.manage") ? setEdit : undefined}
        empty={<div className="empty">{t("No products from this supplier yet.")}</div>}
        columns={[
          { key: "p", label: t("Product"), render: (r) => <span dir="auto">{pname(r)}</span> },
          {
            key: "u",
            label: t("Pack"),
            num: true,
            render: (r) =>
              r.terms.units_per_case ? (
                <span>
                  {r.terms.units_per_case}{" "}
                  <span className="tiny">
                    {r.terms.pack_source === "person" ? t("(confirmed)") : t("(from documents)")}
                  </span>
                </span>
              ) : (
                "—"
              ),
          },
          { key: "m", label: t("Minimum order"), num: true, render: (r) => r.terms.moq_packs ?? "—" },
          {
            key: "l",
            label: t("Lead time"),
            num: true,
            render: (r) => (r.terms.lead_time_days !== null ? t("{0} days", r.terms.lead_time_days) : "—"),
          },
          {
            key: "pr",
            label: t("Preferred"),
            render: (r) => (r.terms.preferred ? <Chip tone="success">{t("Preferred")}</Chip> : ""),
          },
          {
            key: "c",
            label: t("Last confirmed cost"),
            num: true,
            render: (r) => (r.costs?.last_confirmed_minor != null ? formatMoney(r.costs.last_confirmed_minor) : "—"),
          },
          { key: "a", label: t("Status"), render: (r) => (r.terms.active ? "" : <Chip>{t("Inactive")}</Chip>) },
        ]}
      />
      {edit ? (
        <TermsEditor
          row={edit}
          onClose={() => setEdit(null)}
          onSaved={() => {
            setEdit(null);
            void data.reload();
          }}
        />
      ) : null}
    </div>
  );
}

function TermsEditor({
  row,
  onClose,
  onSaved,
}: {
  row: SupplierCatalogueRow;
  onClose: () => void;
  onSaved: () => void;
}) {
  const tm = row.terms;
  const [f, setF] = useState({
    code: tm.supplier_code ?? "",
    upc: tm.units_per_case ? String(tm.units_per_case) : "",
    moq: tm.moq_packs ? String(tm.moq_packs) : "",
    lead: tm.lead_time_days !== null ? String(tm.lead_time_days) : "",
    preferred: tm.preferred,
    active: tm.active,
  });
  const act = useAction();
  const int = (v: string) => (v.trim() === "" ? null : Number.isFinite(Number(v)) ? Math.trunc(Number(v)) : NaN);
  return (
    <Drawer
      title={pname(row)}
      onClose={onClose}
      actions={
        <Button
          variant="primary"
          loading={act.busy}
          onClick={async () => {
            const r = await act.run(() =>
              api.procurement.saveTerms({
                supplier_id: tm.supplier_id,
                product_id: tm.product_id,
                supplier_code: f.code.trim() || null,
                units_per_case: int(f.upc),
                moq_packs: int(f.moq),
                lead_time_days: int(f.lead),
                preferred: f.preferred,
                active: f.active,
                expected_version: tm.version,
              }),
            );
            if (r) onSaved();
          }}
        >
          {t("Save")}
        </Button>
      }
    >
      <div className="col gap-12">
        <TextInput
          label={t("Supplier item code")}
          value={f.code}
          onChange={(e) => setF({ ...f, code: e.target.value })}
        />
        <TextInput
          label={t("Units in one pack")}
          inputMode="numeric"
          value={f.upc}
          onChange={(e) => setF({ ...f, upc: e.target.value })}
        />
        <TextInput
          label={t("Minimum order (packs)")}
          inputMode="numeric"
          value={f.moq}
          onChange={(e) => setF({ ...f, moq: e.target.value })}
        />
        <TextInput
          label={t("Lead time (days)")}
          inputMode="numeric"
          value={f.lead}
          onChange={(e) => setF({ ...f, lead: e.target.value })}
        />
        <Checkbox
          label={t("Preferred supplier for this product")}
          checked={f.preferred}
          onChange={(v) => setF({ ...f, preferred: v })}
        />
        <Checkbox
          label={t("Order this product from this supplier")}
          checked={f.active}
          onChange={(v) => setF({ ...f, active: v })}
        />
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        {row.document_evidence.length ? (
          <div className="col gap-4">
            <h4>{t("How this supplier's documents name it")}</h4>
            {row.document_evidence.map((d, i) => (
              <div key={i} className="tiny" dir="auto">
                {d.kind === "code" ? t("Item code") : t("Description")}: {d.key}
                {d.units_per_case ? ` · ${t("pack {0}", d.units_per_case)}` : ""} · {t("{0} times", d.uses)}
              </div>
            ))}
          </div>
        ) : null}
        {row.costs ? (
          <div className="tiny">
            {t(
              "Last confirmed cost {0} · typical {1} (median of the last {2} receipts)",
              row.costs.last_confirmed_minor !== null ? formatMoney(row.costs.last_confirmed_minor) : "—",
              row.costs.typical_minor !== null ? formatMoney(row.costs.typical_minor) : "—",
              row.costs.typical_receipts,
            )}
          </div>
        ) : null}
      </div>
    </Drawer>
  );
}

/** Product editor card: maximum stock (what suggested orders fill up to). */
export function ProductMaxStockCard({ productId }: { productId: string }) {
  const { has } = useSession();
  const s = useLoad(
    () => api.procurement.suggestions({ product_ids: [productId], states: Object.keys(STATE_WORDS) }),
    [productId],
  );
  const act = useAction();
  const row = s.data?.rows[0];
  const [max, setMax] = useState<string | null>(null);
  if (!row) return null;
  const current = row.facts.max_stock_milli;
  const value = max ?? (current !== null ? formatQty(current) : "");
  return (
    <div className="card card-pad col gap-8" data-testid="product-replenish">
      <h3>{t("Ordering")}</h3>
      <div className="row wrap gap-8">
        <Chip tone={row.state === "order" ? "info" : "default"}>{STATE_WORDS[row.state]?.() ?? row.state}</Chip>
        {row.state === "order" ? (
          <span className="tiny">{t("Suggested: {0}", formatQty(row.suggested_milli))}</span>
        ) : null}
      </div>
      {has("products.manage") ? (
        <div className="row gap-8">
          <TextInput label={t("Maximum stock (order up to)")} value={value} onChange={(e) => setMax(e.target.value)} />
          <Button
            loading={act.busy}
            disabled={max === null}
            onClick={async () => {
              const q = value.trim() ? parseQty(value) : null;
              const r = await act.run(() => api.procurement.maxStock(productId, q ?? null));
              if (r) {
                setMax(null);
                void s.reload();
              }
            }}
          >
            {t("Save")}
          </Button>
        </div>
      ) : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
    </div>
  );
}

// ---------------------------------------------------------------- Supplier returns

export function SupplierReturnsPage() {
  const nav = useNavigate();
  const { has } = useSession();
  const [params] = useSearchParams();
  const [status, setStatus] = useState(params.get("status") ?? "");
  const data = useLoad(() => api.supplierReturns.list(status || null), [status]);
  return (
    <div>
      <PageHeader
        title={t("Supplier returns")}
        subtitle={t(
          "Goods sent back to a supplier, and the credit expected for them. Goods refused at delivery are not returns.",
        )}
        actions={
          has("supplier_returns.manage") ? (
            <Button variant="primary" icon={<Plus size={16} />} onClick={() => nav("/admin/supplier-returns/new")}>
              {t("New return")}
            </Button>
          ) : null
        }
      />
      <div className="row wrap gap-8" style={{ marginBottom: 12 }}>
        <select className="select" value={status} onChange={(e) => setStatus(e.target.value)} aria-label={t("Status")}>
          <option value="">{t("All")}</option>
          {Object.keys(RETURN_WORDS).map((s) => (
            <option key={s} value={s}>
              {RETURN_WORDS[s]()}
            </option>
          ))}
        </select>
      </div>
      {data.error ? <Banner tone="danger">{data.error}</Banner> : null}
      <DataTable
        rows={data.data?.rows ?? null}
        loading={data.loading}
        rowKey={(r) => r.return_id}
        onRowClick={(r) => nav(`/admin/supplier-returns/${r.return_id}`)}
        empty={<Empty title={t("No supplier returns")} />}
        columns={[
          { key: "n", label: t("Number"), render: (r) => <span dir="ltr">{r.number}</span> },
          { key: "s", label: t("Supplier"), render: (r) => <span dir="auto">{r.supplier_name}</span> },
          { key: "st", label: t("Status"), render: (r) => <Chip>{RETURN_WORDS[r.status]()}</Chip> },
          { key: "l", label: t("Lines"), num: true, render: (r) => r.line_count },
          {
            key: "c",
            label: t("Expected credit"),
            num: true,
            render: (r) => (r.expected_credit_minor !== null ? formatMoney(r.expected_credit_minor) : "—"),
          },
          { key: "d", label: t("Created"), render: (r) => formatShort(r.created_at) },
        ]}
      />
    </div>
  );
}

type RetLine = {
  product_id: string;
  name: string;
  lot_id: string;
  qty: string;
  reason: string;
  lots: { lot_id: string; label: string }[];
};

export function SupplierReturnEditorPage() {
  const { id } = useParams();
  const isNew = !id || id === "new";
  const nav = useNavigate();
  const toast = useToast();
  const { has } = useSession();
  const suppliers = useLoad(() => api.suppliers.list(), []);
  const data = useLoad(() => (isNew ? Promise.resolve(null) : api.supplierReturns.get(id!)), [id]);
  const act = useAction();
  const [supplier, setSupplier] = useState("");
  const [note, setNote] = useState("");
  const [lines, setLines] = useState<RetLine[]>([]);
  const [picking, setPicking] = useState(false);
  const [createOp] = useState(newOperationId);
  const [confirmOp] = useState(newOperationId);
  const [reverse, setReverse] = useState<string | null>(null);
  const [reverseOp] = useState(newOperationId);
  const [credit, setCredit] = useState<{ number: string; date: string } | null>(null);
  const r: SupplierReturn | null = data.data ?? null;
  const editable = isNew || r?.status === "draft";
  useEffect(() => {
    if (!r || r.status !== "draft") return;
    setSupplier(r.supplier_id);
    setNote(r.note ?? "");
    setLines(
      r.lines.map((l) => ({
        product_id: l.product_id,
        name: pname(l),
        lot_id: l.lot_id ?? "",
        qty: formatQty(l.qty_milli),
        reason: l.reason,
        lots: l.lot_id ? [{ lot_id: l.lot_id, label: l.supplier_lot_code ?? l.lot_number ?? "" }] : [],
      })),
    );
  }, [r]);
  if (!isNew && !r) return data.error ? <Banner tone="danger">{data.error}</Banner> : <Skeleton rows={8} />;
  const addProduct = async (p: PosSearchRow) => {
    setPicking(false);
    if (lines.some((l) => l.product_id === p.product_id)) return;
    let lots: { lot_id: string; label: string }[];
    try {
      const d = await api.lots.product(p.product_id);
      lots = d.lots
        .filter((l) => l.balance_milli > 0)
        .map((l) => ({
          lot_id: l.lot_id,
          label: `${l.supplier_lot_code ?? l.lot_number}${l.expires_on ? ` · ${formatDate(l.expires_on)}` : ""}`,
        }));
    } catch {
      lots = [];
    }
    setLines([...lines, { product_id: p.product_id, name: p.name, lot_id: "", qty: "1", reason: "damaged", lots }]);
  };
  const save = async () => {
    const x = await act.run(() =>
      api.supplierReturns.save(isNew ? null : r!.return_id, {
        supplier_id: supplier,
        note: note || null,
        operation_id: isNew ? createOp : null,
        lines: lines.map((l) => ({
          product_id: l.product_id,
          lot_id: l.lot_id || null,
          qty_milli: parseQty(l.qty) ?? 0,
          reason: l.reason,
        })),
      }),
    );
    if (x) {
      toast("success", t("Return {0} saved", x.number));
      if (isNew) nav(`/admin/supplier-returns/${x.return_id}`, { replace: true });
      else void data.reload();
    }
  };
  const after = (x: SupplierReturn | undefined, msg: string) => {
    if (x) {
      toast("success", msg);
      void data.reload();
    }
  };
  return (
    <div>
      <div className="page-header">
        <Button
          variant="ghost"
          icon={<ArrowLeft size={18} />}
          aria-label={t("Back")}
          onClick={() => nav("/admin/supplier-returns")}
        />
        <div className="grow">
          <div className="tiny">{t("Supplier return")}</div>
          <h1>
            {isNew ? t("New return") : <span dir="ltr">{r!.number}</span>}{" "}
            {r ? <Chip>{RETURN_WORDS[r.status]()}</Chip> : null}
          </h1>
        </div>
        {editable && has("supplier_returns.manage") ? (
          <Button onClick={save} loading={act.busy} disabled={!supplier || lines.length === 0}>
            {t("Save Draft")}
          </Button>
        ) : null}
        {r?.status === "draft" && has("supplier_returns.manage") ? (
          <>
            <Button
              variant="ghost"
              onClick={async () =>
                after(await act.run(() => api.supplierReturns.cancel(r.return_id)), t("Return cancelled"))
              }
            >
              {t("Cancel return")}
            </Button>
            <Button
              variant="primary"
              icon={<Send size={16} />}
              loading={act.busy}
              onClick={async () =>
                after(
                  await act.run(() => api.supplierReturns.confirm(r.return_id, confirmOp)),
                  t("Goods sent back; stock updated"),
                )
              }
            >
              {t("Confirm: goods leave stock")}
            </Button>
          </>
        ) : null}
        {r?.status === "confirmed" && has("supplier_returns.manage") ? (
          <>
            {!r.credit_invoice_id ? (
              <Button onClick={() => setCredit({ number: "", date: new Date().toISOString().slice(0, 10) })}>
                {t("Record the credit note")}
              </Button>
            ) : null}
            <Button variant="danger-outline" icon={<Undo2 size={16} />} onClick={() => setReverse("")}>
              {t("Reverse")}
            </Button>
          </>
        ) : null}
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {r && r.status !== "draft" ? (
        <div className="card card-pad row wrap gap-16" style={{ marginBottom: 16 }}>
          <div>
            <div className="tiny">{t("Expected credit")}</div>
            <strong>{r.expected_credit_minor !== null ? formatMoney(r.expected_credit_minor) : "—"}</strong>
          </div>
          <div>
            <div className="tiny">{t("Supplier's credit note")}</div>
            <strong>
              {r.credit_note
                ? `${r.credit_note.supplier_number ?? r.credit_note.number} · ${r.credit_note.posting === "posted" ? t("posted") : t("not posted yet")}`
                : "—"}
            </strong>
          </div>
          <div>
            <div className="tiny">{t("Credited")}</div>
            <strong>{r.actual_credit_minor !== null ? formatMoney(r.actual_credit_minor) : "—"}</strong>
          </div>
          {r.credit_difference_minor ? (
            <div>
              <div className="tiny">{t("Difference")}</div>
              <strong>{formatMoney(r.credit_difference_minor)}</strong>
            </div>
          ) : null}
          {r.reversal_reason ? <div className="tiny">{t("Reversed: {0}", r.reversal_reason)}</div> : null}
        </div>
      ) : null}
      <div className="card card-pad" style={{ marginBottom: 16 }}>
        <div className="form-grid">
          <Field label={t("Supplier")} required>
            <select
              className="select"
              value={supplier || r?.supplier_id || ""}
              disabled={!editable}
              onChange={(e) => setSupplier(e.target.value)}
            >
              <option value="">{t("Choose supplier…")}</option>
              {(suppliers.data ?? []).map((s) => (
                <option key={s.supplier_id} value={s.supplier_id}>
                  {s.name}
                </option>
              ))}
            </select>
          </Field>
          <TextInput
            label={t("Notes")}
            value={editable ? note : (r?.note ?? "")}
            disabled={!editable}
            onChange={(e) => setNote(e.target.value)}
          />
        </div>
      </div>
      <div className="card">
        <div className="card-head">
          <h3 className="grow">{t("Items")}</h3>
          {editable ? (
            <Button icon={<Plus size={16} />} onClick={() => setPicking(true)}>
              {t("Add product")}
            </Button>
          ) : null}
        </div>
        <table className="table">
          <thead>
            <tr>
              <th>{t("Product")}</th>
              <th>{t("Batch")}</th>
              <th className="num">{t("Quantity")}</th>
              <th>{t("Reason")}</th>
              {!editable ? <th className="num">{t("Value")}</th> : null}
              {editable ? <th /> : null}
            </tr>
          </thead>
          <tbody>
            {editable
              ? lines.map((l, i) => (
                  <tr key={l.product_id}>
                    <td dir="auto">{l.name}</td>
                    <td>
                      <select
                        className="select"
                        value={l.lot_id}
                        aria-label={t("Batch")}
                        onChange={(e) =>
                          setLines(lines.map((x, j) => (j === i ? { ...x, lot_id: e.target.value } : x)))
                        }
                      >
                        <option value="">{t("Not in a batch")}</option>
                        {l.lots.map((b) => (
                          <option key={b.lot_id} value={b.lot_id}>
                            {b.label}
                          </option>
                        ))}
                      </select>
                    </td>
                    <td className="num">
                      <input
                        className="input num"
                        style={{ width: 90 }}
                        value={l.qty}
                        aria-label={t("Quantity")}
                        onChange={(e) => setLines(lines.map((x, j) => (j === i ? { ...x, qty: e.target.value } : x)))}
                      />
                    </td>
                    <td>
                      <select
                        className="select"
                        value={l.reason}
                        aria-label={t("Reason")}
                        onChange={(e) =>
                          setLines(lines.map((x, j) => (j === i ? { ...x, reason: e.target.value } : x)))
                        }
                      >
                        {RETURN_REASONS.map(([k, w]) => (
                          <option key={k} value={k}>
                            {w()}
                          </option>
                        ))}
                      </select>
                    </td>
                    <td>
                      <Button
                        size="sm"
                        variant="ghost"
                        aria-label={t("Remove line")}
                        icon={<Trash2 size={14} />}
                        onClick={() => setLines(lines.filter((_, j) => j !== i))}
                      />
                    </td>
                  </tr>
                ))
              : r!.lines.map((l) => (
                  <tr key={l.line_id}>
                    <td dir="auto">{pname(l)}</td>
                    <td dir="ltr">{l.supplier_lot_code ?? l.lot_number ?? "—"}</td>
                    <td className="num">{formatQty(l.qty_milli)}</td>
                    <td>{words(RETURN_REASONS, l.reason)}</td>
                    <td className="num">{l.value_minor !== null ? formatMoney(l.value_minor) : "—"}</td>
                  </tr>
                ))}
          </tbody>
        </table>
        {editable && lines.length === 0 ? (
          <div className="empty">{t("Add the products going back to the supplier.")}</div>
        ) : null}
      </div>
      {picking ? (
        <ProductPicker title={t("Add product")} onClose={() => setPicking(false)} onPick={addProduct} />
      ) : null}
      {reverse !== null && r ? (
        <Confirm
          title={t("Reverse the return")}
          confirmLabel={t("Reverse")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setReverse(null)}
          onConfirm={async () => {
            const x = await act.run(() => api.supplierReturns.reverse(r.return_id, reverse, reverseOp));
            if (x) setReverse(null);
            after(x, t("Return reversed; the goods are back in stock"));
          }}
        >
          <TextInput label={t("Why")} value={reverse} onChange={(e) => setReverse(e.target.value)} />
        </Confirm>
      ) : null}
      {credit && r ? (
        <Confirm
          title={t("Record the credit note")}
          confirmLabel={t("Create credit note")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setCredit(null)}
          onConfirm={async () => {
            const x = await act.run(() => api.supplierReturns.draftCredit(r.return_id, credit.number, credit.date));
            if (x) setCredit(null);
            after(x, t("Credit note drafted in Payables"));
          }}
        >
          <div className="col gap-8">
            <div className="tiny">
              {t(
                "A draft credit note at the expected amount is created in Payables, where it is reviewed and posted. It does not move stock.",
              )}
            </div>
            <TextInput
              label={t("Supplier's credit note number")}
              value={credit.number}
              onChange={(e) => setCredit({ ...credit, number: e.target.value })}
            />
            <TextInput
              label={t("Date")}
              type="date"
              value={credit.date}
              onChange={(e) => setCredit({ ...credit, date: e.target.value })}
            />
          </div>
        </Confirm>
      ) : null}
    </div>
  );
}
