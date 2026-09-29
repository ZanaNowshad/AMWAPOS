// WhatsApp orders inbox: each customer chat on the linked number, read into a
// draft digital order with real products, prices and stock. Staff see what
// the customer wrote, what was understood and why, answer open questions,
// correct lines, set delivery, check payment evidence and confirm. Nothing is
// sent, charged or taken from stock without a person doing it here (or at
// the till, where the confirmed order becomes a normal sale).
import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { Hand, Send } from "lucide-react";
import { api } from "../../api";
import type { DeliveryZone, WaOrderDetail, WaOrderLine, WaOrderRow } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { FeatureGate } from "../../components/FeatureGate";
import {
  AddressFields,
  addrFrom,
  addrLine,
  addrPayload,
  addrProblem,
  type AddrValue,
} from "../../components/AddressFields";
import { Banner, Button, Checkbox, Chip, Field, PageHeader, Skeleton, Tabs } from "../../components/ui";
import { Confirm, useAction, useLoad } from "./common";
import { formatMoney, formatQty, parseMoney, parseQty } from "../../lib/money";
import { formatDateTime, relative } from "../../lib/time";
import { t, tb } from "../../i18n";
import { InlineNumber, ProductPick } from "./automation";

type Filter = "open" | "attention" | "all";

const STATE_LABEL: Record<string, () => string> = {
  collecting: () => t("Collecting items"),
  clarifying: () => t("Waiting for an answer"),
  ready: () => t("Ready to confirm"),
  confirmed: () => t("Confirmed"),
  cancelled: () => t("Cancelled"),
  closed: () => t("Closed"),
};
const STATE_TONE: Record<string, "success" | "info" | "warning" | "default"> = {
  collecting: "info",
  clarifying: "warning",
  ready: "success",
  confirmed: "success",
};
const RESOLUTION_LABEL: Record<string, () => string> = {
  resolved: () => t("Matched"),
  ambiguous: () => t("Needs a choice"),
  unmatched: () => t("Not found"),
  unavailable: () => t("Out of stock"),
};
const RESOLUTION_TONE: Record<string, "success" | "warning" | "danger"> = {
  resolved: "success",
  ambiguous: "warning",
  unmatched: "danger",
  unavailable: "danger",
};
const CUSTOMER_LABEL: Record<string, () => string> = {
  known: () => t("Known customer"),
  provisional: () => t("New number"),
  ambiguous: () => t("Several customers match"),
};
const PRIORITY_REASON: Record<string, () => string> = {
  complaint: () => t("Complaint"),
  urgent: () => t("Asked for urgency"),
  large_order: () => t("Large order"),
  payment_waiting: () => t("Payment waiting for review"),
  repeated_unanswered: () => t("Repeated messages without a reply"),
  wrong_item: () => t("Wrong or missing item"),
};

export function WhatsAppOrdersPage() {
  const [filter, setFilter] = useState<Filter>("open");
  const { data, error, reload } = useLoad(() => api.waOrders.list(filter), [filter]);
  const [open, setOpen] = useState<string | null>(null);
  useEffect(() => {
    const iv = window.setInterval(() => void reload(), 5000);
    return () => window.clearInterval(iv);
  }, [reload]);
  return (
    <div>
      <PageHeader
        title={t("WhatsApp orders")}
        subtitle={t(
          "Chats on the linked WhatsApp number, read into draft orders with real products and prices. You answer, correct and confirm; nothing is charged or sent automatically.",
        )}
      />
      <FeatureGate feature="orders.whatsapp_ai">
        <div className="col gap-16">
          <WaMetrics />
          <div className="wa-orders">
            <div className="col gap-8 wa-orders-list" data-testid="wa-orders-list">
              <Tabs<Filter>
                value={filter}
                onChange={setFilter}
                tabs={[
                  { key: "open", label: t("Open") },
                  { key: "attention", label: t("Needs attention") },
                  { key: "all", label: t("All") },
                ]}
              />
              {error ? <Banner tone="danger">{error}</Banner> : null}
              {!data ? <Skeleton /> : null}
              {data && !data.length ? <div className="empty">{t("No WhatsApp orders here.")}</div> : null}
              {(data ?? []).map((r) => (
                <OrderRow
                  key={r.session_id}
                  r={r}
                  active={open === r.session_id}
                  onOpen={() => setOpen(r.session_id)}
                />
              ))}
            </div>
            <div className="wa-orders-detail">
              {open ? (
                <OrderDetail
                  key={open}
                  id={open}
                  name={(data ?? []).find((r) => r.session_id === open)?.push_name ?? null}
                  onChanged={() => void reload()}
                />
              ) : (
                <div className="empty">{t("Choose a conversation.")}</div>
              )}
            </div>
          </div>
        </div>
      </FeatureGate>
    </div>
  );
}

