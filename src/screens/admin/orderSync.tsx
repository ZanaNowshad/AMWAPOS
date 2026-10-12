// Order sync (docs/WHATSAPP_AUTOPILOT.md, "Sync"): every order and delivery
// change is one event, sent in order to each target (a signed webhook for an
// ERP, a script or a spreadsheet service, or a CSV file on a shared folder).
// Temporary failures are retried; an event a target keeps refusing is parked
// here to send again, and the target moves on.
import { useState } from "react";
import { Plus, RefreshCw, Send, Trash2 } from "lucide-react";
import { api } from "../../api";
import type { OrderSyncFailure, OrderSyncOverview, OrderSyncTarget } from "../../api/types";
import { Banner, Button, Checkbox, Chip, Empty, Field, PageHeader, Skeleton } from "../../components/ui";
import { useToast } from "../../components/toast";
import { useSession } from "../../state/session";
import { Confirm, Drawer, useAction, useLoad } from "./common";
import { formatDateTime } from "../../lib/time";
import { t, tb } from "../../i18n";

function TargetStatus({ r }: { r: OrderSyncTarget }) {
  if (!r.enabled) return <Chip>{t("Off")}</Chip>;
  if (r.last_error && r.attempts > 0) return <Chip tone="warning">{t("Retrying")}</Chip>;
  if (r.parked > 0) return <Chip tone="danger">{t("{0} not sent", r.parked)}</Chip>;
  if (r.behind > 0) return <Chip tone="info">{t("{0} waiting", r.behind)}</Chip>;
  return <Chip tone="success">{t("Up to date")}</Chip>;
}

function AddTarget({ onDone, onClose }: { onDone: (secret: string | null) => void; onClose: () => void }) {
  const [kind, setKind] = useState<"webhook" | "csv">("webhook");
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [path, setPath] = useState("");
  const act = useAction();
  return (
    <Drawer title={t("Add a sync target")} onClose={onClose}>
      <div className="col gap-12">
        <Field label={t("Name")}>
          <input className="input" value={name} onChange={(e) => setName(e.target.value)} placeholder={t("e.g. ERP")} />
        </Field>
        <Field label={t("Kind")}>
          <select className="select" value={kind} onChange={(e) => setKind(e.target.value as "webhook" | "csv")}>
            <option value="webhook">{t("Webhook (HTTP POST, signed)")}</option>
            <option value="csv">{t("CSV file (one line per event)")}</option>
          </select>
        </Field>
        {kind === "webhook" ? (
          <Field label={t("Receiving address")}>
            <input
              className="input"
              dir="ltr"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://erp.example.com/amwapos/orders"
            />
          </Field>
        ) : (
          <Field label={t("File")}>
            <input
              className="input"
              dir="ltr"
              value={path}
              onChange={(e) => setPath(e.target.value)}
              placeholder="D:\Shared\orders.csv"
            />
          </Field>
        )}
        <div className="tiny">
          {t(
            "The target starts with the next order change; earlier history is not sent. Each event carries an event id and a version per order, so the receiving system can ignore repeats and older states.",
          )}
        </div>
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <Button
          variant="primary"
          loading={act.busy}
          onClick={async () => {
            const r = await act.run(() => api.orderSync.create({ name, kind, url, path }));
            if (r) onDone(r.secret);
          }}
        >
          {t("Add")}
        </Button>
      </div>
    </Drawer>
  );
}

