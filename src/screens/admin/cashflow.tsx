// Cash-flow Radar (Wave 8): what the records say will go out and come in
// over the next days, worked out by AMWAPOS from posted invoices, expenses,
// repeating expenses, open purchase orders and returns. It is not a bank
// balance: AMWAPOS does not see the bank. Every figure links to its record.
import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { api } from "../../api";
import type { CashflowRadar, RadarBand, RadarLine } from "../../api/types";
import { Banner, Chip, Empty, PageHeader, Skeleton, Tabs } from "../../components/ui";
import { useLoad } from "./common";
import { formatMoney } from "../../lib/money";
import { formatDate } from "../../lib/time";
import { t, tb } from "../../i18n";

const HORIZONS = ["7", "14", "30", "60", "90"] as const;

export const BAND_LABEL: Record<RadarBand, () => string> = {
  known: () => t("Known"),
  scheduled: () => t("Scheduled"),
  exposure: () => t("Exposure"),
};
const BAND_TONE: Record<RadarBand, "danger" | "warning" | "info"> = {
  known: "danger",
  scheduled: "warning",
  exposure: "info",
};

const KIND_LABEL: Record<string, () => string> = {
  supplier_invoice: () => t("Supplier invoice"),
  unapplied_supplier_balance: () => t("Paid or credited, not matched to an invoice"),
  expense_approved: () => t("Approved expense, not paid"),
  expense_submitted: () => t("Expense awaiting approval"),
  expense_recurring_draft: () => t("Repeating expense, drafted"),
  recurring_expense: () => t("Repeating expense"),
  purchase_order: () => t("Purchase order, not invoiced yet"),
  supplier_invoice_unposted: () => t("Approved invoice, not posted yet"),
  expected_supplier_credit: () => t("Credit expected from a return"),
};

function Tile({ label, minor, hint, testId }: { label: string; minor: number | null; hint?: string; testId?: string }) {
  return (
    <div className="card card-pad col gap-4" data-testid={testId} style={{ minWidth: 180, flex: 1 }}>
      <span className="tiny muted">{label}</span>
      <strong style={{ fontSize: 20 }} className="money">
        {minor === null ? "—" : formatMoney(minor)}
      </strong>
      {hint ? <span className="tiny muted">{hint}</span> : null}
    </div>
  );
}

function LineRow({ l }: { l: RadarLine }) {
  const nav = useNavigate();
  return (
    <tr data-testid="radar-line" className="clickable" onClick={() => l.source.link && nav(l.source.link)}>
      <td className="nowrap">{l.date ? formatDate(l.date) : t("No date")}</td>
      <td>
        <Chip tone={BAND_TONE[l.band]}>{BAND_LABEL[l.band]()}</Chip>{" "}
        {l.overdue ? <Chip tone="danger">{t("Overdue")}</Chip> : null}
      </td>
      <td>
        <div className="col">
          <span dir="auto">{l.source.type === "payables" ? tb(l.label) : l.label}</span>
          <span className="tiny muted">{KIND_LABEL[l.kind]?.() ?? l.kind}</span>
        </div>
      </td>
      <td className="num money">{(l.direction === "in" ? "+" : "−") + formatMoney(l.amount_minor)}</td>
    </tr>
  );
}

