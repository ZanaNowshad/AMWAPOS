// The trading day: opening checklist, current totals (X: changes nothing),
// closing the day (Z: a permanent record), closed days, and the day pack.
// The business date is the one every sale already carries (Settings → Shift →
// Trading day ends at); nothing here recomputes it.
import { useState } from "react";
import { AlertTriangle, CheckCircle2, Circle, Download, Info, Lock, RefreshCw, XCircle } from "lucide-react";
import { api } from "../../api";
import type { DayCheck, DayClose, DayCloseRow, DayReport, DayTotals } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { Banner, Button, Checkbox, Chip, Empty, PageHeader, Skeleton, Tabs } from "../../components/ui";
import { DataTable, Drawer, downloadBase64, useAction, useLoad } from "./common";
import { BranchFilter, EndOfDayPage } from "./pillars";
import { methodLabel } from "../pos/labels";
import { formatMoney } from "../../lib/money";
import { formatDate, formatDateTime, todayLocal } from "../../lib/time";
import { newOperationId } from "../../lib/ids";
import { t, tb } from "../../i18n";

type TabKey = "today" | "closed" | "pack";

export function TradingDayPage() {
  const { has } = useSession();
  const canX = has("day.x_report");
  const [tab, setTab] = useState<TabKey>(canX ? "today" : "pack");
  const tabs: { key: TabKey; label: string }[] = [
    ...(canX
      ? [
          { key: "today" as const, label: t("Today") },
          { key: "closed" as const, label: t("Closed days") },
        ]
      : []),
    ...(has("reports.sales") ? [{ key: "pack" as const, label: t("Day pack") }] : []),
  ];
  return (
    <div>
      <PageHeader
        title={t("End of day")}
        subtitle={t(
          "Open the store, see today's totals at any time, and close the trading day when you finish. A closed day is kept exactly as it was.",
        )}
      />
      {tabs.length > 1 ? <Tabs<TabKey> value={tab} onChange={setTab} tabs={tabs} /> : null}
      <div style={{ marginTop: 16 }}>
        {tab === "today" ? <TodayPanel /> : null}
        {tab === "closed" ? <ClosedDays /> : null}
        {tab === "pack" ? <EndOfDayPage embedded /> : null}
      </div>
    </div>
  );
}

const LEVEL_ICON = {
  blocking: <XCircle size={16} color="var(--danger)" />,
  warning: <AlertTriangle size={16} color="var(--warning)" />,
  info: <Info size={16} color="var(--info, var(--muted))" />,
  ok: <CheckCircle2 size={16} color="var(--success)" />,
};

function CheckList({ checks, testid }: { checks: DayCheck[]; testid?: string }) {
  return (
    <ul className="col gap-8" style={{ listStyle: "none", margin: 0, padding: 0 }} data-testid={testid}>
      {checks.map((c, i) => (
        <li key={`${c.code}-${i}`} className="row gap-8" data-level={c.level} style={{ alignItems: "flex-start" }}>
          <span style={{ flex: "none", marginTop: 2 }}>{LEVEL_ICON[c.level] ?? <Circle size={16} />}</span>
          <span dir="auto">{tb(c.message)}</span>
        </li>
      ))}
    </ul>
  );
}

function OpeningCard() {
  const { data, error } = useLoad(() => api.day.opening(), []);
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton rows={3} />;
  const ready = data.verdict === "ready";
  return (
    <div className="card card-pad col gap-12" data-testid="opening">
      <div className="row wrap">
        <h3 className="grow">{t("Opening the store")}</h3>
        <Chip tone={ready ? "success" : "warning"} dot>
          {ready ? t("Ready to trade") : t("Ready, but these need attention")}
        </Chip>
      </div>
      <CheckList checks={data.checks} />
      <div className="tiny">{t("Selling never waits for these. They are here so nothing is missed.")}</div>
    </div>
  );
}

