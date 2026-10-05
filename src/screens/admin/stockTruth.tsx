// Batches and expiry, waste, and days of stock left. Stock is one truth (the
// movements); batches are worked out from it. "Sold" from a batch is an
// estimate (first expiring first out) unless someone recorded the batch, and
// the screen says so. Expired is a condition: the stock stays until someone
// records what happened to it.
import { useMemo, useState } from "react";
import { AlertTriangle, Layers, Plus, Trash2 } from "lucide-react";
import { api } from "../../api";
import { ApiError } from "../../api/transport";
import type { CoverRow, ExpiryRow, ExpiryStatus, LotDetail, WasteRow } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { useApproval, ApprovalCancelled } from "../../components/approval";
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
import { DataTable, Drawer, useAction, useLoad } from "./common";
import { formatMoney, formatQty, parseQty } from "../../lib/money";
import { formatDate, formatDateTime } from "../../lib/time";
import { newOperationId } from "../../lib/ids";
import { getLang, t, tb } from "../../i18n";

export const WASTE_REASONS: [string, () => string][] = [
  ["expired", () => t("Expired")],
  ["damaged", () => t("Damaged")],
  ["spoiled", () => t("Spoiled")],
  ["broken", () => t("Broken")],
  ["shrinkage", () => t("Shrinkage / unexplained difference")],
  ["internal_use", () => t("Used in the shop")],
  ["receiving_rejection", () => t("Rejected at receiving")],
  ["other", () => t("Other")],
];
export const reasonLabel = (r: string) => WASTE_REASONS.find((x) => x[0] === r)?.[1]() ?? r;

const productName = (r: {
  product_name?: string;
  name?: string;
  product_name_ar?: string | null;
  name_ar?: string | null;
}) => (getLang() === "ar" ? (r.product_name_ar ?? r.name_ar) : null) || r.product_name || r.name || "";

/** "Expires in 6 days", "Expired 2 days ago", "Best before in 3 days"… */
export function expiryWords(status: ExpiryStatus | undefined, days: number | null | undefined, kind: string | null) {
  if (status === "depleted") return t("Used up");
  if (days === null || days === undefined) return t("No date");
  const bb = kind === "best_before";
  if (days < 0) return bb ? t("Past best-before by {0} days", -days) : t("Expired {0} days ago", -days);
  if (days === 0) return bb ? t("Best before today") : t("Expires today");
  return bb ? t("Best before in {0} days", days) : t("Expires in {0} days", days);
}

export function expiryTone(status: ExpiryStatus | undefined): "danger" | "warning" | "info" | "default" | "success" {
  switch (status) {
    case "expired":
    case "past_best_before":
      return "danger";
    case "urgent":
      return "warning";
    case "soon":
      return "info";
    default:
      return "default";
  }
}

const groupOf = (s: ExpiryStatus) =>
  s === "expired" || s === "past_best_before" ? "expired" : s === "urgent" ? "urgent" : s === "soon" ? "soon" : "later";

type Group = "expired" | "urgent" | "soon" | "later";

