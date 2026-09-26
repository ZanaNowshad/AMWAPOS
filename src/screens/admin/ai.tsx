import { useEffect, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { Bot, Plus, Send } from "lucide-react";
import { api } from "../../api";
import type { AiConversation, AiProposal, AiProvider, AiSettings, AiTestResult } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { FeatureGate } from "../../components/FeatureGate";
import { Banner, Button, Checkbox, Chip, Field, PageHeader, Skeleton, TextInput } from "../../components/ui";
import { Confirm, useAction, useLoad } from "./common";
import { formatMoney, formatQty } from "../../lib/money";
import { formatDateTime, relative } from "../../lib/time";
import { getLang, t, tb } from "../../i18n";

const TOOL_LABEL: Record<string, () => string> = {
  list_reports: () => t("Listed reports"),
  run_report: () => t("Ran a report"),
  search_products: () => t("Searched products"),
  product_details: () => t("Opened a product"),
  low_stock: () => t("Checked low stock"),
  recent_whatsapp_messages: () => t("Read WhatsApp messages"),
  invoice_scan_text: () => t("Read an invoice scan"),
  propose_price_change: () => t("Proposed a price change"),
  propose_stock_adjustment: () => t("Proposed a stock correction"),
  propose_purchase_order: () => t("Proposed a purchase order"),
};

const n = (v: unknown) => (typeof v === "number" ? v : null);

function ProposalPreview({ p }: { p: AiProposal }) {
  const pv = p.preview;
  if (p.kind === "price_change") {
    return (
      <dl className="kv">
        <dt>{t("Product")}</dt>
        <dd>{String(pv.product)}</dd>
        <dt>{t("Current price")}</dt>
        <dd>{formatMoney(n(pv.old_price_minor))}</dd>
        <dt>{t("New price")}</dt>
        <dd>
          <strong>{formatMoney(n(pv.new_price_minor))}</strong>
        </dd>
        <dt>{t("Cost")}</dt>
        <dd>{formatMoney(n(pv.cost_minor))}</dd>
      </dl>
    );
  }
  if (p.kind === "stock_adjustment") {
    return (
      <dl className="kv">
        <dt>{t("Product")}</dt>
        <dd>{String(pv.product)}</dd>
        <dt>{t("Stock now")}</dt>
        <dd>{formatQty(n(pv.old_stock_milli) ?? 0)}</dd>
        <dt>{t("Stock after")}</dt>
        <dd>
          <strong>{formatQty(n(pv.new_stock_milli) ?? 0)}</strong>
        </dd>
        <dt>{t("Value change")}</dt>
        <dd>{formatMoney(n(pv.value_change_minor))}</dd>
      </dl>
    );
  }
  const lines =
    (pv.lines as { product: string; qty_milli: number; unit_cost_minor: number; line_total_minor: number }[]) ?? [];
  return (
    <div className="col gap-8">
      <div>
        {t("Supplier")}: <strong>{String(pv.supplier)}</strong>
      </div>
      <table className="table">
        <tbody>
          {lines.map((l, i) => (
            <tr key={i}>
              <td>{l.product}</td>
              <td className="num">{formatQty(l.qty_milli)}</td>
              <td className="num">{formatMoney(l.unit_cost_minor)}</td>
              <td className="num">{formatMoney(l.line_total_minor)}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <div>
        {t("Total")}: <strong>{formatMoney(n(pv.total_minor))}</strong>
      </div>
    </div>
  );
}

const STATUS_LABEL: Record<string, () => string> = {
  proposed: () => t("Waiting for your decision"),
  executing: () => t("Running"),
  executed: () => t("Done"),
  rejected: () => t("Rejected"),
  failed: () => t("Failed"),
  undone: () => t("Undone"),
  expired: () => t("Expired"),
};

const KIND_LABEL: Record<string, () => string> = {
  price_change: () => t("Price change"),
  stock_adjustment: () => t("Stock correction"),
  purchase_order: () => t("Draft purchase order"),
};

function ProposalCard({ p, onChanged }: { p: AiProposal; onChanged: () => void }) {
  const { has } = useSession();
  const toast = useToast();
  const act = useAction();
  const [confirm, setConfirm] = useState(false);
  const riskTone = p.risk === "high" ? "danger" : p.risk === "medium" ? "warning" : "success";
  const riskLabel = p.risk === "high" ? t("High risk") : p.risk === "medium" ? t("Medium risk") : t("Low risk");
  return (
    <div className="card card-pad col gap-8" data-testid="ai-proposal">
      <div className="row">
        <strong className="grow">
          {p.proposal_number} · {KIND_LABEL[p.kind]?.()}
        </strong>
        <Chip tone={riskTone}>{riskLabel}</Chip>
        <Chip>{STATUS_LABEL[p.status]?.() ?? p.status}</Chip>
      </div>
      <ProposalPreview p={p} />
      {p.risk_reasons.length ? (
        <ul className="small" style={{ margin: 0, paddingInlineStart: 18 }}>
          {p.risk_reasons.map((r) => (
            <li key={r}>{tb(r)}</li>
          ))}
        </ul>
      ) : null}
      {p.error ? <Banner tone="danger">{tb(p.error)}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {has("ai.mutate") ? (
        <div className="row">
          {p.status === "proposed" ? (
            <>
              <Button variant="primary" onClick={() => setConfirm(true)}>
                {t("Review and confirm")}
              </Button>
              <Button
                variant="ghost"
                loading={act.busy}
                onClick={async () => {
                  if (await act.run(() => api.ai.reject(p.proposal_id))) onChanged();
                }}
              >
                {t("Reject")}
              </Button>
            </>
          ) : null}
          {p.status === "executed" ? (
            <Button
              loading={act.busy}
              onClick={async () => {
                if (await act.run(() => api.ai.undo(p.proposal_id))) {
                  toast("info", t("Undone with a correcting record"));
                  onChanged();
                }
              }}
            >
              {t("Undo")}
            </Button>
          ) : null}
          {p.status === "executed" && p.kind === "purchase_order" && p.result?.po_id ? (
            <Link to={`/admin/purchase-orders/${String(p.result.po_id)}`}>{t("Open order")}</Link>
          ) : null}
        </div>
      ) : (
        <div className="tiny">{t("A manager with permission to approve AI changes must confirm this.")}</div>
      )}
      {confirm ? (
        <Confirm
          title={t("Confirm {0}", p.proposal_number)}
          confirmLabel={t("Confirm change")}
          danger={p.risk === "high"}
          busy={act.busy}
          error={act.error}
          onCancel={() => setConfirm(false)}
          onConfirm={async () => {
            if (await act.run(() => api.ai.confirm(p.proposal_id))) {
              setConfirm(false);
              toast("success", t("Change made and recorded in the audit trail"));
              onChanged();
            }
          }}
        >
          <div className="col gap-16">
            <ProposalPreview p={p} />
            <div className="small">
              {t(
                "AMWAPOS runs this through the normal command with your permissions. It can be undone later with a correcting record; nothing is deleted.",
              )}
            </div>
          </div>
        </Confirm>
      ) : null}
    </div>
  );
}

export function AiAssistantPage() {
  const { has } = useSession();
  const status = useLoad(() => api.ai.status(), []);
  const list = useLoad(() => api.ai.conversations(), []);
  const [cid, setCid] = useState<string | null>(null);
  const [conv, setConv] = useState<AiConversation | null>(null);
  // Other screens may prefill a question (e.g. end of day); it is never sent automatically.
  const [search] = useSearchParams();
  const [text, setText] = useState(() => search.get("q") ?? "");
  const act = useAction();
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!cid) return setConv(null);
    void api.ai.conversation(cid).then(setConv, () => setConv(null));
  }, [cid]);
  useEffect(() => end.current?.scrollIntoView({ block: "end" }), [conv]);
  const st = status.data;
  const reloadConv = async () => {
    if (conv) setConv(await api.ai.conversation(conv.conversation_id));
    void list.reload();
  };
  return (
    <div>
      <PageHeader
        title={t("AI Assistant")}
        subtitle={t("Ask about sales, stock, margins and purchasing. The assistant reads data with your permissions.")}
      />
      {st ? (
        <div className="row" style={{ marginBottom: 12 }} data-testid="ai-provider-chip">
          <Chip tone={st.active_provider === "fake" ? "default" : "info"}>
            {st.active_provider === "fake" ? t("Offline test model") : providerLabel(st.active_provider)} ·{" "}
            {st.model_id}
          </Chip>
        </div>
      ) : null}
      <FeatureGate feature="ai.enabled">
        {!st ? (
          <Skeleton />
        ) : !st.ready ? (
          <Banner tone="info" title={t("The assistant is not set up yet")}>
            <div className="col gap-8">
              {!st.key_configured ? <div>• {t("No AI provider key is stored.")}</div> : null}
              {st.settings.consent === false ? (
                <div>• {t("An owner has not agreed to send store data to the provider.")}</div>
              ) : null}
              {has("settings.manage") ? (
                <Link to="/admin/settings?section=ai">{t("Open Settings → AI")}</Link>
              ) : (
                <div>{t("Ask the owner to finish the setup in Settings → AI.")}</div>
              )}
            </div>
          </Banner>
        ) : (
          <div className="grid-2" style={{ gridTemplateColumns: "280px 1fr", gap: 16, alignItems: "start" }}>
            <div className="card" style={{ maxHeight: 640, overflow: "auto" }}>
              <div style={{ padding: 12 }}>
                <Button icon={<Plus size={16} />} onClick={() => (setCid(null), setConv(null))}>
                  {t("New conversation")}
                </Button>
              </div>
              {(list.data ?? []).map((c) => (
                <button
                  key={c.conversation_id}
                  className={`list-row ${cid === c.conversation_id ? "active" : ""}`}
                  style={{
                    display: "block",
                    width: "100%",
                    textAlign: "start",
                    padding: 12,
                    borderTop: "1px solid var(--border)",
                  }}
                  onClick={() => setCid(c.conversation_id)}
                >
                  <div style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{c.title}</div>
                  <div className="tiny">
                    {relative(c.updated_at)}
                    {c.open_proposals ? ` · ${t("{0} to review", c.open_proposals)}` : ""}
                  </div>
                </button>
              ))}
            </div>
            <div className="col gap-16">
              <div className="small muted">
                {st.mutations
                  ? t(
                      "The assistant may propose price, stock and purchase-order changes. Nothing changes until a person confirms.",
                    )
                  : t("Read-only: the assistant cannot change anything.")}{" "}
                {t("Provider")}: {providerLabel(st.active_provider)} · {st.model_id}
              </div>
              {conv?.untrusted_seen ? (
                <Banner tone="warning">
                  {t("This conversation read customer messages or scanned text. Check any proposal carefully.")}
                </Banner>
              ) : null}
              <div className="card card-pad col gap-16" style={{ minHeight: 320 }}>
                {!conv ? (
                  <div className="empty">
                    <Bot size={28} />
                    <div>{t("Try: “Which products are running low?” or “What were last week's top sellers?”")}</div>
                  </div>
                ) : (
                  conv.messages.map((m, i) => (
                    <div
                      key={i}
                      className={`bubble ${m.role === "user" ? "out" : "in"}`}
                      style={{ alignSelf: m.role === "user" ? "flex-end" : "flex-start", maxWidth: "85%" }}
                    >
                      {m.tools.length ? (
                        <div className="tiny">{m.tools.map((x) => TOOL_LABEL[x]?.() ?? x).join(" · ")}</div>
                      ) : null}
                      {m.text ? <div style={{ whiteSpace: "pre-wrap" }}>{m.text}</div> : null}
                      {m.stop_reason === "refusal" ? (
                        <div className="tiny">{t("The provider declined to answer this request.")}</div>
                      ) : null}
                      {m.stop_reason === "max_tokens" ? (
                        <div className="tiny">{t("The answer was cut short.")}</div>
                      ) : null}
                      <div className="tiny">{formatDateTime(m.at)}</div>
                    </div>
                  ))
                )}
                {conv?.proposals.map((p) => (
                  <ProposalCard key={p.proposal_id} p={p} onChanged={() => void reloadConv()} />
                ))}
                <div ref={end} />
              </div>
              {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
              <div className="row">
                <textarea
                  className="input grow"
                  rows={2}
                  maxLength={4000}
                  value={text}
                  aria-label={t("Question")}
                  placeholder={t("Ask a question")}
                  onChange={(e) => setText(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      (document.getElementById("ai-send") as HTMLButtonElement | null)?.click();
                    }
                  }}
                />
                <Button
                  id="ai-send"
                  variant="primary"
                  icon={<Send size={16} />}
                  loading={act.busy}
                  disabled={!text.trim()}
                  onClick={async () => {
                    const r = await act.run(() => api.ai.ask(text, conv?.conversation_id ?? null, getLang()));
                    if (r) {
                      setText("");
                      setConv(r);
                      setCid(r.conversation_id);
                      void list.reload();
                    }
                  }}
                >
                  {t("Ask")}
                </Button>
              </div>
            </div>
          </div>
        )}
      </FeatureGate>
    </div>
  );
}

export function providerLabel(p: AiProvider): string {
  switch (p) {
    case "openai":
      return "OpenAI";
    case "anthropic":
      return "Anthropic";
    case "google":
      return "Google (Gemini)";
    case "openrouter":
      return "OpenRouter";
    case "custom":
      return t("Custom (OpenAI-compatible)");
    default:
      return t("Offline test model");
  }
}

const BASE_HINT: Record<AiProvider, string> = {
  fake: "",
  openai: "https://api.openai.com/v1",
  anthropic: "https://api.anthropic.com",
  google: "",
  openrouter: "https://openrouter.ai/api/v1",
  custom: "https://…/v1",
};

/** Settings → AI (owner only). Bring your own API key; no subscription sign-in. */
export function AiSettingsSection() {
  const toast = useToast();
  const { session } = useSession();
  const { data, setData, error } = useLoad(() => api.ai.status(), []);
  const [s, setS] = useState<AiSettings | null>(null);
  const [key, setKey] = useState("");
  const [header, setHeader] = useState("");
  const [test, setTest] = useState<AiTestResult | null>(null);
  const act = useAction();
  useEffect(() => {
    if (data && !s && data.is_owner) setS(data.settings as AiSettings);
  }, [data, s]);
  if (session?.role_id !== "role_owner") {
    return <Banner tone="info">{t("Only the owner can change the AI provider and keys.")}</Banner>;
  }
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data || !s) return <Skeleton />;
  const save = async (apiKey: string | null, headerValue: string | null) => {
    const r = await act.run(() => api.ai.configure(s, apiKey, headerValue));
    if (r) {
      setData(r);
      setS(r.settings as AiSettings);
      setKey("");
      setHeader("");
      toast("success", t("Settings saved"));
    }
  };
  const real = s.provider !== "fake";
  const models = s.list_models_cache ?? [];
  return (
    <div className="card card-pad col gap-16">
      <Banner tone="info" title={t("Use an API key, not a chat subscription")}>
        {t(
          "ChatGPT Plus, Claude Pro, Gemini Advanced, and Codex logins do not work here. Create an API key at platform.openai.com, console.anthropic.com, aistudio.google.com, or openrouter.ai. For other servers, choose Custom and enter a base URL that speaks OpenAI Chat Completions.",
        )}
      </Banner>
      {!data.enabled ? (
        <Banner tone="info">
          {t("The AI assistant module is switched off.")}{" "}
          <Link to="/admin/settings?section=features">{t("Settings → Features")}</Link>
        </Banner>
      ) : null}
      <div className="row">
        <span className="small muted">{t("Active now")}:</span>
        <Chip tone={data.active_provider === "fake" ? "default" : "info"}>
          {providerLabel(data.active_provider)} · {data.model_id}
        </Chip>
        {real && !data.key_configured ? (
          <span className="tiny">{t("No key stored: the offline test model answers.")}</span>
        ) : null}
      </div>
      <div className="form-grid">
        <Field label={t("Provider")}>
          <select
            className="select"
            value={s.provider}
            data-testid="ai-provider"
            onChange={(e) => {
              const provider = e.target.value as AiProvider;
              setTest(null);
              setS({
                ...s,
                provider,
                model_id: provider === "fake" ? "fake-local" : provider === s.provider ? s.model_id : "",
                base_url: provider === s.provider ? s.base_url : "",
                list_models_cache: provider === s.provider ? s.list_models_cache : [],
              });
            }}
          >
            {(["fake", "openai", "anthropic", "google", "openrouter", "custom"] as AiProvider[]).map((p) => (
              <option key={p} value={p}>
                {p === "fake" ? t("Offline test model (no key, nothing sent)") : providerLabel(p)}
              </option>
            ))}
          </select>
        </Field>
        {real ? (
          <Field label={t("Model")} hint={t("Pick from the list or type any model id.")}>
            <div className="col gap-8">
              {models.length ? (
                <select
                  className="select"
                  aria-label={t("Models from the provider")}
                  value={models.includes(s.model_id) ? s.model_id : ""}
                  onChange={(e) => e.target.value && setS({ ...s, model_id: e.target.value })}
                >
                  <option value="">{t("Choose a model…")}</option>
                  {models.map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
                </select>
              ) : null}
              <input
                className="input"
                dir="ltr"
                aria-label={t("Model id")}
                placeholder={t("Model id")}
                value={s.model_id}
                onChange={(e) => setS({ ...s, model_id: e.target.value })}
              />
            </div>
          </Field>
        ) : null}
        {real && s.provider !== "google" ? (
          <TextInput
            label={s.provider === "custom" ? t("Base URL (required)") : t("Base URL (optional override)")}
            value={s.base_url}
            dir="ltr"
            placeholder={BASE_HINT[s.provider]}
            hint={
              s.provider === "custom"
                ? t("A server that speaks OpenAI Chat Completions; /v1 is added if missing.")
                : t("Leave empty for the provider's public API.")
            }
            onChange={(e) => setS({ ...s, base_url: e.target.value })}
          />
        ) : null}
        {real ? (
          <TextInput
            label={t("Maximum output tokens")}
            className="num"
            value={String(s.max_output_tokens)}
            hint={t("256 to 32000.")}
            onChange={(e) => setS({ ...s, max_output_tokens: Number(e.target.value.replace(/[^\d]/g, "")) || 0 })}
          />
        ) : null}
        {real ? (
          <TextInput
            label={t("Timeout (ms)")}
            className="num"
            value={String(s.timeout_ms)}
            hint={t("Up to 120000.")}
            onChange={(e) => setS({ ...s, timeout_ms: Number(e.target.value.replace(/[^\d]/g, "")) || 0 })}
          />
        ) : null}
      </div>
      {real ? (
        <>
          <TextInput
            label={t("API key")}
            type="password"
            autoComplete="off"
            value={key}
            data-testid="ai-key"
            placeholder={data.key_configured ? t("Stored — leave empty to keep") : ""}
            hint={t(
              "Stored in Windows Credential Manager on this computer, never in the database. It is never shown again.",
            )}
            onChange={(e) => setKey(e.target.value)}
          />
          <div className="form-grid">
            <TextInput
              label={t("Extra header name (optional)")}
              value={s.extra_header_name}
              dir="ltr"
              placeholder="X-Org-Id"
              onChange={(e) => setS({ ...s, extra_header_name: e.target.value })}
            />
            <TextInput
              label={t("Extra header value")}
              type="password"
              autoComplete="off"
              value={header}
              placeholder={data.extra_header_configured ? t("Stored — leave empty to keep") : ""}
              hint={t("Also kept in Credential Manager.")}
              onChange={(e) => setHeader(e.target.value)}
            />
          </div>
          {s.provider === "anthropic" ? (
            <Checkbox
              label={t("Retry declined requests on Anthropic's fallback model")}
              checked={s.fallbacks}
              onChange={(x) => setS({ ...s, fallbacks: x })}
            />
          ) : null}
          <div className="col gap-8">
            <Checkbox
              label={t("I agree to send store data to this AI provider")}
              checked={s.consent}
              onChange={(x) => setS({ ...s, consent: x })}
            />
            <div className="tiny" style={{ marginInlineStart: 24 }}>
              {t(
                "When someone asks a question, AMWAPOS sends the question and the results of the lookups the assistant makes (product names, prices, stock, report totals) to the provider. PINs, keys and full customer lists are never sent. Customer messages are sent only if WhatsApp is on and the assistant reads them.",
              )}
              {s.consent_at ? ` ${t("Agreed on {0}.", formatDateTime(s.consent_at))}` : ""}
            </div>
          </div>
        </>
      ) : null}
      {test ? (
        <Banner tone={test.ok ? "success" : "danger"} title={test.ok ? t("Connection works") : t("Connection failed")}>
          {test.ok
            ? t("{0} models available.", test.models ?? 0) +
              (test.model_listed === false ? ` ${t("The chosen model is not in the list.")}` : "")
            : `${test.status ? `HTTP ${test.status} · ` : ""}${tb(test.error ?? "")}`}
        </Banner>
      ) : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <div className="row wrap">
        {real && data.key_configured ? (
          <Button variant="ghost" onClick={() => void save("", null)}>
            {t("Remove key")}
          </Button>
        ) : null}
        {real ? (
          <>
            <Button
              disabled={!data.key_configured || act.busy}
              onClick={async () => {
                const r = await act.run(() => api.ai.test());
                if (r) setTest(r);
              }}
            >
              {t("Test connection")}
            </Button>
            <Button
              disabled={!data.key_configured || act.busy}
              onClick={async () => {
                const r = await act.run(() => api.ai.models());
                if (r) {
                  setS({ ...s, list_models_cache: r.models });
                  toast("success", t("{0} models loaded", r.models.length));
                }
              }}
            >
              {t("Refresh models")}
            </Button>
          </>
        ) : null}
        <Button
          variant="primary"
          className="right"
          loading={act.busy}
          onClick={() => void save(key || null, header || null)}
        >
          {t("Save")}
        </Button>
      </div>
      <div className="tiny">{t("Changes apply to the next question; no restart is needed.")}</div>
    </div>
  );
}