export function CashflowPage() {
  const [h, setH] = useState<(typeof HORIZONS)[number]>("30");
  const { data, error, loading } = useLoad<CashflowRadar>(() => api.cashflow.radar(Number(h)), [h]);
  const r = data;
  const totals = r?.horizons.find((x) => x.days === Number(h));
  const dated = r?.lines.filter((l) => l.date) ?? [];
  const undated = r?.lines.filter((l) => !l.date) ?? [];
  return (
    <div>
      <PageHeader
        title={t("Cash-flow Radar")}
        subtitle={t(
          "What your records say will go out and come in over the next days, worked out from posted invoices, expenses, repeating expenses, open orders and returns.",
        )}
      />
      <Banner tone="info">
        <span data-testid="radar-not-bank">
          {tb(
            r?.not_a_bank_balance ??
              "This is not your bank balance. AMWAPOS does not see your bank; it shows what your records say will go out and come in.",
          )}
        </span>
      </Banner>
      <div style={{ marginTop: 12 }}>
        <Tabs<(typeof HORIZONS)[number]>
          value={h}
          onChange={setH}
          tabs={HORIZONS.map((x) => ({ key: x, label: t("{0} days", x) }))}
        />
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {loading && !r ? <Skeleton rows={6} /> : null}
      {r && totals ? (
        <div className="col gap-16" style={{ marginTop: 16 }} data-testid="radar">
          <div className="row wrap gap-12">
            <Tile
              testId="radar-known"
              label={t("Known going out")}
              minor={totals.known_out_minor}
              hint={t("Posted invoices and approved expenses")}
            />
            <Tile label={t("Scheduled going out")} minor={totals.scheduled_out_minor} hint={t("Repeating expenses")} />
            <Tile
              label={t("Exposure going out")}
              minor={totals.exposure_out_minor}
              hint={t("Orders not invoiced, invoices not posted, expenses awaiting approval")}
            />
            <Tile
              label={t("Sales if the last 28 days repeat")}
              minor={totals.scenario_in_minor}
              hint={r.scenario.available ? t("A scenario, not a fact") : t("Fewer than 28 days of sales: no scenario.")}
            />
            <Tile
              testId="radar-cash"
              label={t("Cash recorded in the store now")}
              minor={r.cash_recorded.total_minor}
              hint={t("Drawers, petty cash and riders. Not the bank.")}
            />
          </div>
          {r.overdue_out_minor > 0 ? (
            <Banner tone="warning">
              {t("{0} is already past its due date and is counted today.", formatMoney(r.overdue_out_minor))}
            </Banner>
          ) : null}
          <div className="card card-pad col gap-8">
            <h3>{t("Week by week")}</h3>
            <table className="table" data-testid="radar-weeks">
              <thead>
                <tr>
                  <th>{t("Week")}</th>
                  <th className="num">{t("Known")}</th>
                  <th className="num">{t("Scheduled")}</th>
                  <th className="num">{t("Exposure")}</th>
                  <th className="num">{t("Scenario sales")}</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {r.weeks.map((w) => (
                  <tr key={w.from}>
                    <td className="nowrap">
                      {formatDate(w.from)} – {formatDate(w.to)}
                    </td>
                    <td className="num money">{formatMoney(w.known_out_minor)}</td>
                    <td className="num money">{formatMoney(w.scheduled_out_minor)}</td>
                    <td className="num money">{formatMoney(w.exposure_out_minor)}</td>
                    <td className="num money">
                      {w.scenario_in_minor === null ? "—" : formatMoney(w.scenario_in_minor)}
                    </td>
                    <td>{w.pressure ? <Chip tone="warning">{t("Pressure week")}</Chip> : null}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="card card-pad col gap-8">
            <h3>{t("By date")}</h3>
            {dated.length === 0 ? (
              <Empty title={t("Nothing dated in this period")}>
                {t(
                  "Posted supplier invoices, approved expenses, repeating expenses and open orders appear here on their dates.",
                )}
              </Empty>
            ) : (
              <table className="table">
                <tbody>
                  {dated.map((l, i) => (
                    <LineRow key={`${l.source.id}-${l.date}-${i}`} l={l} />
                  ))}
                </tbody>
              </table>
            )}
          </div>
          {undated.length ? (
            <div className="card card-pad col gap-8">
              <h3>{t("Without a date")}</h3>
              <span className="tiny muted">
                {t("These have no date in the records, so they are listed apart and not spread over the days.")}
              </span>
              <table className="table">
                <tbody>
                  {undated.map((l, i) => (
                    <LineRow key={`${l.source.id}-u-${i}`} l={l} />
                  ))}
                </tbody>
              </table>
            </div>
          ) : null}
          <div className="row wrap gap-12">
            <div className="card card-pad col gap-8" style={{ flex: 1, minWidth: 280 }}>
              <h3>{t("Cash recorded in the store")}</h3>
              <table className="table">
                <tbody>
                  {r.cash_recorded.drawers.map((d) => (
                    <tr key={d.shift_id}>
                      <td>
                        {t("Drawer")} · <span dir="ltr">{d.shift_number}</span> · {d.till ?? ""}
                      </td>
                      <td className="num money">{formatMoney(d.expected_cash_minor)}</td>
                    </tr>
                  ))}
                  {r.cash_recorded.petty_cash.map((p) => (
                    <tr key={p.fund_id}>
                      <td>
                        {t("Petty cash")} · <span dir="auto">{p.name}</span>
                      </td>
                      <td className="num money">{formatMoney(p.balance_minor)}</td>
                    </tr>
                  ))}
                  <tr>
                    <td>{t("With riders, not handed over")}</td>
                    <td className="num money">{formatMoney(r.cash_recorded.riders_held_minor)}</td>
                  </tr>
                </tbody>
              </table>
              {r.cash_recorded.last_counts.length ? (
                <>
                  <h4>{t("Last count at each till")}</h4>
                  <table className="table">
                    <tbody>
                      {r.cash_recorded.last_counts.map((c) => (
                        <tr key={c.shift_number}>
                          <td>
                            <span dir="ltr">{c.shift_number}</span> · {c.till ?? ""}
                          </td>
                          <td className="num money">
                            {c.counted_cash_minor === null ? "—" : formatMoney(c.counted_cash_minor)}
                          </td>
                          <td className="num tiny">
                            {c.difference_minor ? t("Difference {0}", formatMoney(c.difference_minor)) : t("Matched")}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </>
              ) : null}
            </div>
            <div className="card card-pad col gap-8" style={{ flex: 1, minWidth: 280 }}>
              <h3>{t("What customers owe")}</h3>
              <span className="tiny muted">{t("When it will be paid is not known, so it has no date here.")}</span>
              <table className="table">
                <tbody>
                  <tr>
                    <td>{t("Not yet due")}</td>
                    <td className="num money">{formatMoney(r.receivables.current_minor)}</td>
                  </tr>
                  <tr>
                    <td>{t("1–30 days late")}</td>
                    <td className="num money">{formatMoney(r.receivables.d1_30_minor)}</td>
                  </tr>
                  <tr>
                    <td>{t("31–60 days late")}</td>
                    <td className="num money">{formatMoney(r.receivables.d31_60_minor)}</td>
                  </tr>
                  <tr>
                    <td>{t("61–90 days late")}</td>
                    <td className="num money">{formatMoney(r.receivables.d61_90_minor)}</td>
                  </tr>
                  <tr>
                    <td>{t("More than 90 days late")}</td>
                    <td className="num money">{formatMoney(r.receivables.d90_plus_minor)}</td>
                  </tr>
                </tbody>
              </table>
              {r.expected_supplier_credits_minor > 0 ? (
                <span className="tiny">
                  {t("Credits expected from supplier returns: {0}", formatMoney(r.expected_supplier_credits_minor))}
                </span>
              ) : null}
            </div>
          </div>
          <details className="card card-pad" data-testid="radar-formulas">
            <summary>{t("How these figures are worked out")}</summary>
            <ul>
              {r.formulas.map((f) => (
                <li key={f} className="small">
                  {tb(f)}
                </li>
              ))}
            </ul>
          </details>
        </div>
      ) : null}
    </div>
  );
}
