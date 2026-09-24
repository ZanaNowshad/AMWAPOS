import { useEffect, useMemo, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ArrowLeft, PackageCheck, Plus, Send, Trash2, XCircle } from "lucide-react";
import { api } from "../../api";
import type { PoDetail, PoRow, PosSearchRow, SupplierInput, SupplierRow } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { newOperationId } from "../../lib/ids";
import {
  formatAmount,
  formatMoney,
  formatPercent,
  formatQty,
  parseMoney,
  parsePercent,
  parseQty,
} from "../../lib/money";
import { formatShort } from "../../lib/time";
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
import { Confirm, DataTable, Drawer, useAction, useLoad } from "./common";

const PO_TONE: Record<string, "default" | "info" | "warning" | "success"> = {
  draft: "default",
  ordered: "info",
  partially_received: "warning",
  received: "success",
  cancelled: "default",
};
const poLabel = (s: string) => s.replace("_", " ").replace(/^\w/, (c) => c.toUpperCase());

function SupplierForm({
  initial,
  onSaved,
  onCancel,
}: {
  initial: SupplierRow | null;
  onSaved: (s: SupplierRow) => void;
  onCancel: () => void;
}) {
  const [f, setF] = useState<SupplierInput>(
    initial ?? {
      name: "",
      cr_number: "",
      vat_number: "",
      contact_name: "",
      phone: "",
      whatsapp: "",
      email: "",
      address: "",
      payment_terms: "",
      notes: "",
      active: true,
    },
  );
  const act = useAction();
  const set = (k: keyof SupplierInput, v: string | boolean) => setF({ ...f, [k]: v });
  return (
    <div className="col gap-16">
      <div className="form-grid">
        <TextInput
          label="Supplier name"
          required
          value={f.name}
          onChange={(e) => set("name", e.target.value)}
          fieldClass="span-2"
          autoFocus
        />
        <TextInput
          label="Contact person"
          value={f.contact_name ?? ""}
          onChange={(e) => set("contact_name", e.target.value)}
        />
        <TextInput label="Phone" value={f.phone ?? ""} onChange={(e) => set("phone", e.target.value)} />
        <TextInput label="WhatsApp" value={f.whatsapp ?? ""} onChange={(e) => set("whatsapp", e.target.value)} />
        <TextInput label="Email" value={f.email ?? ""} onChange={(e) => set("email", e.target.value)} />
        <TextInput label="CR number" value={f.cr_number ?? ""} onChange={(e) => set("cr_number", e.target.value)} />
        <TextInput label="VAT number" value={f.vat_number ?? ""} onChange={(e) => set("vat_number", e.target.value)} />
        <TextInput
          label="Payment terms"
          value={f.payment_terms ?? ""}
          onChange={(e) => set("payment_terms", e.target.value)}
          placeholder="e.g. 30 days"
        />
        <TextInput
          label="Address"
          value={f.address ?? ""}
          onChange={(e) => set("address", e.target.value)}
          fieldClass="span-2"
        />
        <Field label="Notes" className="span-2">
          <textarea className="textarea" value={f.notes ?? ""} onChange={(e) => set("notes", e.target.value)} />
        </Field>
        <Checkbox label="Active" checked={f.active} onChange={(v) => set("active", v)} />
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <div className="row">
        <Button onClick={onCancel}>Cancel</Button>
        <Button
          variant="primary"
          className="right"
          disabled={!f.name.trim()}
          loading={act.busy}
          onClick={async () => {
            const r = await act.run(() => api.suppliers.save(initial?.supplier_id ?? null, f));
            if (r) onSaved(r);
          }}
        >
          Save
        </Button>
      </div>
    </div>
  );
}

