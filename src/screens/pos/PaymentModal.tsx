import { useEffect, useMemo, useRef, useState } from "react";
import { Banknote, Check, CreditCard, Landmark, Plus, Smartphone, Split, Trash2, Truck, Printer } from "lucide-react";
import { api } from "../../api";
import type { Cart, PrintOutcome, SaleResult, TenderConfig, TenderInput } from "../../api/types";
import { useApproval, ApprovalCancelled } from "../../components/approval";
import { explain } from "../../lib/errors";
import { newOperationId } from "../../lib/ids";
import { digits, formatAmount, formatMoney, formatQty, parseMoney } from "../../lib/money";
import { Banner, Button, Modal } from "../../components/ui";
import { methodLabel } from "./labels";

const icons: Record<string, typeof Banknote> = { cash: Banknote, card: CreditCard, benefitpay: Smartphone, bank_transfer: Landmark };

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
  const [method, setMethod] = useState(tenders.some((t) => t.method === initialMethod) ? initialMethod : tenders[0]?.method ?? "cash");
  const [amount, setAmount] = useState(initialMethod === "cash" ? "" : formatAmount(due));
  const [reference, setReference] = useState("");
  const [rows, setRows] = useState<Row[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // One operation id per payment attempt: a retry after a timeout can never double-charge.
  const [opId, setOpId] = useState(newOperationId);
  const amountRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    amountRef.current?.focus();
    amountRef.current?.select();
  }, [method, split]);

  const cfg = (m: string) => tenders.find((t) => t.method === m);
  const unit = 10 ** digits();

  const tenderList: TenderInput[] | null = useMemo(() => {
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
  }, [split, rows, amount, method, reference, due]);

  const paid = (tenderList ?? []).reduce((a, t) => a + t.amount_minor, 0);
  const nonCash = (tenderList ?? []).filter((t) => !cfg(t.method)?.allows_change).reduce((a, t) => a + t.amount_minor, 0);
  const cashIn = paid - nonCash;
  const remaining = due - paid;
  const change = paid > due ? paid - due : 0;
  const validation = useMemo(() => {
    if (!tenderList) return split ? "Enter an amount for each payment." : method === "cash" ? "Enter the cash received or press Exact." : "Enter the amount.";
    if (nonCash > due) return "Card and other non-cash payments cannot exceed the amount due.";
    if (remaining > 0) return `Remaining ${formatMoney(remaining)}.`;
    if (change > cashIn) return "Change can only be given from cash.";
    for (const t of tenderList) {
      if (cfg(t.method)?.requires_reference && !t.reference) return `${methodLabel(t.method)} requires a reference.`;
    }
    return null;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tenderList, nonCash, due, remaining, change, cashIn, split, method]);

  const complete = async () => {
    if (validation || !tenderList || !cart.cart_id || busy) return;
    setBusy(true);
    setError(null);
    try {
      const sale = await approve((tok) =>
        api.pos.finalize({ cart_id: cart.cart_id!, operation_id: opId, tenders: tenderList, expected_total_minor: due, approval_token: tok }),
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
      if (rows.length === 0) setRows([{ method: tenders[0]?.method ?? "cash", amount: formatAmount(due), reference: "" }]);
      return;
    }
    setSplit(false);
    setMethod(m);
    setAmount(m === "cash" ? "" : formatAmount(due));
    setReference("");
  };

  const denoms = [5, 10, 20, 50].map((d) => d * unit);

  return (
    <Modal
      title="Payment"
      size="xl"
      onClose={busy ? undefined : onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            Back to sale
          </Button>
          <span className="small muted grow" style={{ textAlign: "right" }}>
            {validation ?? (change > 0 ? `Change ${formatMoney(change)}` : "Ready")}
          </span>
          <Button variant="primary" size="lg" onClick={complete} disabled={!!validation} loading={busy} data-testid="complete-sale">
            Complete Sale <span className="kbd">Enter</span>
          </Button>
        </>
      }
    >
      <div className="pay-grid" onKeyDown={(e) => e.key === "Enter" && !e.shiftKey && (e.preventDefault(), void complete())}>
        <div className="col gap-16">
          <div>
            <div className="tiny">Amount Due</div>
            <div className="due" data-testid="amount-due">
              {formatMoney(due)}
            </div>
          </div>
          <div className="method-cards" role="radiogroup" aria-label="Payment method">
            {tenders.map((t) => {
              const Icon = icons[t.method] ?? CreditCard;
              const active = !split && method === t.method;
              return (
                <button key={t.method} role="radio" aria-checked={active} className={`method-card ${active ? "active" : ""}`} onClick={() => pickMethod(t.method)}>
                  <Icon size={20} /> {t.label}
                </button>
              );
            })}
            <button role="radio" aria-checked={split} className={`method-card ${split ? "active" : ""}`} onClick={() => pickMethod("split")}>
              <Split size={20} /> Split
            </button>
          </div>
          {!split ? (
            <>
              <div className="field">
                <label htmlFor="pay-amount">{method === "cash" ? "Cash received" : `${methodLabel(method)} amount`}</label>
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
              {method === "cash" ? (
                <div className="denoms">
                  <Button size="lg" onClick={() => setAmount(formatAmount(due))}>
                    Exact
                  </Button>
                  {denoms.map((d) => (
                    <Button key={d} size="lg" onClick={() => setAmount(formatAmount(d))} disabled={d < due}>
                      {formatAmount(d).replace(/\.0+$/, "")}
                    </Button>
                  ))}
                </div>
              ) : (
                <div className="field">
                  <label htmlFor="pay-ref">Reference {cfg(method)?.requires_reference ? "" : "(optional)"}</label>
                  <input id="pay-ref" className="input" value={reference} onChange={(e) => setReference(e.target.value)} placeholder="Approval code / last 4 digits / transfer ref" />
                  <div className="hint">
                    {method === "benefitpay"
                      ? "Recorded tender — not verified with the bank. Check the customer's BenefitPay confirmation."
                      : "Recorded tender. AMWAPOS does not verify card settlement."}
                  </div>
                </div>
              )}
            </>
          ) : (
            <div className="col">
              {rows.map((r, i) => (
                <div key={i} className="row">
                  <select className="select" style={{ width: 160 }} value={r.method} onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, method: e.target.value } : x)))}>
                    {tenders.map((t) => (
                      <option key={t.method} value={t.method}>
                        {t.label}
                      </option>
                    ))}
                  </select>
                  <input
                    className="input num"
                    style={{ width: 140 }}
                    inputMode="decimal"
                    value={r.amount}
                    aria-label={`Payment ${i + 1} amount`}
                    onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, amount: e.target.value } : x)))}
                    ref={i === 0 ? amountRef : undefined}
                  />
                  <input className="input grow" placeholder="Reference (optional)" value={r.reference} onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, reference: e.target.value } : x)))} />
                  <Button variant="ghost" aria-label="Remove payment" icon={<Trash2 size={16} />} onClick={() => setRows(rows.filter((_, j) => j !== i))} />
                </div>
              ))}
              <Button
                icon={<Plus size={16} />}
                onClick={() => setRows([...rows, { method: tenders.find((t) => t.method !== rows.at(-1)?.method)?.method ?? "cash", amount: remaining > 0 ? formatAmount(remaining) : "", reference: "" }])}
              >
                Add Payment
              </Button>
            </div>
          )}
          {change > 0 && !validation ? (
            <div className="change-panel" data-testid="change">
              <div className="label">CHANGE</div>
              <div className="amount">{formatMoney(change)}</div>
            </div>
          ) : null}
          {error ? <Banner tone="danger" title="Sale was not completed">{error}</Banner> : null}
        </div>
        <div className="card" style={{ alignSelf: "start" }}>
          <div className="card-head">
            <h3>Sale summary</h3>
            <span className="right tiny">{cart.lines.length} lines</span>
          </div>
          <div className="card-body" style={{ maxHeight: 320, overflow: "auto" }}>
            {cart.lines.map((l) => (
              <div key={l.line_id} className="row small" style={{ padding: "3px 0" }}>
                <span className="grow ellipsis">{l.name}</span>
                <span className="num muted">{formatQty(l.qty_milli)}×</span>
                <span className="num" style={{ minWidth: 90, textAlign: "right" }}>
                  {formatMoney(l.line_total_minor)}
                </span>
              </div>
            ))}
          </div>
          <div className="totals">
            <div className="t-row">
              <span>Subtotal</span>
              <span>{formatMoney(cart.totals.subtotal_minor)}</span>
            </div>
            {cart.totals.discount_minor ? (
              <div className="t-row">
                <span>Discount</span>
                <span>{formatMoney(-cart.totals.discount_minor)}</span>
              </div>
            ) : null}
            <div className="t-row">
              <span>VAT</span>
              <span>{formatMoney(cart.totals.tax_minor)}</span>
            </div>
            <div className="t-row" style={{ fontWeight: 700, color: "var(--text)" }}>
              <span>Paid</span>
              <span>{formatMoney(paid)}</span>
            </div>
            <div className="t-row">
              <span>Remaining</span>
              <span>{formatMoney(Math.max(0, remaining))}</span>
            </div>
          </div>
        </div>
      </div>
    </Modal>
  );
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
    const t = setInterval(() => setLeft((l) => l - 1), 1000);
    return () => clearInterval(t);
  }, [paused, returnSeconds, print]);
  useEffect(() => {
    if (left <= 0 && !paused && returnSeconds > 0 && print?.status !== "failed") onClose();
  }, [left, paused, returnSeconds, onClose, print]);
  return (
    <Modal
      title="Sale completed"
      size="md"
      onClose={onClose}
      footer={
        <>
          <Button icon={<Printer size={16} />} onClick={() => (setPaused(true), onReprint())}>
            Reprint
          </Button>
          <Button icon={<Truck size={16} />} onClick={() => (setPaused(true), onDelivery())}>
            Delivery
          </Button>
          <Button variant="primary" size="lg" className="right" onClick={onClose} autoFocus data-testid="new-sale">
            New Sale {returnSeconds > 0 && !paused && print?.status !== "failed" ? `(${Math.max(0, left)})` : ""}
          </Button>
        </>
      }
    >
      <div className="success-screen" onMouseDown={() => setPaused(true)}>
        <div className="success-mark">
          <Check size={34} />
        </div>
        <div className="tiny">Receipt</div>
        <div style={{ fontWeight: 700, fontSize: 18 }} data-testid="receipt-number">
          {sale.receipt_number}
        </div>
        <div className="due">{formatMoney(sale.total_minor)}</div>
        <div className="small muted">{sale.payments.map((p) => `${methodLabel(p.method)} ${formatMoney(p.tendered_minor)}`).join(" · ")}</div>
        {hasCashChange ? (
          <div className="change-panel" style={{ marginTop: 8, minWidth: 260 }}>
            <div className="label">CHANGE</div>
            <div className="amount" data-testid="success-change">
              {formatMoney(sale.change_minor)}
            </div>
          </div>
        ) : null}
        <div style={{ marginTop: 12, width: "100%" }}>
          {print?.status === "printed" ? (
            <Banner tone="success">Receipt printed</Banner>
          ) : print?.status === "failed" ? (
            <Banner
              tone="warning"
              title="Sale completed — receipt could not be printed"
              action={
                print.job_id ? (
                  <Button size="sm" onClick={async () => setPrint(await onRetryPrint(print.job_id!))}>
                    Retry Print
                  </Button>
                ) : null
              }
            >
              {print.message}
            </Banner>
          ) : print?.status === "disabled" ? (
            <Banner tone="info">No receipt printer is configured. The receipt can be reprinted later.</Banner>
          ) : null}
        </div>
      </div>
    </Modal>
  );
}
