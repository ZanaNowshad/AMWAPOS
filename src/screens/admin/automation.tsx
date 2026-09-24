import { useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { CheckCircle2, CircleAlert, FileScan, Image as ImageIcon, RefreshCw, Send, Upload } from "lucide-react";
import { api } from "../../api";
import type {
  FileBlob,
  InvoiceScan,
  InvoiceScanLine,
  PaymentReview,
  SidecarStatus,
  WaConversation,
  WaOutboxRow,
  WaThread,
} from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { FeatureGate, useFeature } from "../../components/FeatureGate";
import { Banner, Button, Checkbox, Chip, Field, PageHeader, Skeleton, Tabs, TextInput } from "../../components/ui";
import { Confirm, DataTable, Drawer, useAction, useLoad } from "./common";
import { formatMoney, formatQty, parseMoney, parseQty } from "../../lib/money";
import { formatDateTime, relative } from "../../lib/time";
import { newOperationId } from "../../lib/ids";
import { t, tb } from "../../i18n";

// ---------------------------------------------------------------- helpers

export function fileToBase64(f: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).split("base64,")[1] ?? "");
    r.onerror = () => reject(r.error);
    r.readAsDataURL(f);
  });
}

const blobUrl = (b: FileBlob | null | undefined) => (b ? `data:${b.mime};base64,${b.base64}` : null);

function StateRow({ label, ok, value, hint }: { label: string; ok: boolean | null; value: string; hint?: string }) {
  return (
    <div className="row" style={{ alignItems: "flex-start" }}>
      {ok === null ? (
        <CircleAlert size={18} color="var(--text-3)" />
      ) : ok ? (
        <CheckCircle2 size={18} color="var(--success)" />
      ) : (
        <CircleAlert size={18} color="var(--danger)" />
      )}
      <div className="grow">
        <div style={{ fontWeight: 600 }}>{label}</div>
        {hint ? <div className="tiny">{hint}</div> : null}
      </div>
      <span className="small" data-testid={`state-${label}`}>
        {value}
      </span>
    </div>
  );
}

const WA_STATE: Record<string, () => string> = {
  stopped: () => t("Not connected"),
  starting: () => t("Starting"),
  pairing: () => t("Waiting for QR scan"),
  connecting: () => t("Connecting"),
  connected: () => t("Connected, syncing"),
  ready: () => t("Ready"),
  logged_out: () => t("Unlinked from the phone"),
  error: () => t("Error"),
};

