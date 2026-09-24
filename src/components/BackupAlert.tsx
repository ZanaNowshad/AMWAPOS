import { useCallback, useEffect, useState } from "react";
import { DatabaseBackup } from "lucide-react";
import { api } from "../api";
import type { DiagnosticItem } from "../api/types";
import { t, tb } from "../i18n";
import { explain } from "../lib/errors";
import { useSession } from "../state/session";
import { useToast } from "./toast";
import { Banner, Button } from "./ui";

const POLL_MS = 5 * 60 * 1000;

/** Backup health, refreshed every few minutes and after a manual backup. */
function useBackupHealth() {
  const [health, setHealth] = useState<DiagnosticItem | null>(null);
  const refresh = useCallback(async () => {
    try {
      setHealth(await api.backup.health());
    } catch {
      /* not signed in / backend busy: keep the last known state */
    }
  }, []);
  useEffect(() => {
    void refresh();
    const id = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(id);
  }, [refresh]);
  return { health, refresh };
}

function useBackupNow(onDone: () => Promise<void>) {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    try {
      await api.backup.create();
      toast("success", t("Backup created and verified"));
      await onDone();
    } catch (e) {
      const ex = explain(e);
      toast("error", ex.title, `${ex.message} ${ex.action}`.trim());
    } finally {
      setBusy(false);
    }
  };
  return { run, busy };
}

/**
 * Admin: a full-width banner on every page while backups are overdue or failing,
 * with one-click Backup Now. Automatic backups only run while AMWAPOS is open
 * (see docs/OPERATIONS.md), so this must be impossible to miss.
 */
export function BackupAlert() {
  const { has } = useSession();
  const { health, refresh } = useBackupHealth();
  const now = useBackupNow(refresh);
  if (!health || health.state === "ok") return null;
  return (
    <div className="backup-alert" data-testid="backup-alert" role="alert">
      <Banner
        tone={health.state === "error" ? "danger" : "warning"}
        title={health.state === "error" ? t("Backup failed") : t("Backup overdue")}
        action={
          has("backup.manage") ? (
            <Button
              variant="primary"
              icon={<DatabaseBackup size={16} />}
              loading={now.busy}
              onClick={() => void now.run()}
            >
              {t("Backup Now")}
            </Button>
          ) : null
        }
      >
        {tb(health.summary)}.{" "}
        {t("Automatic backups run only while AMWAPOS is open on this computer. Keep the hub running, or back up now.")}
      </Banner>
    </div>
  );
}

/** Cashier header: a red pill while backups are overdue; managers can back up from it. */
export function BackupPill() {
  const { has } = useSession();
  const { health, refresh } = useBackupHealth();
  const now = useBackupNow(refresh);
  if (!health || health.state === "ok") return null;
  const label = health.state === "error" ? t("Backup failed") : t("Backup overdue");
  if (!has("backup.manage"))
    return (
      <span className="status-pill err" data-testid="backup-pill" title={tb(health.summary)}>
        <DatabaseBackup size={13} /> {label}
      </span>
    );
  return (
    <button
      type="button"
      className="status-pill err"
      data-testid="backup-pill"
      title={`${tb(health.summary)} — ${t("Backup Now")}`}
      disabled={now.busy}
      onClick={() => void now.run()}
    >
      <DatabaseBackup size={13} /> {now.busy ? t("Backing up…") : `${label} · ${t("Backup Now")}`}
    </button>
  );
}
