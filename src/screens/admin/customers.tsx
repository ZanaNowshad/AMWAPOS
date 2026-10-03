import { AddressFields, type AddrValue, addrFrom, addrPayload, addrProblem } from "../../components/AddressFields";
import { WhatsAppContactsButton } from "./waContacts";
import { TicketRowButton, TicketSheet, channelLabel, payLabel as payStateLabel } from "../pos/SendLoop";
import { AccountTab, AddressesTab } from "./customerAccount";
import { useFeature } from "../../components/FeatureGate";
import { useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { ArrowLeft, MessageCircle, Plus } from "lucide-react";
import { api } from "../../api";
import type { CustomerInput, CustomerRow, DeliveryRow, PayState, TicketRow } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { formatMoney } from "../../lib/money";
import { formatDateTime, formatShort, relative } from "../../lib/time";
import {
  Banner,
  Button,
  Checkbox,
  Chip,
  Empty,
  Money,
  PageHeader,
  Skeleton,
  Tabs,
  TextInput,
} from "../../components/ui";
import { DataTable, Drawer, useAction, useLoad } from "./common";
import { SaleDrawer } from "./sales";
import { t } from "../../i18n";
import { LoyaltyCard } from "./pillars";
import { codeLabel } from "../../i18n/codes";
import { OrderFlowBar } from "../../components/OrderFlow";

function CustomerForm({
  initial,
  onSaved,
  onCancel,
}: {
  initial: CustomerRow | null;
  onSaved: (c: CustomerRow) => void;
  onCancel: () => void;
}) {
  const [f, setF] = useState<CustomerInput>(
    initial ?? { name: "", phone: "", whatsapp: "", email: "", area: "", address: "", active: true },
  );
  const [addr, setAddr] = useState<AddrValue>(() => addrFrom(initial));
  const act = useAction();
  const set = (k: keyof CustomerInput, v: string | boolean) => setF({ ...f, [k]: v });
  return (
    <div className="col gap-16">
      <div className="form-grid">
        <TextInput
          label={t("Name")}
          required
          value={f.name}
          onChange={(e) => set("name", e.target.value)}
          fieldClass="span-2"
          autoFocus
        />
        <TextInput
          label={t("Phone")}
          value={f.phone ?? ""}
          onChange={(e) => set("phone", e.target.value)}
          hint={t("8-digit Bahrain numbers get +973.")}
        />
        <TextInput label={t("WhatsApp")} value={f.whatsapp ?? ""} onChange={(e) => set("whatsapp", e.target.value)} />
        <TextInput label={t("Email")} value={f.email ?? ""} onChange={(e) => set("email", e.target.value)} />
        <div className="span-2">
          <AddressFields value={addr} onChange={setAddr} idPrefix="cf" />
        </div>
        <TextInput label={t("Area")} value={addr.area} onChange={(e) => setAddr({ ...addr, area: e.target.value })} />
        <Checkbox label={t("Active")} checked={f.active} onChange={(v) => set("active", v)} />
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <div className="row">
        <Button onClick={onCancel}>{t("Cancel")}</Button>
        <Button
          variant="primary"
          className="right"
          disabled={!f.name.trim() || !!addrProblem(addr)}
          loading={act.busy}
          onClick={async () => {
            const r = await act.run(() =>
              api.customers.save(initial?.customer_id ?? null, {
                ...f,
                phone: f.phone || null,
                whatsapp: f.whatsapp || null,
                email: f.email || null,
                ...addrPayload(addr),
              }),
            );
            if (r) onSaved(r);
          }}
        >
          {t("Save")}
        </Button>
      </div>
    </div>
  );
}

export function CustomersPage() {
  const nav = useNavigate();
  const { has } = useSession();
  const [q, setQ] = useState("");
  const [creating, setCreating] = useState(false);
  const { data, loading, error, reload } = useLoad(() => api.customers.search(q, false, 200), [q]);
  return (
    <div>
      <PageHeader
        title={t("Customers")}
        actions={
          <>
            <WhatsAppContactsButton onImported={() => void reload()} />
            {has("customers.manage") ? (
              <Button variant="primary" icon={<Plus size={16} />} onClick={() => setCreating(true)}>
                {t("Customer")}
              </Button>
            ) : null}
          </>
        }
      />
      <div className="filters">
        <input
          className="input"
          style={{ width: 300 }}
          placeholder={t("Search phone or name…")}
          value={q}
          onChange={(e) => setQ(e.target.value)}
        />
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<CustomerRow>
        rows={data}
        loading={loading}
        rowKey={(r) => r.customer_id}
        onRowClick={(r) => nav(`/admin/customers/${r.customer_id}`)}
        empty={
          <Empty title={t("No customers found")}>{t("Customers can be added here or quickly from the till.")}</Empty>
        }
        columns={[
          { key: "n", label: t("Name"), render: (r) => r.name, sort: (r) => r.name },
          { key: "p", label: t("Phone"), render: (r) => r.phone ?? "—" },
          { key: "a", label: t("Area"), render: (r) => r.area ?? "—" },
          {
            key: "l",
            label: t("Last Purchase"),
            render: (r) => (r.last_purchase_at ? relative(r.last_purchase_at) : "—"),
            sort: (r) => r.last_purchase_at ?? "",
          },
          {
            key: "c",
            label: t("Purchases"),
            num: true,
            render: (r) => r.purchase_count,
            sort: (r) => r.purchase_count,
          },
          {
            key: "t",
            label: t("Total Purchases"),
            num: true,
            render: (r) => <Money minor={r.total_spent_minor} />,
            sort: (r) => r.total_spent_minor,
          },
          {
            key: "s",
            label: t("Status"),
            render: (r) => (r.active ? <Chip tone="success">{t("Active")}</Chip> : <Chip>{t("Inactive")}</Chip>),
          },
        ]}
      />
      {creating ? (
        <Drawer title={t("New customer")} onClose={() => setCreating(false)}>
          <CustomerForm
            initial={null}
            onCancel={() => setCreating(false)}
            onSaved={(c) => (setCreating(false), void reload(), nav(`/admin/customers/${c.customer_id}`))}
          />
        </Drawer>
      ) : null}
    </div>
  );
}

export function CustomerDetailPage() {
  const { id } = useParams();
  const nav = useNavigate();
  const toast = useToast();
  const { has } = useSession();
  const [tab, setTab] = useState<"overview" | "purchases" | "deliveries" | "notes" | "addresses" | "account">(
    "overview",
  );
  const credit = useFeature("customers.credit");
  const [editing, setEditing] = useState(false);
  const [note, setNote] = useState("");
  const [sale, setSale] = useState<string | null>(null);
  const { data, error, reload } = useLoad(() => api.customers.get(id!), [id]);
  const act = useAction();
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton />;
  const c = data.customer;
  const wa = (c.whatsapp ?? c.phone ?? "").replace(/\D/g, "");
  return (
    <div>
      <div className="page-header">
        <Button
          variant="ghost"
          icon={<ArrowLeft size={18} />}
          aria-label={t("Back")}
          onClick={() => nav("/admin/customers")}
        />
        <div className="grow">
          <div className="tiny">{t("Customer")}</div>
          <h1>{c.name}</h1>
          <div className="muted">{c.phone ?? t("No phone")}</div>
        </div>
        {wa ? (
          <a className="btn" href={`https://wa.me/${wa}`} target="_blank" rel="noreferrer">
            <MessageCircle size={16} /> {t("WhatsApp")}
          </a>
        ) : null}
        {has("customers.manage") ? <Button onClick={() => setEditing(true)}>{t("Edit")}</Button> : null}
      </div>
      <Tabs
        tabs={[
          { key: "overview", label: t("Overview") },
          { key: "purchases", label: t("Purchases") },
          { key: "deliveries", label: t("Deliveries") },
          { key: "notes", label: t("Notes") },
          { key: "addresses", label: t("Addresses") },
          ...(credit ? [{ key: "account" as const, label: t("Account") }] : []),
        ]}
        value={tab}
        onChange={setTab}
      />
      {tab === "addresses" ? <AddressesTab customerId={c.customer_id} /> : null}
      {tab === "account" && credit ? <AccountTab customerId={c.customer_id} /> : null}
      {tab === "overview" ? <LoyaltyCard customerId={c.customer_id} /> : null}
      {tab === "overview" ? (
        <div className="grid-2">
          <div className="card card-pad">
            <dl className="kv">
              <dt>{t("Phone")}</dt>
              <dd>{c.phone ?? "—"}</dd>
              <dt>{t("WhatsApp")}</dt>
              <dd>{c.whatsapp ?? "—"}</dd>
              <dt>{t("Email")}</dt>
              <dd>{c.email ?? "—"}</dd>
              <dt>{t("Area")}</dt>
              <dd>{c.area ?? "—"}</dd>
              <dt>{t("Address")}</dt>
              <dd>{c.address ?? "—"}</dd>
              <dt>{t("Customer since")}</dt>
              <dd>{formatShort(c.created_at)}</dd>
            </dl>
          </div>
          <div className="kpis">
            <div className="card kpi">
              <div className="k-label">{t("Purchases")}</div>
              <div className="k-value">{c.purchase_count}</div>
            </div>
            <div className="card kpi">
              <div className="k-label">{t("Total (net of refunds)")}</div>
              <div className="k-value">{formatMoney(c.total_spent_minor)}</div>
            </div>
            <div className="card kpi">
              <div className="k-label">{t("Last purchase")}</div>
              <div className="k-value" style={{ fontSize: 16 }}>
                {c.last_purchase_at ? formatDateTime(c.last_purchase_at) : "—"}
              </div>
            </div>
          </div>
        </div>
      ) : null}
      {tab === "purchases" ? (
        <DataTable<Record<string, unknown>>
          rows={data.purchases}
          rowKey={(r) => String(r.sale_id)}
          onRowClick={(r) => setSale(String(r.sale_id))}
          empty={
            <div className="empty">
              {has("sales.view") ? t("No purchases yet.") : t("Your role cannot view purchase history.")}
            </div>
          }
          columns={[
            { key: "r", label: t("Receipt"), render: (r) => <span className="mono">{String(r.receipt_number)}</span> },
            { key: "d", label: t("Date"), render: (r) => formatShort(String(r.completed_at)) },
            { key: "t", label: t("Total"), num: true, render: (r) => <Money minor={Number(r.total_minor)} /> },
          ]}
        />
      ) : null}
      {tab === "deliveries" ? <DeliveryTable rows={data.deliveries} /> : null}
      {tab === "notes" ? (
        <div className="col gap-16">
          {has("customers.manage") ? (
            <div className="row">
              <input
                className="input grow"
                placeholder={t("Add a note…")}
                value={note}
                onChange={(e) => setNote(e.target.value)}
              />
              <Button
                variant="primary"
                disabled={!note.trim()}
                onClick={async () => {
                  const r = await act.run(() => api.customers.addNote(c.customer_id, note));
                  if (r !== undefined) {
                    setNote("");
                    toast("success", t("Note added"));
                    void reload();
                  }
                }}
              >
                {t("Add")}
              </Button>
            </div>
          ) : null}
          {data.notes.map((n) => (
            <div key={n.note_id} className="card card-pad">
              <div className="tiny">
                {formatDateTime(n.created_at)} · {n.author}
              </div>
              <div style={{ whiteSpace: "pre-wrap" }}>{n.note}</div>
            </div>
          ))}
          {data.notes.length === 0 ? <div className="empty">{t("No notes.")}</div> : null}
        </div>
      ) : null}
      {editing ? (
        <Drawer title={t("Edit {0}", c.name)} onClose={() => setEditing(false)}>
          <CustomerForm
            initial={c}
            onCancel={() => setEditing(false)}
            onSaved={() => (setEditing(false), void reload())}
          />
        </Drawer>
      ) : null}
      {sale ? <SaleDrawer saleId={sale} onClose={() => setSale(null)} /> : null}
    </div>
  );
}

const DSTATUS: Record<string, "default" | "info" | "warning" | "success"> = {
  pending: "warning",
  preparing: "info",
  dispatched: "info",
  delivered: "success",
  cancelled: "default",
};
const payLabel = (p: string) => (p === "cod" ? t("Cash on Delivery") : p === "paid" ? t("Paid") : t("Payment Pending"));

function DeliveryTable({ rows, onOpen }: { rows: DeliveryRow[]; onOpen?: (d: DeliveryRow) => void }) {
  return (
    <DataTable<DeliveryRow>
      rows={rows}
      rowKey={(r) => r.delivery_id}
      onRowClick={onOpen}
      empty={<div className="empty">{t("No deliveries.")}</div>}
      columns={[
        { key: "n", label: t("Order"), render: (r) => <span className="mono">{r.delivery_number}</span> },
        { key: "c", label: t("Customer"), render: (r) => r.customer_name ?? "—" },
        { key: "a", label: t("Area"), render: (r) => r.area ?? "—" },
        { key: "m", label: t("Amount"), num: true, render: (r) => <Money minor={r.amount_minor} /> },
        {
          key: "p",
          label: t("Payment"),
          render: (r) => (
            <Chip tone={r.payment_status === "paid" ? "success" : "warning"}>{payLabel(r.payment_status)}</Chip>
          ),
        },
        { key: "s", label: t("Status"), render: (r) => <Chip tone={DSTATUS[r.status]}>{codeLabel(r.status)}</Chip> },
        { key: "t", label: t("Created"), render: (r) => formatShort(r.created_at) },
      ]}
    />
  );
}

export function DeliveriesPage() {
  const [view, setView] = useState<"board" | "table">("board");
  const [closed, setClosed] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const { data, loading, error, reload } = useLoad(() => api.deliveries.list(undefined, closed), [closed]);
  return (
    <div>
      <PageHeader
        title={t("Deliveries")}
        subtitle={t(
          "Every order going out, from packing to the door. Tap one to move it on, take payment or message the customer.",
        )}
        actions={
          <>
            <button className={`filter-chip ${view === "board" ? "active" : ""}`} onClick={() => setView("board")}>
              {t("Board")}
            </button>
            <button className={`filter-chip ${view === "table" ? "active" : ""}`} onClick={() => setView("table")}>
              {t("List")}
            </button>
            {view === "table" ? (
              <Checkbox label={t("Include delivered / cancelled")} checked={closed} onChange={setClosed} />
            ) : null}
          </>
        }
      />
      <OrderFlowBar />
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {loading && !data && view === "table" ? <Skeleton /> : null}
      {view === "board" ? <TicketBoard /> : null}
      {view === "table" && data ? <DeliveryTable rows={data} onOpen={(d) => setOpen(d.delivery_id)} /> : null}
      {open ? <TicketSheet ticketId={open} onClose={() => setOpen(null)} onChanged={() => void reload()} /> : null}
    </div>
  );
}

type BoardCol = "now" | "prep" | "out" | "done" | "problem";

function boardCol(r: TicketRow): BoardCol {
  if (r.problem) return "problem";
  if (r.status === "preparing") return "prep";
  if (r.status === "dispatched") return "out";
  if (r.status === "delivered" || r.status === "cancelled") return "done";
  return "now";
}

/** Admin board: the same tickets as the till's rail, in five columns. */
function TicketBoard() {
  const [f, setF] = useState<{ area?: string; rider?: string; pay_state?: string; channel?: string }>({});
  const [open, setOpen] = useState<string | null>(null);
  const { data, error, reload } = useLoad(() => api.tickets.list({ tab: "board" }), []);
  useEffect(() => {
    const id = setInterval(() => void reload(), 30000);
    return () => clearInterval(id);
  }, [reload]);
  const rows = (data ?? []).filter(
    (r) =>
      (!f.area || r.area === f.area) &&
      (!f.rider || r.assigned_user_id === f.rider) &&
      (!f.pay_state || r.pay_state === f.pay_state) &&
      (!f.channel || r.channel === f.channel),
  );
  const uniq = <T,>(xs: (T | null)[]) => [...new Set(xs.filter((x): x is T => x !== null && x !== ""))];
  const areas = uniq((data ?? []).map((r) => r.area));
  const riders = [
    ...new Map((data ?? []).filter((r) => r.assigned_user_id).map((r) => [r.assigned_user_id!, r.assigned_name ?? ""])),
  ];
  const channels = uniq((data ?? []).map((r) => r.channel));
  const cols: { key: BoardCol; label: string }[] = [
    { key: "now", label: t("New") },
    { key: "prep", label: t("Packing") },
    { key: "out", label: t("On the way") },
    { key: "done", label: t("Done") },
    { key: "problem", label: t("Needs help") },
  ];
  const chip = (key: keyof typeof f, value: string, label: string) => (
    <button
      key={`${key}-${value}`}
      type="button"
      className={`filter-chip ${f[key] === value ? "active" : ""}`}
      onClick={() => setF({ ...f, [key]: f[key] === value ? undefined : value })}
    >
      {label}
    </button>
  );
  return (
    <div className="stack-16" data-testid="ticket-board">
      <div className="area-chips">
        {(["unpaid", "screenshot_pending", "recorded", "paid"] as PayState[]).map((p) =>
          chip("pay_state", p, payStateLabel(p)),
        )}
        {channels.map((c) => chip("channel", c, channelLabel(c)))}
        {areas.map((a) => chip("area", a, a))}
        {riders.map(([id, name]) => chip("rider", id, name))}
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <div className="kanban board5">
        {cols.map((c) => {
          const list = rows.filter((r) => boardCol(r) === c.key);
          return (
            <div key={c.key} className={`kanban-col ${c.key === "problem" && list.length ? "problem" : ""}`}>
              <h3 style={{ margin: "4px 4px 10px" }}>
                {c.label} <span className="tiny">({list.length})</span>
              </h3>
              {list.map((r) => (
                <TicketRowButton key={r.ticket_id} row={r} onOpen={(x) => setOpen(x.ticket_id)} />
              ))}
            </div>
          );
        })}
      </div>
      {open ? <TicketSheet ticketId={open} onClose={() => setOpen(null)} onChanged={() => void reload()} /> : null}
    </div>
  );
}