export function SuppliersPage() {
  const nav = useNavigate();
  const [q, setQ] = useState("");
  const [inactive, setInactive] = useState(false);
  const [creating, setCreating] = useState(false);
  const { data, loading, error, reload } = useLoad(() => api.suppliers.list(q, inactive), [q, inactive]);
  return (
    <div>
      <PageHeader
        title="Suppliers"
        actions={
          <Button variant="primary" icon={<Plus size={16} />} onClick={() => setCreating(true)}>
            Supplier
          </Button>
        }
      />
      <div className="filters">
        <input
          className="input"
          style={{ width: 280 }}
          placeholder="Search name, phone or contact…"
          value={q}
          onChange={(e) => setQ(e.target.value)}
        />
        <Checkbox label="Show inactive" checked={inactive} onChange={setInactive} />
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<SupplierRow>
        rows={data}
        loading={loading}
        rowKey={(r) => r.supplier_id}
        onRowClick={(r) => nav(`/admin/suppliers/${r.supplier_id}`)}
        empty={<Empty title="No suppliers yet">Add suppliers to create purchase orders and track costs.</Empty>}
        columns={[
          { key: "n", label: "Supplier", render: (r) => r.name, sort: (r) => r.name },
          { key: "p", label: "Phone", render: (r) => r.phone ?? "—" },
          { key: "c", label: "Contact", render: (r) => r.contact_name ?? "—" },
          { key: "t", label: "Payment Terms", render: (r) => r.payment_terms ?? "—" },
          { key: "o", label: "Open POs", num: true, render: (r) => r.open_po_count },
          { key: "l", label: "Last Purchase", render: (r) => formatShort(r.last_purchase_at) },
          {
            key: "s",
            label: "Status",
            render: (r) => (r.active ? <Chip tone="success">Active</Chip> : <Chip>Inactive</Chip>),
          },
        ]}
      />
      {creating ? (
        <Drawer title="New supplier" onClose={() => setCreating(false)}>
          <SupplierForm
            initial={null}
            onCancel={() => setCreating(false)}
            onSaved={() => (setCreating(false), void reload())}
          />
        </Drawer>
      ) : null}
    </div>
  );
}

export function SupplierDetailPage() {
  const { id } = useParams();
  const nav = useNavigate();
  const [tab, setTab] = useState<"overview" | "pos" | "products">("overview");
  const [editing, setEditing] = useState(false);
  const { data, error, reload } = useLoad(() => api.suppliers.get(id!), [id]);
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton />;
  const s = data.supplier;
  return (
    <div>
      <div className="page-header">
        <Button
          variant="ghost"
          icon={<ArrowLeft size={18} />}
          aria-label="Back"
          onClick={() => nav("/admin/suppliers")}
        />
        <div className="grow">
          <div className="tiny">Supplier</div>
          <h1>{s.name}</h1>
        </div>
        <Button onClick={() => setEditing(true)}>Edit</Button>
        <Button
          variant="primary"
          icon={<Plus size={16} />}
          onClick={() => nav(`/admin/purchase-orders/new?supplier=${s.supplier_id}`)}
        >
          Purchase Order
        </Button>
      </div>
      <Tabs
        tabs={[
          { key: "overview", label: "Overview" },
          { key: "pos", label: "Purchase Orders" },
          { key: "products", label: "Products" },
        ]}
        value={tab}
        onChange={setTab}
      />
      {tab === "overview" ? (
        <div className="card card-pad">
          <dl className="kv">
            <dt>Contact</dt>
            <dd>{s.contact_name ?? "—"}</dd>
            <dt>Phone</dt>
            <dd>{s.phone ?? "—"}</dd>
            <dt>WhatsApp</dt>
            <dd>{s.whatsapp ?? "—"}</dd>
            <dt>Email</dt>
            <dd>{s.email ?? "—"}</dd>
            <dt>CR / VAT</dt>
            <dd>
              {s.cr_number ?? "—"} / {s.vat_number ?? "—"}
            </dd>
            <dt>Payment terms</dt>
            <dd>{s.payment_terms ?? "—"}</dd>
            <dt>Address</dt>
            <dd>{s.address ?? "—"}</dd>
            <dt>Notes</dt>
            <dd style={{ whiteSpace: "pre-wrap" }}>{s.notes ?? "—"}</dd>
          </dl>
        </div>
      ) : null}
      {tab === "pos" ? (
        <DataTable<PoRow>
          rows={data.purchase_orders}
          rowKey={(r) => r.po_id}
          onRowClick={(r) => nav(`/admin/purchase-orders/${r.po_id}`)}
          columns={[
            { key: "n", label: "PO", render: (r) => <span className="mono">{r.po_number}</span> },
            { key: "d", label: "Date", render: (r) => formatShort(r.created_at) },
            { key: "t", label: "Total", num: true, render: (r) => <Money minor={r.total_minor} /> },
            { key: "s", label: "Status", render: (r) => <Chip tone={PO_TONE[r.status]}>{poLabel(r.status)}</Chip> },
          ]}
        />
      ) : null}
      {tab === "products" ? (
        <DataTable<Record<string, unknown>>
          rows={data.products}
          rowKey={(r) => String(r.product_id)}
          onRowClick={(r) => nav(`/admin/products/${String(r.product_id)}`)}
          empty={<div className="empty">No products received from this supplier yet.</div>}
          columns={[
            { key: "n", label: "Product", render: (r) => String(r.name) },
            { key: "s", label: "SKU", render: (r) => String(r.sku) },
            {
              key: "c",
              label: "Last cost",
              num: true,
              render: (r) => <Money minor={r.last_cost_minor as number | null} />,
            },
            { key: "d", label: "Last received", render: (r) => formatShort(String(r.last_received_at)) },
          ]}
        />
      ) : null}
      {editing ? (
        <Drawer title={`Edit ${s.name}`} onClose={() => setEditing(false)}>
          <SupplierForm
            initial={s}
            onCancel={() => setEditing(false)}
            onSaved={() => (setEditing(false), void reload())}
          />
        </Drawer>
      ) : null}
    </div>
  );
}

