import { AlertOctagon } from "lucide-react";
import { explain } from "../../lib/errors";
import { ApiError } from "../../api/transport";
import { Button } from "../../components/ui";
import { Logo } from "../../components/Logo";
import { t } from "../../i18n";

export function Startup({ stage }: { stage: string }) {
  return (
    <div className="splash">
      <Logo size={56} light />
      <div className="splash-name">{t("AMWAPOS")}</div>
      <div className="splash-sub">{t("Retail Operations System")}</div>
      <div className="splash-bar" role="progressbar" aria-label={t("Starting")}>
        <span />
      </div>
      <div className="splash-stage">{stage}</div>
    </div>
  );
}

export function StartupFailure({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  const ex = explain(error);
  const apiErr = error instanceof ApiError ? error : null;
  const recovery = apiErr?.code === "database_corrupt" || apiErr?.code === "not_found";
  return (
    <div className="splash failure">
      <div className="failure-card">
        <AlertOctagon size={44} color="var(--danger)" />
        <h1>{t("AMWAPOS could not start")}</h1>
        <p className="muted" style={{ maxWidth: 520, textAlign: "center" }}>
          {ex.message}
        </p>
        <p className="small">{t("No business data has been modified.")}</p>
        <div className="row" style={{ justifyContent: "center", marginTop: 8 }}>
          <Button variant="primary" onClick={onRetry}>
            {t("Retry")}
          </Button>
          {recovery ? (
            <span className="small muted">
              {t(
                "To restore, open the data folder listed below and copy a verified backup into place, or reinstall and restore from Admin → Backups.",
              )}
            </span>
          ) : null}
        </div>
        <details className="tech" style={{ marginTop: 16, width: "100%" }}>
          <summary>{t("Technical details")}</summary>
          <pre>
            {JSON.stringify(
              apiErr ? { code: apiErr.code, message: apiErr.message, details: apiErr.details } : String(error),
              null,
              2,
            )}
          </pre>
        </details>
      </div>
    </div>
  );
}
