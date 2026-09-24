import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import { AlertTriangle, CheckCircle2, Info, X, XCircle } from "lucide-react";
import { t } from "../i18n";

type Tone = "success" | "error" | "warning" | "info";
interface Toast {
  id: number;
  tone: Tone;
  title: string;
  body?: string;
  count: number;
}

const Ctx = createContext<(tone: Tone, title: string, body?: string) => void>(() => {});
export const useToast = () => useContext(Ctx);

const icons = { success: CheckCircle2, error: XCircle, warning: AlertTriangle, info: Info };

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const seq = useRef(0);
  const dismiss = useCallback((id: number) => setToasts((tv) => tv.filter((x) => x.id !== id)), []);
  const push = useCallback(
    (tone: Tone, title: string, body?: string) => {
      setToasts((cur) => {
        // Collapse duplicates.
        const dup = cur.find((tv) => tv.title === title && tv.body === body && tv.tone === tone);
        if (dup) return cur.map((tv) => (tv === dup ? { ...tv, count: tv.count + 1 } : tv));
        const id = ++seq.current;
        const ttl = tone === "success" ? 3000 : tone === "info" ? 4000 : 8000;
        setTimeout(() => dismiss(id), ttl);
        return [...cur, { id, tone, title, body, count: 1 }].slice(-4);
      });
    },
    [dismiss],
  );
  const value = useMemo(() => push, [push]);
  return (
    <Ctx.Provider value={value}>
      {children}
      <div className="toasts" aria-live="polite">
        {toasts.map((tv) => {
          const Icon = icons[tv.tone];
          return (
            <div key={tv.id} className={`toast ${tv.tone}`} role={tv.tone === "error" ? "alert" : "status"}>
              <Icon size={18} className="t-icon" aria-hidden />
              <div className="grow">
                <div className="t-title">
                  {tv.title}
                  {tv.count > 1 ? ` (${tv.count})` : ""}
                </div>
                {tv.body ? <div className="small muted">{tv.body}</div> : null}
              </div>
              <button className="btn ghost sm icon" aria-label={t("Dismiss")} onClick={() => dismiss(tv.id)}>
                <X size={14} />
              </button>
            </div>
          );
        })}
      </div>
    </Ctx.Provider>
  );
}
