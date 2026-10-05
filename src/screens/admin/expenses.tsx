// Expenses: what running the shop costs (rent, electricity, salaries, repairs…).
// An expense is entered, approved and paid; a mistake is voided and entered
// again, never edited after it is submitted. Petty cash is a small fund kept
// apart from the till drawer. Repeats make a draft on their day; nothing is
// paid by itself.
import { useMemo, useState } from "react";
import { Paperclip, Plus, Repeat, Wallet } from "lucide-react";
import { api } from "../../api";
import type { Expense, ExpenseCategory, ExpenseRecurring, PettyFund } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
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
import { Confirm, DataTable, Drawer, downloadBase64, useAction, useLoad } from "./common";
import { fileToBase64 } from "./automation";
import { formatAmount, formatMoney, parseMoney } from "../../lib/money";
import { formatDate, formatShort, todayLocal } from "../../lib/time";
import { newOperationId } from "../../lib/ids";
import { getLang, t } from "../../i18n";

const PAY_METHODS: [string, () => string][] = [
  ["petty_cash", () => t("Petty cash")],
  ["till_paid_out", () => t("Paid out of the till")],
  ["bank_transfer", () => t("Bank transfer")],
  ["card", () => t("Card")],
  ["cheque", () => t("Cheque")],
  ["other", () => t("Other")],
];
const methodName = (m: string | null) => PAY_METHODS.find((x) => x[0] === m)?.[1]() ?? "—";

/** Category name in the screen language (custom names as typed). */
const catName = (c: { category?: string; name?: string; category_ar?: string | null; name_ar?: string | null }) =>
  (getLang() === "ar" ? (c.category_ar ?? c.name_ar) : null) || c.category || c.name || "";

/** Where an expense is, in one plain word. */
export function expenseStage(s: Expense["status"]): {
  label: string;
  tone: "default" | "warning" | "success" | "info" | "danger";
} {
  switch (s) {
    case "draft":
      return { label: t("Draft"), tone: "default" };
    case "submitted":
      return { label: t("Waiting for approval"), tone: "warning" };
    case "approved":
      return { label: t("To pay"), tone: "info" };
    case "paid":
      return { label: t("Paid"), tone: "success" };
    case "rejected":
      return { label: t("Rejected"), tone: "danger" };
    default:
      return { label: t("Void"), tone: "default" };
  }
}

export function ExpensesPage() {
  const { has } = useSession();
  const [tab, setTab] = useState<"expenses" | "petty" | "repeats">("expenses");
  const list = useLoad(() => api.expenses.list(), []);
  const cats = useLoad(() => api.expenses.categories(), []);
  const [open, setOpen] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const d = list.data;
  const reload = () => void list.reload();
  return (
    <div>
      <PageHeader
        title={t("Expenses")}
        subtitle={t(
          "What running the shop costs. Enter a bill, get it approved, record how it was paid. A mistake is voided and entered again.",
        )}
        actions={
          has("expenses.create") ? (
            <Button
              variant="primary"
              icon={<Plus size={16} />}
              onClick={() => setAdding(true)}
              data-testid="add-expense"
            >
              {t("Add expense")}
            </Button>
          ) : null
        }
      />
      {list.error ? <Banner tone="danger">{list.error}</Banner> : null}
      {!d ? (
        <Skeleton />
      ) : (
        <div className="col gap-16">
          <div className="kpis">
            <div className="card kpi" data-testid="exp-spent">
              <div className="k-label">{t("Spent this month")}</div>
              <div className="k-value money">{formatMoney(d.spent_minor)}</div>
            </div>
            <div className="card kpi">
              <div className="k-label">{t("Waiting for approval")}</div>
              <div className="k-value num">{d.waiting_approval}</div>
            </div>
            <div className="card kpi">
              <div className="k-label">{t("Approved, not paid")}</div>
              <div className="k-value num">{d.approved_unpaid}</div>
            </div>
          </div>
          <Tabs<"expenses" | "petty" | "repeats">
            value={tab}
            onChange={setTab}
            tabs={[
              { key: "expenses", label: t("Expenses") },
              { key: "petty", label: t("Petty cash") },
              { key: "repeats", label: t("Repeats") },
            ]}
          />
          {tab === "expenses" ? (
            <DataTable<Expense>
              rows={d.rows}
              loading={list.loading}
              rowKey={(r) => r.expense_id}
              onRowClick={(r) => setOpen(r.expense_id)}
              empty={
                <Empty title={t("No expenses yet")}>
                  {t("Add rent, bills, salaries and other running costs to see what the shop really makes.")}
                </Empty>
              }
              columns={[
                {
                  key: "d",
                  label: t("Date"),
                  render: (r) => formatDate(r.business_date),
                  sort: (r) => r.business_date,
                },
                { key: "n", label: t("No."), render: (r) => <span className="mono">{r.number}</span> },
                { key: "c", label: t("Category"), render: (r) => <span dir="auto">{catName(r)}</span> },
                {
                  key: "p",
                  label: t("Paid to"),
                  render: (r) => (
                    <span dir="auto">
                      {r.payee ?? "—"}
                      {r.attachments ? (
                        <Paperclip size={12} aria-label={t("Bill attached")} style={{ marginInlineStart: 4 }} />
                      ) : null}
                    </span>
                  ),
                },
                {
                  key: "a",
                  label: t("Amount"),
                  num: true,
                  render: (r) => formatMoney(r.total_minor),
                  sort: (r) => r.total_minor,
                },
                {
                  key: "s",
                  label: t("Status"),
                  render: (r) => {
                    const st = expenseStage(r.status);
                    return <Chip tone={st.tone}>{st.label}</Chip>;
                  },
                },
              ]}
            />
          ) : tab === "petty" ? (
            <PettyCashPanel />
          ) : (
            <RepeatsPanel categories={cats.data ?? []} />
          )}
        </div>
      )}
      {adding ? (
        <ExpenseEditor
          categories={cats.data ?? []}
          onClose={() => setAdding(false)}
          onSaved={(id) => {
            setAdding(false);
            reload();
            setOpen(id);
          }}
        />
      ) : null}
      {open ? (
        <ExpenseDrawer id={open} categories={cats.data ?? []} onClose={() => setOpen(null)} onChanged={reload} />
      ) : null}
    </div>
  );
}

