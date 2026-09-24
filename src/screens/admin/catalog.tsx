import { useEffect, useMemo, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { ArrowLeft, Archive, Download, Plus, Star, Trash2, Upload, RotateCcw } from "lucide-react";
import { api } from "../../api";
import type {
  CategoryRow,
  ProductDetail,
  ProductInput,
  ProductRow,
  TaxRuleRow,
  UnknownBarcodeRow,
} from "../../api/types";
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
  mulDivRound,
} from "../../lib/money";
import { formatDateTime, formatShort } from "../../lib/time";
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
  StockStatus,
  Tabs,
  TextInput,
} from "../../components/ui";
import { Confirm, DataTable, Drawer, Pager, download, useAction, useLoad, type Column } from "./common";
import { AdjustDialog } from "./inventory";
import { t } from "../../i18n";
import { codeLabel } from "../../i18n/codes";

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
    () =>
      api.products.search({
        q: q || undefined,
        category_id: cat || undefined,
        status,
        stock: stock || undefined,
        limit,
        offset,
      }),
    [q, cat, status, stock, limit, offset],
  );
  const showCost = has("products.view_cost");
  const cols: Column<ProductRow>[] = [
    {
      key: "name",
      label: t("Product"),
      render: (r) => (
        <div>
          <div style={{ fontWeight: 600 }}>
            {r.is_favorite ? (
              <Star size={12} fill="var(--warning)" color="var(--warning)" style={{ marginInlineEnd: 4 }} />
            ) : null}
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
    { key: "sku", label: t("SKU"), render: (r) => <span className="mono">{r.sku}</span>, sort: (r) => r.sku },
    {
      key: "bc",
      label: t("Barcodes"),
      render: (r) =>
        r.primary_barcode ? (
          <span className="mono">
            {r.primary_barcode}
            {r.barcode_count > 1 ? (
              <span className="chip" style={{ marginInlineStart: 6 }}>
                +{r.barcode_count - 1}
              </span>
            ) : null}
          </span>
        ) : (
          <span className="muted">—</span>
        ),
    },
    { key: "cat", label: t("Category"), render: (r) => r.category_name ?? "—", sort: (r) => r.category_name ?? "" },
    {
      key: "price",
      label: t("Price"),
      num: true,
      render: (r) =>
        r.price_minor === null ? <Chip tone="danger">{t("No price")}</Chip> : <Money minor={r.price_minor} />,
      sort: (r) => r.price_minor ?? -1,
    },
    ...(showCost
      ? [
          {
            key: "cost",
            label: t("Cost"),
            num: true,
            render: (r: ProductRow) => <Money minor={r.cost_minor} />,
            sort: (r: ProductRow) => r.cost_minor ?? 0,
          },
        ]
      : []),
    {
      key: "stock",
      label: t("Stock"),
      num: true,
      render: (r) => (r.track_inventory ? formatQty(r.stock_milli) : "—"),
      sort: (r) => r.stock_milli,
    },
    {
      key: "st",
      label: t("Status"),
      render: (r) => (r.active ? <StockStatus status={r.stock_status} /> : <Chip>{t("Archived")}</Chip>),
    },
  ];
  const bulk = async (active: boolean) => {
    const n = await act.run(() => api.products.bulkSetActive(Array.from(sel), active));
    if (n !== undefined) {
      toast("success", t("{0} product(s) {1}", n, active ? "restored" : "archived"));
      setSel(new Set());
      setConfirm(null);
      void reload();
    }
  };
  return (
    <div>
      <PageHeader
        title={t("Products")}
        actions={
          <>
            {has("import.run") ? (
              <Button icon={<Upload size={16} />} onClick={() => nav("/admin/import")}>
                {t("Import")}
              </Button>
            ) : null}
            <Button
              icon={<Download size={16} />}
              onClick={async () => {
                const csv = await act.run(() => api.products.exportCsv(status !== "active"));
                if (csv) download(`products-${new Date().toISOString().slice(0, 10)}.csv`, csv);
              }}
            >
              {t("Export")}
            </Button>
            {has("products.manage") ? (
              <Button variant="primary" icon={<Plus size={16} />} onClick={() => nav("/admin/products/new")}>
                {t("Add Product")}
              </Button>
            ) : null}
          </>
        }
      />
      <div className="filters">
        <input
          className="input"
          style={{ width: 280 }}
          placeholder={t("Search name, SKU or barcode…")}
          value={q}
          onChange={(e) => (setQ(e.target.value), setOffset(0))}
          aria-label={t("Search products")}
        />
        <select
          className="select"
          style={{ width: 180 }}
          value={cat}
          onChange={(e) => (setCat(e.target.value), setOffset(0))}
          aria-label={t("Category")}
        >
          <option value="">{t("All categories")}</option>
          {(cats.data ?? []).map((c) => (
            <option key={c.category_id} value={c.category_id}>
              {c.name}
            </option>
          ))}
        </select>
        <select
          className="select"
          style={{ width: 140 }}
          value={status}
          onChange={(e) => (setStatus(e.target.value), setOffset(0))}
          aria-label={t("Status")}
        >
          <option value="active">{t("Active")}</option>
          <option value="archived">{t("Archived")}</option>
          <option value="all">{t("All")}</option>
        </select>
        <select
          className="select"
          style={{ width: 150 }}
          value={stock}
          onChange={(e) => (setStock(e.target.value), setOffset(0))}
          aria-label={t("Stock")}
        >
          <option value="">{t("Any stock")}</option>
          <option value="low">{t("Low stock")}</option>
          <option value="out">{t("Out of stock")}</option>
          <option value="negative">{t("Negative")}</option>
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
            title={q ? t("No matching products") : t("No products yet")}
            actions={
              q ? null : (
                <>
                  <Button onClick={() => nav("/admin/import")}>{t("Import Products")}</Button>
                  <Button variant="primary" onClick={() => nav("/admin/products/new")}>
                    {t("Add Product")}
                  </Button>
                </>
              )
            }
          >
            {q ? t("Try a different search.") : t("Import your existing catalogue or create your first product.")}
          </Empty>
        }
      />
      {data ? (
        <Pager
          total={data.total}
          limit={limit}
          offset={offset}
          onChange={setOffset}
          onLimit={(l) => (setLimit(l), setOffset(0))}
        />
      ) : null}
      {sel.size > 0 ? (
        <div className="batch-bar">
          <strong>{sel.size} selected</strong>
          <span className="grow" />
          {status !== "archived" ? (
            <Button size="sm" icon={<Archive size={14} />} onClick={() => setConfirm("archive")}>
              {t("Archive")}
            </Button>
          ) : null}
          {status !== "active" ? (
            <Button size="sm" icon={<RotateCcw size={14} />} onClick={() => setConfirm("restore")}>
              {t("Restore")}
            </Button>
          ) : null}
          <Button size="sm" onClick={() => setSel(new Set())}>
            {t("Clear selection")}
          </Button>
        </div>
      ) : null}
      {confirm ? (
        <Confirm
          title={confirm === "archive" ? t("Archive products") : t("Restore products")}
          confirmLabel={confirm === "archive" ? t("Archive {0}", sel.size) : t("Restore {0}", sel.size)}
          danger={confirm === "archive"}
          busy={act.busy}
          error={act.error}
          onCancel={() => setConfirm(null)}
          onConfirm={() => void bulk(confirm === "restore")}
        >
          {confirm === "archive"
            ? t(
                "Archive {0} selected product(s)? They will no longer appear in POS search. Sales history is kept.",
                sel.size,
              )
            : t("Restore {0} product(s)? They will be sellable again.", sel.size)}
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
      const tv = taxes.data.find((x) => x.active && x.rate_bp > 0) ?? taxes.data.find((x) => x.active);
      setForm(emptyInput(tv?.tax_rule_id ?? ""));
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
      act.setError(t("Reorder point must be a number."));
      return;
    }
    const input = {
      ...form,
      reorder_point_milli: rp,
      sku: form.sku || null,
      name_ar: form.name_ar || null,
      description: form.description || null,
    };
    if (isNew) {
      const p = parseMoney(price);
      if (p === null || p < 0) {
        act.setError(t("Enter a valid selling price."));
        return;
      }
      const c = cost.trim() ? parseMoney(cost) : null;
      if (cost.trim() && (c === null || c < 0)) {
        act.setError(t("Enter a valid cost."));
        return;
      }
      const o = opening.trim() ? parseQty(opening) : null;
      const created = await act.run(() =>
        api.products.create({
          ...input,
          price_minor: p,
          cost_minor: has("products.view_cost") ? c : null,
          barcodes: bcInput.trim() ? [...barcodes, bcInput.trim()] : barcodes,
          opening_stock_milli: o,
        }),
      );
      if (created) {
        toast("success", t("Product created"));
        setDirty(false);
        nav(`/admin/products/${created.product_id}`, { replace: true });
      }
    } else if (detail) {
      const updated = await act.run(() =>
        api.products.update({ ...input, product_id: detail.product_id, expected_version: detail.version }),
      );
      if (updated) {
        toast("success", t("Product saved"));
        await loadDetail();
      }
    }
  };

  if (!form) return <Skeleton rows={10} />;
  const tabs = isNew
    ? [{ key: "general" as const, label: t("General") }]
    : [
        { key: "general" as const, label: t("General") },
        { key: "barcodes" as const, label: t("Barcodes") },
        { key: "pricing" as const, label: t("Pricing") },
        { key: "inventory" as const, label: t("Inventory") },
        { key: "history" as const, label: t("History") },
      ];
  return (
    <div>
      <div className="page-header">
        <Button
          variant="ghost"
          icon={<ArrowLeft size={18} />}
          aria-label={t("Back")}
          onClick={() => (dirty ? setLeave(true) : nav("/admin/products"))}
        />
        <div className="grow">
          <div className="tiny">{t("Products")}</div>
          <h1>
            {isNew ? t("New product") : detail?.name} {detail && !detail.active ? <Chip>{t("Archived")}</Chip> : null}
          </h1>
        </div>
        {!isNew && detail && canEdit ? (
          <Button
            icon={detail.active ? <Archive size={16} /> : <RotateCcw size={16} />}
            onClick={async () => {
              const d = await act.run(() => api.products.setActive(detail.product_id, !detail.active));
              if (d) {
                toast("success", d.active ? t("Product restored") : t("Product archived"));
                await loadDetail();
              }
            }}
          >
            {detail.active ? t("Archive") : t("Restore")}
          </Button>
        ) : null}
        {canEdit && (tab === "general" || isNew) ? (
          <Button variant="primary" onClick={save} loading={act.busy} disabled={!form.name.trim()}>
            {t("Save")}
          </Button>
        ) : null}
      </div>
      <Tabs tabs={tabs} value={tab} onChange={setTab} />
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {tab === "general" ? (
        <div className="grid-3">
          <div className="card card-pad">
            <div className="form-grid">
              <TextInput
                label={t("Name")}
                required
                value={form.name}
                onChange={(e) => set("name", e.target.value)}
                fieldClass="span-2"
                disabled={!canEdit}
                autoFocus={isNew}
              />
              <TextInput
                label={t("Arabic name")}
                dir="rtl"
                value={form.name_ar ?? ""}
                onChange={(e) => set("name_ar", e.target.value)}
                disabled={!canEdit}
              />
              <TextInput
                label={t("SKU")}
                value={form.sku ?? ""}
                onChange={(e) => set("sku", e.target.value)}
                hint={isNew ? t("Leave empty to generate.") : undefined}
                disabled={!canEdit}
              />
              <Field label={t("Category")}>
                <select
                  className="select"
                  value={form.category_id ?? ""}
                  onChange={(e) => set("category_id", e.target.value || null)}
                  disabled={!canEdit}
                >
                  <option value="">{t("Uncategorised")}</option>
                  {(cats.data ?? []).map((c) => (
                    <option key={c.category_id} value={c.category_id}>
                      {c.name}
                    </option>
                  ))}
                </select>
              </Field>
              <Field label={t("Tax rule")} required>
                <select
                  className="select"
                  value={form.tax_rule_id}
                  onChange={(e) => set("tax_rule_id", e.target.value)}
                  disabled={!canEdit}
                >
                  {(taxes.data ?? [])
                    .filter((tv) => tv.active || tv.tax_rule_id === form.tax_rule_id)
                    .map((tv) => (
                      <option key={tv.tax_rule_id} value={tv.tax_rule_id}>
                        {tv.name} ({formatPercent(tv.rate_bp)} {tv.inclusive ? t("incl.") : t("excl.")})
                      </option>
                    ))}
                </select>
              </Field>
              <Field label={t("Description")} className="span-2">
                <textarea
                  className="textarea"
                  value={form.description ?? ""}
                  onChange={(e) => set("description", e.target.value)}
                  disabled={!canEdit}
                />
              </Field>
              <TextInput
                label={t("Unit")}
                value={form.unit}
                onChange={(e) => set("unit", e.target.value)}
                hint={t("pcs, kg, box…")}
                disabled={!canEdit}
              />
              <TextInput
                label={t("Reorder point")}
                className="num"
                value={reorder}
                onChange={(e) => (setReorder(e.target.value), setDirty(true))}
                disabled={!canEdit}
              />
              <div className="col span-2">
                <Checkbox
                  label={t("Track stock")}
                  checked={form.track_inventory}
                  onChange={(v) => set("track_inventory", v)}
                  disabled={!canEdit}
                />
                <Checkbox
                  label={t("Sold by weight / decimal quantity")}
                  checked={form.allow_decimal_quantity}
                  onChange={(v) => set("allow_decimal_quantity", v)}
                  disabled={!canEdit}
                />
                <Checkbox
                  label={t("Show in POS favorites")}
                  checked={form.is_favorite}
                  onChange={(v) => set("is_favorite", v)}
                  disabled={!canEdit}
                />
              </div>
            </div>
          </div>
          <div className="col gap-16">
            {isNew ? (
              <div className="card card-pad col gap-16">
                <h3>{t("Price & stock")}</h3>
                <TextInput
                  label={t("Selling price")}
                  required
                  className="num"
                  inputMode="decimal"
                  value={price}
                  onChange={(e) => setPrice(e.target.value)}
                  placeholder={formatAmount(0)}
                />
                {has("products.view_cost") ? (
                  <TextInput
                    label={t("Cost")}
                    className="num"
                    inputMode="decimal"
                    value={cost}
                    onChange={(e) => setCost(e.target.value)}
                    placeholder={formatAmount(0)}
                  />
                ) : null}
                <TextInput
                  label={t("Opening stock")}
                  className="num"
                  inputMode="decimal"
                  value={opening}
                  onChange={(e) => setOpening(e.target.value)}
                  disabled={!form.track_inventory}
                />
                <Field
                  label={t("Barcodes")}
                  hint={t("Press Enter after each barcode. Barcodes are stored as text; leading zeros are kept.")}
                >
                  <div className="row wrap">
                    {barcodes.map((b) => (
                      <span key={b} className="chip brand mono">
                        {b}
                        <button
                          className="link"
                          aria-label={t("Remove {0}", b)}
                          onClick={() => setBarcodes(barcodes.filter((x) => x !== b))}
                        >
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
                    placeholder={t("Scan or type barcode")}
                  />
                </Field>
              </div>
            ) : detail ? (
              <div className="card card-pad">
                <dl className="kv">
                  <dt>{t("Selling price")}</dt>
                  <dd>
                    <Money minor={detail.price_minor} />
                  </dd>
                  {detail.avg_cost_minor !== null ? (
                    <>
                      <dt>{t("Average cost")}</dt>
                      <dd>
                        <Money minor={detail.avg_cost_minor} />
                      </dd>
                    </>
                  ) : null}
                  <dt>{t("Stock")}</dt>
                  <dd>
                    {detail.track_inventory ? `${formatQty(detail.stock_milli)} ${detail.unit}` : t("Not tracked")}
                  </dd>
                  <dt>{t("Barcodes")}</dt>
                  <dd>{detail.barcodes.length}</dd>
                  <dt>{t("Updated")}</dt>
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
        <Confirm
          title={t("Discard unsaved changes?")}
          confirmLabel={t("Discard")}
          danger
          onCancel={() => setLeave(false)}
          onConfirm={() => nav("/admin/products")}
        >
          {t("You have unsaved changes to this product.")}
        </Confirm>
      ) : null}
    </div>
  );
}

function BarcodesTab({
  detail,
  onChanged,
  canEdit,
}: {
  detail: ProductDetail;
  onChanged: (d: ProductDetail) => void;
  canEdit: boolean;
}) {
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
        <h3 className="grow">{t("Barcodes")}</h3>
      </div>
      <table className="table">
        <thead>
          <tr>
            <th>{t("Barcode")}</th>
            <th>{t("Source")}</th>
            <th>{t("Added")}</th>
            <th>{t("Primary")}</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {detail.barcodes.map((b) => (
            <tr key={b.barcode_id}>
              <td className="mono">{b.barcode}</td>
              <td>{codeLabel(b.source)}</td>
              <td>{formatShort(b.created_at)}</td>
              <td>{b.is_primary ? <Chip tone="brand">{t("Primary")}</Chip> : null}</td>
              <td className="num">
                {canEdit ? (
                  <div className="row" style={{ justifyContent: "flex-end" }}>
                    {!b.is_primary ? (
                      <Button
                        size="sm"
                        onClick={async () =>
                          onChanged((await act.run(() => api.barcodes.setPrimary(b.barcode_id))) ?? detail)
                        }
                      >
                        {t("Set Primary")}
                      </Button>
                    ) : null}
                    <Button
                      size="sm"
                      variant="danger-outline"
                      icon={<Trash2 size={14} />}
                      onClick={async () =>
                        onChanged((await act.run(() => api.barcodes.remove(b.barcode_id))) ?? detail)
                      }
                    >
                      {t("Remove")}
                    </Button>
                  </div>
                ) : null}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {detail.barcodes.length === 0 ? (
        <div className="empty">{t("No barcodes. Add one so the product can be scanned.")}</div>
      ) : null}
      {canEdit ? (
        <div className="card-body col">
          <div className="row">
            <input
              className="input mono grow"
              placeholder={t("Scan or type a barcode")}
              value={v}
              onChange={(e) => setV(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && v.trim() && add()}
              aria-label={t("New barcode")}
            />
            <Button variant="primary" icon={<Plus size={16} />} onClick={add} disabled={!v.trim()} loading={act.busy}>
              {t("Add Barcode")}
            </Button>
          </div>
          {act.error ? (
            <Banner
              tone="danger"
              action={
                owner ? (
                  <button className="link" onClick={() => nav(`/admin/products/${owner.id}`)}>
                    {t("Open {0}", owner.name)}
                  </button>
                ) : null
              }
            >
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
  const margin =
    detail.price_minor && detail.avg_cost_minor !== null ? detail.price_minor - detail.avg_cost_minor : null;
  return (
    <div className="grid-2">
      <div className="card card-pad col gap-16">
        <h3>{t("Selling price")}</h3>
        <div className="due">{formatMoney(detail.price_minor)}</div>
        {has("prices.manage") ? (
          <>
            <div className="form-grid">
              <TextInput
                label={t("New price")}
                className="num"
                inputMode="decimal"
                value={price}
                onChange={(e) => setPrice(e.target.value)}
              />
              <TextInput label={t("Reason")} value={reason} onChange={(e) => setReason(e.target.value)} />
            </div>
            <Button
              variant="primary"
              disabled={parseMoney(price) === null}
              loading={act.busy}
              onClick={async () => {
                const r = await act.run(() =>
                  api.products.priceUpdate(detail.product_id, parseMoney(price)!, reason || null),
                );
                if (r) {
                  toast("success", t("Price changed"));
                  setReason("");
                  await onChanged();
                }
              }}
            >
              {t("Change Price")}
            </Button>
          </>
        ) : null}
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <table className="table">
          <thead>
            <tr>
              <th>{t("Effective")}</th>
              <th>{t("Until")}</th>
              <th className="num">{t("Price")}</th>
              <th>{t("Changed by")}</th>
              <th>{t("Reason")}</th>
            </tr>
          </thead>
          <tbody>
            {detail.price_history.map((p) => (
              <tr key={p.price_id}>
                <td>{formatShort(p.effective_from)}</td>
                <td>{p.effective_to ? formatShort(p.effective_to) : <Chip tone="success">{t("Current")}</Chip>}</td>
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
          <h3>{t("Cost")}</h3>
          <dl className="kv">
            <dt>{t("Average cost")}</dt>
            <dd>{formatMoney(detail.avg_cost_minor)}</dd>
            <dt>{t("Last cost")}</dt>
            <dd>{formatMoney(detail.last_cost_minor)}</dd>
            <dt>{t("Unit margin")}</dt>
            <dd className={margin !== null && margin < 0 ? "neg-num" : ""}>
              {margin === null ? "—" : formatMoney(margin)}
            </dd>
          </dl>
          {has("products.manage") ? (
            <div className="row" style={{ alignItems: "flex-end" }}>
              <TextInput
                label={t("Set standard cost")}
                className="num"
                value={cost}
                onChange={(e) => setCost(e.target.value)}
                fieldClass="grow"
                hint={t("Overrides the weighted average. Receiving updates it automatically.")}
              />
              <Button
                disabled={parseMoney(cost) === null}
                onClick={async () => {
                  const r = await act.run(() =>
                    api.products.costUpdate(detail.product_id, parseMoney(cost)!, "Manual"),
                  );
                  if (r) {
                    toast("success", t("Cost updated"));
                    await onChanged();
                  }
                }}
              >
                {t("Save cost")}
              </Button>
            </div>
          ) : null}
          <table className="table">
            <thead>
              <tr>
                <th>{t("Date")}</th>
                <th>{t("Source")}</th>
                <th>{t("Supplier")}</th>
                <th className="num">{t("Cost")}</th>
              </tr>
            </thead>
            <tbody>
              {detail.cost_history.map((c) => (
                <tr key={c.cost_id}>
                  <td>{formatShort(c.effective_at)}</td>
                  <td>{codeLabel(c.source)}</td>
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
  const moves = useLoad(
    () => api.inventory.movements({ product_id: detail.product_id, limit: 100 }),
    [detail.product_id, detail.stock_milli],
  );
  return (
    <div className="col gap-16">
      <div className="card card-pad row">
        <div className="grow">
          <div className="tiny">{t("On hand")}</div>
          <div className="due">
            {detail.track_inventory ? formatQty(detail.stock_milli) : t("Not tracked")}{" "}
            <span className="small muted">{detail.unit}</span>
          </div>
          <div className="small muted">{t("Reorder point {0}", formatQty(detail.reorder_point_milli))}</div>
        </div>
        <StockStatus status={detail.stock_status} />
        {has("inventory.adjust") && detail.track_inventory ? (
          <Button variant="primary" onClick={() => setAdjust(true)}>
            {t("Adjust stock")}
          </Button>
        ) : null}
      </div>
      <DataTable
        rows={moves.data?.rows ?? null}
        loading={moves.loading}
        rowKey={(r) => r.movement_id}
        empty={<div className="empty">{t("No stock movements yet.")}</div>}
        columns={[
          { key: "t", label: t("Time"), render: (r) => formatShort(r.created_at) },
          { key: "k", label: t("Type"), render: (r) => r.kind },
          {
            key: "q",
            label: t("Qty Change"),
            num: true,
            render: (r) => (
              <span className={r.qty_delta_milli > 0 ? "pos-num" : "neg-num"}>
                {r.qty_delta_milli > 0 ? "+" : ""}
                {formatQty(r.qty_delta_milli)}
              </span>
            ),
          },
          { key: "b", label: t("Balance"), num: true, render: (r) => formatQty(r.balance_after_milli) },
          { key: "s", label: t("Source"), render: (r) => r.source_ref ?? r.source_type },
          { key: "r", label: t("Reason"), render: (r) => r.reason ?? "—" },
          { key: "u", label: t("User"), render: (r) => r.user_name ?? "—" },
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
  const audit = useLoad(
    () =>
      has("audit.view")
        ? api.audit.list({ entity_type: "product", entity_id: productId, limit: 100 })
        : Promise.resolve(null),
    [productId],
  );
  if (!has("audit.view")) return <div className="empty">{t("Your role cannot view the audit history.")}</div>;
  return (
    <DataTable
      rows={audit.data?.rows ?? null}
      loading={audit.loading}
      rowKey={(r) => r.audit_id}
      empty={<div className="empty">{t("No history.")}</div>}
      columns={[
        { key: "t", label: t("Time"), render: (r) => formatDateTime(r.created_at) },
        { key: "e", label: t("Event"), render: (r) => r.event_type },
        { key: "u", label: t("User"), render: (r) => r.user_name ?? t("System") },
        { key: "a", label: t("Approved by"), render: (r) => r.approver_name ?? "—" },
        {
          key: "d",
          label: t("Change"),
          render: (r) => (
            <span className="small mono ellipsis" style={{ maxWidth: 420, display: "inline-block" }}>
              {JSON.stringify(r.after ?? r.before)}
            </span>
          ),
        },
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
    setParent(c === "new" ? "" : (c.parent_id ?? ""));
  };
  const byId = new Map((data ?? []).map((c) => [c.category_id, c]));
  return (
    <div>
      <PageHeader
        title={t("Categories")}
        actions={
          canEdit ? (
            <Button variant="primary" icon={<Plus size={16} />} onClick={() => open("new")}>
              {t("Add Category")}
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
          {
            key: "n",
            label: t("Name"),
            render: (r) => (r.parent_id ? `${byId.get(r.parent_id)?.name ?? "…"} › ${r.name}` : r.name),
            sort: (r) => r.name,
          },
          {
            key: "p",
            label: t("Active products"),
            num: true,
            render: (r) => r.product_count,
            sort: (r) => r.product_count,
          },
          {
            key: "s",
            label: t("Status"),
            render: (r) => (r.active ? <Chip tone="success">{t("Active")}</Chip> : <Chip>{t("Archived")}</Chip>),
          },
          {
            key: "a",
            label: "",
            num: true,
            render: (r) =>
              canEdit && r.active ? (
                <Button size="sm" onClick={(e) => (e.stopPropagation(), setArchive(r), setTarget(""))}>
                  {t("Archive")}
                </Button>
              ) : null,
          },
        ]}
      />
      {editing ? (
        <Drawer
          title={editing === "new" ? t("New category") : t("Edit {0}", editing.name)}
          onClose={() => setEditing(null)}
        >
          <div className="col gap-16">
            <TextInput label={t("Name")} required value={name} onChange={(e) => setName(e.target.value)} autoFocus />
            <Field label={t("Parent category")}>
              <select className="select" value={parent} onChange={(e) => setParent(e.target.value)}>
                <option value="">{t("None (top level)")}</option>
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
                const r = await act.run(() =>
                  api.categories.save({
                    category_id: editing === "new" ? null : editing.category_id,
                    name,
                    parent_id: parent || null,
                  }),
                );
                if (r) {
                  toast("success", t("Category saved"));
                  setEditing(null);
                  void reload();
                }
              }}
            >
              {t("Save")}
            </Button>
          </div>
        </Drawer>
      ) : null}
      {archive ? (
        <Confirm
          title={t("Archive {0}", archive.name)}
          confirmLabel={t("Archive")}
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
              <span>{t("{0} product(s) use this category. Move them to:", archive.product_count)}</span>
              <select
                className="select"
                value={target}
                onChange={(e) => setTarget(e.target.value)}
                aria-label={t("Move products to")}
              >
                <option value="">{t("Choose category…")}</option>
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
            t("The category will be hidden from pickers. Historical sales keep their category.")
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
  const { data, loading, reload } = useLoad(
    () => api.products.search({ q: q || undefined, category_id: cat || undefined, limit: 500 }),
    [q, cat],
  );
  const computeNew = (p: ProductRow): number | null => {
    if (p.price_minor === null) return null;
    const base = p.price_minor;
    if (rule.kind === "percent") {
      const bp = parsePercent(rule.value);
      if (bp === null) return null;
      return Math.max(0, base + mulDivRound(base, bp, 10000));
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
    const r = await act.run(() =>
      api.products.bulkPrice(
        changes.map((c) => ({ product_id: c.p.product_id, amount_minor: c.next! })),
        reason || t("Bulk price change"),
        newOperationId(),
      ),
    );
    if (r) {
      toast("success", t("{0} price(s) changed", r.changed));
      setConfirm(false);
      setSel(new Set());
      void reload();
    }
  };
  if (!has("prices.manage")) return null;
  return (
    <div>
      <PageHeader
        title={t("Pricing")}
        subtitle={t("Bulk price changes. Every change is previewed first and recorded in price history.")}
      />
      <div className="filters">
        <input
          className="input"
          style={{ width: 240 }}
          placeholder={t("Search products…")}
          value={q}
          onChange={(e) => setQ(e.target.value)}
          aria-label={t("Search")}
        />
        <select
          className="select"
          style={{ width: 180 }}
          value={cat}
          onChange={(e) => setCat(e.target.value)}
          aria-label={t("Category")}
        >
          <option value="">{t("All categories")}</option>
          {(cats.data ?? []).map((c) => (
            <option key={c.category_id} value={c.category_id}>
              {c.name}
            </option>
          ))}
        </select>
        <span className="grow" />
        <select
          className="select"
          style={{ width: 170 }}
          value={rule.kind}
          onChange={(e) =>
            setRule({ kind: e.target.value as Rule["kind"], value: e.target.value === "round" ? "0.050" : rule.value })
          }
          aria-label={t("Rule")}
        >
          <option value="percent">{t("Percentage change")}</option>
          <option value="fixed">{t("Fixed change")}</option>
          <option value="set">{t("Set price")}</option>
          <option value="round">{t("Round up to")}</option>
        </select>
        <input
          className="input num"
          style={{ width: 110 }}
          value={rule.value}
          onChange={(e) => setRule({ ...rule, value: e.target.value })}
          aria-label={t("Rule value")}
        />
        <Button variant="primary" disabled={changes.length === 0} onClick={() => setConfirm(true)}>
          {t("Preview {0} change(s)", changes.length)}
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
          { key: "n", label: t("Product"), render: (r) => r.name, sort: (r) => r.name },
          { key: "p", label: t("Current Price"), num: true, render: (r) => <Money minor={r.price_minor} /> },
          ...(has("products.view_cost")
            ? [
                { key: "c", label: t("Cost"), num: true, render: (r: ProductRow) => <Money minor={r.cost_minor} /> },
                {
                  key: "m",
                  label: t("Margin"),
                  num: true,
                  render: (r: ProductRow) =>
                    r.price_minor && r.cost_minor !== null
                      ? formatPercent(Math.round(((r.price_minor - r.cost_minor) * 10000) / r.price_minor))
                      : "—",
                },
              ]
            : []),
          {
            key: "x",
            label: t("New Price"),
            num: true,
            render: (r) =>
              sel.has(r.product_id) ? <strong>{formatMoney(computeNew(r))}</strong> : <span className="muted">—</span>,
          },
        ]}
      />
      {confirm ? (
        <Confirm
          title={t("Apply price changes")}
          confirmLabel={t("Change {0} price(s)", changes.length)}
          busy={act.busy}
          error={act.error}
          onCancel={() => setConfirm(false)}
          onConfirm={apply}
        >
          <div className="col gap-16">
            <div>
              {t(
                "Change the selling price of {0} product(s). New prices apply immediately at every till.",
                changes.length,
              )}
            </div>
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
            <TextInput label={t("Reason")} value={reason} onChange={(e) => setReason(e.target.value)} />
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
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [merging, setMerging] = useState(false);
  const [q, setQ] = useState("");
  const results = useLoad(() => (q.trim() ? api.products.search({ q, limit: 10 }) : Promise.resolve(null)), [q]);
  const act = useAction();
  return (
    <div>
      <PageHeader
        title={t("Unknown Barcodes")}
        subtitle={t("Barcodes scanned at the till that are not in the catalogue.")}
      />
      <div className="filters">
        {["open", "resolved", "dismissed", "all"].map((s) => (
          <button key={s} className={`filter-chip ${status === s ? "active" : ""}`} onClick={() => setStatus(s)}>
            {s === "open"
              ? t("Open")
              : s === "resolved"
                ? t("Resolved")
                : s === "dismissed"
                  ? t("Dismissed")
                  : t("All")}
          </button>
        ))}
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {sel.size > 0 ? (
        <div className="bulk-bar row" style={{ marginBottom: 12 }}>
          <strong>{t("{0} selected", sel.size)}</strong>
          <Button variant="primary" onClick={() => (setMerging(true), setQ(""))}>
            {t("Assign selected to one product")}
          </Button>
          <Button variant="ghost" onClick={() => setSel(new Set())}>
            {t("Clear selection")}
          </Button>
        </div>
      ) : null}
      <DataTable<UnknownBarcodeRow>
        rows={data}
        loading={loading}
        rowKey={(r) => r.barcode}
        selectable={status === "open"}
        selected={sel}
        onSelect={setSel}
        onRowClick={(r) => (r.status === "open" || r.status === "dismissed") && (setOpen(r), setQ(""))}
        empty={<div className="empty">{t("No unknown barcodes. Every scanned barcode was recognised.")}</div>}
        columns={[
          { key: "b", label: t("Barcode"), render: (r) => <span className="mono">{r.barcode}</span> },
          {
            key: "f",
            label: t("First Seen"),
            render: (r) => formatShort(r.first_seen_at),
            sort: (r) => r.first_seen_at,
          },
          { key: "l", label: t("Last Seen"), render: (r) => formatShort(r.last_seen_at), sort: (r) => r.last_seen_at },
          { key: "c", label: t("Scan Count"), num: true, render: (r) => r.scan_count, sort: (r) => r.scan_count },
          { key: "t", label: t("Terminal"), render: (r) => r.last_device_name ?? "—" },
          {
            key: "s",
            label: t("Status"),
            render: (r) =>
              r.status === "open" ? (
                <Chip tone="warning">{t("Open")}</Chip>
              ) : r.status === "resolved" ? (
                <Chip tone="success">{t("Resolved · {0}", r.resolved_product_name)}</Chip>
              ) : (
                <Chip>{t("Dismissed")}</Chip>
              ),
          },
        ]}
      />
      {open ? (
        <Drawer title={t("Resolve {0}", open.barcode)} onClose={() => setOpen(null)}>
          <div className="col gap-16">
            <div className="small muted">
              {t("Scanned {0} time(s), last on {1}.", open.scan_count, formatDateTime(open.last_seen_at))}
            </div>
            {open.status === "dismissed" ? (
              <Button
                onClick={async () => {
                  const r = await act.run(() => api.barcodes.reopen(open.barcode));
                  if (r !== undefined) {
                    toast("success", t("Barcode reopened"));
                    setOpen(null);
                    void reload();
                  }
                }}
              >
                {t("Reopen")}
              </Button>
            ) : null}
            <h3>{t("Assign to an existing product")}</h3>
            <input
              className="input"
              placeholder={t("Search product by name or SKU…")}
              value={q}
              onChange={(e) => setQ(e.target.value)}
              autoFocus
            />
            {(results.data?.rows ?? []).map((p) => (
              <div key={p.product_id} className="result-row">
                <div className="grow">
                  <div style={{ fontWeight: 600 }}>{p.name}</div>
                  <div className="tiny">
                    {p.sku} · {p.primary_barcode ?? t("no barcode")}
                  </div>
                </div>
                <Button
                  size="sm"
                  variant="primary"
                  onClick={async () => {
                    const r = await act.run<unknown>(() =>
                      open.status === "dismissed"
                        ? api.barcodes.merge(p.product_id, [open.barcode])
                        : api.barcodes.add(p.product_id, open.barcode, false),
                    );
                    if (r) {
                      toast("success", t("Barcode assigned to {0}", p.name));
                      setOpen(null);
                      void reload();
                    }
                  }}
                >
                  {t("Assign")}
                </Button>
              </div>
            ))}
            <div className="divider" />
            <div className="row">
              <Button
                variant="primary"
                onClick={() => nav(`/admin/products/new?barcode=${encodeURIComponent(open.barcode)}`)}
              >
                {t("Create product")}
              </Button>
              <Button
                variant="danger-outline"
                className="right"
                disabled={open.status !== "open"}
                onClick={async () => {
                  const r = await act.run(() => api.barcodes.dismiss(open.barcode));
                  if (r !== undefined) {
                    setOpen(null);
                    void reload();
                  }
                }}
              >
                {t("Dismiss")}
              </Button>
            </div>
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          </div>
        </Drawer>
      ) : null}
      {merging ? (
        <Drawer title={t("Assign {0} barcode(s) to one product", sel.size)} onClose={() => setMerging(false)}>
          <div className="col gap-16">
            <div className="mono small">{[...sel].join(" · ")}</div>
            <input
              className="input"
              placeholder={t("Search product by name or SKU…")}
              value={q}
              onChange={(e) => setQ(e.target.value)}
              autoFocus
            />
            {(results.data?.rows ?? []).map((p) => (
              <div key={p.product_id} className="result-row">
                <div className="grow">
                  <div style={{ fontWeight: 600 }}>{p.name}</div>
                  <div className="tiny">{p.sku}</div>
                </div>
                <Button
                  size="sm"
                  variant="primary"
                  onClick={async () => {
                    const r = await act.run(() => api.barcodes.merge(p.product_id, [...sel]));
                    if (r) {
                      toast("success", t("{0} barcode(s) assigned to {1}", r.resolved, r.product_name));
                      setMerging(false);
                      setSel(new Set());
                      void reload();
                    }
                  }}
                >
                  {t("Assign")}
                </Button>
              </div>
            ))}
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          </div>
        </Drawer>
      ) : null}
    </div>
  );
}

export type { TaxRuleRow };
