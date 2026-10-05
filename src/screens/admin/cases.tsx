// Cases: something to look into, with the facts, a status and a history that
// is never changed. First kind: a drawer that did not match the expected
// cash. The page states facts ("Drawer is BHD 6.250 short"), never blame.
import { useState } from "react";
import { Paperclip } from "lucide-react";
import { api } from "../../api";
import type { CaseDetail, CaseRow, CaseStatus } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { Banner, Button, Chip, Empty, Field, PageHeader, Skeleton, Tabs } from "../../components/ui";
import { DataTable, Drawer, downloadBase64, useAction, useLoad } from "./common";
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

type Filter = "open" | "closed" | "all";

export function CasesPage() {
  const [filter, setFilter] = useState<Filter>("open");
  const list = useLoad(() => api.cases.list(filter), [filter]);
  const [open, setOpen] = useState<string | null>(null);
  return (
    <div>
      <PageHeader
        title={t("Cases")}
        subtitle={t(
          "Things to look into, such as a drawer that did not match. Each case keeps its facts and every step taken, permanently.",
        )}
      />
      <Tabs<Filter>
        value={filter}
        onChange={setFilter}
        tabs={[
          { key: "open", label: t("Open") },
          { key: "closed", label: t("Finished") },
          { key: "all", label: t("All") },
        ]}
      />
      <div style={{ marginTop: 16 }}>
        {list.error ? <Banner tone="danger">{list.error}</Banner> : null}
        <DataTable<CaseRow>
          rows={list.data}
          loading={list.loading}
          rowKey={(r) => r.case_id}
          onRowClick={(r) => setOpen(r.case_id)}
          empty={
            <Empty title={filter === "open" ? t("Nothing to look into") : t("No cases")}>
              {t("A drawer that differs from the expected cash by more than the setting opens a case here.")}
            </Empty>
          }
          columns={[
            { key: "n", label: t("Case"), render: (r) => <span dir="ltr">{r.case_number}</span> },
            { key: "t", label: t("What"), render: (r) => <span dir="auto">{tb(r.title)}</span> },
            {
              key: "w",
              label: t("Register"),
              render: (r) => r.facts.register_name ?? r.facts.device_name ?? "",
            },
            { key: "c", label: t("Cashier"), render: (r) => r.facts.cashier_name ?? "" },
            {
              key: "d",
              label: t("Trading day"),
              render: (r) => (r.facts.business_date ? formatDate(r.facts.business_date) : ""),
              sort: (r) => r.facts.business_date ?? "",
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
      </div>
      {open ? <CaseDrawer id={open} onClose={() => setOpen(null)} onChanged={() => void list.reload()} /> : null}
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
  const [outcome, setOutcome] = useState("counting_error");
  const step = async (action: "acknowledge" | "start" | "resolve" | "dismiss" | "note" | "assign", extra = {}) => {
    const r = await act.run(() =>
      api.cases.act({
        case_id: id,
        action,
        note: note.trim() || null,
        resolution_code: action === "resolve" ? outcome : null,
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
          <div className="card card-pad">
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
                          ? t("Added a file")
                          : t("Note")}
                  {e.evidence?.resolution_code
                    ? ` (${OUTCOMES.find((o) => o[0] === e.evidence?.resolution_code)?.[1]() ?? ""})`
                    : ""}
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
                    <select className="select" value={outcome} onChange={(e) => setOutcome(e.target.value)}>
                      {OUTCOMES.map(([k, l]) => (
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
              {t("Finished by {0} on {1}.", data.resolved_by_name ?? "—", formatDateTime(data.resolved_at))}{" "}
              {data.resolution_note ? <span dir="auto">{data.resolution_note}</span> : null}
            </Banner>
          ) : null}
        </div>
      )}
    </Drawer>
  );
}
