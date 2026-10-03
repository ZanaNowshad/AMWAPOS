// Payables: what the shop owes its suppliers. A supplier invoice is entered
// (by hand or from a scanned document), reviewed, then posted; only posted
// records count as owed. Payments are recorded against the invoices they pay.
// Posted records never change: a mistake is undone by a reversal.
import { useMemo, useState } from "react";
import { Plus, Wallet } from "lucide-react";
import { api } from "../../api";
import type { ApBucket, ApInvoice, ApMethod, ApOpenInvoice, ApSupplier, SupplierInvoice } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { Banner, Button, Chip, Empty, Field, Modal, PageHeader, Skeleton, Tabs, TextInput } from "../../components/ui";
import { Confirm, DataTable, Drawer, useAction, useLoad } from "./common";
import { formatAmount, formatMoney, parseMoney } from "../../lib/money";
import { todayLocal } from "../../lib/time";
import { newOperationId } from "../../lib/ids";
import { t } from "../../i18n";

export const BUCKET_LABEL: Record<ApBucket, () => string> = {
  current: () => t("Not due yet"),
  "1_30": () => t("1–30 days late"),
  "31_60": () => t("31–60 days late"),
  "61_90": () => t("61–90 days late"),
  "90_plus": () => t("Over 90 days late"),
};

const METHODS: ApMethod[] = ["bank_transfer", "cash", "cheque", "benefitpay", "card", "other"];
const METHOD_LABEL: Record<ApMethod, () => string> = {
  bank_transfer: () => t("Bank transfer"),
  cash: () => t("Cash"),
  cheque: () => t("Cheque"),
  benefitpay: () => t("BenefitPay"),
  card: () => t("Card"),
  other: () => t("Other"),
};

/** Where a supplier invoice record is, in one plain word. */
export function invoiceStage(r: Pick<SupplierInvoice, "status" | "posting">): {
  label: string;
  tone: "default" | "warning" | "success" | "info";
} {
  if (r.status === "void") return { label: t("Void"), tone: "default" };
  if (r.posting === "reversed") return { label: t("Reversed"), tone: "default" };
  if (r.posting === "posted") return { label: t("Posted"), tone: "success" };
  if (r.status === "approved") return { label: t("Ready to post"), tone: "info" };
  return { label: t("To review"), tone: "warning" };
}

/**
 * Spread a payment over open invoices, oldest due date first. Returns the
 * amount per invoice; what is left over stays on the supplier's account.
 */
export function oldestFirst(
  amount: number,
  open: Pick<ApOpenInvoice, "invoice_id" | "due_date" | "outstanding_minor">[],
) {
  const out: Record<string, number> = {};
  let left = Math.max(0, amount);
  for (const i of [...open].sort((a, b) => a.due_date.localeCompare(b.due_date))) {
    const take = Math.min(left, i.outstanding_minor);
    out[i.invoice_id] = take;
    left -= take;
  }
  return out;
}

