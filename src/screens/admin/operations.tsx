// Operational control screens (Wave 7, docs/OPERATIONAL_CONTROL.md):
// * Sync problems: records that could not be saved between computers, in
//   plain words, with what can be done. Trying again uses the normal path;
//   closing without applying needs a reason. Nobody edits a record by hand.
// * Terminals: each till's health from what the hub observed. Unknown stays
//   unknown. Credential rotation and revocation live here.
import { useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { api } from "../../api";
import type { DeadLetterRow, TerminalHealthRow } from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { Banner, Button, Checkbox, Chip, Empty, Field, Modal, PageHeader, Tabs, TextInput } from "../../components/ui";
import { DataTable, Pager, useAction, useLoad } from "./common";
import { newOperationId } from "../../lib/ids";
import { formatDateTime } from "../../lib/time";
import { t } from "../../i18n";
import { REASONS, healthLabel, healthReason, reasonLabel, recordLabel } from "./opsLabels";

const MONEY_OR_STOCK = new Set([
  "sales",
  "sale_items",
  "payments",
  "refunds",
  "refund_items",
  "refund_tenders",
  "cash_events",
  "stock_movements",
  "shifts",
  "customer_ledger",
  "loyalty_ledger",
  "sale_collections",
  "rider_handovers",
  "rider_handover_items",
  "sale_voids",
  "sale_item_promotions",
  "coupon_redemptions",
  "receipt_snapshots",
]);

type DeadStatus = "open" | "closed" | "all";
const PAGE = 50;

function resolutionLabel(r: DeadLetterRow): string {
  switch (r.resolution) {
    case "applied":
      return t("Tried again and saved");
    case "recovered":
      return t("Saved later by itself");
    case "settled_on_hub":
      return t("Settled on the hub");
    case "closed_without_applying":
      return t("Closed without applying");
    default:
      return r.status === "open" ? "" : t("Finished before this update");
  }
}

export function SyncReconciliationPage() {
  const [params] = useSearchParams();
  const toast = useToast();
  const [status, setStatus] = useState<DeadStatus>("open");
  const [origin, setOrigin] = useState<string>(params.get("origin") ?? "");
  const [reason, setReason] = useState<string>("");
  const [offset, setOffset] = useState(0);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [closing, setClosing] = useState<DeadLetterRow | null>(null);
  const [detail, setDetail] = useState<DeadLetterRow | null>(null);
  const act = useAction();
  const page = useLoad(
    () => api.sync.deadLetters({ status, origin: origin || null, reason_code: reason || null, limit: PAGE, offset }),
    [status, origin, reason, offset],
  );
  const reload = () => (setSelected(new Set()), void page.reload());
  const canAct = page.data?.can_act ?? false;
  const retryOne = async (r: DeadLetterRow) => {
    const out = await act.run(() => api.sync.retryDeadLetter(r.dead_id, newOperationId()));
    if (!out) return;
    const msg: Record<string, string> = {
      applied: t("Saved."),
      failed: t("Still could not be saved. The reason is updated."),
      superseded: t("A newer version was saved since; this one was not applied."),
      not_retryable: t("Trying again cannot help this one."),
      already_finished: t("This one was already finished."),
      not_here: t("This one is handled on the hub."),
    };
    toast(out.outcome === "applied" ? "success" : "info", msg[out.outcome] ?? out.outcome);
    reload();
  };
  const retryMany = async () => {
    const out = await act.run(() =>
      api.sync.retryDeadLetters(
        selected.size
          ? { dead_ids: [...selected], operation_id: newOperationId() }
          : { origin: origin || null, reason_code: reason || null, operation_id: newOperationId() },
      ),
    );
    if (!out) return;
    toast(
      "info",
      t(
        "{0} could be tried again, {1} could not. Saved: {2}. Still failing: {3}. Newer version kept: {4}.",
        out.eligible,
        out.ineligible,
        out.applied,
        out.failed,
        out.superseded,
      ) + (out.remaining ? " " + t("{0} more are left for the next run.", out.remaining) : ""),
    );
    reload();
  };
  const groups = page.data?.groups ?? [];
  const origins = [...new Map(groups.map((g) => [g.origin_id ?? "", g.origin ?? t("This computer")])).entries()];
  return (
    <div>
      <PageHeader
        title={t("Sync problems")}
        subtitle={t(
          "Records that could not be saved between computers. Nothing here is edited by hand: a record is tried again the normal way, or closed without applying it with a reason.",
        )}
        actions={
          canAct && status === "open" ? (
            <Button loading={act.busy} onClick={() => void retryMany()} data-testid="sync-retry-many">
              {selected.size ? t("Try the selected again ({0})", selected.size) : t("Try everything shown again")}
            </Button>
          ) : null
        }
      />
      {page.data && !canAct ? (
        <Banner tone="info">{t("This list is read-only here. Sync problems are handled on the hub.")}</Banner>
      ) : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {groups.length ? (
        <div className="row wrap gap-8" style={{ marginBottom: 12 }} data-testid="sync-groups">
          {groups.map((g, i) => (
            <button
              key={i}
              className="card card-pad"
              style={{ textAlign: "start", cursor: "pointer" }}
              onClick={() => (setOrigin(g.origin_id ?? ""), setReason(g.reason_code), setOffset(0))}
            >
              <div className="tiny muted">{g.origin ?? t("This computer")}</div>
              <div>
                <strong>{g.count}</strong> · {reasonLabel(g.reason_code).label}
              </div>
              <div className="tiny">
                {g.retryable === g.count ? t("Can be tried again") : t("{0} can be tried again", g.retryable)}
              </div>
            </button>
          ))}
        </div>
      ) : null}
      <Tabs<DeadStatus>
        value={status}
        onChange={(s) => (setStatus(s), setOffset(0), setSelected(new Set()))}
        tabs={[
          { key: "open", label: t("Waiting") },
          { key: "closed", label: t("Finished") },
          { key: "all", label: t("All") },
        ]}
      />
      <div className="row wrap gap-8" style={{ margin: "12px 0" }}>
        <select
          className="select"
          style={{ width: "auto" }}
          aria-label={t("From")}
          value={origin}
          onChange={(e) => (setOrigin(e.target.value), setOffset(0))}
        >
          <option value="">{t("From any computer")}</option>
          {origins.map(([id, name]) => (
            <option key={id} value={id}>
              {name}
            </option>
          ))}
        </select>
        <select
          className="select"
          style={{ width: "auto" }}
          aria-label={t("Reason")}
          value={reason}
          onChange={(e) => (setReason(e.target.value), setOffset(0))}
        >
          <option value="">{t("Any reason")}</option>
          {REASONS.map((r) => (
            <option key={r} value={r}>
              {reasonLabel(r).label}
            </option>
          ))}
        </select>
      </div>
      {page.error ? <Banner tone="danger">{page.error}</Banner> : null}
      <div data-testid="sync-problems">
        <DataTable<DeadLetterRow>
          rows={page.data?.rows ?? null}
          loading={page.loading}
          rowKey={(r) => r.dead_id}
          onRowClick={setDetail}
          selectable={canAct && status === "open"}
          selected={selected}
          onSelect={setSelected}
          empty={
            <Empty title={status === "open" ? t("Every record was saved") : t("Nothing here")}>
              {t("When a record cannot be saved between computers it waits here until it is handled.")}
            </Empty>
          }
          columns={[
            { key: "w", label: t("When"), render: (r) => formatDateTime(r.created_at), sort: (r) => r.created_at },
            { key: "f", label: t("From"), render: (r) => r.origin ?? t("This computer") },
            {
              key: "r",
              label: t("Record"),
              render: (r) => (
                <span>
                  {recordLabel(r.table)} {r.record_ref ? <span dir="ltr">{r.record_ref}</span> : null}
                </span>
              ),
            },
            { key: "why", label: t("Why"), render: (r) => reasonLabel(r.reason_code).label },
            { key: "n", label: t("Tries"), render: (r) => r.attempts },
            {
              key: "s",
              label: t("Status"),
              render: (r) =>
                r.status === "open" ? (
                  <Chip tone="warning">{t("Waiting")}</Chip>
                ) : (
                  <Chip tone={r.status === "closed" ? "default" : "success"}>{resolutionLabel(r)}</Chip>
                ),
            },
            {
              key: "a",
              label: "",
              render: (r) =>
                canAct && r.status === "open" ? (
                  <div className="row gap-4" onClick={(e) => e.stopPropagation()}>
                    {r.can_retry ? (
                      <Button size="sm" loading={act.busy} onClick={() => void retryOne(r)}>
                        {t("Try again")}
                      </Button>
                    ) : null}
                    <Button size="sm" variant="ghost" onClick={() => setClosing(r)}>
                      {t("Close…")}
                    </Button>
                  </div>
                ) : null,
            },
          ]}
        />
        {page.data && page.data.total > PAGE ? (
          <Pager total={page.data.total} limit={PAGE} offset={offset} onChange={setOffset} />
        ) : null}
      </div>
      {detail ? <DeadLetterDetail row={detail} onClose={() => setDetail(null)} /> : null}
      {closing ? (
        <CloseDialog row={closing} onClose={() => setClosing(null)} onDone={() => (setClosing(null), reload())} />
      ) : null}
    </div>
  );
}

function DeadLetterDetail({ row, onClose }: { row: DeadLetterRow; onClose: () => void }) {
  const why = reasonLabel(row.reason_code);
  return (
    <Modal title={`${recordLabel(row.table)} ${row.record_ref ?? ""}`} onClose={onClose}>
      <div className="col gap-8" data-testid="sync-problem-detail">
        <Banner tone={row.status === "open" ? "warning" : "success"} title={why.label}>
          {why.help}
        </Banner>
        <table className="table">
          <tbody>
            <tr>
              <td className="tiny">{t("From")}</td>
              <td>{row.origin ?? t("This computer")}</td>
            </tr>
            <tr>
              <td className="tiny">{t("First failed")}</td>
              <td>{formatDateTime(row.created_at)}</td>
            </tr>
            <tr>
              <td className="tiny">{t("Last tried")}</td>
              <td>{formatDateTime(row.last_attempt_at)}</td>
            </tr>
            <tr>
              <td className="tiny">{t("Tries")}</td>
              <td>{row.attempts}</td>
            </tr>
            {row.status !== "open" ? (
              <tr>
                <td className="tiny">{t("Outcome")}</td>
                <td>
                  {resolutionLabel(row)}
                  {row.resolved_by ? ` · ${row.resolved_by}` : ""}
                  {row.resolved_at ? ` · ${formatDateTime(row.resolved_at)}` : ""}
                  {row.resolution_note ? (
                    <div dir="auto" className="muted">
                      {row.resolution_note}
                    </div>
                  ) : null}
                </td>
              </tr>
            ) : null}
            <tr>
              <td className="tiny">{t("Technical message")}</td>
              <td className="mono tiny" dir="ltr">
                {row.error}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </Modal>
  );
}

function CloseDialog({ row, onClose, onDone }: { row: DeadLetterRow; onClose: () => void; onDone: () => void }) {
  const act = useAction();
  const [note, setNote] = useState("");
  const [confirm, setConfirm] = useState(false);
  const money = MONEY_OR_STOCK.has(row.table);
  return (
    <Modal
      title={t("Close without applying")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button
            variant="danger"
            loading={act.busy}
            disabled={note.trim().length < 5 || (money && !confirm)}
            data-testid="sync-close-confirm"
            onClick={async () => {
              const r = await act.run(() =>
                api.sync.closeDeadLetter({
                  dead_id: row.dead_id,
                  note: note.trim(),
                  confirm_financial: confirm,
                  operation_id: newOperationId(),
                }),
              );
              if (r) onDone();
            }}
          >
            {t("Close without applying")}
          </Button>
        </>
      }
    >
      <div className="col gap-12">
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <div>
          {t(
            "{0} from {1} will not be applied. It stays in this list as closed, with your reason.",
            recordLabel(row.table),
            row.origin ?? t("This computer"),
          )}
        </div>
        <Field label={t("Why")}>
          <textarea
            className="input"
            rows={3}
            dir="auto"
            value={note}
            data-testid="sync-close-note"
            onChange={(e) => setNote(e.target.value)}
          />
        </Field>
        {money ? (
          <Checkbox
            checked={confirm}
            onChange={setConfirm}
            label={t("I understand this is a money or stock record and the hub's totals will not include it.")}
          />
        ) : null}
      </div>
    </Modal>
  );
}

// ------------------------------------------------------------------ terminals

export function TerminalsPage() {
  const { has } = useSession();
  const nav = useNavigate();
  const toast = useToast();
  const h = useLoad(() => api.sync.terminalsHealth(), []);
  const act = useAction();
  const [revoking, setRevoking] = useState<TerminalHealthRow | null>(null);
  const canManage = has("devices.manage") && h.data?.mode === "hub";
  const data = h.data;
  return (
    <div>
      <PageHeader
        title={t("Terminals")}
        subtitle={t(
          "Each till as the hub last saw it. Anything a till has not reported is shown as unknown, never guessed.",
        )}
      />
      {h.error ? <Banner tone="danger">{h.error}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {data ? (
        <div className="row wrap gap-8" style={{ marginBottom: 12 }} data-testid="terminal-summary">
          {(["healthy", "attention", "offline", "unknown", "revoked"] as const).map((k) =>
            data.counts[k] ? (
              <Chip key={k} tone={healthLabel(k).tone}>
                {healthLabel(k).label}: {data.counts[k]}
              </Chip>
            ) : null,
          )}
          <span className="tiny muted">
            {t(
              "Not seen recently after {0} minutes; a case opens after {1} minutes during an open shift. Behind with sending: oldest waiting record over {2} minutes.",
              data.thresholds.not_seen_minutes,
              data.thresholds.not_seen_case_minutes,
              data.thresholds.backlog_minutes,
            )}
          </span>
        </div>
      ) : null}
      {data ? (
        <div className="row wrap gap-8" style={{ marginBottom: 12 }}>
          <div className="card card-pad">
            <div className="tiny muted">{t("Hub")}</div>
            <div className="small">
              {t(
                "App {0} · database {1} · sync protocol {2}",
                data.hub.app_version,
                data.hub.schema_version,
                data.hub.protocol_version,
              )}
            </div>
          </div>
          <div className="card card-pad">
            <div className="tiny muted">{t("Backups (store)")}</div>
            <div className="small">{data.store.backup ? data.store.backup.summary : t("Unknown")}</div>
          </div>
          <div className="card card-pad">
            <div className="tiny muted">{t("Printing on this computer")}</div>
            <div className="small">
              {t("{0} failed in the last 24 hours", data.store.printing_here.failed_24h)}
              <div className="tiny muted">{t("Tills do not report their printers, so theirs are unknown here.")}</div>
            </div>
          </div>
        </div>
      ) : null}
      <div data-testid="terminals">
        <DataTable<TerminalHealthRow>
          rows={data?.terminals ?? null}
          loading={h.loading}
          rowKey={(r) => r.device_id}
          empty={<Empty title={t("No tills paired")}>{t("Pair a till from Sync / Hub to see it here.")}</Empty>}
          columns={[
            {
              key: "n",
              label: t("Till"),
              render: (r) => (
                <div className="col">
                  <strong>{r.name}</strong>
                  <span className="tiny muted" dir="ltr">
                    {r.code}
                  </span>
                </div>
              ),
            },
            {
              key: "h",
              label: t("Health"),
              render: (r) => (
                <div className="col gap-4">
                  <Chip tone={healthLabel(r.health).tone}>{healthLabel(r.health).label}</Chip>
                  {r.reasons.map((x) => (
                    <span key={x} className="tiny">
                      {healthReason(x)}
                    </span>
                  ))}
                </div>
              ),
            },
            {
              key: "seen",
              label: t("Last seen"),
              render: (r) => (r.last_seen_at ? formatDateTime(r.last_seen_at) : t("Never")),
            },
            {
              key: "v",
              label: t("Versions"),
              render: (r) => (
                <div className="col tiny">
                  <span>
                    {t("App")}: {r.versions.app.reported ?? t("Unknown")}
                  </span>
                  <span>
                    {t("Database")}: {r.versions.schema.reported ?? t("Unknown")}
                    {r.versions.schema.matches === false ? ` ≠ ${r.versions.schema.hub}` : ""}
                  </span>
                  <span>
                    {t("Sync protocol")}: {r.versions.protocol.reported ?? t("Unknown")}
                    {r.versions.protocol.matches === false ? ` ≠ ${r.versions.protocol.hub}` : ""}
                  </span>
                </div>
              ),
            },
            {
              key: "s",
              label: t("Sending"),
              render: (r) => (
                <div className="col tiny">
                  <span>
                    {t("Waiting")}: {r.sending.pending ?? t("Unknown")}
                  </span>
                  {r.sending.oldest_pending_at ? (
                    <span>{t("Oldest since {0}", formatDateTime(r.sending.oldest_pending_at))}</span>
                  ) : null}
                  {r.refused.on_hub > 0 ? (
                    <button
                      className="link"
                      onClick={() => nav(`/admin/sync-reconciliation?origin=${encodeURIComponent(r.device_id)}`)}
                    >
                      {t("{0} could not be saved", r.refused.on_hub)}
                    </button>
                  ) : null}
                </div>
              ),
            },
            {
              key: "sh",
              label: t("Shift"),
              render: (r) =>
                r.shift ? (
                  <div className="col tiny">
                    <span dir="ltr">{r.shift.shift_number}</span>
                    <span>{r.shift.user ?? ""}</span>
                  </div>
                ) : (
                  <span className="tiny muted">{t("No open shift received")}</span>
                ),
            },
            {
              key: "c",
              label: t("Cases"),
              render: (r) =>
                r.open_cases > 0 ? (
                  <button className="link" onClick={() => nav("/admin/cases")}>
                    {t("{0} open", r.open_cases)}
                  </button>
                ) : (
                  ""
                ),
            },
            {
              key: "k",
              label: t("Credential"),
              render: (r) => (
                <div className="col gap-4 tiny" data-testid={`credential-${r.code}`}>
                  <span>{t("Version {0}", r.credential.version)}</span>
                  {r.credential.next_version ? (
                    <Chip tone="info">{t("Version {0} waiting to be picked up", r.credential.next_version)}</Chip>
                  ) : null}
                  {r.credential.lost_or_stolen ? <Chip tone="danger">{t("Lost or stolen")}</Chip> : null}
                  {canManage && r.active ? (
                    <div className="row gap-4">
                      {r.credential.next_version ? (
                        <Button
                          size="sm"
                          variant="ghost"
                          loading={act.busy}
                          onClick={async () => {
                            if (await act.run(() => api.sync.cancelRotation(r.device_id))) void h.reload();
                          }}
                        >
                          {t("Cancel")}
                        </Button>
                      ) : (
                        <Button
                          size="sm"
                          loading={act.busy}
                          data-testid={`rotate-${r.code}`}
                          onClick={async () => {
                            const out = await act.run(() => api.sync.rotateCredential(r.device_id));
                            if (out) {
                              toast(
                                "info",
                                t(
                                  "The till picks up its new credential at its next sync. Its current one keeps working until then.",
                                ),
                              );
                              void h.reload();
                            }
                          }}
                        >
                          {t("Rotate credential")}
                        </Button>
                      )}
                      <Button size="sm" variant="ghost" onClick={() => setRevoking(r)}>
                        {t("Revoke…")}
                      </Button>
                    </div>
                  ) : null}
                </div>
              ),
            },
          ]}
        />
      </div>
      {revoking ? (
        <RevokeDialog
          row={revoking}
          onClose={() => setRevoking(null)}
          onDone={() => (setRevoking(null), void h.reload())}
        />
      ) : null}
    </div>
  );
}

function RevokeDialog({ row, onClose, onDone }: { row: TerminalHealthRow; onClose: () => void; onDone: () => void }) {
  const act = useAction();
  const [reason, setReason] = useState("");
  const [lost, setLost] = useState(false);
  return (
    <Modal
      title={t("Revoke {0}", row.name)}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button
            variant="danger"
            loading={act.busy}
            disabled={reason.trim().length < 3}
            data-testid="revoke-confirm"
            onClick={async () => {
              if (await act.run(() => api.sync.revokeDevice(row.device_id, reason.trim(), lost))) onDone();
            }}
          >
            {t("Revoke")}
          </Button>
        </>
      }
    >
      <div className="col gap-12">
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <div>
          {t(
            "The hub refuses this till from now on. Records it has not sent stay on the till. This is not a credential rotation.",
          )}
        </div>
        <TextInput label={t("Why")} value={reason} onChange={(e) => setReason(e.target.value)} />
        <Checkbox
          checked={lost}
          onChange={setLost}
          label={t(
            "Lost or stolen: its credential can never be used again, even if the till is found (pair a new till instead).",
          )}
        />
      </div>
    </Modal>
  );
}
