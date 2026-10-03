import { addrPayload, addrProblem } from "../../components/AddressFields";
import { WhatsAppSendButton } from "../admin/automation";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  Banknote,
  Check,
  CreditCard,
  Delete,
  Landmark,
  Plus,
  Smartphone,
  Split,
  Trash2,
  Truck,
  Printer,
} from "lucide-react";
import { api } from "../../api";
import type { Cart, PrintOutcome, SaleResult, TenderConfig, TenderInput } from "../../api/types";
import { useApproval, ApprovalCancelled } from "../../components/approval";
import { explain } from "../../lib/errors";
import { newOperationId } from "../../lib/ids";
import { digits, formatAmount, formatMoney, formatQty, parseMoney } from "../../lib/money";
import { Banner, Button, Modal } from "../../components/ui";
import { methodLabel } from "./labels";
import { SendPanel, initialSend, type SendState } from "./SendLoop";
import { t, tb } from "../../i18n";

const icons: Record<string, typeof Banknote> = {
  cash: Banknote,
  card: CreditCard,
  benefitpay: Smartphone,
  bank_transfer: Landmark,
};

interface Row {
  method: string;
  amount: string;
  reference: string;
}

export function PaymentModal({
  cart,
  initialMethod,
  tenders,
  onClose,
  onPaid,
  onCartChanged,
}: {
  cart: Cart;
  initialMethod: string;
  tenders: TenderConfig[];
  onClose: () => void;
  onPaid: (s: SaleResult) => void;
  onCartChanged: (c: Cart) => void;
}) {
  const approve = useApproval();
  const due = cart.totals.total_minor;
  const [split, setSplit] = useState(false);
  const [method, setMethod] = useState(
    tenders.some((tv) => tv.method === initialMethod) ? initialMethod : (tenders[0]?.method ?? "cash"),
  );
  const [amount, setAmount] = useState(initialMethod === "cash" ? "" : formatAmount(due));
  const [reference, setReference] = useState("");
  const [rows, setRows] = useState<Row[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // One operation id per payment attempt: a retry after a timeout can never double-charge.
  const [opId, setOpId] = useState(newOperationId);
  const amountRef = useRef<HTMLInputElement>(null);
  // Here (default) or Send; Send may be paid on delivery (no tender now).
  const [send, setSend] = useState<SendState>(() => initialSend(cart));
  const sending = send.mode === "send";
  const pod = sending && send.pod;

  useEffect(() => {
    amountRef.current?.focus();
    amountRef.current?.select();
  }, [method, split]);

  const cfg = (m: string) => tenders.find((tv) => tv.method === m);
  const unit = 10 ** digits();

  const tenderList: TenderInput[] | null = useMemo(() => {
    if (pod) return due > 0 ? [{ method: "pay_on_delivery", amount_minor: due, reference: null }] : [];
    if (split) {
      const out: TenderInput[] = [];
      for (const r of rows) {
        const v = parseMoney(r.amount);
        if (v === null || v <= 0) return null;
        out.push({ method: r.method, amount_minor: v, reference: r.reference || null });
      }
      return out;
    }
    const v = amount.trim() === "" ? (method === "cash" ? null : due) : parseMoney(amount);
    if (v === null || v <= 0) return null;
    return [{ method, amount_minor: v, reference: reference || null }];
  }, [split, rows, amount, method, reference, due, pod]);

  const paid = (tenderList ?? []).reduce((a, tv) => a + tv.amount_minor, 0);
  const nonCash = (tenderList ?? [])
    .filter((tv) => !cfg(tv.method)?.allows_change)
    .reduce((a, tv) => a + tv.amount_minor, 0);
  const cashIn = paid - nonCash;
  const remaining = due - paid;
  const change = paid > due ? paid - due : 0;
  const validation = useMemo(() => {
    if (sending && !cart.customer) return t("Choose the customer to send to.");
    if (sending && addrProblem(send)) return addrProblem(send);
    if (sending && (!send.building.trim() || !send.block.trim())) return t("Enter the building and block to send to.");
    if (pod) return null;
    if (!tenderList)
      return split
        ? t("Enter an amount for each payment.")
        : method === "cash"
          ? t("Enter the cash received or press Exact.")
          : t("Enter the amount.");
    if (nonCash > due) return t("Card and other non-cash payments cannot exceed the amount due.");
    if (remaining > 0) return t("Remaining {0}.", formatMoney(remaining));
    if (change > cashIn) return t("Change can only be given from cash.");
    for (const tv of tenderList) {
      if (cfg(tv.method)?.requires_reference && !tv.reference)
        return t("{0} requires a reference.", methodLabel(tv.method));
    }
    return null;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tenderList, nonCash, due, remaining, change, cashIn, split, method, sending, pod, send, cart.customer]);

  const complete = async () => {
    if (validation || !tenderList || !cart.cart_id || busy) return;
    setBusy(true);
    setError(null);
    try {
      const sale = await approve((tok) =>
        api.pos.finalize({
          cart_id: cart.cart_id!,
          operation_id: opId,
          tenders: tenderList,
          expected_total_minor: due,
          approval_token: tok,
          fulfilment: sending
            ? {
                mode: "send",
                ...addrPayload(send),
                save_on_customer: send.save,
                channel: cart.order?.channel ?? "walk_in",
              }
            : { mode: "here" },
        }),
      );
      onPaid(sale);
    } catch (e) {
      if (e instanceof ApprovalCancelled) return;
      const ex = explain(e);
      setError(`${ex.message} ${ex.action}`.trim());
      // If the basket changed server-side, refresh it and start a fresh attempt.
      if ((e as { code?: string }).code === "conflict") {
        try {
          onCartChanged(await api.pos.cart());
        } catch {
          /* ignore */
        }
        setOpId(newOperationId());
      }
    } finally {
      setBusy(false);
    }
  };

  const pickMethod = (m: string) => {
    if (m === "split") {
      setSplit(true);
      if (rows.length === 0)
        setRows([{ method: tenders[0]?.method ?? "cash", amount: formatAmount(due), reference: "" }]);
      return;
    }
    setSplit(false);
    setMethod(m);
    setAmount(m === "cash" ? "" : formatAmount(due));
    setReference("");
  };

  const denoms = quickCash(due, unit);
  // The numpad writes to the single amount, or to the split row last touched.
  const [activeRow, setActiveRow] = useState(0);
  const editValue = (fn: (v: string) => string) => {
    if (split) setRows(rows.map((x, j) => (j === activeRow ? { ...x, amount: fn(x.amount) } : x)));
    else setAmount(fn(amount));
  };
  const onPad = (k: string) => {
    if (k === "Backspace") editValue((v) => v.slice(0, -1));
    else if (k === "C") editValue(() => "");
    else if (k === "Exact")
      editValue(() => formatAmount(split ? Math.max(0, remaining) + parseMoneyOr0(rows[activeRow]?.amount) : due));
    else if (k === ".") editValue((v) => (v.includes(".") ? v : `${v || "0"}.`));
    else
      editValue((v) => {
        const [, frac] = v.split(".");
        if (frac !== undefined && frac.length >= digits()) return v;
        return v + k;
      });
    amountRef.current?.focus();
  };
  const tiles = [
    ...tenders.map((tv) => ({ method: tv.method, label: t(tv.label) })),
    { method: "split", label: t("Split") },
  ];

  return (
    <Modal
      title={t("Payment")}
      size="sheet"
      testId="payment-sheet"
      onClose={busy ? undefined : onClose}
      footer={
        <div className="pay-foot">
          <div className={`pay-status ${validation ? "" : "ok"}`} role="status">
            {/* Repeated next to the button: at 700 px the change panel can be below the fold. */}
            {validation ?? (change > 0 ? t("Give change {0}", formatMoney(change)) : t("Ready"))}
          </div>
          <Button
            variant="pay"
            size="xl"
            block
            onClick={complete}
            disabled={!!validation}
            loading={busy}
            data-testid="complete-sale"
          >
            {pod ? t("Send, pay on delivery") : sending ? t("Complete and send") : t("Complete Sale")}{" "}
            <span className="money">{formatMoney(due)}</span> <kbd>{t("Enter")}</kbd>
          </Button>
        </div>
      }
    >
      <div
        className="pay-sheet"
        onKeyDown={(e) => e.key === "Enter" && !e.shiftKey && (e.preventDefault(), void complete())}
      >
        <div className="pay-due">
          <div>
            <div className="label">{t("Amount Due")}</div>
            <div className="due money" data-testid="amount-due">
              {formatMoney(due)}
            </div>
          </div>
          <dl className="pay-mini">
            <div>
              <dt>{pod ? t("On delivery") : t("Paid")}</dt>
              <dd className="money">{formatMoney(paid)}</dd>
            </div>
            <div>
              <dt>{t("Remaining")}</dt>
              <dd className="money">{formatMoney(Math.max(0, remaining))}</dd>
            </div>
          </dl>
        </div>
        <SendPanel cart={cart} value={send} onChange={setSend} onCartChanged={onCartChanged} podAllowed={due > 0} />
        <div className={`pay-cols ${pod ? "pod-hidden" : ""}`} hidden={pod}>
          <div className="pay-left">
            <div className="tender-tiles" role="radiogroup" aria-label={t("Payment method")}>
              {tiles.map((tv) => {
                const Icon = tv.method === "split" ? Split : (icons[tv.method] ?? CreditCard);
                const active = tv.method === "split" ? split : !split && method === tv.method;
                return (
                  <button
                    key={tv.method}
                    type="button"
                    role="radio"
                    aria-checked={active}
                    data-testid={`tender-${tv.method}`}
                    className={`tender-tile ${active ? "active" : ""}`}
                    onClick={() => pickMethod(tv.method)}
                  >
                    <Icon size={24} aria-hidden />
                    <span>{tv.label}</span>
                  </button>
                );
              })}
            </div>
            {!split && method === "cash" ? (
              <div className="denoms" aria-label={t("Quick cash")}>
                {denoms.map((d) => (
                  <button key={d} type="button" className="denom money" onClick={() => setAmount(formatAmount(d))}>
                    {formatAmount(d).replace(/\.0+$/, "")}
                  </button>
                ))}
              </div>
            ) : null}
            {!split && method !== "cash" ? (
              <div className="field">
                <label htmlFor="pay-ref">
                  {t("Reference")} {cfg(method)?.requires_reference ? "" : t("(optional)")}
                </label>
                <input
                  id="pay-ref"
                  className="input"
                  value={reference}
                  onChange={(e) => setReference(e.target.value)}
                  placeholder={t("Approval code / last 4 digits / transfer ref")}
                />
                <div className="hint">
                  {method === "benefitpay"
                    ? t("Recorded tender — not verified with the bank. Check the customer's BenefitPay confirmation.")
                    : t("Recorded tender. AMWAPOS does not verify card settlement.")}
                </div>
              </div>
            ) : null}
          </div>
          <div className="pay-right">
            {!split ? (
              <div className="field">
                <label htmlFor="pay-amount">
                  {method === "cash" ? t("Cash received") : t("{0} amount", methodLabel(method))}
                </label>
                <input
                  id="pay-amount"
                  ref={amountRef}
                  className="input lg num"
                  inputMode="decimal"
                  placeholder={formatAmount(due)}
                  value={amount}
                  onChange={(e) => setAmount(e.target.value)}
                  data-testid="pay-amount"
                />
              </div>
            ) : (
              <div className="split-rows">
                {rows.map((r, i) => (
                  <div key={i} className={`split-row ${i === activeRow ? "active" : ""}`}>
                    <select
                      className="select"
                      value={r.method}
                      aria-label={t("Payment {0} method", i + 1)}
                      onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, method: e.target.value } : x)))}
                    >
                      {tenders.map((tv) => (
                        <option key={tv.method} value={tv.method}>
                          {t(tv.label)}
                        </option>
                      ))}
                    </select>
                    <input
                      className="input num"
                      inputMode="decimal"
                      value={r.amount}
                      aria-label={t("Payment {0} amount", i + 1)}
                      onFocus={() => setActiveRow(i)}
                      onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, amount: e.target.value } : x)))}
                      ref={i === activeRow ? amountRef : undefined}
                    />
                    <Button
                      variant="ghost"
                      aria-label={t("Remove payment")}
                      icon={<Trash2 size={20} />}
                      onClick={() => (setRows(rows.filter((_, j) => j !== i)), setActiveRow(0))}
                    />
                    {cfg(r.method)?.requires_reference || r.reference ? (
                      <input
                        className="input ref"
                        placeholder={t("Reference")}
                        aria-label={t("Payment {0} reference", i + 1)}
                        value={r.reference}
                        onChange={(e) =>
                          setRows(rows.map((x, j) => (j === i ? { ...x, reference: e.target.value } : x)))
                        }
                      />
                    ) : null}
                  </div>
                ))}
                <Button
                  icon={<Plus size={20} />}
                  onClick={() => {
                    setRows([
                      ...rows,
                      {
                        method: tenders.find((tv) => tv.method !== rows.at(-1)?.method)?.method ?? "cash",
                        amount: remaining > 0 ? formatAmount(remaining) : "",
                        reference: "",
                      },
                    ]);
                    setActiveRow(rows.length);
                  }}
                >
                  {t("Add Payment")}
                </Button>
              </div>
            )}
            <div className="numpad" aria-label={t("Number pad")}>
              {["7", "8", "9", "Backspace", "4", "5", "6", "C", "1", "2", "3", "Exact", "0", "00", "."].map((k) => (
                <button
                  key={k}
                  type="button"
                  className={`np-key ${k === "Exact" ? "exact" : ""} ${k === "Backspace" || k === "C" ? "fn" : ""}`}
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => onPad(k)}
                  aria-label={
                    k === "Backspace"
                      ? t("Delete digit")
                      : k === "C"
                        ? t("Clear amount")
                        : k === "Exact"
                          ? t("Exact")
                          : k
                  }
                >
                  {k === "Backspace" ? <Delete size={22} aria-hidden /> : k === "Exact" ? t("Exact") : k}
                </button>
              ))}
            </div>
          </div>
        </div>
        {change > 0 && !validation ? (
          <div className="change-panel" data-testid="change">
            <span className="label">{t("CHANGE")}</span>
            <span className="amount money">{formatMoney(change)}</span>
          </div>
        ) : null}
        {error ? (
          <Banner tone="danger" title={t("Sale was not completed")}>
            {error}
          </Banner>
        ) : null}
        <details className="pay-summary">
          <summary>
            {t(
              cart.lines.length === 1 ? "{0} line · {1} items" : "{0} lines · {1} items",
              cart.lines.length,
              formatQty(cart.totals.item_count_milli),
            )}{" "}
            · {t("VAT")} <span className="money">{formatMoney(cart.totals.tax_minor)}</span>
            {cart.totals.discount_minor ? (
              <>
                {" "}
                · {t("Discount")} <span className="money">{formatMoney(-cart.totals.discount_minor)}</span>
              </>
            ) : null}
          </summary>
          <div className="pay-lines">
            {cart.lines.map((l) => (
              <div key={l.line_id} className="row small">
                <span className="grow ellipsis" dir="auto">
                  {l.name}
                </span>
                <span className="num muted">{formatQty(l.qty_milli)}×</span>
                <span className="money" style={{ minWidth: 96, textAlign: "end" }}>
                  {formatMoney(l.line_total_minor)}
                </span>
              </div>
            ))}
          </div>
        </details>
      </div>
    </Modal>
  );
}