function ExpenseEditor({
  categories,
  initial,
  onClose,
  onSaved,
}: {
  categories: ExpenseCategory[];
  initial?: Expense;
  onClose: () => void;
  onSaved: (id: string) => void;
}) {
  const act = useAction();
  const [cat, setCat] = useState(initial?.category_id ?? "");
  const [date, setDate] = useState(initial?.business_date ?? todayLocal());
  const [payee, setPayee] = useState(initial?.payee ?? "");
  const [desc, setDesc] = useState(initial?.description ?? "");
  const [amount, setAmount] = useState(initial ? formatAmount(initial.total_minor) : "");
  const [hasVat, setHasVat] = useState(!!initial?.vat_minor);
  const [vat, setVat] = useState(initial?.vat_minor ? formatAmount(initial.vat_minor) : "");
  const [reference, setReference] = useState(initial?.reference ?? "");
  const total = parseMoney(amount);
  const vatMinor = hasVat ? parseMoney(vat) : 0;
  const ok = !!cat && desc.trim() && total !== null && total > 0 && vatMinor !== null && vatMinor <= (total ?? 0);
  const save = async (submit: boolean) => {
    const r = await act.run(async () => {
      const saved = await api.expenses.save(initial?.expense_id ?? null, {
        category_id: cat,
        business_date: date,
        payee: payee.trim() || null,
        description: desc.trim(),
        total_minor: total ?? 0,
        vat_minor: vatMinor ?? 0,
        reference: reference.trim() || null,
      });
      return submit ? api.expenses.submit(saved.expense_id) : saved;
    });
    if (r) onSaved(r.expense_id);
  };
  return (
    <Modal title={initial ? t("Edit draft {0}", initial.number) : t("Add expense")} onClose={onClose}>
      <div className="col gap-16" data-testid="expense-editor">
        <Field label={t("Category")} required>
          <select className="select" value={cat} onChange={(e) => setCat(e.target.value)} aria-label={t("Category")}>
            <option value="">{t("Choose…")}</option>
            {categories
              .filter((c) => c.active || c.category_id === cat)
              .map((c) => (
                <option key={c.category_id} value={c.category_id}>
                  {catName(c)}
                </option>
              ))}
          </select>
        </Field>
        <TextInput label={t("What for")} required value={desc} onChange={(e) => setDesc(e.target.value)} dir="auto" />
        <div className="form-grid">
          <TextInput
            label={t("Amount paid")}
            required
            inputMode="decimal"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
            hint={t("Including any VAT.")}
          />
          <TextInput
            label={t("Date")}
            type="date"
            value={date}
            max={todayLocal()}
            onChange={(e) => setDate(e.target.value)}
          />
        </div>
        <Checkbox label={t("The bill shows VAT")} checked={hasVat} onChange={setHasVat} />
        {hasVat ? (
          <TextInput
            label={t("VAT on the bill")}
            inputMode="decimal"
            value={vat}
            onChange={(e) => setVat(e.target.value)}
            error={
              vatMinor === null || (total !== null && (vatMinor ?? 0) > total)
                ? t("Enter VAT no larger than the amount.")
                : null
            }
          />
        ) : null}
        <div className="form-grid">
          <TextInput label={t("Paid to")} value={payee} onChange={(e) => setPayee(e.target.value)} dir="auto" />
          <TextInput
            label={t("Bill or reference number")}
            value={reference}
            onChange={(e) => setReference(e.target.value)}
          />
        </div>
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <div className="row">
          <Button onClick={() => save(false)} disabled={!ok} loading={act.busy}>
            {t("Save draft")}
          </Button>
          <Button
            variant="primary"
            className="right"
            onClick={() => save(true)}
            disabled={!ok}
            loading={act.busy}
            data-testid="expense-submit"
          >
            {t("Save and submit")}
          </Button>
        </div>
      </div>
    </Modal>
  );
}

