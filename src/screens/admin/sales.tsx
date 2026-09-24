import { useState } from "react";
import { AlertTriangle, Printer } from "lucide-react";
import { api } from "../../api";
import type { SaleRow, ShiftSummary } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { formatMoney, formatQty } from "../../lib/money";
import { formatDateTime, formatShort, todayLocal } from "../../lib/time";
import { Banner, Button, Chip, Money, PageHeader, Skeleton } from "../../components/ui";
import { DataTable, DateRange, Drawer, Pager, useLoad } from "./common";
import { methodLabel } from "../pos/labels";
import { t } from "../../i18n";

export function SaleDrawer({ saleId, onClose }: { saleId: string; onClose: () => void }) {
  const { has } = useSession();
  const toast = useToast();
  const { data: s, error } = useLoad(() => api.sales.get(saleId), [saleId]);
  const { data: receipt } = useLoad(() => api.receipts.preview("sale", saleId), [saleId]);
  const [tab, setTab] = useState<"details" | "receipt">("details");
  return (
    <Drawer
      title={s ? t("Receipt {0}", s.receipt_number) : t("Sale")}
      onClose={onClose}
      actions={
        has("pos.reprint") ? (
          <Button
            size="sm"
            icon={<Printer size={14} />}
            onClick={async () => {
              const r = await api.sales.reprint(saleId);
              toast(
                r.status === "printed" ? "success" : "warning",
                r.status === "printed" ? t("Reprinted") : t("Not printed"),
                r.message ?? undefined,
              );
            }}
          >
            {t("Reprint")}
          </Button>
        ) : null
      }
    >
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {!s ? (
        <Skeleton />
      ) : (
        <div className="stack-16">
          <div className="row">
            <button className={`filter-chip ${tab === "details" ? "active" : ""}`} onClick={() => setTab("details")}>
              {t("Details")}
            </button>
            <button className={`filter-chip ${tab === "receipt" ? "active" : ""}`} onClick={() => setTab("receipt")}>
              {t("Receipt preview")}
            </button>
          </div>
          {tab === "receipt" ? (
            <div className="receipt-stage">
              <div className="receipt-paper" style={{ width: "fit-content" }}>
                {receipt?.text}
              </div>
            </div>
          ) : (
            <>
              <dl className="kv">
                <dt>{t("Status")}</dt>
                <dd>
                  {s.refunds.length ? (
                    <Chip tone="warning">{t("Refunds recorded")}</Chip>
                  ) : (
                    <Chip tone="success">{t("Completed")}</Chip>
                  )}
                </dd>
                <dt>{t("Date")}</dt>
                <dd>{formatDateTime(s.completed_at)}</dd>
                <dt>{t("Cashier")}</dt>
                <dd>{s.cashier_name}</dd>
                <dt>{t("Terminal")}</dt>
                <dd>{s.device_name}</dd>
                <dt>{t("Customer")}</dt>
                <dd>{s.customer_name ? `${s.customer_name} ${s.customer_phone ?? ""}` : "—"}</dd>
              </dl>
              <table className="table">
                <thead>
                  <tr>
                    <th>{t("Item")}</th>
                    <th className="num">{t("Qty")}</th>
                    <th className="num">{t("Price")}</th>
                    <th className="num">{t("Disc.")}</th>
                    <th className="num">{t("VAT")}</th>
                    <th className="num">{t("Total")}</th>
                  </tr>
                </thead>
                <tbody>
                  {s.items.map((i) => (
                    <tr key={i.sale_item_id}>
                      <td>
                        {i.name}
                        {i.refunded_qty_milli ? (
                          <div className="tiny">{t("Refunded {0}", formatQty(i.refunded_qty_milli))}</div>
                        ) : null}
                      </td>
                      <td className="num">{formatQty(i.qty_milli)}</td>
                      <td className="num">{formatMoney(i.unit_price_minor)}</td>
                      <td className="num">{i.discount_minor ? formatMoney(i.discount_minor) : "—"}</td>
                      <td className="num">{formatMoney(i.tax_minor)}</td>
                      <td className="num">{formatMoney(i.line_total_minor)}</td>
                    </tr>
                  ))}
                </tbody>
                <tfoot>
                  <tr>
                    <td colSpan={4}>{t("Total (VAT {0})", formatMoney(s.tax_minor))}</td>
                    <td />
                    <td className="num">{formatMoney(s.total_minor)}</td>
                  </tr>
                </tfoot>
              </table>
              <div>
                <h3 style={{ marginBottom: 6 }}>{t("Payments")}</h3>
                {s.payments.map((p, i) => (
                  <div key={i} className="row small">
                    <span className="grow">
                      {methodLabel(p.method)} {p.reference ? t("· ref {0}", p.reference) : ""}
                    </span>
                    <span className="num">
                      {formatMoney(p.amount_minor)}
                      {p.change_minor
                        ? t(" (tendered {0}, change {1})", formatMoney(p.tendered_minor), formatMoney(p.change_minor))
                        : ""}
                    </span>
                  </div>
                ))}
                <div className="tiny" style={{ marginTop: 6 }}>
                  {t("Card and BenefitPay are recorded tenders, not verified settlements.")}
                </div>
              </div>
              {s.cost_total_minor !== null ? (
                <dl className="kv">
                  <dt>{t("Cost (snapshot)")}</dt>
                  <dd>{formatMoney(s.cost_total_minor)}</dd>
                  <dt>{t("Gross profit")}</dt>
                  <dd>{formatMoney(s.total_minor - s.tax_minor - s.cost_total_minor)}</dd>
                </dl>
              ) : null}
              {s.refunds.length ? (
                <div>
                  <h3 style={{ marginBottom: 6 }}>{t("Refunds")}</h3>
                  {s.refunds.map((r) => (
                    <div key={r.refund_id} className="row small">
                      <span className="mono grow">{r.refund_receipt_number}</span>
                      <span>{formatShort(r.created_at)}</span>
                      <span className="num">{formatMoney(-r.total_minor)}</span>
                    </div>
                  ))}
                </div>
              ) : null}
            </>
          )}
        </div>
      )}
    </Drawer>
  );
}

