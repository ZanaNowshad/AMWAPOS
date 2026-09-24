import { useEffect, useMemo, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ArrowLeft, Archive, Download, Plus, Star, Trash2, Upload, RotateCcw } from "lucide-react";
import { api } from "../../api";
import type { CategoryRow, ProductDetail, ProductInput, ProductRow, TaxRuleRow, UnknownBarcodeRow } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { newOperationId } from "../../lib/ids";
import { formatAmount, formatMoney, formatPercent, formatQty, parseMoney, parsePercent, parseQty } from "../../lib/money";
import { formatDateTime, formatShort } from "../../lib/time";
import { Banner, Button, Checkbox, Chip, Empty, Field, Money, PageHeader, Skeleton, StockStatus, Tabs, TextInput } from "../../components/ui";
import { Confirm, DataTable, Drawer, Pager, download, useAction, useLoad, type Column } from "./common";
import { AdjustDialog } from "./inventory";

export function ProductsPage() {
  const { has } = useSession();
  const nav = useNavigate();
  const toast = useToast();
  const [q, setQ] = useState("");
  const [cat, setCat] = useState("");
  const [status, setStatus] = useState("active");
  const [stock, setStock] = useState("");
  const [offset, setOffset] = useState(0);
  const [limit, setLimit] = useState(50);
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [confirm, setConfirm] = useState<null | "archive" | "restore">(null);
  const act = useAction();
  const cats = useLoad(() => api.categories.list(), []);
  const { data, loading, error, reload } = useLoad(
    () => api.products.search({ q: q || undefined, category_id: cat || undefined, status, stock: stock || undefined, limit, offset }),
    [q, cat, status, stock, limit, offset],
  );
  const showCost = has("products.view_cost");
  const cols: Column<ProductRow>[] = [
    {
      key: "name",
      label: "Product",
      render: (r) => (
        <div>
          <div style={{ fontWeight: 600 }}>
            {r.is_favorite ? <Star size={12} fill="var(--warning)" color="var(--warning)" style={{ marginRight: 4 }} /> : null}
            {r.name}
          </div>
          {r.name_ar ? (
            <div className="tiny" dir="rtl">
              {r.name_ar}
            </div>
          ) : null}
        </div>
      ),
      sort: (r) => r.name.toLowerCase(),
    },
    { key: "sku", label: "SKU", render: (r) => <span className="mono">{r.sku}</span>, sort: (r) => r.sku },
    {
      key: "bc",
      label: "Barcodes",
      render: (r) =>
        r.primary_barcode ? (
          <span className="mono">
            {r.primary_barcode}
            {r.barcode_count > 1 ? <span className="chip" style={{ marginLeft: 6 }}>+{r.barcode_count - 1}</span> : null}
          </span>
        ) : (
          <span className="muted">—</span>
        ),
    },
    { key: "cat", label: "Category", render: (r) => r.category_name ?? "—", sort: (r) => r.category_name ?? "" },
    { key: "price", label: "Price", num: true, render: (r) => (r.price_minor === null ? <Chip tone="danger">No price</Chip> : <Money minor={r.price_minor} />), sort: (r) => r.price_minor ?? -1 },
    ...(showCost ? [{ key: "cost", label: "Cost", num: true, render: (r: ProductRow) => <Money minor={r.cost_minor} />, sort: (r: ProductRow) => r.cost_minor ?? 0 }] : []),
    { key: "stock", label: "Stock", num: true, render: (r) => (r.track_inventory ? formatQty(r.stock_milli) : "—"), sort: (r) => r.stock_milli },
    { key: "st", label: "Status", render: (r) => (r.active ? <StockStatus status={r.stock_status} /> : <Chip>Archived</Chip>) },
  ];
  const bulk = async (active: boolean) => {
    const n = await act.run(() => api.products.bulkSetActive(Array.from(sel), active));
    if (n !== undefined) {
      toast("success", `${n} product(s) ${active ? "restored" : "archived"}`);
      setSel(new Set());
      setConfirm(null);
      void reload();
    }
  };
  return (
    <div>
      <PageHeader
        title="Products"
        actions={
          <>
            {has("import.run") ? (
              <Button icon={<Upload size={16} />} onClick={() => nav("/admin/import")}>
                Import
              </Button>
            ) : null}
            <Button
              icon={<Download size={16} />}
              onClick={async () => {
                const csv = await act.run(() => api.products.exportCsv(status !== "active"));
                if (csv) download(`products-${new Date().toISOString().slice(0, 10)}.csv`, csv);
              }}
            >
              Export
            </Button>
            {has("products.manage") ? (
              <Button variant="primary" icon={<Plus size={16} />} onClick={() => nav("/admin/products/new")}>
                Add Product
              </Button>
            ) : null}
          </>
        }
      />
      <div className="filters">
        <input className="input" style={{ width: 280 }} placeholder="Search name, SKU or barcode…" value={q} onChange={(e) => (setQ(e.target.value), setOffset(0))} aria-label="Search products" />
        <select className="select" style={{ width: 180 }} value={cat} onChange={(e) => (setCat(e.target.value), setOffset(0))} aria-label="Category">
          <option value="">All categories</option>
          {(cats.data ?? []).map((c) => (
            <option key={c.category_id} value={c.category_id}>
              {c.name}
            </option>
          ))}
        </select>
        <select className="select" style={{ width: 140 }} value={status} onChange={(e) => (setStatus(e.target.value), setOffset(0))} aria-label="Status">
          <option value="active">Active</option>
          <option value="archived">Archived</option>
          <option value="all">All</option>
        </select>
        <select className="select" style={{ width: 150 }} value={stock} onChange={(e) => (setStock(e.target.value), setOffset(0))} aria-label="Stock">
          <option value="">Any stock</option>
          <option value="low">Low stock</option>
          <option value="out">Out of stock</option>
          <option value="negative">Negative</option>
        </select>
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <DataTable<ProductRow>
        rows={data?.rows ?? null}
        loading={loading}
        columns={cols}
        rowKey={(r) => r.product_id}
        onRowClick={(r) => nav(`/admin/products/${r.product_id}`)}
        selectable={has("products.manage")}
        selected={sel}
        onSelect={setSel}
        empty={
          <Empty
            title={q ? "No matching products" : "No products yet"}
            actions={
              q ? null : (
                <>
                  <Button onClick={() => nav("/admin/import")}>Import Products</Button>
                  <Button variant="primary" onClick={() => nav("/admin/products/new")}>
                    Add Product
                  </Button>
                </>
              )
            }
          >
            {q ? "Try a different search." : "Import your existing catalogue or create your first product."}
          </Empty>
        }
      />
      {data ? <Pager total={data.total} limit={limit} offset={offset} onChange={setOffset} onLimit={(l) => (setLimit(l), setOffset(0))} /> : null}
      {sel.size > 0 ? (
        <div className="batch-bar">
          <strong>{sel.size} selected</strong>
          <span className="grow" />
          {status !== "archived" ? (
            <Button size="sm" icon={<Archive size={14} />} onClick={() => setConfirm("archive")}>
              Archive
            </Button>
          ) : null}
          {status !== "active" ? (
            <Button size="sm" icon={<RotateCcw size={14} />} onClick={() => setConfirm("restore")}>
              Restore
            </Button>
          ) : null}
          <Button size="sm" onClick={() => setSel(new Set())}>
            Clear selection
          </Button>
        </div>
      ) : null}
      {confirm ? (
        <Confirm
          title={confirm === "archive" ? "Archive products" : "Restore products"}
          confirmLabel={confirm === "archive" ? `Archive ${sel.size}` : `Restore ${sel.size}`}
          danger={confirm === "archive"}
          busy={act.busy}
          error={act.error}
          onCancel={() => setConfirm(null)}
          onConfirm={() => void bulk(confirm === "restore")}
        >
          {confirm === "archive"
            ? `Archive ${sel.size} selected product(s)? They will no longer appear in POS search. Sales history is kept.`
            : `Restore ${sel.size} product(s)? They will be sellable again.`}
        </Confirm>
      ) : null}
    </div>
  );
}

