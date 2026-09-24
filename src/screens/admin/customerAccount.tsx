import { useState } from "react";
import { Star, Trash2 } from "lucide-react";
import { api } from "../../api";
import type { CustomerAddress } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { Banner, Button, Checkbox, Chip, Field, Skeleton, TextInput } from "../../components/ui";
import { Confirm, DataTable, useAction, useLoad } from "./common";
import { formatMoney, parseMoney } from "../../lib/money";
import { formatDateTime } from "../../lib/time";
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
            value={limit ?? formatMoney(a.credit_limit_minor).split(" ").pop() ?? ""}
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
