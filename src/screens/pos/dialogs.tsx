import { useEffect, useState } from "react";
import { Lock, Plus, Printer, Search, UserPlus, X } from "lucide-react";
import { api } from "../../api";
import type { Cart, CartLine, CustomerRef, CustomerRow, HeldCart, PrintJobRow, SaleRow } from "../../api/types";
import { useApproval, ApprovalCancelled } from "../../components/approval";
import { useToast } from "../../components/toast";
import { explain } from "../../lib/errors";
import { newOperationId } from "../../lib/ids";
import {
  formatAmount,
  formatMoney,
  formatPercent,
  formatQty,
  parseMoney,
  parsePercent,
  parseQty,
} from "../../lib/money";
import { formatShort } from "../../lib/time";
import { Banner, Button, Chip, Keypad, Modal, TextInput } from "../../components/ui";
import { methodLabel } from "./labels";

function useErr() {
  const [error, setError] = useState<string | null>(null);
  const handle = (e: unknown) => {
    if (e instanceof ApprovalCancelled) return;
    const ex = explain(e);
    setError(`${ex.message} ${ex.action}`.trim());
  };
  return { error, setError, handle };
}

export function UnknownBarcodeDialog({
  barcode,
  canCustom,
  onClose,
  onSearch,
  onCustom,
}: {
  barcode: string;
  canCustom: boolean;
  onClose: () => void;
  onSearch: () => void;
  onCustom: () => void;
}) {
  return (
    <Modal
      title="Barcode not found"
      size="sm"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          {canCustom ? <Button onClick={onCustom}>Add custom item</Button> : null}
          <Button variant="primary" className="right" onClick={onSearch} autoFocus>
            Search product
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        <div>
          <div className="tiny">Barcode</div>
          <div className="mono" style={{ fontSize: 20, fontWeight: 600 }} data-testid="unknown-barcode">
            {barcode}
          </div>
        </div>
        <div className="small muted">
          This barcode was recorded for review by management. Search by name to sell the item now.
        </div>
      </div>
    </Modal>
  );
}

