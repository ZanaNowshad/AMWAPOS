import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { ArrowLeft, CheckCircle2, ClipboardCheck, Plus, ScanBarcode, Trash2 } from "lucide-react";
import { api } from "../../api";
import type { MovementRow, PosSearchRow, ProductRow, StocktakeRow } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { newOperationId } from "../../lib/ids";
import { formatAmount, formatMoney, formatQty, parseMoney, parseQty, mulDivRound } from "../../lib/money";
import { formatShort, todayLocal } from "../../lib/time";
import {
  Banner,
  Button,
  Checkbox,
  Chip,
  Field,
  Modal,
  Money,
  PageHeader,
  Skeleton,
  StockStatus,
  TextInput,
} from "../../components/ui";
import { Confirm, DataTable, DateRange, Pager, useAction, useLoad } from "./common";
import { t } from "../../i18n";
import { codeLabel } from "../../i18n/codes";

export function AdjustDialog({
  product,
  onClose,
  onDone,
}: {
  product: Pick<ProductRow, "product_id" | "name" | "stock_milli" | "allow_decimal_quantity" | "unit">;
  onClose: () => void;
  onDone: () => void;
}) {
  const toast = useToast();
  const [mode, setMode] = useState<"increase" | "decrease" | "set">("decrease");
  const [qty, setQty] = useState("");
  const [reason, setReason] = useState("");
  const [opId] = useState(newOperationId);
  const act = useAction();
  const q = parseQty(qty);
  const after =
    q === null
      ? null
      : mode === "increase"
        ? product.stock_milli + q
        : mode === "decrease"
          ? product.stock_milli - q
          : q;
  const valid =
    q !== null &&
    q >= 0 &&
    (mode === "set" || q > 0) &&
    reason.trim() &&
    (product.allow_decimal_quantity || q % 1000 === 0);
  return (
    <Modal
      title={t("Adjust stock — {0}", product.name)}
      size="sm"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button
            variant="primary"
            className="right"
            disabled={!valid}
            loading={act.busy}
            onClick={async () => {
              const r = await act.run(() =>
                api.inventory.adjust({
                  product_id: product.product_id,
                  mode,
                  qty_milli: q!,
                  reason,
                  operation_id: opId,
                }),
              );
              if (r) {
                toast("success", t("Stock adjusted to {0}", formatQty(r.after_milli)));
                onDone();
              }
            }}
          >
            {t("Apply Adjustment")}
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        <div className="row">
          {(["increase", "decrease", "set"] as const).map((m) => (
            <button key={m} className={`filter-chip ${mode === m ? "active" : ""}`} onClick={() => setMode(m)}>
              {m === "set" ? t("Set count") : m === "increase" ? t("Increase") : t("Decrease")}
            </button>
          ))}
        </div>
        <TextInput
          label={mode === "set" ? t("Counted quantity") : t("Quantity")}
          className="num"
          inputMode="decimal"
          value={qty}
          onChange={(e) => setQty(e.target.value)}
          autoFocus
        />
        <Field label={t("Reason")} required>
          <select className="select" value={reason} onChange={(e) => setReason(e.target.value)}>
            <option value="">{t("Select reason…")}</option>
            {[
              t("Damaged"),
              t("Expired"),
              t("Theft / loss"),
              t("Count correction"),
              t("Supplier return"),
              t("Internal use"),
              t("Found stock"),
              t("Other"),
            ].map((r) => (
              <option key={r}>{r}</option>
            ))}
          </select>
        </Field>
        <div className="banner">
          {t("Before:")} <strong>{formatQty(product.stock_milli)}</strong> {t("→ After:")}{" "}
          <strong>{after === null ? "—" : formatQty(after)}</strong> {product.unit}
        </div>
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      </div>
    </Modal>
  );
}

