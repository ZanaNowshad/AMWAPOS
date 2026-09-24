import { useState } from "react";
import { FolderOpen, Upload } from "lucide-react";
import { api } from "../../api";
import type { MigrationTable } from "../../api/types";
import { useToast } from "../../components/toast";
import { Banner, Button, Checkbox, Chip, Field, PageHeader } from "../../components/ui";
import { Confirm, useAction } from "./common";
import { fileToBase64 } from "./automation";
import { newOperationId } from "../../lib/ids";
import { t, tb } from "../../i18n";

type Entity = MigrationTable["entity"];
type FieldDef = { key: string; label: string };

const ENTITY_LABEL: Record<Entity, () => string> = {
  products: () => t("Products"),
  customers: () => t("Customers"),
  suppliers: () => t("Suppliers"),
  stock: () => t("Opening stock"),
  unknown: () => t("Not recognised"),
};

// Suppliers and products first: opening stock needs the products to exist.
const ORDER: Entity[] = ["suppliers", "customers", "products", "stock", "unknown"];

interface Summary {
  total: number;
  creates: number;
  updates: number;
  skipped: number;
  errors: number;
  warnings: number;
}

interface RowView {
  row: number;
  action: string;
  label: string;
  errors: string[];
  warnings: string[];
}

/** Products use the product importer's preview shape; other entities the migration shape. */
function normalize(
  entity: string,
  p: Record<string, unknown>,
): { summary: Summary; rows: RowView[]; warning: string | null } {
  if (entity === "products") {
    const rows =
      (p.rows as {
        row: number;
        action: string;
        name: string;
        sku: string | null;
        errors: string[];
        warnings: string[];
      }[]) ?? [];
    return {
      summary: {
        total: Number(p.total_rows ?? 0),
        creates: Number(p.creates ?? 0),
        updates: Number(p.updates ?? 0),
        skipped: 0,
        errors: Number(p.errors ?? 0),
        warnings: Number(p.warnings ?? 0),
      },
      rows: rows.map((r) => ({
        row: r.row,
        action: r.action,
        label: r.name || r.sku || "",
        errors: r.errors,
        warnings: r.warnings,
      })),
      warning: (p.spreadsheet_warning as string | null) ?? null,
    };
  }
  const s = p.summary as Record<string, number>;
  return {
    summary: {
      total: s.total_rows,
      creates: s.creates,
      updates: s.updates,
      skipped: s.skipped,
      errors: s.errors,
      warnings: s.warnings,
    },
    rows: (p.rows as RowView[]) ?? [],
    warning: (p.spreadsheet_warning as string | null) ?? null,
  };
}

const RESULT_LABEL: Record<string, () => string> = {
  applied: () => t("Applied"),
  created: () => t("Created"),
  updated: () => t("Updated"),
  skipped: () => t("Skipped"),
  barcodes_added: () => t("Barcodes added"),
};

interface TableState {
  table: MigrationTable;
  preview: ReturnType<typeof normalize> | null;
  result: Record<string, unknown> | null;
  opId: string;
}

