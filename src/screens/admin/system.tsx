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
        title="Devices"
        subtitle="Terminals registered to this store. Revoking a terminal blocks its hub access immediately."
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
            label: "Terminal Name",
            render: (r) => (
              <span>
                {r.name} {r.is_this_device ? <Chip tone="brand">This computer</Chip> : null}
              </span>
            ),
          },
          { key: "c", label: "Code", render: (r) => <span className="mono">{r.device_code}</span> },
          { key: "id", label: "Device ID", render: (r) => <span className="mono">{r.device_id.slice(-8)}</span> },
          { key: "m", label: "Mode", render: (r) => r.mode },
          { key: "b", label: "Branch", render: (r) => r.branch_name ?? "—" },
          { key: "l", label: "Last Seen", render: (r) => (r.is_this_device ? "now" : relative(r.last_seen_at)) },
          { key: "v", label: "Version", render: (r) => r.app_version ?? "—" },
          {
            key: "a",
            label: "Status",
            render: (r) => (r.active ? <Chip tone="success">Active</Chip> : <Chip tone="danger">Revoked</Chip>),
          },
        ]}
      />
      {open ? (
        <Drawer title={open.name} onClose={() => setOpen(null)}>
          <div className="col gap-16">
            <dl className="kv">
              <dt>Full device ID</dt>
              <dd className="mono">{open.device_id}</dd>
              <dt>Mode</dt>
              <dd>{open.mode}</dd>
              <dt>Registered</dt>
              <dd>{formatDateTime(open.activated_at)}</dd>
              <dt>Version</dt>
              <dd>{open.app_version ?? "—"}</dd>
              <dt>Last heartbeat</dt>
              <dd>{formatDateTime(open.last_seen_at)}</dd>
              <dt>Pending changes</dt>
              <dd>{open.pending_count ?? "—"}</dd>
              <dt>Last error</dt>
              <dd>{open.last_error ?? "—"}</dd>
            </dl>
            {has("devices.manage") ? (
              <>
                <div className="row" style={{ alignItems: "flex-end" }}>
                  <TextInput label="Rename" value={name} onChange={(e) => setName(e.target.value)} fieldClass="grow" />
                  <Button
                    onClick={async () => {
                      const r = await act.run(() => api.devices.rename(open.device_id, name));
                      if (r) {
                        toast("success", "Terminal renamed");
                        void reload();
                      }
                    }}
                  >
                    Save
                  </Button>
                </div>
                <div className="divider" />
                <div className="tiny">Sensitive actions</div>
                {open.active ? (
                  <Button variant="danger-outline" disabled={open.is_this_device} onClick={() => setRevoke(open)}>
                    Revoke terminal
                  </Button>
                ) : (
                  <Button
                    onClick={async () => {
                      await act.run(() => api.devices.setActive(open.device_id, true));
                      setOpen(null);
                      void reload();
                    }}
                  >
                    Re-activate terminal
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
          title="Revoke terminal"
          confirmLabel="Revoke"
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
          {revoke.name} will no longer be able to synchronize with the hub. Its local records are kept and it can be
          re-activated later.
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
  const act = useAction();
  useEffect(() => {
    const t = setInterval(() => void st.reload(), 10000);
    return () => clearInterval(t);
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
        title="Sync / Hub"
        subtitle="Terminals keep selling when the network is down. Changes synchronize automatically when the hub is reachable."
        actions={
          mode === "terminal" ? (
            <Button
              icon={<RefreshCw size={16} />}
              loading={act.busy}
              onClick={async () => {
                const r = await act.run(() => api.sync.runNow());
                if (r) toast("success", "Synchronized", `${r.pushed} sent · ${r.pulled} received`);
                void st.reload();
              }}
            >
              Sync now
            </Button>
          ) : null
        }
      />
      {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
      {s.blocked_reason ? (
        <Banner
          tone="danger"
          title="Synchronization paused"
          action={
            has("sync.manage") ? (
              <Button size="sm" onClick={async () => (await act.run(() => api.sync.unblock(true)), void st.reload())}>
                I have checked the hub — resume
              </Button>
            ) : null
          }
        >
          {String(s.blocked_reason)}
        </Banner>
      ) : null}
      <div className="kpis" style={{ margin: "16px 0" }}>
        <div className="card kpi">
          <div className="k-label">Mode</div>
          <div className="k-value" style={{ textTransform: "capitalize" }}>
            {mode}
          </div>
        </div>
        {mode === "terminal" ? (
          <>
            <div className="card kpi">
              <div className="k-label">Pending changes</div>
              <div className="k-value">{String(s.pending)}</div>
            </div>
            <div className="card kpi">
              <div className="k-label">Last successful sync</div>
              <div className="k-value" style={{ fontSize: 16 }}>
                {relative(s.last_success_at as string | null)}
              </div>
            </div>
            <div className="card kpi">
              <div className="k-label">Hub</div>
              <div className="k-value" style={{ fontSize: 14 }}>
                {String(s.hub_url)}
              </div>
            </div>
          </>
        ) : null}
        <div className="card kpi">
          <div className="k-label">Unresolved changes</div>
          <div className={`k-value ${Number(s.dead_letters) > 0 ? "neg-num" : ""}`}>{String(s.dead_letters)}</div>
        </div>
      </div>
      {mode === "terminal" && s.last_error ? (
        <Banner tone="warning" title="Last sync attempt failed">
          {String(s.last_error)} ({relative(s.last_error_at as string)})
        </Banner>
      ) : null}
      {mode === "standalone" && has("sync.manage") ? (
        <div className="card card-pad col gap-16">
          <h3>Use this computer as the store hub</h3>
          <div className="muted">
            Other tills can then pair with this computer over the store network. This computer keeps working exactly as
            before; catalogue and settings are managed here.
          </div>
          <div>
            <Button variant="primary" icon={<Server size={16} />} onClick={() => setEnable(true)}>
              Enable hub mode
            </Button>
          </div>
        </div>
      ) : null}
      {mode === "hub" ? (
        <div className="grid-3" style={{ marginBottom: 16 }}>
          <div className="card">
            <div className="card-head">
              <h3>Connected terminals</h3>
            </div>
            <table className="table">
              <thead>
                <tr>
                  <th>Device</th>
                  <th>Status</th>
                  <th>Last Seen</th>
                  <th className="num">Pending</th>
                  <th>Last Push</th>
                  <th>Last Pull</th>
                  <th>Version</th>
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
                  A terminal runs a different AMWAPOS version than the hub (hub {String(s.app_version)}). It will not
                  synchronize until both run the same version.
                </Banner>
              </div>
            ) : null}
          </div>
          <div className="card card-pad col gap-16">
            <h3>Pair a terminal</h3>
            <div className="small muted">
              On the new till choose “Join an existing hub” and enter this address and code.
            </div>
            <div>
              <div className="tiny">Hub address</div>
              {(addr.data?.addresses ?? []).map((a) => (
                <div key={a} className="mono" style={{ fontWeight: 600 }}>
                  {a}
                </div>
              ))}
              {addr.data && !addr.data.running ? (
                <Banner tone="warning">
                  The hub service is not running. Restart AMWAPOS or check that port {addr.data.port} is free.
                </Banner>
              ) : null}
            </div>
            {code ? (
              <div className="banner info col" style={{ alignItems: "flex-start" }}>
                <div className="tiny">Pairing code (single use, expires {formatShort(code.expires_at)})</div>
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
                Generate pairing code
              </Button>
            ) : null}
            <div className="tiny">
              Pair terminals on the store's trusted network. After pairing, every request is signed and revocable.
            </div>
          </div>
        </div>
      ) : null}
      {(dead.data ?? []).length ? (
        <div className="card">
          <div className="card-head">
            <h3>Changes that could not be applied</h3>
          </div>
          <table className="table">
            <thead>
              <tr>
                <th>When</th>
                <th>Direction</th>
                <th>From</th>
                <th>Record</th>
                <th>Problem</th>
                <th className="num">Attempts</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {(dead.data ?? []).map((d) => (
                <tr key={String(d.dead_id)}>
                  <td>{formatShort(String(d.created_at))}</td>
                  <td>{String(d.direction)}</td>
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
                      Retry
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : null}
      {enable ? (
        <Confirm
          title="Enable hub mode"
          confirmLabel="Enable hub"
          busy={act.busy}
          error={act.error}
          onCancel={() => setEnable(false)}
          onConfirm={async () => {
            const r = await act.run(() => api.sync.enableHub());
            if (r) {
              setEnable(false);
              toast("success", "Hub mode enabled");
              void st.reload();
            }
          }}
        >
          This computer will accept connections from paired terminals on the store network (TCP port {String(s.port)}).
          Make sure the Windows firewall allows AMWAPOS on private networks.
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
  const steps = ["Upload", "Column Mapping", "Validation", "Preview", "Results"];
  return (
    <div>
      <PageHeader
        title="Import products"
        subtitle="CSV import with explicit review. Nothing is changed until you press Apply Import."
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
          <h3 style={{ margin: "10px 0 4px" }}>Drop a CSV file or choose one</h3>
          <div className="small muted">
            Columns such as Name, Barcode, Price, Cost, Category, SKU and Stock are detected automatically. Separate
            multiple barcodes with |.
          </div>
          <label className="btn primary" style={{ marginTop: 16 }}>
            Choose file
            <input
              type="file"
              accept=".csv,text/csv,.txt"
              hidden
              onChange={(e) => e.target.files?.[0] && void load(e.target.files[0])}
            />
          </label>
          <div style={{ marginTop: 12 }}>
            <Checkbox label="Update existing products with the same SKU" checked={update} onChange={setUpdate} />
          </div>
        </div>
      ) : null}
      {step === 2 && preview && mapping ? (
        <div className="card card-pad col gap-16">
          <h3>Map columns — {fileName}</h3>
          <div className="form-grid">
            {preview.fields.map((f) => (
              <Field key={f.key} label={f.label} required={f.key === "name"}>
                <select
                  className="select"
                  value={mapping[f.key] ?? ""}
                  onChange={(e) => setMapping({ ...mapping, [f.key]: e.target.value })}
                >
                  <option value="">— not imported —</option>
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
            <Button onClick={() => setStep(1)}>Back</Button>
            <Button
              variant="primary"
              className="right"
              onClick={async () => {
                const clean = Object.fromEntries(Object.entries(mapping).filter(([, v]) => v));
                const p = await repreview(clean);
                if (p) setStep(3);
              }}
            >
              Validate
            </Button>
          </div>
        </div>
      ) : null}
      {step === 3 && preview ? (
        <div className="stack-16">
          <div className="kpis">
            {[
              ["Valid (create)", preview.creates, "success"],
              ["Valid (update)", preview.updates, "success"],
              ["Rows with warnings", preview.warnings, "warning"],
              ["Rows with errors", preview.errors, "danger"],
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
            empty={<div className="empty">No problems found.</div>}
            columns={[
              { key: "r", label: "Row", num: true, render: (r) => r.row },
              { key: "n", label: "Name", render: (r) => r.name || "—" },
              {
                key: "b",
                label: "Barcodes",
                render: (r) => <span className="mono small">{r.barcodes.join(" | ")}</span>,
              },
              {
                key: "p",
                label: "Problem",
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
            <Button onClick={() => setStep(2)}>Back</Button>
            {preview.errors > 0 ? (
              <Checkbox label={`Skip the ${preview.errors} row(s) with errors`} checked={skip} onChange={setSkip} />
            ) : null}
            <Button
              variant="primary"
              className="right"
              disabled={preview.errors > 0 && !skip}
              onClick={() => setStep(4)}
            >
              Continue
            </Button>
          </div>
        </div>
      ) : null}
      {step === 4 && preview ? (
        <div className="card card-pad col gap-16">
          <h3>Preview</h3>
          <dl className="kv">
            <dt>Creates</dt>
            <dd>{preview.creates}</dd>
            <dt>Updates</dt>
            <dd>{preview.updates}</dd>
            <dt>Skipped</dt>
            <dd>{skip ? preview.errors : 0}</dd>
            <dt>Barcodes added</dt>
            <dd>{preview.barcodes_added}</dd>
            <dt>New categories</dt>
            <dd>{preview.new_categories.join(", ") || "—"}</dd>
          </dl>
          <DataTable
            rows={preview.rows.filter((r) => r.action !== "error").slice(0, 20)}
            rowKey={(r) => String(r.row)}
            columns={[
              {
                key: "a",
                label: "Action",
                render: (r) => <Chip tone={r.action === "create" ? "success" : "info"}>{r.action}</Chip>,
              },
              { key: "n", label: "Name", render: (r) => r.name },
              { key: "s", label: "SKU", render: (r) => r.sku ?? "auto" },
              {
                key: "b",
                label: "Barcodes",
                render: (r) => <span className="mono small">{r.barcodes.join(" | ")}</span>,
              },
              { key: "p", label: "Price", num: true, render: (r) => formatMoney(r.price_minor) },
            ]}
          />
          <div className="row">
            <Button onClick={() => setStep(3)}>Back</Button>
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
                  toast("success", "Import completed");
                }
              }}
            >
              Apply Import
            </Button>
          </div>
        </div>
      ) : null}
      {step === 5 && result ? (
        <div className="card card-pad col gap-16">
          <div className="row">
            <CheckCircle2 color="var(--success)" /> <h3>Import completed</h3>
          </div>
          <dl className="kv">
            <dt>Products created</dt>
            <dd>{result.created}</dd>
            <dt>Products updated</dt>
            <dd>{result.updated}</dd>
            <dt>Rows skipped</dt>
            <dd>{result.skipped}</dd>
            <dt>Barcodes added</dt>
            <dd>{result.barcodes_added}</dd>
            <dt>Categories created</dt>
            <dd>{result.categories_created}</dd>
            <dt>Duration</dt>
            <dd>{result.duration_ms} ms</dd>
          </dl>
          <div>
            <Button onClick={() => (setStep(1), setCsv(""), setPreview(null), setOpId(newOperationId()))}>
              Import another file
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
        title="Backups"
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
                  "Backup created and verified",
                  `${r.file_name} · ${((r.size_bytes ?? 0) / 1048576).toFixed(1)} MB in ${r.duration_ms} ms`,
                );
                void reload();
              }
            }}
          >
            Backup Now
          </Button>
        }
      />
      {act.error && !restoring ? <Banner tone="danger">{act.error}</Banner> : null}
      <div className="kpis" style={{ marginBottom: 16 }}>
        <div className="card kpi">
          <div className="k-label">Last successful backup</div>
          <div className="k-value" style={{ fontSize: 16 }}>
            {data.last_success_at ? formatDateTime(String(data.last_success_at)) : "Never"}
          </div>
        </div>
        <div className="card kpi">
          <div className="k-label">Next automatic backup</div>
          <div className="k-value" style={{ fontSize: 16 }}>
            {data.next_due_at
              ? formatDateTime(String(data.next_due_at))
              : (data.settings as { automatic: boolean }).automatic
                ? "Within the next minute"
                : "Disabled"}
          </div>
        </div>
        <div className="card kpi">
          <div className="k-label">Folder</div>
          <div className="k-value mono" style={{ fontSize: 12.5, wordBreak: "break-all" }}>
            {String(data.directory)}
          </div>
          <div className="k-delta muted">
            {data.free_bytes ? `${(Number(data.free_bytes) / 1073741824).toFixed(1)} GB free` : ""}
          </div>
        </div>
      </div>
      <DataTable<BackupRow>
        rows={backups}
        rowKey={(r) => r.path + r.created_at}
        empty={<div className="empty">No backups yet. Create one now.</div>}
        columns={[
          { key: "d", label: "Date", render: (r) => formatDateTime(r.created_at), sort: (r) => r.created_at },
          { key: "k", label: "Type", render: (r) => r.kind },
          {
            key: "s",
            label: "Size",
            num: true,
            render: (r) => (r.size_bytes ? `${(r.size_bytes / 1048576).toFixed(1)} MB` : "—"),
          },
          {
            key: "st",
            label: "Status",
            render: (r) =>
              r.status === "completed" ? (
                r.exists ? (
                  <Chip tone="success">Verified</Chip>
                ) : (
                  <Chip tone="warning">File missing</Chip>
                )
              ) : (
                <Chip tone="danger">Failed</Chip>
              ),
          },
          { key: "f", label: "File", render: (r) => <span className="mono small">{r.file_name || r.error}</span> },
          {
            key: "a",
            label: "",
            num: true,
            render: (r) =>
              has("backup.restore") && r.exists && r.status === "completed" ? (
                <Button size="sm" icon={<RotateCcw size={14} />} onClick={() => void inspect(r.path)}>
                  Restore
                </Button>
              ) : null,
          },
        ]}
      />
      {has("backup.restore") ? (
        <div className="card card-pad row" style={{ marginTop: 16, alignItems: "flex-end" }}>
          <TextInput
            label="Restore from another file (full path)"
            value={custom}
            onChange={(e) => setCustom(e.target.value)}
            fieldClass="grow"
            placeholder="E:\\AMWAPOS backups\\AMWAPOS-MAIN-manual-20260924-190000.amwbak"
          />
          <Button disabled={!custom.trim()} onClick={() => void inspect(custom.trim())}>
            Inspect
          </Button>
        </div>
      ) : null}
      {restoring ? (
        <Modal
          title="Restore backup"
          size="lg"
          onClose={() => (setRestoring(null), act.setError(null))}
          footer={
            restored ? (
              <Button variant="primary" className="right" onClick={() => void logout()}>
                Sign in again
              </Button>
            ) : (
              <>
                <Button onClick={() => setRestoring(null)}>Cancel</Button>
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
                  Restore
                </Button>
              </>
            )
          }
        >
          {restored ? (
            <div className="col gap-16">
              <Banner tone="success" title="Restore completed">
                Restored in {String(restored.duration_ms)} ms. Record counts{" "}
                {restored.counts_verified ? "match the backup" : "differ (the backup was upgraded to this version)"}. A
                safety backup of the replaced data was saved to{" "}
                <span className="mono">{String(restored.safety_backup)}</span>.
              </Banner>
              <div className="small">Everyone must sign in again.</div>
            </div>
          ) : !insp ? (
            act.error ? (
              <Banner tone="danger">{act.error}</Banner>
            ) : (
              <Skeleton />
            )
          ) : (
            <div className="col gap-16">
              <Banner tone="warning" title="This replaces all current data on this computer">
                Every sale, product and setting recorded after this backup was made will be replaced. A safety backup of
                the current data is taken automatically first.
              </Banner>
              <dl className="kv">
                <dt>File</dt>
                <dd className="mono small">{insp.path}</dd>
                <dt>Business</dt>
                <dd>{insp.business_name ?? "—"}</dd>
                <dt>Created</dt>
                <dd>{formatDateTime(insp.created_at)}</dd>
                <dt>Integrity</dt>
                <dd>
                  {insp.integrity === "ok" ? (
                    <Chip tone="success">OK</Chip>
                  ) : (
                    <Chip tone="danger">{insp.integrity}</Chip>
                  )}
                </dd>
                <dt>Checksum</dt>
                <dd>
                  {insp.checksum_matches === null ? (
                    "No manifest"
                  ) : insp.checksum_matches ? (
                    <Chip tone="success">Matches</Chip>
                  ) : (
                    <Chip tone="danger">Mismatch</Chip>
                  )}
                </dd>
                <dt>Schema</dt>
                <dd>
                  {insp.schema_version} (this version: {insp.current_schema_version})
                </dd>
                <dt>Records</dt>
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
                  label={`I understand this backup belongs to a different business (${insp.business_name}).`}
                  checked={ack}
                  onChange={setAck}
                />
              ) : null}
              <TextInput label='Type "RESTORE" to confirm' value={typed} onChange={(e) => setTyped(e.target.value)} />
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
        title="Audit"
        subtitle="Tamper-evident log of every sensitive action. Entries cannot be edited or deleted."
      />
      {verify.data ? (
        <Banner
          tone={verify.data.valid ? "success" : "danger"}
          title={verify.data.valid ? "Audit chain verified" : "Audit chain broken"}
        >
          {verify.data.message}
        </Banner>
      ) : null}
      <div className="filters" style={{ marginTop: 12 }}>
        <DateRange from={from} to={to} onChange={(a, b) => (setFrom(a), setTo(b), setOffset(0))} />
        <input
          className="input"
          style={{ width: 180 }}
          placeholder="Action (e.g. price)"
          value={event}
          onChange={(e) => (setEvent(e.target.value), setOffset(0))}
        />
        <select
          className="select"
          style={{ width: 160 }}
          value={entity}
          onChange={(e) => (setEntity(e.target.value), setOffset(0))}
          aria-label="Entity"
        >
          <option value="">All entities</option>
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
          { key: "t", label: "Time", render: (r) => formatShort(r.created_at) },
          { key: "u", label: "User", render: (r) => r.user_name ?? "System" },
          { key: "a", label: "Action", render: (r) => <span className="mono small">{r.event_type}</span> },
          { key: "e", label: "Entity", render: (r) => r.entity_type },
          { key: "ap", label: "Approved by", render: (r) => r.approver_name ?? "—" },
          { key: "d", label: "Device", render: (r) => r.device_name ?? "—" },
        ]}
      />
      {data ? <Pager total={data.total} limit={100} offset={offset} onChange={setOffset} /> : null}
      {open ? (
        <Drawer title={open.event_type} onClose={() => setOpen(null)}>
          <div className="col gap-16">
            <dl className="kv">
              <dt>Time</dt>
              <dd>{formatDateTime(open.created_at)}</dd>
              <dt>User</dt>
              <dd>{open.user_name ?? "System"}</dd>
              <dt>Approved by</dt>
              <dd>{open.approver_name ?? "—"}</dd>
              <dt>Device</dt>
              <dd>{open.device_name ?? "—"}</dd>
              <dt>Entity</dt>
              <dd className="mono small">
                {open.entity_type} {open.entity_id}
              </dd>
            </dl>
            <AuditDiff before={open.before} after={open.after} />
            <details className="tech">
              <summary>Technical details</summary>
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
  if (!keys.length) return <div className="small muted">No field changes recorded.</div>;
  const show = (v: unknown) => (v === undefined ? "" : typeof v === "object" ? JSON.stringify(v) : String(v));
  return (
    <table className="table">
      <thead>
        <tr>
          <th>Field</th>
          <th>Before</th>
          <th>After</th>
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
    ["business", "Business"],
    ["tax", "Tax"],
    ["pos", "POS"],
    ["shift", "Shifts & cash"],
    ["payments", "Payments"],
    ["receipt", "Receipts"],
    ["printer", "Printers"],
    ["inventory", "Inventory"],
    ["security", "Security"],
    ["backup", "Backups"],
    ["appearance", "Appearance"],
    ["about", "About"],
  ];
  return (
    <div>
      <PageHeader title="Settings" />
      <div className="settings-layout">
        <nav className="subnav" aria-label="Settings sections">
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
          Save changes
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
          label="Business name"
          required
          value={String(data.name ?? "")}
          onChange={(e) => set("name", e.target.value)}
        />
        <TextInput
          label="Arabic name"
          dir="rtl"
          value={String(data.name_ar ?? "")}
          onChange={(e) => set("name_ar", e.target.value)}
        />
        <TextInput
          label="CR number"
          value={String(data.cr_number ?? "")}
          onChange={(e) => set("cr_number", e.target.value)}
        />
        <TextInput
          label="VAT number"
          value={String(data.vat_number ?? "")}
          onChange={(e) => set("vat_number", e.target.value)}
        />
        <TextInput label="Phone" value={String(data.phone ?? "")} onChange={(e) => set("phone", e.target.value)} />
        <TextInput
          label="Timezone"
          value={String(data.timezone ?? "")}
          onChange={(e) => set("timezone", e.target.value)}
        />
        <TextInput
          label="Address"
          value={String(data.address ?? "")}
          onChange={(e) => set("address", e.target.value)}
          fieldClass="span-2"
        />
        <TextInput
          label="Currency"
          value={String(data.currency ?? "")}
          disabled
          hint="The currency is locked after the first sale."
        />
      </div>
      <SaveBar
        busy={act.busy}
        error={act.error}
        onSave={async () => {
          const r = await act.run(() => api.business.update(data));
          if (r) {
            toast("success", "Business details saved");
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
        Tax rules are versioned. To change a rate, create a new rule and move products to it; past sales keep the rate
        they were sold with.
      </Banner>
      <DataTable<TaxRuleRow>
        rows={data}
        rowKey={(r) => r.tax_rule_id}
        columns={[
          { key: "n", label: "Name", render: (r) => r.name },
          { key: "r", label: "Rate", num: true, render: (r) => formatPercent(r.rate_bp) },
          { key: "i", label: "Prices", render: (r) => (r.inclusive ? "Include VAT" : "Exclude VAT") },
          { key: "p", label: "Products", num: true, render: (r) => r.product_count },
          { key: "f", label: "Since", render: (r) => formatShort(r.effective_from) },
          {
            key: "s",
            label: "Status",
            render: (r) => (r.active ? <Chip tone="success">Active</Chip> : <Chip>Inactive</Chip>),
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
                {r.active ? "Deactivate" : "Activate"}
              </Button>
            ),
          },
        ]}
      />
      <div className="card card-pad col gap-16">
        <h3>New tax rule</h3>
        <div className="form-grid">
          <TextInput label="Name" value={name} onChange={(e) => setName(e.target.value)} placeholder="VAT 10%" />
          <TextInput label="Rate (%)" value={rate} onChange={(e) => setRate(e.target.value)} className="num" />
          <Checkbox label="Prices include this tax" checked={incl} onChange={setIncl} />
          <Field label="Replace existing rule (moves its products)">
            <select className="select" value={replace} onChange={(e) => setReplace(e.target.value)}>
              <option value="">Do not replace</option>
              {(data ?? [])
                .filter((t) => t.active)
                .map((t) => (
                  <option key={t.tax_rule_id} value={t.tax_rule_id}>
                    {t.name} ({t.product_count} products)
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
                toast("success", "Tax rule created");
                setName("");
                setRate("");
                setReplace("");
                void reload();
              }
            }}
          >
            Create rule
          </Button>
        </div>
      </div>
    </div>
  );
}

const DESCRIPTIONS: Record<string, Record<string, string>> = {
  pos: {
    allow_negative_stock: "Allow selling tracked items when recorded stock is zero or below without manager approval.",
    allow_custom_item: "Allow custom (non-catalogue) items at the till.",
    cashier_max_discount_bp: "Largest discount (basis points, 1000 = 10%) a cashier can give without manager approval.",
    idle_lock_minutes: "Lock the terminal after this many idle minutes (0 = never). The current sale is kept.",
    receipt_auto_print: "Print a receipt automatically after every sale.",
    return_to_scan_seconds: "Seconds before the success screen returns to a new sale (0 = wait for the cashier).",
    scan_sound: "Play a short tone on scans and errors.",
    duplicate_scan_window_ms: "Ignore an identical barcode scanned again within this many milliseconds (0 = off).",
  },
  shift: {
    blind_close: "Hide the expected drawer amount from cashiers until they have counted.",
    variance_approval_minor: "Cash differences above this amount (minor units) need manager acknowledgement.",
    paid_out_approval_minor: "Paid-outs above this amount (minor units) need manager approval (0 = off).",
  },
  inventory: {
    costing_method: "Costing method used for margins. v1 supports weighted average only.",
    require_adjust_reason: "Require a reason for manual stock adjustments.",
    stocktake_blind_default: "New stocktakes hide expected quantities while counting.",
  },
  security: {
    pin_min_length: "Minimum PIN length.",
    pin_max_length: "Maximum PIN length.",
    max_failed_attempts: "Incorrect PINs before an account is locked.",
    lockout_minutes: "How long a locked account stays locked.",
  },
  "local.backup": {
    directory: "Folder for backups. Prefer a second disk or USB drive.",
    automatic: "Create verified backups automatically.",
    interval_hours: "Hours between automatic backups.",
    keep: "Number of automatic backups to keep.",
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
                <div className="tiny" style={{ marginLeft: 24 }}>
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
            toast("success", "Settings saved");
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
        Card, BenefitPay and bank transfers are recorded as tenders. AMWAPOS does not verify settlement until an
        authorised payment-provider integration is added.
      </Banner>
      <table className="table">
        <thead>
          <tr>
            <th>Method</th>
            <th>Label</th>
            <th>Enabled</th>
            <th>Requires reference</th>
          </tr>
        </thead>
        <tbody>
          {data.tenders.map((t, i) => (
            <tr key={t.method}>
              <td className="mono">{t.method}</td>
              <td>
                <input
                  className="input"
                  value={t.label}
                  onChange={(e) =>
                    setData({ tenders: data.tenders.map((x, j) => (j === i ? { ...x, label: e.target.value } : x)) })
                  }
                  aria-label={`${t.method} label`}
                />
              </td>
              <td>
                <Checkbox
                  label=""
                  checked={t.enabled}
                  onChange={(v) =>
                    setData({ tenders: data.tenders.map((x, j) => (j === i ? { ...x, enabled: v } : x)) })
                  }
                />
              </td>
              <td>
                <Checkbox
                  label=""
                  checked={t.requires_reference}
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
            toast("success", "Payment methods saved");
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
    const lines = [
      c(String(b.name ?? config?.business_name ?? "")),
      ...(data.show_cr_number && b.cr_number ? [c(`CR: ${b.cr_number}`)] : []),
      ...(data.show_vat_number && b.vat_number ? [c(`VAT No: ${b.vat_number}`)] : []),
      ...data.header_lines.map(c),
      "-".repeat(w),
      c(data.title),
      pair("Receipt: T01-0000123", "24 Sep 2026 19:42"),
      ...(data.show_cashier ? [pair("Cashier: Sara", "Till 1")] : []),
      "-".repeat(w),
      "Coca-Cola Original 330ml",
      pair("  2 x 0.250", "0.500"),
      ...(data.show_barcode ? ["  6291100001234"] : []),
      "-".repeat(w),
      pair("Subtotal", "0.500"),
      pair("VAT 10% (incl.)", "0.045"),
      pair("TOTAL", "BHD 0.500"),
      pair("Cash", "1.000"),
      pair("Change", "0.500"),
      "-".repeat(w),
      ...data.footer_lines.map(c),
    ];
    return lines.join("\n");
  }, [data, biz.data, config]);
  if (!data) return <Skeleton />;
  return (
    <div className="grid-2">
      <div className="card card-pad col gap-16">
        <TextInput label="Title" value={data.title} onChange={(e) => setData({ ...data, title: e.target.value })} />
        <Field label="Header lines">
          <textarea
            className="textarea"
            value={data.header_lines.join("\n")}
            onChange={(e) =>
              setData({ ...data, header_lines: e.target.value.split("\n").filter((x, i, a) => x || i < a.length - 1) })
            }
          />
        </Field>
        <Field label="Footer lines">
          <textarea
            className="textarea"
            value={data.footer_lines.join("\n")}
            onChange={(e) =>
              setData({ ...data, footer_lines: e.target.value.split("\n").filter((x, i, a) => x || i < a.length - 1) })
            }
          />
        </Field>
        <Checkbox
          label="Show VAT number"
          checked={data.show_vat_number}
          onChange={(v) => setData({ ...data, show_vat_number: v })}
        />
        <Checkbox
          label="Show CR number"
          checked={data.show_cr_number}
          onChange={(v) => setData({ ...data, show_cr_number: v })}
        />
        <Checkbox
          label="Show cashier"
          checked={data.show_cashier}
          onChange={(v) => setData({ ...data, show_cashier: v })}
        />
        <Checkbox
          label="Show barcodes"
          checked={data.show_barcode}
          onChange={(v) => setData({ ...data, show_barcode: v })}
        />
        <Field label="Paper width">
          <select
            className="select"
            value={data.paper_width_mm}
            onChange={(e) => setData({ ...data, paper_width_mm: Number(e.target.value) })}
          >
            <option value={80}>80 mm</option>
            <option value={58}>58 mm</option>
          </select>
        </Field>
        <SaveBar
          busy={act.busy}
          error={act.error}
          onSave={async () => {
            const r = await act.run(() => api.settings.save("receipt", data));
            if (r) toast("success", "Receipt settings saved");
          }}
        />
      </div>
      <div>
        <div className="tiny" style={{ marginBottom: 8 }}>
          Live preview
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
        <Field label="Printer connection">
          <select className="select" value={data.mode} onChange={(e) => setData({ ...data, mode: e.target.value })}>
            <option value="none">No printer</option>
            <option value="windows">Windows printer (spooler, RAW)</option>
            <option value="network">Network printer (ESC/POS TCP 9100)</option>
            <option value="file">File (testing)</option>
          </select>
        </Field>
        {data.mode === "windows" ? (
          <Field
            label="Windows printer"
            hint={isDesktop() ? undefined : "Printer list is available in the desktop app."}
          >
            <select
              className="select"
              value={data.target}
              onChange={(e) => setData({ ...data, target: e.target.value })}
            >
              <option value="">Choose…</option>
              {(printers.data ?? []).map((p) => (
                <option key={p}>{p}</option>
              ))}
              {data.target && !(printers.data ?? []).includes(data.target) ? <option>{data.target}</option> : null}
            </select>
          </Field>
        ) : data.mode !== "none" ? (
          <TextInput
            label={data.mode === "network" ? "Printer address" : "Output file"}
            value={data.target}
            onChange={(e) => setData({ ...data, target: e.target.value })}
            placeholder={data.mode === "network" ? "192.168.1.50:9100" : "C:\\AMWAPOS\\printer.txt"}
          />
        ) : (
          <div />
        )}
        <Field label="Paper size">
          <select
            className="select"
            value={data.paper_width_mm}
            onChange={(e) => setData({ ...data, paper_width_mm: Number(e.target.value) })}
          >
            <option value={80}>80 mm</option>
            <option value={58}>58 mm</option>
          </select>
        </Field>
        <div className="col">
          <Checkbox
            label="Cut paper after each receipt"
            checked={data.cut}
            onChange={(v) => setData({ ...data, cut: v })}
          />
          <Checkbox
            label="Open cash drawer (printer kick) on cash sales"
            checked={data.drawer_pulse}
            onChange={(v) => setData({ ...data, drawer_pulse: v })}
          />
        </div>
      </div>
      {status ? <Banner tone={status.tone}>{status.text}</Banner> : null}
      <Banner tone="info">
        Arabic text is not yet rasterized for thermal printers; receipts print product names from the English name
        field.
      </Banner>
      <div className="row">
        <Button
          onClick={async () => {
            await act.run(() => api.settings.save("local.printer", data));
            const r = await act.run(() => api.print.test());
            if (r)
              setStatus(
                r.status === "printed"
                  ? { tone: "success", text: "Test page sent to the printer." }
                  : { tone: "danger", text: r.message ?? "Printer not verified." },
              );
          }}
        >
          Test Print
        </Button>
        <Button
          onClick={async () => {
            await act.run(() => api.settings.save("local.printer", data));
            const r = await act.run(() => api.print.drawerTest());
            if (r)
              setStatus(
                r.status === "printed"
                  ? { tone: "success", text: "Drawer pulse sent." }
                  : { tone: "danger", text: r.message ?? "Drawer not verified." },
              );
          }}
        >
          Test cash drawer
        </Button>
        <Button
          variant="primary"
          className="right"
          loading={act.busy}
          onClick={async () => {
            const r = await act.run(() => api.settings.save("local.printer", data));
            if (r) {
              toast("success", "Printer settings saved");
              await reloadConfig();
            }
          }}
        >
          Save changes
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
        <Field label="Theme">
          <select
            className="select"
            value={data.theme || "light"}
            onChange={(e) => setData({ ...data, theme: e.target.value })}
          >
            <option value="light">Light</option>
            <option value="dark">Dark</option>
            <option value="system">System</option>
          </select>
        </Field>
        <Field label="Density">
          <select
            className="select"
            value={data.density || "comfortable"}
            onChange={(e) => setData({ ...data, density: e.target.value })}
          >
            <option value="comfortable">Comfortable</option>
            <option value="compact">Compact</option>
          </select>
        </Field>
        <Field label="Cashier font size">
          <select
            className="select"
            value={data.cashier_font || "normal"}
            onChange={(e) => setData({ ...data, cashier_font: e.target.value })}
          >
            <option value="normal">Normal</option>
            <option value="large">Large</option>
          </select>
        </Field>
      </div>
      <SaveBar
        busy={act.busy}
        error={act.error}
        onSave={async () => {
          const r = await act.run(() => api.settings.save("local.appearance", data));
          if (r) {
            toast("success", "Appearance saved");
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
        <dt>Product</dt>
        <dd>AMWAPOS — Retail Operations System</dd>
        <dt>Version</dt>
        <dd>{status.app_version}</dd>
        <dt>Schema</dt>
        <dd>{status.schema_version}</dd>
        <dt>Data folder</dt>
        <dd className="mono small">{status.data_dir}</dd>
        <dt>Runtime</dt>
        <dd>{isDesktop() ? "Desktop (Tauri)" : "Browser development bridge"}</dd>
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
        title="Diagnostics"
        actions={
          <>
            <Button icon={<ShieldCheck size={16} />} onClick={() => (setFull(true), void reload())} loading={loading}>
              Run Health Check
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
                  toast("success", "Diagnostics exported", "Secrets and customer details are not included.");
                }
              }}
            >
              Export Diagnostics
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
                <strong style={{ width: 140 }}>{d.component}</strong>
                <span className="grow">{d.summary}</span>
                <span className="tiny">{d.state}</span>
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
      <PageHeader title="Updates" />
      <div className="card card-pad col gap-16">
        <dl className="kv">
          <dt>Current version</dt>
          <dd>{status.app_version}</dd>
          <dt>Database schema</dt>
          <dd>{status.schema_version}</dd>
        </dl>
        <Banner tone="info" title="Updates are installed from signed installers">
          Automatic update checks require the publisher's update-signing key, which has not been configured for this
          build. Install new versions with the signed AMWAPOS installer; your data is kept, a safety backup is taken
          before any database upgrade, and unsigned updates are never installed.
        </Banner>
        <div className="small muted">
          Do not update while a sale is in progress. Update the hub and all terminals to the same version.
        </div>
      </div>
    </div>
  );
}