export function PurchaseOrdersPage() {
  const nav = useNavigate();
  const { has } = useSession();
  const [status, setStatus] = useState("");
  const { data, loading, error } = useLoad(() => api.po.list(status || undefined), [status]);
  return (
    <div>
      <PageHeader
        title="Purchase Orders"
        actions={
          has("purchasing.manage") ? (
            <Button variant="primary" icon={<Plus size={16} />} onClick={() => nav("/admin/purchase-orders/new")}>
              Purchase Order
            </Button>
          ) : null
        }
      />
      <div className="filters">
        {["", "draft", "ordered", "partially_received", "received", "cancelled"].map((s) => (
          <button key={s} className={`filter-chip ${status === s ? "active" : ""}`} onClick={() => setStatus(s)}>
            {s ? poLabel(s) : "All"}
          </button>
        ))}
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<PoRow>
        rows={data}
        loading={loading}
        rowKey={(r) => r.po_id}
        onRowClick={(r) => nav(`/admin/purchase-orders/${r.po_id}`)}
        empty={<Empty title="No purchase orders">Create a purchase order to order stock from a supplier.</Empty>}
        columns={[
          {
            key: "n",
            label: "PO",
            render: (r) => <span className="mono">{r.po_number}</span>,
            sort: (r) => r.po_number,
          },
          { key: "s", label: "Supplier", render: (r) => r.supplier_name, sort: (r) => r.supplier_name },
          { key: "d", label: "Date", render: (r) => formatShort(r.created_at), sort: (r) => r.created_at },
          { key: "i", label: "Items", num: true, render: (r) => r.line_count },
          {
            key: "t",
            label: "Total",
            num: true,
            render: (r) => <Money minor={r.total_minor} />,
            sort: (r) => r.total_minor,
          },
          { key: "r", label: "Received", num: true, render: (r) => `${r.received_pct}%` },
          { key: "st", label: "Status", render: (r) => <Chip tone={PO_TONE[r.status]}>{poLabel(r.status)}</Chip> },
        ]}
      />
    </div>
  );
}

interface EditLine {
  product_id: string;
  name: string;
  qty: string;
  cost: string;
  tax: string;
}