export function ExpiryPage() {
  const { has } = useSession();
  const data = useLoad(() => api.expiry.overview(), []);
  const [group, setGroup] = useState<Group>("urgent");
  const [lot, setLot] = useState<string | null>(null);
  const [waste, setWaste] = useState<ExpiryRow | null>(null);
  const d = data.data;
  const counts = useMemo(() => {
    const c = { expired: 0, urgent: 0, soon: 0, later: 0 };
    d?.rows.forEach((r) => (c[groupOf(r.status)] += 1));
    return c;
  }, [d]);
  const rows = (d?.rows ?? []).filter((r) => groupOf(r.status) === group);
  return (
    <div>
      <PageHeader
        title={t("Expiry")}
        subtitle={t(
          "Batches with stock, by how soon they expire. Expired stock stays on the shelf until you record what happened to it.",
        )}
      />
      {data.error ? <Banner tone="danger">{data.error}</Banner> : null}
      {!d ? (
        <Skeleton rows={6} />
      ) : (
        <div className="col gap-16">
          <div className="kpis">
            <div className="card kpi" data-testid="expired-on-hand">
              <div className="k-label">{t("Expired, still in stock")}</div>
              <div className="k-value num">{formatQty(d.expired_on_hand_milli)}</div>
              {d.expired_on_hand_minor !== null ? (
                <div className="tiny">{formatMoney(d.expired_on_hand_minor)}</div>
              ) : null}
            </div>
            {d.buckets
              .filter((b) => b.days === 7 || b.days === 30 || b.days === 90)
              .map((b) => (
                <div key={b.days} className="card kpi">
                  <div className="k-label">{t("Expiring within {0} days", b.days)}</div>
                  <div className="k-value num">{formatQty(b.qty_milli)}</div>
                  {b.value_minor !== null ? <div className="tiny">{formatMoney(b.value_minor)}</div> : null}
                </div>
              ))}
            {d.at_risk_minor !== null ? (
              <div className="card kpi">
                <div className="k-label">{t("Likely left at expiry")}</div>
                <div className="k-value money">{formatMoney(d.at_risk_minor)}</div>
                <div className="tiny">{t("At risk, not lost yet")}</div>
              </div>
            ) : null}
            {d.expired_waste_this_month_minor !== null ? (
              <div className="card kpi">
                <div className="k-label">{t("Thrown away as expired this month")}</div>
                <div className="k-value money">{formatMoney(d.expired_waste_this_month_minor)}</div>
                <div className="tiny">{t("Already lost")}</div>
              </div>
            ) : null}
          </div>
          <Tabs<Group>
            value={group}
            onChange={setGroup}
            tabs={[
              { key: "expired", label: `${t("Expired")} (${counts.expired})` },
              { key: "urgent", label: `${t("Urgent")} (${counts.urgent})` },
              { key: "soon", label: `${t("Soon")} (${counts.soon})` },
              { key: "later", label: `${t("Later")} (${counts.later})` },
            ]}
          />
          <DataTable<ExpiryRow>
            rows={rows}
            loading={data.loading}
            rowKey={(r) => r.lot_id}
            onRowClick={(r) => setLot(r.lot_id)}
            empty={
              d.rows.length === 0 ? (
                <Empty title={t("No batch-tracked stock yet")}>
                  {t("Batches appear when stock is received with a batch code or an expiry date.")}
                </Empty>
              ) : (
                <Empty title={t("Nothing here")}>{t("Nothing in this group is nearing expiry.")}</Empty>
              )
            }
            columns={[
              {
                key: "p",
                label: t("Product"),
                render: (r) => (
                  <span>
                    <span dir="auto">{productName(r)}</span>{" "}
                    <span className="tiny" dir="ltr">
                      {r.lot_number}
                      {r.supplier_lot_code ? ` · ${r.supplier_lot_code}` : ""}
                    </span>
                  </span>
                ),
              },
              {
                key: "e",
                label: t("Expiry"),
                render: (r) => (
                  <Chip tone={expiryTone(r.status)}>{expiryWords(r.status, r.days_left, r.expiry_kind)}</Chip>
                ),
                sort: (r) => r.expires_on ?? "9999",
              },
              { key: "q", label: t("Left"), render: (r) => formatQty(r.balance_milli), num: true },
              {
                key: "v",
                label: t("Sells a day"),
                render: (r) => (r.demand_state === "ok" ? formatQty(r.per_day_milli) : "—"),
                num: true,
              },
              {
                key: "l",
                label: t("Likely left at expiry"),
                render: (r) => (r.likely_left_milli > 0 ? <strong>{formatQty(r.likely_left_milli)}</strong> : "0"),
                num: true,
              },
              ...(has("products.view_cost")
                ? [
                    {
                      key: "c",
                      label: t("Value"),
                      render: (r: ExpiryRow) => formatMoney(r.value_minor ?? 0),
                      num: true,
                    },
                  ]
                : []),
              { key: "w", label: t("Where"), render: (r) => <span dir="auto">{r.location_name ?? ""}</span> },
              {
                key: "a",
                label: "",
                render: (r) =>
                  has("waste.record") ? (
                    <Button
                      size="sm"
                      variant="ghost"
                      icon={<Trash2 size={14} />}
                      onClick={(e) => {
                        e.stopPropagation();
                        setWaste(r);
                      }}
                    >
                      {t("Record waste")}
                    </Button>
                  ) : null,
              },
            ]}
          />
        </div>
      )}
      {lot ? <LotDrawer id={lot} onClose={() => setLot(null)} onChanged={() => void data.reload()} /> : null}
      {waste ? (
        <RecordWasteDialog
          productId={waste.product_id}
          productName={productName(waste)}
          lotId={waste.lot_id}
          lotLabel={waste.lot_number}
          defaultReason={groupOf(waste.status) === "expired" ? "expired" : "damaged"}
          onClose={() => setWaste(null)}
          onDone={() => {
            setWaste(null);
            void data.reload();
          }}
        />
      ) : null}
    </div>
  );
}