function Kpi({ label, value, testid }: { label: string; value: string; testid?: string }) {
  return (
    <div className="card kpi" data-testid={testid}>
      <div className="k-label">{label}</div>
      <div className="k-value money">{value}</div>
    </div>
  );
}

function TotalsTable({ tot }: { tot: DayTotals }) {
  return (
    <table className="table">
      <tbody>
        <tr>
          <td>{t("Sales ({0})", tot.sale_count)}</td>
          <td className="num money">{formatMoney(tot.sales_minor)}</td>
        </tr>
        <tr>
          <td className="tiny">{t("Before discounts")}</td>
          <td className="num money tiny">{formatMoney(tot.gross_minor)}</td>
        </tr>
        <tr>
          <td className="tiny">{t("Discounts")}</td>
          <td className="num money tiny">{formatMoney(tot.discount_minor)}</td>
        </tr>
        <tr>
          <td>{t("Refunds ({0})", tot.refund_count)}</td>
          <td className="num money">{formatMoney(-tot.refunds_minor)}</td>
        </tr>
        <tr>
          <td>{t("Voids ({0})", tot.void_count)}</td>
          <td className="num money">{formatMoney(-tot.voids_minor)}</td>
        </tr>
        <tr>
          <td>
            <strong>{t("Net sales")}</strong>
          </td>
          <td className="num money">
            <strong>{formatMoney(tot.net_sales_minor)}</strong>
          </td>
        </tr>
        <tr>
          <td>{t("VAT")}</td>
          <td className="num money">{formatMoney(tot.tax_minor)}</td>
        </tr>
        <tr>
          <td>{t("Net of VAT")}</td>
          <td className="num money">{formatMoney(tot.net_ex_vat_minor)}</td>
        </tr>
      </tbody>
    </table>
  );
}