export function MigrationPage() {
  const toast = useToast();
  const [tables, setTables] = useState<TableState[]>([]);
  const [fields, setFields] = useState<Record<string, FieldDef[]>>({});
  const [ignored, setIgnored] = useState<string[]>([]);
  const [update, setUpdate] = useState(false);
  const [skip, setSkip] = useState(true);
  const [applying, setApplying] = useState<number | null>(null);
  const act = useAction();

  const load = async (files: FileList | null) => {
    if (!files?.length) return;
    const list = await Promise.all(
      Array.from(files).map(async (f) => ({ name: f.webkitRelativePath || f.name, data: await fileToBase64(f) })),
    );
    const r = await act.run(() => api.migration.read(list));
    if (r) {
      const sorted = [...r.tables].sort((a, b) => ORDER.indexOf(a.entity) - ORDER.indexOf(b.entity));
      setTables(sorted.map((table) => ({ table, preview: null, result: null, opId: newOperationId() })));
      setFields(r.fields);
      setIgnored(r.ignored);
    }
  };
  const patch = (i: number, p: Partial<TableState>) =>
    setTables((ts) => ts.map((x, j) => (j === i ? { ...x, ...p } : x)));
  const setTable = (i: number, table: MigrationTable) => patch(i, { table, preview: null, result: null });
  const preview = async (i: number) => {
    const ts = tables[i];
    const r = await act.run(() =>
      api.migration.preview({ table: ts.table, update_existing: update, skip_errors: skip }),
    );
    if (r) patch(i, { preview: normalize(r.entity, r.preview) });
  };
  const apply = async (i: number) => {
    const ts = tables[i];
    const r = await act.run(() =>
      api.migration.apply({ table: ts.table, update_existing: update, skip_errors: skip, operation_id: ts.opId }),
    );
    setApplying(null);
    if (r) {
      patch(i, { result: r.result });
      toast("success", t("{0} imported", ENTITY_LABEL[ts.table.entity]()));
    }
  };

  return (
    <div>
      <PageHeader
        title={t("Migration")}
        subtitle={t(
          "Bring products, customers, suppliers and opening stock from another system. Nothing changes until you apply a table.",
        )}
      />
      <div className="col gap-16">
        <div className="card card-pad col gap-16">
          <div className="row wrap">
            <label className="btn primary">
              <Upload size={16} /> {t("Choose files (CSV, Excel, ZIP)")}
              <input
                type="file"
                multiple
                hidden
                accept=".csv,.txt,.tsv,.xlsx,.xlsm,.xls,.ods,.zip"
                onChange={(e) => void load(e.target.files)}
                data-testid="migration-files"
              />
            </label>
            <label className="btn">
              <FolderOpen size={16} /> {t("Choose a folder")}
              <input
                type="file"
                hidden
                // Non-standard attribute supported by WebView2/Chromium for folder upload.
                {...({ webkitdirectory: "", directory: "" } as Record<string, string>)}
                onChange={(e) => void load(e.target.files)}
              />
            </label>
          </div>
          <div className="row wrap">
            <Checkbox label={t("Update records that already exist")} checked={update} onChange={setUpdate} />
            <Checkbox label={t("Skip rows with errors")} checked={skip} onChange={setSkip} />
          </div>
          <div className="tiny">
            {t(
              "Every cell is read as text, so barcodes keep their leading zeros. Import suppliers and products before opening stock.",
            )}
          </div>
          {ignored.length ? <div className="tiny">{t("Ignored (not a table): {0}", ignored.join(", "))}</div> : null}
          {act.error ? <Banner tone="danger">{act.error}</Banner> : null}
        </div>
        {tables.map((ts, i) => (
          <TableCard
            key={`${ts.table.source}-${i}`}
            ts={ts}
            fields={fields[ts.table.entity] ?? []}
            busy={act.busy}
            onChange={(tb2) => setTable(i, tb2)}
            onPreview={() => void preview(i)}
            onApply={() => setApplying(i)}
          />
        ))}
      </div>
      {applying !== null ? (
        <Confirm
          title={t("Apply {0}", tables[applying].table.source)}
          confirmLabel={t("Apply")}
          busy={act.busy}
          error={act.error}
          onCancel={() => setApplying(null)}
          onConfirm={() => void apply(applying)}
        >
          {t(
            "{0} rows will be written through the normal commands and recorded in the audit trail. Rows with errors are skipped.",
            tables[applying].preview?.summary.total ?? 0,
          )}
        </Confirm>
      ) : null}
    </div>
  );
}