const emptyInput = (tax: string): ProductInput => ({
  sku: "",
  name: "",
  name_ar: "",
  description: "",
  category_id: null,
  tax_rule_id: tax,
  unit: "pcs",
  track_inventory: true,
  allow_decimal_quantity: false,
  reorder_point_milli: 0,
  is_favorite: false,
});

export function ProductEditorPage() {
  const { id } = useParams();
  const [search] = useSearchParams();
  const isNew = !id;
  const nav = useNavigate();
  const { has } = useSession();
  const toast = useToast();
  const [tab, setTab] = useState<"general" | "barcodes" | "pricing" | "inventory" | "history">("general");
  const [detail, setDetail] = useState<ProductDetail | null>(null);
  const [form, setForm] = useState<ProductInput | null>(null);
  const [dirty, setDirty] = useState(false);
  const [price, setPrice] = useState("");
  const [cost, setCost] = useState("");
  const [barcodes, setBarcodes] = useState<string[]>(() => (search.get("barcode") ? [search.get("barcode")!] : []));
  const [bcInput, setBcInput] = useState("");
  const [opening, setOpening] = useState("");
  const [reorder, setReorder] = useState("0");
  const [leave, setLeave] = useState(false);
  const act = useAction();
  const cats = useLoad(() => api.categories.list(), []);
  const taxes = useLoad(() => api.tax.list(), []);
  const canEdit = has("products.manage");

  const loadDetail = async () => {
    if (!id) return;
    const d = await api.products.get(id);
    setDetail(d);
    setForm({
      sku: d.sku,
      name: d.name,
      name_ar: d.name_ar,
      description: d.description,
      category_id: d.category_id,
      tax_rule_id: d.tax_rule_id,
      unit: d.unit,
      track_inventory: d.track_inventory,
      allow_decimal_quantity: d.allow_decimal_quantity,
      reorder_point_milli: d.reorder_point_milli,
      is_favorite: d.is_favorite,
    });
    setReorder(formatQty(d.reorder_point_milli));
    setDirty(false);
  };
  useEffect(() => {
    if (id) void act.run(loadDetail);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);
  useEffect(() => {
    if (isNew && !form && taxes.data) {
      const t = taxes.data.find((x) => x.active && x.rate_bp > 0) ?? taxes.data.find((x) => x.active);
      setForm(emptyInput(t?.tax_rule_id ?? ""));
    }
  }, [isNew, form, taxes.data]);
  // Warn before leaving with unsaved edits (browser close / reload).
  useEffect(() => {
    const h = (e: BeforeUnloadEvent) => {
      if (dirty) e.preventDefault();
    };
    window.addEventListener("beforeunload", h);
    return () => window.removeEventListener("beforeunload", h);
  }, [dirty]);

  const set = <K extends keyof ProductInput>(k: K, v: ProductInput[K]) => {
    setForm((f) => (f ? { ...f, [k]: v } : f));
    setDirty(true);
  };

  const save = async () => {
    if (!form) return;
    const rp = parseQty(reorder);
    if (rp === null || rp < 0) {
      act.setError("Reorder point must be a number.");
      return;
    }
    const input = { ...form, reorder_point_milli: rp, sku: form.sku || null, name_ar: form.name_ar || null, description: form.description || null };
    if (isNew) {
      const p = parseMoney(price);
      if (p === null || p < 0) {
        act.setError("Enter a valid selling price.");
        return;
      }
      const c = cost.trim() ? parseMoney(cost) : null;
      if (cost.trim() && (c === null || c < 0)) {
        act.setError("Enter a valid cost.");
        return;
      }
      const o = opening.trim() ? parseQty(opening) : null;
      const created = await act.run(() =>
        api.products.create({ ...input, price_minor: p, cost_minor: has("products.view_cost") ? c : null, barcodes: bcInput.trim() ? [...barcodes, bcInput.trim()] : barcodes, opening_stock_milli: o }),
      );
      if (created) {
        toast("success", "Product created");
        setDirty(false);
        nav(`/admin/products/${created.product_id}`, { replace: true });
      }
    } else if (detail) {
      const updated = await act.run(() => api.products.update({ ...input, product_id: detail.product_id, expected_version: detail.version }));
      if (updated) {
        toast("success", "Product saved");
        await loadDetail();
      }
    }
  };

  if (!form) return <Skeleton rows={10} />;
  const tabs = isNew
    ? [{ key: "general" as const, label: "General" }]
    : [
        { key: "general" as const, label: "General" },
        { key: "barcodes" as const, label: "Barcodes" },
        { key: "pricing" as const, label: "Pricing" },
        { key: "inventory" as const, label: "Inventory" },
        { key: "history" as const, label: "History" },
      ];
  return (
    <div>
      <div className="page-header">
        <Button variant="ghost" icon={<ArrowLeft size={18} />} aria-label="Back" onClick={() => (dirty ? setLeave(true) : nav("/admin/products"))} />
        <div className="grow">
          <div className="tiny">Products</div>
          <h1>
            {isNew ? "New product" : detail?.name} {detail && !detail.active ? <Chip>Archived</Chip> : null}
          </h1>
        </div>
        {!isNew && detail && canEdit ? (
          <Button
            icon={detail.active ? <Archive size={16} /> : <RotateCcw size={16} />}
            onClick={async () => {
              const d = await act.run(() => api.products.setActive(detail.product_id, !detail.active));
              if (d) {
                toast("success", d.active ? "Product restored" : "Product archived");
                await loadDetail();
              }
            }}
          >
            {detail.active ? "Archive" : "Restore"}
          </Button>
        ) : null}
        {canEdit && (tab === "general" || isNew) ? (
          <Button variant="primary" onClick={save} loading={act.busy} disabled={!form.name.trim()}>
            Save
          </Button>
        ) : null}
      </div>
      <Tabs tabs={tabs} value={tab} onChange={setTab} />
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {tab === "general" ? (
        <div className="grid-3">
          <div className="card card-pad">
            <div className="form-grid">
              <TextInput label="Name" required value={form.name} onChange={(e) => set("name", e.target.value)} fieldClass="span-2" disabled={!canEdit} autoFocus={isNew} />
              <TextInput label="Arabic name" dir="rtl" value={form.name_ar ?? ""} onChange={(e) => set("name_ar", e.target.value)} disabled={!canEdit} />
              <TextInput label="SKU" value={form.sku ?? ""} onChange={(e) => set("sku", e.target.value)} hint={isNew ? "Leave empty to generate." : undefined} disabled={!canEdit} />
              <Field label="Category">
                <select className="select" value={form.category_id ?? ""} onChange={(e) => set("category_id", e.target.value || null)} disabled={!canEdit}>
                  <option value="">Uncategorised</option>
                  {(cats.data ?? []).map((c) => (
                    <option key={c.category_id} value={c.category_id}>
                      {c.name}
                    </option>
                  ))}
                </select>
              </Field>
              <Field label="Tax rule" required>
                <select className="select" value={form.tax_rule_id} onChange={(e) => set("tax_rule_id", e.target.value)} disabled={!canEdit}>
                  {(taxes.data ?? [])
                    .filter((t) => t.active || t.tax_rule_id === form.tax_rule_id)
                    .map((t) => (
                      <option key={t.tax_rule_id} value={t.tax_rule_id}>
                        {t.name} ({formatPercent(t.rate_bp)} {t.inclusive ? "incl." : "excl."})
                      </option>
                    ))}
                </select>
              </Field>
              <Field label="Description" className="span-2">
                <textarea className="textarea" value={form.description ?? ""} onChange={(e) => set("description", e.target.value)} disabled={!canEdit} />
              </Field>
              <TextInput label="Unit" value={form.unit} onChange={(e) => set("unit", e.target.value)} hint="pcs, kg, box…" disabled={!canEdit} />
              <TextInput label="Reorder point" className="num" value={reorder} onChange={(e) => (setReorder(e.target.value), setDirty(true))} disabled={!canEdit} />
              <div className="col span-2">
                <Checkbox label="Track stock" checked={form.track_inventory} onChange={(v) => set("track_inventory", v)} disabled={!canEdit} />
                <Checkbox label="Sold by weight / decimal quantity" checked={form.allow_decimal_quantity} onChange={(v) => set("allow_decimal_quantity", v)} disabled={!canEdit} />
                <Checkbox label="Show in POS favorites" checked={form.is_favorite} onChange={(v) => set("is_favorite", v)} disabled={!canEdit} />
              </div>
            </div>
          </div>
          <div className="col gap-16">
            {isNew ? (
              <div className="card card-pad col gap-16">
                <h3>Price & stock</h3>
                <TextInput label="Selling price" required className="num" inputMode="decimal" value={price} onChange={(e) => setPrice(e.target.value)} placeholder={formatAmount(0)} />
                {has("products.view_cost") ? <TextInput label="Cost" className="num" inputMode="decimal" value={cost} onChange={(e) => setCost(e.target.value)} placeholder={formatAmount(0)} /> : null}
                <TextInput label="Opening stock" className="num" inputMode="decimal" value={opening} onChange={(e) => setOpening(e.target.value)} disabled={!form.track_inventory} />
                <Field label="Barcodes" hint="Press Enter after each barcode. Barcodes are stored as text; leading zeros are kept.">
                  <div className="row wrap">
                    {barcodes.map((b) => (
                      <span key={b} className="chip brand mono">
                        {b}
                        <button className="link" aria-label={`Remove ${b}`} onClick={() => setBarcodes(barcodes.filter((x) => x !== b))}>
                          ×
                        </button>
                      </span>
                    ))}
                  </div>
                  <input
                    className="input mono"
                    value={bcInput}
                    onChange={(e) => setBcInput(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        e.preventDefault();
                        const v = bcInput.trim();
                        if (v && !barcodes.includes(v)) setBarcodes([...barcodes, v]);
                        setBcInput("");
                      }
                    }}
                    placeholder="Scan or type barcode"
                  />
                </Field>
              </div>
            ) : detail ? (
              <div className="card card-pad">
                <dl className="kv">
                  <dt>Selling price</dt>
                  <dd>
                    <Money minor={detail.price_minor} />
                  </dd>
                  {detail.avg_cost_minor !== null ? (
                    <>
                      <dt>Average cost</dt>
                      <dd>
                        <Money minor={detail.avg_cost_minor} />
                      </dd>
                    </>
                  ) : null}
                  <dt>Stock</dt>
                  <dd>{detail.track_inventory ? `${formatQty(detail.stock_milli)} ${detail.unit}` : "Not tracked"}</dd>
                  <dt>Barcodes</dt>
                  <dd>{detail.barcodes.length}</dd>
                  <dt>Updated</dt>
                  <dd>{formatDateTime(detail.updated_at)}</dd>
                </dl>
              </div>
            ) : null}
          </div>
        </div>
      ) : null}
      {tab === "barcodes" && detail ? <BarcodesTab detail={detail} onChanged={setDetail} canEdit={canEdit} /> : null}
      {tab === "pricing" && detail ? <PricingTab detail={detail} onChanged={loadDetail} /> : null}
      {tab === "inventory" && detail ? <InventoryTab detail={detail} onChanged={loadDetail} /> : null}
      {tab === "history" && detail ? <HistoryTab productId={detail.product_id} /> : null}
      {leave ? (
        <Confirm title="Discard unsaved changes?" confirmLabel="Discard" danger onCancel={() => setLeave(false)} onConfirm={() => nav("/admin/products")}>
          You have unsaved changes to this product.
        </Confirm>
      ) : null}
    </div>
  );
}