export function PoEditorPage() {
  const { id } = useParams();
  const [params] = useSearchParams();
  const nav = useNavigate();
  const toast = useToast();
  const { has } = useSession();
  const isNew = !id || id === "new";
  const [po, setPo] = useState<PoDetail | null>(null);
  const [supplier, setSupplier] = useState(params.get("supplier") ?? "");
  const [reference, setReference] = useState("");
  const [expected, setExpected] = useState("");
  const [notes, setNotes] = useState("");
  const [lines, setLines] = useState<EditLine[]>([]);
  const [search, setSearch] = useState("");
  const [results, setResults] = useState<PosSearchRow[]>([]);
  const [receiving, setReceiving] = useState(params.get("receive") === "1");
  const [recv, setRecv] = useState<Record<string, { qty: string; cost: string }>>({});
  const [recvRef, setRecvRef] = useState("");
  const [recvOp, setRecvOp] = useState(newOperationId);
  const [confirm, setConfirm] = useState<null | "order" | "cancel" | "close">(null);
  const suppliers = useLoad(() => api.suppliers.list(), []);
  const act = useAction();

  const load = async () => {
    if (isNew) return;
    const d = await api.po.get(id!);
    setPo(d);
    setSupplier(d.supplier_id);
    setReference(d.reference ?? "");
    setExpected(d.expected_at ?? "");
    setNotes(d.notes ?? "");
    setLines(
      d.lines.map((l) => ({
        product_id: l.product_id,
        name: l.product_name,
        qty: formatQty(l.qty_ordered_milli),
        cost: formatAmount(l.unit_cost_minor),
        tax: formatPercent(l.tax_rate_bp).replace("%", ""),
      })),
    );
    setRecv(
      Object.fromEntries(
        d.lines.map((l) => [
          l.po_item_id,
          {
            qty: l.qty_remaining_milli > 0 ? formatQty(l.qty_remaining_milli) : "",
            cost: formatAmount(l.unit_cost_minor),
          },
        ]),
      ),
    );
  };
  useEffect(() => {
    void act.run(load);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);
  useEffect(() => {
    if (!search.trim()) return setResults([]);
    const t = setTimeout(
      () =>
        api.pos
          .search(search, { limit: 8 })
          .then(setResults)
          .catch(() => {}),
      150,
    );
    return () => clearTimeout(t);
  }, [search]);

  const editable = isNew || po?.status === "draft";
  const totals = useMemo(() => {
    let sub = 0;
    for (const l of lines) {
      const q = parseQty(l.qty);
      const c = parseMoney(l.cost);
      if (q !== null && c !== null) sub += Math.round((c * q) / 1000);
    }
    return sub;
  }, [lines]);

  const save = async () => {
    const payload = {
      supplier_id: supplier,
      reference: reference || null,
      expected_at: expected || null,
      notes: notes || null,
      lines: lines.map((l) => ({
        product_id: l.product_id,
        qty_milli: parseQty(l.qty) ?? 0,
        unit_cost_minor: parseMoney(l.cost) ?? -1,
        tax_rate_bp: parsePercent(l.tax || "0") ?? -1,
      })),
    };
    const r = await act.run(() => api.po.save(isNew ? null : id!, payload));
    if (r) {
      toast("success", `Purchase order ${r.po_number} saved`);
      if (isNew) nav(`/admin/purchase-orders/${r.po_id}`, { replace: true });
      else await load();
    }
  };

  const doReceive = async () => {
    if (!po) return;
    const rl = po.lines
      .map((l) => ({
        po_item_id: l.po_item_id,
        qty_milli: parseQty(recv[l.po_item_id]?.qty ?? "") ?? 0,
        unit_cost_minor: parseMoney(recv[l.po_item_id]?.cost ?? ""),
      }))
      .filter((l) => l.qty_milli > 0);
    const r = await act.run(() =>
      api.po.receive({ po_id: po.po_id, reference: recvRef || null, lines: rl, operation_id: recvOp }),
    );
    if (r) {
      toast(
        "success",
        "Goods received",
        r.status === "received" ? "Purchase order fully received" : "Partial delivery recorded",
      );
      setReceiving(false);
      setRecvOp(newOperationId());
      await load();
    }
  };

  if (!isNew && !po) return act.error ? <Banner tone="danger">{act.error}</Banner> : <Skeleton rows={10} />;
  return (
    <div>
      <div className="page-header">
        <Button
          variant="ghost"
          icon={<ArrowLeft size={18} />}
          aria-label="Back"
          onClick={() => nav("/admin/purchase-orders")}
        />
        <div className="grow">
          <div className="tiny">Purchase order</div>
          <h1>
            {isNew ? "New purchase order" : po!.po_number}{" "}
            {po ? <Chip tone={PO_TONE[po.status]}>{poLabel(po.status)}</Chip> : null}
          </h1>
        </div>
        {editable && has("purchasing.manage") ? (
          <Button onClick={save} loading={act.busy} disabled={!supplier || lines.length === 0}>
            Save Draft
          </Button>
        ) : null}
        {po?.status === "draft" && has("purchasing.manage") ? (
          <Button variant="primary" icon={<Send size={16} />} onClick={() => setConfirm("order")}>
            Place Order
          </Button>
        ) : null}
        {po && (po.status === "ordered" || po.status === "partially_received") && has("inventory.receive") ? (
          <Button variant="primary" icon={<PackageCheck size={16} />} onClick={() => setReceiving(true)}>
            Receive Goods
          </Button>
        ) : null}
        {po &&
        (po.status === "draft" || po.status === "ordered") &&
        po.received_pct === 0 &&
        has("purchasing.manage") ? (
          <Button variant="danger-outline" icon={<XCircle size={16} />} onClick={() => setConfirm("cancel")}>
            Cancel PO
          </Button>
        ) : null}
        {po?.status === "partially_received" && has("purchasing.manage") ? (
          <Button onClick={() => setConfirm("close")}>Close short</Button>
        ) : null}
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <div className="card card-pad" style={{ marginBottom: 16 }}>
        <div className="form-grid" style={{ gridTemplateColumns: "repeat(4, minmax(0,1fr))" }}>
          <Field label="Supplier" required>
            <select
              className="select"
              value={supplier}
              onChange={(e) => setSupplier(e.target.value)}
              disabled={!editable}
            >
              <option value="">Choose supplier…</option>
              {(suppliers.data ?? []).map((s) => (
                <option key={s.supplier_id} value={s.supplier_id}>
                  {s.name}
                </option>
              ))}
            </select>
          </Field>
          <TextInput
            label="Reference"
            value={reference}
            onChange={(e) => setReference(e.target.value)}
            disabled={!editable}
          />
          <TextInput
            label="Expected date"
            type="date"
            value={expected}
            onChange={(e) => setExpected(e.target.value)}
            disabled={!editable}
          />
          <TextInput label="Notes" value={notes} onChange={(e) => setNotes(e.target.value)} disabled={!editable} />
        </div>
      </div>
      {receiving && po ? (
        <div className="card">
          <div className="card-head">
            <h3 className="grow">Receive goods</h3>
            <input
              className="input"
              style={{ width: 240 }}
              placeholder="Delivery note / invoice ref"
              value={recvRef}
              onChange={(e) => setRecvRef(e.target.value)}
            />
          </div>
          <table className="table">
            <thead>
              <tr>
                <th>Product</th>
                <th className="num">Ordered</th>
                <th className="num">Previously received</th>
                <th className="num">Receiving now</th>
                <th className="num">Unit cost</th>
                <th className="num">Remaining</th>
              </tr>
            </thead>
            <tbody>
              {po.lines.map((l) => (
                <tr key={l.po_item_id}>
                  <td>
                    {l.product_name}
                    <div className="tiny mono">{l.primary_barcode}</div>
                  </td>
                  <td className="num">{formatQty(l.qty_ordered_milli)}</td>
                  <td className="num">{formatQty(l.qty_received_milli)}</td>
                  <td className="num">
                    <input
                      className="input num"
                      style={{ width: 100 }}
                      value={recv[l.po_item_id]?.qty ?? ""}
                      disabled={l.qty_remaining_milli === 0}
                      onChange={(e) =>
                        setRecv({ ...recv, [l.po_item_id]: { ...recv[l.po_item_id], qty: e.target.value } })
                      }
                      aria-label={`Receive ${l.product_name}`}
                    />
                  </td>
                  <td className="num">
                    <input
                      className="input num"
                      style={{ width: 110 }}
                      value={recv[l.po_item_id]?.cost ?? ""}
                      onChange={(e) =>
                        setRecv({ ...recv, [l.po_item_id]: { ...recv[l.po_item_id], cost: e.target.value } })
                      }
                      aria-label={`Cost ${l.product_name}`}
                    />
                  </td>
                  <td className="num">{formatQty(l.qty_remaining_milli)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <div className="card-body row">
            <Button onClick={() => setReceiving(false)}>Cancel</Button>
            <Button variant="primary" className="right" onClick={doReceive} loading={act.busy}>
              Receive Goods
            </Button>
          </div>
        </div>
      ) : (
        <div className="card">
          <div className="card-head">
            <h3 className="grow">Items</h3>
            {editable ? (
              <div style={{ position: "relative", width: 360 }}>
                <input
                  className="input"
                  placeholder="Add product by name, SKU or barcode…"
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                  aria-label="Add product"
                />
                {results.length ? (
                  <div className="menu" style={{ left: 0, right: 0 }}>
                    {results.map((r) => (
                      <button
                        key={r.product_id}
                        onClick={() => {
                          if (!lines.some((l) => l.product_id === r.product_id))
                            setLines([
                              ...lines,
                              { product_id: r.product_id, name: r.name, qty: "1", cost: "", tax: "0" },
                            ]);
                          setSearch("");
                          setResults([]);
                        }}
                      >
                        {r.name} <span className="tiny">{r.sku}</span>
                      </button>
                    ))}
                  </div>
                ) : null}
              </div>
            ) : null}
          </div>
          <table className="table">
            <thead>
              <tr>
                <th>Product</th>
                <th className="num">Ordered Qty</th>
                <th className="num">Unit Cost</th>
                <th className="num">Tax %</th>
                <th className="num">Total</th>
                {po && !editable ? <th className="num">Received</th> : null}
                <th />
              </tr>
            </thead>
            <tbody>
              {lines.map((l, i) => {
                const q = parseQty(l.qty);
                const c = parseMoney(l.cost);
                const pl = po?.lines.find((x) => x.product_id === l.product_id);
                return (
                  <tr key={l.product_id}>
                    <td>{l.name}</td>
                    <td className="num">
                      {editable ? (
                        <input
                          className="input num"
                          style={{ width: 100 }}
                          value={l.qty}
                          onChange={(e) => setLines(lines.map((x, j) => (j === i ? { ...x, qty: e.target.value } : x)))}
                          aria-label="Quantity"
                        />
                      ) : (
                        l.qty
                      )}
                    </td>
                    <td className="num">
                      {editable ? (
                        <input
                          className="input num"
                          style={{ width: 110 }}
                          value={l.cost}
                          placeholder={formatAmount(0)}
                          onChange={(e) =>
                            setLines(lines.map((x, j) => (j === i ? { ...x, cost: e.target.value } : x)))
                          }
                          aria-label="Unit cost"
                        />
                      ) : (
                        formatMoney(c)
                      )}
                    </td>
                    <td className="num">
                      {editable ? (
                        <input
                          className="input num"
                          style={{ width: 70 }}
                          value={l.tax}
                          onChange={(e) => setLines(lines.map((x, j) => (j === i ? { ...x, tax: e.target.value } : x)))}
                          aria-label="Tax percent"
                        />
                      ) : (
                        `${l.tax}%`
                      )}
                    </td>
                    <td className="num">{q !== null && c !== null ? formatMoney(Math.round((c * q) / 1000)) : "—"}</td>
                    {po && !editable ? <td className="num">{pl ? formatQty(pl.qty_received_milli) : "—"}</td> : null}
                    <td className="num">
                      {editable ? (
                        <Button
                          size="sm"
                          variant="ghost"
                          aria-label="Remove line"
                          icon={<Trash2 size={14} />}
                          onClick={() => setLines(lines.filter((_, j) => j !== i))}
                        />
                      ) : null}
                    </td>
                  </tr>
                );
              })}
            </tbody>
            <tfoot>
              <tr>
                <td colSpan={4}>
                  {po
                    ? `Subtotal ${formatMoney(po.subtotal_minor)} · Tax ${formatMoney(po.tax_minor)}`
                    : "Subtotal (before tax)"}
                </td>
                <td className="num">{po && !editable ? formatMoney(po.total_minor) : formatMoney(totals)}</td>
                <td colSpan={2} />
              </tr>
            </tfoot>
          </table>
          {lines.length === 0 ? <div className="empty">Add products to this purchase order.</div> : null}
        </div>
      )}
      {po && po.receipts.length ? (
        <div className="card" style={{ marginTop: 16 }}>
          <div className="card-head">
            <h3>Deliveries received</h3>
          </div>
          <table className="table">
            <tbody>
              {po.receipts.map((r) => (
                <tr key={r.receipt_id}>
                  <td>{formatShort(r.created_at)}</td>
                  <td>{r.reference ?? "—"}</td>
                  <td>{r.user_name}</td>
                  <td className="num">{formatMoney(r.total_cost_minor)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
      {confirm && po ? (
        <Confirm
          title={
            confirm === "order"
              ? "Place order"
              : confirm === "cancel"
                ? "Cancel purchase order"
                : "Close purchase order short"
          }
          confirmLabel={confirm === "order" ? "Place Order" : confirm === "cancel" ? "Cancel PO" : "Close PO"}
          danger={confirm === "cancel"}
          busy={act.busy}
          error={act.error}
          onCancel={() => setConfirm(null)}
          onConfirm={async () => {
            const r = await act.run(() =>
              api.po.setStatus(
                po.po_id,
                confirm === "order" ? "ordered" : confirm === "cancel" ? "cancelled" : "received",
              ),
            );
            if (r) {
              setConfirm(null);
              await load();
            }
          }}
        >
          {confirm === "order"
            ? `Mark ${po.po_number} as ordered from ${po.supplier_name}. Lines can no longer be edited.`
            : confirm === "cancel"
              ? `Cancel ${po.po_number}? Nothing has been received on it.`
              : `Close ${po.po_number}? The remaining quantities will not be expected any more.`}
        </Confirm>
      ) : null}
    </div>
  );
}