export function PayablesPage() {
  const { has } = useSession();
  const [tab, setTab] = useState<"suppliers" | "invoices">("suppliers");
  const ov = useLoad(() => api.payables.overview(), []);
  const [supplier, setSupplier] = useState<string | null>(null);
  const [invoice, setInvoice] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const reload = () => void ov.reload();
  const d = ov.data;
  return (
    <div>
      <PageHeader
        title={t("Payables")}
        subtitle={t(
          "What you owe your suppliers. Review and post their invoices, then record what you pay. Posted records are never edited: a mistake is undone with a reversal.",
        )}
        actions={
          has("payables.review") ? (
            <Button variant="primary" icon={<Plus size={16} />} onClick={() => setAdding(true)}>
              {t("Add supplier invoice")}
            </Button>
          ) : null
        }
      />
      {ov.error ? <Banner tone="danger">{ov.error}</Banner> : null}
      {!d ? (
        <Skeleton />
      ) : (
        <div className="col gap-16">
          <div className="kpis">
            <div className="card kpi" data-testid="ap-owed">
              <div className="k-label">{t("You owe")}</div>
              <div className="k-value money">{formatMoney(d.outstanding_minor)}</div>
            </div>
            <div className="card kpi">
              <div className="k-label">{t("Overdue")}</div>
              <div className={`k-value money ${d.overdue_minor ? "neg-num" : ""}`}>{formatMoney(d.overdue_minor)}</div>
            </div>
            <div className="card kpi">
              <div className="k-label">{t("Due in the next 7 days")}</div>
              <div className="k-value money">{formatMoney(d.due_within_7_days_minor)}</div>
            </div>
          </div>
          {d.to_review || d.to_post ? (
            <Banner
              tone="info"
              title={
                d.to_review && d.to_post
                  ? t("{0} invoices to review, {1} ready to post", d.to_review, d.to_post)
                  : d.to_review
                    ? t("{0} invoices to review", d.to_review)
                    : t("{0} invoices ready to post", d.to_post)
              }
            >
              <Button size="sm" onClick={() => setTab("invoices")}>
                {t("Show invoices")}
              </Button>
            </Banner>
          ) : null}
          <div className="ap-ageing" data-testid="ap-ageing" aria-label={t("Owed by how late it is")}>
            {d.ageing.map((a) => (
              <div key={a.bucket} className={`ap-age ${a.bucket !== "current" && a.amount_minor ? "late" : ""}`}>
                <span className="tiny">{BUCKET_LABEL[a.bucket]()}</span>
                <strong className="money">{formatMoney(a.amount_minor)}</strong>
              </div>
            ))}
          </div>
          <Tabs<"suppliers" | "invoices">
            value={tab}
            onChange={setTab}
            tabs={[
              { key: "suppliers", label: t("Suppliers") },
              { key: "invoices", label: t("Invoices") },
            ]}
          />
          {tab === "suppliers" ? (
            <DataTable
              rows={d.suppliers}
              rowKey={(r) => r.supplier_id}
              onRowClick={(r) => setSupplier(r.supplier_id)}
              empty={
                <Empty title={t("Nothing owed")}>
                  {t("Suppliers appear here once one of their invoices is posted.")}
                </Empty>
              }
              columns={[
                { key: "n", label: t("Supplier"), render: (r) => r.supplier_name, sort: (r) => r.supplier_name },
                { key: "o", label: t("Open invoices"), num: true, render: (r) => r.open_invoices },
                {
                  key: "late",
                  label: t("Overdue"),
                  num: true,
                  render: (r) =>
                    r.overdue_minor ? <span className="neg-num">{formatMoney(r.overdue_minor)}</span> : "—",
                  sort: (r) => r.overdue_minor,
                },
                {
                  key: "b",
                  label: t("Balance"),
                  num: true,
                  render: (r) => <strong>{formatMoney(r.balance_minor)}</strong>,
                  sort: (r) => r.balance_minor,
                },
              ]}
            />
          ) : (
            <InvoiceList onOpen={setInvoice} />
          )}
        </div>
      )}
      {supplier ? (
        <SupplierAccount
          id={supplier}
          onClose={() => setSupplier(null)}
          onOpenInvoice={(i) => setInvoice(i)}
          onChanged={reload}
        />
      ) : null}
      {invoice ? <InvoiceDrawer id={invoice} onClose={() => setInvoice(null)} onChanged={reload} /> : null}
      {adding ? (
        <ManualInvoiceDialog
          onClose={() => setAdding(false)}
          onSaved={(id) => {
            setAdding(false);
            reload();
            setInvoice(id);
          }}
        />
      ) : null}
    </div>
  );
}