function Failures({ r, onChanged }: { r: OrderSyncTarget; onChanged: () => void }) {
  const { data, reload } = useLoad<OrderSyncFailure[]>(() => api.orderSync.failures(r.sink_id), [r.sink_id, r.parked]);
  const act = useAction();
  if (!data?.length) return null;
  const run = async (action: "retry" | "dismiss", seq?: number) => {
    if (await act.run(() => api.orderSync.failureAction(r.sink_id, action, seq ?? null))) {
      await reload();
      onChanged();
    }
  };
  return (
    <div className="col gap-8" data-testid="sync-failures">
      <div className="row">
        <strong className="grow">{t("Not sent")}</strong>
        <Button size="sm" icon={<RefreshCw size={14} />} loading={act.busy} onClick={() => run("retry")}>
          {t("Send all again")}
        </Button>
      </div>
      <table className="table">
        <tbody>
          {data.map((f) => (
            <tr key={f.event_seq}>
              <td className="nowrap">{f.occurred_at ? formatDateTime(f.occurred_at) : "—"}</td>
              <td>
                <span dir="ltr">{f.order_number ?? "—"}</span> · <span dir="ltr">{f.kind ?? ""}</span>
              </td>
              <td className="tiny">{tb(f.error)}</td>
              <td className="nowrap">
                {f.status === "retry" ? (
                  <Chip tone="info">{t("Queued again")}</Chip>
                ) : (
                  <>
                    <Button size="sm" onClick={() => run("retry", f.event_seq)}>
                      {t("Send again")}
                    </Button>{" "}
                    <Button size="sm" variant="ghost" onClick={() => run("dismiss", f.event_seq)}>
                      {t("Dismiss")}
                    </Button>
                  </>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
    </div>
  );
}

function TargetCard({ r, onChanged }: { r: OrderSyncTarget; onChanged: () => void }) {
  const toast = useToast();
  const act = useAction();
  const [confirmDelete, setConfirmDelete] = useState(false);
  return (
    <div className="card card-pad col gap-8" data-testid="sync-target">
      <div className="row wrap gap-8">
        <strong className="grow">{r.name}</strong>
        <Chip>{r.kind === "webhook" ? t("Webhook") : t("CSV file")}</Chip>
        <TargetStatus r={r} />
      </div>
      <div className="tiny" dir="ltr">
        {r.url ?? r.path}
      </div>
      <dl className="kv">
        <dt>{t("Sent")}</dt>
        <dd className="num">{r.delivered}</dd>
        <dt>{t("Waiting")}</dt>
        <dd className="num">{r.behind}</dd>
        <dt>{t("Last sent")}</dt>
        <dd>{r.last_ok_at ? formatDateTime(r.last_ok_at) : "—"}</dd>
        {r.last_error ? (
          <>
            <dt>{t("Last problem")}</dt>
            <dd className="tiny">{tb(r.last_error)}</dd>
          </>
        ) : null}
        {r.next_at ? (
          <>
            <dt>{t("Next try")}</dt>
            <dd>{formatDateTime(r.next_at)}</dd>
          </>
        ) : null}
      </dl>
      <div className="row wrap gap-8">
        <Checkbox
          label={t("On")}
          checked={r.enabled}
          onChange={async (x) => {
            const input = { name: r.name, url: r.url ?? undefined, path: r.path ?? undefined, enabled: x };
            if (await act.run(() => api.orderSync.update(r.sink_id, input))) onChanged();
          }}
        />
        <span className="grow" />
        <Button
          size="sm"
          icon={<Send size={14} />}
          loading={act.busy}
          onClick={async () => {
            const res = await act.run(() => api.orderSync.test(r.sink_id));
            if (res) toast(res.ok ? "success" : "error", res.ok ? t("Test event accepted") : tb(res.error ?? ""));
          }}
        >
          {t("Send a test")}
        </Button>
        <Button size="sm" variant="ghost" icon={<Trash2 size={14} />} onClick={() => setConfirmDelete(true)}>
          {t("Remove")}
        </Button>
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      <Failures r={r} onChanged={onChanged} />
      {confirmDelete ? (
        <Confirm
          title={t("Remove this sync target?")}
          confirmLabel={t("Remove")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setConfirmDelete(false)}
          onConfirm={async () => {
            if (await act.run(() => api.orderSync.remove(r.sink_id).then(() => true))) {
              setConfirmDelete(false);
              onChanged();
            }
          }}
        >
          {t("Orders are not affected. The receiving system gets no further events from AMWAPOS.")}
        </Confirm>
      ) : null}
    </div>
  );
}

export function OrderSyncPage() {
  const { has } = useSession();
  const { data, error, reload } = useLoad<OrderSyncOverview>(() => api.orderSync.list(), []);
  const [adding, setAdding] = useState(false);
  const [secret, setSecret] = useState<string | null>(null);
  return (
    <div>
      <PageHeader
        title={t("Order sync")}
        subtitle={t(
          "Sends every order and delivery change to your other systems, in order. AMWAPOS stays the record: nothing a receiving system answers changes an order.",
        )}
        actions={
          has("settings.manage") ? (
            <Button variant="primary" icon={<Plus size={16} />} onClick={() => setAdding(true)}>
              {t("Add target")}
            </Button>
          ) : null
        }
      />
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {secret ? (
        <Banner tone="warning" title={t("Signing secret: copy it now")}>
          <div className="col gap-4">
            <span>
              {t(
                "The receiving system checks each event with this secret: X-Amwapos-Signature is sha256= followed by the HMAC-SHA256 of the body. It is shown only once.",
              )}
            </span>
            <code dir="ltr" data-testid="sync-secret" style={{ userSelect: "all", wordBreak: "break-all" }}>
              {secret}
            </code>
            <Button size="sm" onClick={() => setSecret(null)}>
              {t("I have copied it")}
            </Button>
          </div>
        </Banner>
      ) : null}
      {!data ? (
        <Skeleton rows={4} />
      ) : data.targets.length === 0 ? (
        <Empty title={t("No sync targets")}>
          {t(
            "Add a webhook to send order events to an ERP, an accounting system or a spreadsheet service, or a CSV file on a shared folder.",
          )}
        </Empty>
      ) : (
        <div className="col gap-12" style={{ marginTop: 12 }}>
          {data.last_event ? (
            <div className="tiny muted">
              {t("Latest order event: {0} at {1}", data.last_event.kind, formatDateTime(data.last_event.at))}
            </div>
          ) : null}
          {data.targets.map((r) => (
            <TargetCard key={r.sink_id} r={r} onChanged={() => void reload()} />
          ))}
        </div>
      )}
      {adding ? (
        <AddTarget
          onClose={() => setAdding(false)}
          onDone={(s) => {
            setAdding(false);
            setSecret(s);
            void reload();
          }}
        />
      ) : null}
    </div>
  );
}
