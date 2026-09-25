import { useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { CheckCircle2, CircleAlert, FileScan, Image as ImageIcon, RefreshCw, Send, Upload } from "lucide-react";
import { api } from "../../api";
import type {
  AutomationStatus,
  FileBlob,
  InvoiceScan,
  InvoiceScanLine,
  PaymentReview,
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

const WA_PROCESS: Record<string, () => string> = {
  disabled: () => t("Off (module disabled)"),
  stopped: () => t("Stopped"),
  starting: () => t("Starting"),
  running: () => t("Running"),
  restarting: () => t("Restarting after a failure"),
  failed: () => t("Failed, retrying"),
};

const WA_SESSION: Record<string, () => string> = {
  none: () => t("Not linked"),
  pairing: () => t("Waiting for the phone to link"),
  paired: () => t("Linked"),
  logged_out: () => t("Unlinked from the phone"),
};

/** WhatsApp process / session / connection / readiness and OCR, each shown separately. */
function AutomationStates({ st }: { st: AutomationStatus }) {
  const wa = st.whatsapp;
  const ocr = st.ocr;
  return (
    <div className="card card-pad col gap-16">
      <StateRow
        label={t("WhatsApp client")}
        ok={wa.process === "running" ? true : wa.process === "stopped" || wa.process === "disabled" ? null : false}
        value={WA_PROCESS[wa.process]?.() ?? wa.process}
        hint={
          wa.restarts > 0
            ? t("Restarted {0} times since AMWAPOS started.", wa.restarts) +
              (wa.next_retry_at ? " " + t("Next attempt {0}.", relative(wa.next_retry_at)) : "")
            : undefined
        }
      />
      <StateRow
        label={t("Session")}
        ok={wa.session === "paired" ? true : wa.session === "logged_out" ? false : null}
        value={WA_SESSION[wa.session]?.() ?? wa.session}
        hint={wa.account ? t("Number {0}", wa.account.split("@")[0].split(":")[0]) : undefined}
      />
      <StateRow
        label={t("Connected")}
        ok={wa.process === "running" ? wa.connected : null}
        value={wa.connected ? t("Yes") : t("No")}
      />
      <StateRow
        label={t("Ready to send")}
        ok={wa.process === "running" ? wa.ready : null}
        value={wa.ready ? t("Yes") : t("No")}
        hint={
          st.queue
            ? t("{0} waiting to send, {1} failed.", st.queue.queued, st.queue.failed) +
              (wa.last_send_at ? " " + t("Last sent {0}.", relative(wa.last_send_at)) : "")
            : undefined
        }
      />
      {st.features["ocr.enabled"] ? (
        <StateRow
          label={t("Offline OCR")}
          ok={ocr.available}
          value={
            ocr.available
              ? t("Ready ({0})", ocr.languages.join(", "))
              : ocr.error_code === "ocr_model_missing"
                ? t("Models missing")
                : t("Unavailable")
          }
          hint={ocr.error ? tb(ocr.error) : (ocr.engine ?? undefined)}
        />
      ) : null}
      {wa.banned_until ? (
        <Banner tone="danger">
          {t("WhatsApp temporarily blocked this number until {0}.", formatDateTime(wa.banned_until))}
        </Banner>
      ) : null}
      {wa.last_error && !wa.ready ? (
        <div className="tiny">
          {t("Last WhatsApp message")}: {wa.last_error}
        </div>
      ) : null}
      {wa.last_send_error ? (
        <div className="tiny">
          {t("Last send error")}: {wa.last_send_error}
        </div>
      ) : null}
    </div>
  );
}

/** Always shown (also when the module is off): what this module is and its risks. */
function WaAbout() {
  return (
    <Banner tone="warning" title={t("About WhatsApp in AMWAPOS")}>
      <ul className="col gap-8" style={{ margin: 0, paddingInlineStart: 18 }}>
        <li>
          {t(
            "Switched on by the owner in Settings → Features (WhatsApp). It is off by default; selling, refunds and shifts never depend on it.",
          )}
        </li>
        <li>
          {t(
            "AMWAPOS links to a WhatsApp number like WhatsApp Web does, using an unofficial client built into AMWAPOS. This is not the WhatsApp Business API.",
          )}
        </li>
        <li>
          <strong>{t("Ban risk")}:</strong>{" "}
          {t(
            "WhatsApp's terms do not allow unofficial clients. WhatsApp can restrict or ban a number that uses one, especially for bulk or unsolicited messages. Use a number you can afford to lose and message only customers who expect it.",
          )}
        </li>
        <li>
          {t(
            "The link (session keys) is stored on this computer in its own file inside the AMWAPOS data folder, separate from the sales database. Anyone with that file can use the number.",
          )}
        </li>
      </ul>
    </Banner>
  );
}

// ---------------------------------------------------------------- WhatsApp

type WaTab = "connection" | "conversations" | "outbox" | "templates" | "diagnostics";

export function WhatsAppPage() {
  const [tab, setTab] = useState<WaTab>("connection");
  return (
    <div>
      <PageHeader
        title={t("WhatsApp")}
        subtitle={t(
          "Receipts, delivery updates and customer messages through a WhatsApp number linked to this computer.",
        )}
      />
      <div className="col gap-16">
        <WaAbout />
        <FeatureGate feature="whatsapp.enabled">
          <Tabs<WaTab>
            value={tab}
            onChange={setTab}
            tabs={[
              { key: "connection", label: t("Connection") },
              { key: "conversations", label: t("Conversations") },
              { key: "outbox", label: t("Sent messages") },
              { key: "templates", label: t("Templates") },
              { key: "diagnostics", label: t("Diagnostics") },
            ]}
          />
          <div style={{ marginTop: 16 }}>
            {tab === "connection" ? <WaConnection /> : null}
            {tab === "conversations" ? <WaConversations /> : null}
            {tab === "outbox" ? <WaOutbox /> : null}
            {tab === "templates" ? <WaTemplates /> : null}
            {tab === "diagnostics" ? <WaDiagnostics /> : null}
          </div>
        </FeatureGate>
      </div>
    </div>
  );
}

const svgUrl = (svg: string) => `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;

function WaConnection() {
  const toast = useToast();
  const { has } = useSession();
  const { data: st, error, reload } = useLoad(() => api.whatsapp.status(), []);
  const act = useAction();
  const [unlink, setUnlink] = useState(false);
  const [backup, setBackup] = useState(false);
  const [ack, setAck] = useState(false);
  const [pairPhone, setPairPhone] = useState("");
  const wa = st?.whatsapp;
  const busy = wa
    ? wa.process === "starting" || wa.session === "pairing" || (wa.process === "running" && !wa.ready)
    : false;
  useEffect(() => {
    const id = window.setInterval(() => void reload(), busy ? 2000 : 6000);
    return () => window.clearInterval(id);
  }, [busy, reload]);
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!st || !wa) return <Skeleton />;
  const running = wa.process === "running" || wa.process === "starting" || wa.process === "restarting";
  return (
    <div className="grid-2" style={{ gap: 16, alignItems: "start" }}>
      <AutomationStates st={st} />
      <div className="card card-pad col gap-16">
        <h3>{t("Linked phone")}</h3>
        {wa.session === "pairing" && wa.qr?.svg ? (
          <div className="col gap-8" style={{ alignItems: "center" }}>
            <img
              src={svgUrl(wa.qr.svg)}
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
        {wa.session === "pairing" && wa.pair_code ? (
          <div className="col gap-8" style={{ alignItems: "center" }}>
            <div className="mono" style={{ fontSize: 32, letterSpacing: 4 }} dir="ltr" data-testid="wa-pair-code">
              {wa.pair_code.code}
            </div>
            <div className="small" style={{ textAlign: "center" }}>
              {t(
                "On the phone: Linked devices → Link a device → Link with phone number instead, then enter this code.",
              )}
            </div>
          </div>
        ) : null}
        {wa.ready ? (
          <Banner tone="success" title={t("WhatsApp is ready")}>
            {wa.account ? t("Linked as {0}.", wa.account.split("@")[0].split(":")[0]) : null}
          </Banner>
        ) : null}
        {wa.session === "logged_out" ? (
          <Banner tone="warning">{t("This computer was unlinked from the phone. Link again to continue.")}</Banner>
        ) : null}
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <div className="row">
          {!running ? (
            <Button
              variant="primary"
              loading={act.busy}
              onClick={async () => {
                if (await act.run(() => api.whatsapp.start())) void reload();
              }}
            >
              {wa.session === "paired" ? t("Reconnect") : t("Link and show QR code")}
            </Button>
          ) : (
            <Button
              loading={act.busy}
              onClick={async () => {
                await act.run(() => api.whatsapp.stop());
                void reload();
              }}
            >
              {t("Stop")}
            </Button>
          )}
          {wa.session === "paired" ? (
            <Button variant="danger" onClick={() => setUnlink(true)}>
              {t("Unlink phone")}
            </Button>
          ) : null}
          <Button variant="ghost" icon={<RefreshCw size={16} />} onClick={() => void reload()}>
            {t("Refresh")}
          </Button>
        </div>
        {wa.session !== "paired" ? (
          <div className="row" style={{ alignItems: "flex-end" }}>
            <TextInput
              label={t("Or link with a code (shop's WhatsApp number)")}
              value={pairPhone}
              dir="ltr"
              placeholder="+973…"
              onChange={(e) => setPairPhone(e.target.value)}
            />
            <Button
              loading={act.busy}
              disabled={!pairPhone.trim()}
              onClick={async () => {
                if (await act.run(() => api.whatsapp.pairCode(pairPhone))) void reload();
              }}
            >
              {t("Get pairing code")}
            </Button>
          </div>
        ) : null}
        <div className="tiny">
          {t(
            "WhatsApp runs inside AMWAPOS with its own supervisor. If it stops, it restarts by itself and the tills keep selling; messages wait in the queue.",
          )}
        </div>
        {has("settings.manage") && wa.session === "paired" ? (
          <div className="row">
            <Button variant="ghost" onClick={() => setBackup(true)}>
              {t("Back up WhatsApp session…")}
            </Button>
          </div>
        ) : null}
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
            if ((await act.run(() => api.whatsapp.logout())) !== undefined) {
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
      {backup ? (
        <Confirm
          title={t("Back up WhatsApp session")}
          confirmLabel={t("Create backup")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => {
            setBackup(false);
            setAck(false);
          }}
          onConfirm={async () => {
            if (!ack) return;
            const r = await act.run(() => api.whatsapp.sessionBackup(true));
            if (r) {
              setBackup(false);
              setAck(false);
              toast("success", t("Session saved to {0}", r.path));
            }
          }}
        >
          <div className="col gap-16">
            <Banner tone="danger">
              {t(
                "This file is the WhatsApp link itself. Anyone who has it can read and send this shop's WhatsApp messages without the phone. Normal AMWAPOS backups do not include it. Store it offline and delete it when no longer needed.",
              )}
            </Banner>
            <Checkbox label={t("I understand the risk")} checked={ack} onChange={setAck} />
          </div>
        </Confirm>
      ) : null}
    </div>
  );
}

function WaDiagnostics() {
  const { data, error, reload } = useLoad(() => api.whatsapp.recent(50), []);
  const { data: st } = useLoad(() => api.whatsapp.status(), []);
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton />;
  return (
    <div className="col gap-16">
      {st ? (
        <div className="card card-pad">
          <dl className="kv">
            <dt>{t("Client")}</dt>
            <dd>{st.whatsapp.adapter}</dd>
            <dt>{t("Session file")}</dt>
            <dd className="mono small" dir="ltr">
              {st.whatsapp.session_file}
            </dd>
          </dl>
        </div>
      ) : null}
      <div className="row">
        <h3 className="grow">{t("Recent sends")}</h3>
        <Button variant="ghost" icon={<RefreshCw size={16} />} onClick={() => void reload()}>
          {t("Refresh")}
        </Button>
      </div>
      <DataTable
        rows={data.sent}
        rowKey={(r) => r.message_id}
        empty={t("Nothing sent yet.")}
        columns={[
          { key: "at", label: t("Queued"), render: (r) => formatDateTime(r.created_at) },
          { key: "kind", label: t("Type"), render: (r) => r.kind },
          { key: "to", label: t("To"), render: (r) => <span dir="ltr">{r.to_phone}</span> },
          { key: "status", label: t("Status"), render: (r) => r.status },
          {
            key: "id",
            label: t("WhatsApp id"),
            render: (r) => <span className="mono tiny">{r.wa_message_id ?? "—"}</span>,
          },
          { key: "err", label: t("Error"), render: (r) => <span className="tiny">{r.last_error ?? ""}</span> },
        ]}
      />
      <h3>{t("Recent received")}</h3>
      <DataTable
        rows={data.received}
        rowKey={(r) => String(r.seq)}
        empty={t("No messages received yet.")}
        columns={[
          { key: "at", label: t("Received"), render: (r) => formatDateTime(r.received_at) },
          { key: "kind", label: t("Type"), render: (r) => r.kind },
          { key: "chat", label: t("From"), render: (r) => <span dir="ltr">{r.chat.split("@")[0]}</span> },
          { key: "media", label: t("Attachment"), render: (r) => (r.media_state === "none" ? "" : r.media_state) },
          { key: "id", label: t("WhatsApp id"), render: (r) => <span className="mono tiny">{r.wa_id}</span> },
        ]}
      />
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
  const toast = useToast();
  const { has } = useSession();
  const act = useAction();
  if (error) return <Banner tone="danger">{error}</Banner>;
  const unknown = (data ?? []).filter((c) => !c.customer_id && c.phone).length;
  return (
    <div className="col gap-16">
      {unknown > 0 && has("customers.manage") ? (
        <div className="row">
          <span className="small grow">{t("{0} senders are not customers yet.", unknown)}</span>
          <Button
            loading={act.busy}
            onClick={async () => {
              const r = await act.run(() => api.whatsapp.importContacts());
              if (r) {
                toast("success", t("{0} customers created, {1} linked", r.created, r.linked));
                void reload();
              }
            }}
          >
            {t("Import senders as customers")}
          </Button>
        </div>
      ) : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
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
  auto_payment_ack: boolean;
  receipt: { en: string; ar: string };
  dispatch: { en: string; ar: string };
  delivered: { en: string; ar: string };
  reminder: { en: string; ar: string };
  payment_ack: { en: string; ar: string };
}

export function WaTemplates() {
  const toast = useToast();
  const { has } = useSession();
  const { data, setData, error } = useLoad(() => api.settings.get<WaSettings>("whatsapp"), []);
  const act = useAction();
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton />;
  const editable = has("settings.manage");
  const tpl = (k: "receipt" | "dispatch" | "delivered" | "reminder" | "payment_ack", label: string, vars: string) => (
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
      <Checkbox
        label={t("Send the payment acknowledgement when a person confirms a payment screenshot")}
        checked={data.auto_payment_ack}
        disabled={!editable}
        onChange={(x) => setData({ ...data, auto_payment_ack: x })}
      />
      {tpl("receipt", t("Receipt"), "{business} {customer} {receipt} {total} {date}")}
      {tpl("dispatch", t("Out for delivery"), "{business} {customer} {delivery} {amount}")}
      {tpl("delivered", t("Delivered"), "{business} {customer} {delivery} {amount}")}
      {tpl("reminder", t("Payment reminder"), "{business} {customer} {delivery} {amount}")}
      {tpl("payment_ack", t("Payment received"), "{business} {customer} {amount} {reference} {delivery}")}
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
  kind: "receipt" | "dispatch" | "delivered" | "reminder";
  saleId?: string | null;
  deliveryId?: string | null;
  customerId?: string | null;
  phone?: string | null;
  size?: "sm";
}) {
  const on = useFeature(
    kind === "receipt"
      ? "whatsapp.send_receipts"
      : kind === "reminder"
        ? "whatsapp.enabled"
        : "whatsapp.delivery_notices",
  );
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
        : kind === "delivered"
          ? t("Send delivered notice")
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
  ocr_match: () => t("OCR match"),
  likely_match: () => t("Likely match"),
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
  no_reference: () => t("No bank reference found"),
  duplicate_image: () => t("Same screenshot or bank reference seen before"),
  ocr_failed: () => t("OCR failed"),
};

function reviewTone(s: string) {
  return s === "ocr_match" || s === "confirmed"
    ? "success"
    : s === "mismatch" || s === "rejected"
      ? "danger"
      : s === "needs_review" || s === "likely_match"
        ? "warning"
        : "info";
}

export function PaymentReviewsPage() {
  const ocr = useFeature("ocr.enabled");
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
      <FeatureGate feature="ocr.payment_screenshots">
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
            {["open", "ocr_match", "likely_match", "mismatch", "needs_review", "confirmed", "rejected", "all"].map(
              (s) => (
                <button key={s} className={`filter-chip ${status === s ? "active" : ""}`} onClick={() => setStatus(s)}>
                  {s === "open" ? t("Open") : s === "all" ? t("All") : REVIEW_STATUS[s]()}
                </button>
              ),
            )}
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
      <FeatureGate feature="ocr.supplier_invoices">
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
  const { has } = useSession();
  const [picking, setPicking] = useState<InvoiceScanLine | null>(null);
  const [receiveNow, setReceiveNow] = useState(false);
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
              {has("inventory.receive") ? (
                <Checkbox label={t("Receive the stock now")} checked={receiveNow} onChange={setReceiveNow} />
              ) : null}
              <Button
                variant="primary"
                disabled={!supplier}
                loading={act.busy}
                onClick={async () => {
                  const r = await act.run(() => api.invoiceScan.confirm(id, supplier, receiveNow));
                  if (r) {
                    setData(r);
                    onChanged();
                    toast(
                      "success",
                      receiveNow ? t("Purchase order created and received") : t("Draft purchase order created"),
                    );
                  }
                }}
              >
                {receiveNow ? t("Create order and receive stock") : t("Create draft purchase order")}
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
