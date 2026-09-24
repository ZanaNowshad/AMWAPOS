import { useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { ArrowLeft, Download, FileBarChart2, Printer, Play } from "lucide-react";
import { api } from "../../api";
import type { Report, ReportColumn, ReportParams } from "../../api/types";
import { formatMoney, formatPercent, formatQty } from "../../lib/money";
import { formatDate, formatShort, todayLocal } from "../../lib/time";
import { Banner, Button, PageHeader, Skeleton } from "../../components/ui";
import { DateRange, download, useAction, useLoad } from "./common";
import { BarChart } from "./charts";
import { t, tb } from "../../i18n";

export function ReportsHome() {
  const nav = useNavigate();
  const { data, error } = useLoad(() => api.reports.catalog(), []);
  const groups = Array.from(new Set((data ?? []).map((r) => r.group)));
  return (
    <div>
      <PageHeader
        title={t("Reports")}
        subtitle={t("All figures come from recorded transactions. Export any report to CSV.")}
      />
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {!data ? <Skeleton /> : null}
      <div className="stack-24">
        {groups.map((g) => (
          <div key={g}>
            <h3 style={{ marginBottom: 10 }}>{g}</h3>
            <div className="choice-cards">
              {data!
                .filter((r) => r.group === g)
                .map((r) => (
                  <button key={r.key} className="choice-card" onClick={() => nav(`/admin/reports/${r.key}`)}>
                    <FileBarChart2 size={20} color="var(--brand)" />
                    <h3>{tb(r.title)}</h3>
                    <div className="small muted">{tb(r.description)}</div>
                  </button>
                ))}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

export function fmtCell(c: ReportColumn, v: unknown): string {
  if (v === null || v === undefined) return "—";
  switch (c.kind) {
    case "money":
      return formatMoney(Number(v));
    case "qty":
      return formatQty(Number(v));
    case "percent_bp":
      return formatPercent(Number(v));
    case "datetime":
      return formatShort(String(v));
    case "date":
      return formatDate(String(v));
    case "int":
      return Number(v).toLocaleString("en");
    default:
      return String(v).replace(/_/g, " ");
  }
}

function fmtKpi(kind: string, v: number): string {
  return kind === "money"
    ? formatMoney(v)
    : kind === "qty"
      ? formatQty(v)
      : kind === "percent_bp"
        ? formatPercent(v)
        : v.toLocaleString("en");
}

export function ReportViewer() {
  const { key } = useParams();
  const nav = useNavigate();
  const [params, setParams] = useState<ReportParams>({
    from: todayLocal(),
    to: todayLocal(),
    group_by: key === "sales" ? "hour" : undefined,
    days: 60,
  });
  const [report, setReport] = useState<Report | null>(null);
  const act = useAction();
  const run = async (p = params) => {
    const r = await act.run(() => api.reports.run(key!, p));
    if (r) setReport(r);
  };
  useEffect(() => {
    void run(params);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  const noDates = key === "inventory" || key === "dead_stock";
  return (
    <div>
      <div className="page-header">
        <Button
          variant="ghost"
          icon={<ArrowLeft size={18} />}
          aria-label={t("Back")}
          onClick={() => nav("/admin/reports")}
        />
        <div className="grow">
          <div className="tiny">{t("Reports")}</div>
          <h1>{report?.title ?? t("Report")}</h1>
        </div>
        <Button
          icon={<Download size={16} />}
          disabled={!report}
          onClick={async () => {
            const csv = await act.run(() => api.reports.csv(key!, params));
            if (csv) download(`${key}-${params.from}-${params.to}.csv`, csv);
          }}
        >
          {t("Export CSV")}
        </Button>
        <Button icon={<Printer size={16} />} disabled={!report} onClick={() => window.print()}>
          {t("Print")}
        </Button>
      </div>
      <div className="card card-pad filters" style={{ marginBottom: 16 }}>
        {!noDates ? (
          <DateRange
            from={params.from!}
            to={params.to!}
            onChange={(a, b) => setParams({ ...params, from: a, to: b })}
          />
        ) : null}
        {key === "sales" ? (
          <select
            className="select"
            style={{ width: 150 }}
            value={params.group_by}
            onChange={(e) => setParams({ ...params, group_by: e.target.value })}
            aria-label={t("Group by")}
          >
            <option value="hour">{t("By hour")}</option>
            <option value="day">{t("By day")}</option>
            <option value="weekday">{t("By weekday")}</option>
            <option value="cashier">{t("By cashier")}</option>
            <option value="device">{t("By terminal")}</option>
          </select>
        ) : null}
        {key === "dead_stock" ? (
          <label className="row small">
            {t("No sales in the last")}
            <input
              className="input num"
              style={{ width: 80 }}
              value={params.days ?? 60}
              onChange={(e) => setParams({ ...params, days: Number(e.target.value.replace(/\D/g, "")) || 60 })}
            />
            days
          </label>
        ) : null}
        <Button variant="primary" icon={<Play size={15} />} onClick={() => run()} loading={act.busy}>
          {t("Run")}
        </Button>
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {!report ? (
        <Skeleton rows={8} />
      ) : (
        <div className="stack-16">
          <div className="kpis">
            {report.kpis.map((k) => (
              <div key={k.label} className="card kpi">
                <div className="k-label">{tb(k.label)}</div>
                <div className="k-value">{fmtKpi(k.kind, k.value)}</div>
                {k.previous !== null ? (
                  <div className="k-delta muted">{t("Previous period: {0}", fmtKpi(k.kind, k.previous))}</div>
                ) : null}
              </div>
            ))}
          </div>
          {report.series && report.series.length > 1 ? (
            <div className="card card-pad">
              <BarChart
                label={tb(report.title)}
                data={report.series.map((s) => ({ label: String(s.label), value: Number(s.value) }))}
              />
            </div>
          ) : null}
          {report.notes.map((n) => (
            <Banner key={n} tone="info">
              {n}
            </Banner>
          ))}
          <div className="card">
            <div className="table-wrap" style={{ maxHeight: "60vh" }}>
              <table className="table">
                <thead>
                  <tr>
                    {report.columns.map((c) => (
                      <th
                        key={c.key}
                        className={c.kind !== "text" && c.kind !== "datetime" && c.kind !== "date" ? "num" : ""}
                      >
                        {tb(c.label)}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {report.rows.map((r, i) => (
                    <tr key={i}>
                      {report.columns.map((c) => {
                        const v = r[c.key];
                        const neg = (c.key === "profit" || c.key === "variance") && typeof v === "number" && v < 0;
                        return (
                          <td
                            key={c.key}
                            className={`${c.kind !== "text" && c.kind !== "datetime" && c.kind !== "date" ? "num" : ""} ${neg ? "neg-num" : ""}`}
                          >
                            {fmtCell(c, v)}
                          </td>
                        );
                      })}
                    </tr>
                  ))}
                </tbody>
                {report.totals ? (
                  <tfoot>
                    <tr>
                      {report.columns.map((c) => (
                        <td key={c.key} className={c.kind !== "text" ? "num" : ""}>
                          {report.totals![c.key] !== undefined ? fmtCell(c, report.totals![c.key]) : ""}
                        </td>
                      ))}
                    </tr>
                  </tfoot>
                ) : null}
              </table>
              {report.rows.length === 0 ? <div className="empty">{t("No data for this period.")}</div> : null}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

interface Insight {
  title: string;
  evidence: string;
  impact: string;
  confidence: string;
  action: string;
  link?: string;
}

export function AnalyticsPage() {
  const nav = useNavigate();
  const { data, error } = useLoad(async () => {
    const from = todayLocal(-29);
    const to = todayLocal();
    const [dead, margin, refunds, inventory, sales] = await Promise.all([
      api.reports.run("dead_stock", { days: 60 }),
      api.reports.run("margin", { from, to }),
      api.reports.run("refunds", { from, to }),
      api.reports.run("inventory", {}),
      api.reports.run("products", { from, to }),
    ]);
    const out: Insight[] = [];
    const deadValue = dead.kpis.find((k) => k.label === "Value tied up")?.value ?? 0;
    if (dead.rows.length) {
      out.push({
        title: t("Dead stock"),
        evidence: t("{0} stocked product(s) had no sales in 60 days.", dead.rows.length),
        impact: t("{0} tied up at average cost.", formatMoney(deadValue)),
        confidence: t("High — based on recorded sales and stock levels."),
        action: t("Promote, return to supplier or stop reordering the top items."),
        link: "/admin/reports/dead_stock",
      });
    }
    const negative = margin.rows.filter((r) => Number(r.profit) < 0);
    if (negative.length) {
      out.push({
        title: t("Sold below cost"),
        evidence: t(
          "{0} product(s) had negative gross profit in the last 30 days (e.g. {1}).",
          negative.length,
          negative
            .slice(0, 3)
            .map((r) => r.name)
            .join(", "),
        ),
        impact: t("{0} gross profit.", formatMoney(negative.reduce((a, r) => a + Number(r.profit), 0))),
        confidence: t("Medium — depends on the accuracy of recorded costs."),
        action: t("Review selling prices and recent supplier costs for these products."),
        link: "/admin/reports/margin",
      });
    }
    // Stockout risk: low stock items that sold in the last 30 days.
    const soldIds = new Map(sales.rows.map((r) => [String(r.name), Number(r.qty)]));
    const risk = inventory.rows.filter(
      (r) => (r.status === "low_stock" || r.status === "out_of_stock") && (soldIds.get(String(r.name)) ?? 0) > 0,
    );
    if (risk.length) {
      out.push({
        title: t("Stockout risk"),
        evidence: t(
          "{0} selling product(s) are at or below their reorder point (e.g. {1}).",
          risk.length,
          risk
            .slice(0, 3)
            .map((r) => r.name)
            .join(", "),
        ),
        impact: t("Lost sales when shelves are empty."),
        confidence: t("High — based on current stock and 30-day sales."),
        action: t("Create purchase orders for these items."),
        link: "/admin/inventory",
      });
    }
    const byUser = new Map<string, number>();
    for (const r of refunds.rows)
      byUser.set(String(r.user ?? "—"), (byUser.get(String(r.user ?? "—")) ?? 0) + Number(r.total));
    const totalRef = Array.from(byUser.values()).reduce((a, b) => a + b, 0);
    for (const [u, v] of byUser) {
      if (totalRef > 0 && v * 100 >= totalRef * 60 && refunds.rows.length >= 5) {
        out.push({
          title: t("Refund concentration"),
          evidence: t("{0} processed {1}% of refund value in 30 days.", u, Math.round((v * 100) / totalRef)),
          impact: t("{0} refunded.", formatMoney(v)),
          confidence: t("Low — may reflect shift patterns rather than a problem."),
          action: t("Review the refund list and approvals for this user."),
          link: "/admin/refunds",
        });
      }
    }
    return out;
  }, []);
  return (
    <div>
      <PageHeader
        title={t("Analytics")}
        subtitle={t("Operational insights with the evidence behind them. Computed locally from your records.")}
      />
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {!data ? <Skeleton /> : null}
      {data && data.length === 0 ? <div className="empty">{t("No issues detected in the last 30 days.")}</div> : null}
      <div className="grid-2">
        {(data ?? []).map((i) => (
          <div key={i.title + i.evidence} className="card card-pad col">
            <h3>{tb(i.title)}</h3>
            <dl className="kv">
              <dt>{t("Evidence")}</dt>
              <dd>{tb(i.evidence)}</dd>
              <dt>{t("Impact")}</dt>
              <dd>{tb(i.impact)}</dd>
              <dt>{t("Confidence")}</dt>
              <dd>{tb(i.confidence)}</dd>
              <dt>{t("Suggested action")}</dt>
              <dd>{tb(i.action)}</dd>
            </dl>
            {i.link ? (
              <div>
                <Button size="sm" onClick={() => nav(i.link!)}>
                  {t("Open")}
                </Button>
              </div>
            ) : null}
          </div>
        ))}
      </div>
    </div>
  );
}
