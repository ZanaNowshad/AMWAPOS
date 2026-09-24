import { WhatsAppSendButton } from "./automation";
import { useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { ArrowLeft, MessageCircle, Plus } from "lucide-react";
import { api } from "../../api";
import type { CustomerInput, CustomerRow, DeliveryRow } from "../../api/types";
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
  Field,
  Money,
  PageHeader,
  Skeleton,
  Tabs,
  TextInput,
} from "../../components/ui";
import { DataTable, Drawer, useAction, useLoad } from "./common";
import { SaleDrawer } from "./sales";
import { t, tb } from "../../i18n";
import { codeLabel } from "../../i18n/codes";

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
        <TextInput label={t("Area")} value={f.area ?? ""} onChange={(e) => set("area", e.target.value)} />
        <TextInput
          label={t("Address")}
          value={f.address ?? ""}
          onChange={(e) => set("address", e.target.value)}
          fieldClass="span-2"
        />
        <Checkbox label={t("Active")} checked={f.active} onChange={(v) => set("active", v)} />
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <div className="row">
        <Button onClick={onCancel}>{t("Cancel")}</Button>
        <Button
          variant="primary"
          className="right"
          disabled={!f.name.trim()}
          loading={act.busy}
          onClick={async () => {
            const r = await act.run(() =>
              api.customers.save(initial?.customer_id ?? null, {
                ...f,
                phone: f.phone || null,
                whatsapp: f.whatsapp || null,
                email: f.email || null,
                area: f.area || null,
                address: f.address || null,
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
          has("customers.manage") ? (
            <Button variant="primary" icon={<Plus size={16} />} onClick={() => setCreating(true)}>
              {t("Customer")}
            </Button>
          ) : null
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
  const [tab, setTab] = useState<"overview" | "purchases" | "deliveries" | "notes">("overview");
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
        ]}
        value={tab}
        onChange={setTab}
      />
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
  const { has } = useSession();
  const toast = useToast();
  const [view, setView] = useState<"board" | "table">("board");
  const [closed, setClosed] = useState(false);
  const [open, setOpen] = useState<DeliveryRow | null>(null);
  const { data, loading, error, reload } = useLoad(() => api.deliveries.list(undefined, closed), [closed]);
  const users = useLoad(() => (has("users.manage") ? api.users.list() : Promise.resolve([])), []);
  const detail = useLoad(
    () => (open ? api.deliveries.get(open.delivery_id) : Promise.resolve(null)),
    [open?.delivery_id],
  );
  const act = useAction();
  const update = async (a: Parameters<typeof api.deliveries.update>[0]) => {
    const r = await act.run(() => api.deliveries.update(a));
    if (r) {
      toast("success", t("Delivery {0} updated", r.delivery_number));
      setOpen(r);
      void reload();
      void detail.reload();
    }
  };
  const waitMin = (d: DeliveryRow) => Math.round((Date.now() - new Date(d.created_at).getTime()) / 60000);
  return (
    <div>
      <PageHeader
        title={t("Deliveries")}
        actions={
          <>
            <button className={`filter-chip ${view === "board" ? "active" : ""}`} onClick={() => setView("board")}>
              {t("Board")}
            </button>
            <button className={`filter-chip ${view === "table" ? "active" : ""}`} onClick={() => setView("table")}>
              {t("Table")}
            </button>
            <Checkbox label={t("Include delivered / cancelled")} checked={closed} onChange={setClosed} />
          </>
        }
      />
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {loading && !data ? <Skeleton /> : null}
      {view === "board" && data ? (
        <div className="kanban">
          {(["pending", "preparing", "dispatched"] as const).map((st) => (
            <div key={st} className="kanban-col">
              <h3 style={{ margin: "4px 4px 10px" }}>
                {st[0].toUpperCase() + st.slice(1)}{" "}
                <span className="tiny">({data.filter((d) => d.status === st).length})</span>
              </h3>
              {data
                .filter((d) => d.status === st)
                .map((d) => (
                  <div
                    key={d.delivery_id}
                    className="kanban-card"
                    onClick={() => setOpen(d)}
                    tabIndex={0}
                    role="button"
                  >
                    <div className="row">
                      <strong className="grow">{d.delivery_number}</strong>
                      <span className="tiny">{waitMin(d)} min</span>
                    </div>
                    <div className="small">{d.customer_name ?? t("Customer")}</div>
                    <div className="tiny">{d.area ?? d.address ?? ""}</div>
                    <div className="row" style={{ marginTop: 6 }}>
                      <span className="money grow">{formatMoney(d.amount_minor)}</span>
                      <Chip tone={d.payment_status === "paid" ? "success" : "warning"}>
                        {payLabel(d.payment_status)}
                      </Chip>
                    </div>
                  </div>
                ))}
            </div>
          ))}
        </div>
      ) : null}
      {view === "table" && data ? <DeliveryTable rows={data} onOpen={setOpen} /> : null}
      {open ? (
        <Drawer title={t("Delivery {0}", open.delivery_number)} onClose={() => setOpen(null)}>
          <div className="stack-16">
            <div className="row">
              <Chip tone={DSTATUS[open.status]}>{codeLabel(open.status)}</Chip>
              <Chip tone={open.payment_status === "paid" ? "success" : "warning"}>{payLabel(open.payment_status)}</Chip>
            </div>
            <dl className="kv">
              <dt>{t("Customer")}</dt>
              <dd>{open.customer_name ?? "—"}</dd>
              <dt>{t("Phone")}</dt>
              <dd>{open.phone ?? "—"}</dd>
              <dt>{t("Address")}</dt>
              <dd>{[open.area, open.address].filter(Boolean).join(", ") || "—"}</dd>
              <dt>{t("Linked sale")}</dt>
              <dd className="mono">{open.receipt_number ?? "—"}</dd>
              <dt>{t("Amount")}</dt>
              <dd>{formatMoney(open.amount_minor)}</dd>
              <dt>{t("Rider")}</dt>
              <dd>{open.assigned_name ?? t("Unassigned")}</dd>
              <dt>{t("Notes")}</dt>
              <dd>{open.notes ?? "—"}</dd>
            </dl>
            {has("deliveries.manage") ? (
              <div className="col">
                <Field label={t("Assign rider")}>
                  <select
                    className="select"
                    value={open.assigned_user_id ?? ""}
                    onChange={(e) => update({ delivery_id: open.delivery_id, assigned_user_id: e.target.value })}
                  >
                    <option value="">{t("Unassigned")}</option>
                    {(users.data ?? [])
                      .filter((u) => u.active)
                      .map((u) => (
                        <option key={u.user_id} value={u.user_id}>
                          {u.display_name} ({tb(u.role_name)})
                        </option>
                      ))}
                  </select>
                </Field>
                <Field label={t("Payment")}>
                  <select
                    className="select"
                    value={open.payment_status}
                    onChange={(e) => update({ delivery_id: open.delivery_id, payment_status: e.target.value })}
                  >
                    <option value="paid">{t("Paid")}</option>
                    <option value="cod">{t("Cash on Delivery")}</option>
                    <option value="pending">{t("Payment Pending")}</option>
                  </select>
                </Field>
              </div>
            ) : null}
            <div className="row wrap">
              {open.status === "pending" ? (
                <Button onClick={() => update({ delivery_id: open.delivery_id, status: "preparing" })}>
                  {t("Preparing")}
                </Button>
              ) : null}
              {open.status === "pending" || open.status === "preparing" ? (
                <Button
                  variant="primary"
                  onClick={() => update({ delivery_id: open.delivery_id, status: "dispatched" })}
                >
                  {t("Dispatch")}
                </Button>
              ) : null}
              {open.status === "dispatched" ? (
                <Button
                  variant="primary"
                  onClick={() => update({ delivery_id: open.delivery_id, status: "delivered" })}
                >
                  {t("Delivered")}
                </Button>
              ) : null}
              {["pending", "preparing", "dispatched"].includes(open.status) ? (
                <WhatsAppSendButton kind="dispatch" deliveryId={open.delivery_id} phone={open.phone} />
              ) : null}
              {open.payment_status !== "paid" && open.status !== "cancelled" ? (
                <WhatsAppSendButton kind="reminder" deliveryId={open.delivery_id} phone={open.phone} />
              ) : null}
              {["pending", "preparing", "dispatched"].includes(open.status) && has("deliveries.manage") ? (
                <Button
                  variant="danger-outline"
                  onClick={() => update({ delivery_id: open.delivery_id, status: "cancelled" })}
                >
                  {t("Cancel")}
                </Button>
              ) : null}
            </div>
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
            {detail.data ? (
              <>
                <div>
                  <h3 style={{ marginBottom: 6 }}>{t("Timeline")}</h3>
                  {detail.data.events.map((e, i) => (
                    <div key={i} className="small row">
                      <span style={{ width: 130 }}>{formatShort(e.at)}</span>
                      <span className="grow">
                        {e.from ? `${e.from} → ` : ""}
                        {e.to} {e.note ? `· ${e.note}` : ""}
                      </span>
                      <span className="muted">{e.user}</span>
                    </div>
                  ))}
                </div>
                {detail.data.items.length ? (
                  <div>
                    <h3 style={{ marginBottom: 6 }}>{t("Items")}</h3>
                    {detail.data.items.map((it, i) => (
                      <div key={i} className="small row">
                        <span className="grow">{String(it.name)}</span>
                        <span className="num">{formatMoney(Number(it.line_total_minor))}</span>
                      </div>
                    ))}
                  </div>
                ) : null}
              </>
            ) : null}
          </div>
        </Drawer>
      ) : null}
    </div>
  );
}
