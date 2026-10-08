// The Alert Centre (Wave 7): cases are the one place where something to look
// into is handled, with the facts, a status and a history that is never
// changed. A person opens some (a drawer that did not match); the system
// opens others from conditions it measured (records not saved, a till not
// seen during a shift, an overdue backup) and resolves those itself only
// after it saw the condition clear. The page states facts, never blame.
import { useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { Paperclip } from "lucide-react";
import { api } from "../../api";
import type { CaseDetail, CaseRow, CaseStatus } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { Banner, Button, Chip, Empty, Field, PageHeader, Skeleton, Tabs } from "../../components/ui";
import { DataTable, Drawer, Pager, downloadBase64, useAction, useLoad } from "./common";
import { CASE_KINDS, factLabel, kindLabel, reasonLabel, recordLabel } from "./opsLabels";
import { fileToBase64 } from "./automation";
import { formatMoney } from "../../lib/money";
import { formatDate, formatDateTime } from "../../lib/time";
import { newOperationId } from "../../lib/ids";
import { t, tb } from "../../i18n";

export function caseStage(s: CaseStatus): {
  label: string;
  tone: "default" | "warning" | "success" | "info" | "danger";
} {
  switch (s) {
    case "new":
      return { label: t("New"), tone: "warning" };
    case "acknowledged":
      return { label: t("Seen"), tone: "info" };
    case "in_progress":
      return { label: t("Looking into it"), tone: "info" };
    case "resolved":
      return { label: t("Resolved"), tone: "success" };
    default:
      return { label: t("Dismissed"), tone: "default" };
  }
}

const SEVERITY: Record<string, () => string> = {
  low: () => t("Small"),
  medium: () => t("Medium"),
  high: () => t("Large"),
};

export const OUTCOMES: [string, () => string][] = [
  ["counting_error", () => t("Counting mistake")],
  ["cash_found", () => t("Cash found")],
  ["change_error", () => t("Wrong change given")],
  ["unexplained", () => t("Not explained")],
  ["other", () => t("Other")],
];

/** What a person can record when finishing an operational case. */
export const OPERATIONAL_OUTCOMES: [string, () => string][] = [
  ["fixed", () => t("Fixed")],
  ["not_a_problem", () => t("Not a problem")],
  ["duplicate", () => t("Same as another case")],
  ["other", () => t("Other")],
];

function outcomeLabel(code: string | null | undefined): string {
  if (!code) return "";
  if (code === "recovered") return t("Recovered by itself");
  if (code === "legacy_dismissed") return t("Dismissed in the old AI inbox");
  return OUTCOMES.find((o) => o[0] === code)?.[1]() ?? OPERATIONAL_OUTCOMES.find((o) => o[0] === code)?.[1]() ?? code;
}

function sourceLabel(src: CaseRow["source"]): string {
  return src === "system" ? t("The system") : src === "legacy" ? t("Old AI inbox") : t("A person");
}

type Filter = "needs_attention" | "open" | "closed" | "all";
const PAGE = 50;

/** The Alert Centre: every operational problem is a case, whoever opened it. */
export function CasesPage() {
  const [params, setParams] = useSearchParams();
  const [filter, setFilter] = useState<Filter>("needs_attention");
  const [kind, setKind] = useState<string>("");
  const [source, setSource] = useState<string>("");
  const [offset, setOffset] = useState(0);
  const list = useLoad(
    () => api.cases.query({ status: filter, kind: kind || null, source: source || null, limit: PAGE, offset }),
    [filter, kind, source, offset],
  );
  const open = params.get("case");
  const { has } = useSession();
  const checks = useAction();
  const setOpen = (id: string | null) => {
    const p = new URLSearchParams(params);
    if (id) p.set("case", id);
    else p.delete("case");
    setParams(p, { replace: true });
  };
  return (
    <div>
      <PageHeader
        actions={
          has("cases.manage") ? (
            <Button
              loading={checks.busy}
              data-testid="alert-check-now"
              onClick={async () => {
                if (await checks.run(() => api.cases.checkNow())) void list.reload();
              }}
            >
              {t("Check now")}
            </Button>
          ) : null
        }
        title={t("Alert Centre")}
        subtitle={t(
          "Everything that needs looking into, in one place: problems the system measured and cases people opened. Each keeps its facts and every step taken, permanently.",
        )}
      />
      <Tabs<Filter>
        value={filter}
        onChange={(f) => (setFilter(f), setOffset(0))}
        tabs={[
          { key: "needs_attention", label: t("Needs attention") },
          { key: "open", label: t("Open") },
          { key: "closed", label: t("Finished") },
          { key: "all", label: t("All") },
        ]}
      />
      <div className="row wrap gap-8" style={{ marginTop: 12 }}>
        <select
          className="select"
          style={{ width: "auto" }}
          aria-label={t("Kind")}
          value={kind}
          onChange={(e) => (setKind(e.target.value), setOffset(0))}
        >
          <option value="">{t("Every kind")}</option>
          {CASE_KINDS.map((k) => (
            <option key={k} value={k}>
              {kindLabel(k)}
            </option>
          ))}
        </select>
        <select
          className="select"
          style={{ width: "auto" }}
          aria-label={t("Opened by")}
          value={source}
          onChange={(e) => (setSource(e.target.value), setOffset(0))}
        >
          <option value="">{t("Opened by anyone")}</option>
          <option value="system">{t("The system")}</option>
          <option value="user">{t("A person")}</option>
          <option value="legacy">{t("Old AI inbox")}</option>
        </select>
      </div>
      <div style={{ marginTop: 16 }} data-testid="alert-centre">
        {list.error ? <Banner tone="danger">{list.error}</Banner> : null}
        {checks.error ? <Banner tone="danger">{checks.error}</Banner> : null}
        <DataTable<CaseRow>
          rows={list.data?.rows ?? null}
          loading={list.loading}
          rowKey={(r) => r.case_id}
          onRowClick={(r) => setOpen(r.case_id)}
          empty={
            <Empty title={filter === "closed" || filter === "all" ? t("No cases") : t("Nothing to look into")}>
              {t(
                "Records that could not be saved, tills not seen during a shift, an overdue backup, failed printing or a drawer that did not match open a case here.",
              )}
            </Empty>
          }
          columns={[
            { key: "n", label: t("Case"), render: (r) => <span dir="ltr">{r.case_number}</span> },
            {
              key: "t",
              label: t("What"),
              render: (r) => (
                <div className="col">
                  <span dir="auto">{tb(r.title)}</span>
                  <span className="tiny muted">{kindLabel(r.kind)}</span>
                </div>
              ),
            },
            {
              key: "w",
              label: t("Where"),
              render: (r) => r.device_name ?? r.facts.register_name ?? r.facts.device_name ?? "",
            },
            {
              key: "since",
              label: t("Since"),
              render: (r) => formatDateTime(r.first_seen_at ?? r.created_at),
              sort: (r) => r.first_seen_at ?? r.created_at,
            },
            {
              key: "now",
              label: t("Now"),
              render: (r) =>
                r.condition_active === null ? (
                  ""
                ) : r.condition_active ? (
                  <Chip tone="warning">{t("Still happening")}</Chip>
                ) : (
                  <Chip tone="success">{t("Cleared")}</Chip>
                ),
            },
            { key: "s", label: t("Size"), render: (r) => SEVERITY[r.severity]?.() ?? r.severity },
            {
              key: "st",
              label: t("Status"),
              render: (r) => {
                const s = caseStage(r.status);
                return <Chip tone={s.tone}>{s.label}</Chip>;
              },
            },
          ]}
        />
        {list.data && list.data.total > PAGE ? (
          <Pager total={list.data.total} limit={PAGE} offset={offset} onChange={setOffset} />
        ) : null}
      </div>
      {open ? <CaseDrawer id={open} onClose={() => setOpen(null)} onChanged={() => void list.reload()} /> : null}
    </div>
  );
}

function factValue(k: string, v: unknown): React.ReactNode {
  if (v === null || v === undefined || v === "") return "—";
  if (typeof v === "boolean") return v ? t("Yes") : t("No");
  if (k === "reason" && typeof v === "string") return reasonLabel(v).label;
  if (k === "amount_minor" && typeof v === "number") return formatMoney(v);
  if (typeof v === "string" && /^\d{4}-\d{2}-\d{2}T/.test(v)) return formatDateTime(v);
  if (Array.isArray(v)) return v.map((x) => (k === "tables" ? recordLabel(String(x)) : String(x))).join(", ");
  if (typeof v === "object") return <span className="mono tiny">{JSON.stringify(v)}</span>;
  return <span dir="auto">{String(v)}</span>;
}

/** The facts a system case recorded when it opened, and the latest measure. */
function SystemFacts({ data }: { data: CaseRow }) {
  const nav = useNavigate();
  const keys = Object.keys(data.facts).filter((k) => k !== "episode" && k !== "source");
  const latest = data.latest ?? {};
  return (
    <div className="card card-pad col gap-8" data-testid="case-system-facts">
      <h3>{t("The facts")}</h3>
      <div className="row wrap gap-8">
        <Chip>{kindLabel(data.kind)}</Chip>
        <span className="tiny">{t("Opened by {0}", sourceLabel(data.source))}</span>
        {data.device_name ? <span className="tiny">{t("Computer: {0}", data.device_name)}</span> : null}
      </div>
      {data.condition_active !== null ? (
        <Banner tone={data.condition_active ? "warning" : "success"}>
          {data.condition_active
            ? t("Still happening. Last checked {0}.", formatDateTime(data.last_seen_at))
            : t("The system no longer sees this. If it stays clear, the case resolves itself.")}
          {data.occurrences > 1
            ? " " + t("It has come back {0} times while this case was open.", data.occurrences - 1)
            : ""}
        </Banner>
      ) : null}
      <table className="table">
        <thead>
          <tr>
            <th />
            <th className="num">{t("When opened")}</th>
            {data.latest ? <th className="num">{t("Latest")}</th> : null}
          </tr>
        </thead>
        <tbody>
          {keys.map((k) => (
            <tr key={k}>
              <td className="tiny">{factLabel(k)}</td>
              <td className="num">{factValue(k, data.facts[k])}</td>
              {data.latest ? <td className="num">{factValue(k, latest[k])}</td> : null}
            </tr>
          ))}
        </tbody>
      </table>
      {data.link && data.link !== "/admin/cases" ? (
        <div>
          <Button variant="primary" onClick={() => nav(data.link!)} data-testid="case-go-fix">
            {t("Open the screen that fixes this")}
          </Button>
        </div>
      ) : null}
    </div>
  );
}

function Fact({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <tr>
      <td className="tiny">{label}</td>
      <td className="num">{value}</td>
    </tr>
  );
}

export function CaseDrawer({ id, onClose, onChanged }: { id: string; onClose: () => void; onChanged: () => void }) {
  const { has } = useSession();
  const toast = useToast();
  const { data, error, setData } = useLoad<CaseDetail>(() => api.cases.get(id), [id]);
  const users = useLoad(() => (has("users.manage") ? api.users.list() : Promise.resolve([])), []);
  const act = useAction();
  const [note, setNote] = useState("");
  const [outcome, setOutcome] = useState<string | null>(null);
  const step = async (action: "acknowledge" | "start" | "resolve" | "dismiss" | "note" | "assign", extra = {}) => {
    const r = await act.run(() =>
      api.cases.act({
        case_id: id,
        action,
        note: note.trim() || null,
        resolution_code: action === "resolve" || action === "dismiss" ? chosenOutcome : null,
        operation_id: newOperationId(),
        ...extra,
      }),
    );
    if (r) {
      setData(r);
      setNote("");
      onChanged();
      toast("success", t("Saved"));
    }
  };
  const f = data?.facts;
  const finished = data && (data.status === "resolved" || data.status === "dismissed");
  const isCash = data?.kind === "cash_variance";
  const outcomes = isCash ? OUTCOMES : OPERATIONAL_OUTCOMES;
  const chosenOutcome = outcome ?? outcomes[0][0];
  return (
    <Drawer wide title={data ? `${data.case_number} · ${tb(data.title)}` : t("Case")} onClose={onClose}>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {!data || !f ? (
        <Skeleton rows={6} />
      ) : (
        <div className="col gap-16" data-testid="case-drawer">
          <div className="row wrap gap-8">
            <Chip tone={caseStage(data.status).tone}>{caseStage(data.status).label}</Chip>
            <Chip>{SEVERITY[data.severity]?.()}</Chip>
            {data.assignee_name ? <span className="tiny">{t("With {0}", data.assignee_name)}</span> : null}
          </div>
          {!isCash ? <SystemFacts data={data} /> : null}
          <div className="card card-pad" hidden={!isCash}>
            <h3>{t("The facts")}</h3>
            <table className="table">
              <tbody>
                <Fact label={t("Shift")} value={<span dir="ltr">{f.shift_number}</span>} />
                <Fact label={t("Trading day")} value={f.business_date ? formatDate(f.business_date) : ""} />
                <Fact label={t("Register")} value={f.register_name ?? f.device_name ?? "—"} />
                <Fact label={t("Drawer")} value={f.drawer_name ?? "—"} />
                <Fact label={t("Cashier")} value={f.cashier_name ?? ""} />
                <Fact label={t("Opened")} value={formatDateTime(f.opened_at)} />
                <Fact label={t("Counted")} value={formatDateTime(f.closed_at ?? null)} />
                <Fact label={t("Float")} value={formatMoney(f.opening_float_minor ?? 0)} />
                <Fact label={t("Cash sales")} value={formatMoney(f.cash_sales_minor ?? 0)} />
                <Fact label={t("Cash refunds")} value={formatMoney(-(f.cash_refunds_minor ?? 0))} />
                <Fact label={t("Cash in")} value={formatMoney(f.paid_in_minor ?? 0)} />
                <Fact label={t("Cash out")} value={formatMoney(-(f.paid_out_minor ?? 0))} />
                <Fact label={t("Safe drops")} value={formatMoney(-(f.safe_drop_minor ?? 0))} />
                <Fact label={t("Delivery collections")} value={formatMoney(f.delivery_collections_minor ?? 0)} />
                <Fact label={t("Expected cash")} value={<strong>{formatMoney(f.expected_cash_minor ?? 0)}</strong>} />
                <Fact label={t("Counted cash")} value={<strong>{formatMoney(f.counted_cash_minor ?? 0)}</strong>} />
                <Fact label={t("Difference")} value={<strong>{formatMoney(f.variance_minor ?? 0)}</strong>} />
                {f.approved_by_name ? <Fact label={t("Accepted at the count by")} value={f.approved_by_name} /> : null}
                {f.close_note ? (
                  <Fact label={t("Note at the count")} value={<span dir="auto">{f.close_note}</span>} />
                ) : null}
              </tbody>
            </table>
            {f.cash_movements?.length ? (
              <>
                <h4>{t("Cash movements in this shift")}</h4>
                <table className="table">
                  <tbody>
                    {f.cash_movements.map((m, i) => (
                      <tr key={i}>
                        <td className="nowrap tiny">{formatDateTime(m.at)}</td>
                        <td>
                          {m.kind === "paid_in"
                            ? t("Cash in")
                            : m.kind === "paid_out"
                              ? t("Cash out")
                              : m.kind === "safe_drop"
                                ? t("Safe drop")
                                : t("Drawer opened")}
                        </td>
                        <td dir="auto">{m.reason}</td>
                        <td className="num money">{formatMoney(m.amount_minor)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </>
            ) : null}
          </div>
          <div className="card card-pad col gap-8">
            <h3>{t("History")}</h3>
            <ol className="col gap-8" style={{ margin: 0, paddingInlineStart: 18 }}>
              {data.events.map((e) => (
                <li key={e.seq}>
                  <span className="tiny">{formatDateTime(e.at)}</span> · {e.user_name ?? t("The system")} ·{" "}
                  {e.kind === "created"
                    ? t("Opened")
                    : e.kind === "status" && e.to_status
                      ? caseStage(e.to_status).label
                      : e.kind === "assigned"
                        ? t("Assigned")
                        : e.kind === "evidence"
                          ? e.evidence?.file_id
                            ? t("Added a file")
                            : t("Update")
                          : t("Note")}
                  {e.evidence?.resolution_code ? ` (${outcomeLabel(e.evidence.resolution_code)})` : ""}
                  {e.note ? (
                    <div dir="auto" className="muted">
                      {e.note}
                    </div>
                  ) : null}
                  {e.evidence?.file_id ? (
                    <Button
                      size="sm"
                      variant="ghost"
                      icon={<Paperclip size={14} />}
                      onClick={async () => {
                        const file = await act.run(() => api.cases.evidence(id, e.evidence!.file_id!));
                        if (file) downloadBase64(file.file_name, file.base64, file.mime);
                      }}
                    >
                      {e.evidence.file_name}
                    </Button>
                  ) : null}
                </li>
              ))}
            </ol>
          </div>
          {!finished && has("cases.manage") ? (
            <div className="card card-pad col gap-12" data-testid="case-actions">
              <Field label={t("Note")}>
                <textarea
                  className="input"
                  rows={3}
                  dir="auto"
                  value={note}
                  onChange={(e) => setNote(e.target.value)}
                  placeholder={t("What was checked or found")}
                />
              </Field>
              <div className="row wrap gap-8">
                {data.status === "new" ? (
                  <Button loading={act.busy} onClick={() => void step("acknowledge")} data-testid="case-ack">
                    {t("Mark as seen")}
                  </Button>
                ) : null}
                {data.status !== "in_progress" ? (
                  <Button loading={act.busy} onClick={() => void step("start")}>
                    {t("Start looking into it")}
                  </Button>
                ) : null}
                <Button loading={act.busy} disabled={!note.trim()} onClick={() => void step("note")}>
                  {t("Add note")}
                </Button>
                <label className="btn">
                  <Paperclip size={16} /> {t("Add a file")}
                  <input
                    type="file"
                    hidden
                    accept="image/*,application/pdf"
                    onChange={async (e) => {
                      const file = e.target.files?.[0];
                      if (!file) return;
                      const b64 = await fileToBase64(file);
                      const r = await act.run(() => api.cases.attach(id, file.name, b64));
                      if (r) setData(r);
                    }}
                  />
                </label>
              </div>
              {users.data && users.data.length ? (
                <Field label={t("Who is looking into it")}>
                  <select
                    className="select"
                    value={data.assignee_user_id ?? ""}
                    onChange={(e) => void step("assign", { assignee_user_id: e.target.value || null })}
                  >
                    <option value="">{t("Nobody yet")}</option>
                    {users.data
                      .filter((u) => u.active)
                      .map((u) => (
                        <option key={u.user_id} value={u.user_id}>
                          {u.display_name}
                        </option>
                      ))}
                  </select>
                </Field>
              ) : null}
              {has("cases.resolve") ? (
                <div className="row wrap gap-8" style={{ alignItems: "flex-end" }}>
                  <Field label={t("What was found")}>
                    <select className="select" value={chosenOutcome} onChange={(e) => setOutcome(e.target.value)}>
                      {outcomes.map(([k, l]) => (
                        <option key={k} value={k}>
                          {l()}
                        </option>
                      ))}
                    </select>
                  </Field>
                  <Button
                    variant="primary"
                    loading={act.busy}
                    disabled={!note.trim()}
                    onClick={() => void step("resolve")}
                    data-testid="case-resolve"
                  >
                    {t("Resolve")}
                  </Button>
                  <Button loading={act.busy} disabled={!note.trim()} onClick={() => void step("dismiss")}>
                    {t("Dismiss")}
                  </Button>
                </div>
              ) : null}
              <div className="tiny">
                {t("Resolving or dismissing needs a note. A finished case cannot be changed.")}
              </div>
            </div>
          ) : null}
          {finished ? (
            <Banner tone="success">
              {data.resolved_by_name
                ? t("Finished by {0} on {1}.", data.resolved_by_name, formatDateTime(data.resolved_at))
                : t("Finished by the system on {0}.", formatDateTime(data.resolved_at))}{" "}
              {data.resolution_code ? `(${outcomeLabel(data.resolution_code)}) ` : ""}
              {data.resolution_note ? <span dir="auto">{data.resolution_note}</span> : null}
            </Banner>
          ) : null}
        </div>
      )}
    </Drawer>
  );
}
