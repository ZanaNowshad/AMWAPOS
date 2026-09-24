import { useCallback, useEffect, useState } from "react";
import { api } from "../../api";
import type { ShiftSummary } from "../../api/types";
import { useSession } from "../../state/session";
import { Skeleton } from "../../components/ui";
import { PosScreen } from "./PosScreen";
import { ShiftOpen } from "./ShiftScreens";

/** Cashier Mode: a dedicated checkout workstation. A shift is required to sell. */
export function CashierMode() {
  const { handleAuthError } = useSession();
  const [shift, setShift] = useState<ShiftSummary | null | undefined>(undefined);
  const load = useCallback(async () => {
    try {
      setShift(await api.shift.current());
    } catch (e) {
      if (!handleAuthError(e)) setShift(null);
    }
  }, [handleAuthError]);
  useEffect(() => {
    void load();
  }, [load]);
  if (shift === undefined) return <Skeleton rows={8} />;
  if (shift === null) return <ShiftOpen onOpened={setShift} />;
  return <PosScreen shift={shift} onShiftClosed={() => setShift(null)} reloadShift={load} />;
}
