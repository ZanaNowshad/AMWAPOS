// The Send loop on the till and the board: Person → Channel → Ticket → Drop → Close.
// A ticket is an order the shop must fulfil (a sent sale or a digital order);
// its drop is the delivery job. One pay state everywhere.
import { useCallback, useEffect, useState } from "react";
import {
  Banknote,
  Check,
  MapPin,
  MessageCircle,
  Paperclip,
  Search,
  Store,
  Truck,
  Undo2,
  UserPlus,
  X,
} from "lucide-react";
import { api } from "../../api";
import type {
  Cart,
  CustomerRow,
  PayState,
  RiderCash,
  TicketCounts,
  TicketRow,
  TicketSheet as Sheet,
} from "../../api/types";
import { useSession } from "../../state/session";
import { useFeature } from "../../components/FeatureGate";
import { Banner, Button, Checkbox, Chip, Modal } from "../../components/ui";
import { explain } from "../../lib/errors";
import { ApprovalCancelled, useApproval } from "../../components/approval";
import { newOperationId } from "../../lib/ids";
import { formatAmount, formatMoney, formatQty, parseMoney } from "../../lib/money";
import { relative } from "../../lib/time";
import { t } from "../../i18n";
import { methodLabel } from "./labels";
import { WhatsAppSendButton, fileToBase64 } from "../admin/automation";
import { AddressFields, type AddrValue, addrFrom, addrPayload, emptyAddr } from "../../components/AddressFields";
import { useConfirmWithWarnings } from "../orderConfirm";

/** Places a drop goes to (same list as the backend lexicon). */
export const AREAS = [
  "Riffa",
  "Riffa East",
  "Riffa West",
  "Muharraq",
  "Manama",
  "Isa Town",
  "Sitra",
  "Hamad Town",
  "A'ali",
  "Budaiya",
  "Saar",
  "Juffair",
  "Seef",
  "Amwaj",
  "Tubli",
  "Sanad",
  "Galali",
  "Duraz",
  "Janabiyah",
  "Hidd",
  "Diyya",
  "Samaheej",
];

const payTone: Record<PayState, "warning" | "success" | "info"> = {
  unpaid: "warning",
  recorded: "info",
  screenshot_pending: "info",
  paid: "success",
};

export function payLabel(p: PayState): string {
  switch (p) {
    case "unpaid":
      return t("Unpaid");
    case "recorded":
      return t("Payment recorded");
    case "screenshot_pending":
      return t("Screenshot to check");
    default:
      return t("Paid");
  }
}

export function PayChip({ state }: { state: PayState }) {
  return (
    <span data-testid="pay-chip" data-state={state}>
      <Chip tone={payTone[state]} dot>
        {payLabel(state)}
      </Chip>
    </span>
  );
}

export function statusLabel(s: string): string {
  switch (s) {
    case "draft":
      return t("Draft");
    case "confirmed":
      return t("Confirmed");
    case "pending":
      return t("New");
    case "preparing":
      return t("Packing");
    case "dispatched":
      return t("On the way");
    case "delivered":
      return t("Delivered");
    case "cancelled":
      return t("Cancelled");
    default:
      return s;
  }
}

/** The button label for moving a drop to this status. */
function stepLabel(s: string): string {
  switch (s) {
    case "preparing":
      return t("Start packing");
    case "dispatched":
      return t("Send out");
    case "delivered":
      return t("Delivered");
    case "cancelled":
      return t("Cancel");
    default:
      return statusLabel(s);
  }
}

export function StatusChip({ status }: { status: string }) {
  const tone =
    status === "delivered"
      ? "success"
      : status === "cancelled"
        ? "danger"
        : status === "dispatched"
          ? "brand"
          : status === "draft" || status === "confirmed"
            ? "default"
            : "info";
  return <Chip tone={tone}>{statusLabel(status)}</Chip>;
}

export function channelLabel(c: string | null): string {
  switch (c) {
    case "walk_in":
      return t("Walk-in");
    case "phone":
      return t("Phone");
    case "whatsapp":
      return t("WhatsApp");
    case "web":
      return t("Web");
    case "other":
      return t("Other");
    default:
      return "";
  }
}

function useErr() {
  const [error, setError] = useState<string | null>(null);
  const handle = (e: unknown) => {
    const ex = explain(e);
    setError(`${ex.message} ${ex.action}`.trim());
  };
  return { error, setError, handle };
}

