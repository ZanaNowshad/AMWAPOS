import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { ChevronDown, ChevronUp, X } from "lucide-react";
import { useSession } from "../../state/session";
import { explain } from "../../lib/errors";
import { ApprovalCancelled } from "../../components/approval";
import { Banner, Button, Modal, Skeleton } from "../../components/ui";
import { todayLocal } from "../../lib/time";
import { t, tb } from "../../i18n";

/** Load data with loading / error state. `deps` re-trigger loading. */
export function useLoad<T>(fn: () => Promise<T>, deps: unknown[]) {
  const { handleAuthError } = useSession();
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const seq = useRef(0);
  const reload = useCallback(async () => {
    const my = ++seq.current;
    setLoading(true);
    try {
      const d = await fn();
      if (my === seq.current) {
        setData(d);
        setError(null);
      }
    } catch (e) {
      if (!handleAuthError(e) && my === seq.current) setError(explain(e).message);
    } finally {
      if (my === seq.current) setLoading(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  useEffect(() => {
    void reload();
  }, [reload]);
  return { data, error, loading, reload, setData };
}

/** Run a mutation and surface errors in a banner-friendly string. */
export function useAction() {
  const { handleAuthError } = useSession();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const run = useCallback(
    async <T,>(fn: () => Promise<T>): Promise<T | undefined> => {
      setBusy(true);
      setError(null);
      try {
        return await fn();
      } catch (e) {
        if (e instanceof ApprovalCancelled) return undefined;
        if (!handleAuthError(e)) {
          const ex = explain(e);
          setError(`${ex.message} ${ex.action}`.trim());
        }
        return undefined;
      } finally {
        setBusy(false);
      }
    },
    [handleAuthError],
  );
  return { error, setError, busy, run };
}

export interface Column<T> {
  key: string;
  label: string;
  num?: boolean;
  render: (row: T) => ReactNode;
  sort?: (row: T) => string | number;
  width?: number | string;
}

export function DataTable<T>({
  rows,
  columns,
  rowKey,
  onRowClick,
  empty,
  loading,
  selectable,
  selected,
  onSelect,
  footer,
  maxHeight,
}: {
  rows: T[] | null;
  columns: Column<T>[];
  rowKey: (r: T) => string;
  onRowClick?: (r: T) => void;
  empty?: ReactNode;
  loading?: boolean;
  selectable?: boolean;
  selected?: Set<string>;
  onSelect?: (s: Set<string>) => void;
  footer?: ReactNode;
  maxHeight?: number | string;
}) {
  const [sort, setSort] = useState<{ key: string; dir: 1 | -1 } | null>(null);
  if (loading && !rows) return <Skeleton rows={8} />;
  let data = rows ?? [];
  if (sort) {
    const col = columns.find((c) => c.key === sort.key);
    if (col?.sort) {
      const f = col.sort;
      data = [...data].sort((a, b) => {
        const x = f(a);
        const y = f(b);
        return (x < y ? -1 : x > y ? 1 : 0) * sort.dir;
      });
    }
  }
  const allSel = selectable && data.length > 0 && data.every((r) => selected?.has(rowKey(r)));
  return (
    <div className="card">
      <div className="table-wrap" style={{ maxHeight: maxHeight ?? "none" }}>
        <table className="table">
          <thead>
            <tr>
              {selectable ? (
                <th style={{ width: 36 }}>
                  <input
                    type="checkbox"
                    aria-label={t("Select all")}
                    checked={!!allSel}
                    onChange={(e) => onSelect?.(e.target.checked ? new Set(data.map(rowKey)) : new Set())}
                  />
                </th>
              ) : null}
              {columns.map((c) => (
                <th
                  key={c.key}
                  className={`${c.num ? "num" : ""} ${c.sort ? "sortable" : ""}`}
                  style={{ width: c.width }}
                  onClick={() =>
                    c.sort &&
                    setSort((s) =>
                      s?.key === c.key ? { key: c.key, dir: s.dir === 1 ? -1 : 1 } : { key: c.key, dir: 1 },
                    )
                  }
                  aria-sort={sort?.key === c.key ? (sort.dir === 1 ? "ascending" : "descending") : undefined}
                >
                  {tb(c.label)}
                  {sort?.key === c.key ? sort.dir === 1 ? <ChevronUp size={12} /> : <ChevronDown size={12} /> : null}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {data.map((r) => {
              const k = rowKey(r);
              return (
                <tr
                  key={k}
                  className={`${onRowClick ? "clickable" : ""} ${selected?.has(k) ? "selected" : ""}`}
                  onClick={() => onRowClick?.(r)}
                  tabIndex={onRowClick ? 0 : undefined}
                  onKeyDown={(e) => e.key === "Enter" && onRowClick?.(r)}
                >
                  {selectable ? (
                    <td onClick={(e) => e.stopPropagation()}>
                      <input
                        type="checkbox"
                        aria-label={t("Select row")}
                        checked={!!selected?.has(k)}
                        onChange={(e) => {
                          const s = new Set(selected);
                          if (e.target.checked) s.add(k);
                          else s.delete(k);
                          onSelect?.(s);
                        }}
                      />
                    </td>
                  ) : null}
                  {columns.map((c) => (
                    <td key={c.key} className={c.num ? "num" : ""}>
                      {c.render(r)}
                    </td>
                  ))}
                </tr>
              );
            })}
          </tbody>
          {footer ? <tfoot>{footer}</tfoot> : null}
        </table>
        {data.length === 0 && !loading ? (
          <div>{empty ?? <div className="empty">{t("Nothing to show.")}</div>}</div>
        ) : null}
      </div>
    </div>
  );
}

export function Pager({
  total,
  limit,
  offset,
  onChange,
  onLimit,
}: {
  total: number;
  limit: number;
  offset: number;
  onChange: (o: number) => void;
  onLimit?: (l: number) => void;
}) {
  const from = total === 0 ? 0 : offset + 1;
  const to = Math.min(total, offset + limit);
  return (
    <div className="pager">
      <span>{t("{0}–{1} of {2}", from, to, total.toLocaleString("en"))}</span>
      {onLimit ? (
        <select
          className="select"
          style={{ width: 90, height: 30 }}
          value={limit}
          onChange={(e) => onLimit(Number(e.target.value))}
          aria-label={t("Rows per page")}
        >
          {[25, 50, 100].map((n) => (
            <option key={n} value={n}>
              {t("{0} / page", n)}
            </option>
          ))}
        </select>
      ) : null}
      <span className="grow" />
      <Button size="sm" disabled={offset === 0} onClick={() => onChange(Math.max(0, offset - limit))}>
        {t("Previous")}
      </Button>
      <Button size="sm" disabled={offset + limit >= total} onClick={() => onChange(offset + limit)}>
        {t("Next")}
      </Button>
    </div>
  );
}

export function Drawer({
  title,
  onClose,
  children,
  actions,
}: {
  title: ReactNode;
  onClose: () => void;
  children: ReactNode;
  actions?: ReactNode;
}) {
  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [onClose]);
  return createPortal(
    <>
      <div className="backdrop" style={{ background: "rgba(15,28,46,0.25)" }} onMouseDown={onClose} />
      <aside className="drawer" role="dialog" aria-modal="true">
        <div className="d-head">
          <h2 className="grow" style={{ fontSize: 18 }}>
            {title}
          </h2>
          {actions}
          <Button variant="ghost" aria-label={t("Close")} icon={<X size={18} />} onClick={onClose} />
        </div>
        <div className="d-body">{children}</div>
      </aside>
    </>,
    document.body,
  );
}

/** Confirmation for consequential actions. The text must describe the consequence. */
export function Confirm({
  title,
  children,
  confirmLabel,
  danger,
  onConfirm,
  onCancel,
  busy,
  error,
}: {
  title: string;
  children: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
  busy?: boolean;
  error?: string | null;
}) {
  return (
    <Modal
      title={title}
      size="sm"
      onClose={onCancel}
      footer={
        <>
          <Button onClick={onCancel}>{t("Cancel")}</Button>
          <Button variant={danger ? "danger" : "primary"} className="right" onClick={onConfirm} loading={busy}>
            {confirmLabel}
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        <div>{children}</div>
        {error ? <Banner tone="danger">{error}</Banner> : null}
      </div>
    </Modal>
  );
}

export function DateRange({
  from,
  to,
  onChange,
}: {
  from: string;
  to: string;
  onChange: (from: string, to: string) => void;
}) {
  const presets: [string, () => [string, string]][] = [
    [t("Today"), () => [todayLocal(), todayLocal()]],
    [t("Yesterday"), () => [todayLocal(-1), todayLocal(-1)]],
    ["7 days", () => [todayLocal(-6), todayLocal()]],
    ["30 days", () => [todayLocal(-29), todayLocal()]],
    [t("This month"), () => [todayLocal().slice(0, 8) + "01", todayLocal()]],
  ];
  return (
    <div className="row wrap">
      {presets.map(([label, f]) => {
        const [a, b] = f();
        return (
          <button
            key={label}
            className={`filter-chip ${a === from && b === to ? "active" : ""}`}
            onClick={() => onChange(a, b)}
          >
            {label}
          </button>
        );
      })}
      <input
        type="date"
        className="input"
        style={{ width: 150 }}
        value={from}
        max={to}
        onChange={(e) => onChange(e.target.value, to)}
        aria-label={t("From date")}
      />
      <span className="muted">to</span>
      <input
        type="date"
        className="input"
        style={{ width: 150 }}
        value={to}
        min={from}
        onChange={(e) => onChange(from, e.target.value)}
        aria-label={t("To date")}
      />
    </div>
  );
}

export function downloadBase64(filename: string, base64: string, type: string) {
  const bin = atob(base64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  const url = URL.createObjectURL(new Blob([bytes], { type }));
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export function download(filename: string, content: string, type = "text/csv;charset=utf-8") {
  const blob = new Blob(["﻿", content], { type });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export function Denied() {
  return (
    <div className="empty" style={{ paddingTop: 80 }}>
      <h3>{t("Access restricted")}</h3>
      <p>{t("Your account does not have permission to access this area.")}</p>
    </div>
  );
}