function parseMoneyOr0(v: string | undefined): number {
  return (v ? parseMoney(v) : 0) ?? 0;
}

export function SaleSuccess({
  sale,
  returnSeconds,
  onClose,
  onReprint,
  onDelivery,
  onRetryPrint,
}: {
  sale: SaleResult;
  returnSeconds: number;
  onClose: () => void;
  onReprint: () => void;
  onDelivery: () => void;
  onRetryPrint: (jobId: string) => Promise<PrintOutcome>;
}) {
  const [print, setPrint] = useState<PrintOutcome | null>(sale.print);
  const [left, setLeft] = useState(returnSeconds);
  const [paused, setPaused] = useState(false);
  const hasCashChange = sale.change_minor > 0;
  useEffect(() => {
    if (paused || returnSeconds <= 0 || print?.status === "failed") return;
    const tv = setInterval(() => setLeft((l) => l - 1), 1000);
    return () => clearInterval(tv);
  }, [paused, returnSeconds, print]);
  useEffect(() => {
    if (left <= 0 && !paused && returnSeconds > 0 && print?.status !== "failed") onClose();
  }, [left, paused, returnSeconds, onClose, print]);
  return (
    <Modal
      title={t("Sale completed")}
      size="sheet narrow"
      onClose={onClose}
      footer={
        <>
          <Button icon={<Printer size={18} />} onClick={() => (setPaused(true), onReprint())}>
            {t("Reprint")}
          </Button>
          <span onClickCapture={() => setPaused(true)}>
            <WhatsAppSendButton kind="receipt" saleId={sale.sale_id} />
          </span>
          <Button icon={<Truck size={18} />} onClick={() => (setPaused(true), onDelivery())}>
            {t("Delivery")}
          </Button>
          <Button variant="primary" size="xl" block onClick={onClose} autoFocus data-testid="new-sale">
            {t("New Sale")} {returnSeconds > 0 && !paused && print?.status !== "failed" ? `(${Math.max(0, left)})` : ""}
          </Button>
        </>
      }
    >
      <div className="success-screen" onMouseDown={() => setPaused(true)}>
        <div className="success-mark">
          <Check size={34} />
        </div>
        <div className="tiny">{t("Receipt")}</div>
        <div style={{ fontWeight: 700, fontSize: 18 }} data-testid="receipt-number">
          {sale.receipt_number}
        </div>
        <div className="due">{formatMoney(sale.total_minor)}</div>
        <div className="small muted">
          {sale.payments.map((p) => `${methodLabel(p.method)} ${formatMoney(p.tendered_minor)}`).join(" · ")}
        </div>
        {hasCashChange ? (
          <div className="change-panel" style={{ marginTop: 8, minWidth: 260 }}>
            <div className="label">{t("CHANGE")}</div>
            <div className="amount" data-testid="success-change">
              {formatMoney(sale.change_minor)}
            </div>
          </div>
        ) : null}
        {sale.stock_warnings?.length ? (
          <div style={{ marginTop: 12, width: "100%" }}>
            <Banner tone="warning" title={t("Sold below recorded stock")}>
              {sale.stock_warnings.map((w) => tb(w)).join(", ")}
            </Banner>
          </div>
        ) : null}
        <div style={{ marginTop: 12, width: "100%" }}>
          {print?.status === "printed" ? (
            <Banner tone="success">{t("Receipt printed")}</Banner>
          ) : print?.status === "failed" ? (
            <Banner
              tone="warning"
              title={t("Sale completed — receipt could not be printed")}
              action={
                print.job_id ? (
                  <Button size="sm" onClick={async () => setPrint(await onRetryPrint(print.job_id!))}>
                    {t("Retry Print")}
                  </Button>
                ) : null
              }
            >
              {print.message}
            </Banner>
          ) : print?.status === "disabled" ? (
            <Banner tone="info">{t("No receipt printer is configured. The receipt can be reprinted later.")}</Banner>
          ) : null}
        </div>
      </div>
    </Modal>
  );
}

/**
 * Amounts a customer is likely to hand over for `due` (integer minor units):
 * the bill rounded up to the next 1, 5, 10, 20, 50 and 100 of the currency,
 * without repeats, at most four. Every suggestion covers the bill.
 */
export function quickCash(due: number, unit: number): number[] {
  const out: number[] = [];
  for (const k of [1, 5, 10, 20, 50, 100]) {
    const step = k * unit;
    const v = Math.max(step, Math.ceil(due / step) * step);
    if (!out.includes(v)) out.push(v);
  }
  return out.slice(0, 4);
}
