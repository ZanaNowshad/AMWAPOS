import { useNavigate } from "react-router-dom";
import { AlertTriangle, ChevronRight, CircleAlert, RefreshCw } from "lucide-react";
import { api } from "../../api";
import { useSession } from "../../state/session";
import { formatMoney, formatQty } from "../../lib/money";
import { formatDate } from "../../lib/time";
import { Banner, Button, PageHeader, Skeleton } from "../../components/ui";
import { useLoad } from "./common";
import { BarChart } from "./charts";
import { t, tb } from "../../i18n";

type Dash = {
  business_date: string;
  kpis: Record<string, number | null>;
  hourly: { hour: number; transactions: number; sales: number }[];
  top_products: { name: string; qty: number; total: number }[];
  low_stock: { product_id: string; name: string; qty: number; reorder: number }[];
  counts: Record<string, number>;
  attention: { kind: string; severity: string; text: string; link: string }[];
  health: { backup: { state: string; summary: string }; sync: { state: string; summary: string } };
};

function Delta({ now, prev }: { now: number | null; prev: number | null; money?: boolean }) {
  if (now === null || prev === null || prev === undefined) return null;
  if (prev === 0) return <div className="k-delta muted">{t("No data for the same day last week")}</div>;
  const pct = Math.round(((now - prev) * 1000) / Math.abs(prev)) / 10;
  return (
    <div className={`k-delta ${pct >= 0 ? "pos-num" : "neg-num"}`}>
      {pct >= 0 ? "▲" : "▼"} {t("{0}% vs same day last week", Math.abs(pct))}
    </div>
  );
}

export function Dashboard() {
  const { has } = useSession();
  const nav = useNavigate();
  const { data, error, reload } = useLoad(() => api.reports.dashboard() as Promise<Dash>, []);
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton rows={10} />;
  const k = data.kpis;
  const cards: { label: string; value: string; now: number | null; prev: number | null; money?: boolean }[] = [
    { label: t("Today Sales (net)"), value: formatMoney(k.sales), now: k.sales, prev: k.sales_prev, money: true },
    { label: t("Transactions"), value: String(k.transactions ?? 0), now: k.transactions, prev: k.transactions_prev },
    {
      label: t("Average Basket"),
      value: formatMoney(k.average_basket),
      now: k.average_basket,
      prev: k.average_basket_prev,
      money: true,
    },
    ...(has("reports.financial")
      ? [
          {
            label: t("Gross Profit"),
            value: formatMoney(k.gross_profit),
            now: k.gross_profit,
            prev: k.gross_profit_prev,
            money: true,
          },
        ]
      : []),
    { label: t("Refunds"), value: `${formatMoney(k.refunds)} (${k.refund_count ?? 0})`, now: null, prev: null },
  ];
  return (
    <div className="stack-24">
      <PageHeader
        title={t("Dashboard")}
        subtitle={t("Business date {0}", formatDate(data.business_date))}
        actions={
          <Button icon={<RefreshCw size={15} />} onClick={() => void reload()}>
            {t("Refresh")}
          </Button>
        }
      />
      <div className="kpis">
        {cards.map((c) => (
          <div key={c.label} className="card kpi">
            <div className="k-label">{tb(c.label)}</div>
            <div className="k-value">{c.value}</div>
            <Delta now={c.now} prev={c.prev} money={c.money} />
          </div>
        ))}
      </div>
      <div className="grid-3">
        <div className="card">
          <div className="card-head">
            <h3>{t("Sales by hour — today")}</h3>
          </div>
          <div className="card-body">
            <BarChart
              label={t("Hourly sales today")}
              data={data.hourly
                .filter((h) => h.hour >= 6 || h.sales > 0)
                .map((h) => ({ label: `${String(h.hour).padStart(2, "0")}:00`, value: h.sales }))}
            />
          </div>
        </div>
        <div className="card">
          <div className="card-head">
            <h3>{t("Needs Attention")}</h3>
          </div>
          <div className="card-body col">
            {data.attention.length === 0 ? (
              <div className="muted small">{t("Nothing needs attention right now.")}</div>
            ) : null}
            {data.attention.map((a, i) => (
              <button
                key={i}
                className="result-row"
                style={{ border: 0, background: "none", textAlign: "start", borderRadius: 8 }}
                onClick={() => nav(a.link.split("?")[0])}
              >
                {a.severity === "error" ? (
                  <CircleAlert size={17} color="var(--danger)" />
                ) : (
                  <AlertTriangle size={17} color="var(--warning)" />
                )}
                <span className="grow">{tb(a.text)}</span>
                <ChevronRight size={16} />
              </button>
            ))}
            <div className="divider" />
            <div className="small">
              <div>
                {t("Open deliveries:")} <strong>{data.counts.open_deliveries}</strong> {t("· Active shifts:")}{" "}
                <strong>{data.counts.active_shifts}</strong>
              </div>
              <div className="muted">{t("Backup: {0}", tb(data.health.backup.summary))}</div>
              <div className="muted">{t("Sync: {0}", tb(data.health.sync.summary))}</div>
            </div>
          </div>
        </div>
      </div>
      <div className="grid-2">
        <div className="card">
          <div className="card-head">
            <h3>{t("Top products today")}</h3>
          </div>
          <table className="table">
            <thead>
              <tr>
                <th>{t("Product")}</th>
                <th className="num">{t("Qty")}</th>
                <th className="num">{t("Revenue")}</th>
              </tr>
            </thead>
            <tbody>
              {data.top_products.map((p) => (
                <tr key={p.name}>
                  <td>{p.name}</td>
                  <td className="num">{formatQty(p.qty)}</td>
                  <td className="num">{formatMoney(p.total)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {data.top_products.length === 0 ? <div className="empty">{t("No sales yet today.")}</div> : null}
        </div>
        <div className="card">
          <div className="card-head">
            <h3>{t("Low stock")}</h3>
            <button className="link right" onClick={() => nav("/admin/inventory")}>
              {t("View inventory")}
            </button>
          </div>
          <table className="table">
            <thead>
              <tr>
                <th>{t("Product")}</th>
                <th className="num">{t("On hand")}</th>
                <th className="num">{t("Reorder point")}</th>
              </tr>
            </thead>
            <tbody>
              {data.low_stock.map((p) => (
                <tr key={p.product_id} className="clickable" onClick={() => nav(`/admin/products/${p.product_id}`)}>
                  <td>{p.name}</td>
                  <td className={`num ${p.qty <= 0 ? "neg-num" : ""}`}>{formatQty(p.qty)}</td>
                  <td className="num">{formatQty(p.reorder)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {data.low_stock.length === 0 ? (
            <div className="empty">{t("All tracked products are above their reorder points.")}</div>
          ) : null}
        </div>
      </div>
    </div>
  );
}