/** Area as chips: what matches the typed text, or the common places. */
export function AreaPicker({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const q = value.trim().toLowerCase();
  const shown = (q ? AREAS.filter((a) => a.toLowerCase().includes(q) && a !== value) : AREAS).slice(0, 8);
  return (
    <div className="field">
      <label htmlFor="send-area">{t("Area")}</label>
      <input
        id="send-area"
        className="input"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        autoComplete="off"
        data-testid="send-area"
      />
      {shown.length ? (
        <div className="area-chips" role="listbox" aria-label={t("Areas")}>
          {shown.map((a) => (
            <button
              key={a}
              type="button"
              role="option"
              aria-selected={a === value}
              className={`filter-chip ${a === value ? "active" : ""}`}
              onClick={() => onChange(a)}
            >
              {a}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

export interface SendState extends AddrValue {
  mode: "here" | "send";
  save: boolean;
  pod: boolean;
}

/** Prefill from the customer: their parts, or their one-line address. */
function fromCustomer(c: Cart["customer"]): AddrValue {
  return c ? addrFrom(c) : emptyAddr;
}

export function initialSend(cart: Cart): SendState {
  const o = cart.order;
  const base = o?.address
    ? { ...emptyAddr, address: o.address, area: cart.customer?.area || "" }
    : fromCustomer(cart.customer);
  return { ...base, mode: o?.delivery_wanted ? "send" : "here", save: false, pod: false };
}

const hasAddr = (v: AddrValue) => !!(v.address || v.area || v.flat || v.building || v.road || v.block);

/** Here | Send, and for Send: who, where, and whether they pay at the door. */
export function SendPanel({
  cart,
  value,
  onChange,
  onCartChanged,
  podAllowed,
}: {
  cart: Cart;
  value: SendState;
  onChange: (v: SendState) => void;
  onCartChanged: (c: Cart) => void;
  podAllowed: boolean;
}) {
  const [q, setQ] = useState("");
  const [rows, setRows] = useState<CustomerRow[]>([]);
  const [creating, setCreating] = useState(false);
  const { error, handle } = useErr();
  const customer = cart.customer;
  useEffect(() => {
    if (value.mode !== "send" || customer || q.trim().length < 2) {
      setRows([]);
      return;
    }
    const tv = setTimeout(() => {
      api.customers.search(q, false, 6).then(setRows).catch(handle);
    }, 150);
    return () => clearTimeout(tv);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [q, value.mode, customer]);
  const attach = async (id: string | null) => {
    try {
      const c = await api.pos.setCustomer(id);
      onCartChanged(c);
      if (c.customer && !hasAddr(value)) {
        // Prefill from the customer, editable for this drop only.
        onChange({ ...value, ...fromCustomer(c.customer) });
      }
      setQ("");
    } catch (e) {
      handle(e);
    }
  };
  return (
    <div className="send-panel" data-testid="send-panel">
      <div className="seg" role="radiogroup" aria-label={t("Fulfil")}>
        <button
          type="button"
          role="radio"
          aria-checked={value.mode === "here"}
          className={`seg-btn ${value.mode === "here" ? "active" : ""}`}
          onClick={() => onChange({ ...value, mode: "here", pod: false })}
          data-testid="fulfil-here"
        >
          <Store size={20} aria-hidden /> {t("Here")}
        </button>
        <button
          type="button"
          role="radio"
          aria-checked={value.mode === "send"}
          className={`seg-btn ${value.mode === "send" ? "active" : ""}`}
          onClick={() => onChange({ ...value, ...(hasAddr(value) ? {} : fromCustomer(customer)), mode: "send" })}
          data-testid="fulfil-send"
        >
          <Truck size={20} aria-hidden /> {t("Send")}
        </button>
      </div>
      {value.mode === "send" ? (
        <div className="send-body">
          {customer ? (
            <div className="send-who">
              <div className="grow">
                <div className="strong" dir="auto">
                  {customer.name}
                </div>
                <div className="tiny muted num">{customer.phone}</div>
              </div>
              <Button icon={<X size={16} />} onClick={() => void attach(null)}>
                {t("Change")}
              </Button>
            </div>
          ) : (
            <div className="col gap-8">
              <div className="row">
                <div className="scan-box grow send-search">
                  <Search size={20} className="scan-icon" aria-hidden />
                  <input
                    className="input"
                    placeholder={t("Customer phone or name")}
                    value={q}
                    onChange={(e) => setQ(e.target.value)}
                    aria-label={t("Find customer")}
                    data-testid="send-customer-search"
                  />
                </div>
                <Button icon={<UserPlus size={18} />} onClick={() => setCreating(true)} data-testid="send-new-customer">
                  {t("New customer")}
                </Button>
              </div>
              {rows.map((c) => (
                <button
                  key={c.customer_id}
                  type="button"
                  className="send-row"
                  onClick={() => void attach(c.customer_id)}
                  data-testid="send-customer-row"
                >
                  <span className="grow ellipsis" dir="auto">
                    {c.name}
                  </span>
                  <span className="tiny muted">{[c.area, c.phone].filter(Boolean).join(" · ")}</span>
                </button>
              ))}
              {!rows.length ? <div className="hint">{t("A sent sale needs the customer.")}</div> : null}
            </div>
          )}
          <div className="send-where">
            <AddressFields value={value} onChange={(a) => onChange({ ...value, ...a })} idPrefix="send" />
            <AreaPicker value={value.area} onChange={(area) => onChange({ ...value, area })} />
          </div>
          <div className="send-opts">
            <Checkbox
              label={t("Save on customer")}
              checked={value.save}
              onChange={(save) => onChange({ ...value, save })}
            />
            {podAllowed ? (
              <label className="checkbox pod-toggle" data-testid="pay-on-delivery">
                <input
                  type="checkbox"
                  checked={value.pod}
                  onChange={(e) => onChange({ ...value, pod: e.target.checked })}
                />
                <span>{t("Pay on delivery")}</span>
              </label>
            ) : null}
          </div>
          {value.pod ? (
            <div className="hint">
              {t("Nothing goes in the drawer now. The ticket stays unpaid until the money is recorded.")}
            </div>
          ) : null}
        </div>
      ) : null}
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {creating ? (
        <NewCustomerSheet
          initial={q}
          onClose={() => setCreating(false)}
          onCreated={(c, addr) => {
            setCreating(false);
            onCartChanged(c);
            onChange({ ...value, ...(hasAddr(addr) ? addr : {}), mode: "send" });
          }}
        />
      ) : null}
    </div>
  );
}

/** New customer: name, phone, address, area. Attached to the sale on save. */
export function NewCustomerSheet({
  initial,
  onClose,
  onCreated,
}: {
  initial: string;
  onClose: () => void;
  onCreated: (c: Cart, address: AddrValue) => void;
}) {
  const looksPhone = /^\+?\d[\d\s]*$/.test(initial.trim());
  const [name, setName] = useState(looksPhone ? "" : initial);
  const [phone, setPhone] = useState(looksPhone ? initial : "");
  const [addr, setAddr] = useState<AddrValue>(emptyAddr);
  const [busy, setBusy] = useState(false);
  const { error, handle, setError } = useErr();
  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      const c = await api.customers.save(null, {
        name,
        phone: phone || null,
        ...addrPayload(addr),
        active: true,
      });
      onCreated(await api.pos.setCustomer(c.customer_id), { ...addr, area: addr.area || c.area || "" });
    } catch (e) {
      handle(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title={t("New customer")}
      size="md"
      onClose={busy ? undefined : onClose}
      footer={
        <Button
          variant="primary"
          size="lg"
          block
          onClick={save}
          loading={busy}
          disabled={!name.trim()}
          data-testid="new-customer-save"
        >
          {t("Save customer")}
        </Button>
      }
    >
      <div className="col gap-12">
        <div className="field">
          <label htmlFor="nc-name">{t("Name")}</label>
          <input id="nc-name" className="input" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
        </div>
        <div className="field">
          <label htmlFor="nc-phone">{t("Phone")}</label>
          <input
            id="nc-phone"
            className="input num"
            inputMode="tel"
            value={phone}
            onChange={(e) => setPhone(e.target.value)}
          />
        </div>
        <AddressFields value={addr} onChange={setAddr} idPrefix="nc" />
        <AreaPicker value={addr.area} onChange={(area) => setAddr({ ...addr, area })} />
        {error ? <Banner tone="danger">{error}</Banner> : null}
      </div>
    </Modal>
  );
}

/** One 56 px row on the rail or the board. */
export function TicketRowButton({ row, onOpen }: { row: TicketRow; onOpen: (r: TicketRow) => void }) {
  return (
    <button type="button" className="ticket-row" onClick={() => onOpen(row)} data-testid="ticket-row">
      <span className="tr-main">
        <span className="tr-name ellipsis" dir="auto">
          {row.customer_name || row.phone || row.number}
        </span>
        <span className="tr-place ellipsis tiny muted" dir="auto">
          {[row.area, row.address].filter(Boolean).join(" · ") || t("No address")}
        </span>
      </span>
      <span className="tr-side">
        <span className="money">{formatMoney(row.amount_minor)}</span>
        <span className="tr-chips">
          {row.cash_with ? <CashWithChip name={row.cash_with} /> : <PayChip state={row.pay_state} />}
          <StatusChip status={row.status} />
        </span>
      </span>
    </button>
  );
}

/** Cash collected at the door that the rider still holds. */
export function CashWithChip({ name }: { name: string }) {
  return (
    <span data-testid="cash-with">
      <Chip tone="warning">{t("Cash with {0}", name)}</Chip>
    </span>
  );
}

type RailTab = "now" | "out" | "done";

/** The till's Send rail: a 420 px drawer on the assistant's side, above the dock. */
export function SendRail({
  onClose,
  onOpen,
  reloadKey,
}: {
  onClose: () => void;
  onOpen: (r: TicketRow) => void;
  reloadKey: number;
}) {
  const { has } = useSession();
  const [tab, setTab] = useState<RailTab>("now");
  const [handover, setHandover] = useState<string | null | false>(false);
  const [rows, setRows] = useState<TicketRow[] | null>(null);
  const [counts, setCounts] = useState<TicketCounts | null>(null);
  const [holding, setHolding] = useState<RiderCash[]>([]);
  const { error, handle, setError } = useErr();
  const canHandOver = has("pos.sell");
  const load = useCallback(async () => {
    setError(null);
    try {
      const [r, c, h] = await Promise.all([
        api.tickets.list({ tab }),
        api.tickets.counts(),
        canHandOver ? api.riders.cash() : Promise.resolve([] as RiderCash[]),
      ]);
      setRows(r);
      setCounts(c);
      setHolding(h);
    } catch (e) {
      handle(e);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab]);
  useEffect(() => {
    void load();
  }, [load, reloadKey]);
  const tabs: { key: RailTab; label: string; n?: number }[] = [
    { key: "now", label: t("To do"), n: counts?.now },
    { key: "out", label: t("On the way"), n: counts?.out },
    { key: "done", label: t("Done today"), n: counts?.done },
  ];
  return (
    <aside className="till-ai send-rail" role="complementary" aria-label={t("Send")} data-testid="send-rail">
      <div className="till-ai-head">
        <Truck size={20} aria-hidden />
        <h2 className="grow">{t("Send")}</h2>
        {has("pos.sell") ? (
          <Button
            variant="ghost"
            icon={<Banknote size={20} />}
            onClick={() => setHandover(null)}
            data-testid="rider-handover-open"
          >
            {t("Rider hand-over")}
          </Button>
        ) : null}
        <Button
          variant="ghost"
          className="close-btn"
          aria-label={t("Close")}
          icon={<X size={22} />}
          onClick={onClose}
        />
      </div>
      {handover !== false ? (
        <RiderHandoverSheet
          initialRider={handover}
          onClose={() => setHandover(false)}
          onDone={() => {
            setHandover(false);
            void load();
          }}
        />
      ) : null}
      <div className="rail-tabs" role="tablist">
        {tabs.map((tv) => (
          <button
            key={tv.key}
            type="button"
            role="tab"
            aria-selected={tab === tv.key}
            className={`rail-tab ${tab === tv.key ? "active" : ""}`}
            onClick={() => setTab(tv.key)}
            data-testid={`rail-tab-${tv.key}`}
          >
            {tv.label}
            {tv.n ? <span className="rail-n num">{tv.n}</span> : null}
          </button>
        ))}
      </div>
      {holding.length ? (
        <div className="rail-cash" data-testid="rail-cash">
          {holding.map((h) => (
            <button
              key={h.rider_user_id}
              type="button"
              className="rail-cash-row"
              onClick={() => setHandover(h.rider_user_id)}
              data-testid="rail-cash-row"
            >
              <Banknote size={20} aria-hidden />
              <span className="grow ellipsis">
                <span className="strong" dir="auto">
                  {h.name}
                </span>{" "}
                <span className="tiny">
                  {h.held_minor > 0 ? t("holds {0}", formatMoney(h.held_minor)) : ""}
                  {h.held_minor > 0 && h.uncollected_minor > 0 ? " · " : ""}
                  {h.uncollected_minor > 0 ? t("{0} still to collect", formatMoney(h.uncollected_minor)) : ""}
                </span>
              </span>
              <span className="rail-cash-go">{t("Hand over")}</span>
            </button>
          ))}
        </div>
      ) : null}
      <div className="rail-list">
        {error ? <Banner tone="danger">{error}</Banner> : null}
        {rows && rows.length === 0 ? (
          <div className="rail-empty" data-testid="rail-empty">
            {tab === "now"
              ? t("Nothing to send. To deliver a sale, tap Send on the payment screen.")
              : tab === "out"
                ? t("Nothing is on the way.")
                : t("Nothing closed today.")}
          </div>
        ) : null}
        {rows?.map((r) => (
          <TicketRowButton key={r.ticket_id} row={r} onOpen={onOpen} />
        ))}
      </div>
    </aside>
  );
}

/** The ticket sheet: same on the till and the board. */
export function TicketSheet({
  ticketId,
  onClose,
  onChanged,
  onRungUp,
}: {
  ticketId: string;
  onClose: () => void;
  onChanged: () => void;
  /** Ring up a digital order into this till's sale (till only). */
  onRungUp?: (c: Cart) => void;
}) {
  const { config } = useSession();
  const shotsOn = useFeature("ocr.payment_screenshots");
  const [sheet, setSheet] = useState<Sheet | null>(null);
  const [busy, setBusy] = useState(false);
  const [paying, setPaying] = useState(false);
  const [unable, setUnable] = useState(false);
  const [closing, setClosing] = useState(false);
  const [deliverAsk, setDeliverAsk] = useState(false);
  const [thenDeliver, setThenDeliver] = useState(false);
  const [ringOp] = useState(newOperationId);
  const warn = useConfirmWithWarnings<unknown>();
  const { error, handle, setError } = useErr();
  const load = useCallback(async () => {
    try {
      setSheet(await api.tickets.get(ticketId));
    } catch (e) {
      handle(e);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ticketId]);
  useEffect(() => {
    void load();
  }, [load]);
  const run = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await fn();
      await load();
      onChanged();
    } catch (e) {
      handle(e);
    } finally {
      setBusy(false);
    }
  };
  if (!sheet) {
    return (
      <Modal title={t("Ticket")} size="sheet" onClose={onClose}>
        {error ? <Banner tone="danger">{error}</Banner> : <div className="muted">{t("Loading…")}</div>}
      </Modal>
    );
  }
  const tk = sheet.ticket;
  const did = tk.delivery_id;
  const settled = tk.pay_state === "paid" || tk.pay_state === "recorded";
  const step = (to: string) => {
    if (!did) return;
    if (to === "delivered" && !settled && tk.outstanding_minor > 0) {
      setDeliverAsk(true);
      return;
    }
    void run(() => api.deliveries.update({ delivery_id: did, status: to }));
  };
  const lastEvent = sheet.events.at(-1);
  const canUndo = sheet.can.undo && !!lastEvent?.from && lastEvent.from !== lastEvent.to;
  const failed = sheet.notices.filter((n) => n.status === "failed");
  const waDigits = (tk.phone ?? "").replace(/\D/g, "");
  const primaryStep = sheet.next.find((s) => s !== "cancelled");
  const ringUp = async () => {
    if (!tk.order_id) return;
    setBusy(true);
    setError(null);
    try {
      if (tk.status === "draft") {
        const id = tk.order_id;
        // Short stock or a changed total is explained, not just refused.
        if ((await warn.confirm((ack) => api.orders.confirm(id, ack))) === undefined) return;
      }
      const c = await api.orders.convert(tk.order_id, ringOp);
      onRungUp?.(c);
    } catch (e) {
      handle(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title={
        <span className="row gap-8">
          <span className="num">{tk.number}</span>
          <StatusChip status={tk.status} />
        </span>
      }
      size="sheet"
      testId="ticket-sheet"
      onClose={busy ? undefined : onClose}
      footer={
        <div className="ticket-foot">
          {sheet.can.ring_up && onRungUp ? (
            <Button variant="primary" size="xl" block onClick={ringUp} loading={busy} data-testid="ticket-ring-up">
              {t("Ring up")}
            </Button>
          ) : (
            <div className="ticket-steps">
              {sheet.next.map((s) => (
                <Button
                  key={s}
                  variant={s === primaryStep ? "primary" : s === "cancelled" ? "danger-outline" : "default"}
                  size="lg"
                  onClick={() => step(s)}
                  disabled={busy}
                  data-testid={`ticket-step-${s}`}
                >
                  {s === "delivered" ? <Check size={20} aria-hidden /> : null}
                  {stepLabel(s)}
                </Button>
              ))}
            </div>
          )}
        </div>
      }
    >
      <div className="ticket-body">
        <section className="ticket-who">
          <div className="grow">
            <div className="strong" dir="auto">
              {tk.customer_name || t("No customer")}
            </div>
            <div className="tiny muted">
              {[channelLabel(tk.channel), tk.delivery_number, relative(tk.created_at)].filter(Boolean).join(" · ")}
            </div>
          </div>
          <div className="ticket-pay">
            <span className="money strong">{formatMoney(tk.amount_minor)}</span>
            <PayChip state={tk.pay_state} />
          </div>
        </section>
        <section className="ticket-where">
          <MapPin size={20} aria-hidden />
          <div className="grow">
            <div dir="auto">{tk.address || t("No address")}</div>
            <div className="tiny muted">{[tk.area, tk.phone].filter(Boolean).join(" · ")}</div>
          </div>
          {waDigits ? (
            <a
              className="btn"
              href={`https://wa.me/${waDigits}`}
              target="_blank"
              rel="noreferrer"
              aria-label={t("Open WhatsApp chat")}
            >
              <MessageCircle size={18} aria-hidden /> {t("WhatsApp")}
            </a>
          ) : null}
        </section>
        {failed.length ? (
          <Banner tone="warning" title={t("WhatsApp message not sent")}>
            {failed[0].error || t("It will not be retried by itself. Send it again from Message.")}
          </Banner>
        ) : null}
        {error ? <Banner tone="danger">{error}</Banner> : null}
        {tk.failed_note ? (
          <Banner
            tone={tk.outcome ? "info" : "warning"}
            title={tk.outcome ? t("Not delivered") : t("Could not deliver")}
          >
            <span dir="auto">{tk.failed_note}</span>
          </Banner>
        ) : null}
        {sheet.can.unable || sheet.can.not_delivered ? (
          <section className="row gap-8 wrap">
            {sheet.can.unable ? (
              <Button variant="default" onClick={() => setUnable(true)} data-testid="ticket-unable">
                {t("Unable to deliver")}
              </Button>
            ) : null}
            {sheet.can.not_delivered ? (
              <Button variant="danger-outline" onClick={() => setClosing(true)} data-testid="ticket-not-delivered">
                {t("Close as not delivered")}
              </Button>
            ) : null}
          </section>
        ) : null}
        {tk.cash_with ? (
          <Banner tone="info" title={t("Cash with {0}", tk.cash_with)}>
            {t("Collected at the door. It enters a drawer when the rider hands it over at the till.")}
          </Banner>
        ) : null}
        {!settled && tk.kind === "drop" && tk.status !== "cancelled" ? (
          <section className="ticket-money">
            <div className="grow">
              {tk.outstanding_minor > 0
                ? t("To collect {0}", formatMoney(tk.outstanding_minor))
                : t("Waiting for the payment to be checked.")}
            </div>
            {sheet.can.record_payment ? (
              <Button onClick={() => setPaying(true)} data-testid="ticket-record-payment">
                {t("Record payment")}
              </Button>
            ) : null}
            {sheet.can.attach_screenshot && shotsOn && did ? (
              <label className="btn file-btn">
                <Paperclip size={18} aria-hidden /> {t("Attach screenshot")}
                <input
                  type="file"
                  accept="image/*"
                  hidden
                  onChange={(e) => {
                    const f = e.target.files?.[0];
                    if (!f) return;
                    void run(async () =>
                      api.payreviews.upload({
                        file_name: f.name,
                        data: await fileToBase64(f),
                        expected_minor: tk.outstanding_minor || tk.amount_minor,
                        delivery_id: did,
                      }),
                    );
                  }}
                />
              </label>
            ) : null}
          </section>
        ) : null}
        {sheet.can.assign && sheet.riders.length ? (
          <section>
            <div className="label">{t("Rider")}</div>
            <div className="area-chips">
              {sheet.riders.map((r) => (
                <button
                  key={r.user_id}
                  type="button"
                  className={`filter-chip ${tk.assigned_user_id === r.user_id ? "active" : ""}`}
                  onClick={() =>
                    did &&
                    void run(() =>
                      api.deliveries.update({
                        delivery_id: did,
                        assigned_user_id: tk.assigned_user_id === r.user_id ? "" : r.user_id,
                      }),
                    )
                  }
                >
                  {r.name}
                </button>
              ))}
            </div>
          </section>
        ) : tk.assigned_name ? (
          <div className="small muted">{t("Rider: {0}", tk.assigned_name)}</div>
        ) : null}
        {sheet.can.message && did ? (
          <section className="row gap-8 wrap">
            <WhatsAppSendButton kind="received" deliveryId={did} customerId={tk.customer_id} phone={tk.phone} />
            <WhatsAppSendButton kind="dispatch" deliveryId={did} customerId={tk.customer_id} phone={tk.phone} />
            <WhatsAppSendButton kind="delivered" deliveryId={did} customerId={tk.customer_id} phone={tk.phone} />
          </section>
        ) : null}
        <section className="ticket-lines">
          {sheet.lines.map((l, i) => (
            <div key={i} className="row small">
              <span className="grow ellipsis" dir="auto">
                {l.name}
              </span>
              <span className="num muted">{formatQty(l.qty_milli)}×</span>
              <span className="money">{formatMoney(l.line_total_minor)}</span>
            </div>
          ))}
          {sheet.payments.map((p, i) => (
            <div key={`p${i}`} className="row tiny muted">
              <span className="grow">{methodLabel(p.method)}</span>
              <span className="money">{formatMoney(p.amount_minor)}</span>
            </div>
          ))}
          {sheet.collections.map((p, i) => (
            <div key={`k${i}`} className="row tiny muted">
              <span className="grow">
                {methodLabel(p.method)}
                {p.held_by ? ` · ${p.handed_over ? t("handed over by {0}", p.held_by) : t("with {0}", p.held_by)}` : ""}
              </span>
              <span className="money">{formatMoney(p.amount_minor)}</span>
            </div>
          ))}
        </section>
        {canUndo && did && lastEvent?.from ? (
          <div>
            <Button
              size="sm"
              variant="ghost"
              icon={<Undo2 size={16} />}
              onClick={() => void run(() => api.deliveries.revert(did, lastEvent.from!))}
            >
              {t("Undo: back to {0}", statusLabel(lastEvent.from))}
            </Button>
          </div>
        ) : null}
      </div>
      {paying && did ? (
        <RecordPaymentSheet
          ticket={tk}
          methods={(config?.payments ?? []).map((p) => p.method).filter((m) => m !== "account")}
          onClose={() => (setPaying(false), setThenDeliver(false))}
          onDone={() => {
            setPaying(false);
            const deliver = thenDeliver;
            setThenDeliver(false);
            void run(async () => {
              if (deliver) await api.deliveries.update({ delivery_id: did, status: "delivered" });
            });
          }}
        />
      ) : null}
      {unable && did ? (
        <UnableSheet
          onClose={() => setUnable(false)}
          onSave={(reason) => {
            setUnable(false);
            void run(() => api.tickets.unable(did, reason));
          }}
        />
      ) : null}
      {closing && did ? (
        <NotDeliveredSheet
          ticket={tk}
          paidMinor={Math.max(0, tk.amount_minor - tk.outstanding_minor)}
          methods={(config?.payments ?? []).map((p) => p.method).filter((m) => m !== "account")}
          onClose={() => setClosing(false)}
          onDone={() => {
            setClosing(false);
            void run(async () => undefined);
          }}
        />
      ) : null}
      {warn.dialog}
      {deliverAsk && did ? (
        <Modal
          title={t("Paid?")}
          size="sm"
          onClose={() => setDeliverAsk(false)}
          footer={
            <>
              <Button
                size="lg"
                onClick={() => {
                  setDeliverAsk(false);
                  void run(() => api.deliveries.update({ delivery_id: did, status: "delivered" }));
                }}
                data-testid="deliver-unpaid"
              >
                {t("Still unpaid")}
              </Button>
              {sheet.can.record_payment ? (
                <Button
                  variant="primary"
                  size="lg"
                  onClick={() => (setDeliverAsk(false), setThenDeliver(true), setPaying(true))}
                  data-testid="deliver-take-payment"
                >
                  {t("Take payment")}
                </Button>
              ) : null}
            </>
          }
        >
          <p>
            {t(
              "{0} is still to collect. Take the payment now, or mark it delivered and collect later.",
              formatMoney(tk.outstanding_minor),
            )}
          </p>
        </Modal>
      ) : null}
    </Modal>
  );
}

/** Money for a pay-on-delivery ticket: method chips, amount, one Confirm. */
function RecordPaymentSheet({
  ticket,
  methods,
  onClose,
  onDone,
}: {
  ticket: TicketRow;
  methods: string[];
  onClose: () => void;
  onDone: () => void;
}) {
  const { session, has } = useSession();
  const atDoor = !has("pos.sell") && !!session && ticket.assigned_user_id === session.user_id;
  const [method, setMethod] = useState(methods.includes("cash") ? "cash" : (methods[0] ?? "cash"));
  const [amount, setAmount] = useState(formatAmount(ticket.outstanding_minor));
  const [reference, setReference] = useState("");
  const [opId] = useState(newOperationId);
  const [busy, setBusy] = useState(false);
  const { error, handle, setError } = useErr();
  const minor = parseMoney(amount);
  const save = async () => {
    if (!ticket.delivery_id || minor === null) return;
    setBusy(true);
    setError(null);
    try {
      await api.tickets.recordPayment({
        delivery_id: ticket.delivery_id,
        method,
        amount_minor: minor,
        reference: reference || null,
        operation_id: opId,
      });
      onDone();
    } catch (e) {
      handle(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title={t("Record payment")}
      size="md"
      onClose={busy ? undefined : onClose}
      footer={
        <Button
          variant="pay"
          size="xl"
          block
          onClick={save}
          loading={busy}
          disabled={minor === null || minor <= 0}
          data-testid="record-payment-confirm"
        >
          {t("Confirm")} <span className="money">{minor !== null ? formatMoney(minor) : ""}</span>
        </Button>
      }
    >
      <div className="col gap-12">
        <div className="area-chips" role="radiogroup" aria-label={t("Payment method")}>
          {methods.map((m) => (
            <button
              key={m}
              type="button"
              role="radio"
              aria-checked={m === method}
              className={`filter-chip ${m === method ? "active" : ""}`}
              onClick={() => setMethod(m)}
            >
              {methodLabel(m)}
            </button>
          ))}
        </div>
        <div className="field">
          <label htmlFor="rp-amount">{t("Amount")}</label>
          <input
            id="rp-amount"
            className="input lg num"
            inputMode="decimal"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
          />
        </div>
        {method !== "cash" ? (
          <div className="field">
            <label htmlFor="rp-ref">{t("Reference")}</label>
            <input id="rp-ref" className="input" value={reference} onChange={(e) => setReference(e.target.value)} />
          </div>
        ) : null}
        {method === "cash" ? (
          <div className="hint">
            {atDoor
              ? t("The cash stays with you until you hand it over at the till.")
              : t("Cash goes into this shift's drawer.")}
          </div>
        ) : null}
        {error ? <Banner tone="danger">{error}</Banner> : null}
      </div>
    </Modal>
  );
}

/** A rider hands their cash to the cashier: tick what they collected without
 * recording it, count the notes, and the counted amount enters this drawer. */
function RiderHandoverSheet({
  initialRider,
  onClose,
  onDone,
}: {
  initialRider?: string | null;
  onClose: () => void;
  onDone: () => void;
}) {
  const [riders, setRiders] = useState<RiderCash[] | null>(null);
  const [pick, setPick] = useState<string | null>(null);
  const [ticked, setTicked] = useState<Set<string>>(new Set());
  const [counted, setCounted] = useState("");
  const [note, setNote] = useState("");
  const [opId] = useState(newOperationId);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<{ number: string; variance: number } | null>(null);
  const { error, handle, setError } = useErr();
  useEffect(() => {
    api.riders
      .cash()
      .then((r) => {
        setRiders(r);
        if (initialRider && r.some((x) => x.rider_user_id === initialRider)) setPick(initialRider);
        else if (r.length === 1) setPick(r[0].rider_user_id);
      })
      .catch(handle);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const rider = riders?.find((r) => r.rider_user_id === pick) ?? null;
  const expected =
    (rider?.held_minor ?? 0) +
    (rider?.uncollected.filter((u) => ticked.has(u.delivery_id)).reduce((a, u) => a + u.outstanding_minor, 0) ?? 0);
  const countedMinor = parseMoney(counted);
  const variance = countedMinor === null ? 0 : countedMinor - expected;
  const save = async () => {
    if (!rider || countedMinor === null) return;
    setBusy(true);
    setError(null);
    try {
      const h = await api.riders.handover({
        rider_user_id: rider.rider_user_id,
        collect: [...ticked],
        counted_minor: countedMinor,
        note: note.trim() || null,
        operation_id: opId,
      });
      setDone({ number: h.handover_number, variance: h.variance_minor });
    } catch (e) {
      handle(e);
    } finally {
      setBusy(false);
    }
  };
  if (done) {
    return (
      <Modal
        title={t("Handed over")}
        size="sm"
        onClose={onDone}
        footer={
          <Button variant="primary" size="lg" block onClick={onDone}>
            {t("Done")}
          </Button>
        }
      >
        <p data-testid="handover-done">
          {done.variance === 0
            ? t("{0}: the count matches.", done.number)
            : t("{0}: {1} difference recorded.", done.number, formatMoney(done.variance))}
        </p>
      </Modal>
    );
  }
  return (
    <Modal
      title={t("Rider hand-over")}
      size="md"
      testId="rider-handover"
      onClose={busy ? undefined : onClose}
      footer={
        rider ? (
          <Button
            variant="pay"
            size="xl"
            block
            onClick={save}
            loading={busy}
            disabled={countedMinor === null || expected === 0 || (variance !== 0 && !note.trim())}
            data-testid="handover-confirm"
          >
            {t("Count in")} <span className="money">{countedMinor !== null ? formatMoney(countedMinor) : ""}</span>
          </Button>
        ) : null
      }
    >
      <div className="col gap-12">
        {riders && riders.length === 0 ? <div className="rail-empty">{t("No rider is holding cash.")}</div> : null}
        {riders && riders.length > 1 ? (
          <div className="area-chips" role="radiogroup" aria-label={t("Rider")}>
            {riders.map((r) => (
              <button
                key={r.rider_user_id}
                type="button"
                role="radio"
                aria-checked={r.rider_user_id === pick}
                className={`filter-chip ${r.rider_user_id === pick ? "active" : ""}`}
                onClick={() => (setPick(r.rider_user_id), setTicked(new Set()))}
              >
                {r.name} · <span className="money">{formatMoney(r.held_minor + r.uncollected_minor)}</span>
              </button>
            ))}
          </div>
        ) : null}
        {rider ? (
          <>
            {rider.held.length ? (
              <section className="col gap-4">
                <div className="label">{t("Collected at the door")}</div>
                {rider.held.map((h) => (
                  <div key={h.collection_id} className="row small">
                    <Check size={16} aria-hidden />
                    <span className="grow ellipsis" dir="auto">
                      {[h.number, h.customer_name, h.area].filter(Boolean).join(" · ")}
                    </span>
                    <span className="money">{formatMoney(h.amount_minor)}</span>
                  </div>
                ))}
              </section>
            ) : null}
            {rider.uncollected.length ? (
              <section className="col gap-4">
                <div className="label">{t("Not recorded yet: tick what the rider collected in cash")}</div>
                {rider.uncollected.map((u) => (
                  <Checkbox
                    key={u.delivery_id}
                    checked={ticked.has(u.delivery_id)}
                    onChange={(v) =>
                      setTicked((s) => {
                        const n = new Set(s);
                        if (v) n.add(u.delivery_id);
                        else n.delete(u.delivery_id);
                        return n;
                      })
                    }
                    label={
                      <span className="row gap-8">
                        <span className="grow ellipsis" dir="auto">
                          {[u.number, u.customer_name, u.area].filter(Boolean).join(" · ")}
                        </span>
                        <span className="money">{formatMoney(u.outstanding_minor)}</span>
                      </span>
                    }
                  />
                ))}
              </section>
            ) : null}
            <div className="row strong">
              <span className="grow">{t("Should hand over")}</span>
              <span className="money" data-testid="handover-expected">
                {formatMoney(expected)}
              </span>
            </div>
            <div className="field">
              <label htmlFor="ho-count">{t("Counted")}</label>
              <input
                id="ho-count"
                className="input lg num"
                inputMode="decimal"
                value={counted}
                onChange={(e) => setCounted(e.target.value)}
                data-testid="handover-counted"
              />
            </div>
            {countedMinor !== null && variance !== 0 ? (
              <>
                <Banner tone="warning">
                  {variance < 0 ? t("{0} short.", formatMoney(-variance)) : t("{0} over.", formatMoney(variance))}
                </Banner>
                <div className="field">
                  <label htmlFor="ho-note">{t("Why is it different?")}</label>
                  <input id="ho-note" className="input" value={note} onChange={(e) => setNote(e.target.value)} />
                </div>
              </>
            ) : null}
          </>
        ) : null}
        {error ? <Banner tone="danger">{error}</Banner> : null}
      </div>
    </Modal>
  );
}

const UNABLE_REASONS = [
  () => t("Customer not home"),
  () => t("Wrong address"),
  () => t("Customer refused"),
  () => t("Phone not answered"),
];

/** The rider (or the till) says they could not deliver; the drop stays open. */
function UnableSheet({ onClose, onSave }: { onClose: () => void; onSave: (reason: string) => void }) {
  const [reason, setReason] = useState("");
  return (
    <Modal
      title={t("Unable to deliver")}
      size="md"
      testId="unable-sheet"
      onClose={onClose}
      footer={
        <Button
          variant="primary"
          size="lg"
          block
          disabled={!reason.trim()}
          onClick={() => onSave(reason.trim())}
          data-testid="unable-confirm"
        >
          {t("Flag it")}
        </Button>
      }
    >
      <div className="col gap-12">
        <div className="area-chips">
          {UNABLE_REASONS.map((r) => (
            <button
              key={r()}
              type="button"
              className={`filter-chip ${reason === r() ? "active" : ""}`}
              onClick={() => setReason(r())}
            >
              {r()}
            </button>
          ))}
        </div>
        <div className="field">
          <label htmlFor="un-reason">{t("Reason")}</label>
          <input id="un-reason" className="input" value={reason} onChange={(e) => setReason(e.target.value)} />
        </div>
        <div className="hint">
          {t("The drop stays open. A manager closes it as not delivered, or it is delivered later.")}
        </div>
      </div>
    </Modal>
  );
}

/** Close a drop as not delivered: goods back or damaged, money back, sale refunded. */
function NotDeliveredSheet({
  ticket,
  paidMinor,
  methods,
  onClose,
  onDone,
}: {
  ticket: TicketRow;
  paidMinor: number;
  methods: string[];
  onClose: () => void;
  onDone: () => void;
}) {
  const approve = useApproval();
  const [reason, setReason] = useState(ticket.failed_note ?? "");
  const [restock, setRestock] = useState(true);
  const [method, setMethod] = useState("cash");
  const [opId] = useState(newOperationId);
  const [busy, setBusy] = useState(false);
  const { error, handle, setError } = useErr();
  const save = async () => {
    if (!ticket.delivery_id) return;
    setBusy(true);
    setError(null);
    try {
      await approve((tok) =>
        api.tickets.notDelivered({
          delivery_id: ticket.delivery_id!,
          reason: reason.trim(),
          restock,
          refund_method: paidMinor > 0 ? method : null,
          operation_id: opId,
          approval_token: tok,
        }),
      );
      onDone();
    } catch (e) {
      if (!(e instanceof ApprovalCancelled)) handle(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title={t("Close as not delivered")}
      size="md"
      testId="not-delivered-sheet"
      onClose={busy ? undefined : onClose}
      footer={
        <Button
          variant="danger"
          size="lg"
          block
          loading={busy}
          disabled={!reason.trim()}
          onClick={save}
          data-testid="not-delivered-confirm"
        >
          {t("Close and refund")}
        </Button>
      }
    >
      <div className="col gap-12">
        <div className="field">
          <label htmlFor="nd-reason">{t("Reason")}</label>
          <input id="nd-reason" className="input" value={reason} onChange={(e) => setReason(e.target.value)} />
        </div>
        <div className="label">{t("The goods")}</div>
        <div className="area-chips" role="radiogroup" aria-label={t("The goods")}>
          <button
            type="button"
            role="radio"
            aria-checked={restock}
            className={`filter-chip ${restock ? "active" : ""}`}
            onClick={() => setRestock(true)}
          >
            {t("Back on the shelf")}
          </button>
          <button
            type="button"
            role="radio"
            aria-checked={!restock}
            className={`filter-chip ${!restock ? "active" : ""}`}
            onClick={() => setRestock(false)}
            data-testid="nd-damaged"
          >
            {t("Damaged")}
          </button>
        </div>
        {paidMinor > 0 ? (
          <>
            <div className="label">{t("Give back {0} by", formatMoney(paidMinor))}</div>
            <div className="area-chips" role="radiogroup">
              {methods.map((m) => (
                <button
                  key={m}
                  type="button"
                  role="radio"
                  aria-checked={m === method}
                  className={`filter-chip ${m === method ? "active" : ""}`}
                  onClick={() => setMethod(m)}
                >
                  {methodLabel(m)}
                </button>
              ))}
            </div>
          </>
        ) : null}
        {ticket.outstanding_minor > 0 ? (
          <div className="hint">
            {t("{0} was never collected; it is cancelled, not paid out.", formatMoney(ticket.outstanding_minor))}
          </div>
        ) : null}
        <div className="hint">
          {t("The sale is refunded against its receipt and the drop is closed. This cannot be undone.")}
        </div>
        {error ? <Banner tone="danger">{error}</Banner> : null}
      </div>
    </Modal>
  );
}
