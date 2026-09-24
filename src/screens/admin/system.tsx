import { useEffect, useMemo, useState } from "react";
import {
  AlertTriangle,
  CheckCircle2,
  CircleAlert,
  Copy,
  HardDriveDownload,
  Info,
  Plus,
  RefreshCw,
  RotateCcw,
  Server,
  ShieldCheck,
  Upload,
} from "lucide-react";
import { api } from "../../api";
import type {
  AuditRow,
  BackupInspection,
  BackupRow,
  DeviceRow,
  DiagnosticItem,
  ImportPreview,
  TaxRuleRow,
} from "../../api/types";
import { useSession } from "../../state/session";
import { useToast } from "../../components/toast";
import { isDesktop } from "../../api/transport";
import { newOperationId } from "../../lib/ids";
import { formatMoney, formatPercent, parseMoney, parsePercent } from "../../lib/money";
import { formatDateTime, formatShort, relative, todayLocal } from "../../lib/time";
import { Banner, Button, Checkbox, Chip, Field, Modal, PageHeader, Skeleton, TextInput } from "../../components/ui";
import { Confirm, DataTable, DateRange, Drawer, Pager, download, useAction, useLoad } from "./common";
import { t, tb } from "../../i18n";
import { codeLabel } from "../../i18n/codes";

// ---------------- Devices ----------------