function InvoiceList({ onOpen }: { onOpen: (id: string) => void }) {
  const [filter, setFilter] = useState<"open" | "all">("open");
  const { data, loading, error } = useLoad(() => api.supplierInvoices.list(), []);
  const rows = (data ?? []).filter(
    (r) => filter === "all" || (r.status !== "void" && r.posting !== "posted" && r.posting !== "reversed"),
  );
  return (
    <div className="col gap-8">
      <div className="row">
        {(
          [
            ["open", t("Waiting for you")],
            ["all", t("All")],
          ] as const
        ).map(([k, l]) => (
          <button key={k} className={`filter-chip ${filter === k ? "active" : ""}`} onClick={() => setFilter(k)}>
            {l}
          </button>
        ))}
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<SupplierInvoice>
        rows={data ? rows : null}
        loading={loading}
        rowKey={(r) => r.invoice_id}
        onRowClick={(r) => onOpen(r.invoice_id)}
        empty={
          <div className="empty">
            {filter === "open" ? t("No supplier invoices are waiting.") : t("No supplier invoice records yet.")}
          </div>
        }
        columns={[
          { key: "sup", label: t("Supplier"), render: (r) => r.supplier_name },
          {
            key: "inv",
            label: t("Invoice"),
            render: (r) => (
              <>
                {r.invoice_number ?? r.number}
                {r.doc_type === "credit_note" ? <span className="tiny"> · {t("Credit note")}</span> : null}
              </>
            ),
          },
          { key: "d", label: t("Invoice date"), render: (r) => r.invoice_date ?? "—" },
          { key: "tot", label: t("Total"), num: true, render: (r) => formatMoney(r.total_minor) },
          {
            key: "st",
            label: t("Status"),
            render: (r) => {
              const s = invoiceStage(r);
              return <Chip tone={s.tone}>{s.label}</Chip>;
            },
          },
        ]}
      />
    </div>
  );
}

