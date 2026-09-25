import { useEffect, useMemo, useState } from "react";
import { LogOut, Printer, Settings2 } from "lucide-react";
import { api } from "../../api";
import type { PrintOutcome, ShiftSummary } from "../../api/types";
import { useSession } from "../../state/session";
import { useApproval, ApprovalCancelled } from "../../components/approval";
import { explain } from "../../lib/errors";
import { newOperationId } from "../../lib/ids";
import { formatMoney, parseMoney, formatAmount } from "../../lib/money";
import { formatDate, formatDateTime } from "../../lib/time";
import { Banner, Button, Keypad, Modal } from "../../components/ui";
import { Logo } from "../../components/Logo";
import { methodLabel } from "./labels";
import { t } from "../../i18n";

export function ShiftOpen({ onOpened }: { onOpened: (s: ShiftSummary) => void }) {
  const { session, status, logout, has, setMode } = useSession();
  const [amount, setAmount] = useState("0.000");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [opId] = useState(newOperationId);
  const minor = parseMoney(amount);
  const submit = async () => {
    if (minor === null || minor < 0) {
      setError(t("Enter the opening float, e.g. 20.000"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      onOpened(await api.shift.open(minor, opId));
    } catch (e) {
      setError(explain(e).message);
    } finally {
      setBusy(false);
    }
  };
  const key = (k: string) => {
    setAmount((a) => {
      const cur = a === "0.000" ? "" : a;
      if (k === "Backspace") return cur.slice(0, -1) || "0.000";
      return cur + k;
    });
  };
  return (
    <div className="splash failure">
      <div className="failure-card" style={{ width: 460 }}>
        <Logo size={40} />
        <h1>{t("Start Shift")}</h1>
        <dl className="kv" style={{ width: "100%" }}>
          <dt>{t("Cashier")}</dt>
          <dd>{session?.display_name}</dd>
          <dt>{t("Terminal")}</dt>
          <dd>
            {status.device?.name} ({status.device?.device_code})
          </dd>
          <dt>{t("Business date")}</dt>
          <dd>{formatDate(new Date().toISOString())}</dd>
        </dl>
        {has("shift.open") ? (
          <div className="col" style={{ width: "100%" }}>
            <label htmlFor="float" className="field-label">
              {t("Opening float (cash in drawer)")}
            </label>
            <input
              id="float"
              className="input lg num"
              inputMode="decimal"
              value={amount}
              onFocus={(e) => e.target.select()}
              onChange={(e) => setAmount(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && submit()}
              autoFocus
            />
            <Keypad onKey={key} />
            {error ? <Banner tone="danger">{error}</Banner> : null}
            <Button variant="primary" size="xl" block onClick={submit} loading={busy}>
              {t("Open Shift")}
            </Button>
          </div>
        ) : (
          <Banner tone="warning">{t("You are not allowed to open a shift on this terminal.")}</Banner>
        )}
        <div className="row" style={{ width: "100%" }}>
          {has("admin.access") ? (
            <Button icon={<Settings2 size={16} />} onClick={() => setMode("admin")}>
              {t("Admin")}
            </Button>
          ) : null}
          <Button className="right" icon={<LogOut size={16} />} onClick={() => void logout()}>
            {t("Logout")}
          </Button>
        </div>
      </div>
    </div>
  );
}

export function ShiftClose({
  shiftId,
  onClose,
  onClosed,
}: {
  shiftId: string;
  onClose: () => void;
  onClosed: () => void;
}) {
  const { has } = useSession();
  const approve = useApproval();
  const [sum, setSum] = useState<ShiftSummary | null>(null);
  const [counted, setCounted] = useState("");
  const [note, setNote] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<{ summary: ShiftSummary; print: PrintOutcome } | null>(null);
  const [opId] = useState(newOperationId);
  useEffect(() => {
    api.shift
      .get(shiftId)
      .then(setSum)
      .catch((e) => setError(explain(e).message));
  }, [shiftId]);
  const minor = parseMoney(counted);
  const variance = sum && sum.expected_visible && minor !== null ? minor - sum.expected_cash_minor : null;
  const submit = async () => {
    if (minor === null || minor < 0) {
      setError(t("Enter the counted cash, e.g. 145.250"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const r = await approve((tok) =>
        api.shift.close({
          shift_id: shiftId,
          counted_cash_minor: minor,
          note: note || null,
          operation_id: opId,
          approval_token: tok,
        }),
      );
      setDone(r);
    } catch (e) {
      if (!(e instanceof ApprovalCancelled)) setError(explain(e).message);
    } finally {
      setBusy(false);
    }
  };
  const rows = useMemo(() => {
    if (!sum) return [];
    return [
      [t("Sales"), `${sum.sale_count} · ${formatMoney(sum.sales_total_minor)}`],
      ...sum.by_method.map((m) => [`  ${methodLabel(m.method)}`, formatMoney(m.amount_minor)]),
      [t("Refunds"), formatMoney(-sum.refunds_total_minor)],
      [t("Opening float"), formatMoney(sum.opening_float_minor)],
      [t("Cash sales"), formatMoney(sum.cash_sales_minor)],
      [t("Cash refunds"), formatMoney(-sum.cash_refunds_minor)],
      [t("Paid in"), formatMoney(sum.paid_in_minor)],
      [t("Paid out"), formatMoney(-sum.paid_out_minor)],
      [t("Safe drops"), formatMoney(-sum.safe_drop_minor)],
    ];
  }, [sum]);
  if (done) {
    const v = done.summary.variance_minor ?? 0;
    return (
      <Modal
        title={t("Shift closed")}
        size="sm"
        footer={
          <Button variant="primary" className="right" onClick={onClosed}>
            {t("Done")}
          </Button>
        }
      >
        <div className="col gap-16">
          <dl className="kv">
            <dt>{t("Expected cash")}</dt>
            <dd className="money">{formatMoney(done.summary.expected_cash_minor)}</dd>
            <dt>{t("Counted cash")}</dt>
            <dd className="money">{formatMoney(done.summary.counted_cash_minor)}</dd>
            <dt>{t("Variance")}</dt>
            <dd className={`money ${v === 0 ? "pos-num" : "neg-num"}`}>{formatMoney(v)}</dd>
          </dl>
          {done.print.status === "printed" ? (
            <Banner tone="success">{t("Shift report printed.")}</Banner>
          ) : done.print.status === "failed" ? (
            <Banner tone="warning" title={t("Shift report could not be printed")}>
              {done.print.message}
            </Banner>
          ) : null}
        </div>
      </Modal>
    );
  }
  return (
    <Modal
      title={t("Close shift {0}", sum?.shift_number ?? "")}
      size="lg"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button variant="primary" className="right" onClick={submit} loading={busy} disabled={minor === null}>
            {t("Close Shift")}
          </Button>
        </>
      }
    >
      {!sum ? (
        error ? (
          <Banner tone="danger">{error}</Banner>
        ) : (
          <div className="spinner" />
        )
      ) : (
        <div className="grid-2">
          <div>
            <h3 style={{ marginBottom: 8 }}>{t("Shift summary")}</h3>
            <div className="tiny" style={{ marginBottom: 8 }}>
              {t("Opened {0}", formatDateTime(sum.opened_at))}
            </div>
            <table className="table">
              <tbody>
                {rows.map(([k, v]) => (
                  <tr key={k}>
                    <td>{k}</td>
                    <td className="num">{v}</td>
                  </tr>
                ))}
                {sum.expected_visible ? (
                  <tr>
                    <td style={{ fontWeight: 700 }}>{t("Expected in drawer")}</td>
                    <td className="num" style={{ fontWeight: 700 }}>
                      {formatMoney(sum.expected_cash_minor)}
                    </td>
                  </tr>
                ) : null}
              </tbody>
            </table>
            {sum.expected_visible ? (
              <div className="tiny" style={{ marginTop: 8 }}>
                {t(
                  "Expected in drawer = opening float + cash sales − cash refunds + paid in − paid out − safe drops. Card, wallet and account payments are not in the drawer.",
                )}
              </div>
            ) : null}
            {!sum.expected_visible ? (
              <div className="tiny" style={{ marginTop: 8 }}>
                {t("Blind count: the expected amount is shown after you close.")}
              </div>
            ) : null}
          </div>
          <div className="col gap-16">
            <label className="field-label" htmlFor="counted">
              {t("Counted cash in drawer")}
            </label>
            <input
              id="counted"
              className="input lg num"
              inputMode="decimal"
              value={counted}
              autoFocus
              placeholder={formatAmount(0)}
              onChange={(e) => setCounted(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && submit()}
            />
            {variance !== null ? (
              <Banner
                tone={variance === 0 ? "success" : Math.abs(variance) <= 1000 ? "warning" : "danger"}
                title={t("Variance")}
              >
                {formatMoney(variance)}
              </Banner>
            ) : null}
            <textarea
              className="textarea"
              placeholder={t("Note (optional)")}
              value={note}
              onChange={(e) => setNote(e.target.value)}
            />
            {!has("shift.approve_variance") ? (
              <div className="tiny">{t("A manager must acknowledge large differences.")}</div>
            ) : null}
            {error ? <Banner tone="danger">{error}</Banner> : null}
            <div className="tiny row">
              <Printer size={14} /> {t("A shift report prints on close.")}
            </div>
          </div>
        </div>
      )}
    </Modal>
  );
}