/** The X or Z figures. `rep.kind` decides the wording only. */
export function DayReportView({ rep }: { rep: DayReport }) {
  const after = rep.after_close.sale_count + rep.after_close.refund_count + rep.after_close.void_count > 0;
  const c = rep.cash;
  return (
    <div className="col gap-16">
      <div className="kpis">
        <Kpi label={t("Net sales")} value={formatMoney(rep.total.net_sales_minor)} testid="day-net" />
        <Kpi label={t("VAT")} value={formatMoney(rep.total.tax_minor)} />
        <Kpi label={t("Expected cash")} value={formatMoney(c.expected_cash_minor)} testid="day-expected" />
        <Kpi
          label={c.open_drawers ? t("Difference so far") : t("Cash difference")}
          value={formatMoney(c.variance_minor)}
        />
      </div>
      <div className="grid-2 gap-16">
        <div className="card card-pad col gap-8">
          <h3>{rep.day_closed ? t("Not in a close yet") : t("This day")}</h3>
          <TotalsTable tot={rep.day} />
        </div>
        <div className="card card-pad col gap-8">
          <h3>{t("How customers paid")}</h3>
          <table className="table">
            <tbody>
              {rep.total.tenders.map((x) => (
                <tr key={x.method}>
                  <td>{methodLabel(x.method)}</td>
                  <td className="num money">{formatMoney(x.net_minor)}</td>
                </tr>
              ))}
              {rep.total.tenders.length === 0 ? (
                <tr>
                  <td className="tiny">{t("No payments yet.")}</td>
                </tr>
              ) : null}
            </tbody>
          </table>
          {rep.total.vat.length ? (
            <>
              <h4>{t("VAT by rate")}</h4>
              <table className="table">
                <tbody>
                  {rep.total.vat.map((v) => (
                    <tr key={v.rate_bp}>
                      <td>{`${v.rate_bp / 100}%`}</td>
                      <td className="num money tiny">{formatMoney(v.net_minor)}</td>
                      <td className="num money">{formatMoney(v.tax_minor)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </>
          ) : null}
        </div>
      </div>
      {after ? (
        <div className="card card-pad col gap-8" data-testid="after-close">
          <h3>{t("After-close adjustments")}</h3>
          <div className="tiny">
            {t(
              "These arrived after their own day was closed (for example from a till that was offline). Their day stays as it was closed; they are counted here, once, with their own date.",
            )}
          </div>
          <TotalsTable tot={rep.after_close} />
          <table className="table">
            <thead>
              <tr>
                <th>{t("Trading day")}</th>
                <th>{t("Receipt")}</th>
                <th>{t("Type")}</th>
                <th>{t("Computer")}</th>
                <th className="num">{t("Amount")}</th>
              </tr>
            </thead>
            <tbody>
              {rep.late.map((l) => (
                <tr key={`${l.kind}-${l.number}`}>
                  <td className="nowrap">{formatDate(l.business_date)}</td>
                  <td dir="ltr">{l.number}</td>
                  <td>{l.kind === "sale" ? t("Sale") : l.kind === "void" ? t("Void") : t("Refund")}</td>
                  <td>{l.device_name ?? ""}</td>
                  <td className="num money">{formatMoney(l.kind === "sale" ? l.total_minor : -l.total_minor)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
      <div className="card card-pad col gap-8" data-testid="day-drawers">
        <h3>{t("Drawers")}</h3>
        {rep.drawers.length === 0 ? <div className="tiny">{t("No drawers to show yet.")}</div> : null}
        {rep.drawers.length ? (
          <table className="table">
            <thead>
              <tr>
                <th>{t("Shift")}</th>
                <th>{t("Register")}</th>
                <th>{t("Cashier")}</th>
                <th className="num">{t("Expected")}</th>
                <th className="num">{t("Counted")}</th>
                <th className="num">{t("Difference")}</th>
              </tr>
            </thead>
            <tbody>
              {rep.drawers.map((d) => (
                <tr key={d.shift_id}>
                  <td dir="ltr">
                    {d.shift_number}
                    {d.late ? (
                      <>
                        {" "}
                        <Chip tone="info">{t("After close")}</Chip>
                      </>
                    ) : null}
                  </td>
                  <td>{d.register_name ?? d.device_name ?? ""}</td>
                  <td>{d.cashier_name}</td>
                  <td className="num money">{formatMoney(d.expected_cash_minor)}</td>
                  <td className="num money">
                    {d.status === "open" ? (
                      <Chip tone="warning">{t("Still open")}</Chip>
                    ) : (
                      formatMoney(d.counted_cash_minor ?? 0)
                    )}
                  </td>
                  <td className="num money">{d.status === "open" ? "" : formatMoney(d.variance_minor ?? 0)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : null}
        <table className="table">
          <tbody>
            {(
              [
                [t("Opening floats"), c.opening_float_minor],
                [t("Cash sales"), c.cash_sales_minor],
                [t("Cash refunds"), -c.cash_refunds_minor],
                [t("Cash in"), c.paid_in_minor],
                [t("Cash out"), -c.paid_out_minor],
                [t("Safe drops"), -c.safe_drop_minor],
                [t("Delivery collections"), c.delivery_collections_minor + c.rider_handover_minor],
              ] as [string, number][]
            ).map(([l, v]) => (
              <tr key={l}>
                <td>{l}</td>
                <td className="num money">{formatMoney(v)}</td>
              </tr>
            ))}
            <tr>
              <td>
                <strong>{t("Expected cash")}</strong>
              </td>
              <td className="num money">
                <strong>{formatMoney(c.expected_cash_minor)}</strong>
              </td>
            </tr>
          </tbody>
        </table>
        {c.expenses_from_till_minor || c.account_payments_cash_minor ? (
          <div className="tiny">
            {c.expenses_from_till_minor
              ? t("Of the cash out, {0} paid expenses.", formatMoney(c.expenses_from_till_minor))
              : null}{" "}
            {c.account_payments_cash_minor
              ? t("Of the cash in, {0} were customer account payments.", formatMoney(c.account_payments_cash_minor))
              : null}
          </div>
        ) : null}
        {rep.total.pay_on_delivery_minor ? (
          <div className="tiny">
            {t("Pay on delivery not collected at the till: {0}.", formatMoney(rep.total.pay_on_delivery_minor))}
          </div>
        ) : null}
      </div>
    </div>
  );
}

function TodayPanel() {
  const { has } = useSession();
  const [branch, setBranch] = useState("");
  const [date, setDate] = useState(todayLocal());
  const x = useLoad(() => api.day.x(date, branch || null), [date, branch]);
  const checks = useLoad(() => api.day.checks(date, branch || null), [date, branch]);
  const act = useAction();
  const toast = useToast();
  const [ack, setAck] = useState(false);
  const [closed, setClosed] = useState<DayClose | null>(null);
  const [opId] = useState(newOperationId);
  const reload = () => {
    void x.reload();
    void checks.reload();
  };
  const c = checks.data;
  const blocking = c?.checks.filter((k) => k.level === "blocking") ?? [];
  const warnings = c?.checks.filter((k) => k.level === "warning") ?? [];
  const infos = c?.checks.filter((k) => k.level === "info") ?? [];
  return (
    <div className="col gap-16">
      <OpeningCard />
      <div className="row wrap">
        <input
          type="date"
          className="input"
          style={{ width: 170 }}
          value={date}
          max={todayLocal()}
          aria-label={t("Trading day")}
          onChange={(e) => {
            setDate(e.target.value);
            setAck(false);
          }}
        />
        <BranchFilter value={branch} onChange={setBranch} />
        <div className="grow" />
        <Button icon={<RefreshCw size={16} />} onClick={reload} data-testid="x-refresh">
          {t("View current totals")}
        </Button>
        <Button
          icon={<Download size={16} />}
          loading={act.busy}
          onClick={async () => {
            const f = await act.run(() => api.day.xPdf(date, branch || null));
            if (f) downloadBase64(f.file_name, f.base64, "application/pdf");
          }}
        >
          {t("PDF")}
        </Button>
      </div>
      <Banner tone="info">
        {t("Current totals (X) change nothing. Look at them as often as you like; the day stays open.")}
      </Banner>
      {x.error ? <Banner tone="danger">{x.error}</Banner> : null}
      {!x.data ? <Skeleton rows={6} /> : <DayReportView rep={x.data} />}
      {has("day.close") ? (
        <div className="card card-pad col gap-12" data-testid="close-day">
          <h3>
            <Lock size={16} style={{ verticalAlign: -2 }} /> {t("Close trading day {0}", formatDate(date))}
          </h3>
          <div>
            {t(
              "Closing makes a permanent record of this day's totals and drawers. It cannot be changed afterwards. You can keep selling: sales made after closing count in the next close.",
            )}
          </div>
          {checks.error ? <Banner tone="danger">{checks.error}</Banner> : null}
          {!c ? <Skeleton rows={2} /> : null}
          {blocking.length ? (
            <div className="col gap-8">
              <strong>{t("Do these first")}</strong>
              <CheckList checks={blocking} testid="checks-blocking" />
            </div>
          ) : null}
          {warnings.length ? (
            <div className="col gap-8">
              <strong>{t("Check these")}</strong>
              <CheckList checks={warnings} testid="checks-warning" />
            </div>
          ) : null}
          {infos.length ? (
            <div className="col gap-8">
              <strong>{t("Good to know")}</strong>
              <CheckList checks={infos} testid="checks-info" />
            </div>
          ) : null}
          {c && !blocking.length && warnings.length ? (
            <Checkbox label={t("I have read the items above")} checked={ack} onChange={setAck} />
          ) : null}
          {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          <div>
            <Button
              variant="primary"
              data-testid="close-day-button"
              disabled={!c || !c.can_close || (warnings.length > 0 && !ack)}
              loading={act.busy}
              onClick={async () => {
                const z = await act.run(() => api.day.close(date, opId, ack || warnings.length === 0, branch || null));
                if (z) {
                  toast("success", t("Trading day closed: {0}", z.close_number));
                  setClosed(z);
                  reload();
                }
              }}
            >
              {t("Close trading day")}
            </Button>
          </div>
        </div>
      ) : null}
      {closed ? <CloseDrawer id={closed.close_id} onClose={() => setClosed(null)} /> : null}
    </div>
  );
}

function ClosedDays() {
  const [branch, setBranch] = useState("");
  const list = useLoad(() => api.day.closes(branch || null), [branch]);
  const [open, setOpen] = useState<string | null>(null);
  return (
    <div className="col gap-16">
      <BranchFilter value={branch} onChange={setBranch} />
      {list.error ? <Banner tone="danger">{list.error}</Banner> : null}
      <DataTable<DayCloseRow>
        rows={list.data}
        loading={list.loading}
        rowKey={(r) => r.close_id}
        onRowClick={(r) => setOpen(r.close_id)}
        empty={
          <Empty title={t("No closed days yet")}>
            {t("Close a trading day from the Today tab to keep a permanent record of it.")}
          </Empty>
        }
        columns={[
          {
            key: "d",
            label: t("Trading day"),
            render: (r) => formatDate(r.business_date),
            sort: (r) => r.business_date,
          },
          { key: "n", label: t("Close"), render: (r) => <span dir="ltr">{r.close_number}</span> },
          { key: "s", label: t("Sales"), render: (r) => r.sale_count },
          { key: "net", label: t("Net sales"), render: (r) => formatMoney(r.net_sales_minor), num: true },
          { key: "vat", label: t("VAT"), render: (r) => formatMoney(r.tax_minor), num: true },
          {
            key: "late",
            label: t("After close"),
            render: (r) => (r.late_count ? <Chip tone="info">{r.late_count}</Chip> : ""),
          },
          { key: "v", label: t("Cash difference"), render: (r) => formatMoney(r.variance_minor), num: true },
          { key: "by", label: t("Closed by"), render: (r) => r.closed_by_name ?? "" },
        ]}
      />
      {open ? <CloseDrawer id={open} onClose={() => setOpen(null)} /> : null}
    </div>
  );
}

function CloseDrawer({ id, onClose }: { id: string; onClose: () => void }) {
  const { data, error } = useLoad(() => api.day.get(id), [id]);
  const act = useAction();
  return (
    <Drawer
      wide
      title={data ? t("Trading day {0}", formatDate(data.business_date)) : t("Closed day")}
      onClose={onClose}
      actions={
        <Button
          icon={<Download size={16} />}
          loading={act.busy}
          onClick={async () => {
            const f = await act.run(() => api.day.pdf(id));
            if (f) downloadBase64(f.file_name, f.base64, "application/pdf");
          }}
        >
          {t("PDF")}
        </Button>
      }
    >
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {!data ? (
        <Skeleton rows={6} />
      ) : (
        <div className="col gap-16" data-testid="close-view">
          <div className="row wrap gap-8">
            <Chip tone="brand">
              <span dir="ltr">{data.close_number}</span>
            </Chip>
            <span className="tiny">
              {t("Closed {0} by {1}", formatDateTime(data.closed_at), data.closed_by_name ?? "—")}
            </span>
            {data.verified ? (
              <Chip tone="success">{t("Unchanged since it was closed")}</Chip>
            ) : (
              <Chip tone="danger">{t("This record does not match its fingerprint")}</Chip>
            )}
          </div>
          <DayReportView rep={data.report} />
          <div className="tiny" dir="ltr">
            {t("Fingerprint")}: {data.sha256.slice(0, 16)}…
          </div>
        </div>
      )}
    </Drawer>
  );
}
