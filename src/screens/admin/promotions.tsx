// Wave 6: promotions, coupons and bundles (docs/PROMOTIONS_AND_BUNDLES.md).
// Every figure shown here comes from the backend (coverage, offer prices,
// margins, conflicts, availability). The screen never computes money and
// never switches an offer on by itself: a person does, deliberately.
import { useEffect, useMemo, useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { Gift, Plus, X } from "lucide-react";
import { api } from "../../api";
import type {
  BundleDetail,
  BundleRow,
  OfferAttention,
  Promotion,
  PromotionDetail,
  PromotionInsight,
  PromotionKind,
  PromotionRow,
  PromotionStatus,
} from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { newOperationId } from "../../lib/ids";
import { formatMoney, formatPercent, formatQty, parseMoney, parsePercent } from "../../lib/money";
import { Banner, Button, Checkbox, Chip, Empty, Field, PageHeader, Tabs, TextInput } from "../../components/ui";
import { DataTable, Drawer, useAction, useLoad, type Column } from "./common";
import { channelLabel } from "./commercial";
import { t, tb } from "../../i18n";

// ------------------------------------------------------------------ labels

export function kindLabel(k: PromotionKind): string {
  switch (k) {
    case "percent":
      return t("Percentage off");
    case "amount":
      return t("Amount off each item");
    case "fixed_price":
      return t("Offer price");
    case "quantity":
      return t("Quantity deal (e.g. 3 for 1.000)");
    case "bxgy":
      return t("Buy X, get Y");
    default:
      return t("Spend and save (whole basket)");
  }
}

function stateChip(s: PromotionRow["state"]) {
  switch (s) {
    case "running":
      return <Chip tone="success">{t("Running")}</Chip>;
    case "scheduled":
      return <Chip tone="info">{t("Scheduled")}</Chip>;
    case "finished":
      return <Chip>{t("Past its end date")}</Chip>;
    case "paused":
      return <Chip tone="warning">{t("Paused")}</Chip>;
    case "draft":
      return <Chip>{t("Draft")}</Chip>;
    case "ended":
      return <Chip>{t("Ended")}</Chip>;
    default:
      return <Chip>{t("Archived")}</Chip>;
  }
}

function benefit(p: Promotion): string {
  switch (p.kind) {
    case "percent":
      return t("{0} off", formatPercent(p.percent_bp));
    case "amount":
      return t("{0} off each", formatMoney(p.amount_minor));
    case "fixed_price":
      return t("Now {0}", formatMoney(p.price_minor));
    case "quantity":
      return t("{0} for {1}", p.buy_qty ?? 0, formatMoney(p.price_minor));
    case "bxgy":
      return p.percent_bp && p.percent_bp < 10000
        ? t("Buy {0}, get {1} at {2} off", p.buy_qty ?? 0, p.get_qty ?? 0, formatPercent(p.percent_bp))
        : t("Buy {0}, get {1} free", p.buy_qty ?? 0, p.get_qty ?? 0);
    default:
      return p.percent_bp
        ? t("Spend {0}, save {1}", formatMoney(p.threshold_minor), formatPercent(p.percent_bp))
        : t("Spend {0}, save {1}", formatMoney(p.threshold_minor), formatMoney(p.amount_minor));
  }
}

const schedule = (p: Promotion) =>
  [
    p.starts_at ? t("from {0}", p.starts_at.replace("T", " ")) : null,
    p.ends_at ? t("until {0}", p.ends_at.replace("T", " ")) : null,
  ]
    .filter(Boolean)
    .join(" ") || t("No dates");

const EMPTY: Partial<Promotion> = {
  name: "",
  kind: "percent",
  target: "items",
  status: "draft",
  priority: 0,
  stackable: false,
  requires_coupon: false,
  buy_products: [],
  buy_categories: [],
  get_products: [],
  get_categories: [],
  channels: null,
  version: 0,
};

const CHANNELS = ["pos", "whatsapp", "phone", "web", "other"];

// ------------------------------------------------------------------ promotions

export function PromotionsPage() {
  const { has } = useSession();
  const [tab, setTab] = useState<"all" | "running" | "scheduled" | "draft" | "paused" | "ended">("all");
  const list = useLoad(() => api.promotions.list(tab === "all" ? null : tab), [tab]);
  const [open, setOpen] = useState<Partial<Promotion> | null>(null);
  const [params] = useSearchParams();
  useEffect(() => {
    const id = params.get("open");
    if (id) setOpen({ promotion_id: id });
  }, [params]);
  const canEdit = has("promotions.manage");
  const cols: Column<PromotionRow>[] = [
    {
      key: "name",
      label: t("Offer"),
      render: (r) => (
        <span dir="auto">
          <strong>{r.promotion.name}</strong>
          {r.promotion.requires_coupon ? (
            <>
              {" "}
              <Chip tone="brand">{t("Coupon")}</Chip>
            </>
          ) : null}
        </span>
      ),
    },
    { key: "benefit", label: t("Benefit"), render: (r) => benefit(r.promotion) },
    { key: "when", label: t("When"), render: (r) => schedule(r.promotion) },
    { key: "state", label: t("Status"), render: (r) => stateChip(r.state) },
    { key: "sales", label: t("Sales"), num: true, render: (r) => r.sales },
    { key: "disc", label: t("Discount given"), num: true, render: (r) => formatMoney(r.discount_minor) },
  ];
  return (
    <div className="col gap-16">
      <PageHeader
        title={t("Promotions")}
        subtitle={t("Offers change what a sale pays while they run. They never change a product's normal price.")}
        actions={
          canEdit ? (
            <Button variant="primary" icon={<Plus size={16} />} onClick={() => setOpen({ ...EMPTY })}>
              {t("New offer")}
            </Button>
          ) : null
        }
      />
      <Tabs
        value={tab}
        onChange={setTab}
        tabs={[
          { key: "all", label: t("All") },
          { key: "running", label: t("Running") },
          { key: "scheduled", label: t("Scheduled") },
          { key: "draft", label: t("Draft") },
          { key: "paused", label: t("Paused") },
          { key: "ended", label: t("Ended") },
        ]}
      />
      {list.data && list.data.rows.length === 0 ? (
        <Empty title={t("No offers here.")}>
          {t("Create an offer, check its products and margin, then switch it on.")}
        </Empty>
      ) : (
        <DataTable
          rows={list.data?.rows ?? null}
          loading={list.loading}
          columns={cols}
          rowKey={(r) => r.promotion.promotion_id}
          onRowClick={(r) => setOpen(r.promotion)}
        />
      )}
      {open ? (
        <PromotionEditor
          initial={open}
          canEdit={canEdit}
          onClose={() => setOpen(null)}
          onChanged={() => void list.reload()}
        />
      ) : null}
    </div>
  );
}

function PromotionEditor({
  initial,
  canEdit,
  onClose,
  onChanged,
}: {
  initial: Partial<Promotion>;
  canEdit: boolean;
  onClose: () => void;
  onChanged: () => void;
}) {
  const { has } = useSession();
  const toast = useToast();
  const act = useAction();
  const [p, setP] = useState<Partial<Promotion>>(initial);
  const [detail, setDetail] = useState<PromotionDetail | null>(null);
  const [insight, setInsight] = useState<PromotionInsight | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [num, setNum] = useState({
    pct: initial.percent_bp != null ? formatPercent(initial.percent_bp).replace("%", "") : "",
    amount: initial.amount_minor != null ? formatMoney(initial.amount_minor).replace(/[^0-9.]/g, "") : "",
    price: initial.price_minor != null ? formatMoney(initial.price_minor).replace(/[^0-9.]/g, "") : "",
    threshold: initial.threshold_minor != null ? formatMoney(initial.threshold_minor).replace(/[^0-9.]/g, "") : "",
  });
  const cats = useLoad(() => api.categories.list(), []);
  const [names, setNames] = useState<Record<string, string>>({});
  const id = p.promotion_id;
  useEffect(() => {
    if (!id) return;
    void api.promotions.get(id).then((d) => {
      setDetail(d);
      setP(d.promotion);
      setInsight(d.insight);
      const q = d.promotion;
      const money = (v: number | null) => (v != null ? formatMoney(v).replace(/[^0-9.]/g, "") : "");
      setNum({
        pct: q.percent_bp != null ? formatPercent(q.percent_bp).replace("%", "") : "",
        amount: money(q.amount_minor),
        price: money(q.price_minor),
        threshold: money(q.threshold_minor),
      });
    });
  }, [id]);
  // The offer as the backend would read it.
  const draft: Partial<Promotion> = useMemo(() => {
    const kind = p.kind ?? "percent";
    const usesPct = kind === "percent" || kind === "bxgy" || (kind === "basket" && num.pct.trim() !== "");
    return {
      ...p,
      percent_bp: usesPct && num.pct.trim() ? parsePercent(num.pct) : null,
      amount_minor:
        (kind === "amount" || (kind === "basket" && num.pct.trim() === "")) && num.amount.trim()
          ? parseMoney(num.amount)
          : null,
      price_minor: (kind === "fixed_price" || kind === "quantity") && num.price.trim() ? parseMoney(num.price) : null,
      threshold_minor: kind === "basket" && num.threshold.trim() ? parseMoney(num.threshold) : null,
    };
  }, [p, num]);
  // Coverage, margins and conflicts follow the edit (from the backend).
  useEffect(() => {
    if (!canEdit) return;
    const h = setTimeout(() => {
      void api.promotions
        .preview(draft)
        .then((r) => {
          setInsight(r.insight);
          setProblem(r.problem);
        })
        .catch(() => undefined);
    }, 300);
    return () => clearTimeout(h);
  }, [draft, canEdit]);
  const editable =
    canEdit && (!detail || (detail.promotion.status !== "ended" && detail.promotion.status !== "archived"));
  const save = async () => {
    const r = await act.run(() => api.promotions.save(draft, newOperationId()));
    if (r) {
      toast("success", t("Offer saved. It is not switched on until you switch it on."));
      setDetail(r);
      setP(r.promotion);
      setInsight(r.insight);
      onChanged();
    }
  };
  const move = async (status: PromotionStatus) => {
    if (!detail) return;
    const r = await act.run(() =>
      api.promotions.setStatus(detail.promotion.promotion_id, status, detail.promotion.version, newOperationId()),
    );
    if (r) {
      setDetail(r);
      setP(r.promotion);
      toast("success", t("Offer updated"));
      onChanged();
    }
  };
  const status = detail?.promotion.status ?? "draft";
  const kind = p.kind ?? "percent";
  const nameOf = (pid: string) =>
    names[pid] ?? insight?.margins.find((m) => m.product_id === pid)?.name ?? pid.slice(0, 8);
  return (
    <Drawer
      wide
      title={detail ? detail.promotion.name : t("New offer")}
      onClose={onClose}
      actions={
        <>
          {detail && canEdit ? (
            <>
              {status === "draft" || status === "paused" ? (
                <Button variant="primary" onClick={() => void move("active")} data-testid="promo-switch-on">
                  {status === "draft" ? t("Switch on") : t("Resume")}
                </Button>
              ) : null}
              {status === "active" ? <Button onClick={() => void move("paused")}>{t("Pause")}</Button> : null}
              {status === "active" || status === "paused" ? (
                <Button variant="danger-outline" onClick={() => void move("ended")}>
                  {t("End")}
                </Button>
              ) : null}
              {status === "draft" || status === "paused" || status === "ended" ? (
                <Button onClick={() => void move("archived")}>{t("Archive")}</Button>
              ) : null}
            </>
          ) : null}
          {editable ? (
            <Button variant={detail ? "default" : "primary"} loading={act.busy} onClick={save} data-testid="promo-save">
              {t("Save")}
            </Button>
          ) : null}
        </>
      }
    >
      <div className="col gap-12">
        {detail ? (
          <div className="row gap-8">
            {stateChip(detail.state)}
            <span className="small muted">{t("Store time now: {0}", detail.local_now.replace("T", " "))}</span>
          </div>
        ) : (
          <Banner tone="info">
            {t("A new offer starts as a draft. Reaching its start date does not switch it on.")}
          </Banner>
        )}
        <div className="form-grid">
          <TextInput
            label={t("Name (on receipts)")}
            value={p.name ?? ""}
            disabled={!editable}
            onChange={(e) => setP({ ...p, name: e.target.value })}
            data-testid="promo-name"
          />
          <TextInput
            label={t("Arabic name")}
            value={p.name_ar ?? ""}
            dir="rtl"
            disabled={!editable}
            onChange={(e) => setP({ ...p, name_ar: e.target.value })}
          />
        </div>
        <Field label={t("Type of offer")}>
          <select
            className="input"
            value={kind}
            disabled={!editable}
            data-testid="promo-kind"
            onChange={(e) => setP({ ...p, kind: e.target.value as PromotionKind })}
          >
            {(["percent", "amount", "fixed_price", "quantity", "bxgy", "basket"] as PromotionKind[]).map((k) => (
              <option key={k} value={k}>
                {kindLabel(k)}
              </option>
            ))}
          </select>
        </Field>
        <div className="form-grid">
          {kind === "percent" || kind === "bxgy" || kind === "basket" ? (
            <TextInput
              label={kind === "bxgy" ? t("Reward discount % (100 = free)") : t("Percentage off")}
              value={num.pct}
              inputMode="decimal"
              disabled={!editable}
              data-testid="promo-percent"
              onChange={(e) => setNum({ ...num, pct: e.target.value })}
            />
          ) : null}
          {kind === "amount" || kind === "basket" ? (
            <TextInput
              label={kind === "basket" ? t("Or an amount off") : t("Amount off each item")}
              value={num.amount}
              inputMode="decimal"
              disabled={!editable}
              onChange={(e) => setNum({ ...num, amount: e.target.value })}
            />
          ) : null}
          {kind === "fixed_price" || kind === "quantity" ? (
            <TextInput
              label={kind === "quantity" ? t("Price for the group") : t("Offer price")}
              value={num.price}
              inputMode="decimal"
              disabled={!editable}
              data-testid="promo-price"
              onChange={(e) => setNum({ ...num, price: e.target.value })}
            />
          ) : null}
          {kind === "quantity" || kind === "bxgy" ? (
            <TextInput
              label={kind === "quantity" ? t("How many items") : t("Buy (X)")}
              type="number"
              value={p.buy_qty ?? ""}
              disabled={!editable}
              data-testid="promo-buy-qty"
              onChange={(e) => setP({ ...p, buy_qty: e.target.value ? Number(e.target.value) : null })}
            />
          ) : null}
          {kind === "bxgy" ? (
            <TextInput
              label={t("Get (Y)")}
              type="number"
              value={p.get_qty ?? ""}
              disabled={!editable}
              onChange={(e) => setP({ ...p, get_qty: e.target.value ? Number(e.target.value) : null })}
            />
          ) : null}
          {kind === "quantity" || kind === "bxgy" ? (
            <TextInput
              label={t("At most this many times per sale (optional)")}
              type="number"
              value={p.max_uses ?? ""}
              disabled={!editable}
              onChange={(e) => setP({ ...p, max_uses: e.target.value ? Number(e.target.value) : null })}
            />
          ) : null}
          {kind === "basket" ? (
            <TextInput
              label={t("Minimum spend")}
              value={num.threshold}
              inputMode="decimal"
              disabled={!editable}
              hint={t("Measured on the qualifying items after item offers, before this offer.")}
              onChange={(e) => setNum({ ...num, threshold: e.target.value })}
            />
          ) : null}
        </div>
        <Field label={t("Which products")}>
          <select
            className="input"
            value={p.target ?? "items"}
            disabled={!editable}
            onChange={(e) => setP({ ...p, target: e.target.value as "items" | "all" })}
          >
            <option value="items">{t("Chosen products and categories")}</option>
            <option value="all">{t("Everything in the shop")}</option>
          </select>
        </Field>
        {p.target !== "all" ? (
          <ItemChooser
            label={kind === "bxgy" ? t("Customer buys") : t("Products")}
            products={p.buy_products ?? []}
            categories={p.buy_categories ?? []}
            cats={cats.data ?? []}
            disabled={!editable}
            nameOf={nameOf}
            onNames={(n) => setNames({ ...names, ...n })}
            onChange={(products, categories) => setP({ ...p, buy_products: products, buy_categories: categories })}
          />
        ) : null}
        {kind === "bxgy" ? (
          <ItemChooser
            label={t("Customer gets (empty: from the same items)")}
            products={p.get_products ?? []}
            categories={p.get_categories ?? []}
            cats={cats.data ?? []}
            disabled={!editable}
            nameOf={nameOf}
            onNames={(n) => setNames({ ...names, ...n })}
            onChange={(products, categories) => setP({ ...p, get_products: products, get_categories: categories })}
          />
        ) : null}
        <div className="form-grid">
          <TextInput
            label={t("Starts (store time)")}
            type="datetime-local"
            value={p.starts_at ?? ""}
            disabled={!editable}
            onChange={(e) => setP({ ...p, starts_at: e.target.value || null })}
          />
          <TextInput
            label={t("Ends (store time)")}
            type="datetime-local"
            value={p.ends_at ?? ""}
            disabled={!editable}
            onChange={(e) => setP({ ...p, ends_at: e.target.value || null })}
          />
        </div>
        <Field label={t("Where it applies")} hint={t("No channel ticked: every channel.")}>
          <div className="row gap-12 wrap">
            {CHANNELS.map((c) => (
              <Checkbox
                key={c}
                label={channelLabel(c)}
                disabled={!editable}
                checked={(p.channels ?? []).includes(c)}
                onChange={(v) => {
                  const next = v ? [...(p.channels ?? []), c] : (p.channels ?? []).filter((x) => x !== c);
                  setP({ ...p, channels: next.length ? next : null });
                }}
              />
            ))}
          </div>
        </Field>
        <div className="form-grid">
          <TextInput
            label={t("Priority")}
            type="number"
            value={p.priority ?? 0}
            disabled={!editable}
            hint={t("Higher wins. Equal priority: the better deal for the customer wins.")}
            onChange={(e) => setP({ ...p, priority: Number(e.target.value || 0) })}
          />
          <div className="col gap-8">
            <Checkbox
              label={t("Can combine with other offers")}
              checked={!!p.stackable}
              disabled={!editable}
              onChange={(v) => setP({ ...p, stackable: v })}
            />
            <Checkbox
              label={t("Needs a coupon code")}
              checked={!!p.requires_coupon}
              disabled={!editable}
              onChange={(v) => setP({ ...p, requires_coupon: v })}
            />
          </div>
        </div>
        {problem && editable ? <Banner tone="warning">{tb(problem)}</Banner> : null}
        {insight ? <InsightPanel insight={insight} /> : null}
        {detail && detail.promotion.requires_coupon && has("coupons.manage") ? (
          <CouponsCard detail={detail} onChanged={(d) => (setDetail(d), onChanged())} />
        ) : null}
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      </div>
    </Drawer>
  );
}

function ItemChooser({
  label,
  products,
  categories,
  cats,
  disabled,
  nameOf,
  onNames,
  onChange,
}: {
  label: string;
  products: string[];
  categories: string[];
  cats: { category_id: string; name: string }[];
  disabled: boolean;
  nameOf: (id: string) => string;
  onNames: (n: Record<string, string>) => void;
  onChange: (products: string[], categories: string[]) => void;
}) {
  const [q, setQ] = useState("");
  const found = useLoad(
    () => (q.trim().length >= 2 ? api.products.search({ q, limit: 8 }) : Promise.resolve(null)),
    [q],
  );
  return (
    <Field label={label}>
      <div className="col gap-8">
        <div className="row gap-8 wrap">
          {products.map((pid) => (
            <span key={pid} className="chip brand">
              {nameOf(pid)}
              {!disabled ? (
                <button
                  type="button"
                  className="chip-x"
                  aria-label={t("Remove {0}", nameOf(pid))}
                  onClick={() =>
                    onChange(
                      products.filter((x) => x !== pid),
                      categories,
                    )
                  }
                >
                  <X size={14} />
                </button>
              ) : null}
            </span>
          ))}
          {categories.map((cid) => (
            <span key={cid} className="chip info">
              {t("Category: {0}", cats.find((c) => c.category_id === cid)?.name ?? "?")}
              {!disabled ? (
                <button
                  type="button"
                  className="chip-x"
                  aria-label={t("Remove")}
                  onClick={() =>
                    onChange(
                      products,
                      categories.filter((x) => x !== cid),
                    )
                  }
                >
                  <X size={14} />
                </button>
              ) : null}
            </span>
          ))}
        </div>
        {!disabled ? (
          <div className="form-grid">
            <TextInput
              label={t("Add a product")}
              value={q}
              placeholder={t("Name or barcode")}
              data-testid="promo-product-search"
              onChange={(e) => setQ(e.target.value)}
            />
            <Field label={t("Add a category")}>
              <select
                className="input"
                value=""
                onChange={(e) => e.target.value && onChange(products, [...new Set([...categories, e.target.value])])}
              >
                <option value="">{t("Choose…")}</option>
                {cats.map((c) => (
                  <option key={c.category_id} value={c.category_id}>
                    {c.name}
                  </option>
                ))}
              </select>
            </Field>
          </div>
        ) : null}
        {(found.data?.rows ?? []).map((r) => (
          <button
            key={r.product_id}
            type="button"
            className="list-row"
            style={{ textAlign: "start", padding: 8 }}
            onClick={() => {
              onNames({ [r.product_id]: r.name });
              onChange([...new Set([...products, r.product_id])], categories);
              setQ("");
            }}
          >
            <strong dir="auto">{r.name}</strong> <span className="tiny">{r.sku}</span>
          </button>
        ))}
      </div>
    </Field>
  );
}

function InsightPanel({ insight }: { insight: PromotionInsight }) {
  return (
    <div className="card col gap-8" data-testid="promo-insight">
      <div className="row gap-8 wrap">
        <strong>{t("{0} products covered", insight.coverage)}</strong>
        {insight.below_cost > 0 ? <Chip tone="danger">{t("{0} below cost", insight.below_cost)}</Chip> : null}
        {insight.negative_margin > 0 && insight.below_cost === 0 ? (
          <Chip tone="warning">{t("{0} with negative margin", insight.negative_margin)}</Chip>
        ) : null}
      </div>
      {insight.coverage === 0 ? (
        <Banner tone="warning">{t("No product is covered: this offer would never apply.")}</Banner>
      ) : null}
      {insight.below_cost > 0 ? (
        <Banner tone="warning">
          {t("Some items would sell below cost. The offer is not changed for you; adjust it if that is not intended.")}
        </Banner>
      ) : null}
      {insight.conflicts.length ? (
        <Banner tone="info">
          {t("Overlaps with: {0}", insight.conflicts.map((c) => `${c.name} (${relation(c.relation)})`).join(", "))}
        </Banner>
      ) : null}
      {insight.margins.length ? (
        <table className="table compact">
          <thead>
            <tr>
              <th>{t("Product")}</th>
              <th className="num">{t("Normal")}</th>
              <th className="num">{t("Offer")}</th>
              {insight.show_cost ? <th className="num">{t("Margin")}</th> : null}
            </tr>
          </thead>
          <tbody>
            {insight.margins.slice(0, 20).map((m) => (
              <tr key={m.product_id}>
                <td dir="auto">{m.name}</td>
                <td className="num money">{formatMoney(m.normal_minor)}</td>
                <td className="num money">{formatMoney(m.offer_minor)}</td>
                {insight.show_cost ? (
                  <td className="num">
                    {m.below_cost ? <Chip tone="danger">{t("Below cost")}</Chip> : formatPercent(m.margin_bp ?? null)}
                  </td>
                ) : null}
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}
    </div>
  );
}

function relation(r: "higher" | "lower" | "same"): string {
  return r === "higher" ? t("higher priority") : r === "lower" ? t("lower priority") : t("same priority");
}

function CouponsCard({ detail, onChanged }: { detail: PromotionDetail; onChanged: (d: PromotionDetail) => void }) {
  const act = useAction();
  const toast = useToast();
  const [code, setCode] = useState("");
  const [limited, setLimited] = useState(false);
  const [max, setMax] = useState("1");
  const add = async () => {
    const r = await act.run(() =>
      api.promotions.saveCoupon({
        promotion_id: detail.promotion.promotion_id,
        code,
        kind: limited ? "limited" : "reusable",
        max_redemptions: limited ? Number(max) : null,
        operation_id: newOperationId(),
      }),
    );
    if (r) {
      toast("success", t("Coupon code added"));
      setCode("");
      onChanged(r);
    }
  };
  return (
    <div className="card col gap-8" data-testid="coupons-card">
      <strong>{t("Coupon codes")}</strong>
      <p className="small muted">
        {t(
          "Reusable codes work on every till, even offline. Limited codes are counted on the main computer only; a till without it says so and the sale goes on at the normal price.",
        )}
      </p>
      {detail.coupons.map((c) => (
        <div key={c.coupon_id} className="row gap-8">
          <strong className="num">{c.code}</strong>
          <Chip>{c.kind === "limited" ? t("Limited: {0} uses", c.max_redemptions ?? 0) : t("Reusable")}</Chip>
          <span className="small">{t("Used {0} times", c.redemptions)}</span>
          <span className="grow" />
          <Button
            size="sm"
            onClick={async () => {
              const r = await act.run(() =>
                api.promotions.saveCoupon({
                  coupon_id: c.coupon_id,
                  promotion_id: detail.promotion.promotion_id,
                  code: c.code,
                  kind: c.kind,
                  active: !c.active,
                  version: c.version,
                  operation_id: newOperationId(),
                }),
              );
              if (r) onChanged(r);
            }}
          >
            {c.active ? t("Switch off") : t("Switch on")}
          </Button>
        </div>
      ))}
      <div className="form-grid">
        <TextInput
          label={t("New code")}
          value={code}
          onChange={(e) => setCode(e.target.value)}
          data-testid="coupon-new-code"
        />
        <div className="col gap-8">
          <Checkbox label={t("Limited number of uses")} checked={limited} onChange={setLimited} />
          {limited ? (
            <TextInput label={t("Uses in total")} type="number" value={max} onChange={(e) => setMax(e.target.value)} />
          ) : null}
        </div>
      </div>
      <Button onClick={add} disabled={!code.trim()} loading={act.busy}>
        {t("Add code")}
      </Button>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
    </div>
  );
}

// ------------------------------------------------------------------ bundles

export function BundlesPage() {
  const { has } = useSession();
  const list = useLoad(() => api.bundles.list(), []);
  const [params] = useSearchParams();
  const [open, setOpen] = useState<string | "new" | null>(params.get("open"));
  const canEdit = has("bundles.manage");
  const cols: Column<BundleRow>[] = [
    { key: "name", label: t("Bundle"), render: (b) => <strong dir="auto">{b.name}</strong> },
    { key: "price", label: t("Price"), num: true, render: (b) => formatMoney(b.price_minor) },
    {
      key: "available",
      label: t("Can make now"),
      num: true,
      render: (b) =>
        b.available === null ? t("No stock limit") : b.available === 0 ? <Chip tone="danger">0</Chip> : b.available,
    },
    { key: "limit", label: t("Limited by"), render: (b) => b.limited_by ?? "—" },
    {
      key: "on",
      label: t("Status"),
      render: (b) => (b.active ? <Chip tone="success">{t("On")}</Chip> : <Chip>{t("Off")}</Chip>),
    },
  ];
  return (
    <div className="col gap-16">
      <PageHeader
        title={t("Bundles and hampers")}
        subtitle={t("A bundle is made when it is sold: its items leave stock then. It has no stock of its own.")}
        actions={
          canEdit ? (
            <Button variant="primary" icon={<Gift size={16} />} onClick={() => setOpen("new")}>
              {t("New bundle")}
            </Button>
          ) : null
        }
      />
      {list.data && list.data.rows.length === 0 ? (
        <Empty title={t("No bundles yet.")}>
          {t("Create the bundle as a product first (its name and price), then choose its items here.")}
        </Empty>
      ) : (
        <DataTable
          rows={list.data?.rows ?? null}
          loading={list.loading}
          columns={cols}
          rowKey={(b) => b.bundle_product_id}
          onRowClick={(b) => setOpen(b.bundle_product_id)}
        />
      )}
      {open ? (
        <BundleEditor
          id={open === "new" ? null : open}
          canEdit={canEdit}
          onClose={() => setOpen(null)}
          onChanged={() => void list.reload()}
        />
      ) : null}
    </div>
  );
}

function BundleEditor({
  id,
  canEdit,
  onClose,
  onChanged,
}: {
  id: string | null;
  canEdit: boolean;
  onClose: () => void;
  onChanged: () => void;
}) {
  const toast = useToast();
  const act = useAction();
  const [detail, setDetail] = useState<BundleDetail | null>(null);
  const [parent, setParent] = useState<{ id: string; name: string } | null>(null);
  const [items, setItems] = useState<{ product_id: string; name: string; qty: string }[]>([]);
  const [active, setActive] = useState(true);
  const [q, setQ] = useState("");
  const [pick, setPick] = useState<"parent" | "item">("item");
  const found = useLoad(
    () => (q.trim().length >= 2 ? api.products.search({ q, limit: 8 }) : Promise.resolve(null)),
    [q],
  );
  useEffect(() => {
    if (!id) return;
    void api.bundles.get(id).then((d) => {
      setDetail(d);
      setParent({ id: d.bundle_product_id, name: d.name });
      setActive(d.active);
      setItems(d.components.map((c) => ({ product_id: c.product_id, name: c.name, qty: formatQty(c.qty_milli) })));
    });
  }, [id]);
  const save = async () => {
    if (!parent) return;
    const r = await act.run(() =>
      api.bundles.save({
        bundle_product_id: parent.id,
        components: items.map((i) => ({ product_id: i.product_id, qty_milli: Math.round(Number(i.qty || 0) * 1000) })),
        active,
        version: detail?.version ?? 0,
        operation_id: newOperationId(),
      }),
    );
    if (r) {
      setDetail(r);
      toast("success", t("Bundle saved"));
      onChanged();
    }
  };
  return (
    <Drawer
      wide
      title={detail ? detail.name : t("New bundle")}
      onClose={onClose}
      actions={
        canEdit ? (
          <Button
            variant="primary"
            loading={act.busy}
            disabled={!parent || !items.length}
            onClick={save}
            data-testid="bundle-save"
          >
            {t("Save")}
          </Button>
        ) : null
      }
    >
      <div className="col gap-12">
        {detail ? (
          <div className="card col gap-8" data-testid="bundle-summary">
            <div className="row gap-12 wrap">
              <span>{t("Price {0}", formatMoney(detail.price_minor))}</span>
              <span>{t("Items normally {0}", formatMoney(detail.normal_minor))}</span>
              {detail.saving_minor ? (
                <Chip tone="success">{t("Saves {0}", formatMoney(detail.saving_minor))}</Chip>
              ) : null}
              {detail.margin_bp != null ? (
                <Chip tone={detail.margin_bp < 0 ? "danger" : "default"}>
                  {t("Margin {0}", formatPercent(detail.margin_bp))}
                </Chip>
              ) : null}
            </div>
            <span>
              {detail.available === null
                ? t("No item is stock-tracked.")
                : t("Stock can make {0} now (limited by {1}).", detail.available, detail.limited_by ?? "—")}
            </span>
            <span className="small muted">{t("Version {0}. Sales keep the version they sold.", detail.version)}</span>
          </div>
        ) : null}
        <Field label={t("Bundle product")} hint={t("Its stock tracking is switched off: the items carry the stock.")}>
          {parent ? (
            <strong dir="auto">{parent.name}</strong>
          ) : (
            <Button onClick={() => setPick("parent")}>{t("Choose the bundle product below")}</Button>
          )}
        </Field>
        <table className="table compact">
          <thead>
            <tr>
              <th>{t("Item")}</th>
              <th className="num">{t("Quantity in one bundle")}</th>
              {detail ? <th className="num">{t("In stock")}</th> : null}
              <th />
            </tr>
          </thead>
          <tbody>
            {items.map((i, k) => (
              <tr key={i.product_id}>
                <td dir="auto">{i.name}</td>
                <td className="num">
                  <input
                    className="input"
                    style={{ width: 80 }}
                    value={i.qty}
                    disabled={!canEdit}
                    inputMode="numeric"
                    onChange={(e) => setItems(items.map((x, j) => (j === k ? { ...x, qty: e.target.value } : x)))}
                  />
                </td>
                {detail ? (
                  <td className="num">
                    {(() => {
                      const c = detail.components.find((x) => x.product_id === i.product_id);
                      return c?.stock_milli != null ? formatQty(c.stock_milli) : "—";
                    })()}
                  </td>
                ) : null}
                <td>
                  {canEdit ? (
                    <button
                      type="button"
                      className="chip-x"
                      aria-label={t("Remove {0}", i.name)}
                      onClick={() => setItems(items.filter((_, j) => j !== k))}
                    >
                      <X size={14} />
                    </button>
                  ) : null}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {canEdit ? (
          <>
            <div className="form-grid">
              <Field label={t("Search for")}>
                <select className="input" value={pick} onChange={(e) => setPick(e.target.value as "parent" | "item")}>
                  {!detail ? <option value="parent">{t("The bundle product")}</option> : null}
                  <option value="item">{t("An item in the bundle")}</option>
                </select>
              </Field>
              <TextInput
                label={t("Product")}
                value={q}
                placeholder={t("Name or barcode")}
                data-testid="bundle-search"
                onChange={(e) => setQ(e.target.value)}
              />
            </div>
            {(found.data?.rows ?? []).map((r) => (
              <button
                key={r.product_id}
                type="button"
                className="list-row"
                style={{ textAlign: "start", padding: 8 }}
                onClick={() => {
                  if (pick === "parent" && !detail) {
                    setParent({ id: r.product_id, name: r.name });
                    setPick("item");
                  } else if (!items.some((x) => x.product_id === r.product_id)) {
                    setItems([...items, { product_id: r.product_id, name: r.name, qty: "1" }]);
                  }
                  setQ("");
                }}
              >
                <strong dir="auto">{r.name}</strong> <span className="tiny">{r.sku}</span>
              </button>
            ))}
            <Checkbox label={t("Bundle is on sale")} checked={active} onChange={setActive} />
          </>
        ) : null}
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      </div>
    </Drawer>
  );
}

// ------------------------------------------------------------------ dashboard

function attentionText(a: OfferAttention): string {
  switch (a.kind) {
    case "below_cost":
      return t("{0}: {1} items would sell below cost", a.name, a.count ?? 0);
    case "no_products":
      return t("{0} starts soon but covers no products", a.name);
    case "ending_soon":
      return t("{0} ends {1}", a.name, (a.at ?? "").replace("T", " "));
    case "not_switched_on":
      return t("{0} starts {1} but is still a draft", a.name, (a.at ?? "").replace("T", " "));
    case "coupon_used_up":
      return t("Coupon {0} is used up", a.name);
    default:
      return t("{0} cannot be made from stock (short of {1})", a.name, a.limited_by ?? "?");
  }
}

/** Offers, coupons and bundles that need a person now; each opens its screen. */
export function OfferAttentionCard() {
  const r = useLoad(() => api.promotions.attention(), []);
  const nav = useNavigate();
  const items = r.data?.items ?? [];
  if (!items.length) return null;
  return (
    <div className="card pad col gap-8" data-testid="offer-attention">
      <strong>{t("Offers and bundles to check")}</strong>
      {items.slice(0, 8).map((a, i) => (
        <button
          key={i}
          type="button"
          className="list-row"
          style={{ textAlign: "start", padding: 8 }}
          onClick={() => nav(a.link)}
        >
          <Chip tone={a.kind === "below_cost" || a.kind === "bundle_unavailable" ? "danger" : "warning"}>
            {t("Check")}
          </Chip>{" "}
          <span dir="auto">{attentionText(a)}</span>
        </button>
      ))}
    </div>
  );
}