export function HoldDialog({ cart, onClose, onHeld }: { cart: Cart; onClose: () => void; onHeld: () => void }) {
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const { error, handle } = useErr();
  const hold = async () => {
    setBusy(true);
    try {
      await api.pos.hold(note || null);
      onHeld();
    } catch (e) {
      handle(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title="Hold current sale"
      size="sm"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" className="right" onClick={hold} loading={busy}>
            Hold Sale
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        <div className="banner">
          {cart.lines.length} lines · {formatQty(cart.totals.item_count_milli)} items ·{" "}
          <strong>{formatMoney(cart.totals.total_minor)}</strong>
        </div>
        <TextInput
          label="Note (customer / reason)"
          value={note}
          onChange={(e) => setNote(e.target.value)}
          autoFocus
          onKeyDown={(e) => e.key === "Enter" && hold()}
        />
        {error ? <Banner tone="danger">{error}</Banner> : null}
      </div>
    </Modal>
  );
}

export function HeldCartsDialog({
  onClose,
  onRestored,
  currentHasLines,
}: {
  onClose: () => void;
  onRestored: (c: Cart) => void;
  currentHasLines: boolean;
}) {
  const approve = useApproval();
  const [rows, setRows] = useState<HeldCart[] | null>(null);
  const { error, handle } = useErr();
  const load = () => api.pos.held().then(setRows).catch(handle);
  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return (
    <Modal title="Held sales" size="lg" onClose={onClose}>
      {currentHasLines ? <Banner tone="info">Hold or finish the current sale before resuming another.</Banner> : null}
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {rows && rows.length === 0 ? (
        <div className="empty">
          <h3>No held sales</h3>
        </div>
      ) : null}
      {rows && rows.length ? (
        <table className="table">
          <thead>
            <tr>
              <th>#</th>
              <th>Held</th>
              <th>Cashier</th>
              <th>Customer / note</th>
              <th className="num">Items</th>
              <th className="num">Total</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.cart_id}>
                <td>{r.hold_number}</td>
                <td>{formatShort(r.held_at)}</td>
                <td>{r.cashier_name}</td>
                <td>{[r.customer_name, r.note].filter(Boolean).join(" · ") || "—"}</td>
                <td className="num">{formatQty(r.item_count_milli)}</td>
                <td className="num">{formatMoney(r.total_minor)}</td>
                <td className="num">
                  <div className="row" style={{ justifyContent: "flex-end" }}>
                    {r.locked ? <Lock size={14} aria-label="Held by another cashier" /> : null}
                    <Button
                      size="sm"
                      variant="primary"
                      disabled={currentHasLines || r.locked}
                      onClick={async () => {
                        try {
                          onRestored(await api.pos.restore(r.cart_id));
                        } catch (e) {
                          handle(e);
                        }
                      }}
                    >
                      Resume
                    </Button>
                    <Button
                      size="sm"
                      variant="danger-outline"
                      onClick={async () => {
                        try {
                          await approve((tok) => api.pos.heldDelete(r.cart_id, tok));
                          void load();
                        } catch (e) {
                          handle(e);
                        }
                      }}
                    >
                      Delete
                    </Button>
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}
    </Modal>
  );
}

export function CustomerPicker({
  current,
  onClose,
  onPicked,
}: {
  current: CustomerRef | null;
  onClose: () => void;
  onPicked: (c: Cart) => void;
}) {
  const [q, setQ] = useState("");
  const [rows, setRows] = useState<CustomerRow[]>([]);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [phone, setPhone] = useState("");
  const [area, setArea] = useState("");
  const { error, handle, setError } = useErr();
  useEffect(() => {
    const t = setTimeout(() => {
      api.customers.search(q, false, 30).then(setRows).catch(handle);
    }, 150);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [q]);
  const pick = async (id: string | null) => {
    try {
      onPicked(await api.pos.setCustomer(id));
    } catch (e) {
      handle(e);
    }
  };
  const create = async () => {
    setError(null);
    try {
      const c = await api.customers.save(null, { name, phone: phone || null, area: area || null, active: true });
      await pick(c.customer_id);
    } catch (e) {
      handle(e);
    }
  };
  return (
    <Modal title="Customer" size="md" onClose={onClose}>
      {!creating ? (
        <div className="col gap-16">
          <div className="row">
            <div className="scan-box grow">
              <Search size={18} className="scan-icon" />
              <input
                className="input"
                placeholder="Search by phone or name…"
                value={q}
                onChange={(e) => setQ(e.target.value)}
                autoFocus
              />
            </div>
            <Button
              icon={<UserPlus size={16} />}
              onClick={() => (
                setCreating(true),
                setPhone(/^\+?\d[\d\s]*$/.test(q) ? q : ""),
                setName(/^\+?\d/.test(q) ? "" : q)
              )}
            >
              New Customer
            </Button>
          </div>
          {current ? (
            <div className="banner">
              <span className="grow">
                Attached: <strong>{current.name}</strong> {current.phone}
              </span>
              <Button size="sm" icon={<X size={14} />} onClick={() => pick(null)}>
                Remove
              </Button>
            </div>
          ) : null}
          <div style={{ maxHeight: 360, overflow: "auto" }}>
            {rows.map((c) => (
              <div key={c.customer_id} className="result-row" onClick={() => pick(c.customer_id)}>
                <div className="grow">
                  <div style={{ fontWeight: 600 }}>{c.name}</div>
                  <div className="tiny">
                    {c.phone ?? "No phone"} {c.area ? `· ${c.area}` : ""}
                  </div>
                </div>
                <span className="tiny">{c.purchase_count} purchases</span>
              </div>
            ))}
            {rows.length === 0 ? <div className="empty">No customers found.</div> : null}
          </div>
          {error ? <Banner tone="danger">{error}</Banner> : null}
        </div>
      ) : (
        <div className="col gap-16">
          <TextInput label="Name" required value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          <TextInput
            label="Phone"
            value={phone}
            onChange={(e) => setPhone(e.target.value)}
            inputMode="tel"
            hint="8-digit Bahrain numbers get +973 automatically."
          />
          <TextInput label="Area" value={area} onChange={(e) => setArea(e.target.value)} />
          {error ? <Banner tone="danger">{error}</Banner> : null}
          <div className="row">
            <Button onClick={() => setCreating(false)}>Back</Button>
            <Button variant="primary" className="right" onClick={create} disabled={!name.trim()}>
              Save & attach
            </Button>
          </div>
        </div>
      )}
    </Modal>
  );
}

export function QtyDialog({
  line,
  onClose,
  onApply,
}: {
  line: CartLine;
  onClose: () => void;
  onApply: (q: number) => void;
}) {
  const [v, setV] = useState(formatQty(line.qty_milli));
  const q = parseQty(v);
  const valid = q !== null && q > 0 && (line.allow_decimal_quantity || q % 1000 === 0);
  const key = (k: string) => setV((x) => (k === "Backspace" ? x.slice(0, -1) : x + k));
  return (
    <Modal
      title={`Quantity — ${line.name}`}
      size="sm"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" className="right" disabled={!valid} onClick={() => valid && onApply(q!)}>
            Apply
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        <input
          className="input lg num"
          inputMode="decimal"
          value={v}
          autoFocus
          onFocus={(e) => e.target.select()}
          onChange={(e) => setV(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && valid && onApply(q!)}
          aria-label="Quantity"
        />
        <div className="tiny">
          {line.allow_decimal_quantity ? `Up to 3 decimals (${line.unit}).` : "Whole units only."}
        </div>
        <Keypad onKey={key} extra={line.allow_decimal_quantity ? "." : ""} />
      </div>
    </Modal>
  );
}

export function DiscountDialog({
  line,
  cart,
  maxBp,
  onClose,
  onApply,
}: {
  line: CartLine | null;
  cart: Cart;
  maxBp: number;
  onClose: () => void;
  onApply: (minor: number, bp: number) => void;
}) {
  const [mode, setMode] = useState<"percent" | "amount">("percent");
  const [v, setV] = useState("");
  const base = line ? line.gross_minor : cart.totals.subtotal_minor;
  const parsed = mode === "percent" ? parsePercent(v) : parseMoney(v);
  const valid =
    v.trim() === "" || (parsed !== null && parsed >= 0 && (mode === "percent" ? parsed <= 10000 : parsed <= base));
  const apply = () => {
    if (!valid) return;
    if (v.trim() === "") return onApply(0, 0);
    onApply(mode === "amount" ? parsed! : 0, mode === "percent" ? parsed! : 0);
  };
  return (
    <Modal
      title={line ? `Discount — ${line.name}` : "Discount on sale"}
      size="sm"
      onClose={onClose}
      footer={
        <>
          <Button onClick={() => onApply(0, 0)}>Remove discount</Button>
          <Button variant="primary" className="right" disabled={!valid} onClick={apply}>
            Apply
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        <div className="row">
          <button className={`filter-chip ${mode === "percent" ? "active" : ""}`} onClick={() => setMode("percent")}>
            Percentage
          </button>
          <button className={`filter-chip ${mode === "amount" ? "active" : ""}`} onClick={() => setMode("amount")}>
            Amount
          </button>
        </div>
        <input
          className="input lg num"
          inputMode="decimal"
          value={v}
          onChange={(e) => setV(e.target.value)}
          autoFocus
          placeholder={mode === "percent" ? "10" : formatAmount(0)}
          onKeyDown={(e) => e.key === "Enter" && apply()}
          aria-label="Discount"
        />
        <div className="tiny">
          Applies to {formatMoney(base)}. Discounts above {formatPercent(maxBp)} need manager approval.
        </div>
        {!valid ? <Banner tone="danger">Enter a valid discount not larger than the amount.</Banner> : null}
      </div>
    </Modal>
  );
}

export function PriceDialog({
  line,
  onClose,
  onApply,
}: {
  line: CartLine;
  onClose: () => void;
  onApply: (price: number, reason: string | null) => void;
}) {
  const [v, setV] = useState(formatAmount(line.unit_price_minor));
  const [reason, setReason] = useState("");
  const p = parseMoney(v);
  const valid = p !== null && p >= 0;
  return (
    <Modal
      title={`Change price — ${line.name}`}
      size="sm"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" className="right" disabled={!valid} onClick={() => onApply(p!, reason || null)}>
            Apply
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        <div className="tiny">
          Catalogue price {formatMoney(line.catalog_unit_price_minor)}. Price overrides require permission and are
          audited.
        </div>
        <input
          className="input lg num"
          inputMode="decimal"
          value={v}
          onChange={(e) => setV(e.target.value)}
          autoFocus
          onFocus={(e) => e.target.select()}
          aria-label="New unit price"
        />
        <TextInput label="Reason" value={reason} onChange={(e) => setReason(e.target.value)} />
      </div>
    </Modal>
  );
}

export function CustomItemDialog({ onClose, onAdded }: { onClose: () => void; onAdded: (c: Cart) => void }) {
  const approve = useApproval();
  const [name, setName] = useState("");
  const [price, setPrice] = useState("");
  const [qty, setQty] = useState("1");
  const { error, handle } = useErr();
  const p = parseMoney(price);
  const q = parseQty(qty);
  const valid = name.trim() && p !== null && p > 0 && q !== null && q > 0;
  const add = async () => {
    if (!valid) return;
    try {
      onAdded(
        await approve((tok) => api.pos.addCustom({ name, unit_price_minor: p!, qty_milli: q!, approval_token: tok })),
      );
    } catch (e) {
      handle(e);
    }
  };
  return (
    <Modal
      title="Custom item"
      size="sm"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" className="right" disabled={!valid} onClick={add} icon={<Plus size={16} />}>
            Add to sale
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        <TextInput label="Description" required value={name} onChange={(e) => setName(e.target.value)} autoFocus />
        <div className="form-grid">
          <TextInput
            label="Unit price"
            required
            className="num"
            inputMode="decimal"
            value={price}
            onChange={(e) => setPrice(e.target.value)}
          />
          <TextInput
            label="Quantity"
            className="num"
            inputMode="decimal"
            value={qty}
            onChange={(e) => setQty(e.target.value)}
          />
        </div>
        <div className="tiny">Custom items are not tracked in inventory and are listed for management review.</div>
        {error ? <Banner tone="danger">{error}</Banner> : null}
      </div>
    </Modal>
  );
}

const CASH_TITLES = {
  paid_in: "Paid in",
  paid_out: "Paid out",
  safe_drop: "Safe drop",
  no_sale: "Open drawer (no sale)",
};

export function CashEventDialog({
  kind,
  onClose,
  onDone,
}: {
  kind: "paid_in" | "paid_out" | "safe_drop" | "no_sale";
  onClose: () => void;
  onDone: () => void;
}) {
  const approve = useApproval();
  const toast = useToast();
  const [amount, setAmount] = useState("");
  const [reason, setReason] = useState(kind === "no_sale" ? "" : "");
  const [busy, setBusy] = useState(false);
  const [opId] = useState(newOperationId);
  const { error, handle } = useErr();
  const minor = kind === "no_sale" ? 0 : parseMoney(amount);
  const valid = reason.trim() && (kind === "no_sale" || (minor !== null && minor > 0));
  const submit = async () => {
    if (!valid) return;
    setBusy(true);
    try {
      await approve((tok) =>
        api.cash.event({ kind, amount_minor: minor!, reason, operation_id: opId, approval_token: tok }),
      );
      toast("success", `${CASH_TITLES[kind]} recorded`);
      onDone();
    } catch (e) {
      handle(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title={CASH_TITLES[kind]}
      size="sm"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" className="right" disabled={!valid} loading={busy} onClick={submit}>
            Confirm
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        {kind !== "no_sale" ? (
          <TextInput
            label="Amount"
            required
            className="num"
            inputMode="decimal"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
            autoFocus
          />
        ) : null}
        <TextInput
          label="Reason"
          required
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          autoFocus={kind === "no_sale"}
          onKeyDown={(e) => e.key === "Enter" && submit()}
        />
        <div className="tiny">This cash event is recorded against your shift and cannot be edited later.</div>
        {error ? <Banner tone="danger">{error}</Banner> : null}
      </div>
    </Modal>
  );
}

export function RecentSalesDialog({ onClose }: { onClose: () => void }) {
  const toast = useToast();
  const [rows, setRows] = useState<SaleRow[]>([]);
  const [receipt, setReceipt] = useState("");
  const [preview, setPreview] = useState<{ id: string; text: string } | null>(null);
  const { error, handle } = useErr();
  useEffect(() => {
    api.sales
      .list({ receipt: receipt || undefined, limit: 30 })
      .then((p) => setRows(p.rows))
      .catch(handle);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [receipt]);
  return (
    <Modal title="Recent sales" size="xl" onClose={onClose}>
      <div className="grid-2">
        <div className="col">
          <input
            className="input"
            placeholder="Receipt number…"
            value={receipt}
            onChange={(e) => setReceipt(e.target.value)}
            autoFocus
          />
          <div style={{ maxHeight: 440, overflow: "auto" }}>
            <table className="table">
              <tbody>
                {rows.map((s) => (
                  <tr
                    key={s.sale_id}
                    className={`clickable ${preview?.id === s.sale_id ? "selected" : ""}`}
                    onClick={async () => {
                      try {
                        const p = await api.receipts.preview("sale", s.sale_id);
                        setPreview({ id: s.sale_id, text: p.text });
                      } catch (e) {
                        handle(e);
                      }
                    }}
                  >
                    <td className="mono">{s.receipt_number}</td>
                    <td>{formatShort(s.completed_at)}</td>
                    <td>{s.methods.split(",").map(methodLabel).join(", ")}</td>
                    <td className="num">{formatMoney(s.total_minor)}</td>
                    <td>
                      {s.status !== "completed" ? <Chip tone="warning">{s.status.replace("_", " ")}</Chip> : null}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
        <div className="col">
          {preview ? (
            <>
              <div className="receipt-stage">
                <div className="receipt-paper" style={{ width: "fit-content" }}>
                  {preview.text}
                </div>
              </div>
              <Button
                variant="primary"
                icon={<Printer size={16} />}
                onClick={async () => {
                  try {
                    const r = await api.sales.reprint(preview.id);
                    toast(
                      r.status === "printed" ? "success" : "warning",
                      r.status === "printed" ? "Reprinted (marked COPY)" : "Not printed",
                      r.message ?? undefined,
                    );
                  } catch (e) {
                    handle(e);
                  }
                }}
              >
                Reprint (copy)
              </Button>
            </>
          ) : (
            <div className="empty">Select a sale to preview its receipt.</div>
          )}
        </div>
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
    </Modal>
  );
}

export function PrintQueueDialog({ onClose }: { onClose: () => void }) {
  const [rows, setRows] = useState<PrintJobRow[]>([]);
  const { error, handle } = useErr();
  const load = () => api.print.queue().then(setRows).catch(handle);
  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return (
    <Modal title="Print queue" size="lg" onClose={onClose}>
      {rows.length === 0 ? (
        <div className="empty">
          <h3>Nothing waiting to print</h3>
        </div>
      ) : null}
      {rows.length ? (
        <table className="table">
          <thead>
            <tr>
              <th>Document</th>
              <th>Reference</th>
              <th>Created</th>
              <th>Problem</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {rows.map((j) => (
              <tr key={j.job_id}>
                <td>{j.kind.replace("_", " ")}</td>
                <td className="mono">{j.reference}</td>
                <td>{formatShort(j.created_at)}</td>
                <td className="small">{j.last_error ?? "Waiting"}</td>
                <td className="num">
                  <Button size="sm" onClick={async () => (await api.print.retry(j.job_id), void load())}>
                    Retry
                  </Button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}
      {error ? <Banner tone="danger">{error}</Banner> : null}
    </Modal>
  );
}

export function DeliveryQuickDialog({
  saleId,
  customer,
  onClose,
}: {
  saleId: string | null;
  customer: CustomerRef | null;
  onClose: () => void;
}) {
  const toast = useToast();
  const [address, setAddress] = useState("");
  const [area, setArea] = useState("");
  const [phone, setPhone] = useState(customer?.phone ?? "");
  const [notes, setNotes] = useState("");
  const [pay, setPay] = useState(saleId ? "paid" : "cod");
  const { error, handle } = useErr();
  const create = async () => {
    try {
      const d = await api.deliveries.create({
        sale_id: saleId,
        customer_id: customer?.customer_id ?? null,
        address: address || null,
        area: area || null,
        phone: phone || null,
        payment_status: pay,
        notes: notes || null,
      });
      toast("success", `Delivery ${d.delivery_number} created`);
      onClose();
    } catch (e) {
      handle(e);
    }
  };
  return (
    <Modal
      title="Delivery"
      size="md"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" className="right" onClick={create}>
            Create delivery
          </Button>
        </>
      }
    >
      <div className="form-grid">
        <TextInput label="Area" value={area} onChange={(e) => setArea(e.target.value)} autoFocus />
        <TextInput label="Phone" value={phone} onChange={(e) => setPhone(e.target.value)} />
        <TextInput
          label="Address"
          value={address}
          onChange={(e) => setAddress(e.target.value)}
          fieldClass="span-2"
          hint={customer ? "Leave empty to use the customer's saved address." : undefined}
        />
        <div className="field">
          <label>Payment</label>
          <select className="select" value={pay} onChange={(e) => setPay(e.target.value)}>
            <option value="paid">Paid</option>
            <option value="cod">Cash on delivery</option>
            <option value="pending">Payment pending</option>
          </select>
        </div>
        <TextInput label="Notes" value={notes} onChange={(e) => setNotes(e.target.value)} />
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
    </Modal>
  );
}