/** Process / health / identity / link / OCR, each shown separately. */
function SidecarStates({ st }: { st: SidecarStatus }) {
  const wa = st.whatsapp;
  return (
    <div className="card card-pad col gap-16">
      <StateRow
        label={t("Sidecar process")}
        ok={st.process === "running"}
        value={
          st.process === "running"
            ? t("Running (PID {0}, 127.0.0.1:{1})", String(st.pid ?? ""), String(st.port ?? ""))
            : st.process === "failed"
              ? t("Failed")
              : t("Stopped")
        }
        hint={st.installed ? undefined : t("The sidecar is not installed on this computer. Reinstall AMWAPOS.")}
      />
      <StateRow
        label={t("Health check")}
        ok={st.process === "running" ? st.health : null}
        value={st.health ? t("Answering") : t("No answer")}
      />
      <StateRow
        label={t("Identity")}
        ok={st.health ? st.identity : null}
        value={st.identity ? t("Verified AMWAPOS sidecar {0}", st.version ?? "") : t("Not verified")}
        hint={t("Checks that the local service answering is the one AMWAPOS started.")}
      />
      {st.features.whatsapp ? (
        <>
          <StateRow
            label={t("WhatsApp connected")}
            ok={wa ? wa.connected : null}
            value={wa ? (WA_STATE[wa.state]?.() ?? wa.state) : "—"}
          />
          <StateRow
            label={t("WhatsApp ready to send")}
            ok={wa ? wa.ready : null}
            value={wa?.ready ? (wa.me?.name ?? wa.me?.id ?? t("Yes")) : t("No")}
          />
        </>
      ) : null}
      {st.features.ocr ? (
        <StateRow
          label={t("Offline OCR")}
          ok={st.ocr ? st.ocr.enabled : null}
          value={st.ocr ? (st.ocr.enabled ? t("Ready ({0})", st.ocr.languages.join(", ")) : t("Disabled")) : "—"}
          hint={st.ocr?.reason ? tb(st.ocr.reason) : undefined}
        />
      ) : null}
      {st.last_error ? <Banner tone="danger">{tb(st.last_error)}</Banner> : null}
      {wa?.last_error && !wa.ready ? (
        <div className="tiny">
          {t("Last WhatsApp error")}: {wa.last_error}
        </div>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------- WhatsApp

type WaTab = "connection" | "conversations" | "outbox" | "templates";

export function WhatsAppPage() {
  const [tab, setTab] = useState<WaTab>("connection");
  return (
    <div>
      <PageHeader
        title={t("WhatsApp")}
        subtitle={t(
          "Receipts, delivery updates and customer messages through a WhatsApp account linked to this computer.",
        )}
      />
      <FeatureGate feature="whatsapp">
        <Tabs<WaTab>
          value={tab}
          onChange={setTab}
          tabs={[
            { key: "connection", label: t("Connection") },
            { key: "conversations", label: t("Conversations") },
            { key: "outbox", label: t("Sent messages") },
            { key: "templates", label: t("Templates") },
          ]}
        />
        <div style={{ marginTop: 16 }}>
          {tab === "connection" ? <WaConnection /> : null}
          {tab === "conversations" ? <WaConversations /> : null}
          {tab === "outbox" ? <WaOutbox /> : null}
          {tab === "templates" ? <WaTemplates /> : null}
        </div>
      </FeatureGate>
    </div>
  );
}

function WaConnection() {
  const toast = useToast();
  const { data: st, error, reload } = useLoad(() => api.sidecar.status(), []);
  const act = useAction();
  const [unlink, setUnlink] = useState(false);
  const state = st?.whatsapp?.state;
  useEffect(() => {
    const fast = state === "pairing" || state === "starting" || state === "connecting" || state === "connected";
    const id = window.setInterval(() => void reload(), fast ? 2000 : 6000);
    return () => window.clearInterval(id);
  }, [state, reload]);
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!st) return <Skeleton />;
  const wa = st.whatsapp;
  return (
    <div className="grid-2" style={{ gap: 16, alignItems: "start" }}>
      <SidecarStates st={st} />
      <div className="card card-pad col gap-16">
        <h3>{t("Linked phone")}</h3>
        {wa?.state === "pairing" && wa.qr_data_url ? (
          <div className="col gap-8" style={{ alignItems: "center" }}>
            <img
              src={wa.qr_data_url}
              alt={t("WhatsApp pairing QR code")}
              width={280}
              height={280}
              data-testid="wa-qr"
            />
            <div className="small" style={{ textAlign: "center" }}>
              {t("On the store phone open WhatsApp → Settings → Linked devices → Link a device, and scan this code.")}
            </div>
          </div>
        ) : null}
        {wa?.ready ? (
          <Banner tone="success" title={t("WhatsApp is ready")}>
            {t("Linked as {0}.", wa.me?.name ?? wa.me?.id ?? "")}
          </Banner>
        ) : null}
        {wa?.state === "logged_out" ? (
          <Banner tone="warning">
            {t("This computer was unlinked from the phone. Connect again and scan a new code.")}
          </Banner>
        ) : null}
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <div className="row">
          {!wa || wa.state === "stopped" || wa.state === "logged_out" || wa.state === "error" ? (
            <Button
              variant="primary"
              loading={act.busy}
              onClick={async () => {
                if (await act.run(() => api.whatsapp.connect())) void reload();
              }}
            >
              {wa?.linked ? t("Reconnect") : t("Connect and show QR code")}
            </Button>
          ) : (
            <Button
              loading={act.busy}
              onClick={async () => {
                await act.run(() => api.whatsapp.disconnect());
                void reload();
              }}
            >
              {t("Disconnect")}
            </Button>
          )}
          {wa?.linked ? (
            <Button variant="danger" onClick={() => setUnlink(true)}>
              {t("Unlink phone")}
            </Button>
          ) : null}
          <Button
            variant="ghost"
            icon={<RefreshCw size={16} />}
            loading={act.busy}
            onClick={async () => {
              await act.run(() => api.sidecar.restart());
              toast("info", t("Sidecar restarted"));
              void reload();
            }}
          >
            {t("Restart sidecar")}
          </Button>
        </div>
        <div className="tiny">
          {t(
            "The sidecar runs only on this computer and listens on 127.0.0.1. Checkout never waits for WhatsApp; messages queue and are sent when the link is ready.",
          )}
        </div>
      </div>
      {unlink ? (
        <Confirm
          title={t("Unlink phone")}
          confirmLabel={t("Unlink")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setUnlink(false)}
          onConfirm={async () => {
            if ((await act.run(() => api.whatsapp.unlink())) !== undefined) {
              setUnlink(false);
              void reload();
            }
          }}
        >
          {t(
            "AMWAPOS will stop sending and receiving WhatsApp messages until a phone is linked again. Message history stays in AMWAPOS.",
          )}
        </Confirm>
      ) : null}
    </div>
  );
}

function WaConversations() {
  const { data, error, reload } = useLoad(() => api.whatsapp.conversations(), []);
  const [chat, setChat] = useState<WaConversation | null>(null);
  useEffect(() => {
    const id = window.setInterval(() => void reload(), 8000);
    return () => window.clearInterval(id);
  }, [reload]);
  if (error) return <Banner tone="danger">{error}</Banner>;
  return (
    <div className="grid-2" style={{ gridTemplateColumns: "320px 1fr", gap: 16, alignItems: "start" }}>
      <div className="card" style={{ maxHeight: 640, overflow: "auto" }}>
        {!data ? (
          <Skeleton />
        ) : data.length === 0 ? (
          <div className="empty">{t("No messages received yet.")}</div>
        ) : (
          data.map((c) => (
            <button
              key={c.chat}
              className={`list-row ${chat?.chat === c.chat ? "active" : ""}`}
              style={{
                display: "block",
                width: "100%",
                textAlign: "start",
                padding: 12,
                borderBottom: "1px solid var(--border)",
              }}
              onClick={() => setChat(c)}
            >
              <div className="row">
                <strong className="grow">{c.name ?? c.phone ?? c.chat}</strong>
                {c.unread > 0 ? <Chip tone="brand">{c.unread}</Chip> : null}
              </div>
              <div className="tiny" style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                {c.last_text}
              </div>
              <div className="tiny">{relative(c.last_at)}</div>
            </button>
          ))
        )}
      </div>
      {chat ? (
        <WaThreadView key={chat.chat} chat={chat} onRead={() => void reload()} />
      ) : (
        <div className="empty">{t("Choose a conversation.")}</div>
      )}
    </div>
  );
}

function WaThreadView({ chat, onRead }: { chat: WaConversation; onRead: () => void }) {
  const toast = useToast();
  const { data, reload } = useLoad(() => api.whatsapp.thread(chat.chat), [chat.chat]);
  const [text, setText] = useState("");
  const [images, setImages] = useState<Record<number, string>>({});
  const act = useAction();
  useEffect(() => {
    void api.whatsapp.markRead(chat.chat).then(onRead, () => undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chat.chat]);
  const items = useMemo(() => {
    const d: WaThread | null = data;
    if (!d) return [];
    const inb = d.inbound.map((m) => ({ at: m.received_at, key: `i${m.seq}`, mine: false, m }));
    const out = d.outbound.map((m) => ({ at: m.created_at, key: `o${m.message_id}`, mine: true, o: m }));
    return [...inb, ...out].sort((a, b) => (a.at < b.at ? -1 : 1));
  }, [data]);
  return (
    <div className="card card-pad col gap-16">
      <div className="row">
        <h3 className="grow">{chat.name ?? chat.phone ?? chat.chat}</h3>
        {chat.customer_id ? <Link to={`/admin/customers/${chat.customer_id}`}>{t("Open customer")}</Link> : null}
      </div>
      <Banner tone="info">
        {t("Customer messages are shown as plain text. AMWAPOS never follows instructions written in a message.")}
      </Banner>
      <div className="col gap-8" style={{ maxHeight: 460, overflow: "auto" }}>
        {items.map((it) =>
          "m" in it && it.m ? (
            <div key={it.key} className="bubble in" style={{ alignSelf: "flex-start", maxWidth: "80%" }}>
              {it.m.body ? <div style={{ whiteSpace: "pre-wrap" }}>{it.m.body}</div> : null}
              {it.m.caption ? <div style={{ whiteSpace: "pre-wrap" }}>{it.m.caption}</div> : null}
              {it.m.has_media ? (
                images[it.m.seq] ? (
                  <img src={images[it.m.seq]} alt={t("Attachment")} style={{ maxWidth: 260 }} />
                ) : (
                  <Button
                    size="sm"
                    icon={<ImageIcon size={14} />}
                    onClick={async () => {
                      const seq = (it.m as { seq: number }).seq;
                      const b = await act.run(() => api.whatsapp.media(seq));
                      const url = blobUrl(b);
                      if (url) setImages((x) => ({ ...x, [seq]: url }));
                    }}
                  >
                    {t("View attachment")}
                  </Button>
                )
              ) : null}
              {it.m.kind === "other" && !it.m.body ? <div className="tiny">{t("Unsupported message type")}</div> : null}
              <div className="tiny">{formatDateTime(it.at)}</div>
            </div>
          ) : "o" in it && it.o ? (
            <div key={it.key} className="bubble out" style={{ alignSelf: "flex-end", maxWidth: "80%" }}>
              <div style={{ whiteSpace: "pre-wrap" }}>{it.o.body}</div>
              {it.o.document_name ? <div className="tiny">📎 {it.o.document_name}</div> : null}
              <div className="tiny">
                {formatDateTime(it.at)} · <OutboxStatus row={it.o} />
              </div>
            </div>
          ) : null,
        )}
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <div className="row">
        <textarea
          className="input grow"
          rows={2}
          maxLength={4000}
          value={text}
          placeholder={t("Type a reply")}
          aria-label={t("Reply")}
          onChange={(e) => setText(e.target.value)}
        />
        <Button
          variant="primary"
          icon={<Send size={16} />}
          disabled={!text.trim() || !data?.phone}
          loading={act.busy}
          onClick={async () => {
            const r = await act.run(() =>
              api.whatsapp.queue({
                operation_id: newOperationId(),
                kind: "text",
                to_phone: data?.phone,
                customer_id: chat.customer_id,
                text,
              }),
            );
            if (r) {
              setText("");
              toast("success", t("Message queued"));
              void reload();
            }
          }}
        >
          {t("Send")}
        </Button>
      </div>
    </div>
  );
}

function OutboxStatus({ row }: { row: WaOutboxRow }) {
  const tone =
    row.status === "sent"
      ? "success"
      : row.status === "failed"
        ? "danger"
        : row.status === "cancelled"
          ? "default"
          : "info";
  const label: Record<string, string> = {
    queued: t("Queued"),
    sending: t("Sending"),
    sent: t("Sent"),
    failed: t("Failed"),
    cancelled: t("Cancelled"),
  };
  return <Chip tone={tone}>{label[row.status] ?? row.status}</Chip>;
}

function WaOutbox() {
  const [status, setStatus] = useState("");
  const { data, loading, error, reload } = useLoad(() => api.whatsapp.outbox(status || undefined), [status]);
  const act = useAction();
  const kinds: Record<string, string> = {
    receipt: t("Receipt"),
    dispatch: t("Dispatch"),
    reminder: t("Reminder"),
    text: t("Message"),
  };
  return (
    <div className="col gap-16">
      <div className="filters">
        {[
          ["", t("All")],
          ["queued", t("Queued")],
          ["sent", t("Sent")],
          ["failed", t("Failed")],
        ].map(([k, l]) => (
          <button key={k} className={`filter-chip ${status === k ? "active" : ""}`} onClick={() => setStatus(k)}>
            {l}
          </button>
        ))}
      </div>
      {error || act.error ? <Banner tone="danger">{error ?? act.error}</Banner> : null}
      <DataTable<WaOutboxRow>
        rows={data}
        loading={loading}
        rowKey={(r) => r.message_id}
        empty={<div className="empty">{t("No messages.")}</div>}
        columns={[
          { key: "at", label: t("Created"), render: (r) => formatDateTime(r.created_at), sort: (r) => r.created_at },
          { key: "kind", label: t("Type"), render: (r) => kinds[r.kind] ?? r.kind },
          {
            key: "to",
            label: t("To"),
            render: (r) => <span dir="ltr">{r.customer_name ? `${r.customer_name} · ${r.to_phone}` : r.to_phone}</span>,
          },
          { key: "status", label: t("Status"), render: (r) => <OutboxStatus row={r} /> },
          {
            key: "err",
            label: t("Details"),
            render: (r) =>
              r.last_error ? (
                <span className="tiny">{tb(r.last_error)}</span>
              ) : r.attempts ? (
                t("{0} attempts", r.attempts)
              ) : (
                ""
              ),
          },
          {
            key: "act",
            label: "",
            render: (r) =>
              r.status === "failed" ? (
                <div className="row">
                  <Button
                    size="sm"
                    onClick={async () =>
                      (await act.run(() => api.whatsapp.outboxAction(r.message_id, "retry"))) && reload()
                    }
                  >
                    {t("Retry")}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={async () =>
                      (await act.run(() => api.whatsapp.outboxAction(r.message_id, "cancel"))) && reload()
                    }
                  >
                    {t("Cancel")}
                  </Button>
                </div>
              ) : r.status === "queued" ? (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={async () =>
                    (await act.run(() => api.whatsapp.outboxAction(r.message_id, "cancel"))) && reload()
                  }
                >
                  {t("Cancel")}
                </Button>
              ) : null,
          },
        ]}
      />
    </div>
  );
}

interface WaSettings {
  default_lang: "en" | "ar";
  attach_pdf: boolean;
  send_read_receipts: boolean;
  receipt: { en: string; ar: string };
  dispatch: { en: string; ar: string };
  reminder: { en: string; ar: string };
}

export function WaTemplates() {
  const toast = useToast();
  const { has } = useSession();
  const { data, setData, error } = useLoad(() => api.settings.get<WaSettings>("whatsapp"), []);
  const act = useAction();
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton />;
  const editable = has("settings.manage");
  const tpl = (k: "receipt" | "dispatch" | "reminder", label: string, vars: string) => (
    <div className="col gap-8">
      <h3>{label}</h3>
      <div className="tiny">
        {t("Placeholders")}: <span dir="ltr">{vars}</span>
      </div>
      <div className="grid-2" style={{ gap: 12 }}>
        <Field label={t("English")}>
          <textarea
            className="input"
            rows={4}
            dir="ltr"
            disabled={!editable}
            value={data[k].en}
            onChange={(e) => setData({ ...data, [k]: { ...data[k], en: e.target.value } })}
          />
        </Field>
        <Field label={t("Arabic")}>
          <textarea
            className="input"
            rows={4}
            dir="rtl"
            disabled={!editable}
            value={data[k].ar}
            onChange={(e) => setData({ ...data, [k]: { ...data[k], ar: e.target.value } })}
          />
        </Field>
      </div>
    </div>
  );
  return (
    <div className="card card-pad col gap-16">
      <div className="form-grid">
        <Field label={t("Default message language")}>
          <select
            className="select"
            disabled={!editable}
            value={data.default_lang}
            onChange={(e) => setData({ ...data, default_lang: e.target.value as "en" | "ar" })}
          >
            <option value="en">{t("English")}</option>
            <option value="ar">{t("Arabic")}</option>
          </select>
        </Field>
      </div>
      <Checkbox
        label={t("Attach the PDF receipt to receipt messages")}
        checked={data.attach_pdf}
        disabled={!editable}
        onChange={(x) => setData({ ...data, attach_pdf: x })}
      />
      <Checkbox
        label={t("Show messages as read on the phone when opened here")}
        checked={data.send_read_receipts}
        disabled={!editable}
        onChange={(x) => setData({ ...data, send_read_receipts: x })}
      />
      {tpl("receipt", t("Receipt"), "{business} {customer} {receipt} {total} {date}")}
      {tpl("dispatch", t("Out for delivery"), "{business} {customer} {delivery} {amount}")}
      {tpl("reminder", t("Payment reminder"), "{business} {customer} {delivery} {amount}")}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {editable ? (
        <div className="row">
          <Button
            variant="primary"
            className="right"
            loading={act.busy}
            onClick={async () => {
              const r = await act.run(() => api.settings.save("whatsapp", data));
              if (r) toast("success", t("Settings saved"));
            }}
          >
            {t("Save")}
          </Button>
        </div>
      ) : null}
    </div>
  );
}

/** Send a receipt / delivery message from other screens. Renders nothing when the module is off. */
export function WhatsAppSendButton({
  kind,
  saleId,
  deliveryId,
  customerId,
  phone,
  size,
}: {
  kind: "receipt" | "dispatch" | "reminder";
  saleId?: string | null;
  deliveryId?: string | null;
  customerId?: string | null;
  phone?: string | null;
  size?: "sm";
}) {
  const on = useFeature("whatsapp");
  const { has } = useSession();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [to, setTo] = useState(phone ?? "");
  const [lang, setLang] = useState<"en" | "ar" | "">("");
  const [opId] = useState(newOperationId);
  const act = useAction();
  if (!on || !(has("whatsapp.send") || has("whatsapp.manage"))) return null;
  const label =
    kind === "receipt"
      ? t("Send receipt on WhatsApp")
      : kind === "dispatch"
        ? t("Send delivery update")
        : t("Send payment reminder");
  return (
    <>
      <Button size={size} icon={<Send size={14} />} onClick={() => setOpen(true)} data-testid={`wa-send-${kind}`}>
        {label}
      </Button>
      {open ? (
        <Confirm
          title={label}
          confirmLabel={t("Send")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setOpen(false)}
          onConfirm={async () => {
            const r = await act.run(() =>
              api.whatsapp.queue({
                operation_id: opId,
                kind,
                sale_id: saleId,
                delivery_id: deliveryId,
                customer_id: customerId,
                to_phone: to || null,
                lang: lang || null,
              }),
            );
            if (r) {
              setOpen(false);
              toast("success", t("Queued for WhatsApp. It is sent as soon as the link is ready."));
            }
          }}
        >
          <div className="col gap-16">
            <TextInput
              label={t("WhatsApp number")}
              value={to}
              dir="ltr"
              placeholder="+973…"
              onChange={(e) => setTo(e.target.value)}
              hint={t("Leave empty to use the customer's saved number.")}
            />
            <Field label={t("Language")}>
              <select className="select" value={lang} onChange={(e) => setLang(e.target.value as "en" | "ar" | "")}>
                <option value="">{t("Default")}</option>
                <option value="en">{t("English")}</option>
                <option value="ar">{t("Arabic")}</option>
              </select>
            </Field>
          </div>
        </Confirm>
      ) : null}
    </>
  );
}

// ---------------------------------------------------------------- payment reviews

const REVIEW_STATUS: Record<string, () => string> = {
  pending: () => t("Pending"),
  matched: () => t("Matched"),
  mismatch: () => t("Mismatch"),
  needs_review: () => t("Needs review"),
  confirmed: () => t("Confirmed"),
  rejected: () => t("Rejected"),
};

const REVIEW_REASON: Record<string, () => string> = {
  awaiting_ocr: () => t("Waiting for OCR"),
  amount_matches: () => t("Amount matches the expected amount"),
  amount_differs: () => t("Amount differs from the expected amount"),
  amount_not_found: () => t("No amount found in the screenshot"),
  no_expected_amount: () => t("No expected amount: link a delivery or enter the amount"),
  low_confidence: () => t("OCR confidence is low"),
  duplicate_image: () => t("Same screenshot or bank reference seen before"),
  ocr_failed: () => t("OCR failed"),
};

function reviewTone(s: string) {
  return s === "matched" || s === "confirmed"
    ? "success"
    : s === "mismatch" || s === "rejected"
      ? "danger"
      : s === "needs_review"
        ? "warning"
        : "info";
}

export function PaymentReviewsPage() {
  const ocr = useFeature("ocr");
  const [status, setStatus] = useState("open");
  const { data, loading, error, reload } = useLoad(
    () => api.payreviews.list(status === "all" ? undefined : status),
    [status],
  );
  const [open, setOpen] = useState<string | null>(null);
  const [upload, setUpload] = useState(false);
  return (
    <div>
      <PageHeader
        title={t("Payment Reviews")}
        subtitle={t("BenefitPay and bank transfer screenshots compared with the amount you expect.")}
        actions={
          <Button icon={<Upload size={16} />} onClick={() => setUpload(true)}>
            {t("Upload screenshot")}
          </Button>
        }
      />
      <FeatureGate feature="payment_reviews">
        <div className="col gap-16">
          <Banner tone="warning" title={t("A screenshot is not proof of payment")}>
            {t(
              "Screenshots can be edited or reused. Check the BenefitPay or bank statement before confirming a payment.",
            )}
          </Banner>
          {!ocr ? (
            <Banner tone="info">{t("OCR is switched off: screenshots wait for a person to read the amount.")}</Banner>
          ) : null}
          <div className="filters">
            {["open", "matched", "mismatch", "needs_review", "confirmed", "rejected", "all"].map((s) => (
              <button key={s} className={`filter-chip ${status === s ? "active" : ""}`} onClick={() => setStatus(s)}>
                {s === "open" ? t("Open") : s === "all" ? t("All") : REVIEW_STATUS[s]()}
              </button>
            ))}
          </div>
          {error ? <Banner tone="danger">{error}</Banner> : null}
          <DataTable<PaymentReview>
            rows={data}
            loading={loading}
            rowKey={(r) => r.review_id}
            onRowClick={(r) => setOpen(r.review_id)}
            empty={<div className="empty">{t("No payment screenshots to review.")}</div>}
            columns={[
              { key: "n", label: t("Review"), render: (r) => r.review_number },
              {
                key: "at",
                label: t("Received"),
                render: (r) => formatDateTime(r.created_at),
                sort: (r) => r.created_at,
              },
              {
                key: "from",
                label: t("From"),
                render: (r) => r.customer_name ?? <span dir="ltr">{r.phone ?? t("Upload")}</span>,
              },
              {
                key: "exp",
                label: t("Expected"),
                num: true,
                render: (r) => (r.expected_minor === null ? "—" : formatMoney(r.expected_minor)),
              },
              {
                key: "det",
                label: t("Detected"),
                num: true,
                render: (r) => (r.detected_minor === null ? "—" : formatMoney(r.detected_minor)),
              },
              {
                key: "st",
                label: t("Status"),
                render: (r) => <Chip tone={reviewTone(r.status)}>{REVIEW_STATUS[r.status]?.() ?? r.status}</Chip>,
              },
            ]}
          />
        </div>
        {open ? <ReviewDrawer id={open} onClose={() => setOpen(null)} onChanged={() => void reload()} /> : null}
        {upload ? (
          <ReviewUpload
            onClose={() => setUpload(false)}
            onDone={(id) => (setUpload(false), void reload(), setOpen(id))}
          />
        ) : null}
      </FeatureGate>
    </div>
  );
}

function ReviewUpload({ onClose, onDone }: { onClose: () => void; onDone: (id: string) => void }) {
  const [file, setFile] = useState<File | null>(null);
  const [expected, setExpected] = useState("");
  const act = useAction();
  return (
    <Confirm
      title={t("Upload screenshot")}
      confirmLabel={t("Upload")}
      busy={act.busy}
      error={act.error}
      onCancel={onClose}
      onConfirm={async () => {
        if (!file) return act.setError(t("Choose an image file."));
        const r = await act.run(async () =>
          api.payreviews.upload({
            file_name: file.name,
            data: await fileToBase64(file),
            expected_minor: expected ? parseMoney(expected) : null,
          }),
        );
        if (r) onDone(r.review_id);
      }}
    >
      <div className="col gap-16">
        <input
          type="file"
          accept="image/*"
          aria-label={t("Screenshot")}
          onChange={(e) => setFile(e.target.files?.[0] ?? null)}
        />
        <TextInput
          label={t("Expected amount (optional)")}
          className="num"
          value={expected}
          onChange={(e) => setExpected(e.target.value)}
        />
      </div>
    </Confirm>
  );
}

function ReviewDrawer({ id, onClose, onChanged }: { id: string; onClose: () => void; onChanged: () => void }) {
  const toast = useToast();
  const { data, error, reload } = useLoad(() => api.payreviews.get(id), [id]);
  const [note, setNote] = useState("");
  const [expected, setExpected] = useState("");
  const [delivery, setDelivery] = useState("");
  const act = useAction();
  const r = data?.review;
  const closed = r?.status === "confirmed" || r?.status === "rejected";
  const done = async (p: Promise<unknown>) => {
    if ((await act.run(() => p)) !== undefined) {
      void reload();
      onChanged();
      return true;
    }
    return false;
  };
  return (
    <Drawer title={r ? `${t("Payment review")} ${r.review_number}` : t("Payment review")} onClose={onClose}>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {!r ? (
        <Skeleton />
      ) : (
        <div className="col gap-16">
          <div className="row">
            <Chip tone={reviewTone(r.status)}>{REVIEW_STATUS[r.status]?.() ?? r.status}</Chip>
            {r.reason ? <span className="small">{REVIEW_REASON[r.reason]?.() ?? r.reason}</span> : null}
          </div>
          <Banner tone="warning">
            {t(
              "Check the BenefitPay or bank statement before confirming. The screenshot alone does not prove the money arrived.",
            )}
          </Banner>
          {data.image ? (
            <img
              src={blobUrl(data.image) ?? ""}
              alt={t("Payment screenshot")}
              style={{ maxWidth: "100%", border: "1px solid var(--border)" }}
            />
          ) : (
            <div className="tiny">{t("Image not available.")}</div>
          )}
          <dl className="kv">
            <dt>{t("Expected")}</dt>
            <dd>{r.expected_minor === null ? "—" : formatMoney(r.expected_minor)}</dd>
            <dt>{t("Detected by OCR")}</dt>
            <dd>{r.detected_minor === null ? "—" : formatMoney(r.detected_minor)}</dd>
            <dt>{t("Reference")}</dt>
            <dd dir="ltr">{r.detected_reference ?? "—"}</dd>
            <dt>{t("OCR confidence")}</dt>
            <dd>{r.ocr_confidence === null ? "—" : `${r.ocr_confidence}%`}</dd>
            <dt>{t("Delivery")}</dt>
            <dd>{r.delivery_number ?? "—"}</dd>
            <dt>{t("Customer")}</dt>
            <dd>{r.customer_name ?? r.phone ?? "—"}</dd>
            {r.duplicate_of ? (
              <>
                <dt>{t("Duplicate of")}</dt>
                <dd>{r.duplicate_of}</dd>
              </>
            ) : null}
            {r.decided_at ? (
              <>
                <dt>{t("Decided by")}</dt>
                <dd>
                  {r.decided_by_name} · {formatDateTime(r.decided_at)}
                </dd>
                <dt>{t("Note")}</dt>
                <dd>{r.note}</dd>
              </>
            ) : null}
          </dl>
          {r.ocr_text ? (
            <details>
              <summary>{t("Text read from the image")}</summary>
              <pre className="small" style={{ whiteSpace: "pre-wrap" }}>
                {r.ocr_text}
              </pre>
            </details>
          ) : null}
          {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          {!closed ? (
            <>
              <div className="card card-pad col gap-8">
                <strong>{t("Expected amount")}</strong>
                <div className="row">
                  <TextInput
                    label={t("Amount")}
                    className="num"
                    value={expected}
                    onChange={(e) => setExpected(e.target.value)}
                  />
                  <TextInput
                    label={t("Delivery ID (optional)")}
                    value={delivery}
                    onChange={(e) => setDelivery(e.target.value)}
                  />
                  <Button
                    onClick={() =>
                      void done(
                        api.payreviews.setExpected(
                          r.review_id,
                          expected ? parseMoney(expected) : null,
                          delivery || null,
                        ),
                      )
                    }
                  >
                    {t("Compare again")}
                  </Button>
                </div>
              </div>
              <TextInput
                label={t("Note (required unless matched)")}
                value={note}
                onChange={(e) => setNote(e.target.value)}
              />
              <div className="row">
                <Button
                  variant="primary"
                  loading={act.busy}
                  onClick={async () => {
                    if (
                      await done(
                        api.payreviews.decide({ review_id: r.review_id, decision: "confirm", note: note || null }),
                      )
                    )
                      toast("success", t("Payment confirmed"));
                  }}
                >
                  {t("Confirm payment")}
                </Button>
                <Button
                  variant="danger"
                  loading={act.busy}
                  onClick={async () => {
                    if (
                      await done(
                        api.payreviews.decide({ review_id: r.review_id, decision: "reject", note: note || null }),
                      )
                    )
                      toast("info", t("Screenshot rejected"));
                  }}
                >
                  {t("Reject")}
                </Button>
                {r.reason === "ocr_failed" || r.status === "needs_review" ? (
                  <Button variant="ghost" onClick={() => void done(api.ocr.retry("payment", r.review_id))}>
                    {t("Read again")}
                  </Button>
                ) : null}
              </div>
            </>
          ) : null}
        </div>
      )}
    </Drawer>
  );
}

// ---------------------------------------------------------------- invoice scan

const SCAN_STATUS: Record<string, () => string> = {
  imported: () => t("Waiting for OCR"),
  read: () => t("Read"),
  review: () => t("Ready for review"),
  confirmed: () => t("Draft order created"),
  rejected: () => t("Rejected"),
  failed: () => t("OCR failed"),
};

const MATCH_LABEL: Record<string, () => string> = {
  barcode: () => t("Barcode"),
  sku: () => t("SKU"),
  name: () => t("Name"),
  manual: () => t("Chosen"),
  none: () => t("No match"),
};

export function InvoiceScanPage() {
  const { data, loading, error, reload } = useLoad(() => api.invoiceScan.list(), []);
  const [open, setOpen] = useState<string | null>(null);
  const [upload, setUpload] = useState(false);
  const pending = data?.some((s) => s.status === "imported");
  useEffect(() => {
    if (!pending) return;
    const id = window.setInterval(() => void reload(), 3000);
    return () => window.clearInterval(id);
  }, [pending, reload]);
  return (
    <div>
      <PageHeader
        title={t("Invoice Scan")}
        subtitle={t("Read a supplier invoice with offline OCR, check every line, then create a draft purchase order.")}
        actions={
          <Button variant="primary" icon={<FileScan size={16} />} onClick={() => setUpload(true)}>
            {t("Scan invoice")}
          </Button>
        }
      />
      <FeatureGate feature="ocr">
        <div className="col gap-16">
          <Banner tone="info">
            {t(
              "Stock never changes from a scan. The draft order is received on the Receiving page when the goods arrive.",
            )}
          </Banner>
          {error ? <Banner tone="danger">{error}</Banner> : null}
          <DataTable<InvoiceScan>
            rows={data}
            loading={loading}
            rowKey={(r) => r.scan_id}
            onRowClick={(r) => setOpen(r.scan_id)}
            empty={<div className="empty">{t("No invoices scanned yet.")}</div>}
            columns={[
              { key: "n", label: t("Scan"), render: (r) => r.scan_number },
              {
                key: "at",
                label: t("Scanned"),
                render: (r) => formatDateTime(r.created_at),
                sort: (r) => r.created_at,
              },
              { key: "sup", label: t("Supplier"), render: (r) => r.supplier_name ?? "—" },
              { key: "inv", label: t("Invoice"), render: (r) => r.invoice_number ?? "—" },
              {
                key: "tot",
                label: t("Invoice total"),
                num: true,
                render: (r) => (r.total_minor === null ? "—" : formatMoney(r.total_minor)),
              },
              {
                key: "st",
                label: t("Status"),
                render: (r) => (
                  <Chip
                    tone={
                      r.status === "confirmed"
                        ? "success"
                        : r.status === "failed" || r.status === "rejected"
                          ? "danger"
                          : r.status === "review"
                            ? "warning"
                            : "info"
                    }
                  >
                    {SCAN_STATUS[r.status]?.() ?? r.status}
                  </Chip>
                ),
              },
            ]}
          />
        </div>
        {open ? <ScanDrawer id={open} onClose={() => setOpen(null)} onChanged={() => void reload()} /> : null}
        {upload ? (
          <ScanUpload
            onClose={() => setUpload(false)}
            onDone={(id) => (setUpload(false), void reload(), setOpen(id))}
          />
        ) : null}
      </FeatureGate>
    </div>
  );
}

function SupplierSelect({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const { data } = useLoad(() => api.suppliers.list(), []);
  return (
    <Field label={t("Supplier")}>
      <select className="select" value={value} onChange={(e) => onChange(e.target.value)}>
        <option value="">{t("Choose…")}</option>
        {(data ?? []).map((s) => (
          <option key={s.supplier_id} value={s.supplier_id}>
            {s.name}
          </option>
        ))}
      </select>
    </Field>
  );
}

function ScanUpload({ onClose, onDone }: { onClose: () => void; onDone: (id: string) => void }) {
  const [file, setFile] = useState<File | null>(null);
  const [supplier, setSupplier] = useState("");
  const act = useAction();
  return (
    <Confirm
      title={t("Scan invoice")}
      confirmLabel={t("Upload and read")}
      busy={act.busy}
      error={act.error}
      onCancel={onClose}
      onConfirm={async () => {
        if (!file) return act.setError(t("Choose an image file."));
        const r = await act.run(async () =>
          api.invoiceScan.import({
            file_name: file.name,
            data: await fileToBase64(file),
            supplier_id: supplier || null,
          }),
        );
        if (r) onDone(r.scan_id);
      }}
    >
      <div className="col gap-16">
        <input
          type="file"
          accept="image/*"
          aria-label={t("Invoice image")}
          onChange={(e) => setFile(e.target.files?.[0] ?? null)}
        />
        <div className="tiny">
          {t("Use a clear photo or scan (PNG or JPG). For a PDF invoice, save the page as an image first.")}
        </div>
        <SupplierSelect value={supplier} onChange={setSupplier} />
      </div>
    </Confirm>
  );
}

function ScanDrawer({ id, onClose, onChanged }: { id: string; onClose: () => void; onChanged: () => void }) {
  const toast = useToast();
  const { data, error, setData, reload } = useLoad(() => api.invoiceScan.get(id), [id]);
  const [supplier, setSupplier] = useState("");
  const [picking, setPicking] = useState<InvoiceScanLine | null>(null);
  const [reject, setReject] = useState(false);
  const [reason, setReason] = useState("");
  const act = useAction();
  const s = data?.scan;
  useEffect(() => {
    if (s?.supplier_id && !supplier) setSupplier(s.supplier_id);
  }, [s?.supplier_id, supplier]);
  useEffect(() => {
    if (s?.status !== "imported") return;
    const iv = window.setInterval(() => void reload(), 2500);
    return () => window.clearInterval(iv);
  }, [s?.status, reload]);
  const upd = async (line: InvoiceScanLine, patch: Record<string, unknown>) => {
    const r = await act.run(() => api.invoiceScan.updateLine({ scan_id: id, line_no: line.line_no, ...patch }));
    if (r) setData(r);
  };
  const editable = s?.status === "review";
  return (
    <Drawer title={s ? `${t("Invoice scan")} ${s.scan_number}` : t("Invoice scan")} onClose={onClose}>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {!data || !s ? (
        <Skeleton />
      ) : (
        <div className="col gap-16">
          <div className="row">
            <Chip>{SCAN_STATUS[s.status]?.() ?? s.status}</Chip>
            {s.duplicate_of ? <Chip tone="warning">{t("Same image as {0}", s.duplicate_of)}</Chip> : null}
            {s.po_id ? (
              <Link to={`/admin/purchase-orders/${s.po_id}`}>{t("Open draft order {0}", s.po_number ?? "")}</Link>
            ) : null}
          </div>
          {s.status === "imported" ? (
            <Banner tone="info">{t("Reading the invoice… this can take up to a minute.")}</Banner>
          ) : null}
          {s.status === "failed" ? (
            <Banner
              tone="danger"
              action={
                <Button
                  onClick={async () => (await act.run(() => api.ocr.retry("invoice", id))) !== undefined && reload()}
                >
                  {t("Read again")}
                </Button>
              }
            >
              {s.error ? tb(s.error) : t("OCR failed")}
            </Banner>
          ) : null}
          <dl className="kv">
            <dt>{t("Invoice number")}</dt>
            <dd>{s.invoice_number ?? "—"}</dd>
            <dt>{t("Invoice date")}</dt>
            <dd>{s.invoice_date ?? "—"}</dd>
            <dt>{t("Invoice total")}</dt>
            <dd>{s.total_minor === null ? "—" : formatMoney(s.total_minor)}</dd>
            <dt>{t("Included lines total")}</dt>
            <dd>{formatMoney(s.lines_total_minor)}</dd>
            <dt>{t("OCR confidence")}</dt>
            <dd>{s.ocr_confidence === null ? "—" : `${s.ocr_confidence}%`}</dd>
          </dl>
          {data.lines.length ? (
            <div className="table-wrap card">
              <table className="table">
                <thead>
                  <tr>
                    <th>{t("Use")}</th>
                    <th>{t("Invoice line")}</th>
                    <th>{t("Product")}</th>
                    <th className="num">{t("Qty")}</th>
                    <th className="num">{t("Unit cost")}</th>
                    <th className="num">{t("Line total")}</th>
                  </tr>
                </thead>
                <tbody>
                  {data.lines.map((l) => (
                    <tr key={l.line_no} className={l.include ? "" : "muted"}>
                      <td>
                        <input
                          type="checkbox"
                          aria-label={t("Include line {0}", l.line_no)}
                          disabled={!editable}
                          checked={l.include}
                          onChange={(e) => void upd(l, { include: e.target.checked })}
                        />
                      </td>
                      <td className="small" dir="auto">
                        {l.raw_text}
                      </td>
                      <td>
                        <div className="col" style={{ gap: 4 }}>
                          <span>{l.product_name ?? <em className="muted">{t("No match")}</em>}</span>
                          <span className="tiny">
                            {MATCH_LABEL[l.match_kind]?.()}
                            {l.match_kind === "name" ? ` ${l.match_score}%` : ""}
                            {l.current_cost_minor !== null &&
                            l.unit_cost_minor !== null &&
                            l.current_cost_minor !== l.unit_cost_minor
                              ? ` · ${t("last cost {0}", formatMoney(l.current_cost_minor))}`
                              : ""}
                          </span>
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
                          value={l.qty_milli === null ? "" : formatQty(l.qty_milli)}
                          onCommit={(v) => {
                            const q = parseQty(v);
                            if (q !== null) void upd(l, { qty_milli: q });
                          }}
                        />
                      </td>
                      <td className="num">
                        <InlineNumber
                          disabled={!editable}
                          value={
                            l.unit_cost_minor === null ? "" : (formatMoney(l.unit_cost_minor).split(" ").pop() ?? "")
                          }
                          onCommit={(v) => {
                            const c = parseMoney(v);
                            if (c !== null) void upd(l, { unit_cost_minor: c });
                          }}
                        />
                      </td>
                      <td className="num">{l.line_total_minor === null ? "—" : formatMoney(l.line_total_minor)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : s.status === "review" ? (
            <Banner tone="warning">
              {t("No item lines were recognised. Check the image quality or enter the order manually.")}
            </Banner>
          ) : null}
          {data.image ? (
            <details>
              <summary>{t("Invoice image")}</summary>
              <img src={blobUrl(data.image) ?? ""} alt={t("Invoice image")} style={{ maxWidth: "100%" }} />
            </details>
          ) : null}
          {data.ocr_text ? (
            <details>
              <summary>{t("Text read from the image")}</summary>
              <pre className="small" style={{ whiteSpace: "pre-wrap" }}>
                {data.ocr_text}
              </pre>
            </details>
          ) : null}
          {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          {editable ? (
            <div className="row" style={{ alignItems: "flex-end" }}>
              <SupplierSelect value={supplier} onChange={setSupplier} />
              <Button
                variant="primary"
                disabled={!supplier}
                loading={act.busy}
                onClick={async () => {
                  const r = await act.run(() => api.invoiceScan.confirm(id, supplier));
                  if (r) {
                    setData(r);
                    onChanged();
                    toast("success", t("Draft purchase order created"));
                  }
                }}
              >
                {t("Create draft purchase order")}
              </Button>
              <Button variant="danger" onClick={() => setReject(true)}>
                {t("Reject scan")}
              </Button>
            </div>
          ) : null}
        </div>
      )}
      {picking ? (
        <ProductPick
          initial={picking.description ?? ""}
          onClose={() => setPicking(null)}
          onPick={(pid) => (void upd(picking, { product_id: pid }), setPicking(null))}
        />
      ) : null}
      {reject ? (
        <Confirm
          title={t("Reject scan")}
          confirmLabel={t("Reject")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setReject(false)}
          onConfirm={async () => {
            const r = await act.run(() => api.invoiceScan.reject(id, reason));
            if (r) {
              setReject(false);
              onChanged();
              void reload();
            }
          }}
        >
          <TextInput label={t("Reason")} value={reason} onChange={(e) => setReason(e.target.value)} />
        </Confirm>
      ) : null}
    </Drawer>
  );
}

function InlineNumber({
  value,
  onCommit,
  disabled,
}: {
  value: string;
  onCommit: (v: string) => void;
  disabled?: boolean;
}) {
  const [v, setV] = useState(value);
  useEffect(() => setV(value), [value]);
  return (
    <input
      className="input num"
      style={{ width: 90 }}
      disabled={disabled}
      value={v}
      onChange={(e) => setV(e.target.value)}
      onBlur={() => v !== value && onCommit(v)}
      onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
    />
  );
}

function ProductPick({
  initial,
  onPick,
  onClose,
}: {
  initial: string;
  onPick: (id: string) => void;
  onClose: () => void;
}) {
  const [q, setQ] = useState(initial.split(" ").slice(0, 2).join(" "));
  const results = useLoad(() => (q.trim() ? api.products.search({ q, limit: 12 }) : Promise.resolve(null)), [q]);
  return (
    <Drawer title={t("Choose product")} onClose={onClose}>
      <div className="col gap-16">
        <TextInput label={t("Search products")} value={q} autoFocus onChange={(e) => setQ(e.target.value)} />
        {(results.data?.rows ?? []).map((p) => (
          <button
            key={p.product_id}
            className="list-row"
            style={{ textAlign: "start", padding: 10 }}
            onClick={() => onPick(p.product_id)}
          >
            <strong>{p.name}</strong>
            <div className="tiny">
              {p.sku} · {p.primary_barcode ?? ""}
            </div>
          </button>
        ))}
      </div>
    </Drawer>
  );
}
