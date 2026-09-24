import { useCallback, useEffect, useState } from "react";
import { api } from "./api";
import type { SetupStatus } from "./api/types";
import { ApprovalProvider } from "./components/approval";
import { ToastProvider } from "./components/toast";
import { Startup, StartupFailure } from "./screens/boot/Startup";
import { SetupWizard } from "./screens/setup/SetupWizard";
import { SessionProvider, useSession } from "./state/session";
import { LoginScreen } from "./screens/login/Login";
import { LockScreen } from "./screens/login/LockScreen";
import { CashierMode } from "./screens/pos/CashierMode";
import { AdminShell } from "./screens/admin/AdminShell";
import { DeliveryDesk } from "./screens/pos/DeliveryDesk";

type Boot = { phase: "loading"; stage: string } | { phase: "failed"; error: unknown } | { phase: "ready"; status: SetupStatus };

export default function App() {
  const [boot, setBoot] = useState<Boot>({ phase: "loading", stage: "Opening database" });
  const start = useCallback(async () => {
    setBoot({ phase: "loading", stage: "Opening database" });
    try {
      const t0 = Date.now();
      const status = await api.setup.status();
      setBoot({ phase: "loading", stage: "Checking schema" });
      // Avoid flashing the splash when startup is quick.
      const elapsed = Date.now() - t0;
      if (elapsed < 300) await new Promise((r) => setTimeout(r, 0));
      setBoot({ phase: "ready", status });
    } catch (e) {
      setBoot({ phase: "failed", error: e });
    }
  }, []);
  useEffect(() => {
    void start();
  }, [start]);

  if (boot.phase === "loading") return <Startup stage={boot.stage} />;
  if (boot.phase === "failed") return <StartupFailure error={boot.error} onRetry={start} />;
  return (
    <ToastProvider>
      <SessionProvider initialStatus={boot.status}>
        <ApprovalProvider>
          <Root />
        </ApprovalProvider>
      </SessionProvider>
    </ToastProvider>
  );
}

function Root() {
  const { status, session, locked, mode, has, refreshStatus } = useSession();
  if (!status.setup_complete || !status.device) {
    return <SetupWizard onDone={refreshStatus} />;
  }
  if (!session) return <LoginScreen />;
  const canSell = has("pos.sell");
  const canAdmin = has("admin.access");
  let content;
  if ((mode === "admin" && canAdmin) || (!canSell && canAdmin)) content = <AdminShell />;
  else if (canSell) content = <CashierMode />;
  else content = <DeliveryDesk />;
  return (
    <>
      {content}
      {locked ? <LockScreen /> : null}
    </>
  );
}