export function SalesPage() {
  const [from, setFrom] = useState(todayLocal());
  const [to, setTo] = useState(todayLocal());
  const [receipt, setReceipt] = useState("");
  const [method, setMethod] = useState("");
  const [offset, setOffset] = useState(0);
  const [limit, setLimit] = useState(50);
  const [open, setOpen] = useState<string | null>(null);
  const { data, loading, error } = useLoad(
    () => api.sales.list({ from, to, receipt: receipt || undefined, method: method || undefined, limit, offset }),
    [from, to, receipt, method, limit, offset],
  );
  return (
    <div>
      <PageHeader
        title={t("Sales")}
        subtitle={t("Completed sales. Records are immutable; corrections are made with refunds.")}
      />
      <div className="filters">
        <DateRange from={from} to={to} onChange={(a, b) => (setFrom(a), setTo(b), setOffset(0))} />
        <input
          className="input"
          style={{ width: 200 }}
          placeholder={t("Receipt number")}
          value={receipt}
          onChange={(e) => (setReceipt(e.target.value), setOffset(0))}
        />
        <select
          className="select"
          style={{ width: 160 }}
          value={method}
          onChange={(e) => (setMethod(e.target.value), setOffset(0))}
          aria-label={t("Payment method")}
        >
          <option value="">{t("All payments")}</option>
          {["cash", "card", "benefitpay", "bank_transfer", "wallet"].map((m) => (
            <option key={m} value={m}>
              {methodLabel(m)}
            </option>
          ))}
        </select>
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<SaleRow>
        rows={data?.rows ?? null}
        loading={loading}
        rowKey={(r) => r.sale_id}
        onRowClick={(r) => setOpen(r.sale_id)}
        empty={<div className="empty">{t("No sales in this period.")}</div>}
        columns={[
          {
            key: "r",
            label: t("Receipt"),
            render: (r) => <span className="mono">{r.receipt_number}</span>,
            sort: (r) => r.receipt_number,
          },
          { key: "t", label: t("Time"), render: (r) => formatShort(r.completed_at), sort: (r) => r.completed_at },
          { key: "c", label: t("Cashier"), render: (r) => r.cashier_name, sort: (r) => r.cashier_name },
          { key: "d", label: t("Terminal"), render: (r) => r.device_name ?? "—" },
          { key: "cu", label: t("Customer"), render: (r) => r.customer_name ?? "—" },
          { key: "i", label: t("Items"), num: true, render: (r) => formatQty(r.item_count_milli) },
          { key: "p", label: t("Payment"), render: (r) => r.methods.split(",").map(methodLabel).join(", ") },
          {
            key: "tot",
            label: t("Total"),
            num: true,
            render: (r) => <Money minor={r.total_minor} />,
            sort: (r) => r.total_minor,
          },
          {
            key: "s",
            label: t("Status"),
            render: (r) =>
              r.status === "completed" ? (
                <Chip tone="success">{t("Completed")}</Chip>
              ) : (
                <Chip tone="warning">{r.status === "refunded" ? t("Refunded") : t("Partly refunded")}</Chip>
              ),
          },
        ]}
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
      {open ? <SaleDrawer saleId={open} onClose={() => setOpen(null)} /> : null}
    </div>
  );
}