function BarcodesTab({ detail, onChanged, canEdit }: { detail: ProductDetail; onChanged: (d: ProductDetail) => void; canEdit: boolean }) {
  const nav = useNavigate();
  const [v, setV] = useState("");
  const [owner, setOwner] = useState<{ id: string; name: string } | null>(null);
  const act = useAction();
  const add = async () => {
    setOwner(null);
    const r = await act.run(async () => {
      try {
        return await api.barcodes.add(detail.product_id, v, false);
      } catch (e) {
        const d = (e as { details?: Record<string, string> }).details;
        if (d?.product_id) setOwner({ id: d.product_id, name: d.product_name });
        throw e;
      }
    });
    if (r) {
      onChanged(r);
      setV("");
    }
  };
  return (
    <div className="card">
      <div className="card-head">
        <h3 className="grow">Barcodes</h3>
      </div>
      <table className="table">
        <thead>
          <tr>
            <th>Barcode</th>
            <th>Source</th>
            <th>Added</th>
            <th>Primary</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {detail.barcodes.map((b) => (
            <tr key={b.barcode_id}>
              <td className="mono">{b.barcode}</td>
              <td>{b.source}</td>
              <td>{formatShort(b.created_at)}</td>
              <td>{b.is_primary ? <Chip tone="brand">Primary</Chip> : null}</td>
              <td className="num">
                {canEdit ? (
                  <div className="row" style={{ justifyContent: "flex-end" }}>
                    {!b.is_primary ? (
                      <Button size="sm" onClick={async () => onChanged((await act.run(() => api.barcodes.setPrimary(b.barcode_id))) ?? detail)}>
                        Set Primary
                      </Button>
                    ) : null}
                    <Button size="sm" variant="danger-outline" icon={<Trash2 size={14} />} onClick={async () => onChanged((await act.run(() => api.barcodes.remove(b.barcode_id))) ?? detail)}>
                      Remove
                    </Button>
                  </div>
                ) : null}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {detail.barcodes.length === 0 ? <div className="empty">No barcodes. Add one so the product can be scanned.</div> : null}
      {canEdit ? (
        <div className="card-body col">
          <div className="row">
            <input className="input mono grow" placeholder="Scan or type a barcode" value={v} onChange={(e) => setV(e.target.value)} onKeyDown={(e) => e.key === "Enter" && v.trim() && add()} aria-label="New barcode" />
            <Button variant="primary" icon={<Plus size={16} />} onClick={add} disabled={!v.trim()} loading={act.busy}>
              Add Barcode
            </Button>
          </div>
          {act.error ? (
            <Banner tone="danger" action={owner ? <button className="link" onClick={() => nav(`/admin/products/${owner.id}`)}>Open {owner.name}</button> : null}>
              {act.error}
            </Banner>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function PricingTab({ detail, onChanged }: { detail: ProductDetail; onChanged: () => Promise<void> }) {
  const { has } = useSession();
  const toast = useToast();
  const [price, setPrice] = useState(detail.price_minor !== null ? formatAmount(detail.price_minor) : "");
  const [reason, setReason] = useState("");
  const [cost, setCost] = useState(detail.avg_cost_minor !== null ? formatAmount(detail.avg_cost_minor) : "");
  const act = useAction();
  const margin = detail.price_minor && detail.avg_cost_minor !== null ? detail.price_minor - detail.avg_cost_minor : null;
  return (
    <div className="grid-2">
      <div className="card card-pad col gap-16">
        <h3>Selling price</h3>
        <div className="due">{formatMoney(detail.price_minor)}</div>
        {has("prices.manage") ? (
          <>
            <div className="form-grid">
              <TextInput label="New price" className="num" inputMode="decimal" value={price} onChange={(e) => setPrice(e.target.value)} />
              <TextInput label="Reason" value={reason} onChange={(e) => setReason(e.target.value)} />
            </div>
            <Button
              variant="primary"
              disabled={parseMoney(price) === null}
              loading={act.busy}
              onClick={async () => {
                const r = await act.run(() => api.products.priceUpdate(detail.product_id, parseMoney(price)!, reason || null));
                if (r) {
                  toast("success", "Price changed");
                  setReason("");
                  await onChanged();
                }
              }}
            >
              Change Price
            </Button>
          </>
        ) : null}
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <table className="table">
          <thead>
            <tr>
              <th>Effective</th>
              <th>Until</th>
              <th className="num">Price</th>
              <th>Changed by</th>
              <th>Reason</th>
            </tr>
          </thead>
          <tbody>
            {detail.price_history.map((p) => (
              <tr key={p.price_id}>
                <td>{formatShort(p.effective_from)}</td>
                <td>{p.effective_to ? formatShort(p.effective_to) : <Chip tone="success">Current</Chip>}</td>
                <td className="num">{formatMoney(p.amount_minor)}</td>
                <td>{p.created_by_name ?? "—"}</td>
                <td>{p.reason ?? "—"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {detail.cost_history ? (
        <div className="card card-pad col gap-16">
          <h3>Cost</h3>
          <dl className="kv">
            <dt>Average cost</dt>
            <dd>{formatMoney(detail.avg_cost_minor)}</dd>
            <dt>Last cost</dt>
            <dd>{formatMoney(detail.last_cost_minor)}</dd>
            <dt>Unit margin</dt>
            <dd className={margin !== null && margin < 0 ? "neg-num" : ""}>{margin === null ? "—" : formatMoney(margin)}</dd>
          </dl>
          {has("products.manage") ? (
            <div className="row" style={{ alignItems: "flex-end" }}>
              <TextInput label="Set standard cost" className="num" value={cost} onChange={(e) => setCost(e.target.value)} fieldClass="grow" hint="Overrides the weighted average. Receiving updates it automatically." />
              <Button
                disabled={parseMoney(cost) === null}
                onClick={async () => {
                  const r = await act.run(() => api.products.costUpdate(detail.product_id, parseMoney(cost)!, "Manual"));
                  if (r) {
                    toast("success", "Cost updated");
                    await onChanged();
                  }
                }}
              >
                Save cost
              </Button>
            </div>
          ) : null}
          <table className="table">
            <thead>
              <tr>
                <th>Date</th>
                <th>Source</th>
                <th>Supplier</th>
                <th className="num">Cost</th>
              </tr>
            </thead>
            <tbody>
              {detail.cost_history.map((c) => (
                <tr key={c.cost_id}>
                  <td>{formatShort(c.effective_at)}</td>
                  <td>{c.source}</td>
                  <td>{c.supplier_name ?? "—"}</td>
                  <td className="num">{formatMoney(c.cost_minor)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
    </div>
  );
}

function InventoryTab({ detail, onChanged }: { detail: ProductDetail; onChanged: () => Promise<void> }) {
  const { has } = useSession();
  const [adjust, setAdjust] = useState(false);
  const moves = useLoad(() => api.inventory.movements({ product_id: detail.product_id, limit: 100 }), [detail.product_id, detail.stock_milli]);
  return (
    <div className="col gap-16">
      <div className="card card-pad row">
        <div className="grow">
          <div className="tiny">On hand</div>
          <div className="due">
            {detail.track_inventory ? formatQty(detail.stock_milli) : "Not tracked"} <span className="small muted">{detail.unit}</span>
          </div>
          <div className="small muted">Reorder point {formatQty(detail.reorder_point_milli)}</div>
        </div>
        <StockStatus status={detail.stock_status} />
        {has("inventory.adjust") && detail.track_inventory ? (
          <Button variant="primary" onClick={() => setAdjust(true)}>
            Adjust stock
          </Button>
        ) : null}
      </div>
      <DataTable
        rows={moves.data?.rows ?? null}
        loading={moves.loading}
        rowKey={(r) => r.movement_id}
        empty={<div className="empty">No stock movements yet.</div>}
        columns={[
          { key: "t", label: "Time", render: (r) => formatShort(r.created_at) },
          { key: "k", label: "Type", render: (r) => r.kind },
          { key: "q", label: "Qty Change", num: true, render: (r) => <span className={r.qty_delta_milli > 0 ? "pos-num" : "neg-num"}>{r.qty_delta_milli > 0 ? "+" : ""}{formatQty(r.qty_delta_milli)}</span> },
          { key: "b", label: "Balance", num: true, render: (r) => formatQty(r.balance_after_milli) },
          { key: "s", label: "Source", render: (r) => r.source_ref ?? r.source_type },
          { key: "r", label: "Reason", render: (r) => r.reason ?? "—" },
          { key: "u", label: "User", render: (r) => r.user_name ?? "—" },
        ]}
      />
      {adjust ? (
        <AdjustDialog
          product={detail}
          onClose={() => setAdjust(false)}
          onDone={async () => {
            setAdjust(false);
            await onChanged();
          }}
        />
      ) : null}
    </div>
  );
}

function HistoryTab({ productId }: { productId: string }) {
  const { has } = useSession();
  const audit = useLoad(() => (has("audit.view") ? api.audit.list({ entity_type: "product", entity_id: productId, limit: 100 }) : Promise.resolve(null)), [productId]);
  if (!has("audit.view")) return <div className="empty">Your role cannot view the audit history.</div>;
  return (
    <DataTable
      rows={audit.data?.rows ?? null}
      loading={audit.loading}
      rowKey={(r) => r.audit_id}
      empty={<div className="empty">No history.</div>}
      columns={[
        { key: "t", label: "Time", render: (r) => formatDateTime(r.created_at) },
        { key: "e", label: "Event", render: (r) => r.event_type },
        { key: "u", label: "User", render: (r) => r.user_name ?? "System" },
        { key: "a", label: "Approved by", render: (r) => r.approver_name ?? "—" },
        { key: "d", label: "Change", render: (r) => <span className="small mono ellipsis" style={{ maxWidth: 420, display: "inline-block" }}>{JSON.stringify(r.after ?? r.before)}</span> },
      ]}
    />
  );
}

export function CategoriesPage() {
  const { has } = useSession();
  const toast = useToast();
  const { data, loading, error, reload } = useLoad(() => api.categories.list(true), []);
  const [editing, setEditing] = useState<CategoryRow | "new" | null>(null);
  const [name, setName] = useState("");
  const [parent, setParent] = useState("");
  const [archive, setArchive] = useState<CategoryRow | null>(null);
  const [target, setTarget] = useState("");
  const act = useAction();
  const canEdit = has("products.manage");
  const open = (c: CategoryRow | "new") => {
    setEditing(c);
    setName(c === "new" ? "" : c.name);
    setParent(c === "new" ? "" : c.parent_id ?? "");
  };
  const byId = new Map((data ?? []).map((c) => [c.category_id, c]));
  return (
    <div>
      <PageHeader
        title="Categories"
        actions={
          canEdit ? (
            <Button variant="primary" icon={<Plus size={16} />} onClick={() => open("new")}>
              Add Category
            </Button>
          ) : null
        }
      />
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<CategoryRow>
        rows={data}
        loading={loading}
        rowKey={(r) => r.category_id}
        onRowClick={canEdit ? open : undefined}
        columns={[
          { key: "n", label: "Name", render: (r) => (r.parent_id ? `${byId.get(r.parent_id)?.name ?? "…"} › ${r.name}` : r.name), sort: (r) => r.name },
          { key: "p", label: "Active products", num: true, render: (r) => r.product_count, sort: (r) => r.product_count },
          { key: "s", label: "Status", render: (r) => (r.active ? <Chip tone="success">Active</Chip> : <Chip>Archived</Chip>) },
          {
            key: "a",
            label: "",
            num: true,
            render: (r) =>
              canEdit && r.active ? (
                <Button size="sm" onClick={(e) => (e.stopPropagation(), setArchive(r), setTarget(""))}>
                  Archive
                </Button>
              ) : null,
          },
        ]}
      />
      {editing ? (
        <Drawer title={editing === "new" ? "New category" : `Edit ${editing.name}`} onClose={() => setEditing(null)}>
          <div className="col gap-16">
            <TextInput label="Name" required value={name} onChange={(e) => setName(e.target.value)} autoFocus />
            <Field label="Parent category">
              <select className="select" value={parent} onChange={(e) => setParent(e.target.value)}>
                <option value="">None (top level)</option>
                {(data ?? [])
                  .filter((c) => c.active && (editing === "new" || c.category_id !== editing.category_id))
                  .map((c) => (
                    <option key={c.category_id} value={c.category_id}>
                      {c.name}
                    </option>
                  ))}
              </select>
            </Field>
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
            <Button
              variant="primary"
              loading={act.busy}
              disabled={!name.trim()}
              onClick={async () => {
                const r = await act.run(() => api.categories.save({ category_id: editing === "new" ? null : editing.category_id, name, parent_id: parent || null }));
                if (r) {
                  toast("success", "Category saved");
                  setEditing(null);
                  void reload();
                }
              }}
            >
              Save
            </Button>
          </div>
        </Drawer>
      ) : null}
      {archive ? (
        <Confirm
          title={`Archive ${archive.name}`}
          confirmLabel="Archive"
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setArchive(null)}
          onConfirm={async () => {
            const r = await act.run(() => api.categories.archive(archive.category_id, target || null));
            if (r !== undefined) {
              setArchive(null);
              void reload();
            }
          }}
        >
          {archive.product_count > 0 ? (
            <div className="col">
              <span>{archive.product_count} product(s) use this category. Move them to:</span>
              <select className="select" value={target} onChange={(e) => setTarget(e.target.value)} aria-label="Move products to">
                <option value="">Choose category…</option>
                {(data ?? [])
                  .filter((c) => c.active && c.category_id !== archive.category_id)
                  .map((c) => (
                    <option key={c.category_id} value={c.category_id}>
                      {c.name}
                    </option>
                  ))}
              </select>
            </div>
          ) : (
            "The category will be hidden from pickers. Historical sales keep their category."
          )}
        </Confirm>
      ) : null}
    </div>
  );
}

type Rule = { kind: "percent" | "fixed" | "set" | "round"; value: string };

export function PricingPage() {
  const toast = useToast();
  const { has } = useSession();
  const [q, setQ] = useState("");
  const [cat, setCat] = useState("");
  const [rule, setRule] = useState<Rule>({ kind: "percent", value: "5" });
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [confirm, setConfirm] = useState(false);
  const [reason, setReason] = useState("");
  const act = useAction();
  const cats = useLoad(() => api.categories.list(), []);
  const { data, loading, reload } = useLoad(() => api.products.search({ q: q || undefined, category_id: cat || undefined, limit: 500 }), [q, cat]);
  const computeNew = (p: ProductRow): number | null => {
    if (p.price_minor === null) return null;
    const base = p.price_minor;
    if (rule.kind === "percent") {
      const bp = parsePercent(rule.value);
      if (bp === null) return null;
      return Math.max(0, base + Math.round((base * bp) / 10000));
    }
    if (rule.kind === "fixed") {
      const v = parseMoney(rule.value);
      return v === null ? null : Math.max(0, base + v);
    }
    if (rule.kind === "set") return parseMoney(rule.value);
    // round up to the nearest step, e.g. 0.050
    const step = parseMoney(rule.value);
    if (!step || step <= 0) return null;
    return Math.ceil(base / step) * step;
  };
  const changes = useMemo(
    () =>
      (data?.rows ?? [])
        .filter((p) => sel.has(p.product_id))
        .map((p) => ({ p, next: computeNew(p) }))
        .filter((x) => x.next !== null && x.next !== x.p.price_minor),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [data, sel, rule],
  );
  const apply = async () => {
    const r = await act.run(() => api.products.bulkPrice(changes.map((c) => ({ product_id: c.p.product_id, amount_minor: c.next! })), reason || "Bulk price change", newOperationId()));
    if (r) {
      toast("success", `${r.changed} price(s) changed`);
      setConfirm(false);
      setSel(new Set());
      void reload();
    }
  };
  if (!has("prices.manage")) return null;
  return (
    <div>
      <PageHeader title="Pricing" subtitle="Bulk price changes. Every change is previewed first and recorded in price history." />
      <div className="filters">
        <input className="input" style={{ width: 240 }} placeholder="Search products…" value={q} onChange={(e) => setQ(e.target.value)} aria-label="Search" />
        <select className="select" style={{ width: 180 }} value={cat} onChange={(e) => setCat(e.target.value)} aria-label="Category">
          <option value="">All categories</option>
          {(cats.data ?? []).map((c) => (
            <option key={c.category_id} value={c.category_id}>
              {c.name}
            </option>
          ))}
        </select>
        <span className="grow" />
        <select className="select" style={{ width: 170 }} value={rule.kind} onChange={(e) => setRule({ kind: e.target.value as Rule["kind"], value: e.target.value === "round" ? "0.050" : rule.value })} aria-label="Rule">
          <option value="percent">Percentage change</option>
          <option value="fixed">Fixed change</option>
          <option value="set">Set price</option>
          <option value="round">Round up to</option>
        </select>
        <input className="input num" style={{ width: 110 }} value={rule.value} onChange={(e) => setRule({ ...rule, value: e.target.value })} aria-label="Rule value" />
        <Button variant="primary" disabled={changes.length === 0} onClick={() => setConfirm(true)}>
          Preview {changes.length} change(s)
        </Button>
      </div>
      <DataTable<ProductRow>
        rows={data?.rows ?? null}
        loading={loading}
        rowKey={(r) => r.product_id}
        selectable
        selected={sel}
        onSelect={setSel}
        maxHeight="62vh"
        columns={[
          { key: "n", label: "Product", render: (r) => r.name, sort: (r) => r.name },
          { key: "p", label: "Current Price", num: true, render: (r) => <Money minor={r.price_minor} /> },
          ...(has("products.view_cost")
            ? [
                { key: "c", label: "Cost", num: true, render: (r: ProductRow) => <Money minor={r.cost_minor} /> },
                {
                  key: "m",
                  label: "Margin",
                  num: true,
                  render: (r: ProductRow) => (r.price_minor && r.cost_minor !== null ? formatPercent(Math.round(((r.price_minor - r.cost_minor) * 10000) / r.price_minor)) : "—"),
                },
              ]
            : []),
          { key: "x", label: "New Price", num: true, render: (r) => (sel.has(r.product_id) ? <strong>{formatMoney(computeNew(r))}</strong> : <span className="muted">—</span>) },
        ]}
      />
      {confirm ? (
        <Confirm title="Apply price changes" confirmLabel={`Change ${changes.length} price(s)`} busy={act.busy} error={act.error} onCancel={() => setConfirm(false)} onConfirm={apply}>
          <div className="col gap-16">
            <div>Change the selling price of {changes.length} product(s). New prices apply immediately at every till.</div>
            <div style={{ maxHeight: 240, overflow: "auto" }}>
              <table className="table">
                <tbody>
                  {changes.slice(0, 200).map((c) => (
                    <tr key={c.p.product_id}>
                      <td>{c.p.name}</td>
                      <td className="num">{formatMoney(c.p.price_minor)}</td>
                      <td className="num">→ {formatMoney(c.next)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <TextInput label="Reason" value={reason} onChange={(e) => setReason(e.target.value)} />
          </div>
        </Confirm>
      ) : null}
    </div>
  );
}

export function UnknownBarcodesPage() {
  const nav = useNavigate();
  const toast = useToast();
  const [status, setStatus] = useState("open");
  const { data, loading, error, reload } = useLoad(() => api.barcodes.unknown(status), [status]);
  const [open, setOpen] = useState<UnknownBarcodeRow | null>(null);
  const [q, setQ] = useState("");
  const results = useLoad(() => (q.trim() ? api.products.search({ q, limit: 10 }) : Promise.resolve(null)), [q]);
  const act = useAction();
  return (
    <div>
      <PageHeader title="Unknown Barcodes" subtitle="Barcodes scanned at the till that are not in the catalogue." />
      <div className="filters">
        {["open", "resolved", "dismissed", "all"].map((s) => (
          <button key={s} className={`filter-chip ${status === s ? "active" : ""}`} onClick={() => setStatus(s)}>
            {s[0].toUpperCase() + s.slice(1)}
          </button>
        ))}
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<UnknownBarcodeRow>
        rows={data}
        loading={loading}
        rowKey={(r) => r.barcode}
        onRowClick={(r) => r.status === "open" && (setOpen(r), setQ(""))}
        empty={<div className="empty">No unknown barcodes. Every scanned barcode was recognised.</div>}
        columns={[
          { key: "b", label: "Barcode", render: (r) => <span className="mono">{r.barcode}</span> },
          { key: "f", label: "First Seen", render: (r) => formatShort(r.first_seen_at), sort: (r) => r.first_seen_at },
          { key: "l", label: "Last Seen", render: (r) => formatShort(r.last_seen_at), sort: (r) => r.last_seen_at },
          { key: "c", label: "Scan Count", num: true, render: (r) => r.scan_count, sort: (r) => r.scan_count },
          { key: "t", label: "Terminal", render: (r) => r.last_device_name ?? "—" },
          {
            key: "s",
            label: "Status",
            render: (r) => (r.status === "open" ? <Chip tone="warning">Open</Chip> : r.status === "resolved" ? <Chip tone="success">Resolved · {r.resolved_product_name}</Chip> : <Chip>Dismissed</Chip>),
          },
        ]}
      />
      {open ? (
        <Drawer title={`Resolve ${open.barcode}`} onClose={() => setOpen(null)}>
          <div className="col gap-16">
            <div className="small muted">
              Scanned {open.scan_count} time(s), last on {formatDateTime(open.last_seen_at)}.
            </div>
            <h3>Assign to an existing product</h3>
            <input className="input" placeholder="Search product by name or SKU…" value={q} onChange={(e) => setQ(e.target.value)} autoFocus />
            {(results.data?.rows ?? []).map((p) => (
              <div key={p.product_id} className="result-row">
                <div className="grow">
                  <div style={{ fontWeight: 600 }}>{p.name}</div>
                  <div className="tiny">
                    {p.sku} · {p.primary_barcode ?? "no barcode"}
                  </div>
                </div>
                <Button
                  size="sm"
                  variant="primary"
                  onClick={async () => {
                    const r = await act.run(() => api.barcodes.add(p.product_id, open.barcode, false));
                    if (r) {
                      toast("success", `Barcode assigned to ${p.name}`);
                      setOpen(null);
                      void reload();
                    }
                  }}
                >
                  Assign
                </Button>
              </div>
            ))}
            <div className="divider" />
            <div className="row">
              <Button variant="primary" onClick={() => nav(`/admin/products/new?barcode=${encodeURIComponent(open.barcode)}`)}>
                Create product
              </Button>
              <Button
                variant="danger-outline"
                className="right"
                onClick={async () => {
                  const r = await act.run(() => api.barcodes.dismiss(open.barcode));
                  if (r !== undefined) {
                    setOpen(null);
                    void reload();
                  }
                }}
              >
                Dismiss
              </Button>
            </div>
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          </div>
        </Drawer>
      ) : null}
    </div>
  );
}

export type { TaxRuleRow };