function TableCard({
  ts,
  fields,
  busy,
  onChange,
  onPreview,
  onApply,
}: {
  ts: TableState;
  fields: FieldDef[];
  busy: boolean;
  onChange: (t: MigrationTable) => void;
  onPreview: () => void;
  onApply: () => void;
}) {
  const tbl = ts.table;
  const p = ts.preview;
  return (
    <div className="card card-pad col gap-16" data-testid="migration-table">
      <div className="row">
        <strong className="grow" dir="auto">
          {tbl.source}
        </strong>
        <span className="small">{t("{0} rows", tbl.rows.length)}</span>
        <Field label={t("Contains")}>
          <select
            className="select"
            value={tbl.entity}
            onChange={(e) => onChange({ ...tbl, entity: e.target.value as Entity, mapping: {} })}
          >
            {(["products", "customers", "suppliers", "stock", "unknown"] as Entity[]).map((e) => (
              <option key={e} value={e}>
                {ENTITY_LABEL[e]()}
              </option>
            ))}
          </select>
        </Field>
      </div>
      {tbl.entity !== "unknown" ? (
        <div className="form-grid">
          {fields.map((f) => (
            <Field key={f.key} label={tb(f.label)}>
              <select
                className="select"
                value={tbl.mapping[f.key] ?? ""}
                onChange={(e) => {
                  const mapping = { ...tbl.mapping };
                  if (e.target.value) mapping[f.key] = e.target.value;
                  else delete mapping[f.key];
                  onChange({ ...tbl, mapping });
                }}
              >
                <option value="">{t("— not imported —")}</option>
                {tbl.headers.map((h) => (
                  <option key={h} value={h}>
                    {h}
                  </option>
                ))}
              </select>
            </Field>
          ))}
        </div>
      ) : (
        <Banner tone="info">{t("Choose what this table contains to map its columns.")}</Banner>
      )}
      {tbl.rows.length ? (
        <details>
          <summary>{t("Sample rows")}</summary>
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  {tbl.headers.map((h) => (
                    <th key={h}>{h}</th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {tbl.rows.slice(0, 5).map((r, i) => (
                  <tr key={i}>
                    {r.map((c, j) => (
                      <td key={j} dir="auto">
                        {c}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </details>
      ) : null}
      {p ? (
        <div className="col gap-8">
          <div className="row wrap">
            <Chip tone="success">{t("Create {0}", p.summary.creates)}</Chip>
            <Chip tone="info">{t("Update {0}", p.summary.updates)}</Chip>
            <Chip>{t("Skip {0}", p.summary.skipped)}</Chip>
            <Chip tone={p.summary.errors ? "danger" : "default"}>{t("Errors {0}", p.summary.errors)}</Chip>
            <Chip tone={p.summary.warnings ? "warning" : "default"}>{t("Warnings {0}", p.summary.warnings)}</Chip>
          </div>
          {p.warning ? <Banner tone="warning">{tb(p.warning)}</Banner> : null}
          {p.rows.some((r) => r.errors.length || r.warnings.length) ? (
            <div className="table-wrap" style={{ maxHeight: 280 }}>
              <table className="table">
                <tbody>
                  {p.rows
                    .filter((r) => r.errors.length || r.warnings.length)
                    .map((r) => (
                      <tr key={r.row}>
                        <td className="num">{r.row}</td>
                        <td dir="auto">{r.label}</td>
                        <td>
                          {r.errors.map((e) => (
                            <div key={e} className="text-danger small">
                              {tb(e)}
                            </div>
                          ))}
                          {r.warnings.map((w) => (
                            <div key={w} className="small">
                              {tb(w)}
                            </div>
                          ))}
                        </td>
                      </tr>
                    ))}
                </tbody>
              </table>
            </div>
          ) : null}
        </div>
      ) : null}
      {ts.result ? (
        <Banner tone="success" title={t("Applied")}>
          <div className="row wrap">
            {Object.entries(ts.result)
              .filter(([, v]) => typeof v === "number")
              .map(([k, v]) => (
                <Chip key={k}>
                  {RESULT_LABEL[k]?.() ?? k}: {String(v)}
                </Chip>
              ))}
          </div>
          {((ts.result.failed as { row: number; error: string }[] | undefined) ?? []).map((f) => (
            <div key={f.row} className="small">
              {t("Row {0}", f.row)}: {tb(f.error)}
            </div>
          ))}
        </Banner>
      ) : null}
      <div className="row">
        <Button disabled={tbl.entity === "unknown"} loading={busy} onClick={onPreview}>
          {t("Validate and preview")}
        </Button>
        <Button variant="primary" disabled={!p || !!ts.result} onClick={onApply}>
          {t("Apply")}
        </Button>
      </div>
    </div>
  );
}
