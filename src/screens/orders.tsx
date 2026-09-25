// Digital orders (phone, WhatsApp, web, other). Shared by the admin page,
// the delivery desk and the till. An order never becomes a sale by itself:
// a person confirms it, then a cashier loads it into a sale and takes payment.
import { useEffect, useState } from "react";
import { Plus, ShoppingCart, Trash2 } from "lucide-react";
import { api } from "../api";
import type { Cart, CustomerRow, DigitalOrder, OrderChannel, OrderInput, OrderPaymentState } from "../api/types";
import { useSession } from "../state/session";
import { explain } from "../lib/errors";
import { newOperationId } from "../lib/ids";
import { formatMoney, formatQty } from "../lib/money";
import { formatShort } from "../lib/time";
import { Banner, Button, Checkbox, Chip, Empty, Field, Modal, TextInput } from "../components/ui";
import { t } from "../i18n";
import { codeLabel } from "../i18n/codes";

const CHANNELS: OrderChannel[] = ["phone", "whatsapp", "web", "other"];
const PAY: OrderPaymentState[] = ["unpaid", "recorded", "screenshot_pending"];

interface LineDraft {
  product_id: string | null;
  name: string;
  description: string;
  qty: string;
}

function toDraft(o: DigitalOrder | null): LineDraft[] {
  return (o?.lines ?? []).map((l) => ({
    product_id: l.product_id,
    name: l.product_name ?? "",
    description: l.description,
    qty: formatQty(l.qty_milli),
  }));
}

function statusTone(s: DigitalOrder["status"]): "default" | "brand" | "success" | "danger" {
  return s === "confirmed" ? "brand" : s === "converted" ? "success" : s === "cancelled" ? "danger" : "default";
}

