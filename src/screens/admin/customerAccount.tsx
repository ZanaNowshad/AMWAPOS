import { useState } from "react";
import { Star, Trash2 } from "lucide-react";
import { api } from "../../api";
import type { Ageing, CustomerAddress } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { Banner, Button, Checkbox, Chip, Field, Skeleton, TextInput } from "../../components/ui";
import { Confirm, DataTable, DateRange, downloadBase64, useAction, useLoad } from "./common";
import { formatAmount, formatMoney, parseMoney } from "../../lib/money";
import { formatDate, formatDateTime, todayLocal } from "../../lib/time";
import { newOperationId } from "../../lib/ids";
import { methodLabel } from "../pos/labels";
import { t } from "../../i18n";

export function AddressesTab({ customerId }: { customerId: string }) {
  const { has } = useSession();
  const { data, error, setData, reload } = useLoad(() => api.customers.account(customerId), [customerId]);
  const [edit, setEdit] = useState<Partial<CustomerAddress> | null>(null);
  const act = useAction();
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton />;
  return (
    <div className="col gap-16">
      {has("customers.manage") ? (
        <div>
          <Button onClick={() => setEdit({ label: "", address: "", is_default: data.addresses.length === 0 })}>
            {t("Add address")}
          </Button>
        </div>
      ) : null}
      {data.addresses.length === 0 ? <div className="empty">{t("No saved addresses.")}</div> : null}
      {data.addresses.map((a) => (
        <div key={a.address_id} className="card card-pad row">
          <div className="grow">
            <div className="row">
              <strong>{a.label}</strong>
              {a.is_default ? (
                <Chip tone="brand">
                  <Star size={12} /> {t("Default")}
                </Chip>
              ) : null}
            </div>
            <div dir="auto">{[a.area, a.address].filter(Boolean).join(" · ")}</div>
            {a.notes ? <div className="tiny">{a.notes}</div> : null}
          </div>
          {has("customers.manage") ? (
            <>
              <Button size="sm" onClick={() => setEdit(a)}>
                {t("Edit")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                aria-label={t("Delete")}
                icon={<Trash2 size={14} />}
                onClick={async () => {
                  if ((await act.run(() => api.customers.addressDelete(a.address_id))) !== undefined) void reload();
                }}
              />
            </>
          ) : null}
        </div>
      ))}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {edit ? (
        <Confirm
          title={edit.address_id ? t("Edit address") : t("Add address")}
          confirmLabel={t("Save")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setEdit(null)}
          onConfirm={async () => {
            const r = await act.run(() =>
              api.customers.addressSave({
                address_id: edit.address_id ?? null,
                customer_id: customerId,
                label: edit.label ?? "",
                area: edit.area ?? null,
                address: edit.address ?? "",
                notes: edit.notes ?? null,
                is_default: !!edit.is_default,
              }),
            );
            if (r) {
              setData(r);
              setEdit(null);
            }
          }}
        >
          <div className="col gap-16">
            <TextInput
              label={t("Label")}
              placeholder={t("Home, Work…")}
              value={edit.label ?? ""}
              onChange={(e) => setEdit({ ...edit, label: e.target.value })}
            />
            <TextInput
              label={t("Area")}
              value={edit.area ?? ""}
              onChange={(e) => setEdit({ ...edit, area: e.target.value })}
            />
            <TextInput
              label={t("Address")}
              value={edit.address ?? ""}
              onChange={(e) => setEdit({ ...edit, address: e.target.value })}
            />
            <TextInput
              label={t("Directions")}
              value={edit.notes ?? ""}
              onChange={(e) => setEdit({ ...edit, notes: e.target.value })}
            />
            <Checkbox
              label={t("Default delivery address")}
              checked={!!edit.is_default}
              onChange={(x) => setEdit({ ...edit, is_default: x })}
            />
          </div>
        </Confirm>
      ) : null}
    </div>
  );
}

const KIND: Record<string, () => string> = {
  sale: () => t("Sale on account"),
  payment: () => t("Payment"),
  refund: () => t("Refund"),
  adjustment: () => t("Adjustment"),
};

export function AccountTab({ customerId }: { customerId: string }) {
  const { has, config } = useSession();
  const toast = useToast();
  const { data, error, setData, reload } = useLoad(() => api.customers.account(customerId), [customerId]);
  const [limit, setLimit] = useState<string | null>(null);
  const [pay, setPay] = useState<{ amount: string; method: string; reference: string; op: string } | null>(null);
  const [adjust, setAdjust] = useState<{ amount: string; note: string; op: string } | null>(null);
  const act = useAction();
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton />;
  const a = data.account ?? { enabled: false, credit_limit_minor: 0, balance_minor: 0, available_minor: 0 };
  const canCredit = has("customers.credit");
  const methods = (config?.payments ?? []).filter((p) => p.method !== "account");
  return (
    <div className="col gap-16">
      <div className="grid-3" style={{ gap: 16 }}>
        <div className="card card-pad">
          <div className="tiny">{t("Balance owed")}</div>
          <div className="kpi" data-testid="account-balance">
            {formatMoney(a.balance_minor)}
          </div>
        </div>
        <div className="card card-pad">
          <div className="tiny">{t("Credit limit")}</div>
          <div className="kpi">{formatMoney(a.credit_limit_minor)}</div>
        </div>
        <div className="card card-pad">
          <div className="tiny">{t("Available")}</div>
          <div className="kpi">{formatMoney(a.available_minor)}</div>
        </div>
      </div>
      {!a.enabled ? (
        <Banner tone="info">{t("This customer cannot buy on account until the account is enabled.")}</Banner>
      ) : null}
      {canCredit ? (
        <div className="card card-pad row wrap" style={{ alignItems: "flex-end" }}>
          <TextInput
            label={t("Credit limit")}
            className="num"
            value={limit ?? (a.credit_limit_minor === null ? "" : formatAmount(a.credit_limit_minor))}
            onChange={(e) => setLimit(e.target.value)}
          />
          <Button
            variant="primary"
            loading={act.busy}
            onClick={async () => {
              const l = limit === null ? a.credit_limit_minor : parseMoney(limit);
              if (l === null) return act.setError(t("Enter a valid amount."));
              const r = await act.run(() => api.customers.accountSet(customerId, true, l));
              if (r) {
                setData(r);
                setLimit(null);
                toast("success", t("Account saved"));
              }
            }}
          >
            {a.enabled ? t("Save limit") : t("Enable account")}
          </Button>
          {a.enabled ? (
            <Button
              onClick={async () => {
                const r = await act.run(() => api.customers.accountSet(customerId, false, a.credit_limit_minor));
                if (r) setData(r);
              }}
            >
              {t("Disable account")}
            </Button>
          ) : null}
          <Button
            disabled={a.balance_minor <= 0}
            onClick={() =>
              setPay({ amount: "", method: methods[0]?.method ?? "cash", reference: "", op: newOperationId() })
            }
          >
            {t("Take payment")}
          </Button>
          {has("customers.credit_override") ? (
            <Button variant="ghost" onClick={() => setAdjust({ amount: "", note: "", op: newOperationId() })}>
              {t("Adjust balance")}
            </Button>
          ) : null}
        </div>
      ) : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <DataTable
        rows={data.ledger}
        rowKey={(r) => r.entry_id}
        empty={<div className="empty">{t("No account activity.")}</div>}
        columns={[
          { key: "at", label: t("Date"), render: (r) => formatDateTime(r.created_at) },
          { key: "kind", label: t("Type"), render: (r) => KIND[r.kind]?.() ?? r.kind },
          { key: "ref", label: t("Reference"), render: (r) => r.reference ?? (r.method ? methodLabel(r.method) : "") },
          { key: "note", label: t("Note"), render: (r) => r.note ?? "" },
          { key: "user", label: t("By"), render: (r) => r.user ?? "" },
          { key: "amt", label: t("Amount"), num: true, render: (r) => formatMoney(r.amount_minor) },
        ]}
      />
      <StatementCard customerId={customerId} canCredit={canCredit} />
      {pay ? (
        <Confirm
          title={t("Take payment")}
          confirmLabel={t("Record payment")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setPay(null)}
          onConfirm={async () => {
            const amt = parseMoney(pay.amount);
            if (!amt) return act.setError(t("Enter a valid amount."));
            const r = await act.run(() =>
              api.customers.accountPayment({
                customer_id: customerId,
                amount_minor: amt,
                method: pay.method,
                reference: pay.reference || null,
                operation_id: pay.op,
              }),
            );
            if (r) {
              setPay(null);
              toast("success", t("Payment recorded"));
              void reload();
            }
          }}
        >
          <div className="col gap-16">
            <TextInput
              label={t("Amount")}
              className="num"
              value={pay.amount}
              onChange={(e) => setPay({ ...pay, amount: e.target.value })}
            />
            <Field label={t("Payment method")}>
              <select
                className="select"
                value={pay.method}
                onChange={(e) => setPay({ ...pay, method: e.target.value })}
              >
                {methods.map((m) => (
                  <option key={m.method} value={m.method}>
                    {methodLabel(m.method)}
                  </option>
                ))}
              </select>
            </Field>
            <TextInput
              label={t("Reference")}
              value={pay.reference}
              onChange={(e) => setPay({ ...pay, reference: e.target.value })}
            />
            {pay.method === "cash" ? (
              <div className="tiny">{t("Cash payments are added to the open shift's drawer.")}</div>
            ) : null}
          </div>
        </Confirm>
      ) : null}
      {adjust ? (
        <Confirm
          title={t("Adjust balance")}
          confirmLabel={t("Record adjustment")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setAdjust(null)}
          onConfirm={async () => {
            const neg = adjust.amount.trim().startsWith("-");
            const amt = parseMoney(adjust.amount.replace("-", ""));
            if (!amt) return act.setError(t("Enter a valid amount."));
            const r = await act.run(() =>
              api.customers.accountAdjust(customerId, neg ? -amt : amt, adjust.note, adjust.op),
            );
            if (r) {
              setAdjust(null);
              void reload();
            }
          }}
        >
          <div className="col gap-16">
            <TextInput
              label={t("Amount")}
              className="num"
              hint={t("Positive adds to what the customer owes; negative reduces it.")}
              value={adjust.amount}
              onChange={(e) => setAdjust({ ...adjust, amount: e.target.value })}
            />
            <TextInput
              label={t("Reason")}
              value={adjust.note}
              onChange={(e) => setAdjust({ ...adjust, note: e.target.value })}
            />
          </div>
        </Confirm>
      ) : null}
    </div>
  );
}

const AGE_LABELS: [keyof Ageing, () => string][] = [
  ["current_minor", () => t("Not due yet")],
  ["d1_30_minor", () => t("1–30 days late")],
  ["d31_60_minor", () => t("31–60 days late")],
  ["d61_90_minor", () => t("61–90 days late")],
  ["d90_plus_minor", () => t("Over 90 days late")],
];

/** Account statement for a period: opening balance, every entry with the
 *  running balance, closing balance and how late what is owed is. */
function StatementCard({ customerId, canCredit }: { customerId: string; canCredit: boolean }) {
  const { has } = useSession();
  const toast = useToast();
  const [from, setFrom] = useState(todayLocal().slice(0, 8) + "01");
  const [to, setTo] = useState(todayLocal());
  const st = useLoad(() => api.statements.get(customerId, from, to), [customerId, from, to]);
  const act = useAction();
  const [terms, setTerms] = useState<string | null>(null);
  const d = st.data;
  const kindLabel = (k: string) => KIND[k]?.() ?? k;
  return (
    <div className="card card-pad col gap-16" data-testid="statement">
      <div className="row wrap">
        <h3 className="grow">{t("Statement")}</h3>
        <Button
          size="sm"
          loading={act.busy}
          onClick={async () => {
            const f = await act.run(() => api.statements.pdf(customerId, from, to));
            if (f) downloadBase64(f.file_name, f.base64, "application/pdf");
          }}
        >
          {t("PDF")}
        </Button>
        {has("whatsapp.manage") && d?.phone ? (
          <Button
            size="sm"
            loading={act.busy}
            onClick={async () => {
              const f = await act.run(() => api.statements.pdf(customerId, from, to));
              if (!f || !d.phone) return;
              const r = await act.run(() =>
                api.whatsapp.queue({
                  operation_id: newOperationId(),
                  kind: "document",
                  customer_id: customerId,
                  to_phone: d.phone,
                  document_b64: f.base64,
                  document_name: f.file_name,
                  text: t("Your account statement, {0} to {1}.", formatDate(from), formatDate(to)),
                }),
              );
              if (r) toast("success", t("Statement queued for WhatsApp"));
            }}
          >
            {t("Send on WhatsApp")}
          </Button>
        ) : null}
      </div>
      <DateRange from={from} to={to} onChange={(a, b) => (setFrom(a), setTo(b))} />
      {st.error ? <Banner tone="danger">{st.error}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {!d ? (
        <Skeleton />
      ) : (
        <>
          <div className="ap-ageing" aria-label={t("Owed by how late it is")}>
            {AGE_LABELS.map(([k, label]) => (
              <div key={k} className={`ap-age ${k !== "current_minor" && d.ageing[k] ? "late" : ""}`}>
                <span className="tiny">{label()}</span>
                <strong className="money">{formatMoney(d.ageing[k])}</strong>
              </div>
            ))}
          </div>
          <table className="table">
            <thead>
              <tr>
                <th>{t("Date")}</th>
                <th>{t("Type")}</th>
                <th>{t("Reference")}</th>
                <th className="num">{t("Charged")}</th>
                <th className="num">{t("Paid or credited")}</th>
                <th className="num">{t("Balance")}</th>
              </tr>
            </thead>
            <tbody>
              <tr>
                <td>{formatDate(d.from)}</td>
                <td colSpan={4}>{t("Opening balance")}</td>
                <td className="num">{formatMoney(d.opening_minor)}</td>
              </tr>
              {d.lines.map((l, i) => (
                <tr key={i}>
                  <td>{formatDate(l.date)}</td>
                  <td>{kindLabel(l.kind)}</td>
                  <td className="mono">{l.reference ?? (l.method ? methodLabel(l.method) : "")}</td>
                  <td className="num">{l.charge_minor ? formatMoney(l.charge_minor) : ""}</td>
                  <td className="num">{l.credit_minor ? formatMoney(l.credit_minor) : ""}</td>
                  <td className="num">{formatMoney(l.balance_minor)}</td>
                </tr>
              ))}
              <tr>
                <td>{formatDate(d.to)}</td>
                <td colSpan={4}>
                  <strong>{t("Closing balance")}</strong>
                </td>
                <td className="num">
                  <strong>{formatMoney(d.closing_minor)}</strong>
                </td>
              </tr>
            </tbody>
          </table>
          {canCredit ? (
            <div className="row wrap" style={{ alignItems: "flex-end" }}>
              <TextInput
                label={t("Days to pay")}
                hint={t("A charge counts as late after this many days.")}
                inputMode="numeric"
                value={terms ?? String(d.terms_days)}
                onChange={(e) => setTerms(e.target.value.replace(/\D/g, "").slice(0, 3))}
              />
              <Button
                disabled={terms === null || terms === String(d.terms_days)}
                loading={act.busy}
                onClick={async () => {
                  const r = await act.run(() => api.statements.setTerms(customerId, Number(terms)));
                  if (r) {
                    setTerms(null);
                    void st.reload();
                    toast("success", t("Saved"));
                  }
                }}
              >
                {t("Save")}
              </Button>
            </div>
          ) : null}
        </>
      )}
    </div>
  );
}