export function InventoryPage() {
  const { has } = useSession();
  const nav = useNavigate();
  const [q, setQ] = useState("");
  const [stock, setStock] = useState(
    () => new URLSearchParams(window.location.hash.split("?")[1] ?? "").get("stock") ?? "",
  );
  const [offset, setOffset] = useState(0);
  const [adjust, setAdjust] = useState<ProductRow | null>(null);
  const { data, loading, error, reload } = useLoad(
    () => api.products.search({ q: q || undefined, stock: stock || undefined, limit: 50, offset, sort: "name" }),
    [q, stock, offset],
  );
  const counts = useLoad(async () => {
    const [all, low, out, neg] = await Promise.all([
      api.products.search({ limit: 1 }),
      api.products.search({ stock: "low", limit: 1 }),
      api.products.search({ stock: "out", limit: 1 }),
      api.products.search({ stock: "negative", limit: 1 }),
    ]);
    return { all: all.total, low: low.total, out: out.total, neg: neg.total };
  }, []);
  return (
    <div>
      <PageHeader
        title={t("Inventory")}
        actions={
          <>
            {has("inventory.receive") ? <Button onClick={() => nav("/admin/receiving")}>{t("Receive")}</Button> : null}
            {has("stocktake.manage") ? (
              <Button variant="primary" icon={<ClipboardCheck size={16} />} onClick={() => nav("/admin/stocktake")}>
                {t("Stocktake")}
              </Button>
            ) : null}
          </>
        }
      />
      <div className="kpis" style={{ marginBottom: 16 }}>
        {[
          [t("Total SKUs"), counts.data?.all, ""],
          [t("Low Stock"), counts.data?.low, "low"],
          [t("Out of Stock"), counts.data?.out, "out"],
          [t("Negative Stock"), counts.data?.neg, "negative"],
        ].map(([label, v, key]) => (
          <button
            key={String(label)}
            className="card kpi"
            style={{ textAlign: "start", cursor: "pointer" }}
            onClick={() => (setStock(String(key)), setOffset(0))}
          >
            <div className="k-label">{label}</div>
            <div className={`k-value ${key === "negative" && Number(v) > 0 ? "neg-num" : ""}`}>{v ?? "…"}</div>
          </button>
        ))}
      </div>
      <div className="filters">
        <input
          className="input"
          style={{ width: 280 }}
          placeholder={t("Search name, SKU or barcode…")}
          value={q}
          onChange={(e) => (setQ(e.target.value), setOffset(0))}
          aria-label={t("Search")}
        />
        {[
          ["", t("All")],
          ["attention", t("Needs reorder")],
          ["low", t("Low")],
          ["out", t("Out")],
          ["negative", t("Negative")],
          ["in_stock", t("In stock")],
        ].map(([k, l]) => (
          <button
            key={k}
            className={`filter-chip ${stock === k ? "active" : ""}`}
            onClick={() => (setStock(k), setOffset(0))}
          >
            {l}
          </button>
        ))}
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<ProductRow>
        rows={data?.rows ?? null}
        loading={loading}
        rowKey={(r) => r.product_id}
        onRowClick={(r) => nav(`/admin/products/${r.product_id}`)}
        columns={[
          { key: "p", label: t("Product"), render: (r) => r.name, sort: (r) => r.name },
          { key: "s", label: t("SKU"), render: (r) => <span className="mono">{r.sku}</span> },
          { key: "b", label: t("Barcode"), render: (r) => <span className="mono">{r.primary_barcode ?? "—"}</span> },
          {
            key: "q",
            label: t("Stock"),
            num: true,
            render: (r) =>
              r.track_inventory ? (
                <span className={r.stock_milli < 0 ? "neg-num" : ""}>{formatQty(r.stock_milli)}</span>
              ) : (
                "—"
              ),
            sort: (r) => r.stock_milli,
          },
          { key: "r", label: t("Reorder"), num: true, render: (r) => formatQty(r.reorder_point_milli) },
          ...(has("products.view_cost")
            ? [{ key: "c", label: t("Avg Cost"), num: true, render: (r: ProductRow) => <Money minor={r.cost_minor} /> }]
            : []),
          { key: "st", label: t("Status"), render: (r) => <StockStatus status={r.stock_status} /> },
          {
            key: "a",
            label: "",
            num: true,
            render: (r) =>
              has("inventory.adjust") && r.track_inventory ? (
                <Button size="sm" onClick={(e) => (e.stopPropagation(), setAdjust(r))}>
                  {t("Adjust")}
                </Button>
              ) : null,
          },
        ]}
      />
      {data ? <Pager total={data.total} limit={50} offset={offset} onChange={setOffset} /> : null}
      {adjust ? (
        <AdjustDialog
          product={adjust}
          onClose={() => setAdjust(null)}
          onDone={() => (setAdjust(null), void reload(), void counts.reload())}
        />
      ) : null}
    </div>
  );
}

