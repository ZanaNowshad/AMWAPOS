// Confirming an order can meet two honest warnings from the backend: some
// items are short right now, or the total changed since the customer was told
// it. Neither is an error the person cannot get past: they see exactly what is
// wrong, in plain words, and choose to go back or confirm anyway.
import { useCallback, useState } from "react";
import { AlertTriangle } from "lucide-react";
import { ApiError } from "../api/transport";
import type { ConfirmAck } from "../api/types";
import { Button, Modal } from "../components/ui";
import { formatMoney, formatQty } from "../lib/money";
import { t } from "../i18n";

export interface ShortLine {
  name: string;
  wanted_milli: number;
  free_milli: number;
}

export type ConfirmWarning =
  | { kind: "stock_shortage"; lines: ShortLine[] }
  | { kind: "price_changed_since_quote"; quoted_minor: number; current_minor: number };

/** The warning carried by a confirm error, or null for any other error. */
export function warningOf(e: unknown): ConfirmWarning | null {
  if (!(e instanceof ApiError) || e.code !== "conflict" || !e.details) return null;
  const d = e.details as Record<string, unknown>;
  if (d.kind === "stock_shortage" && Array.isArray(d.lines)) {
    return { kind: "stock_shortage", lines: d.lines as ShortLine[] };
  }
  if (d.kind === "price_changed_since_quote") {
    return {
      kind: "price_changed_since_quote",
      quoted_minor: Number(d.quoted_minor),
      current_minor: Number(d.current_minor),
    };
  }
  return null;
}

/** The acknowledgement that answers a warning. */
export function ackFor(w: ConfirmWarning): ConfirmAck {
  return w.kind === "stock_shortage" ? { acknowledge_shortage: true } : { acknowledge_price_change: true };
}

interface Pending<T> {
  warning: ConfirmWarning;
  ack: ConfirmAck;
  fn: (ack: ConfirmAck) => Promise<T>;
  resolve: (v: T | undefined) => void;
  reject: (e: unknown) => void;
}

/**
 * `confirm(ack => api.orders.confirm(id, ack))` resolves with the result, or
 * `undefined` when the person chose to go back. Other errors are thrown.
 * Render `dialog` somewhere in the component.
 */
export function useConfirmWithWarnings<T>() {
  const [pending, setPending] = useState<Pending<T> | null>(null);
  const [busy, setBusy] = useState(false);
  const attempt = useCallback(
    (fn: (ack: ConfirmAck) => Promise<T>, ack: ConfirmAck): Promise<T | undefined> =>
      fn(ack).catch((e: unknown) => {
        const warning = warningOf(e);
        if (!warning) throw e;
        return new Promise<T | undefined>((resolve, reject) => setPending({ warning, ack, fn, resolve, reject }));
      }),
    [],
  );
  const confirm = useCallback((fn: (ack: ConfirmAck) => Promise<T>) => attempt(fn, {}), [attempt]);
  const dialog = pending ? (
    <WarningDialog
      warning={pending.warning}
      busy={busy}
      onBack={() => {
        setPending(null);
        pending.resolve(undefined);
      }}
      onAnyway={async () => {
        const p = pending;
        setBusy(true);
        setPending(null);
        try {
          // A second, different warning may follow (price first, then stock).
          p.resolve(await attempt(p.fn, { ...p.ack, ...ackFor(p.warning) }));
        } catch (e) {
          p.reject(e);
        } finally {
          setBusy(false);
        }
      }}
    />
  ) : null;
  return { confirm, dialog };
}

function WarningDialog({
  warning,
  busy,
  onBack,
  onAnyway,
}: {
  warning: ConfirmWarning;
  busy: boolean;
  onBack: () => void;
  onAnyway: () => void;
}) {
  const stock = warning.kind === "stock_shortage";
  return (
    <Modal
      title={
        <span className="row gap-8">
          <AlertTriangle size={20} aria-hidden /> {stock ? t("Not enough stock") : t("The total has changed")}
        </span>
      }
      size="sm"
      onClose={onBack}
      testId="confirm-warning"
      footer={
        <>
          <Button onClick={onBack}>{stock ? t("Change the order") : t("Go back")}</Button>
          <Button variant="primary" className="right" loading={busy} onClick={onAnyway} data-testid="confirm-anyway">
            {t("Confirm anyway")}
          </Button>
        </>
      }
    >
      {warning.kind === "stock_shortage" ? (
        <div className="col gap-12">
          <p>{t("Some items are short right now:")}</p>
          <ul className="plain-list">
            {warning.lines.map((l) => (
              <li key={l.name} className="row">
                <strong className="grow" dir="auto">
                  {l.name}
                </strong>
                <span className="small">
                  {t("wanted {0}, available {1}", formatQty(l.wanted_milli), formatQty(Math.max(0, l.free_milli)))}
                </span>
              </li>
            ))}
          </ul>
          <p className="small muted">
            {t("Change the order first, or confirm anyway: only the stock that is available is held for it.")}
          </p>
        </div>
      ) : (
        <div className="col gap-12">
          <p>
            {t(
              "The customer was told {0}. With today's prices it is {1}.",
              formatMoney(warning.quoted_minor),
              formatMoney(warning.current_minor),
            )}
          </p>
          <p className="small muted">{t("Send the customer the new total first, or confirm anyway.")}</p>
        </div>
      )}
    </Modal>
  );
}