/** Create or edit a draft order. */
export function OrderEditor({
  order,
  onClose,
  onSaved,
}: {
  order: DigitalOrder | null;
  onClose: () => void;
  onSaved: (o: DigitalOrder) => void;
}) {
  const [channel, setChannel] = useState<OrderChannel>(order?.channel ?? "phone");
  const [ref, setRef] = useState(order?.external_ref ?? "");
  const [customer, setCustomer] = useState<{ id: string; name: string } | null>(
    order?.customer_id ? { id: order.customer_id, name: order.customer_name ?? "" } : null,
  );
  const [phone, setPhone] = useState(order?.phone ?? "");
  const [address, setAddress] = useState(order?.address ?? "");
  const [delivery, setDelivery] = useState(order?.delivery_wanted ?? false);
  const [pay, setPay] = useState<OrderPaymentState>(order?.payment_state ?? "unpaid");
  const [note, setNote] = useState(order?.note ?? "");
  const [lines, setLines] = useState<LineDraft[]>(toDraft(order));
  const [cq, setCq] = useState("");
  const [custRows, setCustRows] = useState<CustomerRow[]>([]);
  const [pq, setPq] = useState("");
  const [prodRows, setProdRows] = useState<
    { product_id: string; name: string; sku: string; price_minor: number | null }[]
  >([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (cq.trim().length < 2) return setCustRows([]);
    const h = setTimeout(() => {
      api.customers
        .search(cq, false, 8)
        .then(setCustRows)
        .catch(() => setCustRows([]));
    }, 200);
    return () => clearTimeout(h);
  }, [cq]);
  useEffect(() => {
    if (pq.trim().length < 2) return setProdRows([]);
    const h = setTimeout(() => {
      api.orders
        .products(pq)
        .then(setProdRows)
        .catch(() => setProdRows([]));
    }, 200);
    return () => clearTimeout(h);
  }, [pq]);
  const save = async () => {
    setError(null);
    const out: OrderInput["lines"] = [];
    for (const l of lines) {
      const q = Number(l.qty.replace(",", "."));
      if (!Number.isFinite(q) || q <= 0) {
        setError(t("Quantities must be more than zero."));
        return;
      }
      out.push({ product_id: l.product_id, description: l.description || l.name, qty_milli: Math.round(q * 1000) });
    }
    setBusy(true);
    try {
      const o = await api.orders.save(order?.order_id ?? null, {
        channel,
        external_ref: ref || null,
        customer_id: customer?.id ?? null,
        phone: phone || null,
        payment_state: pay,
        note: note || null,
        address: address || null,
        delivery_wanted: delivery,
        lines: out,
      });
      onSaved(o);
    } catch (e) {
      setError(explain(e).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title={order ? t("Order {0}", order.order_number) : t("New order")}
      size="lg"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button variant="primary" className="right" loading={busy} onClick={save}>
            {t("Save draft")}
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        {order?.inbox_seq ? (
          <Banner tone="info" title={t("Suggested from a WhatsApp message")}>
            {t("Check every line against the message before confirming. Unmatched text stays as a note on the line.")}
          </Banner>
        ) : null}
        <div className="grid-2">
          <Field label={t("Channel")}>
            <select className="select" value={channel} onChange={(e) => setChannel(e.target.value as OrderChannel)}>
              {CHANNELS.map((c) => (
                <option key={c} value={c}>
                  {codeLabel(c)}
                </option>
              ))}
            </select>
          </Field>
          <TextInput
            label={t("External reference")}
            value={ref}
            onChange={(e) => setRef(e.target.value)}
            hint={t("Order number from the website or app, if any.")}
          />
        </div>
        <div className="grid-2">
          <div className="col gap-8">
            <Field label={t("Customer")}>
              {customer ? (
                <div className="row">
                  <strong className="grow">{customer.name}</strong>
                  <Button size="sm" onClick={() => setCustomer(null)}>
                    {t("Remove")}
                  </Button>
                </div>
              ) : (
                <input
                  className="input"
                  placeholder={t("Search by phone or name…")}
                  value={cq}
                  onChange={(e) => setCq(e.target.value)}
                />
              )}
            </Field>
            {!customer && custRows.length ? (
              <div className="card" style={{ maxHeight: 160, overflow: "auto" }}>
                {custRows.map((c) => (
                  <div
                    key={c.customer_id}
                    className="result-row"
                    onClick={() => {
                      setCustomer({ id: c.customer_id, name: c.name });
                      if (!phone && c.phone) setPhone(c.phone);
                      if (!address && c.address) setAddress(c.address);
                      setCq("");
                    }}
                  >
                    <span className="grow">{c.name}</span>
                    <span className="tiny">{c.phone ?? ""}</span>
                  </div>
                ))}
              </div>
            ) : null}
          </div>
          <TextInput label={t("Phone")} value={phone} inputMode="tel" onChange={(e) => setPhone(e.target.value)} />
        </div>
        <div className="grid-2">
          <Field label={t("Payment")}>
            <select className="select" value={pay} onChange={(e) => setPay(e.target.value as OrderPaymentState)}>
              {PAY.map((p) => (
                <option key={p} value={p}>
                  {codeLabel(p)}
                </option>
              ))}
            </select>
          </Field>
          <div className="col gap-8">
            <Checkbox label={t("Deliver this order")} checked={delivery} onChange={setDelivery} />
            {delivery ? (
              <TextInput label={t("Delivery address")} value={address} onChange={(e) => setAddress(e.target.value)} />
            ) : null}
          </div>
        </div>
        <div className="col gap-8">
          <h3>{t("Items")}</h3>
          {lines.length === 0 ? <div className="muted small">{t("No items yet.")}</div> : null}
          {lines.map((l, i) => (
            <div key={i} className="row" data-testid="order-line">
              <div className="grow">
                {l.product_id ? (
                  <strong>{l.name || l.description}</strong>
                ) : (
                  <Chip tone="warning">{t("Not matched: {0}", l.description)}</Chip>
                )}
                {l.product_id && l.description && l.description !== l.name ? (
                  <div className="tiny">{l.description}</div>
                ) : null}
              </div>
              <input
                className="input num"
                style={{ width: 90 }}
                aria-label={t("Quantity")}
                value={l.qty}
                onChange={(e) => setLines(lines.map((x, j) => (j === i ? { ...x, qty: e.target.value } : x)))}
              />
              <Button
                size="sm"
                variant="ghost"
                aria-label={t("Remove")}
                icon={<Trash2 size={14} />}
                onClick={() => setLines(lines.filter((_, j) => j !== i))}
              />
            </div>
          ))}
          <input
            className="input"
            placeholder={t("Add a product: type a name, SKU or barcode…")}
            value={pq}
            onChange={(e) => setPq(e.target.value)}
          />
          {prodRows.length ? (
            <div className="card" style={{ maxHeight: 200, overflow: "auto" }}>
              {prodRows.map((p) => (
                <div
                  key={p.product_id}
                  className="result-row"
                  onClick={() => {
                    // Matching an unmatched line keeps its text; otherwise add a new line.
                    const idx = lines.findIndex((x) => !x.product_id);
                    if (idx >= 0)
                      setLines(lines.map((x, j) => (j === idx ? { ...x, product_id: p.product_id, name: p.name } : x)));
                    else
                      setLines([...lines, { product_id: p.product_id, name: p.name, description: p.name, qty: "1" }]);
                    setPq("");
                  }}
                >
                  <span className="grow">{p.name}</span>
                  <span className="tiny">{p.sku}</span>
                  <span className="num">{p.price_minor === null ? "—" : formatMoney(p.price_minor)}</span>
                </div>
              ))}
            </div>
          ) : null}
        </div>
        <Field label={t("Note")}>
          <textarea className="textarea" value={note} onChange={(e) => setNote(e.target.value)} />
        </Field>
        {error ? <Banner tone="danger">{error}</Banner> : null}
      </div>
    </Modal>
  );
}

/** Order list with the actions the user may take. `onConverted` enables "Sell on this till". */
export function OrdersList({ onConverted }: { onConverted?: (c: Cart) => void }) {
  const { has } = useSession();
  const [status, setStatus] = useState<string>("");
  const [rows, setRows] = useState<DigitalOrder[] | null>(null);
  const [editing, setEditing] = useState<DigitalOrder | null | "new">(null);
  const [error, setError] = useState<string | null>(null);
  const [ops] = useState(() => new Map<string, string>());
  const manage = has("orders.manage");
  const load = () =>
    api.orders
      .list(status || null)
      .then((r) => (setRows(r), setError(null)))
      .catch((e) => setError(explain(e).message));
  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status]);
  const act = async (f: () => Promise<unknown>) => {
    setError(null);
    try {
      await f();
      await load();
    } catch (e) {
      setError(explain(e).message);
    }
  };
  const convert = async (o: DigitalOrder) => {
    // One operation id per order on this screen: a double click or retry replays.
    const op = ops.get(o.order_id) ?? newOperationId();
    ops.set(o.order_id, op);
    setError(null);
    try {
      onConverted?.(await api.orders.convert(o.order_id, op));
    } catch (e) {
      setError(explain(e).message);
    }
  };
  return (
    <div className="col gap-16">
      <div className="row wrap">
        {[
          ["", t("Open")],
          ["converted", codeLabel("converted")],
          ["cancelled", codeLabel("cancelled")],
        ].map(([k, l]) => (
          <button key={k} className={`filter-chip ${status === k ? "active" : ""}`} onClick={() => setStatus(k)}>
            {l}
          </button>
        ))}
        <span className="grow" />
        {manage ? (
          <Button variant="primary" icon={<Plus size={16} />} onClick={() => setEditing("new")}>
            {t("New order")}
          </Button>
        ) : null}
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {rows && rows.length === 0 ? (
        <Empty title={t("No orders")}>
          {t("Orders taken by phone, WhatsApp or the website appear here until they are sold.")}
        </Empty>
      ) : null}
      {(rows ?? []).map((o) => (
        <div key={o.order_id} className="card card-pad col gap-8" data-testid="order-card">
          <div className="row wrap">
            <strong>{o.order_number}</strong>
            <Chip tone={statusTone(o.status)}>{codeLabel(o.status)}</Chip>
            <Chip>{codeLabel(o.channel)}</Chip>
            <Chip tone={o.payment_state === "unpaid" ? "warning" : "default"}>{codeLabel(o.payment_state)}</Chip>
            {o.delivery_wanted ? <Chip>{t("Delivery")}</Chip> : null}
            <span className="grow" />
            <span className="tiny">{formatShort(o.created_at)}</span>
          </div>
          <div className="small">
            {[o.customer_name, o.phone, o.external_ref ? `#${o.external_ref}` : null].filter(Boolean).join(" · ") ||
              t("No customer")}
          </div>
          <div className="small muted">
            {o.lines.map((l) => `${formatQty(l.qty_milli)} × ${l.product_name ?? l.description}`).join(" · ")}
          </div>
          <div className="row wrap">
            <span className="small">
              {t("Estimate")} <strong className="money">{formatMoney(o.estimate_minor)}</strong>
            </span>
            {o.receipt_number ? <span className="small">{t("Receipt {0}", o.receipt_number)}</span> : null}
            <span className="grow" />
            {manage && o.status === "draft" ? (
              <>
                <Button size="sm" onClick={() => setEditing(o)}>
                  {t("Edit")}
                </Button>
                <Button size="sm" variant="primary" onClick={() => act(() => api.orders.confirm(o.order_id))}>
                  {t("Confirm")}
                </Button>
              </>
            ) : null}
            {manage && (o.status === "draft" || o.status === "confirmed") ? (
              <>
                <select
                  className="select"
                  style={{ width: 170, height: 30 }}
                  aria-label={t("Payment")}
                  value={o.payment_state}
                  onChange={(e) => act(() => api.orders.setPayment(o.order_id, e.target.value as OrderPaymentState))}
                >
                  {PAY.map((p) => (
                    <option key={p} value={p}>
                      {codeLabel(p)}
                    </option>
                  ))}
                </select>
                <Button size="sm" variant="danger-outline" onClick={() => act(() => api.orders.cancel(o.order_id))}>
                  {t("Cancel order")}
                </Button>
              </>
            ) : null}
            {onConverted && o.status === "confirmed" ? (
              <Button size="sm" variant="primary" icon={<ShoppingCart size={14} />} onClick={() => void convert(o)}>
                {t("Sell on this till")}
              </Button>
            ) : null}
          </div>
        </div>
      ))}
      {editing ? (
        <OrderEditor
          order={editing === "new" ? null : editing}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            void load();
          }}
        />
      ) : null}
    </div>
  );
}