export function MovementsPage() {
  const [from, setFrom] = useState(todayLocal(-6));
  const [to, setTo] = useState(todayLocal());
  const [kind, setKind] = useState("");
  const [offset, setOffset] = useState(0);
  const { data, loading, error } = useLoad(
    () => api.inventory.movements({ from, to, kind: kind || undefined, limit: 100, offset }),
    [from, to, kind, offset],
  );
  return (
    <div>
      <PageHeader
        title={t("Stock Movements")}
        subtitle={t("The append-only ledger behind every stock level. Movements cannot be edited or deleted.")}
      />
      <div className="filters">
        <DateRange from={from} to={to} onChange={(a, b) => (setFrom(a), setTo(b), setOffset(0))} />
        <select
          className="select"
          style={{ width: 160 }}
          value={kind}
          onChange={(e) => (setKind(e.target.value), setOffset(0))}
          aria-label={t("Movement type")}
        >
          <option value="">{t("All types")}</option>
          {["sale", "refund", "receive", "adjust", "stocktake", "opening"].map((k) => (
            <option key={k} value={k}>
              {k}
            </option>
          ))}
        </select>
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<MovementRow>
        rows={data?.rows ?? null}
        loading={loading}
        rowKey={(r) => r.movement_id}
        columns={[
          { key: "t", label: t("Time"), render: (r) => formatShort(r.created_at) },
          { key: "p", label: t("Product"), render: (r) => r.product_name },
          { key: "k", label: t("Type"), render: (r) => <Chip>{codeLabel(r.kind)}</Chip> },
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
          {
            key: "s",
            label: t("Source"),
            render: (r) => <span className="mono">{r.source_ref ?? codeLabel(r.source_type)}</span>,
          },
          { key: "r", label: t("Reason"), render: (r) => r.reason ?? "—" },
          { key: "u", label: t("User"), render: (r) => r.user_name ?? "—" },
        ]}
        empty={<div className="empty">{t("No movements in this period.")}</div>}
      />
      {data ? <Pager total={data.total} limit={100} offset={offset} onChange={setOffset} /> : null}
    </div>
  );
}

export function StocktakesPage() {
  const nav = useNavigate();
  const { data, loading, error, reload } = useLoad(() => api.stocktake.list(), []);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState(`Stocktake ${todayLocal()}`);
  const [scope, setScope] = useState("all");
  const [cat, setCat] = useState("");
  const [blind, setBlind] = useState(true);
  const cats = useLoad(() => api.categories.list(), []);
  const act = useAction();
  return (
    <div>
      <PageHeader
        title={t("Stocktake")}
        actions={
          <Button variant="primary" icon={<Plus size={16} />} onClick={() => setCreating(true)}>
            {t("New Stocktake")}
          </Button>
        }
      />
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<StocktakeRow>
        rows={data}
        loading={loading}
        rowKey={(r) => r.stocktake_id}
        onRowClick={(r) => nav(`/admin/stocktake/${r.stocktake_id}`)}
        empty={<div className="empty">{t("No stocktakes yet. Start one to count stock with a scanner.")}</div>}
        columns={[
          { key: "n", label: t("Stocktake"), render: (r) => `${r.stocktake_number} · ${r.name}` },
          { key: "s", label: t("Scope"), render: (r) => r.scope_type },
          { key: "c", label: t("Created"), render: (r) => formatShort(r.created_at) },
          {
            key: "st",
            label: t("Status"),
            render: (r) => (
              <Chip
                tone={
                  r.status === "completed"
                    ? "success"
                    : r.status === "cancelled"
                      ? "default"
                      : r.status === "review"
                        ? "warning"
                        : "info"
                }
              >
                {codeLabel(r.status)}
              </Chip>
            ),
          },
          { key: "e", label: t("Products"), num: true, render: (r) => r.line_count },
          { key: "cc", label: t("Counted"), num: true, render: (r) => r.counted_count },
          { key: "v", label: t("Variances"), num: true, render: (r) => r.variance_lines },
        ]}
      />
      {creating ? (
        <Modal
          title={t("New stocktake")}
          size="sm"
          onClose={() => setCreating(false)}
          footer={
            <>
              <Button onClick={() => setCreating(false)}>{t("Cancel")}</Button>
              <Button
                variant="primary"
                className="right"
                loading={act.busy}
                onClick={async () => {
                  const r = await act.run(() =>
                    api.stocktake.create({
                      name,
                      scope_type: scope,
                      category_id: scope === "category" ? cat : null,
                      blind,
                    }),
                  );
                  if (r) {
                    void reload();
                    nav(`/admin/stocktake/${r.stocktake_id}`);
                  }
                }}
              >
                {t("Create")}
              </Button>
            </>
          }
        >
          <div className="col gap-16">
            <TextInput label={t("Name")} value={name} onChange={(e) => setName(e.target.value)} />
            <Field label={t("Scope")}>
              <select className="select" value={scope} onChange={(e) => setScope(e.target.value)}>
                <option value="all">{t("All inventory")}</option>
                <option value="category">{t("Category")}</option>
              </select>
            </Field>
            {scope === "category" ? (
              <select
                className="select"
                value={cat}
                onChange={(e) => setCat(e.target.value)}
                aria-label={t("Category")}
              >
                <option value="">{t("Choose category…")}</option>
                {(cats.data ?? []).map((c) => (
                  <option key={c.category_id} value={c.category_id}>
                    {c.name}
                  </option>
                ))}
              </select>
            ) : null}
            <Checkbox
              label={t("Blind count (hide expected quantities while counting)")}
              checked={blind}
              onChange={setBlind}
            />
            <div className="tiny">
              {t(
                "Expected quantities are frozen now. Sales during counting are handled: each count is compared with the system quantity at the moment it is recorded.",
              )}
            </div>
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          </div>
        </Modal>
      ) : null}
    </div>
  );
}