function SupplierAccount({
  id,
  onClose,
  onOpenInvoice,
  onChanged,
}: {
  id: string;
  onClose: () => void;
  onOpenInvoice: (id: string) => void;
  onChanged: () => void;
}) {
  const { has } = useSession();
  const { data, error, reload } = useLoad(() => api.payables.supplier(id), [id]);
  const [paying, setPaying] = useState(false);
  return (
    <Drawer
      wide
      title={data?.supplier_name ?? t("Supplier account")}
      onClose={onClose}
      actions={
        has("payables.pay") && data && data.open_invoices.length ? (
          <Button variant="primary" icon={<Wallet size={16} />} onClick={() => setPaying(true)} data-testid="ap-pay">
            {t("Record payment")}
          </Button>
        ) : null
      }
    >
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {!data ? (
        <Skeleton />
      ) : (
        <div className="col gap-16" data-testid="ap-supplier">
          <div className="row" style={{ alignItems: "baseline" }}>
            <span className="grow muted">{t("Balance owed")}</span>
            <strong className="money" style={{ fontSize: 24 }}>
              {formatMoney(data.balance_minor)}
            </strong>
          </div>
          {data.balance_minor < 0 ? (
            <Banner tone="info">
              {t("You have paid more than you owe this supplier. The extra is kept on account.")}
            </Banner>
          ) : null}
          <section className="col gap-8">
            <h3>{t("Open invoices")}</h3>
            <DataTable
              rows={data.open_invoices}
              rowKey={(r) => r.invoice_id}
              onRowClick={(r) => onOpenInvoice(r.invoice_id)}
              empty={<div className="empty">{t("No open invoices.")}</div>}
              columns={[
                { key: "n", label: t("Invoice"), render: (r) => r.invoice_number ?? r.number },
                { key: "due", label: t("Due"), render: (r) => r.due_date },
                {
                  key: "late",
                  label: t("Status"),
                  render: (r) =>
                    r.days_overdue > 0 ? (
                      <Chip tone="warning">{t("{0} days late", r.days_overdue)}</Chip>
                    ) : (
                      <Chip>{t("Not due yet")}</Chip>
                    ),
                },
                { key: "o", label: t("Still owed"), num: true, render: (r) => formatMoney(r.outstanding_minor) },
              ]}
            />
          </section>
          {data.payments.length ? (
            <section className="col gap-8">
              <h3>{t("Payments")}</h3>
              {data.payments.map((p) => (
                <div key={p.payment_id} className="row small">
                  <span className="grow">
                    {p.paid_on} · {METHOD_LABEL[p.method as ApMethod]?.() ?? p.method}
                    {p.reference ? ` · ${p.reference}` : ""}
                    {p.status !== "posted" ? ` · ${t("Reversed")}` : ""}
                  </span>
                  {p.unallocated_minor > 0 ? (
                    <span className="tiny muted">
                      {t("{0} not applied to an invoice", formatMoney(p.unallocated_minor))}
                    </span>
                  ) : null}
                  <span className="money">{formatMoney(p.amount_minor)}</span>
                </div>
              ))}
            </section>
          ) : null}
          {data.credits.length ? (
            <section className="col gap-8">
              <h3>{t("Credit notes")}</h3>
              {data.credits.map((c) => (
                <button key={c.credit_id} className="list-row row small" onClick={() => onOpenInvoice(c.invoice_id)}>
                  <span className="grow">
                    {c.invoice_number ?? c.number} · {c.doc_date}
                  </span>
                  {c.unapplied_minor > 0 ? (
                    <span className="tiny muted">{t("{0} not used yet", formatMoney(c.unapplied_minor))}</span>
                  ) : null}
                  <span className="money">{formatMoney(c.amount_minor)}</span>
                </button>
              ))}
            </section>
          ) : null}
          <details>
            <summary>{t("Statement")}</summary>
            <table className="table" style={{ marginTop: 8 }}>
              <thead>
                <tr>
                  <th>{t("Date")}</th>
                  <th>{t("Entry")}</th>
                  <th className="num">{t("Amount")}</th>
                  <th className="num">{t("Balance")}</th>
                </tr>
              </thead>
              <tbody>
                {data.statement.map((e, i) => (
                  <tr key={i}>
                    <td>{e.date}</td>
                    <td>
                      {e.kind === "invoice" ? t("Invoice") : e.kind === "payment" ? t("Payment") : t("Credit note")}
                      {e.reference ? ` · ${e.reference}` : ""}
                    </td>
                    <td className="num">{formatMoney(e.amount_minor)}</td>
                    <td className="num">{formatMoney(e.balance_minor)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </details>
        </div>
      )}
      {paying && data ? (
        <PaymentDialog
          account={data}
          onClose={() => setPaying(false)}
          onDone={() => {
            setPaying(false);
            void reload();
            onChanged();
          }}
        />
      ) : null}
    </Drawer>
  );
}

function PaymentDialog({ account, onClose, onDone }: { account: ApSupplier; onClose: () => void; onDone: () => void }) {
  const toast = useToast();
  const act = useAction();
  // One id for this payment: a retry after a lost reply never pays twice.
  const [op] = useState(newOperationId);
  const [amount, setAmount] = useState("");
  const [date, setDate] = useState(todayLocal());
  const [method, setMethod] = useState<ApMethod>("bank_transfer");
  const [reference, setReference] = useState("");
  const [split, setSplit] = useState<Record<string, string> | null>(null);
  const minor = parseMoney(amount);
  const auto = useMemo(() => oldestFirst(minor ?? 0, account.open_invoices), [minor, account.open_invoices]);
  const plan: Record<string, number> = split
    ? Object.fromEntries(Object.entries(split).map(([k, v]) => [k, parseMoney(v) ?? 0]))
    : auto;
  const applied = Object.values(plan).reduce((a, b) => a + b, 0);
  const over = account.open_invoices.find((i) => (plan[i.invoice_id] ?? 0) > i.outstanding_minor);
  const problem =
    minor === null || minor <= 0
      ? t("Enter the amount paid.")
      : applied > minor
        ? t("More is applied to invoices than was paid.")
        : over
          ? t("More than is owed is applied to {0}.", over.invoice_number ?? over.number)
          : null;
  const save = async () => {
    if (problem || minor === null) return;
    const r = await act.run(() =>
      api.payables.recordPayment({
        supplier_id: account.supplier_id,
        paid_on: date,
        amount_minor: minor,
        method,
        reference: reference.trim() || null,
        allocations: Object.entries(plan)
          .filter(([, v]) => v > 0)
          .map(([invoice_id, amount_minor]) => ({ invoice_id, amount_minor })),
        operation_id: op,
      }),
    );
    if (r) {
      toast("success", t("Payment {0} recorded", r.number));
      onDone();
    }
  };
  return (
    <Modal
      title={t("Pay {0}", account.supplier_name)}
      size="lg"
      onClose={act.busy ? undefined : onClose}
      testId="ap-payment-dialog"
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button variant="primary" className="right" loading={act.busy} disabled={!!problem} onClick={save}>
            {minor ? t("Record payment of {0}", formatMoney(minor)) : t("Record payment")}
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        <div className="grid-2">
          <TextInput
            label={t("Amount paid")}
            inputMode="decimal"
            autoFocus
            value={amount}
            onChange={(e) => (setAmount(e.target.value), setSplit(null))}
            data-testid="ap-pay-amount"
          />
          <TextInput label={t("Paid on")} type="date" value={date} onChange={(e) => setDate(e.target.value)} />
        </div>
        <div className="grid-2">
          <Field label={t("How it was paid")}>
            <select className="select" value={method} onChange={(e) => setMethod(e.target.value as ApMethod)}>
              {METHODS.map((m) => (
                <option key={m} value={m}>
                  {METHOD_LABEL[m]()}
                </option>
              ))}
            </select>
          </Field>
          <TextInput
            label={t("Reference (optional)")}
            value={reference}
            onChange={(e) => setReference(e.target.value)}
            hint={t("Transfer or cheque number.")}
          />
        </div>
        <div className="col gap-8">
          <div className="row">
            <h3 className="grow">{t("Which invoices it pays")}</h3>
            {split ? (
              <Button size="sm" variant="ghost" onClick={() => setSplit(null)}>
                {t("Oldest first")}
              </Button>
            ) : null}
          </div>
          <div className="tiny muted">
            {t("Filled oldest first. Change any amount if the supplier was paid for specific invoices.")}
          </div>
          {account.open_invoices.map((i) => (
            <div key={i.invoice_id} className="row">
              <span className="grow small">
                {i.invoice_number ?? i.number} · {t("due {0}", i.due_date)} ·{" "}
                <span className="muted">{t("owed {0}", formatMoney(i.outstanding_minor))}</span>
              </span>
              <input
                className="input num"
                style={{ width: 130 }}
                inputMode="decimal"
                aria-label={t("Amount for {0}", i.invoice_number ?? i.number)}
                value={split ? (split[i.invoice_id] ?? "") : plan[i.invoice_id] ? formatAmount(plan[i.invoice_id]) : ""}
                onChange={(e) => {
                  const base =
                    split ?? Object.fromEntries(Object.entries(auto).map(([k, v]) => [k, v ? formatAmount(v) : ""]));
                  setSplit({ ...base, [i.invoice_id]: e.target.value });
                }}
              />
            </div>
          ))}
          {minor && minor > applied && !problem ? (
            <div className="small muted">
              {t("{0} is not applied to an invoice and stays on the supplier's account.", formatMoney(minor - applied))}
            </div>
          ) : null}
        </div>
        {problem && amount ? <div className="small neg-num">{problem}</div> : null}
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      </div>
    </Modal>
  );
}

function InvoiceDrawer({ id, onClose, onChanged }: { id: string; onClose: () => void; onChanged: () => void }) {
  const { has } = useSession();
  const toast = useToast();
  const { data, error, setData, reload } = useLoad(() => api.payables.invoice(id), [id]);
  const act = useAction();
  const [posting, setPosting] = useState(false);
  const [reversing, setReversing] = useState(false);
  const [reason, setReason] = useState("");
  const [op] = useState(newOperationId);
  const done = (r: ApInvoice | undefined) => {
    if (r) {
      setData(r);
      onChanged();
    }
    return r;
  };
  const stage = data ? invoiceStage(data) : null;
  const credit = data?.doc_type === "credit_note";
  return (
    <Drawer
      wide
      title={data ? `${data.supplier_name} · ${data.invoice_number ?? data.number}` : t("Supplier invoice")}
      onClose={onClose}
    >
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {!data || !stage ? (
        <Skeleton />
      ) : (
        <div className="col gap-16" data-testid="ap-invoice">
          <div className="row">
            <Chip tone={stage.tone}>{stage.label}</Chip>
            {credit ? <Chip>{t("Credit note")}</Chip> : null}
            {data.outstanding_minor !== null && data.posting === "posted" && !credit ? (
              <span className="small grow" style={{ textAlign: "end" }}>
                {data.outstanding_minor > 0
                  ? t("Still owed {0}", formatMoney(data.outstanding_minor))
                  : t("Paid in full")}
              </span>
            ) : null}
          </div>
          <dl className="kv">
            <dt>{t("Invoice date")}</dt>
            <dd>{data.invoice_date ?? "—"}</dd>
            <dt>{t("Due date")}</dt>
            <dd>{data.liability?.due_date ?? data.due_date ?? t("From the supplier's payment terms")}</dd>
            <dt>{t("Subtotal")}</dt>
            <dd>{formatMoney(data.subtotal_minor ?? null)}</dd>
            <dt>{t("VAT")}</dt>
            <dd>{formatMoney(data.vat_minor ?? null)}</dd>
            <dt>{t("Total")}</dt>
            <dd>
              <strong>{formatMoney(data.total_minor)}</strong>
            </dd>
          </dl>
          {data.status === "draft" ? (
            <Banner tone="info">{t("Check it against the supplier's paper, then mark it reviewed.")}</Banner>
          ) : data.status === "approved" && data.posting === "not_posted" ? (
            <Banner tone="info">
              {credit
                ? t("Reviewed. Posting lowers what you owe this supplier.")
                : t("Reviewed. Posting adds it to what you owe this supplier.")}
            </Banner>
          ) : null}
          {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          <div className="row wrap">
            {data.status === "draft" && (has("payables.review") || has("purchasing.manage")) ? (
              <Button
                variant="primary"
                loading={act.busy}
                onClick={async () => {
                  if (await act.run(() => api.payables.approve(id))) {
                    await reload();
                    onChanged();
                  }
                }}
                data-testid="ap-approve"
              >
                {t("Mark reviewed")}
              </Button>
            ) : null}
            {data.status === "approved" && data.posting === "not_posted" && has("payables.post") ? (
              <Button variant="primary" onClick={() => setPosting(true)} data-testid="ap-post">
                {t("Post")}
              </Button>
            ) : null}
            {data.posting === "posted" && has("payables.post") ? (
              <Button variant="danger-outline" onClick={() => setReversing(true)}>
                {t("Reverse")}
              </Button>
            ) : null}
          </div>
        </div>
      )}
      {posting && data ? (
        <Confirm
          title={t("Post {0}?", data.invoice_number ?? data.number)}
          confirmLabel={t("Post")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setPosting(false)}
          onConfirm={async () => {
            if (done(await act.run(() => api.payables.post(id, op)))) {
              setPosting(false);
              toast("success", t("Posted"));
            }
          }}
        >
          {credit
            ? t(
                "{0} is taken off what you owe {1}. Once posted it can only be undone with a reversal.",
                formatMoney(data.total_minor),
                data.supplier_name,
              )
            : t(
                "{0} is added to what you owe {1}. Once posted it can only be undone with a reversal.",
                formatMoney(data.total_minor),
                data.supplier_name,
              )}
        </Confirm>
      ) : null}
      {reversing && data ? (
        <Confirm
          title={t("Reverse {0}?", data.invoice_number ?? data.number)}
          confirmLabel={t("Reverse")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setReversing(false)}
          onConfirm={async () => {
            if (!reason.trim()) return act.setError(t("Enter the reason for the reversal."));
            if (done(await act.run(() => api.payables.reverse(id, reason.trim())))) {
              setReversing(false);
              toast("success", t("Reversed"));
            }
          }}
        >
          <div className="col gap-8">
            <div>
              {t(
                "The posting is cancelled by a reversal entry; the original stays in the history. Payments applied to it must be removed first.",
              )}
            </div>
            <TextInput label={t("Reason")} value={reason} onChange={(e) => setReason(e.target.value)} autoFocus />
          </div>
        </Confirm>
      ) : null}
    </Drawer>
  );
}

function ManualInvoiceDialog({ onClose, onSaved }: { onClose: () => void; onSaved: (id: string) => void }) {
  const suppliers = useLoad(() => api.suppliers.list(), []);
  const act = useAction();
  const [supplier, setSupplier] = useState("");
  const [kind, setKind] = useState<"invoice" | "credit_note">("invoice");
  const [number, setNumber] = useState("");
  const [date, setDate] = useState(todayLocal());
  const [due, setDue] = useState("");
  const [subtotal, setSubtotal] = useState("");
  const [vat, setVat] = useState("");
  const sub = parseMoney(subtotal);
  const tax = vat.trim() ? parseMoney(vat) : 0;
  const total = sub !== null && tax !== null ? sub + tax : null;
  const problem = !supplier
    ? t("Choose the supplier.")
    : !number.trim()
      ? t("Enter the supplier's invoice number.")
      : sub === null || tax === null
        ? t("Enter the amounts.")
        : !total
          ? t("The total must be more than zero.")
          : null;
  const save = async () => {
    if (problem || sub === null || tax === null || total === null) return;
    const r = await act.run(() =>
      api.payables.createInvoice({
        supplier_id: supplier,
        doc_type: kind,
        invoice_number: number.trim(),
        invoice_date: date,
        due_date: due || null,
        subtotal_minor: sub,
        vat_minor: tax,
        total_minor: total,
      }),
    );
    if (r) onSaved(r.invoice_id);
  };
  return (
    <Modal
      title={t("Add supplier invoice")}
      size="md"
      onClose={act.busy ? undefined : onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button variant="primary" className="right" loading={act.busy} disabled={!!problem} onClick={save}>
            {t("Save for review")}
          </Button>
        </>
      }
    >
      <div className="col gap-12">
        <Field label={t("Supplier")}>
          <select className="select" value={supplier} onChange={(e) => setSupplier(e.target.value)} autoFocus>
            <option value="">{t("Choose…")}</option>
            {(suppliers.data ?? []).map((s) => (
              <option key={s.supplier_id} value={s.supplier_id}>
                {s.name}
              </option>
            ))}
          </select>
        </Field>
        <div className="seg" role="radiogroup" aria-label={t("Document type")}>
          {(
            [
              ["invoice", t("Invoice")],
              ["credit_note", t("Credit note")],
            ] as const
          ).map(([k, l]) => (
            <button
              key={k}
              type="button"
              role="radio"
              aria-checked={kind === k}
              className={`seg-btn ${kind === k ? "active" : ""}`}
              onClick={() => setKind(k)}
            >
              {l}
            </button>
          ))}
        </div>
        <div className="grid-2">
          <TextInput
            label={t("Supplier's invoice number")}
            value={number}
            onChange={(e) => setNumber(e.target.value)}
          />
          <TextInput label={t("Invoice date")} type="date" value={date} onChange={(e) => setDate(e.target.value)} />
        </div>
        <div className="grid-2">
          <TextInput
            label={t("Amount before VAT")}
            inputMode="decimal"
            value={subtotal}
            onChange={(e) => setSubtotal(e.target.value)}
          />
          <TextInput label={t("VAT")} inputMode="decimal" value={vat} onChange={(e) => setVat(e.target.value)} />
        </div>
        <div className="row">
          <span className="grow muted">{t("Total")}</span>
          <strong className="money">{formatMoney(total)}</strong>
        </div>
        <TextInput
          label={t("Due date (optional)")}
          type="date"
          value={due}
          onChange={(e) => setDue(e.target.value)}
          hint={t("Leave empty to use the supplier's payment terms.")}
        />
        <div className="tiny muted">{t("It is saved for review. Nothing is owed until it is posted.")}</div>
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      </div>
    </Modal>
  );
}
