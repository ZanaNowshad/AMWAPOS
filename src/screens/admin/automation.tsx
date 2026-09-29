import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import {
  CheckCircle2,
  CircleAlert,
  FileScan,
  Image as ImageIcon,
  RefreshCw,
  Send,
  Sparkles,
  Upload,
} from "lucide-react";
import { api } from "../../api";
import type {
  AutomationStatus,
  FileBlob,
  InvoiceScan,
  PaymentReview,
  WaConversation,
  WaOutboxRow,
  WaThread,
} from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { FeatureGate, useFeature } from "../../components/FeatureGate";
import { WaQr } from "../../components/WaQr";
import { Banner, Button, Checkbox, Chip, Field, PageHeader, Skeleton, Tabs, TextInput } from "../../components/ui";
import { Confirm, DataTable, Drawer, useAction, useLoad } from "./common";
import { formatMoney, parseMoney } from "../../lib/money";
import { formatDateTime, relative } from "../../lib/time";
import { newOperationId } from "../../lib/ids";
import { t, tb } from "../../i18n";
import { OrderEditor } from "../orders";
import { AreaPicker, PayChip, TicketRowButton, TicketSheet } from "../pos/SendLoop";
import { WaCatalog } from "./waCatalog";
import { DocMetricsStrip, ReceivingDraftsPanel, SupplierInvoicesPanel } from "./documents";
import type { DigitalOrder, WaTriageItem } from "../../api/types";

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

type WaTab = "connection" | "conversations" | "triage" | "outbox" | "templates" | "catalogue" | "diagnostics";

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
              { key: "triage", label: t("Triage") },
              { key: "outbox", label: t("Sent messages") },
              { key: "templates", label: t("Templates") },
              { key: "catalogue", label: t("Catalogue") },
              { key: "diagnostics", label: t("Diagnostics") },
            ]}
          />
          <div style={{ marginTop: 16 }}>
            {tab === "connection" ? <WaConnection /> : null}
            {tab === "conversations" ? <WaConversations /> : null}
            {tab === "triage" ? <WaTriage /> : null}
            {tab === "outbox" ? <WaOutbox /> : null}
            {tab === "templates" ? <WaTemplates /> : null}
            {tab === "catalogue" ? <WaCatalog /> : null}
            {tab === "diagnostics" ? <WaDiagnostics /> : null}
          </div>
        </FeatureGate>
      </div>
    </div>
  );
}

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
        {wa.session === "pairing" && wa.qr?.svg ? <WaQr svg={wa.qr.svg} /> : null}
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

const TRIAGE_LABEL: Record<WaTriageItem["category"], () => string> = {
  order: () => t("Order"),
  payment: () => t("Payment"),
  complaint: () => t("Complaint"),
  question: () => t("Question"),
  spam: () => t("Spam"),
  other: () => t("Other"),
};
const TRIAGE_TONE: Record<WaTriageItem["category"], "info" | "success" | "danger" | "warning" | "default"> = {
  order: "info",
  payment: "success",
  complaint: "danger",
  question: "warning",
  spam: "default",
  other: "default",
};
const SUGGESTION_LABEL: Record<string, () => string> = {
  draft_order: () => t("New ticket"),
  review_payment: () => t("Review the payment"),
  draft_reply: () => t("Draft a reply"),
  mark_read: () => t("Mark as read"),
  open_thread: () => t("Open the conversation"),
};