function ExpenseDrawer({
  id,
  categories,
  onClose,
  onChanged,
}: {
  id: string;
  categories: ExpenseCategory[];
  onClose: () => void;
  onChanged: () => void;
}) {
  const { has } = useSession();
  const toast = useToast();
  const { data, error, reload } = useLoad(() => api.expenses.get(id), [id]);
  const act = useAction();
  const [editing, setEditing] = useState(false);
  const [rejecting, setRejecting] = useState(false);
  const [paying, setPaying] = useState(false);
  const [voiding, setVoiding] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [note, setNote] = useState("");
  const [op, setOp] = useState(newOperationId);
  const done = async (r: unknown, msg: string) => {
    if (r !== undefined) {
      toast("success", msg);
      setOp(newOperationId());
      setNote("");
      await reload();
      onChanged();
      return true;
    }
    return false;
  };
  const x = data?.expense;
  const st = x ? expenseStage(x.status) : null;
  return (
    <Drawer title={x ? `${x.number} · ${catName(x)}` : t("Expense")} onClose={onClose}>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {!x || !st ? (
        <Skeleton />
      ) : (
        <div className="col gap-16" data-testid="expense-drawer">
          <div className="row">
            <Chip tone={st.tone}>{st.label}</Chip>
            <strong className="money right">{formatMoney(x.total_minor)}</strong>
          </div>
          <dl className="kv">
            <dt>{t("What for")}</dt>
            <dd dir="auto">{x.description}</dd>
            <dt>{t("Date")}</dt>
            <dd>{formatDate(x.business_date)}</dd>
            <dt>{t("Paid to")}</dt>
            <dd dir="auto">{x.payee ?? "—"}</dd>
            <dt>{t("Without VAT")}</dt>
            <dd>{formatMoney(x.net_minor)}</dd>
            <dt>{t("VAT")}</dt>
            <dd>{formatMoney(x.vat_minor)}</dd>
            {x.reference ? (
              <>
                <dt>{t("Reference")}</dt>
                <dd className="mono">{x.reference}</dd>
              </>
            ) : null}
            <dt>{t("Entered by")}</dt>
            <dd>{x.created_by_name ?? "—"}</dd>
            {x.decided_by_name ? (
              <>
                <dt>{x.status === "rejected" ? t("Rejected by") : t("Approved by")}</dt>
                <dd>
                  {x.decided_by_name}
                  {x.decision_note && x.decision_note !== "Approved on entry" ? ` — ${x.decision_note}` : ""}
                </dd>
              </>
            ) : null}
            {x.status === "paid" ? (
              <>
                <dt>{t("Paid")}</dt>
                <dd>
                  {methodName(x.payment_method)} · {formatShort(x.paid_at)}
                </dd>
              </>
            ) : null}
            {x.void_reason ? (
              <>
                <dt>{t("Void because")}</dt>
                <dd dir="auto">{x.void_reason}</dd>
              </>
            ) : null}
          </dl>
          <div className="col gap-8">
            <strong className="small">{t("Bill")}</strong>
            {data.attachments.length ? (
              data.attachments.map((a) => (
                <Button
                  key={a.attachment_id}
                  size="sm"
                  icon={<Paperclip size={14} />}
                  onClick={async () => {
                    const f = await act.run(() => api.expenses.attachment(a.attachment_id));
                    if (f) downloadBase64(f.file_name, f.base64, f.mime);
                  }}
                >
                  <span dir="auto">{a.file_name}</span>
                </Button>
              ))
            ) : (
              <div className="tiny">{t("No photo or PDF of the bill yet.")}</div>
            )}
            {has("expenses.create") && x.status !== "void" ? (
              <label className="btn sm" style={{ alignSelf: "flex-start" }}>
                <Paperclip size={14} /> {t("Attach the bill")}
                <input
                  type="file"
                  accept="image/*,application/pdf,.pdf"
                  hidden
                  aria-label={t("Attach the bill")}
                  onChange={async (e) => {
                    const f = e.target.files?.[0];
                    if (!f) return;
                    const r = await act.run(async () => api.expenses.attach(id, f.name, await fileToBase64(f)));
                    await done(r, t("Bill attached"));
                  }}
                />
              </label>
            ) : null}
          </div>
          {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          <div className="row wrap">
            {x.status === "draft" && has("expenses.create") ? (
              <>
                <Button
                  variant="primary"
                  loading={act.busy}
                  onClick={async () => done(await act.run(() => api.expenses.submit(id)), t("Submitted"))}
                >
                  {t("Submit")}
                </Button>
                <Button onClick={() => setEditing(true)}>{t("Edit")}</Button>
                <Button variant="danger-outline" className="right" onClick={() => setDeleting(true)}>
                  {t("Delete draft")}
                </Button>
              </>
            ) : null}
            {x.status === "submitted" && has("expenses.approve") ? (
              <>
                <Button
                  variant="primary"
                  loading={act.busy}
                  data-testid="expense-approve"
                  onClick={async () => done(await act.run(() => api.expenses.decide(id, true, null)), t("Approved"))}
                >
                  {t("Approve")}
                </Button>
                <Button onClick={() => setRejecting(true)}>{t("Reject")}</Button>
              </>
            ) : null}
            {x.status === "approved" && has("expenses.pay") ? (
              <Button
                variant="primary"
                icon={<Wallet size={16} />}
                onClick={() => setPaying(true)}
                data-testid="expense-pay"
              >
                {t("Record payment")}
              </Button>
            ) : null}
            {["submitted", "approved", "paid"].includes(x.status) && has("expenses.approve") ? (
              <Button variant="danger-outline" className="right" onClick={() => setVoiding(true)}>
                {t("Void")}
              </Button>
            ) : null}
          </div>
        </div>
      )}
      {editing && x ? (
        <ExpenseEditor
          categories={categories}
          initial={x}
          onClose={() => setEditing(false)}
          onSaved={async () => {
            setEditing(false);
            await reload();
            onChanged();
          }}
        />
      ) : null}
      {rejecting && x ? (
        <Confirm
          title={t("Reject {0}?", x.number)}
          confirmLabel={t("Reject")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setRejecting(false)}
          onConfirm={async () => {
            if (await done(await act.run(() => api.expenses.decide(id, false, note.trim() || null)), t("Rejected")))
              setRejecting(false);
          }}
        >
          <TextInput label={t("Why")} value={note} onChange={(e) => setNote(e.target.value)} autoFocus />
        </Confirm>
      ) : null}
      {voiding && x ? (
        <Confirm
          title={t("Void {0}?", x.number)}
          confirmLabel={t("Void")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setVoiding(false)}
          onConfirm={async () => {
            if (!note.trim()) return act.setError(t("Say why it is voided."));
            if (await done(await act.run(() => api.expenses.void(id, note.trim(), op)), t("Voided"))) setVoiding(false);
          }}
        >
          <div className="col gap-8">
            <div>
              {x.status === "paid" && x.payment_method === "petty_cash"
                ? t(
                    "The expense stops counting and {0} goes back into the petty cash fund. This cannot be undone.",
                    formatMoney(x.total_minor),
                  )
                : t("The expense stops counting. It stays in the records, marked void. This cannot be undone.")}
            </div>
            <TextInput label={t("Why")} value={note} onChange={(e) => setNote(e.target.value)} autoFocus />
          </div>
        </Confirm>
      ) : null}
      {deleting && x ? (
        <Confirm
          title={t("Delete draft {0}?", x.number)}
          confirmLabel={t("Delete draft")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setDeleting(false)}
          onConfirm={async () => {
            if ((await act.run(() => api.expenses.deleteDraft(id))) !== undefined) {
              onChanged();
              onClose();
            }
          }}
        >
          {t("The draft has not been submitted, so nothing else changes.")}
        </Confirm>
      ) : null}
      {paying && x ? (
        <PayExpenseDialog
          expense={x}
          onClose={() => setPaying(false)}
          onPaid={async () => {
            setPaying(false);
            toast("success", t("Payment recorded"));
            await reload();
            onChanged();
          }}
        />
      ) : null}
    </Drawer>
  );
}

function PayExpenseDialog({ expense, onClose, onPaid }: { expense: Expense; onClose: () => void; onPaid: () => void }) {
  const act = useAction();
  const funds = useLoad(() => api.petty.funds(), []);
  const paidOuts = useLoad(() => api.expenses.unlinkedPaidOuts(), []);
  const [method, setMethod] = useState("petty_cash");
  const [fund, setFund] = useState("");
  const [cashEvent, setCashEvent] = useState("");
  const [reference, setReference] = useState("");
  const [op] = useState(newOperationId);
  const open = (funds.data ?? []).filter((f) => f.active);
  const matching = (paidOuts.data ?? []).filter((p) => p.amount_minor === expense.total_minor);
  const chosenFund = open.find((f) => f.fund_id === fund);
  const ready =
    method === "petty_cash"
      ? !!chosenFund && chosenFund.balance_minor >= expense.total_minor
      : method === "till_paid_out"
        ? !!cashEvent
        : true;
  return (
    <Modal title={t("Pay {0}", expense.number)} onClose={onClose}>
      <div className="col gap-16" data-testid="expense-pay-dialog">
        <div>{t("How was {0} paid?", formatMoney(expense.total_minor))}</div>
        <div className="row wrap">
          {PAY_METHODS.map(([m, label]) => (
            <button
              key={m}
              className={`filter-chip ${method === m ? "active" : ""}`}
              aria-pressed={method === m}
              onClick={() => setMethod(m)}
            >
              {label()}
            </button>
          ))}
        </div>
        {method === "petty_cash" ? (
          open.length ? (
            <Field label={t("Fund")}>
              <select className="select" value={fund} onChange={(e) => setFund(e.target.value)} aria-label={t("Fund")}>
                <option value="">{t("Choose…")}</option>
                {open.map((f) => (
                  <option key={f.fund_id} value={f.fund_id}>
                    {f.name} — {formatMoney(f.balance_minor)}
                  </option>
                ))}
              </select>
            </Field>
          ) : (
            <Banner tone="info">{t("There is no petty cash fund yet. Open one on the Petty cash tab.")}</Banner>
          )
        ) : null}
        {chosenFund && chosenFund.balance_minor < expense.total_minor ? (
          <Banner tone="warning">
            {t("The fund holds {0}. Top it up first.", formatMoney(chosenFund.balance_minor))}
          </Banner>
        ) : null}
        {method === "till_paid_out" ? (
          matching.length ? (
            <Field
              label={t("Which paid-out")}
              hint={t("Paid-outs of this exact amount, not linked to another expense.")}
            >
              <select
                className="select"
                value={cashEvent}
                onChange={(e) => setCashEvent(e.target.value)}
                aria-label={t("Which paid-out")}
              >
                <option value="">{t("Choose…")}</option>
                {matching.map((p) => (
                  <option key={p.cash_event_id} value={p.cash_event_id}>
                    {formatShort(p.created_at)} · {p.user ?? ""} · {p.reason}
                  </option>
                ))}
              </select>
            </Field>
          ) : (
            <Banner tone="info">
              {t(
                "No till paid-out of {0} is waiting. Record it at the till first (More → Paid out).",
                formatMoney(expense.total_minor),
              )}
            </Banner>
          )
        ) : null}
        {method !== "petty_cash" && method !== "till_paid_out" ? (
          <TextInput label={t("Reference")} value={reference} onChange={(e) => setReference(e.target.value)} />
        ) : null}
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <Button
          variant="primary"
          disabled={!ready}
          loading={act.busy}
          onClick={async () => {
            const r = await act.run(() =>
              api.expenses.pay(expense.expense_id, {
                method,
                fund_id: method === "petty_cash" ? fund : null,
                cash_event_id: method === "till_paid_out" ? cashEvent : null,
                reference: reference.trim() || null,
                operation_id: op,
              }),
            );
            if (r) onPaid();
          }}
        >
          {t("Record payment of {0}", formatMoney(expense.total_minor))}
        </Button>
      </div>
    </Modal>
  );
}

function PettyCashPanel() {
  const { has } = useSession();
  const toast = useToast();
  const funds = useLoad(() => api.petty.funds(), []);
  const [sel, setSel] = useState<string | null>(null);
  const [newFund, setNewFund] = useState(false);
  const [name, setName] = useState("");
  const [move, setMove] = useState<{ fund: PettyFund; kind: "open" | "top_up" | "reimburse" | "count" } | null>(null);
  const [amount, setAmount] = useState("");
  const [note, setNote] = useState("");
  const [op, setOp] = useState(newOperationId);
  const act = useAction();
  const entries = useLoad(() => (sel ? api.petty.entries(sel) : Promise.resolve([])), [sel]);
  const list = funds.data ?? [];
  const current = list.find((f) => f.fund_id === sel) ?? null;
  const kindLabel: Record<string, () => string> = {
    open: () => t("Opening amount"),
    top_up: () => t("Top up"),
    reimburse: () => t("Hand back"),
    adjust: () => t("Adjustment"),
    count: () => t("Count"),
    expense: () => t("Expense"),
    void: () => t("Expense voided"),
  };
  const amt = parseMoney(amount);
  return (
    <div className="col gap-16">
      {funds.error ? <Banner tone="danger">{funds.error}</Banner> : null}
      <div className="row wrap">
        {list.map((f) => (
          <button
            key={f.fund_id}
            className={`card card-pad col ${sel === f.fund_id ? "selected" : ""}`}
            style={{ minWidth: 200, textAlign: "start" }}
            onClick={() => setSel(f.fund_id)}
            data-testid="petty-fund"
          >
            <strong dir="auto">{f.name}</strong>
            <span className="money" style={{ fontSize: 22 }}>
              {formatMoney(f.balance_minor)}
            </span>
            <span className="tiny">
              {f.last_count_at ? t("Counted {0}", formatShort(f.last_count_at)) : t("Not counted yet")}
            </span>
          </button>
        ))}
        {has("petty_cash.manage") ? (
          <Button icon={<Plus size={16} />} onClick={() => setNewFund(true)}>
            {t("New fund")}
          </Button>
        ) : null}
      </div>
      {!list.length && !funds.loading ? (
        <Empty title={t("No petty cash fund")}>
          {t(
            "A petty cash fund is cash kept apart from the till for small shop costs. Open one with the amount it starts with.",
          )}
        </Empty>
      ) : null}
      {current ? (
        <div className="card card-pad col gap-16">
          <div className="row wrap">
            <h3 className="grow" dir="auto">
              {current.name}
            </h3>
            {has("petty_cash.manage") ? (
              <>
                <Button
                  size="sm"
                  onClick={() => (
                    setOp(newOperationId()),
                    setAmount(""),
                    setNote(""),
                    setMove({ fund: current, kind: entries.data?.length ? "top_up" : "open" })
                  )}
                >
                  {entries.data?.length ? t("Top up") : t("Opening amount")}
                </Button>
                <Button
                  size="sm"
                  onClick={() => (
                    setOp(newOperationId()),
                    setAmount(""),
                    setNote(""),
                    setMove({ fund: current, kind: "reimburse" })
                  )}
                >
                  {t("Hand back")}
                </Button>
                <Button
                  size="sm"
                  variant="primary"
                  onClick={() => (
                    setOp(newOperationId()),
                    setAmount(""),
                    setNote(""),
                    setMove({ fund: current, kind: "count" })
                  )}
                >
                  {t("Count")}
                </Button>
              </>
            ) : null}
          </div>
          <table className="table">
            <thead>
              <tr>
                <th>{t("When")}</th>
                <th>{t("What")}</th>
                <th className="num">{t("Amount")}</th>
                <th className="num">{t("Balance")}</th>
              </tr>
            </thead>
            <tbody>
              {(entries.data ?? []).map((e) => (
                <tr key={e.entry_id}>
                  <td>{formatShort(e.created_at)}</td>
                  <td>
                    {kindLabel[e.kind]?.() ?? e.kind}
                    {e.expense_number ? <span className="mono"> {e.expense_number}</span> : null}
                    {e.note && e.kind !== "expense" ? (
                      <div className="tiny" dir="auto">
                        {e.note}
                      </div>
                    ) : null}
                  </td>
                  <td className={`num ${e.amount_minor < 0 ? "neg-num" : ""}`}>{formatMoney(e.amount_minor)}</td>
                  <td className="num">{formatMoney(e.balance_minor)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
      {newFund ? (
        <Modal title={t("New petty cash fund")} onClose={() => setNewFund(false)}>
          <div className="col gap-16">
            <TextInput
              label={t("Name")}
              value={name}
              onChange={(e) => setName(e.target.value)}
              autoFocus
              dir="auto"
              hint={t("For example: Front desk")}
            />
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
            <Button
              variant="primary"
              disabled={!name.trim()}
              loading={act.busy}
              onClick={async () => {
                const r = await act.run(() => api.petty.fundSave(null, name.trim(), null, true));
                if (r) {
                  setNewFund(false);
                  setName("");
                  await funds.reload();
                  setSel(r.fund_id);
                }
              }}
            >
              {t("Create fund")}
            </Button>
          </div>
        </Modal>
      ) : null}
      {move ? (
        <Modal title={`${kindLabel[move.kind]()} — ${move.fund.name}`} onClose={() => setMove(null)}>
          <div className="col gap-16" data-testid="petty-move">
            {move.kind === "count" ? (
              <div>{t("Count the cash in the fund. It should hold {0}.", formatMoney(move.fund.balance_minor))}</div>
            ) : null}
            <TextInput
              label={move.kind === "count" ? t("Cash counted") : t("Amount")}
              inputMode="decimal"
              value={amount}
              onChange={(e) => setAmount(e.target.value)}
              autoFocus
            />
            {move.kind === "count" && amt !== null && amount ? (
              <div className={amt - move.fund.balance_minor ? "neg-num" : ""}>
                {amt === move.fund.balance_minor
                  ? t("Matches.")
                  : t("Difference {0}", formatMoney(amt - move.fund.balance_minor))}
              </div>
            ) : null}
            <TextInput label={t("Note")} value={note} onChange={(e) => setNote(e.target.value)} dir="auto" />
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
            <Button
              variant="primary"
              disabled={amt === null || (move.kind !== "count" && (amt ?? 0) <= 0)}
              loading={act.busy}
              onClick={async () => {
                const r = await act.run<unknown>(() =>
                  move.kind === "count"
                    ? api.petty.count(move.fund.fund_id, amt ?? 0, note.trim() || null, op)
                    : api.petty.entry(move.fund.fund_id, move.kind, amt ?? 0, note.trim() || null, op),
                );
                if (r) {
                  toast("success", t("Saved"));
                  setMove(null);
                  await funds.reload();
                  await entries.reload();
                }
              }}
            >
              {t("Save")}
            </Button>
          </div>
        </Modal>
      ) : null}
    </div>
  );
}

function RepeatsPanel({ categories }: { categories: ExpenseCategory[] }) {
  const { has } = useSession();
  const list = useLoad(() => api.expenses.recurring(), []);
  const [edit, setEdit] = useState<ExpenseRecurring | "new" | null>(null);
  const weekdays = useMemo(
    () => [t("Monday"), t("Tuesday"), t("Wednesday"), t("Thursday"), t("Friday"), t("Saturday"), t("Sunday")],
    [],
  );
  const when = (r: ExpenseRecurring) =>
    r.cadence === "monthly" ? t("Every month on day {0}", r.day) : t("Every {0}", weekdays[r.day - 1]);
  return (
    <div className="col gap-16">
      <div className="row">
        <div className="tiny grow">
          {t("A repeat makes a draft on its day for you to check and submit. Nothing is paid by itself.")}
        </div>
        {has("expenses.approve") ? (
          <Button icon={<Repeat size={16} />} onClick={() => setEdit("new")}>
            {t("New repeat")}
          </Button>
        ) : null}
      </div>
      <DataTable<ExpenseRecurring>
        rows={list.data}
        loading={list.loading}
        rowKey={(r) => r.recurring_id}
        onRowClick={has("expenses.approve") ? setEdit : undefined}
        empty={
          <Empty title={t("No repeats")}>
            {t("Add rent or salaries once and a draft appears each time it is due.")}
          </Empty>
        }
        columns={[
          { key: "n", label: t("Name"), render: (r) => <span dir="auto">{r.name}</span> },
          { key: "c", label: t("Category"), render: (r) => <span dir="auto">{catName(r)}</span> },
          { key: "w", label: t("When"), render: when },
          { key: "x", label: t("Next draft"), render: (r) => (r.active ? formatDate(r.next_date) : t("Stopped")) },
          { key: "a", label: t("Amount"), num: true, render: (r) => formatMoney(r.total_minor) },
        ]}
      />
      {edit ? (
        <RepeatEditor
          categories={categories}
          initial={edit === "new" ? null : edit}
          weekdays={weekdays}
          onClose={() => setEdit(null)}
          onSaved={() => {
            setEdit(null);
            void list.reload();
          }}
        />
      ) : null}
    </div>
  );
}

function RepeatEditor({
  categories,
  initial,
  weekdays,
  onClose,
  onSaved,
}: {
  categories: ExpenseCategory[];
  initial: ExpenseRecurring | null;
  weekdays: string[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const act = useAction();
  const [name, setName] = useState(initial?.name ?? "");
  const [cat, setCat] = useState(initial?.category_id ?? "");
  const [desc, setDesc] = useState(initial?.description ?? "");
  const [payee, setPayee] = useState(initial?.payee ?? "");
  const [amount, setAmount] = useState(initial ? formatAmount(initial.total_minor) : "");
  const [cadence, setCadence] = useState<"monthly" | "weekly">(initial?.cadence ?? "monthly");
  const [day, setDay] = useState(initial?.day ?? 1);
  const [active, setActive] = useState(initial?.active ?? true);
  const total = parseMoney(amount);
  return (
    <Modal title={initial ? t("Edit repeat") : t("New repeat")} onClose={onClose}>
      <div className="col gap-16">
        <TextInput
          label={t("Name")}
          value={name}
          onChange={(e) => setName(e.target.value)}
          dir="auto"
          hint={t("For example: Shop rent")}
        />
        <Field label={t("Category")} required>
          <select className="select" value={cat} onChange={(e) => setCat(e.target.value)} aria-label={t("Category")}>
            <option value="">{t("Choose…")}</option>
            {categories
              .filter((c) => c.active)
              .map((c) => (
                <option key={c.category_id} value={c.category_id}>
                  {catName(c)}
                </option>
              ))}
          </select>
        </Field>
        <TextInput label={t("What for")} value={desc} onChange={(e) => setDesc(e.target.value)} dir="auto" />
        <div className="form-grid">
          <TextInput
            label={t("Amount")}
            inputMode="decimal"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
          />
          <TextInput label={t("Paid to")} value={payee} onChange={(e) => setPayee(e.target.value)} dir="auto" />
        </div>
        <div className="form-grid">
          <Field label={t("Repeats")}>
            <select
              className="select"
              value={cadence}
              onChange={(e) => {
                setCadence(e.target.value as "monthly" | "weekly");
                setDay(1);
              }}
              aria-label={t("Repeats")}
            >
              <option value="monthly">{t("Every month")}</option>
              <option value="weekly">{t("Every week")}</option>
            </select>
          </Field>
          <Field label={cadence === "monthly" ? t("Day of the month") : t("Day of the week")}>
            <select
              className="select"
              value={day}
              onChange={(e) => setDay(Number(e.target.value))}
              aria-label={t("Day")}
            >
              {cadence === "monthly"
                ? Array.from({ length: 28 }, (_, i) => (
                    <option key={i + 1} value={i + 1}>
                      {i + 1}
                    </option>
                  ))
                : weekdays.map((w, i) => (
                    <option key={w} value={i + 1}>
                      {w}
                    </option>
                  ))}
            </select>
          </Field>
        </div>
        {initial ? <Checkbox label={t("Active")} checked={active} onChange={setActive} /> : null}
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <Button
          variant="primary"
          disabled={!name.trim() || !cat || !desc.trim() || !total}
          loading={act.busy}
          onClick={async () => {
            const r = await act.run(() =>
              api.expenses.recurringSave(initial?.recurring_id ?? null, {
                name: name.trim(),
                category_id: cat,
                payee: payee.trim() || null,
                description: desc.trim(),
                total_minor: total ?? 0,
                vat_minor: 0,
                cadence,
                day,
                active,
              }),
            );
            if (r) onSaved();
          }}
        >
          {t("Save")}
        </Button>
      </div>
    </Modal>
  );
}