export function StocktakeDetailPage() {
  const { id } = useParams();
  const nav = useNavigate();
  const toast = useToast();
  const { data, error, reload, setData } = useLoad(() => api.stocktake.get(id!), [id]);
  const [scan, setScan] = useState("");
  const [qty, setQty] = useState("1");
  const [mode, setMode] = useState<"add" | "set">("add");
  const [onlyDiff, setOnlyDiff] = useState(false);
  const [filter, setFilter] = useState("");
  const [finalize, setFinalize] = useState(false);
  const [last, setLast] = useState<string | null>(null);
  const [opId] = useState(newOperationId);
  const scanRef = useRef<HTMLInputElement>(null);
  const act = useAction();
  useEffect(() => scanRef.current?.focus(), [data?.status]);
  const lines = useMemo(() => {
    const l = data?.lines ?? [];
    return l.filter(
      (x) =>
        (!onlyDiff || (x.variance_milli ?? 0) !== 0) &&
        (!filter ||
          x.name.toLowerCase().includes(filter.toLowerCase()) ||
          x.sku.includes(filter) ||
          (x.primary_barcode ?? "").includes(filter)),
    );
  }, [data, onlyDiff, filter]);
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton rows={10} />;
  const record = async (productId: string | null, barcode: string | null, q: number, m: "add" | "set") => {
    const line = await act.run(() =>
      api.stocktake.count({
        stocktake_id: data.stocktake_id,
        product_id: productId ?? undefined,
        barcode: barcode ?? undefined,
        qty_milli: q,
        mode: m,
      }),
    );
    if (line) {
      setLast(`${line.name}: counted ${formatQty(line.counted_qty_milli ?? 0)}`);
      setData({
        ...data,
        counted_count:
          data.counted_count +
          (data.lines.find((x) => x.product_id === line.product_id)?.counted_qty_milli === null ? 1 : 0),
        lines: data.lines.map((x) => (x.product_id === line.product_id ? line : x)),
      });
    }
  };
  const counting = data.status === "counting";
  return (
    <div>
      <div className="page-header">
        <Button
          variant="ghost"
          icon={<ArrowLeft size={18} />}
          aria-label={t("Back")}
          onClick={() => nav("/admin/stocktake")}
        />
        <div className="grow">
          <div className="tiny">{t("Stocktake {0}", data.stocktake_number)}</div>
          <h1>{data.name}</h1>
        </div>
        <Chip tone={data.status === "completed" ? "success" : "info"}>{codeLabel(data.status)}</Chip>
        {counting ? (
          <Button
            onClick={async () => (
              await act.run(() => api.stocktake.setStatus(data.stocktake_id, "review")),
              void reload()
            )}
          >
            {t("Finish counting")}
          </Button>
        ) : null}
        {data.status === "review" ? (
          <>
            <Button
              onClick={async () => (
                await act.run(() => api.stocktake.setStatus(data.stocktake_id, "counting")),
                void reload()
              )}
            >
              {t("Back to counting")}
            </Button>
            <Button variant="primary" icon={<CheckCircle2 size={16} />} onClick={() => setFinalize(true)}>
              {t("Finalize")}
            </Button>
          </>
        ) : null}
        {counting || data.status === "review" ? (
          <Button
            variant="danger-outline"
            onClick={async () => (
              await act.run(() => api.stocktake.setStatus(data.stocktake_id, "cancelled")),
              void reload()
            )}
          >
            {t("Cancel stocktake")}
          </Button>
        ) : null}
      </div>
      <div className="kpis" style={{ marginBottom: 16 }}>
        <div className="card kpi">
          <div className="k-label">{t("Progress")}</div>
          <div className="k-value">
            {data.counted_count} / {data.line_count}
          </div>
        </div>
        <div className="card kpi">
          <div className="k-label">{t("Lines with variance")}</div>
          <div className="k-value">{data.variance_lines}</div>
        </div>
        {data.expected_value_minor !== null ? (
          <>
            <div className="card kpi">
              <div className="k-label">{t("Expected value")}</div>
              <div className="k-value">{formatMoney(data.expected_value_minor)}</div>
            </div>
            <div className="card kpi">
              <div className="k-label">{t("Net variance value")}</div>
              <div className={`k-value ${(data.variance_value_minor ?? 0) < 0 ? "neg-num" : ""}`}>
                {formatMoney(data.variance_value_minor)}
              </div>
            </div>
          </>
        ) : null}
      </div>
      {counting ? (
        <div className="card card-pad row" style={{ marginBottom: 16 }}>
          <div className="scan-box grow">
            <ScanBarcode size={20} className="scan-icon" />
            <input
              ref={scanRef}
              className="input"
              placeholder={t("Scan barcode to count…")}
              value={scan}
              onChange={(e) => setScan(e.target.value)}
              onKeyDown={async (e) => {
                if (e.key === "Enter" && scan.trim()) {
                  const q = parseQty(qty) ?? 1000;
                  await record(null, scan.trim(), q, mode);
                  setScan("");
                }
              }}
              aria-label={t("Scan to count")}
            />
          </div>
          <select
            className="select"
            style={{ width: 150 }}
            value={mode}
            onChange={(e) => setMode(e.target.value as "add" | "set")}
            aria-label={t("Count mode")}
          >
            <option value="add">{t("Add to count")}</option>
            <option value="set">{t("Set count")}</option>
          </select>
          <input
            className="input num"
            style={{ width: 90 }}
            value={qty}
            onChange={(e) => setQty(e.target.value)}
            aria-label={t("Quantity per scan")}
          />
        </div>
      ) : null}
      {last ? <Banner tone="success">{last}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <div className="filters">
        <input
          className="input"
          style={{ width: 260 }}
          placeholder={t("Filter products…")}
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <Checkbox label={t("Only differences")} checked={onlyDiff} onChange={setOnlyDiff} />
      </div>
      <DataTable
        rows={lines}
        rowKey={(r) => r.product_id}
        maxHeight="55vh"
        columns={[
          { key: "n", label: t("Product"), render: (r) => r.name, sort: (r) => r.name },
          { key: "b", label: t("Barcode"), render: (r) => <span className="mono">{r.primary_barcode ?? r.sku}</span> },
          {
            key: "e",
            label: t("Expected"),
            num: true,
            render: (r) =>
              r.expected_qty_milli === null ? <span className="muted">hidden</span> : formatQty(r.expected_qty_milli),
          },
          {
            key: "c",
            label: t("Counted"),
            num: true,
            render: (r) =>
              counting ? (
                <input
                  className="input num"
                  style={{ width: 90 }}
                  defaultValue={r.counted_qty_milli === null ? "" : formatQty(r.counted_qty_milli)}
                  aria-label={t("Count for {0}", r.name)}
                  onBlur={async (e) => {
                    const v = e.target.value.trim();
                    const q = parseQty(v);
                    if (v !== "" && q !== null && q !== r.counted_qty_milli) await record(r.product_id, null, q, "set");
                  }}
                />
              ) : r.counted_qty_milli === null ? (
                <span className="muted">{t("not counted")}</span>
              ) : (
                formatQty(r.counted_qty_milli)
              ),
          },
          {
            key: "v",
            label: t("Variance"),
            num: true,
            render: (r) =>
              r.variance_milli === null ? (
                "—"
              ) : (
                <span className={r.variance_milli < 0 ? "neg-num" : r.variance_milli > 0 ? "pos-num" : ""}>
                  {r.variance_milli > 0 ? "+" : ""}
                  {formatQty(r.variance_milli)}
                </span>
              ),
            sort: (r) => r.variance_milli ?? 0,
          },
          { key: "t", label: t("Counted at"), render: (r) => formatShort(r.counted_at) },
        ]}
      />
      {finalize ? (
        <Confirm
          title={t("Finalize stocktake")}
          confirmLabel={t("Finalize and adjust stock")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setFinalize(false)}
          onConfirm={async () => {
            const r = await act.run(() => api.stocktake.finalize(data.stocktake_id, opId));
            if (r) {
              toast("success", t("Stocktake finalized: {0} product(s) adjusted", String(r.adjusted_lines)));
              setFinalize(false);
              void reload();
            }
          }}
        >
          {t("{0} counted product(s) will be adjusted to their counted quantities.", data.counted_count)}{" "}
          {t(
            "{0} uncounted product(s) will not change. This creates stock movements and cannot be undone.",
            data.line_count - data.counted_count,
          )}
        </Confirm>
      ) : null}
    </div>
  );
}

interface RecvLine {
  product: PosSearchRow;
  qty: string;
  cost: string;
}

export function ReceivingPage() {
  const nav = useNavigate();
  const toast = useToast();
  const { has } = useSession();
  const pos = useLoad(() => api.po.list(), []);
  const suppliers = useLoad(() => api.suppliers.list(), []);
  const [supplier, setSupplier] = useState("");
  const [reference, setReference] = useState("");
  const [lines, setLines] = useState<RecvLine[]>([]);
  const [scan, setScan] = useState("");
  const [results, setResults] = useState<PosSearchRow[]>([]);
  const [opId, setOpId] = useState(newOperationId);
  const act = useAction();
  const openPos = (pos.data ?? []).filter((p) => p.status === "ordered" || p.status === "partially_received");
  useEffect(() => {
    if (!scan.trim()) return setResults([]);
    const tv = setTimeout(
      () =>
        api.pos
          .search(scan, { limit: 8 })
          .then(setResults)
          .catch(() => {}),
      150,
    );
    return () => clearTimeout(tv);
  }, [scan]);
  const add = (p: PosSearchRow) => {
    setLines((ls) => {
      const i = ls.findIndex((l) => l.product.product_id === p.product_id);
      if (i >= 0) return ls.map((l, j) => (j === i ? { ...l, qty: formatQty((parseQty(l.qty) ?? 0) + 1000) } : l));
      return [...ls, { product: p, qty: "1", cost: "" }];
    });
    setScan("");
    setResults([]);
  };
  const total = lines.reduce((a, l) => {
    const q = parseQty(l.qty);
    const c = parseMoney(l.cost);
    return q !== null && c !== null ? a + mulDivRound(c, q, 1000) : a;
  }, 0);
  const valid = lines.length > 0 && lines.every((l) => (parseQty(l.qty) ?? 0) > 0 && parseMoney(l.cost) !== null);
  return (
    <div>
      <PageHeader
        title={t("Receiving")}
        subtitle={t("Receive goods against a purchase order, or record a direct delivery.")}
      />
      {openPos.length ? (
        <div className="card" style={{ marginBottom: 20 }}>
          <div className="card-head">
            <h3>{t("Purchase orders awaiting delivery")}</h3>
          </div>
          <table className="table">
            <tbody>
              {openPos.map((p) => (
                <tr
                  key={p.po_id}
                  className="clickable"
                  onClick={() => nav(`/admin/purchase-orders/${p.po_id}?receive=1`)}
                >
                  <td className="mono">{p.po_number}</td>
                  <td>{p.supplier_name}</td>
                  <td>{p.expected_at ?? "—"}</td>
                  <td className="num">{t("{0}% received", p.received_pct)}</td>
                  <td className="num">
                    <Button size="sm" variant="primary">
                      {t("Receive")}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
      <div className="card card-pad col gap-16">
        <h3>{t("Direct receiving (no purchase order)")}</h3>
        <div className="form-grid">
          <Field label={t("Supplier")}>
            <select className="select" value={supplier} onChange={(e) => setSupplier(e.target.value)}>
              <option value="">{t("No supplier")}</option>
              {(suppliers.data ?? []).map((s) => (
                <option key={s.supplier_id} value={s.supplier_id}>
                  {s.name}
                </option>
              ))}
            </select>
          </Field>
          <TextInput
            label={t("Invoice / delivery reference")}
            value={reference}
            onChange={(e) => setReference(e.target.value)}
          />
        </div>
        <div style={{ position: "relative" }}>
          <div className="scan-box">
            <ScanBarcode size={20} className="scan-icon" />
            <input
              className="input"
              placeholder={t("Scan or search product to add…")}
              value={scan}
              onChange={(e) => setScan(e.target.value)}
              onKeyDown={async (e) => {
                if (e.key === "Enter" && scan.trim()) {
                  const r = await api.pos.search(scan.trim(), { limit: 2 });
                  if (r[0]) add(r[0]);
                }
              }}
            />
          </div>
          {results.length ? (
            <div className="menu" style={{ insetInline: 0 }}>
              {results.map((r) => (
                <button key={r.product_id} onClick={() => add(r)}>
                  {r.name} <span className="tiny">{r.primary_barcode}</span>
                </button>
              ))}
            </div>
          ) : null}
        </div>
        {lines.length ? (
          <table className="table">
            <thead>
              <tr>
                <th>{t("Product")}</th>
                <th className="num">{t("Quantity")}</th>
                <th className="num">{t("Unit cost")}</th>
                <th className="num">{t("Line cost")}</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {lines.map((l, i) => {
                const q = parseQty(l.qty);
                const c = parseMoney(l.cost);
                return (
                  <tr key={l.product.product_id}>
                    <td>{l.product.name}</td>
                    <td className="num">
                      <input
                        className="input num"
                        style={{ width: 100 }}
                        value={l.qty}
                        onChange={(e) => setLines(lines.map((x, j) => (j === i ? { ...x, qty: e.target.value } : x)))}
                        aria-label={t("Quantity")}
                      />
                    </td>
                    <td className="num">
                      <input
                        className="input num"
                        style={{ width: 110 }}
                        value={l.cost}
                        placeholder={formatAmount(0)}
                        onChange={(e) => setLines(lines.map((x, j) => (j === i ? { ...x, cost: e.target.value } : x)))}
                        aria-label={t("Unit cost")}
                      />
                    </td>
                    <td className="num">{q !== null && c !== null ? formatMoney(mulDivRound(c, q, 1000)) : "—"}</td>
                    <td className="num">
                      <Button
                        size="sm"
                        variant="ghost"
                        aria-label={t("Remove")}
                        icon={<Trash2 size={14} />}
                        onClick={() => setLines(lines.filter((_, j) => j !== i))}
                      />
                    </td>
                  </tr>
                );
              })}
            </tbody>
            <tfoot>
              <tr>
                <td colSpan={3}>{t("Total cost")}</td>
                <td className="num">{formatMoney(total)}</td>
                <td />
              </tr>
            </tfoot>
          </table>
        ) : null}
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <div className="row">
          <Button
            variant="primary"
            className="right"
            disabled={!valid || !has("inventory.receive")}
            loading={act.busy}
            onClick={async () => {
              const r = await act.run(() =>
                api.inventory.receive({
                  supplier_id: supplier || null,
                  reference: reference || null,
                  operation_id: opId,
                  lines: lines.map((l) => ({
                    product_id: l.product.product_id,
                    qty_milli: parseQty(l.qty)!,
                    unit_cost_minor: parseMoney(l.cost)!,
                  })),
                }),
              );
              if (r) {
                toast("success", t("Goods received"), `${lines.length} product(s) added to stock`);
                setLines([]);
                setReference("");
                setOpId(newOperationId());
              }
            }}
          >
            {t("Receive Goods")}
          </Button>
        </div>
      </div>
    </div>
  );
}