export function DevicesPage() {
  const toast = useToast();
  const { has } = useSession();
  const { data, loading, error, reload } = useLoad(() => api.devices.list(), []);
  const [open, setOpen] = useState<DeviceRow | null>(null);
  const [name, setName] = useState("");
  const [revoke, setRevoke] = useState<DeviceRow | null>(null);
  const act = useAction();
  return (
    <div>
      <PageHeader
        title={t("Devices")}
        subtitle={t("Terminals registered to this store. Revoking a terminal blocks its hub access immediately.")}
      />
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<DeviceRow>
        rows={data}
        loading={loading}
        rowKey={(r) => r.device_id}
        onRowClick={(r) => (setOpen(r), setName(r.name))}
        columns={[
          {
            key: "n",
            label: t("Terminal Name"),
            render: (r) => (
              <span>
                {r.name} {r.is_this_device ? <Chip tone="brand">{t("This computer")}</Chip> : null}
              </span>
            ),
          },
          { key: "c", label: t("Code"), render: (r) => <span className="mono">{r.device_code}</span> },
          { key: "id", label: t("Device ID"), render: (r) => <span className="mono">{r.device_id.slice(-8)}</span> },
          { key: "m", label: t("Mode"), render: (r) => r.mode },
          { key: "b", label: t("Branch"), render: (r) => r.branch_name ?? "—" },
          { key: "l", label: t("Last Seen"), render: (r) => (r.is_this_device ? "now" : relative(r.last_seen_at)) },
          { key: "v", label: t("Version"), render: (r) => r.app_version ?? "—" },
          {
            key: "a",
            label: t("Status"),
            render: (r) =>
              r.active ? <Chip tone="success">{t("Active")}</Chip> : <Chip tone="danger">{t("Revoked")}</Chip>,
          },
        ]}
      />
      {open ? (
        <Drawer title={open.name} onClose={() => setOpen(null)}>
          <div className="col gap-16">
            <dl className="kv">
              <dt>{t("Full device ID")}</dt>
              <dd className="mono">{open.device_id}</dd>
              <dt>{t("Mode")}</dt>
              <dd>{codeLabel(open.mode)}</dd>
              <dt>{t("Registered")}</dt>
              <dd>{formatDateTime(open.activated_at)}</dd>
              <dt>{t("Version")}</dt>
              <dd>{open.app_version ?? "—"}</dd>
              <dt>{t("Last heartbeat")}</dt>
              <dd>{formatDateTime(open.last_seen_at)}</dd>
              <dt>{t("Pending changes")}</dt>
              <dd>{open.pending_count ?? "—"}</dd>
              <dt>{t("Last error")}</dt>
              <dd>{open.last_error ?? "—"}</dd>
            </dl>
            {has("devices.manage") ? (
              <>
                <div className="row" style={{ alignItems: "flex-end" }}>
                  <TextInput
                    label={t("Rename")}
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    fieldClass="grow"
                  />
                  <Button
                    onClick={async () => {
                      const r = await act.run(() => api.devices.rename(open.device_id, name));
                      if (r) {
                        toast("success", t("Terminal renamed"));
                        void reload();
                      }
                    }}
                  >
                    {t("Save")}
                  </Button>
                </div>
                <div className="divider" />
                <div className="tiny">{t("Sensitive actions")}</div>
                {open.active ? (
                  <Button variant="danger-outline" disabled={open.is_this_device} onClick={() => setRevoke(open)}>
                    {t("Revoke terminal")}
                  </Button>
                ) : (
                  <Button
                    onClick={async () => {
                      await act.run(() => api.devices.setActive(open.device_id, true));
                      setOpen(null);
                      void reload();
                    }}
                  >
                    {t("Re-activate terminal")}
                  </Button>
                )}
              </>
            ) : null}
            {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
          </div>
        </Drawer>
      ) : null}
      {revoke ? (
        <Confirm
          title={t("Revoke terminal")}
          confirmLabel={t("Revoke")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setRevoke(null)}
          onConfirm={async () => {
            const r = await act.run(() => api.devices.setActive(revoke.device_id, false));
            if (r) {
              setRevoke(null);
              setOpen(null);
              void reload();
            }
          }}
        >
          {t(
            "{0} will no longer be able to synchronize with the hub. Its local records are kept and it can be re-activated later.",
            revoke.name,
          )}
        </Confirm>
      ) : null}
    </div>
  );
}

// ---------------- Sync / Hub ----------------

export function SyncPage() {
  const toast = useToast();
  const { has } = useSession();
  const st = useLoad(() => api.sync.status(), []);
  const addr = useLoad(
    () => (st.data?.mode === "hub" ? api.sync.hubAddresses() : Promise.resolve(null)),
    [st.data?.mode],
  );
  const dead = useLoad(() => (has("sync.manage") ? api.sync.deadLetters() : Promise.resolve([])), []);
  const [code, setCode] = useState<{ code: string; expires_at: string } | null>(null);
  const [enable, setEnable] = useState(false);
  const [resetCreds, setResetCreds] = useState(false);
  const act = useAction();
  useEffect(() => {
    const tv = setInterval(() => void st.reload(), 10000);
    return () => clearInterval(tv);
  }, [st]);
  const s = st.data as Record<string, unknown> | null;
  if (!s) return st.error ? <Banner tone="danger">{st.error}</Banner> : <Skeleton />;
  const mode = String(s.mode);
  const devices = (s.devices as Record<string, unknown>[]) ?? [];
  const tone: Record<string, "success" | "warning" | "danger" | "info" | "default"> = {
    healthy: "success",
    hub: "info",
    offline: "warning",
    behind: "warning",
    error: "danger",
    version_mismatch: "danger",
    revoked: "default",
    never_seen: "default",
  };
  return (
    <div>
      <PageHeader
        title={t("Sync / Hub")}
        subtitle={t(
          "Terminals keep selling when the network is down. Changes synchronize automatically when the hub is reachable.",
        )}
        actions={
          mode === "terminal" ? (
            <Button
              icon={<RefreshCw size={16} />}
              loading={act.busy}
              onClick={async () => {
                const r = await act.run(() => api.sync.runNow());
                if (r) toast("success", t("Synchronized"), `${r.pushed} sent · ${r.pulled} received`);
                void st.reload();
              }}
            >
              {t("Sync now")}
            </Button>
          ) : null
        }
      />
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {s.blocked_reason ? (
        <Banner
          tone="danger"
          title={t("Synchronization paused")}
          action={
            has("sync.manage") ? (
              <Button size="sm" onClick={async () => (await act.run(() => api.sync.unblock(true)), void st.reload())}>
                {t("I have checked the hub — resume")}
              </Button>
            ) : null
          }
        >
          {String(s.blocked_reason)}
        </Banner>
      ) : null}
      <div className="kpis" style={{ margin: "16px 0" }}>
        <div className="card kpi">
          <div className="k-label">{t("Mode")}</div>
          <div className="k-value" style={{ textTransform: "capitalize" }}>
            {mode}
          </div>
        </div>
        {mode === "terminal" ? (
          <>
            <div className="card kpi">
              <div className="k-label">{t("Pending changes")}</div>
              <div className="k-value">{String(s.pending)}</div>
            </div>
            <div className="card kpi">
              <div className="k-label">{t("Last successful sync")}</div>
              <div className="k-value" style={{ fontSize: 16 }}>
                {relative(s.last_success_at as string | null)}
              </div>
            </div>
            <div className="card kpi">
              <div className="k-label">{t("Hub")}</div>
              <div className="k-value" style={{ fontSize: 14 }}>
                {String(s.hub_url)}
              </div>
            </div>
          </>
        ) : null}
        <div className="card kpi">
          <div className="k-label">{t("Unresolved changes")}</div>
          <div className={`k-value ${Number(s.dead_letters) > 0 ? "neg-num" : ""}`}>{String(s.dead_letters)}</div>
        </div>
      </div>
      {mode === "terminal" && s.last_error ? (
        <Banner tone="warning" title={t("Last sync attempt failed")}>
          {String(s.last_error)} ({relative(s.last_error_at as string)})
        </Banner>
      ) : null}
      {mode === "standalone" && has("sync.manage") ? (
        <div className="card card-pad col gap-16">
          <h3>{t("Use this computer as the store hub")}</h3>
          <div className="muted">
            {t(
              "Other tills can then pair with this computer over the store network. This computer keeps working exactly as before; catalogue and settings are managed here.",
            )}
          </div>
          <div>
            <Button variant="primary" icon={<Server size={16} />} onClick={() => setEnable(true)}>
              {t("Enable hub mode")}
            </Button>
          </div>
        </div>
      ) : null}
      {mode === "hub" ? (
        <div className="grid-3" style={{ marginBottom: 16 }}>
          <div className="card">
            <div className="card-head">
              <h3>{t("Connected terminals")}</h3>
            </div>
            <table className="table">
              <thead>
                <tr>
                  <th>{t("Device")}</th>
                  <th>{t("Status")}</th>
                  <th>{t("Last Seen")}</th>
                  <th className="num">{t("Pending")}</th>
                  <th>{t("Last Push")}</th>
                  <th>{t("Last Pull")}</th>
                  <th>{t("Version")}</th>
                </tr>
              </thead>
              <tbody>
                {devices.map((d) => (
                  <tr key={String(d.device_id)} title={d.last_error ? String(d.last_error) : undefined}>
                    <td>
                      {String(d.name)} <span className="tiny mono">{String(d.code)}</span>
                    </td>
                    <td>
                      <Chip tone={tone[String(d.status)] ?? "default"}>{String(d.status).replace("_", " ")}</Chip>
                    </td>
                    <td>{relative(d.last_seen_at as string | null)}</td>
                    <td className="num">{d.pending === null ? "—" : String(d.pending)}</td>
                    <td>{formatShort(d.last_push_at as string | null)}</td>
                    <td>{formatShort(d.last_pull_at as string | null)}</td>
                    <td>{(d.app_version as string) ?? "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {devices.some((d) => d.status === "version_mismatch") ? (
              <div className="card-body">
                <Banner tone="danger">
                  {t(
                    "A terminal runs a different AMWAPOS version than the hub (hub {0}). It will not synchronize until both run the same version.",
                    String(s.app_version),
                  )}
                </Banner>
              </div>
            ) : null}
          </div>
          <div className="card card-pad col gap-16">
            <h3>{t("Pair a terminal")}</h3>
            <div className="small muted">
              {t("On the new till choose “Join an existing hub” and enter this address and code.")}
            </div>
            <div>
              <div className="tiny">{t("Hub address")}</div>
              {(addr.data?.addresses ?? []).map((a) => (
                <div key={a} className="mono" style={{ fontWeight: 600 }}>
                  {a}
                </div>
              ))}
              {addr.data && !addr.data.running ? (
                <Banner tone="warning">
                  {t("The hub service is not running. Restart AMWAPOS or check that port {0} is free.", addr.data.port)}
                </Banner>
              ) : null}
            </div>
            {code ? (
              <div className="banner info col" style={{ alignItems: "flex-start" }}>
                <div className="tiny">{t("Pairing code (single use, expires {0})", formatShort(code.expires_at))}</div>
                <div className="mono" style={{ fontSize: 30, fontWeight: 700, letterSpacing: "0.15em" }}>
                  {code.code}
                </div>
              </div>
            ) : null}
            {has("devices.manage") ? (
              <Button
                variant="primary"
                icon={<Plus size={16} />}
                onClick={async () => {
                  const r = await act.run(() => api.sync.pairingCode());
                  if (r) setCode(r);
                }}
              >
                {t("Generate pairing code")}
              </Button>
            ) : null}
            <div className="tiny">
              {t(
                "Pair one terminal at a time: a new code cancels the previous one, and five wrong entries cancel the code. The code is never sent over the network; all hub traffic is encrypted and each terminal can be revoked.",
              )}
            </div>
            {has("sync.manage") ? (
              <Button size="sm" variant="ghost" onClick={() => setResetCreds(true)}>
                {t("Reset hub credentials…")}
              </Button>
            ) : null}
          </div>
        </div>
      ) : null}
      {(dead.data ?? []).length ? (
        <div className="card">
          <div className="card-head">
            <h3>{t("Changes that could not be applied")}</h3>
          </div>
          <table className="table">
            <thead>
              <tr>
                <th>{t("When")}</th>
                <th>{t("Direction")}</th>
                <th>{t("From")}</th>
                <th>{t("Record")}</th>
                <th>{t("Problem")}</th>
                <th className="num">{t("Attempts")}</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {(dead.data ?? []).map((d) => (
                <tr key={String(d.dead_id)}>
                  <td>{formatShort(String(d.created_at))}</td>
                  <td>{codeLabel(String(d.direction))}</td>
                  <td>{String(d.origin ?? "—")}</td>
                  <td className="mono small">{String(d.table)}</td>
                  <td className="small">{String(d.error)}</td>
                  <td className="num">{String(d.attempts)}</td>
                  <td className="num">
                    <Button
                      size="sm"
                      onClick={async () => (
                        await act.run(() => api.sync.retryDeadLetter(String(d.dead_id))),
                        void dead.reload(),
                        void st.reload()
                      )}
                    >
                      {t("Retry")}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
      {resetCreds ? (
        <Confirm
          title={t("Reset hub credentials")}
          confirmLabel={t("Reset credentials")}
          danger
          busy={act.busy}
          error={act.error}
          onCancel={() => setResetCreds(false)}
          onConfirm={async () => {
            const r = await act.run(() => api.sync.resetHubCredentials());
            if (r) {
              setResetCreds(false);
              toast("success", t("Hub credentials replaced. Pair every terminal again."));
              void st.reload();
            }
          }}
        >
          {t(
            "Use this only when the hub reports its credential is missing or does not match. Every paired terminal will stop syncing until it is paired again; their unsynced sales stay safe on the terminal and upload after re-pairing.",
          )}
        </Confirm>
      ) : null}
      {enable ? (
        <Confirm
          title={t("Enable hub mode")}
          confirmLabel={t("Enable hub")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setEnable(false)}
          onConfirm={async () => {
            const r = await act.run(() => api.sync.enableHub());
            if (r) {
              setEnable(false);
              toast("success", t("Hub mode enabled"));
              void st.reload();
            }
          }}
        >
          {t(
            "This computer will accept connections from paired terminals on the store network (TCP port {0}). Make sure the Windows firewall allows AMWAPOS on private networks.",
            String(s.port),
          )}
        </Confirm>
      ) : null}
    </div>
  );
}

// ---------------- Import ----------------

export function ImportPage() {
  const toast = useToast();
  const [step, setStep] = useState(1);
  const [csv, setCsv] = useState("");
  const [fileName, setFileName] = useState("");
  const [mapping, setMapping] = useState<Record<string, string> | null>(null);
  const [preview, setPreview] = useState<ImportPreview | null>(null);
  const [update, setUpdate] = useState(false);
  const [skip, setSkip] = useState(false);
  const [result, setResult] = useState<Record<string, number> | null>(null);
  const [over, setOver] = useState(false);
  const [opId, setOpId] = useState(newOperationId);
  const act = useAction();
  const load = async (f: File) => {
    const text = await f.text();
    setCsv(text);
    setFileName(f.name);
    setMapping(null);
    setResult(null);
    const p = await act.run(() => api.products.importPreview({ csv: text, update_existing: update }));
    if (p) {
      setPreview(p);
      setMapping(p.mapping);
      setStep(2);
    }
  };
  const repreview = async (m = mapping) => {
    const p = await act.run(() => api.products.importPreview({ csv, mapping: m, update_existing: update }));
    if (p) setPreview(p);
    return p;
  };
  const steps = [t("Upload"), t("Column Mapping"), t("Validation"), t("Preview"), t("Results")];
  return (
    <div>
      <PageHeader
        title={t("Import products")}
        subtitle={t("CSV import with explicit review. Nothing is changed until you press Apply Import.")}
      />
      <div className="row" style={{ marginBottom: 16 }}>
        {steps.map((s, i) => (
          <Chip key={s} tone={step === i + 1 ? "brand" : step > i + 1 ? "success" : "default"}>
            {i + 1}. {s}
          </Chip>
        ))}
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {step === 1 ? (
        <div
          className={`dropzone ${over ? "over" : ""}`}
          onDragOver={(e) => (e.preventDefault(), setOver(true))}
          onDragLeave={() => setOver(false)}
          onDrop={(e) => {
            e.preventDefault();
            setOver(false);
            const f = e.dataTransfer.files[0];
            if (f) void load(f);
          }}
        >
          <Upload size={28} color="var(--brand)" />
          <h3 style={{ margin: "10px 0 4px" }}>{t("Drop a CSV file or choose one")}</h3>
          <div className="small muted">
            {t(
              "Columns such as Name, Barcode, Price, Cost, Category, SKU and Stock are detected automatically. Separate multiple barcodes with |.",
            )}
          </div>
          <label className="btn primary" style={{ marginTop: 16 }}>
            {t("Choose file")}
            <input
              type="file"
              accept=".csv,text/csv,.txt"
              hidden
              onChange={(e) => e.target.files?.[0] && void load(e.target.files[0])}
            />
          </label>
          <div style={{ marginTop: 12 }}>
            <Checkbox label={t("Update existing products with the same SKU")} checked={update} onChange={setUpdate} />
          </div>
        </div>
      ) : null}
      {step === 2 && preview && mapping ? (
        <div className="card card-pad col gap-16">
          <h3>{t("Map columns — {0}", fileName)}</h3>
          <div className="form-grid">
            {preview.fields.map((f) => (
              <Field key={f.key} label={f.label} required={f.key === "name"}>
                <select
                  className="select"
                  value={mapping[f.key] ?? ""}
                  onChange={(e) => setMapping({ ...mapping, [f.key]: e.target.value })}
                >
                  <option value="">{t("— not imported —")}</option>
                  {preview.columns.map((c) => (
                    <option key={c} value={c}>
                      {c}
                    </option>
                  ))}
                </select>
              </Field>
            ))}
          </div>
          <div className="row">
            <Button onClick={() => setStep(1)}>{t("Back")}</Button>
            <Button
              variant="primary"
              className="right"
              onClick={async () => {
                const clean = Object.fromEntries(Object.entries(mapping).filter(([, v]) => v));
                const p = await repreview(clean);
                if (p) setStep(3);
              }}
            >
              {t("Validate")}
            </Button>
          </div>
        </div>
      ) : null}
      {step === 3 && preview ? (
        <div className="stack-16">
          <div className="kpis">
            {[
              [t("Valid (create)"), preview.creates, "success"],
              [t("Valid (update)"), preview.updates, "success"],
              [t("Rows with warnings"), preview.warnings, "warning"],
              [t("Rows with errors"), preview.errors, "danger"],
            ].map(([l, v]) => (
              <div key={String(l)} className="card kpi">
                <div className="k-label">{l}</div>
                <div className="k-value">{String(v)}</div>
              </div>
            ))}
          </div>
          {preview.spreadsheet_warning ? <Banner tone="warning">{preview.spreadsheet_warning}</Banner> : null}
          <DataTable
            rows={preview.rows.filter((r) => r.errors.length || r.warnings.length)}
            rowKey={(r) => String(r.row)}
            maxHeight="50vh"
            empty={<div className="empty">{t("No problems found.")}</div>}
            columns={[
              { key: "r", label: t("Row"), num: true, render: (r) => r.row },
              { key: "n", label: t("Name"), render: (r) => r.name || "—" },
              {
                key: "b",
                label: t("Barcodes"),
                render: (r) => <span className="mono small">{r.barcodes.join(" | ")}</span>,
              },
              {
                key: "p",
                label: t("Problem"),
                render: (r) => (
                  <div className="col small" style={{ gap: 2 }}>
                    {r.errors.map((e) => (
                      <span key={e} className="neg-num">
                        {e}
                      </span>
                    ))}
                    {r.warnings.map((w) => (
                      <span key={w} style={{ color: "var(--warning)" }}>
                        {w}
                      </span>
                    ))}
                  </div>
                ),
              },
            ]}
          />
          <div className="row">
            <Button onClick={() => setStep(2)}>{t("Back")}</Button>
            {preview.errors > 0 ? (
              <Checkbox
                label={t("Skip the {0} row(s) with errors", preview.errors)}
                checked={skip}
                onChange={setSkip}
              />
            ) : null}
            <Button
              variant="primary"
              className="right"
              disabled={preview.errors > 0 && !skip}
              onClick={() => setStep(4)}
            >
              {t("Continue")}
            </Button>
          </div>
        </div>
      ) : null}
      {step === 4 && preview ? (
        <div className="card card-pad col gap-16">
          <h3>{t("Preview")}</h3>
          <dl className="kv">
            <dt>{t("Creates")}</dt>
            <dd>{preview.creates}</dd>
            <dt>{t("Updates")}</dt>
            <dd>{preview.updates}</dd>
            <dt>{t("Skipped")}</dt>
            <dd>{skip ? preview.errors : 0}</dd>
            <dt>{t("Barcodes added")}</dt>
            <dd>{preview.barcodes_added}</dd>
            <dt>{t("New categories")}</dt>
            <dd>{preview.new_categories.join(", ") || "—"}</dd>
          </dl>
          <DataTable
            rows={preview.rows.filter((r) => r.action !== "error").slice(0, 20)}
            rowKey={(r) => String(r.row)}
            columns={[
              {
                key: "a",
                label: t("Action"),
                render: (r) => (
                  <Chip tone={r.action === "create" ? "success" : "info"}>
                    {r.action === "create" ? t("Creates") : t("Updated")}
                  </Chip>
                ),
              },
              { key: "n", label: t("Name"), render: (r) => r.name },
              { key: "s", label: t("SKU"), render: (r) => r.sku ?? t("auto") },
              {
                key: "b",
                label: t("Barcodes"),
                render: (r) => <span className="mono small">{r.barcodes.join(" | ")}</span>,
              },
              { key: "p", label: t("Price"), num: true, render: (r) => formatMoney(r.price_minor) },
            ]}
          />
          <div className="row">
            <Button onClick={() => setStep(3)}>{t("Back")}</Button>
            <Button
              variant="primary"
              className="right"
              loading={act.busy}
              onClick={async () => {
                const r = await act.run(() =>
                  api.products.importApply({
                    csv,
                    mapping,
                    update_existing: update,
                    skip_errors: skip,
                    operation_id: opId,
                  }),
                );
                if (r) {
                  setResult(r);
                  setStep(5);
                  toast("success", t("Import completed"));
                }
              }}
            >
              {t("Apply Import")}
            </Button>
          </div>
        </div>
      ) : null}
      {step === 5 && result ? (
        <div className="card card-pad col gap-16">
          <div className="row">
            <CheckCircle2 color="var(--success)" /> <h3>{t("Import completed")}</h3>
          </div>
          <dl className="kv">
            <dt>{t("Products created")}</dt>
            <dd>{result.created}</dd>
            <dt>{t("Products updated")}</dt>
            <dd>{result.updated}</dd>
            <dt>{t("Rows skipped")}</dt>
            <dd>{result.skipped}</dd>
            <dt>{t("Barcodes added")}</dt>
            <dd>{result.barcodes_added}</dd>
            <dt>{t("Categories created")}</dt>
            <dd>{result.categories_created}</dd>
            <dt>{t("Duration")}</dt>
            <dd>{result.duration_ms} ms</dd>
          </dl>
          <div>
            <Button onClick={() => (setStep(1), setCsv(""), setPreview(null), setOpId(newOperationId()))}>
              {t("Import another file")}
            </Button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

// ---------------- Backups ----------------

export function BackupsPage() {
  const toast = useToast();
  const { has } = useSession();
  const { data, error, reload } = useLoad(() => api.backup.list(), []);
  const [restoring, setRestoring] = useState<string | null>(null);
  const [insp, setInsp] = useState<BackupInspection | null>(null);
  const [ack, setAck] = useState(false);
  const [typed, setTyped] = useState("");
  const [restored, setRestored] = useState<Record<string, unknown> | null>(null);
  const [custom, setCustom] = useState("");
  const act = useAction();
  const { logout } = useSession();
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton />;
  const backups = data.backups;
  const inspect = async (path: string) => {
    setRestoring(path);
    setInsp(null);
    setAck(false);
    setTyped("");
    const r = await act.run(() => api.backup.inspect(path));
    if (r) setInsp(r);
  };
  return (
    <div>
      <PageHeader
        title={t("Backups")}
        actions={
          <Button
            variant="primary"
            icon={<HardDriveDownload size={16} />}
            loading={act.busy && !restoring}
            onClick={async () => {
              const r = await act.run(() => api.backup.create());
              if (r) {
                toast(
                  "success",
                  t("Backup created and verified"),
                  `${r.file_name} · ${((r.size_bytes ?? 0) / 1048576).toFixed(1)} MB in ${r.duration_ms} ms`,
                );
                void reload();
              }
            }}
          >
            {t("Backup Now")}
          </Button>
        }
      />
      {act.error && !restoring ? <Banner tone="danger">{act.error}</Banner> : null}
      <div className="kpis" style={{ marginBottom: 16 }}>
        <div className="card kpi">
          <div className="k-label">{t("Last successful backup")}</div>
          <div className="k-value" style={{ fontSize: 16 }}>
            {data.last_success_at ? formatDateTime(String(data.last_success_at)) : t("Never")}
          </div>
        </div>
        <div className="card kpi">
          <div className="k-label">{t("Next automatic backup")}</div>
          <div className="k-value" style={{ fontSize: 16 }}>
            {data.next_due_at
              ? formatDateTime(String(data.next_due_at))
              : (data.settings as { automatic: boolean }).automatic
                ? t("Within the next minute")
                : t("Disabled")}
          </div>
        </div>
        <div className="card kpi">
          <div className="k-label">{t("Folder")}</div>
          <div className="k-value mono" style={{ fontSize: 12.5, wordBreak: "break-all" }}>
            {String(data.directory)}
          </div>
          <div className="k-delta muted">
            {data.free_bytes ? t("{0} GB free", (Number(data.free_bytes) / 1073741824).toFixed(1)) : ""}
          </div>
        </div>
      </div>
      <DataTable<BackupRow>
        rows={backups}
        rowKey={(r) => r.path + r.created_at}
        empty={<div className="empty">{t("No backups yet. Create one now.")}</div>}
        columns={[
          { key: "d", label: t("Date"), render: (r) => formatDateTime(r.created_at), sort: (r) => r.created_at },
          { key: "k", label: t("Type"), render: (r) => r.kind },
          {
            key: "s",
            label: t("Size"),
            num: true,
            render: (r) => (r.size_bytes ? `${(r.size_bytes / 1048576).toFixed(1)} MB` : "—"),
          },
          {
            key: "st",
            label: t("Status"),
            render: (r) =>
              r.status === "completed" ? (
                r.exists ? (
                  <Chip tone="success">{t("Verified")}</Chip>
                ) : (
                  <Chip tone="warning">{t("File missing")}</Chip>
                )
              ) : (
                <Chip tone="danger">{t("Failed")}</Chip>
              ),
          },
          { key: "f", label: t("File"), render: (r) => <span className="mono small">{r.file_name || r.error}</span> },
          {
            key: "a",
            label: "",
            num: true,
            render: (r) =>
              has("backup.restore") && r.exists && r.status === "completed" ? (
                <Button size="sm" icon={<RotateCcw size={14} />} onClick={() => void inspect(r.path)}>
                  {t("Restore")}
                </Button>
              ) : null,
          },
        ]}
      />
      {has("backup.restore") ? (
        <div className="card card-pad row" style={{ marginTop: 16, alignItems: "flex-end" }}>
          <TextInput
            label={t("Restore from another file (full path)")}
            value={custom}
            onChange={(e) => setCustom(e.target.value)}
            fieldClass="grow"
            placeholder={t("E:\\\\AMWAPOS backups\\\\AMWAPOS-MAIN-manual-20260924-190000.amwbak")}
          />
          <Button disabled={!custom.trim()} onClick={() => void inspect(custom.trim())}>
            {t("Inspect")}
          </Button>
        </div>
      ) : null}
      {restoring ? (
        <Modal
          title={t("Restore backup")}
          size="lg"
          onClose={() => (setRestoring(null), act.setError(null))}
          footer={
            restored ? (
              <Button variant="primary" className="right" onClick={() => void logout()}>
                {t("Sign in again")}
              </Button>
            ) : (
              <>
                <Button onClick={() => setRestoring(null)}>{t("Cancel")}</Button>
                <Button
                  variant="danger"
                  className="right"
                  disabled={!insp?.ok || !insp.compatible || typed !== "RESTORE" || (!insp.same_business && !ack)}
                  loading={act.busy}
                  onClick={async () => {
                    const r = await act.run(() => api.backup.restore(restoring, ack));
                    if (r) setRestored(r);
                  }}
                >
                  {t("Restore")}
                </Button>
              </>
            )
          }
        >
          {restored ? (
            <div className="col gap-16">
              <Banner tone="success" title={t("Restore completed")}>
                {t("Restored in {0} ms. Record counts", String(restored.duration_ms))}{" "}
                {restored.counts_verified
                  ? t("match the backup")
                  : t("differ (the backup was upgraded to this version)")}
                {t(". A safety backup of the replaced data was saved to")}{" "}
                <span className="mono">{String(restored.safety_backup)}</span>.
              </Banner>
              <div className="small">{t("Everyone must sign in again.")}</div>
            </div>
          ) : !insp ? (
            act.error ? (
              <Banner tone="danger">{act.error}</Banner>
            ) : (
              <Skeleton />
            )
          ) : (
            <div className="col gap-16">
              <Banner tone="warning" title={t("This replaces all current data on this computer")}>
                {t(
                  "Every sale, product and setting recorded after this backup was made will be replaced. A safety backup of the current data is taken automatically first.",
                )}
              </Banner>
              <dl className="kv">
                <dt>{t("File")}</dt>
                <dd className="mono small">{insp.path}</dd>
                <dt>{t("Business")}</dt>
                <dd>{insp.business_name ?? "—"}</dd>
                <dt>{t("Created")}</dt>
                <dd>{formatDateTime(insp.created_at)}</dd>
                <dt>{t("Integrity")}</dt>
                <dd>
                  {insp.integrity === "ok" ? (
                    <Chip tone="success">{t("OK")}</Chip>
                  ) : (
                    <Chip tone="danger">{insp.integrity}</Chip>
                  )}
                </dd>
                <dt>{t("Checksum")}</dt>
                <dd>
                  {insp.checksum_matches === null ? (
                    t("No manifest")
                  ) : insp.checksum_matches ? (
                    <Chip tone="success">{t("Matches")}</Chip>
                  ) : (
                    <Chip tone="danger">{t("Mismatch")}</Chip>
                  )}
                </dd>
                <dt>{t("Schema")}</dt>
                <dd>{t("{0} (this version: {1})", insp.schema_version, insp.current_schema_version)}</dd>
                <dt>{t("Records")}</dt>
                <dd className="small">
                  {Object.entries(insp.record_counts)
                    .filter(([, v]) => v > 0)
                    .map(([k, v]) => `${k.replace(/_/g, " ")}: ${v}`)
                    .join(" · ")}
                </dd>
              </dl>
              {insp.problems.map((p) => (
                <Banner key={p} tone="danger">
                  {p}
                </Banner>
              ))}
              {!insp.same_business ? (
                <Checkbox
                  label={t("I understand this backup belongs to a different business ({0}).", insp.business_name)}
                  checked={ack}
                  onChange={setAck}
                />
              ) : null}
              <TextInput
                label={t('Type "RESTORE" to confirm')}
                value={typed}
                onChange={(e) => setTyped(e.target.value)}
              />
              {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
            </div>
          )}
        </Modal>
      ) : null}
    </div>
  );
}

// ---------------- Audit ----------------

export function AuditPage() {
  const [from, setFrom] = useState(todayLocal(-6));
  const [to, setTo] = useState(todayLocal());
  const [event, setEvent] = useState("");
  const [entity, setEntity] = useState("");
  const [offset, setOffset] = useState(0);
  const [open, setOpen] = useState<AuditRow | null>(null);
  const { data, loading, error } = useLoad(
    () =>
      api.audit.list({
        from,
        to,
        event_type: event || undefined,
        entity_type: entity || undefined,
        limit: 100,
        offset,
      }),
    [from, to, event, entity, offset],
  );
  const verify = useLoad(() => api.audit.verify(), []);
  return (
    <div>
      <PageHeader
        title={t("Audit")}
        subtitle={t("Tamper-evident log of every sensitive action. Entries cannot be edited or deleted.")}
      />
      {verify.data ? (
        <Banner
          tone={verify.data.valid ? "success" : "danger"}
          title={verify.data.valid ? t("Audit chain verified") : t("Audit chain broken")}
        >
          {verify.data.message}
        </Banner>
      ) : null}
      <div className="filters" style={{ marginTop: 12 }}>
        <DateRange from={from} to={to} onChange={(a, b) => (setFrom(a), setTo(b), setOffset(0))} />
        <input
          className="input"
          style={{ width: 180 }}
          placeholder={t("Action (e.g. price)")}
          value={event}
          onChange={(e) => (setEvent(e.target.value), setOffset(0))}
        />
        <select
          className="select"
          style={{ width: 160 }}
          value={entity}
          onChange={(e) => (setEntity(e.target.value), setOffset(0))}
          aria-label={t("Entity")}
        >
          <option value="">{t("All entities")}</option>
          {[
            "sale",
            "refund",
            "product",
            "cart",
            "shift",
            "cash_event",
            "user",
            "role",
            "settings",
            "device",
            "backup",
            "stocktake",
            "purchase_order",
            "customer",
          ].map((x) => (
            <option key={x} value={x}>
              {x}
            </option>
          ))}
        </select>
      </div>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <DataTable<AuditRow>
        rows={data?.rows ?? null}
        loading={loading}
        rowKey={(r) => r.audit_id}
        onRowClick={setOpen}
        columns={[
          { key: "t", label: t("Time"), render: (r) => formatShort(r.created_at) },
          { key: "u", label: t("User"), render: (r) => r.user_name ?? t("System") },
          { key: "a", label: t("Action"), render: (r) => <span className="mono small">{r.event_type}</span> },
          { key: "e", label: t("Entity"), render: (r) => r.entity_type },
          { key: "ap", label: t("Approved by"), render: (r) => r.approver_name ?? "—" },
          { key: "d", label: t("Device"), render: (r) => r.device_name ?? "—" },
        ]}
      />
      {data ? <Pager total={data.total} limit={100} offset={offset} onChange={setOffset} /> : null}
      {open ? (
        <Drawer title={open.event_type} onClose={() => setOpen(null)}>
          <div className="col gap-16">
            <dl className="kv">
              <dt>{t("Time")}</dt>
              <dd>{formatDateTime(open.created_at)}</dd>
              <dt>{t("User")}</dt>
              <dd>{open.user_name ?? t("System")}</dd>
              <dt>{t("Approved by")}</dt>
              <dd>{open.approver_name ?? "—"}</dd>
              <dt>{t("Device")}</dt>
              <dd>{open.device_name ?? "—"}</dd>
              <dt>{t("Entity")}</dt>
              <dd className="mono small">
                {open.entity_type} {open.entity_id}
              </dd>
            </dl>
            <AuditDiff before={open.before} after={open.after} />
            <details className="tech">
              <summary>{t("Technical details")}</summary>
              <pre>
                {JSON.stringify(
                  {
                    seq: open.seq,
                    audit_id: open.audit_id,
                    previous_hash: open.previous_hash,
                    audit_hash: open.audit_hash,
                  },
                  null,
                  2,
                )}
              </pre>
            </details>
          </div>
        </Drawer>
      ) : null}
    </div>
  );
}

function AuditDiff({ before, after }: { before: unknown; after: unknown }) {
  const b = (before && typeof before === "object" ? before : {}) as Record<string, unknown>;
  const a = (after && typeof after === "object" ? after : {}) as Record<string, unknown>;
  const keys = Array.from(new Set([...Object.keys(b), ...Object.keys(a)]));
  if (!keys.length) return <div className="small muted">{t("No field changes recorded.")}</div>;
  const show = (v: unknown) => (v === undefined ? "" : typeof v === "object" ? JSON.stringify(v) : String(v));
  return (
    <table className="table">
      <thead>
        <tr>
          <th>{t("Field")}</th>
          <th>{t("Before")}</th>
          <th>{t("After")}</th>
        </tr>
      </thead>
      <tbody>
        {keys.map((k) => (
          <tr key={k}>
            <td className="mono small">{k}</td>
            <td className="small" style={{ wordBreak: "break-all" }}>
              {show(b[k])}
            </td>
            <td className="small" style={{ wordBreak: "break-all", fontWeight: show(b[k]) !== show(a[k]) ? 650 : 400 }}>
              {show(a[k])}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

// ---------------- Settings ----------------

type Section =
  | "business"
  | "tax"
  | "pos"
  | "shift"
  | "payments"
  | "receipt"
  | "printer"
  | "inventory"
  | "security"
  | "backup"
  | "appearance"
  | "about";

export function SettingsPage() {
  const [section, setSection] = useState<Section>("business");
  const sections: [Section, string][] = [
    ["business", t("Business")],
    ["tax", t("Tax")],
    ["pos", t("POS")],
    ["shift", t("Shifts & cash")],
    ["payments", t("Payments")],
    ["receipt", t("Receipts")],
    ["printer", t("Printers")],
    ["inventory", t("Inventory")],
    ["security", t("Security")],
    ["backup", t("Backups")],
    ["appearance", t("Appearance")],
    ["about", t("About")],
  ];
  return (
    <div>
      <PageHeader title={t("Settings")} />
      <div className="settings-layout">
        <nav className="subnav" aria-label={t("Settings sections")}>
          {sections.map(([k, l]) => (
            <button key={k} className={section === k ? "active" : ""} onClick={() => setSection(k)}>
              {l}
            </button>
          ))}
        </nav>
        <div>
          {section === "business" ? <BusinessSettings /> : null}
          {section === "tax" ? <TaxSettings /> : null}
          {section === "pos" ? <JsonSettings k="pos" /> : null}
          {section === "shift" ? <JsonSettings k="shift" /> : null}
          {section === "payments" ? <PaymentSettings /> : null}
          {section === "receipt" ? <ReceiptSettings /> : null}
          {section === "printer" ? <PrinterSettings /> : null}
          {section === "inventory" ? <JsonSettings k="inventory" /> : null}
          {section === "security" ? <JsonSettings k="security" /> : null}
          {section === "backup" ? <JsonSettings k="local.backup" /> : null}
          {section === "appearance" ? <AppearanceSettings /> : null}
          {section === "about" ? <AboutSettings /> : null}
        </div>
      </div>
    </div>
  );
}

function SaveBar({ onSave, busy, error }: { onSave: () => void; busy: boolean; error: string | null }) {
  return (
    <div className="col" style={{ marginTop: 16 }}>
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <div>
        <Button variant="primary" onClick={onSave} loading={busy}>
          {t("Save changes")}
        </Button>
      </div>
    </div>
  );
}

function BusinessSettings() {
  const toast = useToast();
  const { reloadConfig } = useSession();
  const { data, setData } = useLoad(() => api.business.get(), []);
  const act = useAction();
  if (!data) return <Skeleton />;
  const set = (k: string, v: string) => setData({ ...data, [k]: v });
  return (
    <div className="card card-pad">
      <div className="form-grid">
        <TextInput
          label={t("Business name")}
          required
          value={String(data.name ?? "")}
          onChange={(e) => set("name", e.target.value)}
        />
        <TextInput
          label={t("Arabic name")}
          dir="rtl"
          value={String(data.name_ar ?? "")}
          onChange={(e) => set("name_ar", e.target.value)}
        />
        <TextInput
          label={t("CR number")}
          value={String(data.cr_number ?? "")}
          onChange={(e) => set("cr_number", e.target.value)}
        />
        <TextInput
          label={t("VAT number")}
          value={String(data.vat_number ?? "")}
          onChange={(e) => set("vat_number", e.target.value)}
        />
        <TextInput label={t("Phone")} value={String(data.phone ?? "")} onChange={(e) => set("phone", e.target.value)} />
        <TextInput
          label={t("Timezone")}
          value={String(data.timezone ?? "")}
          onChange={(e) => set("timezone", e.target.value)}
        />
        <TextInput
          label={t("Address")}
          value={String(data.address ?? "")}
          onChange={(e) => set("address", e.target.value)}
          fieldClass="span-2"
        />
        <TextInput
          label={t("Currency")}
          value={String(data.currency ?? "")}
          disabled
          hint={t("The currency is locked after the first sale.")}
        />
      </div>
      <SaveBar
        busy={act.busy}
        error={act.error}
        onSave={async () => {
          const r = await act.run(() => api.business.update(data));
          if (r) {
            toast("success", t("Business details saved"));
            await reloadConfig();
          }
        }}
      />
    </div>
  );
}

function TaxSettings() {
  const toast = useToast();
  const { data, reload } = useLoad(() => api.tax.list(), []);
  const [name, setName] = useState("");
  const [rate, setRate] = useState("");
  const [incl, setIncl] = useState(true);
  const [replace, setReplace] = useState("");
  const act = useAction();
  return (
    <div className="col gap-16">
      <Banner tone="info">
        {t(
          "Tax rules are versioned. To change a rate, create a new rule and move products to it; past sales keep the rate they were sold with.",
        )}
      </Banner>
      <DataTable<TaxRuleRow>
        rows={data}
        rowKey={(r) => r.tax_rule_id}
        columns={[
          { key: "n", label: t("Name"), render: (r) => r.name },
          { key: "r", label: t("Rate"), num: true, render: (r) => formatPercent(r.rate_bp) },
          { key: "i", label: t("Prices"), render: (r) => (r.inclusive ? t("Include VAT") : t("Exclude VAT")) },
          { key: "p", label: t("Products"), num: true, render: (r) => r.product_count },
          { key: "f", label: t("Since"), render: (r) => formatShort(r.effective_from) },
          {
            key: "s",
            label: t("Status"),
            render: (r) => (r.active ? <Chip tone="success">{t("Active")}</Chip> : <Chip>{t("Inactive")}</Chip>),
          },
          {
            key: "a",
            label: "",
            num: true,
            render: (r) => (
              <Button
                size="sm"
                onClick={async () => (await act.run(() => api.tax.setActive(r.tax_rule_id, !r.active)), void reload())}
              >
                {r.active ? t("Deactivate") : t("Activate")}
              </Button>
            ),
          },
        ]}
      />
      <div className="card card-pad col gap-16">
        <h3>{t("New tax rule")}</h3>
        <div className="form-grid">
          <TextInput
            label={t("Name")}
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={t("VAT 10%")}
          />
          <TextInput label={t("Rate (%)")} value={rate} onChange={(e) => setRate(e.target.value)} className="num" />
          <Checkbox label={t("Prices include this tax")} checked={incl} onChange={setIncl} />
          <Field label={t("Replace existing rule (moves its products)")}>
            <select className="select" value={replace} onChange={(e) => setReplace(e.target.value)}>
              <option value="">{t("Do not replace")}</option>
              {(data ?? [])
                .filter((tv) => tv.active)
                .map((tv) => (
                  <option key={tv.tax_rule_id} value={tv.tax_rule_id}>
                    {t("{0} ({1} products)", tv.name, tv.product_count)}
                  </option>
                ))}
            </select>
          </Field>
        </div>
        {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        <div>
          <Button
            variant="primary"
            disabled={!name.trim() || parsePercent(rate) === null}
            onClick={async () => {
              const r = await act.run(() => api.tax.create(name, parsePercent(rate)!, incl, replace || null));
              if (r) {
                toast("success", t("Tax rule created"));
                setName("");
                setRate("");
                setReplace("");
                void reload();
              }
            }}
          >
            {t("Create rule")}
          </Button>
        </div>
      </div>
    </div>
  );
}

const DESCRIPTIONS: Record<string, Record<string, string>> = {
  pos: {
    allow_negative_stock: t(
      "Allow selling tracked items when recorded stock is zero or below without manager approval.",
    ),
    allow_custom_item: t("Allow custom (non-catalogue) items at the till."),
    cashier_max_discount_bp: t(
      "Largest discount (basis points, 1000 = 10%) a cashier can give without manager approval.",
    ),
    idle_lock_minutes: t("Lock the terminal after this many idle minutes (0 = never). The current sale is kept."),
    receipt_auto_print: t("Print a receipt automatically after every sale."),
    return_to_scan_seconds: t("Seconds before the success screen returns to a new sale (0 = wait for the cashier)."),
    scan_sound: t("Play a short tone on scans and errors."),
    duplicate_scan_window_ms: t("Ignore an identical barcode scanned again within this many milliseconds (0 = off)."),
  },
  shift: {
    blind_close: t("Hide the expected drawer amount from cashiers until they have counted."),
    variance_approval_minor: t("Cash differences above this amount (minor units) need manager acknowledgement."),
    paid_out_approval_minor: t("Paid-outs above this amount (minor units) need manager approval (0 = off)."),
  },
  inventory: {
    costing_method: t("Costing method used for margins. v1 supports weighted average only."),
    require_adjust_reason: t("Require a reason for manual stock adjustments."),
    stocktake_blind_default: t("New stocktakes hide expected quantities while counting."),
  },
  security: {
    pin_min_length: t("Minimum PIN length."),
    pin_max_length: t("Maximum PIN length."),
    max_failed_attempts: t("Incorrect PINs before an account is locked."),
    lockout_minutes: t("How long a locked account stays locked."),
  },
  "local.backup": {
    directory: t("Folder for backups. Prefer a second disk or USB drive."),
    automatic: t("Create verified backups automatically."),
    interval_hours: t("Hours between automatic backups."),
    keep: t("Number of automatic backups to keep."),
  },
};

function JsonSettings({ k }: { k: string }) {
  const toast = useToast();
  const { reloadConfig } = useSession();
  const { data, setData, error } = useLoad(() => api.settings.get<Record<string, unknown>>(k), [k]);
  const act = useAction();
  if (error) return <Banner tone="danger">{error}</Banner>;
  if (!data) return <Skeleton />;
  return (
    <div className="card card-pad col gap-16">
      {Object.entries(data).map(([key, v]) => {
        const help = DESCRIPTIONS[k]?.[key];
        const label = key
          .replace(/_/g, " ")
          .replace(/^\w/, (c) => c.toUpperCase())
          .replace(" bp", " (bp)")
          .replace(" minor", "");
        if (typeof v === "boolean") {
          return (
            <div key={key}>
              <Checkbox
                label={label}
                checked={v}
                onChange={(x) => setData({ ...data, [key]: x })}
                disabled={key === "costing_method"}
              />
              {help ? (
                <div className="tiny" style={{ marginInlineStart: 24 }}>
                  {help}
                </div>
              ) : null}
            </div>
          );
        }
        if (typeof v === "number") {
          const isMoney = key.endsWith("_minor");
          return (
            <TextInput
              key={key}
              label={label}
              className="num"
              value={isMoney ? formatMoney(v).split(" ")[1] : String(v)}
              hint={help}
              onChange={(e) => {
                const x = isMoney ? parseMoney(e.target.value) : Number(e.target.value.replace(/[^\d]/g, ""));
                if (x !== null && !Number.isNaN(x)) setData({ ...data, [key]: x });
              }}
            />
          );
        }
        return (
          <TextInput
            key={key}
            label={label}
            value={String(v ?? "")}
            hint={help}
            disabled={key === "costing_method"}
            onChange={(e) => setData({ ...data, [key]: e.target.value })}
          />
        );
      })}
      <SaveBar
        busy={act.busy}
        error={act.error}
        onSave={async () => {
          const r = await act.run(() => api.settings.save(k, data));
          if (r) {
            toast("success", t("Settings saved"));
            setData(r);
            await reloadConfig();
          }
        }}
      />
    </div>
  );
}

function PaymentSettings() {
  const toast = useToast();
  const { reloadConfig } = useSession();
  const { data, setData } = useLoad(
    () =>
      api.settings.get<{
        tenders: {
          method: string;
          label: string;
          enabled: boolean;
          requires_reference: boolean;
          allows_change: boolean;
        }[];
      }>("payments"),
    [],
  );
  const act = useAction();
  if (!data) return <Skeleton />;
  return (
    <div className="card card-pad col gap-16">
      <Banner tone="info">
        {t(
          "Card, BenefitPay and bank transfers are recorded as tenders. AMWAPOS does not verify settlement until an authorised payment-provider integration is added.",
        )}
      </Banner>
      <table className="table">
        <thead>
          <tr>
            <th>{t("Method")}</th>
            <th>{t("Label")}</th>
            <th>{t("Enabled")}</th>
            <th>{t("Requires reference")}</th>
          </tr>
        </thead>
        <tbody>
          {data.tenders.map((tv, i) => (
            <tr key={tv.method}>
              <td className="mono">{tv.method}</td>
              <td>
                <input
                  className="input"
                  value={tv.label}
                  onChange={(e) =>
                    setData({ tenders: data.tenders.map((x, j) => (j === i ? { ...x, label: e.target.value } : x)) })
                  }
                  aria-label={t("{0} label", tv.method)}
                />
              </td>
              <td>
                <Checkbox
                  label=""
                  checked={tv.enabled}
                  onChange={(v) =>
                    setData({ tenders: data.tenders.map((x, j) => (j === i ? { ...x, enabled: v } : x)) })
                  }
                />
              </td>
              <td>
                <Checkbox
                  label=""
                  checked={tv.requires_reference}
                  onChange={(v) =>
                    setData({ tenders: data.tenders.map((x, j) => (j === i ? { ...x, requires_reference: v } : x)) })
                  }
                />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <SaveBar
        busy={act.busy}
        error={act.error}
        onSave={async () => {
          const r = await act.run(() => api.settings.save("payments", data));
          if (r) {
            toast("success", t("Payment methods saved"));
            await reloadConfig();
          }
        }}
      />
    </div>
  );
}

type ReceiptCfg = {
  header_lines: string[];
  footer_lines: string[];
  show_vat_number: boolean;
  show_cr_number: boolean;
  show_cashier: boolean;
  show_barcode: boolean;
  paper_width_mm: number;
  title: string;
  language: "en" | "bilingual";
};

// Must match `arabic()` in crates/amwapos-core/src/receipt.rs.
const RECEIPT_AR: Record<string, string> = {
  "TAX INVOICE": "فاتورة ضريبية",
  Receipt: "الإيصال",
  Cashier: "الكاشير",
  Subtotal: "المجموع",
  VAT: "الضريبة",
  TOTAL: "الإجمالي",
  Cash: "نقداً",
  Change: "الباقي",
  CR: "س.ت",
  "VAT No": "الرقم الضريبي",
};

function ReceiptSettings() {
  const toast = useToast();
  const { config } = useSession();
  const { data, setData } = useLoad(() => api.settings.get<ReceiptCfg>("receipt"), []);
  const biz = useLoad(() => api.business.get(), []);
  const act = useAction();
  const preview = useMemo(() => {
    if (!data) return "";
    const w = data.paper_width_mm <= 58 ? 32 : 48;
    const c = (s: string) => " ".repeat(Math.max(0, Math.floor((w - s.length) / 2))) + s;
    const pair = (l: string, r: string) => l + " ".repeat(Math.max(1, w - l.length - r.length)) + r;
    const b = biz.data ?? {};
    const rl = (en: string) => (data.language === "bilingual" && RECEIPT_AR[en] ? `${en} / ${RECEIPT_AR[en]}` : en);
    const lines = [
      c(String(b.name ?? config?.business_name ?? "")),
      ...(data.show_cr_number && b.cr_number ? [c(`${rl("CR")}: ${b.cr_number}`)] : []),
      ...(data.show_vat_number && b.vat_number ? [c(`${rl("VAT No")}: ${b.vat_number}`)] : []),
      ...data.header_lines.map(c),
      "-".repeat(w),
      c(rl(data.title)),
      pair(`${rl("Receipt")}: T01-0000123`, "24 Sep 2026 19:42"),
      ...(data.show_cashier ? [pair(`${rl("Cashier")}: Sara`, "Till 1")] : []),
      "-".repeat(w),
      "Coca-Cola Original 330ml",
      ...(data.language === "bilingual" ? ["كوكاكولا 330 مل"] : []),
      pair("  2 x 0.250", "0.500"),
      ...(data.show_barcode ? ["  6291100001234"] : []),
      "-".repeat(w),
      pair(rl("Subtotal"), "0.500"),
      pair(`${rl("VAT")} 10% (incl.)`, "0.045"),
      pair(rl("TOTAL"), "BHD 0.500"),
      pair(rl("Cash"), "1.000"),
      pair(rl("Change"), "0.500"),
      "-".repeat(w),
      ...data.footer_lines.map(c),
    ];
    return lines.join("\n");
  }, [data, biz.data, config]);
  if (!data) return <Skeleton />;
  return (
    <div className="grid-2">
      <div className="card card-pad col gap-16">
        <TextInput
          label={t("Title")}
          value={data.title}
          onChange={(e) => setData({ ...data, title: e.target.value })}
        />
        <Field label={t("Header lines")}>
          <textarea
            className="textarea"
            value={data.header_lines.join("\n")}
            onChange={(e) =>
              setData({ ...data, header_lines: e.target.value.split("\n").filter((x, i, a) => x || i < a.length - 1) })
            }
          />
        </Field>
        <Field label={t("Footer lines")}>
          <textarea
            className="textarea"
            value={data.footer_lines.join("\n")}
            onChange={(e) =>
              setData({ ...data, footer_lines: e.target.value.split("\n").filter((x, i, a) => x || i < a.length - 1) })
            }
          />
        </Field>
        <Checkbox
          label={t("Show VAT number")}
          checked={data.show_vat_number}
          onChange={(v) => setData({ ...data, show_vat_number: v })}
        />
        <Checkbox
          label={t("Show CR number")}
          checked={data.show_cr_number}
          onChange={(v) => setData({ ...data, show_cr_number: v })}
        />
        <Checkbox
          label={t("Show cashier")}
          checked={data.show_cashier}
          onChange={(v) => setData({ ...data, show_cashier: v })}
        />
        <Checkbox
          label={t("Show barcodes")}
          checked={data.show_barcode}
          onChange={(v) => setData({ ...data, show_barcode: v })}
        />
        <Field
          label={t("Receipt language")}
          hint={t("Arabic text (store name, product Arabic names, header and footer lines) always prints correctly.")}
        >
          <select
            className="select"
            value={data.language ?? "en"}
            onChange={(e) => setData({ ...data, language: e.target.value as ReceiptCfg["language"] })}
          >
            <option value="en">{t("English labels")}</option>
            <option value="bilingual">{t("English / Arabic labels")}</option>
          </select>
        </Field>
        <Field label={t("Paper width")}>
          <select
            className="select"
            value={data.paper_width_mm}
            onChange={(e) => setData({ ...data, paper_width_mm: Number(e.target.value) })}
          >
            <option value={80}>{t("80 mm")}</option>
            <option value={58}>{t("58 mm")}</option>
          </select>
        </Field>
        <SaveBar
          busy={act.busy}
          error={act.error}
          onSave={async () => {
            const r = await act.run(() => api.settings.save("receipt", data));
            if (r) toast("success", t("Receipt settings saved"));
          }}
        />
      </div>
      <div>
        <div className="tiny" style={{ marginBottom: 8 }}>
          {t("Live preview")}
        </div>
        <div className="receipt-stage">
          <div className="receipt-paper" style={{ width: "fit-content" }}>
            {preview}
          </div>
        </div>
      </div>
    </div>
  );
}

type PrinterCfg = {
  mode: string;
  target: string;
  paper_width_mm: number;
  cut: boolean;
  drawer_pulse: boolean;
  code_page: string;
};

function PrinterSettings() {
  const toast = useToast();
  const { reloadConfig } = useSession();
  const { data, setData } = useLoad(() => api.settings.get<PrinterCfg>("local.printer"), []);
  const printers = useLoad(() => api.print.printers(), []);
  const act = useAction();
  const [status, setStatus] = useState<{ tone: "success" | "danger"; text: string } | null>(null);
  if (!data) return <Skeleton />;
  return (
    <div className="card card-pad col gap-16">
      <div className="form-grid">
        <Field label={t("Printer connection")}>
          <select className="select" value={data.mode} onChange={(e) => setData({ ...data, mode: e.target.value })}>
            <option value="none">{t("No printer")}</option>
            <option value="windows">{t("Windows printer (spooler, RAW)")}</option>
            <option value="network">{t("Network printer (ESC/POS TCP 9100)")}</option>
            <option value="file">{t("File (testing)")}</option>
          </select>
        </Field>
        {data.mode === "windows" ? (
          <Field
            label={t("Windows printer")}
            hint={isDesktop() ? undefined : t("Printer list is available in the desktop app.")}
          >
            <select
              className="select"
              value={data.target}
              onChange={(e) => setData({ ...data, target: e.target.value })}
            >
              <option value="">{t("Choose…")}</option>
              {(printers.data ?? []).map((p) => (
                <option key={p}>{p}</option>
              ))}
              {data.target && !(printers.data ?? []).includes(data.target) ? <option>{data.target}</option> : null}
            </select>
          </Field>
        ) : data.mode !== "none" ? (
          <TextInput
            label={data.mode === "network" ? t("Printer address") : t("Output file")}
            value={data.target}
            onChange={(e) => setData({ ...data, target: e.target.value })}
            placeholder={data.mode === "network" ? "192.168.1.50:9100" : t("C:\\AMWAPOS\\printer.txt")}
          />
        ) : (
          <div />
        )}
        <Field label={t("Paper size")}>
          <select
            className="select"
            value={data.paper_width_mm}
            onChange={(e) => setData({ ...data, paper_width_mm: Number(e.target.value) })}
          >
            <option value={80}>{t("80 mm")}</option>
            <option value={58}>{t("58 mm")}</option>
          </select>
        </Field>
        <div className="col">
          <Checkbox
            label={t("Cut paper after each receipt")}
            checked={data.cut}
            onChange={(v) => setData({ ...data, cut: v })}
          />
          <Checkbox
            label={t("Open cash drawer (printer kick) on cash sales")}
            checked={data.drawer_pulse}
            onChange={(v) => setData({ ...data, drawer_pulse: v })}
          />
        </div>
      </div>
      {status ? <Banner tone={status.tone}>{status.text}</Banner> : null}
      <Banner tone="info">
        {t(
          "Arabic text prints as a high-resolution image line, so it works on any ESC/POS printer. The test page includes an Arabic line — check it prints joined and right-to-left.",
        )}
      </Banner>
      <div className="row">
        <Button
          onClick={async () => {
            await act.run(() => api.settings.save("local.printer", data));
            const r = await act.run(() => api.print.test());
            if (r)
              setStatus(
                r.status === "printed"
                  ? { tone: "success", text: t("Test page sent to the printer.") }
                  : { tone: "danger", text: r.message ?? t("Printer not verified.") },
              );
          }}
        >
          {t("Test Print")}
        </Button>
        <Button
          onClick={async () => {
            await act.run(() => api.settings.save("local.printer", data));
            const r = await act.run(() => api.print.drawerTest());
            if (r)
              setStatus(
                r.status === "printed"
                  ? { tone: "success", text: t("Drawer pulse sent.") }
                  : { tone: "danger", text: r.message ?? t("Drawer not verified.") },
              );
          }}
        >
          {t("Test cash drawer")}
        </Button>
        <Button
          variant="primary"
          className="right"
          loading={act.busy}
          onClick={async () => {
            const r = await act.run(() => api.settings.save("local.printer", data));
            if (r) {
              toast("success", t("Printer settings saved"));
              await reloadConfig();
            }
          }}
        >
          {t("Save changes")}
        </Button>
      </div>
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
    </div>
  );
}

function AppearanceSettings() {
  const toast = useToast();
  const { reloadConfig } = useSession();
  const { data, setData } = useLoad(
    () => api.settings.get<{ theme: string; density: string; cashier_font: string }>("local.appearance"),
    [],
  );
  const act = useAction();
  if (!data) return <Skeleton />;
  return (
    <div className="card card-pad col gap-16">
      <div className="form-grid">
        <Field label={t("Theme")}>
          <select
            className="select"
            value={data.theme || "light"}
            onChange={(e) => setData({ ...data, theme: e.target.value })}
          >
            <option value="light">{t("Light")}</option>
            <option value="dark">{t("Dark")}</option>
            <option value="system">{t("System")}</option>
          </select>
        </Field>
        <Field label={t("Density")}>
          <select
            className="select"
            value={data.density || "comfortable"}
            onChange={(e) => setData({ ...data, density: e.target.value })}
          >
            <option value="comfortable">{t("Comfortable")}</option>
            <option value="compact">{t("Compact")}</option>
          </select>
        </Field>
        <Field label={t("Cashier font size")}>
          <select
            className="select"
            value={data.cashier_font || "normal"}
            onChange={(e) => setData({ ...data, cashier_font: e.target.value })}
          >
            <option value="normal">{t("Normal")}</option>
            <option value="large">{t("Large")}</option>
          </select>
        </Field>
      </div>
      <SaveBar
        busy={act.busy}
        error={act.error}
        onSave={async () => {
          const r = await act.run(() => api.settings.save("local.appearance", data));
          if (r) {
            toast("success", t("Appearance saved"));
            await reloadConfig();
          }
        }}
      />
    </div>
  );
}

function AboutSettings() {
  const { status } = useSession();
  return (
    <div className="card card-pad">
      <dl className="kv">
        <dt>{t("Product")}</dt>
        <dd>{t("AMWAPOS — Retail Operations System")}</dd>
        <dt>{t("Version")}</dt>
        <dd>{status.app_version}</dd>
        <dt>{t("Schema")}</dt>
        <dd>{status.schema_version}</dd>
        <dt>{t("Data folder")}</dt>
        <dd className="mono small">{status.data_dir}</dd>
        <dt>{t("Runtime")}</dt>
        <dd>{isDesktop() ? t("Desktop (Tauri)") : t("Browser development bridge")}</dd>
      </dl>
    </div>
  );
}

// ---------------- Diagnostics ----------------

const STATE_ICON = { ok: CheckCircle2, warning: AlertTriangle, error: CircleAlert, info: Info };
const STATE_COLOR = { ok: "var(--success)", warning: "var(--warning)", error: "var(--danger)", info: "var(--info)" };

export function DiagnosticsPage() {
  const toast = useToast();
  const [full, setFull] = useState(false);
  const { data, loading, error, reload } = useLoad(() => api.diagnostics.get(full), [full]);
  const act = useAction();
  return (
    <div>
      <PageHeader
        title={t("Diagnostics")}
        actions={
          <>
            <Button icon={<ShieldCheck size={16} />} onClick={() => (setFull(true), void reload())} loading={loading}>
              {t("Run Health Check")}
            </Button>
            <Button
              icon={<Copy size={16} />}
              onClick={async () => {
                const r = await act.run(() => api.diagnostics.export());
                if (r) {
                  download(
                    `amwapos-diagnostics-${new Date().toISOString().replace(/[:.]/g, "-")}.json`,
                    JSON.stringify(r, null, 2),
                    "application/json",
                  );
                  toast("success", t("Diagnostics exported"), t("Secrets and customer details are not included."));
                }
              }}
            >
              {t("Export Diagnostics")}
            </Button>
          </>
        }
      />
      {error ? <Banner tone="danger">{error}</Banner> : null}
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {!data ? <Skeleton /> : null}
      <div className="col">
        {(data ?? []).map((d: DiagnosticItem) => {
          const Icon = STATE_ICON[d.state];
          return (
            <details key={d.component} className="card card-pad">
              <summary className="row" style={{ cursor: "pointer", listStyle: "none" }}>
                <Icon size={18} color={STATE_COLOR[d.state]} aria-label={d.state} />
                <strong style={{ width: 140 }}>{tb(d.component)}</strong>
                <span className="grow">{tb(d.summary)}</span>
                <span className="tiny">{codeLabel(d.state)}</span>
              </summary>
              <pre className="mono small" style={{ marginTop: 10, whiteSpace: "pre-wrap" }}>
                {JSON.stringify(d.details, null, 2)}
              </pre>
            </details>
          );
        })}
      </div>
    </div>
  );
}

// ---------------- Updates ----------------

export function UpdatesPage() {
  const { status } = useSession();
  return (
    <div>
      <PageHeader title={t("Updates")} />
      <div className="card card-pad col gap-16">
        <dl className="kv">
          <dt>{t("Current version")}</dt>
          <dd>{status.app_version}</dd>
          <dt>{t("Database schema")}</dt>
          <dd>{status.schema_version}</dd>
        </dl>
        <Banner tone="info" title={t("Updates are installed from signed installers")}>
          {t(
            "Automatic update checks require the publisher's update-signing key, which has not been configured for this build. Install new versions with the signed AMWAPOS installer; your data is kept, a safety backup is taken before any database upgrade, and unsigned updates are never installed.",
          )}
        </Banner>
        <div className="small muted">
          {t("Do not update while a sale is in progress. Update the hub and all terminals to the same version.")}
        </div>
      </div>
    </div>
  );
}
