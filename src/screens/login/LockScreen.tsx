import { useState } from "react";
import { Lock } from "lucide-react";
import { useSession } from "../../state/session";
import { explain } from "../../lib/errors";
import { Button } from "../../components/ui";
import { PinEntry, initials } from "./Login";
import { t } from "../../i18n";

export function LockScreen() {
  const { session, unlock, switchUser } = useSession();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  if (!session) return null;
  const submit = async (pin: string) => {
    setBusy(true);
    setError(null);
    try {
      await unlock(pin);
    } catch (e) {
      setError(explain(e).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="lock-overlay" role="dialog" aria-modal="true" aria-label={t("Terminal locked")}>
      <div className="lock-card">
        <div className="col" style={{ alignItems: "center", marginBottom: 12 }}>
          <Lock size={22} color="var(--text-2)" />
          <span className="avatar">{initials(session.display_name)}</span>
          <strong>{session.display_name}</strong>
          <span className="tiny">{t("Terminal locked · the current sale is kept")}</span>
        </div>
        <PinEntry title={t("Enter your PIN to unlock")} onSubmit={submit} busy={busy} error={error} />
        <Button variant="ghost" block style={{ marginTop: 8 }} onClick={() => void switchUser()}>
          {t("Switch User")}
        </Button>
      </div>
    </div>
  );
}