function OrderRow({ r, active, onOpen }: { r: WaOrderRow; active: boolean; onOpen: () => void }) {
  return (
    <button
      className={`list-row card card-pad ${active ? "active" : ""}`}
      style={{ textAlign: "start" }}
      onClick={onOpen}
      data-testid="wa-order-row"
    >
      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        <strong className="grow">{r.customer_name ?? r.push_name ?? r.phone ?? r.chat}</strong>
        {r.priority === "high" ? <Chip tone="danger">{t("Priority")}</Chip> : null}
        <Chip tone={STATE_TONE[r.state] ?? "default"}>{STATE_LABEL[r.state]?.() ?? r.state}</Chip>
      </div>
      <div className="small" dir="auto" style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
        {r.last_message ?? ""}
      </div>
      <div className="tiny">
        {relative(r.last_message_at)}
        {r.order_number ? ` · ${r.order_number}` : ""}
        {r.total_minor ? ` · ${formatMoney(r.total_minor)}` : ""}
        {r.open_questions ? ` · ${t("{0} open questions", r.open_questions)}` : ""}
        {r.staff_takeover ? ` · ${t("Staff took over")}` : ""}
        {!r.handled ? ` · ${t("Not handled")}` : ""}
      </div>
    </button>
  );
}

function WaMetrics() {
  const { data } = useLoad(() => api.waOrders.metrics(), []);
  if (!data || !data.messages_processed) return null;
  return (
    <div className="row small" style={{ flexWrap: "wrap", gap: 16 }} data-testid="wa-metrics">
      <span>{t("Messages read: {0}", data.messages_processed)}</span>
      <span>{t("Draft orders: {0}", data.drafts)}</span>
      <span>{t("Confirmed: {0}", data.confirmed)}</span>
      <span>{t("Items matched automatically: {0}%", data.product_resolution_pct)}</span>
      <span>{t("Needed a question: {0}%", data.clarification_rate_pct)}</span>
      <span>{t("Staff corrections: {0}", data.staff_overrides)}</span>
      {data.failed_jobs ? <span className="neg-num">{t("Failed readings: {0}", data.failed_jobs)}</span> : null}
    </div>
  );
}