export function RefundsPage() {
  const [from, setFrom] = useState(todayLocal(-6));
  const [to, setTo] = useState(todayLocal());
  const [open, setOpen] = useState<string | null>(null);
  const { data, loading, error } = useLoad(() => api.refunds.list(from, to), [from, to]);
  type R = Record<string, string | number | null>;
  return (
    <div>
      <PageHeader title={t("Refunds")} subtitle={t("Every refund with its reason, operator and approver.")} />
      <div className="filters">
        <DateRange from={from} to={to} onChange={(a, b) => (setFrom(a), setTo(b))} />
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<R>
        rows={(data as R[] | null) ?? null}
        loading={loading}
        rowKey={(r) => String(r.refund_id)}
        onRowClick={(r) => setOpen(String(r.original_sale_id))}
        empty={<div className="empty">{t("No refunds in this period.")}</div>}
        columns={[
          {
            key: "n",
            label: t("Refund Receipt"),
            render: (r) => <span className="mono">{r.refund_receipt_number}</span>,
          },
          {
            key: "o",
            label: t("Original Receipt"),
            render: (r) => <span className="mono">{r.original_receipt_number}</span>,
          },
          {
            key: "d",
            label: t("Date"),
            render: (r) => formatShort(String(r.created_at)),
            sort: (r) => String(r.created_at),
          },
          { key: "u", label: t("User"), render: (r) => r.user_name ?? "—" },
          { key: "a", label: t("Approved By"), render: (r) => r.approver_name ?? "—" },
          { key: "re", label: t("Reason"), render: (r) => r.reason },
          {
            key: "m",
            label: t("Refunded to"),
            render: (r) => String(r.methods).split(",").filter(Boolean).map(methodLabel).join(", "),
          },
          {
            key: "t",
            label: t("Amount"),
            num: true,
            render: (r) => <Money minor={-Number(r.total_minor)} />,
            sort: (r) => Number(r.total_minor),
          },
        ]}
      />
      {open ? <SaleDrawer saleId={open} onClose={() => setOpen(null)} /> : null}
    </div>
  );
}

export function ShiftsPage() {
  const [from, setFrom] = useState(todayLocal(-6));
  const [to, setTo] = useState(todayLocal());
  const [open, setOpen] = useState<ShiftSummary | null>(null);
  const { data, loading, error } = useLoad(() => api.shift.list(from, to), [from, to]);
  return (
    <div>
      <PageHeader title={t("Shifts")} subtitle={t("Cash reconciliation per cashier shift.")} />
      <div className="filters">
        <DateRange from={from} to={to} onChange={(a, b) => (setFrom(a), setTo(b))} />
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<ShiftSummary>
        rows={data}
        loading={loading}
        rowKey={(r) => r.shift_id}
        onRowClick={setOpen}
        empty={<div className="empty">{t("No shifts in this period.")}</div>}
        columns={[
          { key: "n", label: t("Shift"), render: (r) => <span className="mono">{r.shift_number}</span> },
          { key: "c", label: t("Cashier"), render: (r) => r.cashier_name, sort: (r) => r.cashier_name },
          { key: "t", label: t("Terminal"), render: (r) => r.device_name ?? "—" },
          { key: "o", label: t("Opened"), render: (r) => formatShort(r.opened_at), sort: (r) => r.opened_at },
          { key: "cl", label: t("Closed"), render: (r) => formatShort(r.closed_at) },
          { key: "s", label: t("Sales"), num: true, render: (r) => <Money minor={r.sales_total_minor} /> },
          { key: "e", label: t("Expected Cash"), num: true, render: (r) => <Money minor={r.expected_cash_minor} /> },
          {
            key: "cc",
            label: t("Counted"),
            num: true,
            render: (r) => (r.counted_cash_minor === null ? "—" : <Money minor={r.counted_cash_minor} />),
          },
          {
            key: "v",
            label: t("Variance"),
            num: true,
            render: (r) =>
              r.variance_minor === null ? (
                "—"
              ) : (
                <span className="row" style={{ justifyContent: "flex-end" }}>
                  {Math.abs(r.variance_minor) > 1000 ? (
                    <AlertTriangle size={14} color="var(--warning)" aria-label={t("Large variance")} />
                  ) : null}
                  <Money minor={r.variance_minor} />
                </span>
              ),
            sort: (r) => Math.abs(r.variance_minor ?? 0),
          },
          {
            key: "st",
            label: t("Status"),
            render: (r) => (r.status === "open" ? <Chip tone="info">{t("Open")}</Chip> : <Chip>{t("Closed")}</Chip>),
          },
        ]}
      />
      {open ? (
        <Drawer title={t("Shift {0}", open.shift_number)} onClose={() => setOpen(null)}>
          <ShiftDetail s={open} />
        </Drawer>
      ) : null}
    </div>
  );
}

