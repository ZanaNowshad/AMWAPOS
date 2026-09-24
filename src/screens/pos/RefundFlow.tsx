import { useState } from "react";
import { ArrowLeft, Check, Search } from "lucide-react";
import { api } from "../../api";
import type { RefundPreview, RefundResult, SaleDetail } from "../../api/types";
import { useApproval, ApprovalCancelled } from "../../components/approval";
import { explain } from "../../lib/errors";
import { newOperationId } from "../../lib/ids";
import { formatMoney, formatQty, parseQty } from "../../lib/money";
import { formatDateTime } from "../../lib/time";
import { Banner, Button, Checkbox, Modal } from "../../components/ui";
import { methodLabel } from "./labels";
import { t } from "../../i18n";

const REASONS = [
  t("Damaged / defective"),
  t("Expired"),
  t("Wrong item"),
  t("Customer changed mind"),
  t("Price dispute"),
  t("Other"),
];

export function RefundFlow({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const approve = useApproval();
  const [step, setStep] = useState<"search" | "select" | "review" | "done">("search");
  const [receipt, setReceipt] = useState("");
  const [sale, setSale] = useState<SaleDetail | null>(null);
  const [qty, setQty] = useState<Record<string, string>>({});
  const [restock, setRestock] = useState<Record<string, boolean>>({});
  const [reason, setReason] = useState(REASONS[0]);
  const [other, setOther] = useState("");
  const [method, setMethod] = useState<string>("");
  const [preview, setPreview] = useState<RefundPreview | null>(null);
  const [result, setResult] = useState<RefundResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [opId, setOpId] = useState(newOperationId);

  const fail = (e: unknown) => {
    if (e instanceof ApprovalCancelled) return;
    const ex = explain(e);
    setError(`${ex.message} ${ex.action}`.trim());
  };

  const find = async () => {
    setError(null);
    setBusy(true);
    try {
      const s = await api.refunds.lookup(receipt.trim());
      setSale(s);
      setQty({});
      setRestock(Object.fromEntries(s.items.map((i) => [i.sale_item_id, true])));
      const methods = Array.from(new Set(s.payments.map((p) => p.method)));
      setMethod(methods.includes("cash") ? "cash" : (methods[0] ?? "cash"));
      setStep("select");
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };

  const lines = () =>
    (sale?.items ?? [])
      .map((i) => ({
        sale_item_id: i.sale_item_id,
        qty_milli: parseQty(qty[i.sale_item_id] ?? "") ?? 0,
        restock: restock[i.sale_item_id] ?? true,
      }))
      .filter((l) => l.qty_milli > 0);

  const reasonText = reason === t("Other") ? other.trim() : reason;

  const review = async () => {
    setError(null);
    if (!lines().length) {
      setError(t("Select at least one item and quantity to refund."));
      return;
    }
    if (!reasonText) {
      setError(t("Enter a reason."));
      return;
    }
    setBusy(true);
    try {
      const p = await api.refunds.preview({
        sale_id: sale!.sale_id,
        lines: lines(),
        reason: reasonText,
        operation_id: opId,
        tenders: [],
      });
      const tv = p.total_minor > 0 ? [{ method, amount_minor: p.total_minor }] : [];
      setPreview({ ...p, tenders: tv });
      setStep("review");
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };

  const confirm = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await approve((tok) =>
        api.refunds.create({
          sale_id: sale!.sale_id,
          lines: lines(),
          reason: reasonText,
          operation_id: opId,
          tenders: preview!.tenders,
          approval_token: tok,
        }),
      );
      setResult(r);
      setStep("done");
      onDone();
    } catch (e) {
      fail(e);
      if ((e as { code?: string }).code === "validation") setOpId(newOperationId());
    } finally {
      setBusy(false);
    }
  };

  if (step === "search") {
    return (
      <Modal
        title={t("Refund")}
        size="md"
        onClose={onClose}
        footer={
          <>
            <Button onClick={onClose}>{t("Cancel")}</Button>
            <Button variant="primary" className="right" onClick={find} loading={busy} disabled={!receipt.trim()}>
              {t("Open Refund")}
            </Button>
          </>
        }
      >
        <div className="col gap-16">
          <label className="field-label" htmlFor="refund-receipt">
            {t("Receipt number")}
          </label>
          <div className="scan-box">
            <Search size={18} className="scan-icon" />
            <input
              id="refund-receipt"
              className="input"
              placeholder={t("Scan or type the receipt number, e.g. T01-0000123")}
              value={receipt}
              onChange={(e) => setReceipt(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && find()}
              autoFocus
            />
          </div>
          {error ? <Banner tone="danger">{error}</Banner> : null}
        </div>
      </Modal>
    );
  }

  if (step === "select" && sale) {
    const methods = Array.from(new Set(["cash", ...sale.payments.map((p) => p.method)]));
    return (
      <Modal
        title={t("Refund — receipt {0}", sale.receipt_number)}
        size="full"
        onClose={onClose}
        footer={
          <>
            <Button icon={<ArrowLeft size={16} />} onClick={() => setStep("search")}>
              {t("Back")}
            </Button>
            <Button variant="primary" className="right" onClick={review} loading={busy}>
              {t("Review Refund")}
            </Button>
          </>
        }
      >
        <div className="grid-3">
          <div>
            <div className="tiny" style={{ marginBottom: 8 }}>
              {t("{0} · {1} · {2} · paid", formatDateTime(sale.completed_at), sale.cashier_name, sale.device_name)}{" "}
              {sale.payments.map((p) => methodLabel(p.method)).join(", ")}
            </div>
            <table className="table">
              <thead>
                <tr>
                  <th>{t("Item")}</th>
                  <th className="num">{t("Purchased")}</th>
                  <th className="num">{t("Refunded")}</th>
                  <th className="num">{t("Available")}</th>
                  <th className="num">{t("Refund qty")}</th>
                  <th>{t("Restock")}</th>
                </tr>
              </thead>
              <tbody>
                {sale.items.map((i) => {
                  const avail = i.qty_milli - i.refunded_qty_milli;
                  const v = qty[i.sale_item_id] ?? "";
                  const pv = parseQty(v);
                  const bad = v !== "" && (pv === null || pv <= 0 || pv > avail);
                  return (
                    <tr key={i.sale_item_id}>
                      <td>
                        <div style={{ fontWeight: 600 }}>{i.name}</div>
                        <div className="tiny">
                          {formatMoney(i.line_total_minor)} · {i.sku ?? (i.is_custom ? t("Custom") : "")}
                        </div>
                      </td>
                      <td className="num">{formatQty(i.qty_milli)}</td>
                      <td className="num">{formatQty(i.refunded_qty_milli)}</td>
                      <td className="num">{formatQty(avail)}</td>
                      <td className="num">
                        <div className="row" style={{ justifyContent: "flex-end" }}>
                          <input
                            className={`input num ${bad ? "invalid" : ""}`}
                            style={{ width: 90 }}
                            inputMode="decimal"
                            disabled={avail <= 0}
                            value={v}
                            aria-label={t("Refund quantity for {0}", i.name)}
                            onChange={(e) => setQty({ ...qty, [i.sale_item_id]: e.target.value })}
                          />
                          <Button
                            size="sm"
                            disabled={avail <= 0}
                            onClick={() => setQty({ ...qty, [i.sale_item_id]: formatQty(avail) })}
                          >
                            {t("All")}
                          </Button>
                        </div>
                      </td>
                      <td>
                        <Checkbox
                          label=""
                          checked={restock[i.sale_item_id] ?? true}
                          onChange={(b) => setRestock({ ...restock, [i.sale_item_id]: b })}
                          disabled={!i.product_id}
                        />
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
          <div className="col gap-16">
            <div className="field">
              <label htmlFor="reason">{t("Reason")}</label>
              <select id="reason" className="select" value={reason} onChange={(e) => setReason(e.target.value)}>
                {REASONS.map((r) => (
                  <option key={r}>{r}</option>
                ))}
              </select>
              {reason === t("Other") ? (
                <input
                  className="input"
                  placeholder={t("Describe the reason")}
                  value={other}
                  onChange={(e) => setOther(e.target.value)}
                />
              ) : null}
            </div>
            <div className="field">
              <label htmlFor="refund-method">{t("Refund to")}</label>
              <select id="refund-method" className="select" value={method} onChange={(e) => setMethod(e.target.value)}>
                {methods.map((m) => (
                  <option key={m} value={m}>
                    {methodLabel(m)}
                  </option>
                ))}
              </select>
            </div>
            <div className="tiny">
              {t("Refunds are limited to quantities not yet refunded. Restocked items return to inventory.")}
            </div>
            {error ? <Banner tone="danger">{error}</Banner> : null}
          </div>
        </div>
      </Modal>
    );
  }

  if (step === "review" && preview && sale) {
    return (
      <Modal
        title={t("Review refund")}
        size="md"
        onClose={onClose}
        footer={
          <>
            <Button icon={<ArrowLeft size={16} />} onClick={() => setStep("select")}>
              {t("Back")}
            </Button>
            <Button variant="danger" className="right" onClick={confirm} loading={busy}>
              {t("Confirm Refund {0}", formatMoney(preview.total_minor))}
            </Button>
          </>
        }
      >
        <div className="col gap-16">
          <table className="table">
            <tbody>
              {preview.lines.map((l) => (
                <tr key={l.sale_item_id}>
                  <td>{l.name}</td>
                  <td className="num">× {formatQty(l.qty_milli)}</td>
                  <td>{l.restock ? t("Restock") : t("No restock")}</td>
                  <td className="num">{formatMoney(l.amount_minor)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <dl className="kv">
            <dt>{t("VAT reversed")}</dt>
            <dd className="money">{formatMoney(preview.tax_minor)}</dd>
            <dt>{t("Refund amount")}</dt>
            <dd className="money" style={{ fontWeight: 700 }}>
              {formatMoney(preview.total_minor)}
            </dd>
            <dt>{t("Refunded to")}</dt>
            <dd>
              {preview.tenders.map((tv) => `${methodLabel(tv.method)} ${formatMoney(tv.amount_minor)}`).join(", ") ||
                "—"}
            </dd>
            <dt>{t("Reason")}</dt>
            <dd>{reasonText}</dd>
          </dl>
          {preview.requires_approval ? <Banner tone="info">{t("A manager must approve this refund.")}</Banner> : null}
          {error ? <Banner tone="danger">{error}</Banner> : null}
        </div>
      </Modal>
    );
  }

  if (step === "done" && result) {
    return (
      <Modal
        title={t("Refund completed")}
        size="sm"
        onClose={onClose}
        footer={
          <Button variant="primary" className="right" onClick={onClose} autoFocus>
            {t("Done")}
          </Button>
        }
      >
        <div className="success-screen">
          <div className="success-mark">
            <Check size={30} />
          </div>
          <div style={{ fontWeight: 700 }}>{result.refund_receipt_number}</div>
          <div className="due">{formatMoney(result.total_minor)}</div>
          <div className="small muted">
            {result.tenders.map((tv) => `${methodLabel(tv.method)} ${formatMoney(tv.amount_minor)}`).join(" · ")}
          </div>
          {result.print?.status === "failed" ? (
            <Banner tone="warning" title={t("Refund completed — receipt could not be printed")}>
              {result.print.message}
            </Banner>
          ) : null}
        </div>
      </Modal>
    );
  }
  return null;
}
