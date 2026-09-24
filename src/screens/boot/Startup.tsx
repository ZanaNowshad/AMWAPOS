import { AlertOctagon } from "lucide-react";
import { explain } from "../../lib/errors";
import { ApiError } from "../../api/transport";
import { Button } from "../../components/ui";
import { Logo } from "../../components/Logo";

export function Startup({ stage }: { stage: string }) {
  return (
    <div className="splash">
      <Logo size={56} light />
      <div className="splash-name">AMWAPOS</div>
      <div className="splash-sub">Retail Operations System</div>
      <div className="splash-bar" role="progressbar" aria-label="Starting">
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
        <h1>AMWAPOS could not start</h1>
        <p className="muted" style={{ maxWidth: 520, textAlign: "center" }}>
          {ex.message}
        </p>
        <p className="small">No business data has been modified.</p>
        <div className="row" style={{ justifyContent: "center", marginTop: 8 }}>
          <Button variant="primary" onClick={onRetry}>
            Retry
          </Button>
          {recovery ? <span className="small muted">To restore, open the data folder listed below and copy a verified backup into place, or reinstall and restore from Admin → Backups.</span> : null}
        </div>
        <details className="tech" style={{ marginTop: 16, width: "100%" }}>
          <summary>Technical details</summary>
          <pre>{JSON.stringify(apiErr ? { code: apiErr.code, message: apiErr.message, details: apiErr.details } : String(error), null, 2)}</pre>
        </details>
      </div>
    </div>
  );
}