function OrderDetail({ id, name, onChanged }: { id: string; name: string | null; onChanged: () => void }) {
  const { has } = useSession();
  const toast = useToast();
  const { data, error, setData, reload } = useLoad(() => api.waOrders.get(id), [id]);
  const act = useAction();
  const [picking, setPicking] = useState<WaOrderLine | "new" | null>(null);
  const [learn, setLearn] = useState(true);
  const [confirm, setConfirm] = useState(false);
  const [cancel, setCancel] = useState(false);
  const [sending, setSending] = useState(false);
  const [reply, setReply] = useState<string | null>(null);
  useEffect(() => {
    const iv = window.setInterval(() => void reload(), 6000);
    return () => window.clearInterval(iv);
  }, [reload]);
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton />;
  const s = data.session;
  const o = data.order;
  const editable =
    has("orders.manage") && ["collecting", "clarifying", "ready"].includes(s.state) && o?.status === "draft";
  const done = (r: WaOrderDetail | undefined) => {
    if (r) {
      setData(r);
      onChanged();
    }
    return r;
  };
  const line = async (a: Parameters<typeof api.waOrders.line>[2]) =>
    done(await act.run(() => api.waOrders.line(id, s.revision, a)));
  const flags = async (a: Parameters<typeof api.waOrders.flags>[1]) =>
    done(await act.run(() => api.waOrders.flags(id, a)));
  const replyText = reply ?? data.suggested_reply;

  return (
    <div className="col gap-16" data-testid="wa-order-detail">
      <div className="card card-pad col gap-8">
        <div className="row" style={{ flexWrap: "wrap" }}>
          <h3 className="grow">
            {s.customer_name ?? name ?? s.phone ?? s.chat}
            {!s.customer_name && name && s.phone ? <span className="small muted"> · {s.phone}</span> : null}
          </h3>
          <Chip tone={STATE_TONE[s.state] ?? "default"}>{STATE_LABEL[s.state]?.() ?? s.state}</Chip>
          <Chip tone={s.customer_state === "known" ? "success" : "warning"}>
            {CUSTOMER_LABEL[s.customer_state]?.() ?? s.customer_state}
          </Chip>
          {s.priority === "high" ? <Chip tone="danger">{t("Priority")}</Chip> : null}
          {s.staff_takeover ? <Chip tone="brand">{t("Staff took over")}</Chip> : null}
          {s.ai_status === "ai" ? <Chip tone="brand">{t("AI assisted")}</Chip> : null}
        </div>
        {(s.priority_reasons ?? []).length ? (
          <div className="tiny">{(s.priority_reasons ?? []).map((x) => PRIORITY_REASON[x]?.() ?? x).join(" · ")}</div>
        ) : null}
        <div className="small" data-testid="wa-order-summary">
          {tb(data.summary)}
        </div>
        {has("orders.manage") ? (
          <div className="row" style={{ flexWrap: "wrap" }}>
            <Button
              size="sm"
              icon={<Hand size={14} />}
              onClick={() => void flags({ takeover: !s.staff_takeover })}
              data-testid="wa-takeover"
            >
              {s.staff_takeover ? t("Let the reader continue") : t("Take over this chat")}
            </Button>
            <Button size="sm" onClick={() => void flags({ handled: !s.handled })}>
              {s.handled ? t("Mark not handled") : t("Mark handled")}
            </Button>
            <Button size="sm" onClick={() => void flags({ assign_to_me: true })}>
              {t("Assign to me")}
            </Button>
          </div>
        ) : null}
        {s.staff_takeover ? (
          <div className="tiny">{t("New messages are shown but no longer change the draft automatically.")}</div>
        ) : null}
      </div>

      <div className="wa-order-cols">
        <div className="card card-pad col gap-8 wa-thread" data-testid="wa-thread">
          <h3>{t("Conversation")}</h3>
          {[...(data.messages ?? [])].reverse().map((m) => (
            <div
              key={`${m.dir}-${m.seq}-${m.at}`}
              className={`bubble ${m.dir === "out" ? "out" : ""}`}
              style={{ alignSelf: m.dir === "out" ? "flex-end" : "flex-start", maxWidth: "85%" }}
            >
              <div dir="auto" style={{ whiteSpace: "pre-wrap" }}>
                {m.text ?? (m.kind === "image" ? t("Image") : m.kind)}
              </div>
              <div className="tiny">
                {formatDateTime(m.at)}
                {m.intent ? ` · ${m.intent.replace(/_/g, " ")}` : ""}
              </div>
            </div>
          ))}
        </div>

        <div className="col gap-16">
          {(s.questions ?? []).length ? (
            <div className="card card-pad col gap-8" data-testid="wa-questions">
              <h3>{t("Open questions")}</h3>
              {(s.questions ?? []).map((q) => (
                <div key={q.id} className="col" style={{ gap: 4 }}>
                  <div className="small" dir="auto">
                    {q.text}
                  </div>
                  {editable && q.line_no && (q.options ?? []).length ? (
                    <div className="row" style={{ flexWrap: "wrap", gap: 4 }}>
                      {(q.options ?? []).map((p) => (
                        <Button
                          key={p.product_id}
                          size="sm"
                          onClick={() => void line({ line_no: q.line_no, product_id: p.product_id, learn })}
                        >
                          {p.name} · {formatMoney(p.price_minor)}
                        </Button>
                      ))}
                    </div>
                  ) : null}
                </div>
              ))}
            </div>
          ) : null}

          {o ? (
            <div className="card col gap-8">
              <div className="card-pad row" style={{ paddingBottom: 0 }}>
                <h3 className="grow">
                  {t("Draft order")}{" "}
                  <Link to="/admin/orders" className="small">
                    {o.order_number}
                  </Link>
                </h3>
                {editable ? <Checkbox label={t("Remember my choices")} checked={learn} onChange={setLearn} /> : null}
              </div>
              <div className="table-wrap">
                <table className="table" data-testid="wa-order-lines">
                  <thead>
                    <tr>
                      <th>{t("Customer wrote")}</th>
                      <th>{t("Product")}</th>
                      <th className="num">{t("Qty")}</th>
                      <th className="num">{t("Price")}</th>
                      <th className="num">{t("Total")}</th>
                      <th />
                    </tr>
                  </thead>
                  <tbody>
                    {o.lines.map((l) => (
                      <tr key={l.line_no}>
                        <td className="small" dir="auto">
                          {l.requested ?? "—"}
                        </td>
                        <td>
                          <div className="col" style={{ gap: 4 }}>
                            <span>
                              {l.product_id ? l.name : <em className="muted">{t("Not chosen")}</em>}{" "}
                              <Chip tone={RESOLUTION_TONE[l.resolution] ?? "warning"}>
                                {RESOLUTION_LABEL[l.resolution]?.() ?? l.resolution}
                              </Chip>
                              {l.locked ? <Chip>{t("Set by staff")}</Chip> : null}
                            </span>
                            {editable && (l.candidates ?? []).length > 1 ? (
                              <select
                                className="select"
                                aria-label={t("Options for line {0}", l.line_no)}
                                value={l.product_id ?? ""}
                                onChange={(e) =>
                                  e.target.value && void line({ line_no: l.line_no, product_id: e.target.value, learn })
                                }
                              >
                                <option value="">{t("Choose…")}</option>
                                {(l.candidates ?? []).map((c) => (
                                  <option key={c.product_id} value={c.product_id}>
                                    {c.name} · {formatMoney(c.price_minor)}
                                    {c.availability === "unavailable" ? ` · ${t("out of stock")}` : ""}
                                  </option>
                                ))}
                              </select>
                            ) : null}
                            {(l.alternatives ?? []).length ? (
                              <div className="tiny">
                                {t("In stock instead")}:{" "}
                                {(l.alternatives ?? []).map((a, i) => (
                                  <span key={a.product_id}>
                                    {i ? ", " : ""}
                                    {editable ? (
                                      <button
                                        className="link"
                                        onClick={() => void line({ line_no: l.line_no, product_id: a.product_id })}
                                      >
                                        {a.name}
                                      </button>
                                    ) : (
                                      a.name
                                    )}
                                  </span>
                                ))}
                              </div>
                            ) : null}
                            {editable ? (
                              <Button size="sm" variant="ghost" onClick={() => setPicking(l)}>
                                {l.product_id ? t("Change") : t("Choose product")}
                              </Button>
                            ) : null}
                          </div>
                        </td>
                        <td className="num">
                          <InlineNumber
                            disabled={!editable}
                            value={formatQty(l.qty_milli)}
                            onCommit={(x) => {
                              const q = parseQty(x);
                              if (q !== null) void line({ line_no: l.line_no, qty_milli: q });
                            }}
                          />
                        </td>
                        <td className="num">{formatMoney(l.unit_price_minor)}</td>
                        <td className="num">{formatMoney(l.line_total_minor)}</td>
                        <td>
                          {editable ? (
                            <Button
                              size="sm"
                              variant="ghost"
                              onClick={() => void line({ line_no: l.line_no, remove: true })}
                            >
                              {t("Remove")}
                            </Button>
                          ) : null}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              <div className="card-pad col gap-8" style={{ paddingTop: 0 }}>
                {editable ? (
                  <div>
                    <Button size="sm" onClick={() => setPicking("new")}>
                      {t("Add item")}
                    </Button>
                  </div>
                ) : null}
                <dl className="kv">
                  <dt>{t("Items")}</dt>
                  <dd>{formatMoney(o.subtotal_minor)}</dd>
                  <dt>{t("Delivery fee")}</dt>
                  <dd>
                    {s.delivery_mode !== "delivery"
                      ? "—"
                      : o.delivery_fee_minor === null
                        ? t("Not resolved")
                        : formatMoney(o.delivery_fee_minor)}
                  </dd>
                  <dt>{t("Total")}</dt>
                  <dd>
                    <strong>{formatMoney(o.total_minor)}</strong>
                  </dd>
                  <dt>{t("Payment")}</dt>
                  <dd>{o.payment_state}</dd>
                </dl>
                <div className="tiny">{t("Prices come from the POS at the moment of reading.")}</div>
              </div>
            </div>
          ) : (
            <Banner tone="info">{t("No items yet.")}</Banner>
          )}

          {o ? <DeliveryCard data={data} editable={editable} onDone={done} /> : null}
          {o ? <CustomerCard data={data} editable={editable} onDone={done} /> : null}

          {(data.payment_evidence ?? []).length ? (
            <div className="card card-pad col gap-8" data-testid="wa-payment">
              <h3>{t("Payment evidence")}</h3>
              <div className="tiny">
                {t("A screenshot is evidence only. The order is paid only when a person verifies it.")}
              </div>
              {(data.payment_evidence ?? []).map((p) => (
                <div key={p.review_id} className="row" style={{ flexWrap: "wrap" }}>
                  <span className="grow small">
                    {p.review_number} · {t("Amount read")}: {formatMoney(p.detected_minor)}
                    {p.detected_reference ? ` · ${p.detected_reference}` : ""}
                  </span>
                  <Chip tone={p.verified ? "success" : p.status === "rejected" ? "danger" : "warning"}>
                    {p.verified ? t("Verified") : p.status === "rejected" ? t("Rejected") : t("Not verified")}
                  </Chip>
                  {has("payments.review") && !p.verified && p.status !== "rejected" ? (
                    <>
                      <Button
                        size="sm"
                        onClick={async () =>
                          done(await act.run(() => api.waOrders.payment(id, p.review_id, "verified")))
                        }
                      >
                        {t("Verify")}
                      </Button>
                      <Button
                        size="sm"
                        variant="danger"
                        onClick={async () =>
                          done(await act.run(() => api.waOrders.payment(id, p.review_id, "rejected")))
                        }
                      >
                        {t("Reject")}
                      </Button>
                    </>
                  ) : null}
                </div>
              ))}
            </div>
          ) : null}

          {(data.upsell ?? []).length ? (
            <div className="card card-pad col gap-8">
              <h3>{t("Often bought together")}</h3>
              <div className="tiny">{t("For staff only. Offer them only if it helps the customer.")}</div>
              {(data.upsell ?? []).map((u) => (
                <div key={u.product_id} className="small">
                  {u.name} · {formatMoney(u.price_minor)}
                </div>
              ))}
            </div>
          ) : null}

          {has("whatsapp.send") ? (
            <div className="card card-pad col gap-8" data-testid="wa-reply">
              <h3>{t("Reply")}</h3>
              <textarea
                className="input"
                rows={4}
                dir="auto"
                aria-label={t("Reply")}
                value={replyText}
                placeholder={t("Nothing to ask right now.")}
                onChange={(e) => setReply(e.target.value)}
              />
              <div className="row">
                <span className="tiny grow">
                  {t("Suggested from the draft. Edit it; it is sent only when you press Send.")}
                </span>
                <Button
                  variant="primary"
                  icon={<Send size={16} />}
                  disabled={!replyText.trim()}
                  onClick={() => setSending(true)}
                >
                  {t("Send")}
                </Button>
              </div>
            </div>
          ) : null}

          {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          {editable ? (
            <div className="row">
              <Button
                variant="primary"
                data-testid="wa-confirm"
                disabled={!o || !o.lines.length}
                onClick={() => setConfirm(true)}
              >
                {t("Confirm order")}
              </Button>
              <Button variant="danger" className="right" onClick={() => setCancel(true)}>
                {t("Cancel order")}
              </Button>
            </div>
          ) : null}

          <details>
            <summary>{t("What happened")}</summary>
            <ul className="plain-list tiny" data-testid="wa-events">
              {(data.events ?? []).map((e, i) => (
                <li key={i}>
                  {formatDateTime(e.at)} · {e.kind.replace(/_/g, " ")} · {e.source}
                  {e.data && typeof e.data === "object" && "reasons" in (e.data as object)
                    ? ` · ${((e.data as { reasons: string[] }).reasons ?? []).join("; ")}`
                    : ""}
                </li>
              ))}
            </ul>
          </details>
        </div>
      </div>

      {picking ? (
        <ProductPick
          initial={picking === "new" ? "" : (picking.requested ?? "")}
          onClose={() => setPicking(null)}
          onPick={(pid) => {
            const l = picking;
            setPicking(null);
            void line(
              l === "new" ? { product_id: pid, qty_milli: 1000 } : { line_no: l.line_no, product_id: pid, learn },
            );
          }}
        />
      ) : null}
      {confirm && o ? (
        <Confirm
          title={t("Confirm order")}
          confirmLabel={t("Confirm order")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setConfirm(false)}
          onConfirm={async () => {
            const r = done(await act.run(() => api.waOrders.confirm(id, s.revision)));
            if (r) {
              setConfirm(false);
              toast("success", t("Order {0} confirmed", o.order_number));
            }
          }}
        >
          {t(
            "Order {0} for {1} becomes a confirmed digital order. No payment is taken and no stock moves now: a cashier loads it into a sale as usual.",
            o.order_number,
            formatMoney(o.total_minor),
          )}
        </Confirm>
      ) : null}
      {cancel ? (
        <Confirm
          title={t("Cancel order")}
          confirmLabel={t("Cancel order")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setCancel(false)}
          onConfirm={async () => {
            if (done(await act.run(() => api.waOrders.cancel(id)))) setCancel(false);
          }}
        >
          {t("The draft order is cancelled. The customer is not messaged automatically.")}
        </Confirm>
      ) : null}
      {sending ? (
        <Confirm
          title={t("Send reply")}
          confirmLabel={t("Send")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setSending(false)}
          onConfirm={async () => {
            const r = await act.run(() => api.waOrders.send(id, replyText));
            if (r !== undefined) {
              setSending(false);
              setReply(null);
              toast("success", t("Reply queued on WhatsApp"));
              void reload();
            }
          }}
        >
          <div dir="auto" style={{ whiteSpace: "pre-wrap" }}>
            {replyText}
          </div>
        </Confirm>
      ) : null}
    </div>
  );
}

function DeliveryCard({
  data,
  editable,
  onDone,
}: {
  data: WaOrderDetail;
  editable: boolean;
  onDone: (r: WaOrderDetail | undefined) => WaOrderDetail | undefined;
}) {
  const s = data.session;
  const act = useAction();
  const zones = useLoad(() => api.settings.get<{ zones?: DeliveryZone[] }>("delivery"), []);
  const [mode, setMode] = useState(s.delivery_mode);
  const [addr, setAddr] = useState<AddrValue>(() =>
    addrFrom({ address_parts: s.address?.parts ?? null, area: s.address?.area ?? null, address: s.address_raw }),
  );
  const [zone, setZone] = useState(s.zone_id ?? "");
  const problem = mode === "delivery" ? addrProblem(addr) : null;
  return (
    <div className="card card-pad col gap-8" data-testid="wa-delivery">
      <h3>{t("Delivery")}</h3>
      <div className="small">
        {s.fee_state === "resolved"
          ? t("Delivery fee {0}", formatMoney(s.delivery_fee_minor))
          : s.delivery_mode === "delivery"
            ? t("Delivery fee not resolved: choose the zone or complete the address.")
            : null}
      </div>
      <Field label={t("Delivery or pickup")}>
        <select
          className="select"
          aria-label={t("Delivery or pickup")}
          disabled={!editable}
          value={mode}
          onChange={(e) => setMode(e.target.value)}
        >
          <option value="delivery">{t("Delivery")}</option>
          <option value="pickup">{t("Pickup")}</option>
          <option value="unknown">{t("Not stated")}</option>
        </select>
      </Field>
      {mode === "delivery" ? (
        <>
          {editable ? (
            <AddressFields value={addr} onChange={setAddr} idPrefix="wa-addr" />
          ) : (
            <div className="small">{addrLine(addr) || s.address_raw || "—"}</div>
          )}
          <Field label={t("Delivery zone")}>
            <select
              className="select"
              aria-label={t("Delivery zone")}
              disabled={!editable}
              value={zone}
              onChange={(e) => setZone(e.target.value)}
            >
              <option value="">{t("From the address")}</option>
              {(zones.data?.zones ?? [])
                .filter((z) => z.active)
                .map((z) => (
                  <option key={z.zone_id} value={z.zone_id}>
                    {z.name} · {formatMoney(z.fee_minor)}
                  </option>
                ))}
            </select>
          </Field>
          {!(zones.data?.zones ?? []).length ? (
            <div className="tiny">{t("No delivery zones are set up (Settings → Delivery).")}</div>
          ) : null}
        </>
      ) : null}
      {problem ? <div className="tiny neg-num">{problem}</div> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {editable ? (
        <div>
          <Button
            size="sm"
            loading={act.busy}
            disabled={!!problem}
            onClick={async () =>
              onDone(
                await act.run(() =>
                  api.waOrders.delivery(
                    s.session_id,
                    s.revision,
                    mode as "delivery" | "pickup" | "unknown",
                    mode === "delivery" ? addrPayload(addr).address_parts : null,
                    zone || null,
                  ),
                ),
              )
            }
          >
            {t("Save delivery")}
          </Button>
        </div>
      ) : null}
    </div>
  );
}

function CustomerCard({
  data,
  editable,
  onDone,
}: {
  data: WaOrderDetail;
  editable: boolean;
  onDone: (r: WaOrderDetail | undefined) => WaOrderDetail | undefined;
}) {
  const s = data.session;
  const act = useAction();
  const [q, setQ] = useState("");
  const found = useLoad(() => (q.trim().length >= 2 ? api.customers.search(q, false, 8) : Promise.resolve(null)), [q]);
  const pick = async (cid: string) => onDone(await act.run(() => api.waOrders.customer(s.session_id, s.revision, cid)));
  if (s.customer_state === "known" && !editable) return null;
  return (
    <div className="card card-pad col gap-8" data-testid="wa-customer">
      <h3>{t("Customer")}</h3>
      <div className="small">
        {s.customer_name ?? s.phone} · {CUSTOMER_LABEL[s.customer_state]?.() ?? s.customer_state}
      </div>
      {editable && (s.customer_candidates ?? []).length ? (
        <div className="row" style={{ flexWrap: "wrap", gap: 4 }}>
          {(s.customer_candidates ?? []).map((c) => (
            <Button key={c.customer_id} size="sm" onClick={() => void pick(c.customer_id)}>
              {c.name}
            </Button>
          ))}
        </div>
      ) : null}
      {editable ? (
        <>
          <input
            className="input"
            aria-label={t("Find a customer")}
            placeholder={t("Find a customer")}
            value={q}
            onChange={(e) => setQ(e.target.value)}
          />
          {(found.data ?? []).map((c) => (
            <button
              key={c.customer_id}
              className="list-row"
              style={{ textAlign: "start" }}
              onClick={() => void pick(c.customer_id)}
            >
              {c.name} <span className="tiny">{c.phone ?? ""}</span>
            </button>
          ))}
        </>
      ) : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
    </div>
  );
}

// ---- Settings → Delivery: zones and fees

export function DeliveryZonesEditor({
  zones,
  onChange,
  editable,
}: {
  zones: DeliveryZone[];
  onChange: (z: DeliveryZone[]) => void;
  editable: boolean;
}) {
  const set = (i: number, patch: Partial<DeliveryZone>) =>
    onChange(zones.map((z, j) => (j === i ? { ...z, ...patch } : z)));
  const money = (v: string) => parseMoney(v) ?? 0;
  const plain = (minor: number) => formatMoney(minor).split(" ").pop() ?? "";
  const ranges = (v: string) =>
    v
      .split(",")
      .map((x) => x.trim())
      .filter(Boolean)
      .map((x) => {
        const [a, b] = x.split("-").map((y) => Number(y.replace(/[^\d]/g, "")) || 0);
        return { from: a, to: b || a };
      });
  return (
    <div className="col gap-16" data-testid="delivery-zones">
      <div>
        <h3>{t("Delivery zones")}</h3>
        <div className="tiny">
          {t(
            "WhatsApp orders take the delivery fee from these zones: by block first, then by area. With no zone for an address the fee stays unresolved for a person to set.",
          )}
        </div>
      </div>
      {zones.map((z, i) => (
        <div key={z.zone_id || i} className="card card-pad col gap-8">
          <div className="row" style={{ flexWrap: "wrap", alignItems: "flex-end" }}>
            <Field label={t("Zone name")}>
              <input
                className="input"
                aria-label={t("Zone name")}
                disabled={!editable}
                value={z.name}
                onChange={(e) => set(i, { name: e.target.value })}
              />
            </Field>
            <Field label={t("Delivery fee")}>
              <input
                className="input num"
                inputMode="decimal"
                aria-label={t("Delivery fee")}
                disabled={!editable}
                defaultValue={plain(z.fee_minor)}
                onBlur={(e) => set(i, { fee_minor: money(e.target.value) })}
              />
            </Field>
            <Field label={t("Free delivery over")}>
              <input
                className="input num"
                inputMode="decimal"
                aria-label={t("Free delivery over")}
                disabled={!editable}
                defaultValue={z.free_over_minor === null ? "" : plain(z.free_over_minor)}
                onBlur={(e) => set(i, { free_over_minor: e.target.value.trim() ? money(e.target.value) : null })}
              />
            </Field>
            <Checkbox label={t("Active")} checked={z.active} onChange={(x) => editable && set(i, { active: x })} />
          </div>
          <Field label={t("Blocks (for example 200-260, 301)")}>
            <input
              className="input"
              aria-label={t("Blocks (for example 200-260, 301)")}
              disabled={!editable}
              defaultValue={z.blocks.map((b) => (b.from === b.to ? `${b.from}` : `${b.from}-${b.to}`)).join(", ")}
              onBlur={(e) => set(i, { blocks: ranges(e.target.value) })}
            />
          </Field>
          <Field label={t("Areas (comma separated)")}>
            <input
              className="input"
              aria-label={t("Areas (comma separated)")}
              disabled={!editable}
              defaultValue={z.areas.join(", ")}
              onBlur={(e) =>
                set(i, {
                  areas: e.target.value
                    .split(",")
                    .map((x) => x.trim())
                    .filter(Boolean),
                })
              }
            />
          </Field>
          {editable ? (
            <div>
              <Button size="sm" variant="ghost" onClick={() => onChange(zones.filter((_, j) => j !== i))}>
                {t("Remove zone")}
              </Button>
            </div>
          ) : null}
        </div>
      ))}
      {editable ? (
        <div>
          <Button
            onClick={() =>
              onChange([
                ...zones,
                { zone_id: "", name: "", blocks: [], areas: [], fee_minor: 0, free_over_minor: null, active: true },
              ])
            }
          >
            {t("Add zone")}
          </Button>
        </div>
      ) : null}
    </div>
  );
}
