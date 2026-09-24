import { useEffect, useState } from "react";
import { api, getToken } from "../../api";
import { t, tb } from "../../i18n";

/** Compact local/sync status. Informational only — checkout never depends on it. */
export function ConnectionPill() {
  const [st, setSt] = useState<Record<string, unknown> | null>(null);
  const [online, setOnline] = useState(navigator.onLine);
  useEffect(() => {
    const on = () => setOnline(true);
    const off = () => setOnline(false);
    window.addEventListener("online", on);
    window.addEventListener("offline", off);
    let alive = true;
    const poll = () => {
      if (!getToken()) return;
      api.sync
        .status()
        .then((s) => alive && setSt(s))
        .catch(() => {});
    };
    poll();
    const tv = setInterval(poll, 15000);
    return () => {
      alive = false;
      clearInterval(tv);
      window.removeEventListener("online", on);
      window.removeEventListener("offline", off);
    };
  }, []);
  const mode = (st?.mode as string) ?? "standalone";
  if (mode === "terminal") {
    const pending = Number(st?.pending ?? 0);
    const err = st?.last_error as string | null;
    const kind = st?.last_error_kind as string | null;
    const blocked = st?.blocked_reason as string | null;
    const cls = blocked || err ? "err" : pending > 0 ? "warn" : "ok";
    // A version mismatch (the hub answered 426 / another sync protocol) needs an
    // update, not a network fix, so it is never shown as "disconnected".
    const text = blocked
      ? t("Sync paused")
      : err
        ? kind === "version_mismatch"
          ? t("Update needed")
          : kind === "unreachable"
            ? t("Hub unreachable")
            : kind === "auth"
              ? t("Pair again")
              : t("Sync error")
        : pending > 0
          ? t("{0} pending", pending)
          : t("Synced");
    return (
      <span
        className={`status-pill ${cls}`}
        title={tb(blocked ?? err) || t("Local checkout is always available. Changes sync automatically.")}
        data-testid="sync-pill"
        data-kind={kind ?? undefined}
      >
        <span className="dot" aria-hidden /> {text}
      </span>
    );
  }
  return (
    <span className="status-pill" title={t("Local checkout is available. Online services will resume automatically.")}>
      <span className="dot" aria-hidden style={{ color: online ? "#22c55e" : "#94a3b8" }} />{" "}
      {online ? (mode === "hub" ? t("Hub") : t("Local")) : t("Offline")}
    </span>
  );
}