function ShiftDetail({ s }: { s: ShiftSummary }) {
  const { data: report } = useLoad(
    () => (s.status === "closed" ? api.receipts.preview("shift_report", s.shift_id) : Promise.resolve(null)),
    [s.shift_id],
  );
  const { data: events } = useLoad(() => api.cash.list({ shift_id: s.shift_id }), [s.shift_id]);
  return (
    <div className="stack-16">
      <dl className="kv">
        <dt>{t("Cashier")}</dt>
        <dd>{s.cashier_name}</dd>
        <dt>{t("Opened")}</dt>
        <dd>{formatDateTime(s.opened_at)}</dd>
        <dt>{t("Closed")}</dt>
        <dd>{formatDateTime(s.closed_at)}</dd>
        <dt>{t("Sales")}</dt>
        <dd>
          {s.sale_count} · {formatMoney(s.sales_total_minor)}
        </dd>
        <dt>{t("Refunds")}</dt>
        <dd>
          {s.refund_count} · {formatMoney(s.refunds_total_minor)}
        </dd>
        <dt>{t("Expected cash")}</dt>
        <dd>{formatMoney(s.expected_cash_minor)}</dd>
        <dt>{t("Counted cash")}</dt>
        <dd>{formatMoney(s.counted_cash_minor)}</dd>
        <dt>{t("Variance")}</dt>
        <dd>{formatMoney(s.variance_minor)}</dd>
        <dt>{t("Variance approved by")}</dt>
        <dd>{s.variance_approved_by_name ?? "—"}</dd>
        <dt>{t("Note")}</dt>
        <dd>{s.close_note ?? "—"}</dd>
      </dl>
      <div>
        <h3 style={{ marginBottom: 6 }}>{t("Cash events")}</h3>
        {(events ?? []).length === 0 ? <div className="small muted">{t("None.")}</div> : null}
        {(events ?? []).map((e) => (
          <div key={String(e.cash_event_id)} className="row small">
            <span style={{ width: 90 }}>{String(e.kind).replace("_", " ")}</span>
            <span className="grow">{String(e.reason)}</span>
            <span>{String(e.user_name ?? "")}</span>
            <span className="num">{formatMoney(Number(e.amount_minor))}</span>
          </div>
        ))}
      </div>
      {report ? (
        <div className="receipt-stage">
          <div className="receipt-paper" style={{ width: "fit-content" }}>
            {report.text}
          </div>
        </div>
      ) : null}
    </div>
  );
}

export function CashEventsPage() {
  const [from, setFrom] = useState(todayLocal(-6));
  const [to, setTo] = useState(todayLocal());
  const [kind, setKind] = useState("");
  const { data, loading, error } = useLoad(() => api.cash.list({ from, to }), [from, to]);
  type R = Record<string, string | number | null>;
  const rows = ((data as R[] | null) ?? null)?.filter((r) => !kind || r.kind === kind) ?? null;
  return (
    <div>
      <PageHeader
        title={t("Cash")}
        subtitle={t("Paid in, paid out, safe drops and no-sale drawer openings. Completed events cannot be edited.")}
      />
      <div className="filters">
        <DateRange from={from} to={to} onChange={(a, b) => (setFrom(a), setTo(b))} />
        <select
          className="select"
          style={{ width: 160 }}
          value={kind}
          onChange={(e) => setKind(e.target.value)}
          aria-label={t("Type")}
        >
          <option value="">{t("All types")}</option>
          <option value="paid_in">{t("Paid in")}</option>
          <option value="paid_out">{t("Paid out")}</option>
          <option value="safe_drop">{t("Safe drop")}</option>
          <option value="no_sale">{t("No sale")}</option>
        </select>
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<R>
        rows={rows}
        loading={loading}
        rowKey={(r) => String(r.cash_event_id)}
        empty={<div className="empty">{t("No cash events in this period.")}</div>}
        columns={[
          {
            key: "t",
            label: t("Time"),
            render: (r) => formatShort(String(r.created_at)),
            sort: (r) => String(r.created_at),
          },
          { key: "k", label: t("Type"), render: (r) => String(r.kind).replace("_", " ") },
          { key: "a", label: t("Amount"), num: true, render: (r) => <Money minor={Number(r.amount_minor)} /> },
          { key: "r", label: t("Reason"), render: (r) => r.reason },
          { key: "u", label: t("User"), render: (r) => r.user_name ?? "—" },
          { key: "ap", label: t("Approver"), render: (r) => r.approver_name ?? "—" },
          { key: "s", label: t("Shift"), render: (r) => <span className="mono">{r.shift_number}</span> },
        ]}
      />
    </div>
  );
}