/** E1: incoming messages sorted by rules (and optionally the AI); a person can correct each one. */
function WaTriage() {
  const { data, error, reload } = useLoad(() => api.whatsapp.triage(100), []);
  const act = useAction();
  const toast = useToast();
  const { has } = useSession();
  const [filter, setFilter] = useState<WaTriageItem["category"] | "all">("all");
  const items = (data?.items ?? []).filter((i) => filter === "all" || i.category === filter);
  return (
    <div className="card card-pad col gap-16" data-testid="wa-triage">
      <div className="row wrap">
        <Button variant={filter === "all" ? "primary" : "ghost"} onClick={() => setFilter("all")}>
          {t("All")}
        </Button>
        {(Object.keys(TRIAGE_LABEL) as WaTriageItem["category"][]).map((c) => (
          <Button key={c} variant={filter === c ? "primary" : "ghost"} onClick={() => setFilter(c)}>
            {TRIAGE_LABEL[c]()} {data?.counts[c] ? `(${data.counts[c]})` : ""}
          </Button>
        ))}
        <span className="grow" />
        {has("ai.use") ? (
          <Button
            icon={<Sparkles size={16} />}
            loading={act.busy}
            onClick={async () => {
              const r = await act.run(() => api.whatsapp.triageAi(30));
              if (r) {
                toast("success", t("{0} messages re-sorted by the AI.", r.updated));
                void reload();
              }
            }}
          >
            {t("Sort with AI")}
          </Button>
        ) : null}
      </div>
      <div className="tiny muted">
        {t(
          "Sorted by simple rules first. The AI and people can change a category; nothing is sent or changed automatically.",
        )}
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {!data ? (
        <Skeleton />
      ) : (
        <table className="table">
          <tbody>
            {items.map((i) => (
              <tr key={i.seq}>
                <td style={{ width: 150 }}>
                  <Chip tone={TRIAGE_TONE[i.category]}>{TRIAGE_LABEL[i.category]()}</Chip>
                  <div className="tiny">
                    {i.source === "person" ? t("Set by a person") : i.source === "ai" ? t("AI") : t("Rules")} ·{" "}
                    {i.confidence}%
                  </div>
                </td>
                <td>
                  <strong>{i.push_name ?? i.phone ?? i.chat}</strong>
                  <div className="small" style={{ whiteSpace: "pre-wrap" }}>
                    {i.kind === "image" ? `[${t("Image")}] ` : ""}
                    {i.preview}
                  </div>
                  <div className="tiny">{relative(i.received_at)}</div>
                </td>
                <td style={{ width: 170 }}>
                  <select
                    className="select"
                    aria-label={t("Category")}
                    value={i.category}
                    onChange={async (e) => {
                      const r = await act.run(() =>
                        api.whatsapp.triageSet(i.seq, e.target.value as WaTriageItem["category"]),
                      );
                      if (r) void reload();
                    }}
                  >
                    {(Object.keys(TRIAGE_LABEL) as WaTriageItem["category"][]).map((c) => (
                      <option key={c} value={c}>
                        {TRIAGE_LABEL[c]()}
                      </option>
                    ))}
                  </select>
                </td>
                <td style={{ width: 170 }}>
                  <Link to={i.suggestion.link}>{SUGGESTION_LABEL[i.suggestion.action]?.() ?? i.suggestion.action}</Link>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

/** E4: the payment screenshot against what is owed, check by check. */
export function PaymentComparison({ r }: { r: PaymentReview }) {
  const c = r.comparison;
  if (!c) return null;
  const verdict: Record<string, () => string> = {
    exact: () => t("Amount matches exactly"),
    overpaid: () => t("Paid more than expected"),
    underpaid: () => t("Paid less than expected"),
    amount_not_read: () => t("The amount could not be read"),
    no_expected_amount: () => t("No expected amount to compare with"),
  };
  const check: Record<string, () => string> = {
    amount: () => t("Amount"),
    reference: () => t("Transfer reference"),
    ocr_confidence: () => t("OCR confidence"),
    not_duplicate: () => t("Not a duplicate"),
  };
  return (
    <div className="card card-pad col gap-8" data-testid="pay-compare">
      <div className="row">
        <strong className="grow">{t("Comparison")}</strong>
        <Chip tone={c.all_checks_pass ? "success" : c.verdict === "exact" ? "warning" : "danger"}>
          {verdict[c.verdict]?.()}
        </Chip>
      </div>
      <table className="table">
        <tbody>
          <tr>
            <td>{t("Expected")}</td>
            <td className="num">{c.expected_minor === null ? "—" : formatMoney(c.expected_minor)}</td>
          </tr>
          <tr>
            <td>{t("Detected by OCR")}</td>
            <td className="num">{c.detected_minor === null ? "—" : formatMoney(c.detected_minor)}</td>
          </tr>
          <tr>
            <td>{t("Difference")}</td>
            <td className="num">{c.difference_minor === null ? "—" : formatMoney(c.difference_minor)}</td>
          </tr>
          {c.checks.map((x) => (
            <tr key={x.check}>
              <td>{check[x.check]?.()}</td>
              <td>{x.ok ? <Chip tone="success">{t("OK")}</Chip> : <Chip tone="warning">{t("Check")}</Chip>}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <div className="tiny">
        {t("This only helps you decide. Nothing is confirmed or settled until a person decides.")}
      </div>
    </div>
  );
}

/** Link a chat to a customer: pick one, or create one from the chat. */
function LinkCustomer({
  chat,
  phone,
  suggestedName,
  onClose,
  onLinked,
}: {
  chat: string;
  phone: string | null;
  suggestedName: string;
  onClose: () => void;
  onLinked: () => void;
}) {
  const [q, setQ] = useState(phone ?? "");
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState(suggestedName);
  const [address, setAddress] = useState("");
  const [area, setArea] = useState("");
  const act = useAction();
  const found = useLoad(() => (q.trim().length >= 2 ? api.customers.search(q, false, 8) : Promise.resolve([])), [q]);
  const link = async (id: string | null) => {
    if (await act.run(() => api.whatsapp.linkCustomer(chat, id))) onLinked();
  };
  const create = async () => {
    const c = await act.run(() =>
      api.customers.save(null, {
        name,
        phone,
        whatsapp: phone,
        address: address || null,
        area: area || null,
        active: true,
      }),
    );
    if (c) await link(c.customer_id);
  };
  return (
    <Drawer title={t("Link or create customer")} onClose={onClose}>
      <div className="col gap-12">
        {!creating ? (
          <>
            <TextInput label={t("Find customer")} value={q} onChange={(e) => setQ(e.target.value)} autoFocus />
            {(found.data ?? []).map((c) => (
              <button key={c.customer_id} type="button" className="send-row" onClick={() => void link(c.customer_id)}>
                <span className="grow" dir="auto">
                  {c.name}
                </span>
                <span className="tiny muted">{[c.area, c.phone].filter(Boolean).join(" · ")}</span>
              </button>
            ))}
            <div className="row gap-8">
              <Button variant="primary" onClick={() => setCreating(true)}>
                {t("Create customer")}
              </Button>
              <Button variant="ghost" onClick={() => void link(null)}>
                {t("Unlink")}
              </Button>
            </div>
          </>
        ) : (
          <>
            <TextInput label={t("Name")} value={name} onChange={(e) => setName(e.target.value)} autoFocus />
            <div className="small muted num">{phone}</div>
            <TextInput label={t("Address")} value={address} onChange={(e) => setAddress(e.target.value)} />
            <AreaPicker value={area} onChange={setArea} />
            <Button variant="primary" size="lg" disabled={!name.trim()} loading={act.busy} onClick={create}>
              {t("Save and link")}
            </Button>
          </>
        )}
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      </div>
    </Drawer>
  );
}

function WaThreadView({ chat, onRead }: { chat: WaConversation; onRead: () => void }) {
  const toast = useToast();
  const { data, reload } = useLoad(() => api.whatsapp.thread(chat.chat), [chat.chat]);
  const [text, setText] = useState("");
  const [images, setImages] = useState<Record<number, string>>({});
  const act = useAction();
  const ordersOn = useFeature("orders.digital");
  const docsOn = useFeature("ocr.supplier_invoices");
  const navigate = useNavigate();
  const { has, setMode } = useSession();
  const [draft, setDraft] = useState<DigitalOrder | null>(null);
  const ctx = useLoad(() => api.whatsapp.threadContext(chat.chat), [chat.chat]);
  const [view, setView] = useState<"chat" | "tickets" | "customer">("chat");
  const [ticket, setTicket] = useState<string | null>(null);
  const [linking, setLinking] = useState(false);
  const [askTicket, setAskTicket] = useState(false);
  const person = ctx.data?.customer ?? null;
  const newTicket = async () => {
    const o = await act.run(() =>
      api.orders.save(null, {
        channel: "whatsapp",
        customer_id: person?.customer_id ?? null,
        phone: ctx.data?.phone ?? chat.phone ?? null,
        address: person?.address ?? null,
        delivery_wanted: true,
        lines: [],
      }),
    );
    setAskTicket(false);
    if (o) setDraft(o);
  };
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
      <div className="wa-head" data-testid="wa-thread-head">
        <div className="grow">
          <h3 dir="auto">{person?.name ?? ctx.data?.push_name ?? chat.name ?? t("Unknown person")}</h3>
          <div className="tiny muted">
            {[
              person?.area,
              ctx.data?.phone ?? chat.phone,
              person
                ? ctx.data?.match === "linked"
                  ? t("Linked by hand")
                  : t("Matched by number")
                : t("Not a customer yet"),
            ]
              .filter(Boolean)
              .join(" · ")}
          </div>
        </div>
        {ctx.data?.last_ticket ? (
          <button type="button" className="wa-last" onClick={() => setTicket(ctx.data!.last_ticket!.ticket_id)}>
            <span className="tiny muted">{t("Last ticket")}</span>
            <span className="num">{ctx.data.last_ticket.number}</span>
            <PayChip state={ctx.data.last_ticket.pay_state} />
          </button>
        ) : null}
        <div className="row gap-8">
          {ordersOn && has("orders.manage") ? (
            <Button variant="primary" onClick={() => setAskTicket(true)} data-testid="wa-new-ticket">
              {t("New ticket")}
            </Button>
          ) : null}
          {!ordersOn && person && has("pos.sell") ? (
            <Button
              variant="primary"
              onClick={async () => {
                if (await act.run(() => api.pos.setCustomer(person.customer_id))) {
                  toast("success", t("{0} is on the till's sale. Scan the items, then PAY and Send.", person.name));
                  setMode("cashier");
                }
              }}
              data-testid="wa-start-sale"
            >
              {t("Start till sale")}
            </Button>
          ) : null}
          {person ? (
            <Link className="btn" to={`/admin/customers/${person.customer_id}`}>
              {t("Open customer")}
            </Link>
          ) : null}
          <Button onClick={() => setLinking(true)} data-testid="wa-link">
            {person ? t("Change link") : t("Link or create customer")}
          </Button>
        </div>
      </div>
      <Tabs
        tabs={[
          { key: "chat", label: t("Chat") },
          { key: "tickets", label: t("Tickets") },
          { key: "customer", label: t("Customer") },
        ]}
        value={view}
        onChange={setView}
      />
      {view === "tickets" ? (
        <div className="col gap-8" data-testid="wa-tickets">
          {(ctx.data?.tickets ?? []).length ? (
            ctx.data!.tickets.map((r) => (
              <TicketRowButton key={r.ticket_id} row={r} onOpen={(x) => setTicket(x.ticket_id)} />
            ))
          ) : (
            <div className="muted">
              {person ? t("No tickets yet.") : t("Link the chat to a customer to see their tickets.")}
            </div>
          )}
        </div>
      ) : null}
      {view === "customer" ? (
        person ? (
          <dl className="kv">
            <dt>{t("Name")}</dt>
            <dd dir="auto">{person.name}</dd>
            <dt>{t("Phone")}</dt>
            <dd className="num">{person.phone ?? "—"}</dd>
            <dt>{t("Area")}</dt>
            <dd>{person.area ?? "—"}</dd>
            <dt>{t("Address")}</dt>
            <dd dir="auto">{person.address ?? "—"}</dd>
          </dl>
        ) : (
          <div className="muted">{t("Not a customer yet.")}</div>
        )
      ) : null}
      {ticket ? (
        <TicketSheet ticketId={ticket} onClose={() => setTicket(null)} onChanged={() => void ctx.reload()} />
      ) : null}
      {askTicket ? (
        <Confirm
          title={t("New ticket")}
          confirmLabel={t("Create draft")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setAskTicket(false)}
          onConfirm={newTicket}
        >
          {t(
            "A draft ticket for {0}. You add the items and confirm it; nothing is sold until it is rung up at a till.",
            person?.name ?? ctx.data?.phone ?? "",
          )}
        </Confirm>
      ) : null}
      {linking ? (
        <LinkCustomer
          chat={chat.chat}
          phone={ctx.data?.phone ?? chat.phone ?? null}
          suggestedName={ctx.data?.push_name ?? chat.name ?? ""}
          onClose={() => setLinking(false)}
          onLinked={() => (setLinking(false), void ctx.reload())}
        />
      ) : null}
      {view === "chat" ? (
        <>
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
                  {it.m.kind === "other" && !it.m.body ? (
                    <div className="tiny">{t("Unsupported message type")}</div>
                  ) : null}
                  <div className="tiny">{formatDateTime(it.at)}</div>
                  {docsOn && has("ocr.scan") && (it.m.kind === "image" || it.m.kind === "document") ? (
                    <Button
                      size="sm"
                      icon={<FileScan size={14} />}
                      onClick={async () => {
                        const seq = (it.m as { seq: number }).seq;
                        const d = await act.run(() => api.docs.fromInbox(seq));
                        if (d) navigate(`/admin/invoice-scan/${d.scan_id}`);
                      }}
                    >
                      {t("Read as supplier document")}
                    </Button>
                  ) : null}
                  {ordersOn && has("orders.manage") && (it.m.body || it.m.caption) ? (
                    <Button
                      size="sm"
                      onClick={async () => {
                        const seq = (it.m as { seq: number }).seq;
                        const o = await act.run(() => api.orders.fromInbox(seq));
                        if (o) setDraft(o);
                      }}
                    >
                      {t("New ticket from this message")}
                    </Button>
                  ) : null}
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
        </>
      ) : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {draft ? (
        <OrderEditor
          order={draft}
          onClose={() => setDraft(null)}
          onSaved={() => {
            setDraft(null);
            toast("success", t("Draft ticket saved. It waits on the Send rail until it is rung up."));
            void ctx.reload();
          }}
        />
      ) : null}
      {has("ai.use") && has("whatsapp.send") ? (
        <div className="row">
          <Button
            variant="ghost"
            icon={<Sparkles size={16} />}
            loading={act.busy}
            data-testid="wa-draft"
            onClick={async () => {
              const r = await act.run(() => api.whatsapp.draftReply(chat.chat, text.trim() || null));
              if (r) {
                setText(r.text);
                toast(
                  "info",
                  r.source === "ai"
                    ? t("AI draft ready. Edit it, then press Send.")
                    : t("Template draft ready. Edit it, then press Send."),
                );
              }
            }}
          >
            {t("Draft reply")}
          </Button>
          <span className="tiny muted">
            {t(
              "Type an instruction first (for example: say it arrives at 6) or leave the box empty. Nothing is sent until you press Send.",
            )}
          </span>
        </div>
      ) : null}
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
  auto_delivery_notice: boolean;
  receipt: { en: string; ar: string };
  received: { en: string; ar: string };
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
  const tpl = (
    k: "receipt" | "received" | "dispatch" | "delivered" | "reminder" | "payment_ack",
    label: string,
    vars: string,
  ) => (
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
      <Checkbox
        label={t(
          "Send the order-received, on-the-way and delivered notices by themselves (when a Send order is taken, marked Out or Delivered)",
        )}
        checked={!!data.auto_delivery_notice}
        disabled={!editable}
        onChange={(x) => setData({ ...data, auto_delivery_notice: x })}
      />
      {tpl("receipt", t("Receipt"), "{business} {customer} {receipt} {total} {date}")}
      {tpl("received", t("Order received"), "{business} {customer} {ticket} {delivery} {total} {address} {area}")}
      {tpl(
        "dispatch",
        t("Out for delivery"),
        "{business} {customer} {ticket} {delivery} {amount} {total} {address} {area}",
      )}
      {tpl("delivered", t("Delivered"), "{business} {customer} {ticket} {delivery} {amount} {total} {address} {area}")}
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
  kind: "receipt" | "received" | "dispatch" | "delivered" | "reminder";
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
      : kind === "received"
        ? t("Send order received")
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
          <PaymentComparison r={r} />
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
  imported: () => t("Reading"),
  read: () => t("Read"),
  review: () => t("Ready for review"),
  confirmed: () => t("Draft order created"),
  rejected: () => t("Rejected"),
  failed: () => t("OCR failed"),
};

type DocTab = "documents" | "drafts" | "invoices";

export function InvoiceScanPage() {
  const navigate = useNavigate();
  const { has } = useSession();
  const [tab, setTab] = useState<DocTab>("documents");
  const { data, loading, error, reload } = useLoad(() => api.invoiceScan.list(), []);
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
        title={t("Supplier documents")}
        subtitle={t(
          "Read supplier invoices, credit notes and delivery notes (photo, scan or PDF), check every value against the page, then create drafts. Nothing changes stock until a person posts a receiving draft.",
        )}
        actions={
          has("ocr.scan") ? (
            <Button variant="primary" icon={<FileScan size={16} />} onClick={() => setUpload(true)}>
              {t("Add document")}
            </Button>
          ) : null
        }
      />
      <FeatureGate feature="ocr.supplier_invoices">
        <div className="col gap-16">
          <Tabs<DocTab>
            value={tab}
            onChange={setTab}
            tabs={[
              { key: "documents", label: t("Documents") },
              { key: "drafts", label: t("Receiving drafts") },
              { key: "invoices", label: t("Supplier invoices") },
            ]}
          />
          {tab === "documents" ? (
            <>
              <DocMetricsStrip />
              {error ? <Banner tone="danger">{error}</Banner> : null}
              <DataTable<InvoiceScan>
                rows={data}
                loading={loading}
                rowKey={(r) => r.scan_id}
                onRowClick={(r) => navigate(`/admin/invoice-scan/${r.scan_id}`)}
                empty={<div className="empty">{t("No documents yet.")}</div>}
                columns={[
                  { key: "n", label: t("Document"), render: (r) => r.scan_number },
                  {
                    key: "at",
                    label: t("Added"),
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
            </>
          ) : tab === "drafts" ? (
            <ReceivingDraftsPanel />
          ) : (
            <SupplierInvoicesPanel />
          )}
        </div>
        {upload ? (
          <ScanUpload
            onClose={() => setUpload(false)}
            onDone={(id) => (setUpload(false), navigate(`/admin/invoice-scan/${id}`))}
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
      title={t("Add document")}
      confirmLabel={t("Upload and read")}
      busy={act.busy}
      error={act.error}
      onCancel={onClose}
      onConfirm={async () => {
        if (!file) return act.setError(t("Choose a file."));
        const r = await act.run(async () =>
          api.docs.import({
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
          accept="image/*,application/pdf,.pdf"
          aria-label={t("Document file")}
          onChange={(e) => setFile(e.target.files?.[0] ?? null)}
        />
        <div className="tiny">
          {t(
            "A clear photo, a scan (PNG, JPG, TIFF) or a PDF, up to a few pages. The original is kept on this computer only.",
          )}
        </div>
        <SupplierSelect value={supplier} onChange={setSupplier} />
      </div>
    </Confirm>
  );
}

export function InlineNumber({
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

export function ProductPick({
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
