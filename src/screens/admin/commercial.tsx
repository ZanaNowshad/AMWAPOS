// Wave 5: the retail commercial foundation (docs/PRICING_AND_CATALOGUE.md).
// Likely duplicates and product merge, scale barcode rules, PLU and barcode
// type on the product, channel prices, pricing policies and the pricing
// review. Everything here is a person's decision: nothing changes a price or
// merges a product by itself.
import { useMemo, useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { GitMerge, Plus, ScanLine } from "lucide-react";
import { api } from "../../api";
import type {
  BarcodeKind,
  BarcodeRow,
  ChannelPriceRow,
  DupPair,
  MergeChoices,
  MergePreview,
  PricingGroup,
  PricingPolicy,
  PricingReviewRow,
  ProductDetail,
  ScaleRule,
  ScaleTest,
} from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { useApproval } from "../../components/approval";
import { newOperationId } from "../../lib/ids";
import { formatMoney, formatPercent, formatQty, parseMoney, parsePercent } from "../../lib/money";
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
import { Confirm, DataTable, Drawer, useAction, useLoad, type Column } from "./common";
import { t, tb } from "../../i18n";

// ------------------------------------------------------------------ labels

export function channelLabel(c: string | null | undefined): string {
  switch (c) {
    case "pos":
      return t("Till");
    case "whatsapp":
      return t("WhatsApp");
    case "phone":
      return t("Phone");
    case "web":
      return t("Web");
    case "other":
      return t("Other");
    case "retail":
      return t("Retail");
    default:
      return t("Not recorded");
  }
}

export const KIND_LABELS: Record<BarcodeKind, string> = {
  ean13: "EAN-13",
  ean8: "EAN-8",
  upc_a: "UPC-A",
  upc_e: "UPC-E",
  code128: "Code 128",
  internal: t("Internal"),
  supplier: t("Supplier code"),
};

function evidenceLabel(kind: string): string {
  switch (kind) {
    case "gtin":
      return t("Same barcode number");
    case "supplier_code":
      return t("Same supplier item code");
    case "same_name":
      return t("Same name and size");
    case "similar_name":
      return t("Name differs by one letter");
    case "same_size":
      return t("Same size");
    case "same_category":
      return t("Same category");
    case "similar_price":
      return t("Similar price");
    case "same_unit":
      return t("Same unit");
    default:
      return kind;
  }
}

// ------------------------------------------------------------------ duplicates

export function DuplicatesPage() {
  const { has } = useSession();
  const toast = useToast();
  const [later, setLater] = useState(false);
  const list = useLoad(() => api.duplicates.list(later), [later]);
  const [merge, setMerge] = useState<{ source: string; target: string } | null>(null);
  const act = useAction();
  const decide = async (p: DupPair, d: "not_duplicates" | "later" | null) => {
    const ok = await act.run(() => api.duplicates.decide(p.a.product_id, p.b.product_id, d));
    if (ok !== undefined) {
      toast(
        "success",
        d === "not_duplicates"
          ? t("Marked as not duplicates")
          : d === "later"
            ? t("Set aside for later")
            : t("Back in the list"),
      );
      await list.reload();
    }
  };
  const pairs = list.data?.pairs ?? null;
  return (
    <div className="col gap-16">
      <PageHeader
        title={t("Likely duplicates")}
        subtitle={t(
          "Products that look like the same item, with the evidence. Nothing is merged unless the owner merges it.",
        )}
        actions={
          <Checkbox
            label={t("Show set aside ({0})", list.data?.later_count ?? 0)}
            checked={later}
            onChange={setLater}
          />
        }
      />
      {list.error ? <Banner tone="danger">{list.error}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {list.loading && !pairs ? (
        <Skeleton rows={6} />
      ) : !pairs || pairs.length === 0 ? (
        <Empty title={t("No likely duplicate products found.")}>
          {t("Products are compared by name and size, barcode number and supplier item code.")}
        </Empty>
      ) : (
        <div className="col gap-12" data-testid="duplicates">
          {pairs.map((p) => (
            <div className="card pad col gap-8" key={`${p.a.product_id}-${p.b.product_id}`} data-testid="dup-pair">
              <div className="row gap-12" style={{ alignItems: "flex-start" }}>
                {[p.a, p.b].map((x) => (
                  <div key={x.product_id} className="grow col gap-4">
                    <a href={`#/admin/products/${x.product_id}`} style={{ fontWeight: 650 }} dir="auto">
                      {x.name}
                    </a>
                    <div className="tiny muted">
                      {x.sku} · {formatMoney(x.price_minor)} · {x.barcodes.join(", ") || t("No barcode")}
                    </div>
                  </div>
                ))}
                <Chip tone={p.score >= 80 ? "warning" : "default"}>{t("Match {0}%", p.score)}</Chip>
              </div>
              <div className="row gap-6" style={{ flexWrap: "wrap" }}>
                {p.evidence.map((e) => (
                  <Chip key={e.kind} tone="info">
                    {evidenceLabel(e.kind)}
                  </Chip>
                ))}
                {p.decision === "later" ? <Chip>{t("Set aside")}</Chip> : null}
              </div>
              {has("products.manage") ? (
                <div className="row gap-8">
                  <Button size="sm" onClick={() => decide(p, "not_duplicates")}>
                    {t("Not duplicates")}
                  </Button>
                  {p.decision === "later" ? (
                    <Button size="sm" onClick={() => decide(p, null)}>
                      {t("Back to the list")}
                    </Button>
                  ) : (
                    <Button size="sm" onClick={() => decide(p, "later")}>
                      {t("Review later")}
                    </Button>
                  )}
                  <Button
                    size="sm"
                    variant="primary"
                    className="right"
                    icon={<GitMerge size={16} />}
                    onClick={() => setMerge({ source: p.b.product_id, target: p.a.product_id })}
                  >
                    {t("Merge…")}
                  </Button>
                </div>
              ) : null}
            </div>
          ))}
        </div>
      )}
      {merge ? (
        <MergeDialog
          source={merge.source}
          target={merge.target}
          onSwap={() => setMerge({ source: merge.target, target: merge.source })}
          onClose={() => setMerge(null)}
          onDone={async () => {
            setMerge(null);
            toast("success", t("Products merged"));
            await list.reload();
          }}
        />
      ) : null}
    </div>
  );
}

/** Preview everything a merge moves; the owner chooses what to keep. */
export function MergeDialog({
  source,
  target,
  onSwap,
  onClose,
  onDone,
}: {
  source: string;
  target: string;
  onSwap?: () => void;
  onClose: () => void;
  onDone: () => void;
}) {
  const { has } = useSession();
  const pv = useLoad(() => api.products.mergePreview(source, target), [source, target]);
  const [ch, setCh] = useState<MergeChoices>({ supplier_terms: {} });
  const [confirm, setConfirm] = useState(false);
  const act = useAction();
  const p: MergePreview | null = pv.data;
  const canMerge = has("catalog.merge");
  const missing = p
    ? (p.conflicts.price && !ch.price) ||
      (p.conflicts.plu && !ch.plu) ||
      p.conflicts.supplier_terms.some((s) => !ch.supplier_terms?.[s.supplier_id])
    : true;
  const run = async () => {
    if (!p) return;
    const r = await act.run(() =>
      api.products.merge({
        source_product_id: source,
        target_product_id: target,
        choices: ch,
        preview_hash: p.preview_hash,
        operation_id: newOperationId(),
      }),
    );
    if (r) onDone();
    else {
      setConfirm(false);
      await pv.reload();
    }
  };
  const pick = (
    label: string,
    kept: string,
    retired: string,
    sel: string | null | undefined,
    set: (v: "source" | "target") => void,
  ) => (
    <div className="row gap-8" key={label}>
      <span className="grow">{label}</span>
      {(["target", "source"] as const).map((k) => (
        <label key={k} className="checkbox">
          <input type="radio" name={label} checked={sel === k} onChange={() => set(k)} />
          <span>{k === "target" ? t("Kept product's: {0}", kept) : t("Retired product's: {0}", retired)}</span>
        </label>
      ))}
    </div>
  );
  return (
    <Modal
      title={t("Merge products")}
      size="lg"
      onClose={onClose}
      testId="merge-dialog"
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          {onSwap ? <Button onClick={onSwap}>{t("Keep the other one")}</Button> : null}
          <Button
            variant="danger"
            className="right"
            disabled={!p || !p.can_merge || !!missing || !canMerge}
            onClick={() => setConfirm(true)}
          >
            {t("Merge")}
          </Button>
        </>
      }
    >
      {pv.loading && !p ? <Skeleton rows={6} /> : null}
      {pv.error ? <Banner tone="danger">{pv.error}</Banner> : null}
      {p ? (
        <div className="col gap-12" data-testid="merge-preview">
          <div className="row gap-12">
            <div className="grow card pad">
              <div className="tiny muted">{t("Retired")}</div>
              <div style={{ fontWeight: 650 }} dir="auto">
                {p.source.name}
              </div>
              <div className="tiny">{p.source.sku}</div>
            </div>
            <div className="grow card pad">
              <div className="tiny muted">{t("Kept")}</div>
              <div style={{ fontWeight: 650 }} dir="auto">
                {p.target.name}
              </div>
              <div className="tiny">{p.target.sku}</div>
            </div>
          </div>
          {!canMerge ? <Banner tone="info">{t("Only the owner can merge products.")}</Banner> : null}
          {p.blockers.length > 0 ? (
            <Banner tone="warning" title={t("Finish or cancel these first")}>
              <ul style={{ margin: 0 }}>
                {p.blockers.map((b) => (
                  <li key={b.kind}>
                    {tb(b.label)}: {b.refs.join(", ")}
                  </li>
                ))}
              </ul>
            </Banner>
          ) : null}
          {p.issues.map((i) => (
            <Banner key={i} tone="warning">
              {tb(i)}
            </Banner>
          ))}
          <div className="col gap-4">
            <strong>{t("What moves to the kept product")}</strong>
            {p.moves.stock.map((s) => (
              <div key={s.branch_id} className="tiny">
                {t("Stock {0} at {1}", formatQty(s.source_milli), s.branch_name ?? s.branch_id)}
                {s.lots.length > 0 ? ` · ${t("{0} batch(es) carried with their dates", s.lots.length)}` : ""}
              </div>
            ))}
            <div className="tiny">{t("Barcodes: {0}", p.moves.barcodes.join(", ") || "—")}</div>
            <div className="tiny">
              {t(
                "Supplier terms: {0}, saved names and mappings: {1}",
                p.moves.supplier_terms.length,
                p.moves.aliases + p.moves.supplier_maps,
              )}
            </div>
            {p.not_carried.channel_prices > 0 ? (
              <div className="tiny">
                {t("{0} channel price(s) of the retired product are not carried over.", p.not_carried.channel_prices)}
              </div>
            ) : null}
            <div className="tiny muted">
              {t(
                "History stays as it was: {0} sale line(s), receipts and movements are not changed.",
                p.history.sale_lines,
              )}
            </div>
          </div>
          {p.conflicts.price || p.conflicts.plu || p.conflicts.supplier_terms.length > 0 ? (
            <div className="col gap-8 card pad">
              <strong>{t("Choose what to keep")}</strong>
              {p.conflicts.price
                ? pick(
                    t("Selling price"),
                    formatMoney(p.conflicts.price.target),
                    formatMoney(p.conflicts.price.source),
                    ch.price,
                    (v) => setCh({ ...ch, price: v }),
                  )
                : null}
              {p.conflicts.plu
                ? pick(t("PLU"), p.conflicts.plu.target, p.conflicts.plu.source, ch.plu, (v) =>
                    setCh({ ...ch, plu: v }),
                  )
                : null}
              {p.conflicts.supplier_terms.map((s) =>
                pick(
                  t("Terms of {0}", s.supplier_name),
                  t("current terms"),
                  t("its terms"),
                  ch.supplier_terms?.[s.supplier_id],
                  (v) => setCh({ ...ch, supplier_terms: { ...(ch.supplier_terms ?? {}), [s.supplier_id]: v } }),
                ),
              )}
            </div>
          ) : null}
          {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        </div>
      ) : null}
      {confirm && p ? (
        <Confirm
          title={t("Merge cannot be undone")}
          confirmLabel={t("Merge")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setConfirm(false)}
          onConfirm={run}
        >
          {t(
            "{0} will be retired and everything above moves to {1}. This cannot be undone.",
            p.source.name,
            p.target.name,
          )}
        </Confirm>
      ) : null}
    </Modal>
  );
}

// ------------------------------------------------------------------ scale barcodes

const EMPTY_RULE: Partial<ScaleRule> = {
  name: "",
  prefix: "2",
  length: 13,
  item_start: 3,
  item_length: 5,
  value_kind: "weight",
  value_start: 8,
  value_length: 5,
  decimals: 3,
  check_digit: "ean",
  active: true,
  priority: 0,
  version: 0,
};

export function ScaleRulesPage() {
  const { has } = useSession();
  const toast = useToast();
  const rules = useLoad(() => api.scaleRules.list(), []);
  const [edit, setEdit] = useState<Partial<ScaleRule> | null>(null);
  const [code, setCode] = useState("");
  const [test, setTest] = useState<ScaleTest | null>(null);
  const act = useAction();
  const canEdit = has("barcode_rules.manage");
  const save = async () => {
    if (!edit) return;
    const r = await act.run(() => api.scaleRules.save({ ...edit, rule_id: edit.rule_id ?? "" }));
    if (r) {
      toast("success", t("Rule saved"));
      setEdit(null);
      await rules.reload();
    }
  };
  const num = (k: keyof ScaleRule) => (e: React.ChangeEvent<HTMLInputElement>) =>
    setEdit((x) => (x ? { ...x, [k]: Number(e.target.value || 0) } : x));
  const cols: Column<ScaleRule>[] = [
    { key: "name", label: t("Name"), render: (r) => r.name },
    { key: "prefix", label: t("Starts with"), render: (r) => r.prefix },
    { key: "length", label: t("Digits"), num: true, render: (r) => r.length },
    {
      key: "kind",
      label: t("Holds"),
      render: (r) => (r.value_kind === "weight" ? t("Weight") : t("Price")),
    },
    { key: "priority", label: t("Priority"), num: true, render: (r) => r.priority },
    {
      key: "active",
      label: t("Status"),
      render: (r) => (r.active ? <Chip tone="success">{t("On")}</Chip> : <Chip>{t("Off")}</Chip>),
    },
  ];
  return (
    <div className="col gap-16">
      <PageHeader
        title={t("Scale barcodes")}
        subtitle={t("Barcodes printed by scales carry the item's PLU and a weight or price. Tills read them offline.")}
        actions={
          canEdit ? (
            <Button variant="primary" icon={<Plus size={16} />} onClick={() => setEdit({ ...EMPTY_RULE })}>
              {t("Add rule")}
            </Button>
          ) : null
        }
      />
      <div className="card pad col gap-8">
        <div className="row gap-8">
          <TextInput
            label={t("Try a code")}
            value={code}
            onChange={(e) => setCode(e.target.value)}
            fieldClass="grow"
            inputMode="numeric"
          />
          <Button
            icon={<ScanLine size={16} />}
            className="self-end"
            disabled={!code.trim()}
            onClick={async () => setTest((await act.run(() => api.scaleRules.test(code.trim()))) ?? null)}
          >
            {t("Check")}
          </Button>
        </div>
        {test ? (
          <Banner tone={test.outcome === "ambiguous" ? "danger" : test.outcome === "unknown" ? "warning" : "success"}>
            <div data-testid="scale-test">
              {tb(test.message)}
              {test.read ? (
                <div className="tiny">
                  {test.read.value_kind === "weight"
                    ? t("PLU {0} · weight {1}", test.read.plu, formatQty(test.read.value_milli))
                    : t("PLU {0} · price {1}", test.read.plu, formatMoney(test.read.value_milli))}
                </div>
              ) : null}
              {test.rules.length > 1 ? <div className="tiny">{test.rules.join(", ")}</div> : null}
            </div>
          </Banner>
        ) : null}
      </div>
      {act.error && !edit ? <Banner tone="danger">{act.error}</Banner> : null}
      {rules.data && rules.data.length === 0 ? (
        <Empty title={t("No scale barcode rules configured.")}>
          {t("Add a rule to sell weighed or pre-priced items from scale labels.")}
        </Empty>
      ) : (
        <DataTable
          rows={rules.data}
          loading={rules.loading}
          columns={cols}
          rowKey={(r) => r.rule_id}
          onRowClick={canEdit ? (r) => setEdit(r) : undefined}
        />
      )}
      {edit ? (
        <Drawer
          title={edit.rule_id ? t("Edit scale rule") : t("New scale rule")}
          onClose={() => setEdit(null)}
          actions={
            <Button variant="primary" loading={act.busy} onClick={save}>
              {t("Save")}
            </Button>
          }
        >
          <div className="col gap-12">
            <TextInput
              label={t("Name")}
              value={edit.name ?? ""}
              onChange={(e) => setEdit({ ...edit, name: e.target.value })}
            />
            <div className="form-grid">
              <TextInput
                label={t("Starts with")}
                value={edit.prefix ?? ""}
                onChange={(e) => setEdit({ ...edit, prefix: e.target.value })}
                inputMode="numeric"
              />
              <TextInput
                label={t("Digits in the code")}
                type="number"
                value={edit.length ?? 13}
                onChange={num("length")}
              />
              <TextInput
                label={t("Item code starts at")}
                type="number"
                value={edit.item_start ?? 3}
                onChange={num("item_start")}
              />
              <TextInput
                label={t("Item code digits")}
                type="number"
                value={edit.item_length ?? 5}
                onChange={num("item_length")}
              />
              <Field label={t("The code holds")}>
                <select
                  className="input"
                  value={edit.value_kind}
                  onChange={(e) => setEdit({ ...edit, value_kind: e.target.value as "weight" | "price" })}
                >
                  <option value="weight">{t("Weight")}</option>
                  <option value="price">{t("Price")}</option>
                </select>
              </Field>
              <TextInput label={t("Decimals")} type="number" value={edit.decimals ?? 3} onChange={num("decimals")} />
              <TextInput
                label={t("Value starts at")}
                type="number"
                value={edit.value_start ?? 8}
                onChange={num("value_start")}
              />
              <TextInput
                label={t("Value digits")}
                type="number"
                value={edit.value_length ?? 5}
                onChange={num("value_length")}
              />
              <Field label={t("Check digit")}>
                <select
                  className="input"
                  value={edit.check_digit}
                  onChange={(e) => setEdit({ ...edit, check_digit: e.target.value as "none" | "ean" })}
                >
                  <option value="ean">{t("EAN check digit")}</option>
                  <option value="none">{t("None")}</option>
                </select>
              </Field>
              <TextInput
                label={t("Priority")}
                type="number"
                value={edit.priority ?? 0}
                onChange={num("priority")}
                hint={t(
                  "When two rules fit a code, the higher priority is used. Two at the same priority stop the sale.",
                )}
              />
            </div>
            <Checkbox
              label={t("Rule is on")}
              checked={edit.active ?? true}
              onChange={(v) => setEdit({ ...edit, active: v })}
            />
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          </div>
        </Drawer>
      ) : null}
    </div>
  );
}

// ------------------------------------------------------------------ product: PLU, barcode type, channel prices

export function PluCard({ detail, onChanged }: { detail: ProductDetail; onChanged: () => Promise<void> }) {
  const { has } = useSession();
  const [plu, setPlu] = useState(detail.plu ?? "");
  const act = useAction();
  const toast = useToast();
  return (
    <div className="card pad col gap-8">
      <strong>{t("PLU")}</strong>
      <div className="tiny muted">
        {t("A short number typed at the till or printed by a scale. Leading zeros are ignored.")}
      </div>
      <div className="row gap-8">
        <input
          className="input num"
          aria-label={t("PLU")}
          value={plu}
          disabled={!has("products.manage")}
          inputMode="numeric"
          onChange={(e) => setPlu(e.target.value)}
        />
        {has("products.manage") ? (
          <Button
            loading={act.busy}
            disabled={plu === (detail.plu ?? "")}
            onClick={async () => {
              const r = await act.run(() => api.products.setPlu(detail.product_id, plu.trim() || null));
              if (r) {
                toast("success", t("PLU saved"));
                setPlu(r.plu ?? "");
                await onChanged();
              }
            }}
          >
            {t("Save PLU")}
          </Button>
        ) : null}
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
    </div>
  );
}

export function BarcodeKindSelect({ row, onChanged }: { row: BarcodeRow; onChanged: () => Promise<void> }) {
  const { has } = useSession();
  const act = useAction();
  return (
    <span className="row gap-4">
      <select
        className="input"
        aria-label={t("Type of {0}", row.barcode)}
        value={row.kind ?? ""}
        disabled={!has("products.manage") || act.busy}
        onChange={async (e) => {
          const v = (e.target.value || null) as BarcodeKind | null;
          if ((await act.run(() => api.barcodes.setKind(row.barcode_id, v))) !== undefined) await onChanged();
        }}
      >
        <option value="">
          {row.suggested_kind ? t("Not recorded (looks like {0})", KIND_LABELS[row.suggested_kind]) : t("Not recorded")}
        </option>
        {(Object.keys(KIND_LABELS) as BarcodeKind[]).map((k) => (
          <option key={k} value={k}>
            {KIND_LABELS[k]}
          </option>
        ))}
      </select>
      {act.error ? <span className="tiny danger-text">{act.error}</span> : null}
    </span>
  );
}

export function ChannelPricesCard({ productId }: { productId: string }) {
  const { has } = useSession();
  const rows = useLoad(() => api.products.channelPrices(productId), [productId]);
  const [edit, setEdit] = useState<Record<string, string>>({});
  const act = useAction();
  const toast = useToast();
  const canEdit = has("prices.manage");
  const save = async (r: ChannelPriceRow, clear = false) => {
    const v = clear ? null : parseMoney(edit[r.price_type] ?? "");
    if (!clear && (v === null || v <= 0)) {
      act.setError(t("Enter a valid selling price."));
      return;
    }
    const out = await act.run(() => api.products.channelPriceSet(productId, r.price_type, v));
    if (out) {
      rows.setData(out);
      setEdit({ ...edit, [r.price_type]: "" });
      toast("success", clear ? t("Channel price removed") : t("Channel price saved"));
    }
  };
  return (
    <div className="card pad col gap-8" data-testid="channel-prices">
      <strong>{t("Channel prices")}</strong>
      <div className="tiny muted">{t("Retail price will be used when no channel price is set.")}</div>
      {rows.data?.map((r) => (
        <div key={r.price_type} className="row gap-8">
          <span style={{ minWidth: 90 }}>{channelLabel(r.price_type)}</span>
          <span className="grow">
            <strong>{formatMoney(r.effective_price_minor)}</strong>{" "}
            {r.using_retail ? <Chip>{t("Using retail price")}</Chip> : null}
          </span>
          {canEdit ? (
            <>
              <input
                className="input num"
                style={{ maxWidth: 110 }}
                aria-label={t("{0} price", channelLabel(r.price_type))}
                value={edit[r.price_type] ?? ""}
                placeholder={r.own_price_minor !== null ? formatMoney(r.own_price_minor) : ""}
                onChange={(e) => setEdit({ ...edit, [r.price_type]: e.target.value })}
              />
              <Button size="sm" disabled={!(edit[r.price_type] ?? "").trim()} onClick={() => save(r)}>
                {t("Set")}
              </Button>
              {r.own_price_minor !== null ? (
                <Button size="sm" variant="ghost" onClick={() => save(r, true)}>
                  {t("Use retail")}
                </Button>
              ) : null}
            </>
          ) : null}
        </div>
      ))}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
    </div>
  );
}

// ------------------------------------------------------------------ pricing review

const GROUPS: { key: PricingGroup; label: string }[] = [
  { key: "below_min_margin", label: t("Below minimum margin") },
  { key: "cost_changed", label: t("Cost changed") },
  { key: "recommendation", label: t("Recommendation available") },
  { key: "channel_price_missing", label: t("Channel price missing") },
  { key: "no_policy", label: t("No policy") },
];

export function PricingReviewPage() {
  const { has } = useSession();
  const toast = useToast();
  const approval = useApproval();
  const nav = useNavigate();
  const [search, setSearch] = useSearchParams();
  const group = (search.get("group") as PricingGroup) || "below_min_margin";
  const rv = useLoad(() => api.pricing.review(group, 500, 0), [group]);
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [modify, setModify] = useState<Record<string, string>>({});
  const [preview, setPreview] = useState<Awaited<ReturnType<typeof api.pricing.applyPreview>> | null>(null);
  const [items, setItems] = useState<{ product_id: string; price_type: string; amount_minor: number }[]>([]);
  const [postpone, setPostpone] = useState<PricingReviewRow | null>(null);
  const [until, setUntil] = useState("");
  const act = useAction();
  const key = (r: PricingReviewRow) => `${r.product_id}|${r.price_type}`;
  const priceFor = (r: PricingReviewRow): number | null => {
    const typed = modify[key(r)];
    if (typed !== undefined && typed.trim()) return parseMoney(typed);
    return r.recommended_minor;
  };
  const startApply = async (rows: PricingReviewRow[]) => {
    const its = rows
      .map((r) => ({ product_id: r.product_id, price_type: r.price_type, amount_minor: priceFor(r) ?? 0 }))
      .filter((x) => x.amount_minor > 0);
    if (its.length === 0) {
      act.setError(t("Choose prices to apply."));
      return;
    }
    const pv = await act.run(() => api.pricing.applyPreview(its));
    if (pv) {
      setItems(its);
      setPreview(pv);
    }
  };
  const apply = async () => {
    const op = newOperationId();
    const r = await act.run(() =>
      approval((tok) =>
        api.pricing.apply({ items, operation_id: op, reason: t("Pricing review"), approval_token: tok }),
      ),
    );
    if (r) {
      toast("success", t("{0} price(s) applied", r.applied));
      setPreview(null);
      setSel(new Set());
      setModify({});
      await rv.reload();
    }
  };
  const decide = async (r: PricingReviewRow, d: "dismissed" | "postponed" | null, u?: string) => {
    if ((await act.run(() => api.pricing.decide(r.product_id, r.price_type, d, u ?? null))) !== undefined) {
      toast("success", d === "dismissed" ? t("Dismissed") : t("Postponed"));
      setPostpone(null);
      await rv.reload();
    }
  };
  const rows = rv.data?.rows ?? null;
  const canApply = has("prices.manage");
  const cols: Column<PricingReviewRow>[] = [
    {
      key: "name",
      label: t("Product"),
      render: (r) => (
        <div className="col">
          <span dir="auto">{r.name}</span>
          <span className="tiny muted">
            {channelLabel(r.price_type)} · {r.policy_name ?? t("No policy")}
          </span>
        </div>
      ),
    },
    { key: "cost", label: t("Cost"), num: true, render: (r) => formatMoney(r.cost_minor) },
    {
      key: "current",
      label: t("Current"),
      num: true,
      render: (r) => (
        <div className="col">
          <span>{formatMoney(r.current_minor)}</span>
          <span className={`tiny ${r.groups.includes("below_min_margin") ? "danger-text" : "muted"}`}>
            {formatPercent(r.margin_bp)}
          </span>
        </div>
      ),
    },
    {
      key: "floor",
      label: t("Minimum"),
      num: true,
      render: (r) =>
        r.floor_price_minor !== null ? `${formatMoney(r.floor_price_minor)} (${formatPercent(r.floor_bp)})` : "—",
    },
    {
      key: "rec",
      label: t("Recommended"),
      num: true,
      render: (r) =>
        r.recommended_minor !== null ? (
          <div className="col">
            <span>{formatMoney(r.recommended_minor)}</span>
            <span className="tiny muted">{formatPercent(r.recommended_margin_bp)}</span>
          </div>
        ) : (
          "—"
        ),
    },
    {
      key: "modify",
      label: t("New price"),
      render: (r) =>
        canApply ? (
          <input
            className="input num"
            style={{ maxWidth: 100 }}
            aria-label={t("New price for {0}", r.name)}
            placeholder={r.recommended_minor !== null ? formatMoney(r.recommended_minor) : ""}
            value={modify[key(r)] ?? ""}
            onClick={(e) => e.stopPropagation()}
            onChange={(e) => setModify({ ...modify, [key(r)]: e.target.value })}
          />
        ) : null,
    },
    {
      key: "actions",
      label: "",
      render: (r) => (
        <div className="row gap-4" onClick={(e) => e.stopPropagation()}>
          <Button size="sm" onClick={() => nav(`/admin/products/${r.product_id}`)}>
            {t("Inspect")}
          </Button>
          {canApply ? (
            <>
              <Button size="sm" variant="primary" disabled={priceFor(r) === null} onClick={() => startApply([r])}>
                {t("Accept")}
              </Button>
              <Button size="sm" variant="ghost" onClick={() => decide(r, "dismissed")}>
                {t("Dismiss")}
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setPostpone(r)}>
                {t("Postpone")}
              </Button>
            </>
          ) : null}
        </div>
      ),
    },
  ];
  const selected = (rows ?? []).filter((r) => sel.has(key(r)));
  return (
    <div className="col gap-16">
      <PageHeader
        title={t("Pricing review")}
        subtitle={t("Recommendations from your pricing policies. Nothing changes until you apply it.")}
        actions={
          <>
            <Button onClick={() => nav("/admin/pricing-policies")}>{t("Pricing policies")}</Button>
            {canApply ? (
              <Button variant="primary" disabled={selected.length === 0} onClick={() => startApply(selected)}>
                {t("Apply {0} selected…", selected.length)}
              </Button>
            ) : null}
          </>
        }
      />
      <Tabs
        tabs={GROUPS.map((g) => ({ key: g.key, label: `${g.label} (${rv.data?.counts?.[g.key] ?? 0})` }))}
        value={group}
        onChange={(g) => {
          setSel(new Set());
          setSearch({ group: g });
        }}
      />
      {rv.data && rv.data.active_policies === 0 ? (
        <Banner
          tone="info"
          action={<Button onClick={() => nav("/admin/pricing-policies")}>{t("Add a policy")}</Button>}
        >
          {t("No pricing policy yet. Add one to get recommendations and margin protection.")}
        </Banner>
      ) : null}
      {rv.data?.ambiguous.map((a) => (
        <Banner key={`${a.policy}-${a.tied_with.join()}`} tone="warning">
          {t(
            "Policies {0} and {1} apply equally; {0} is used until you change a priority.",
            a.policy ?? "",
            a.tied_with.join(", "),
          )}
        </Banner>
      ))}
      {rv.error ? <Banner tone="danger">{rv.error}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {rows && rows.length === 0 ? (
        <Empty
          title={
            group === "below_min_margin" ? t("All active products are within your margin rules.") : t("Nothing here.")
          }
        >
          {group === "channel_price_missing" ? t("Retail price will be used when no channel price is set.") : null}
        </Empty>
      ) : (
        <DataTable
          rows={rows}
          loading={rv.loading}
          columns={cols}
          rowKey={key}
          selectable={canApply}
          selected={sel}
          onSelect={setSel}
        />
      )}
      {preview ? (
        <Modal
          title={t("Apply prices")}
          size="lg"
          onClose={() => setPreview(null)}
          testId="pricing-apply"
          footer={
            <>
              <Button onClick={() => setPreview(null)}>{t("Cancel")}</Button>
              <Button
                variant="primary"
                className="right"
                loading={act.busy}
                disabled={preview.changes === 0}
                onClick={apply}
              >
                {t("Apply {0} price(s)", preview.changes)}
              </Button>
            </>
          }
        >
          <div className="col gap-8">
            {preview.below_min_margin > 0 ? (
              <Banner tone="warning">
                {t("{0} price(s) are below the minimum margin.", preview.below_min_margin)}{" "}
                {preview.needs_approval ? t("A manager who may set pricing policies must approve.") : null}
              </Banner>
            ) : null}
            <table className="table">
              <thead>
                <tr>
                  <th>{t("Product")}</th>
                  <th>{t("Price list")}</th>
                  <th className="num">{t("Old")}</th>
                  <th className="num">{t("New")}</th>
                  <th className="num">{t("Margin")}</th>
                </tr>
              </thead>
              <tbody>
                {preview.rows.map((r) => (
                  <tr key={`${r.product_id}|${r.price_type}`} className={r.below_min_margin ? "danger-row" : ""}>
                    <td dir="auto">{r.name}</td>
                    <td>{channelLabel(r.price_type)}</td>
                    <td className="num">{formatMoney(r.old_minor)}</td>
                    <td className="num">{r.unchanged ? t("No change") : formatMoney(r.new_minor)}</td>
                    <td className="num">{formatPercent(r.new_margin_bp)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Modal>
      ) : null}
      {postpone ? (
        <Modal
          title={t("Postpone")}
          size="sm"
          onClose={() => setPostpone(null)}
          footer={
            <>
              <Button onClick={() => setPostpone(null)}>{t("Cancel")}</Button>
              <Button
                variant="primary"
                className="right"
                disabled={!until}
                onClick={() => decide(postpone, "postponed", until)}
              >
                {t("Postpone")}
              </Button>
            </>
          }
        >
          <TextInput label={t("Until")} type="date" value={until} onChange={(e) => setUntil(e.target.value)} />
        </Modal>
      ) : null}
    </div>
  );
}

// ------------------------------------------------------------------ pricing policies

const STEPS = [1, 5, 10, 25, 50, 100, 250, 500, 1000];
const EMPTY_POLICY: Partial<PricingPolicy> = {
  policy_id: "",
  name: "",
  scope: "global",
  scope_id: null,
  markup_bp: null,
  target_margin_bp: null,
  min_margin_bp: null,
  rounding_step_minor: 5,
  ending_minor: null,
  priority: 0,
  cost_basis: "average",
  active: true,
  version: 0,
};

export function PricingPoliciesPage() {
  const { has } = useSession();
  const toast = useToast();
  const list = useLoad(() => api.pricing.policies(), []);
  const cats = useLoad(() => api.categories.list(), []);
  const sups = useLoad(() => api.suppliers.list(), []);
  const [edit, setEdit] = useState<Partial<PricingPolicy> | null>(null);
  const [mode, setMode] = useState<"margin" | "markup" | "none">("margin");
  const [pct, setPct] = useState({ target: "", min: "", ending: "" });
  const act = useAction();
  const canEdit = has("pricing.policy");
  const open = (p: Partial<PricingPolicy>) => {
    setEdit(p);
    setMode(p.markup_bp != null ? "markup" : p.target_margin_bp != null ? "margin" : p.policy_id ? "none" : "margin");
    setPct({
      target:
        p.markup_bp != null
          ? formatPercent(p.markup_bp)
          : p.target_margin_bp != null
            ? formatPercent(p.target_margin_bp)
            : "",
      min: p.min_margin_bp != null ? formatPercent(p.min_margin_bp) : "",
      ending: p.ending_minor != null ? formatMoney(p.ending_minor).replace(/[^0-9.]/g, "") : "",
    });
  };
  const scopeName = (p: PricingPolicy) => {
    switch (p.scope) {
      case "global":
        return t("All products");
      case "category":
        return t("Category: {0}", cats.data?.find((c) => c.category_id === p.scope_id)?.name ?? "?");
      case "supplier":
        return t("Preferred supplier: {0}", sups.data?.find((s) => s.supplier_id === p.scope_id)?.name ?? "?");
      case "branch":
        return t("Branch");
      default:
        return t("Channel: {0}", channelLabel(p.scope_id));
    }
  };
  const save = async () => {
    if (!edit) return;
    const target = pct.target.trim() ? parsePercent(pct.target) : null;
    const min = pct.min.trim() ? parsePercent(pct.min) : null;
    const end = pct.ending.trim() ? Math.round(Number(pct.ending.replace(/^0?\./, "0.")) * 1000) % 1000 : null;
    const p = {
      ...edit,
      markup_bp: mode === "markup" ? target : null,
      target_margin_bp: mode === "margin" ? target : null,
      min_margin_bp: min,
      ending_minor: end,
    };
    const r = await act.run(() => api.pricing.savePolicy(p));
    if (r) {
      toast("success", t("Policy saved. No price was changed."));
      setEdit(null);
      await list.reload();
    }
  };
  const cols: Column<PricingPolicy>[] = [
    { key: "name", label: t("Name"), render: (p) => p.name },
    { key: "scope", label: t("Applies to"), render: scopeName },
    {
      key: "terms",
      label: t("Terms"),
      render: (p) =>
        [
          p.markup_bp != null ? t("Markup {0} on cost", formatPercent(p.markup_bp)) : null,
          p.target_margin_bp != null ? t("Margin {0} on price", formatPercent(p.target_margin_bp)) : null,
          p.min_margin_bp != null ? t("Minimum {0}", formatPercent(p.min_margin_bp)) : null,
        ]
          .filter(Boolean)
          .join(" · "),
    },
    { key: "round", label: t("Rounding"), render: (p) => formatMoney(p.rounding_step_minor) },
    { key: "priority", label: t("Priority"), num: true, render: (p) => p.priority },
    {
      key: "active",
      label: t("Status"),
      render: (p) => (p.active ? <Chip tone="success">{t("On")}</Chip> : <Chip>{t("Off")}</Chip>),
    },
  ];
  const scopeOptions = useMemo(() => {
    if (!edit) return [];
    if (edit.scope === "category") return (cats.data ?? []).map((c) => ({ id: c.category_id, name: c.name }));
    if (edit.scope === "supplier") return (sups.data ?? []).map((s) => ({ id: s.supplier_id, name: s.name }));
    if (edit.scope === "channel")
      return ["pos", "whatsapp", "phone", "web"].map((c) => ({ id: c, name: channelLabel(c) }));
    return [];
  }, [edit, cats.data, sups.data]);
  return (
    <div className="col gap-16">
      <PageHeader
        title={t("Pricing policies")}
        subtitle={t("Rules that recommend prices and protect your margin. They never change a price by themselves.")}
        actions={
          canEdit ? (
            <Button variant="primary" icon={<Plus size={16} />} onClick={() => open({ ...EMPTY_POLICY })}>
              {t("Add policy")}
            </Button>
          ) : null
        }
      />
      <Banner tone="info">
        {t(
          "The most specific policy applies: channel, then branch, then preferred supplier, then category, then all products. The highest minimum margin of all matching policies always applies.",
        )}
      </Banner>
      {list.data && list.data.length === 0 ? (
        <Empty title={t("No pricing policies yet.")}>{t("Add a policy for all products to start.")}</Empty>
      ) : (
        <DataTable
          rows={list.data}
          loading={list.loading}
          columns={cols}
          rowKey={(p) => p.policy_id}
          onRowClick={canEdit ? open : undefined}
        />
      )}
      {edit ? (
        <Drawer
          title={edit.policy_id ? t("Edit policy") : t("New policy")}
          onClose={() => setEdit(null)}
          actions={
            <Button variant="primary" loading={act.busy} onClick={save}>
              {t("Save")}
            </Button>
          }
        >
          <div className="col gap-12">
            <TextInput
              label={t("Name")}
              value={edit.name ?? ""}
              onChange={(e) => setEdit({ ...edit, name: e.target.value })}
            />
            <Field label={t("Applies to")}>
              <select
                className="input"
                value={edit.scope}
                onChange={(e) => setEdit({ ...edit, scope: e.target.value as PricingPolicy["scope"], scope_id: null })}
              >
                <option value="global">{t("All products")}</option>
                <option value="category">{t("A category")}</option>
                <option value="supplier">{t("A preferred supplier")}</option>
                <option value="channel">{t("A sales channel")}</option>
              </select>
            </Field>
            {edit.scope !== "global" ? (
              <Field label={t("Which one")}>
                <select
                  className="input"
                  value={edit.scope_id ?? ""}
                  onChange={(e) => setEdit({ ...edit, scope_id: e.target.value || null })}
                >
                  <option value="">{t("Choose…")}</option>
                  {scopeOptions.map((o) => (
                    <option key={o.id} value={o.id}>
                      {o.name}
                    </option>
                  ))}
                </select>
              </Field>
            ) : null}
            <Field
              label={t("Price from cost")}
              hint={t(
                "Margin is measured on the selling price without VAT; markup is added on cost. 25% markup is a 20% margin.",
              )}
            >
              <select className="input" value={mode} onChange={(e) => setMode(e.target.value as typeof mode)}>
                <option value="margin">{t("Target margin on price")}</option>
                <option value="markup">{t("Markup on cost")}</option>
                <option value="none">{t("Only protect a minimum margin")}</option>
              </select>
            </Field>
            {mode !== "none" ? (
              <TextInput
                label={mode === "margin" ? t("Target margin %") : t("Markup %")}
                value={pct.target}
                inputMode="decimal"
                onChange={(e) => setPct({ ...pct, target: e.target.value })}
              />
            ) : null}
            <TextInput
              label={t("Minimum margin %")}
              value={pct.min}
              inputMode="decimal"
              onChange={(e) => setPct({ ...pct, min: e.target.value })}
              hint={t("Prices below it are flagged and need approval to apply. Rounding never goes below it.")}
            />
            <div className="form-grid">
              <Field label={t("Round to")}>
                <select
                  className="input"
                  value={edit.rounding_step_minor ?? 5}
                  onChange={(e) => setEdit({ ...edit, rounding_step_minor: Number(e.target.value) })}
                >
                  {STEPS.map((s) => (
                    <option key={s} value={s}>
                      {formatMoney(s)}
                    </option>
                  ))}
                </select>
              </Field>
              <TextInput
                label={t("Preferred ending (optional)")}
                value={pct.ending}
                placeholder="0.950"
                inputMode="decimal"
                onChange={(e) => setPct({ ...pct, ending: e.target.value })}
              />
              <Field label={t("Cost used")}>
                <select
                  className="input"
                  value={edit.cost_basis}
                  onChange={(e) => setEdit({ ...edit, cost_basis: e.target.value as "average" | "last" })}
                >
                  <option value="average">{t("Average cost")}</option>
                  <option value="last">{t("Last cost paid")}</option>
                </select>
              </Field>
              <TextInput
                label={t("Priority")}
                type="number"
                value={edit.priority ?? 0}
                onChange={(e) => setEdit({ ...edit, priority: Number(e.target.value || 0) })}
              />
            </div>
            <Checkbox
              label={t("Policy is on")}
              checked={edit.active ?? true}
              onChange={(v) => setEdit({ ...edit, active: v })}
            />
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          </div>
        </Drawer>
      ) : null}
    </div>
  );
}

// ------------------------------------------------------------------ dashboard cards

export function CommercialCards() {
  const s = useLoad(() => api.pricing.summary(), []);
  const nav = useNavigate();
  const d = s.data;
  if (!d) return null;
  const cards: { label: string; n: number | undefined; to: string; tone: "warning" | "info" }[] = [
    {
      label: t("Prices below minimum margin"),
      n: d.below_min_margin,
      to: "/admin/pricing-review?group=below_min_margin",
      tone: "warning",
    },
    {
      label: t("Price recommendations"),
      n: d.recommendations,
      to: "/admin/pricing-review?group=recommendation",
      tone: "info",
    },
    {
      label: t("Costs changed since the price was set"),
      n: d.cost_changed,
      to: "/admin/pricing-review?group=cost_changed",
      tone: "info",
    },
    { label: t("Likely duplicate products"), n: d.likely_duplicates, to: "/admin/duplicates", tone: "info" },
  ];
  const shown = cards.filter((c) => (c.n ?? 0) > 0);
  if (shown.length === 0) return null;
  return (
    <div className="row gap-12" style={{ flexWrap: "wrap" }} data-testid="commercial-cards">
      {shown.map((c) => (
        <button
          key={c.to}
          className="card pad col gap-4 kpi-card"
          style={{ minWidth: 200, textAlign: "start" }}
          onClick={() => nav(c.to)}
        >
          <span className="tiny muted">{c.label}</span>
          <span style={{ fontSize: 22, fontWeight: 700 }} className={c.tone === "warning" ? "danger-text" : ""}>
            {c.n}
          </span>
        </button>
      ))}
    </div>
  );
}