export function LotDrawer({ id, onClose, onChanged }: { id: string; onClose: () => void; onChanged?: () => void }) {
  const { has } = useSession();
  const { data, error, setData } = useLoad<LotDetail>(() => api.lots.get(id), [id]);
  const [fixing, setFixing] = useState(false);
  const [wasting, setWasting] = useState(false);
  const exp = useLoad(() => api.expiry.overview(data ? { product_id: data.product_id } : {}), [data?.product_id]);
  const row = exp.data?.rows.find((r) => r.lot_id === id);
  return (
    <Drawer wide title={data ? `${data.lot_number} · ${data.product_name}` : t("Batch")} onClose={onClose}>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {!data ? (
        <Skeleton rows={6} />
      ) : (
        <div className="col gap-16" data-testid="lot-drawer">
          <div className="row wrap gap-8">
            <Chip tone={expiryTone(row?.status)}>{expiryWords(row?.status, row?.days_left, data.expiry_kind)}</Chip>
            {data.expiry_source === "document" ? <Chip>{t("Date read from the supplier document")}</Chip> : null}
            {data.corrected ? <Chip tone="info">{t("Corrected")}</Chip> : null}
          </div>
          <table className="table">
            <tbody>
              <tr>
                <td className="tiny">{t("Batch code")}</td>
                <td dir="auto">{data.supplier_lot_code ?? "—"}</td>
              </tr>
              <tr>
                <td className="tiny">{data.expiry_kind === "best_before" ? t("Best before") : t("Expiry date")}</td>
                <td>{data.expires_on ? formatDate(data.expires_on) : "—"}</td>
              </tr>
              <tr>
                <td className="tiny">{t("Supplier")}</td>
                <td dir="auto">{data.supplier_name ?? "—"}</td>
              </tr>
              <tr>
                <td className="tiny">{t("Received")}</td>
                <td>{formatDateTime(data.received_at)}</td>
              </tr>
              {data.unit_cost_minor !== null ? (
                <tr>
                  <td className="tiny">{t("Unit cost")}</td>
                  <td>{formatMoney(data.unit_cost_minor)}</td>
                </tr>
              ) : null}
            </tbody>
          </table>
          <div className="card card-pad">
            <h3>{t("How much is left")}</h3>
            <table className="table">
              <tbody>
                <tr>
                  <td>{data.provenance === "count" ? t("Counted into this batch") : t("Received")}</td>
                  <td className="num">{formatQty(data.in_milli)}</td>
                </tr>
                <tr>
                  <td>{t("Recorded out (waste, counts)")}</td>
                  <td className="num">{formatQty(-data.explicit_out_milli)}</td>
                </tr>
                <tr>
                  <td>
                    {t("Estimated sold")}{" "}
                    <span className="tiny">{t("(first expiring first out — the till does not know the batch)")}</span>
                  </td>
                  <td className="num">{formatQty(-data.estimated_out_milli)}</td>
                </tr>
                <tr>
                  <td>
                    <strong>{t("Left")}</strong>
                  </td>
                  <td className="num">
                    <strong>{formatQty(data.balance_milli)}</strong>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          {row && row.markdowns.length ? (
            <div className="card card-pad col gap-8" data-testid="markdowns">
              <h3>{t("If you reduce the price")}</h3>
              <div className="tiny">
                {row.demand_state === "ok"
                  ? t(
                      "At {0} a day, about {1} may be left when it expires. These are options only; nothing changes until you change the price on the product.",
                      formatQty(row.per_day_milli),
                      formatQty(row.likely_left_milli),
                    )
                  : t("Not enough recent sales to say how much will be left. These are options only.")}
              </div>
              <table className="table">
                <tbody>
                  {row.markdowns.map((m) => (
                    <tr key={m.percent_off}>
                      <td>{t("{0}% off", m.percent_off)}</td>
                      <td className="num money">{formatMoney(m.price_minor)}</td>
                      <td className="num">
                        {m.below_cost ? (
                          <Chip tone="danger">{t("Below cost")}</Chip>
                        ) : (
                          <span className="tiny">{t("Margin {0}", formatMoney(m.margin_minor))}</span>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
              {has("prices.manage") ? (
                <div>
                  <a className="btn sm" href={`#/admin/products/${data.product_id}`}>
                    {t("Open the product to change its price")}
                  </a>
                </div>
              ) : null}
            </div>
          ) : null}
          <div className="card card-pad col gap-8">
            <h3>{t("History")}</h3>
            <table className="table">
              <tbody>
                {data.movements.map((m, i) => (
                  <tr key={i}>
                    <td className="nowrap tiny">{formatDateTime(m.at)}</td>
                    <td dir="auto">{tb(m.reason ?? "") || m.kind}</td>
                    <td className="num">{formatQty(m.qty_milli)}</td>
                    <td className="tiny">{m.user_name ?? ""}</td>
                  </tr>
                ))}
                {data.corrections.map((c, i) => (
                  <tr key={`c${i}`}>
                    <td className="nowrap tiny">{formatDateTime(c.at)}</td>
                    <td dir="auto">
                      {t("Corrected")}: {c.expires_on ? formatDate(c.expires_on) : ""} {c.supplier_lot_code ?? ""} —{" "}
                      {c.reason}
                    </td>
                    <td />
                    <td className="tiny">{c.user_name ?? ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="row gap-8">
            {has("waste.record") && data.balance_milli > 0 ? (
              <Button icon={<Trash2 size={16} />} onClick={() => setWasting(true)}>
                {t("Record waste")}
              </Button>
            ) : null}
            {has("lots.manage") ? <Button onClick={() => setFixing(true)}>{t("Correct details")}</Button> : null}
          </div>
        </div>
      )}
      {fixing && data ? (
        <CorrectLotDialog
          lot={data}
          onClose={() => setFixing(false)}
          onDone={(l) => {
            setData(l);
            setFixing(false);
            onChanged?.();
          }}
        />
      ) : null}
      {wasting && data ? (
        <RecordWasteDialog
          productId={data.product_id}
          productName={data.product_name}
          lotId={data.lot_id}
          lotLabel={data.lot_number}
          defaultReason={row && groupOf(row.status) === "expired" ? "expired" : "damaged"}
          onClose={() => setWasting(false)}
          onDone={() => {
            setWasting(false);
            onChanged?.();
            void api.lots.get(id).then(setData);
          }}
        />
      ) : null}
    </Drawer>
  );
}

/** Date warnings from the backend (impossible dates are refused outright). */
export function dateWarnings(e: unknown): string[] | null {
  return e instanceof ApiError && e.details?.kind === "lot_date_warnings" ? (e.details.warnings as string[]) : null;
}

function CorrectLotDialog({
  lot,
  onClose,
  onDone,
}: {
  lot: LotDetail;
  onClose: () => void;
  onDone: (l: LotDetail) => void;
}) {
  const [code, setCode] = useState(lot.supplier_lot_code ?? "");
  const [exp, setExp] = useState(lot.expires_on ?? "");
  const [reason, setReason] = useState("");
  const [warnings, setWarnings] = useState<string[] | null>(null);
  const [opId] = useState(newOperationId);
  const act = useAction();
  const save = async (confirm: boolean) => {
    try {
      const r = await api.lots.correct({
        lot_id: lot.lot_id,
        supplier_lot_code: code !== (lot.supplier_lot_code ?? "") ? code : null,
        expires_on: exp && exp !== lot.expires_on ? exp : null,
        reason,
        operation_id: opId,
        confirm_warnings: confirm,
      });
      onDone(r);
    } catch (e) {
      const w = dateWarnings(e);
      if (w) setWarnings(w);
      else await act.run(() => Promise.reject(e));
    }
  };
  return (
    <Modal
      title={t("Correct batch {0}", lot.lot_number)}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button variant="primary" disabled={!reason.trim()} onClick={() => void save(!!warnings)}>
            {warnings ? t("Keep these dates") : t("Save")}
          </Button>
        </>
      }
    >
      <div className="col gap-12">
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        {warnings ? (
          <Banner tone="warning" title={t("Check the dates")}>
            {warnings.map((w) => (
              <div key={w}>{tb(w)}</div>
            ))}
          </Banner>
        ) : null}
        <TextInput label={t("Batch code")} value={code} dir="auto" onChange={(e) => setCode(e.target.value)} />
        <Field label={t("Expiry date")}>
          <input type="date" className="input" value={exp} onChange={(e) => setExp(e.target.value)} />
        </Field>
        <TextInput label={t("Why")} value={reason} dir="auto" onChange={(e) => setReason(e.target.value)} />
        <div className="tiny">{t("The original details stay in the history; the correction applies from now on.")}</div>
      </div>
    </Modal>
  );
}

export function RecordWasteDialog({
  productId,
  productName: name,
  lotId,
  lotLabel,
  defaultReason,
  onClose,
  onDone,
}: {
  productId: string;
  productName: string;
  lotId?: string | null;
  lotLabel?: string | null;
  defaultReason?: string;
  onClose: () => void;
  onDone: (w: WasteRow) => void;
}) {
  const toast = useToast();
  const approval = useApproval();
  const [qty, setQty] = useState("1");
  const [reason, setReason] = useState(defaultReason ?? "damaged");
  const [note, setNote] = useState("");
  const [opId] = useState(newOperationId);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const q = parseQty(qty);
  const submit = async () => {
    if (!q || q <= 0) return;
    setBusy(true);
    setError(null);
    try {
      const w = await approval((tok) =>
        api.waste.record({
          product_id: productId,
          lot_id: lotId ?? null,
          qty_milli: q,
          reason,
          note: note.trim() || null,
          operation_id: opId,
          approval_token: tok,
        }),
      );
      toast("success", t("Waste recorded"), `${w.waste_number}`);
      onDone(w);
    } catch (e) {
      if (!(e instanceof ApprovalCancelled)) setError(e instanceof Error ? tb(e.message) : String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title={t("Record waste")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button
            variant="primary"
            loading={busy}
            disabled={!q || q <= 0}
            onClick={() => void submit()}
            data-testid="waste-submit"
          >
            {t("Record waste")}
          </Button>
        </>
      }
    >
      <div className="col gap-12" data-testid="waste-dialog">
        {error ? <Banner tone="danger">{error}</Banner> : null}
        <div>
          <strong dir="auto">{name}</strong>
          {lotLabel ? (
            <span className="tiny" dir="ltr">
              {" "}
              · {lotLabel}
            </span>
          ) : null}
        </div>
        <TextInput label={t("Quantity")} value={qty} onChange={(e) => setQty(e.target.value)} inputMode="decimal" />
        <Field label={t("Why")}>
          <div className="row wrap gap-8">
            {WASTE_REASONS.map(([k, l]) => (
              <Button key={k} size="sm" variant={reason === k ? "primary" : "default"} onClick={() => setReason(k)}>
                {l()}
              </Button>
            ))}
          </div>
        </Field>
        <TextInput label={t("Note (optional)")} value={note} dir="auto" onChange={(e) => setNote(e.target.value)} />
        <div className="tiny">
          {reason === "shrinkage"
            ? t("Use this when stock is missing and nobody knows why. A manager confirms it.")
            : t("The stock goes down now. A mistake can be reversed later; the record stays.")}
        </div>
      </div>
    </Modal>
  );
}

export function WastePage() {
  const { has } = useSession();
  const toast = useToast();
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [tab, setTab] = useState<"list" | "summary">("list");
  const list = useLoad(() => api.waste.list(from || null, to || null), [from, to]);
  const summary = useLoad(
    () => (has("products.view_cost") ? api.waste.summary(from || null, to || null) : Promise.resolve(null)),
    [from, to],
  );
  const [adding, setAdding] = useState<{ product_id: string; name: string } | null>(null);
  const [search, setSearch] = useState("");
  const [found, setFound] = useState<{ product_id: string; name: string }[]>([]);
  const [reversing, setReversing] = useState<WasteRow | null>(null);
  const [why, setWhy] = useState("");
  const act = useAction();
  return (
    <div>
      <PageHeader
        title={t("Waste")}
        subtitle={t("Stock that left without being sold: expired, damaged, spoiled, used in the shop or missing.")}
      />
      <div className="row wrap gap-8" style={{ marginBottom: 16 }}>
        <input
          type="date"
          className="input"
          style={{ width: 160 }}
          value={from}
          aria-label={t("From")}
          onChange={(e) => setFrom(e.target.value)}
        />
        <input
          type="date"
          className="input"
          style={{ width: 160 }}
          value={to}
          aria-label={t("To")}
          onChange={(e) => setTo(e.target.value)}
        />
        <div className="grow" />
        {has("waste.record") ? (
          <div style={{ position: "relative" }}>
            <input
              className="input"
              placeholder={t("Product to write off…")}
              value={search}
              data-testid="waste-search"
              onChange={async (e) => {
                setSearch(e.target.value);
                setFound(
                  e.target.value.trim()
                    ? (await api.pos.search(e.target.value.trim(), { limit: 6 })).map((p) => ({
                        product_id: p.product_id,
                        name: p.name,
                      }))
                    : [],
                );
              }}
            />
            {found.length ? (
              <div className="menu" style={{ insetInline: 0 }}>
                {found.map((p) => (
                  <button
                    key={p.product_id}
                    onClick={() => {
                      setAdding(p);
                      setFound([]);
                      setSearch("");
                    }}
                  >
                    <Plus size={14} /> <span dir="auto">{p.name}</span>
                  </button>
                ))}
              </div>
            ) : null}
          </div>
        ) : null}
      </div>
      <Tabs<"list" | "summary">
        value={tab}
        onChange={setTab}
        tabs={[
          { key: "list", label: t("Records") },
          ...(has("products.view_cost") ? [{ key: "summary" as const, label: t("Summary") }] : []),
        ]}
      />
      <div style={{ marginTop: 16 }}>
        {list.error ? <Banner tone="danger">{list.error}</Banner> : null}
        {tab === "list" ? (
          <DataTable<WasteRow>
            rows={list.data?.rows ?? null}
            loading={list.loading}
            rowKey={(r) => r.waste_id}
            empty={<Empty title={t("No waste recorded for this period")} />}
            columns={[
              { key: "d", label: t("Date"), render: (r) => formatDate(r.business_date), sort: (r) => r.created_at },
              { key: "n", label: t("Record"), render: (r) => <span dir="ltr">{r.waste_number}</span> },
              {
                key: "p",
                label: t("Product"),
                render: (r) => (
                  <span>
                    <span dir="auto">{r.product_name}</span>{" "}
                    {r.lot_number ? (
                      <span className="tiny" dir="ltr">
                        {r.lot_number}
                      </span>
                    ) : null}
                  </span>
                ),
              },
              { key: "q", label: t("Quantity"), render: (r) => formatQty(r.qty_milli), num: true },
              { key: "r", label: t("Why"), render: (r) => reasonLabel(r.reason) },
              ...(has("products.view_cost")
                ? [{ key: "c", label: t("Cost"), render: (r: WasteRow) => formatMoney(r.cost_minor ?? 0), num: true }]
                : []),
              { key: "u", label: t("By"), render: (r) => r.user_name ?? "" },
              {
                key: "s",
                label: t("Status"),
                render: (r) =>
                  r.status === "reversed" ? (
                    <Chip>{t("Reversed")}</Chip>
                  ) : has("waste.approve") ? (
                    <Button size="sm" variant="ghost" onClick={() => setReversing(r)}>
                      {t("Reverse")}
                    </Button>
                  ) : (
                    <Chip tone="success">{t("Recorded")}</Chip>
                  ),
              },
            ]}
          />
        ) : null}
        {tab === "summary" && summary.data ? (
          <div className="col gap-16" data-testid="waste-summary">
            <div className="kpis">
              <div className="card kpi">
                <div className="k-label">{t("Waste at cost")}</div>
                <div className="k-value money">{formatMoney(summary.data.cost_minor)}</div>
              </div>
              <div className="card kpi">
                <div className="k-label">{t("% of sales")}</div>
                <div className="k-value num">
                  {summary.data.pct_of_sales_bp === null ? "—" : `${(summary.data.pct_of_sales_bp / 100).toFixed(2)}%`}
                </div>
              </div>
              <div className="card kpi">
                <div className="k-label">{t("% of goods received")}</div>
                <div className="k-value num">
                  {summary.data.pct_of_purchases_bp === null
                    ? "—"
                    : `${(summary.data.pct_of_purchases_bp / 100).toFixed(2)}%`}
                </div>
              </div>
            </div>
            <div className="grid-2 gap-16">
              {(
                [
                  [t("By reason"), summary.data.by_reason, true],
                  [t("By product"), summary.data.by_product, false],
                  [t("By category"), summary.data.by_category, false],
                  [t("By supplier"), summary.data.by_supplier, false],
                ] as const
              ).map(([title, rows, isReason]) => (
                <div key={title} className="card card-pad">
                  <h3>{title}</h3>
                  <table className="table">
                    <tbody>
                      {rows.map((g, i) => (
                        <tr key={i}>
                          <td dir="auto">{isReason ? reasonLabel(g.key ?? "") : (g.key ?? t("Unknown"))}</td>
                          <td className="num">{formatQty(g.qty_milli)}</td>
                          <td className="num money">{formatMoney(g.cost_minor)}</td>
                        </tr>
                      ))}
                      {rows.length === 0 ? (
                        <tr>
                          <td className="tiny">{t("No waste recorded for this period")}</td>
                        </tr>
                      ) : null}
                    </tbody>
                  </table>
                </div>
              ))}
            </div>
            <div className="tiny">
              {summary.data.definitions.map((d) => (
                <div key={d}>{tb(d)}</div>
              ))}
            </div>
          </div>
        ) : null}
      </div>
      {adding ? (
        <RecordWasteDialog
          productId={adding.product_id}
          productName={adding.name}
          onClose={() => setAdding(null)}
          onDone={() => {
            setAdding(null);
            void list.reload();
            void summary.reload();
          }}
        />
      ) : null}
      {reversing ? (
        <Modal
          title={t("Reverse waste {0}", reversing.waste_number)}
          onClose={() => setReversing(null)}
          footer={
            <>
              <Button onClick={() => setReversing(null)}>{t("Cancel")}</Button>
              <Button
                variant="primary"
                loading={act.busy}
                disabled={!why.trim()}
                onClick={async () => {
                  const r = await act.run(() => api.waste.reverse(reversing.waste_id, why.trim(), newOperationId()));
                  if (r) {
                    toast("success", t("Waste reversed"));
                    setReversing(null);
                    setWhy("");
                    void list.reload();
                  }
                }}
              >
                {t("Reverse")}
              </Button>
            </>
          }
        >
          <div className="col gap-12">
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
            <div>{t("The stock comes back and the record is kept, marked as reversed.")}</div>
            <TextInput label={t("Why")} value={why} dir="auto" onChange={(e) => setWhy(e.target.value)} />
          </div>
        </Modal>
      ) : null}
    </div>
  );
}

function coverWords(r: CoverRow) {
  switch (r.state) {
    case "no_stock":
      return t("No stock");
    case "no_demand":
      return t("No sales recently");
    case "not_enough_history":
      return t("Not enough recent sales to estimate");
    default:
      return t("{0} days", ((r.cover_tenths ?? 0) / 10).toFixed(1));
  }
}

export function StockCoverPage() {
  const [window, setWindow] = useState(30);
  const [search, setSearch] = useState("");
  const data = useLoad(() => api.stockCover({ window_days: window, search: search || null }), [window, search]);
  return (
    <div>
      <PageHeader
        title={t("Days of stock left")}
        subtitle={t(
          "How long stock will last at the recent selling rate (units sold less refunds, per day). Stock on order is shown apart.",
        )}
      />
      <div className="row wrap gap-8" style={{ marginBottom: 16 }}>
        <Field label={t("Selling rate over")}>
          <select className="select" value={window} onChange={(e) => setWindow(Number(e.target.value))}>
            {[7, 30, 60, 90].map((d) => (
              <option key={d} value={d}>
                {t("Last {0} days", d)}
              </option>
            ))}
          </select>
        </Field>
        <TextInput label={t("Search")} value={search} onChange={(e) => setSearch(e.target.value)} />
      </div>
      {data.error ? <Banner tone="danger">{data.error}</Banner> : null}
      <DataTable<CoverRow>
        rows={data.data?.rows ?? null}
        loading={data.loading}
        rowKey={(r) => r.product_id}
        empty={<Empty title={t("No products that track stock")} />}
        columns={[
          { key: "p", label: t("Product"), render: (r) => <span dir="auto">{productName(r)}</span> },
          { key: "a", label: t("Available"), render: (r) => formatQty(r.available_milli), num: true },
          {
            key: "v",
            label: t("Sells a day"),
            render: (r) => (r.state === "ok" || r.state === "no_demand" ? formatQty(r.per_day_milli) : "—"),
            num: true,
          },
          {
            key: "c",
            label: t("Days of stock left"),
            render: (r) =>
              r.state === "ok" ? (
                <Chip tone={(r.cover_tenths ?? 0) < 70 ? "warning" : "default"}>{coverWords(r)}</Chip>
              ) : (
                <span className="tiny">{coverWords(r)}</span>
              ),
            sort: (r) => r.cover_tenths ?? Number.MAX_SAFE_INTEGER,
          },
          { key: "o", label: t("Runs out"), render: (r) => (r.stockout_on ? formatDate(r.stockout_on) : "—") },
          {
            key: "i",
            label: t("With stock on order"),
            render: (r) =>
              r.inbound_milli > 0
                ? `${formatQty(r.inbound_milli)} · ${r.cover_with_inbound_tenths !== null ? t("{0} days", (r.cover_with_inbound_tenths / 10).toFixed(1)) : "—"}`
                : "—",
          },
        ]}
      />
    </div>
  );
}

/** Product editor card: batch tracking, the batches on hand, count old stock in. */
export function ProductBatchesCard({ productId }: { productId: string }) {
  const { has } = useSession();
  const data = useLoad(() => api.lots.product(productId), [productId]);
  const act = useAction();
  const [lot, setLot] = useState<string | null>(null);
  const [counting, setCounting] = useState(false);
  const d = data.data;
  if (!d) return data.error ? <Banner tone="danger">{data.error}</Banner> : null;
  return (
    <div className="card card-pad col gap-12" data-testid="product-batches">
      <h3>
        <Layers size={16} style={{ verticalAlign: -2 }} /> {t("Batches and expiry")}
      </h3>
      {has("products.manage") ? (
        <div className="row wrap gap-16">
          <Checkbox
            label={t("Ask for batch and expiry when receiving")}
            checked={d.track_lots}
            onChange={async (v) => {
              const r = await act.run(() => api.lots.productSettings(productId, v, d.expiry_kind));
              if (r) void data.reload();
            }}
          />
          <Field label={t("The date on the pack means")}>
            <select
              className="select"
              value={d.expiry_kind ?? ""}
              onChange={async (e) => {
                const r = await act.run(() =>
                  api.lots.productSettings(productId, d.track_lots, e.target.value || null),
                );
                if (r) void data.reload();
              }}
            >
              <option value="">{t("Not set")}</option>
              <option value="use_by">{t("Expiry (use by)")}</option>
              <option value="best_before">{t("Best before")}</option>
            </select>
          </Field>
        </div>
      ) : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <table className="table">
        <tbody>
          {d.lots
            .filter((l) => l.balance_milli > 0)
            .map((l) => (
              <tr key={l.lot_id} className="clickable" onClick={() => setLot(l.lot_id)}>
                <td dir="ltr">{l.lot_number}</td>
                <td dir="auto">{l.supplier_lot_code ?? ""}</td>
                <td>
                  <Chip tone={expiryTone(l.status)}>{expiryWords(l.status, l.days_left, l.expiry_kind)}</Chip>
                </td>
                <td className="num">{formatQty(l.balance_milli)}</td>
              </tr>
            ))}
          <tr>
            <td colSpan={3}>{t("Not in a batch")}</td>
            <td className="num">{formatQty(d.unlotted_milli)}</td>
          </tr>
        </tbody>
      </table>
      {d.unlotted_milli > 0 && has("lots.manage") ? (
        <div>
          <Button size="sm" onClick={() => setCounting(true)}>
            {t("Count stock into a batch")}
          </Button>
        </div>
      ) : null}
      {lot ? <LotDrawer id={lot} onClose={() => setLot(null)} onChanged={() => void data.reload()} /> : null}
      {counting ? (
        <CountInDialog
          productId={productId}
          max={d.unlotted_milli}
          onClose={() => setCounting(false)}
          onDone={() => {
            setCounting(false);
            void data.reload();
          }}
        />
      ) : null}
    </div>
  );
}

function CountInDialog({
  productId,
  max,
  onClose,
  onDone,
}: {
  productId: string;
  max: number;
  onClose: () => void;
  onDone: () => void;
}) {
  const [qty, setQty] = useState(formatQty(max));
  const [exp, setExp] = useState("");
  const [code, setCode] = useState("");
  const [warnings, setWarnings] = useState<string[] | null>(null);
  const [opId] = useState(newOperationId);
  const act = useAction();
  const q = parseQty(qty);
  const save = async () => {
    try {
      await api.lots.countIn({
        product_id: productId,
        qty_milli: q!,
        lot: { expires_on: exp || null, supplier_lot_code: code || null, confirm_warnings: !!warnings },
        operation_id: opId,
      });
      onDone();
    } catch (e) {
      const w = dateWarnings(e);
      if (w) setWarnings(w);
      else await act.run(() => Promise.reject(e));
    }
  };
  return (
    <Modal
      title={t("Count stock into a batch")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button variant="primary" disabled={!q || q <= 0 || (!exp && !code)} onClick={() => void save()}>
            {warnings ? t("Keep these dates") : t("Save")}
          </Button>
        </>
      }
    >
      <div className="col gap-12">
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        {warnings ? (
          <Banner tone="warning" title={t("Check the dates")}>
            {warnings.map((w) => (
              <div key={w}>{tb(w)}</div>
            ))}
          </Banner>
        ) : null}
        <div className="tiny">
          <AlertTriangle size={14} style={{ verticalAlign: -2 }} />{" "}
          {t(
            "Stock on hand does not change. Use this when you read the date off stock that was received before batches were kept.",
          )}
        </div>
        <TextInput label={t("Quantity")} value={qty} onChange={(e) => setQty(e.target.value)} />
        <Field label={t("Expiry date")}>
          <input type="date" className="input" value={exp} onChange={(e) => setExp(e.target.value)} />
        </Field>
        <TextInput label={t("Batch code")} value={code} dir="auto" onChange={(e) => setCode(e.target.value)} />
      </div>
    </Modal>
  );
}
