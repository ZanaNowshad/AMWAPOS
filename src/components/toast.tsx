import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import { AlertTriangle, CheckCircle2, Info, X, XCircle } from "lucide-react";

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
  const dismiss = useCallback((id: number) => setToasts((t) => t.filter((x) => x.id !== id)), []);
  const push = useCallback(
    (tone: Tone, title: string, body?: string) => {
      setToasts((cur) => {
        // Collapse duplicates.
        const dup = cur.find((t) => t.title === title && t.body === body && t.tone === tone);
        if (dup) return cur.map((t) => (t === dup ? { ...t, count: t.count + 1 } : t));
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
        {toasts.map((t) => {
          const Icon = icons[t.tone];
          return (
            <div key={t.id} className={`toast ${t.tone}`} role={t.tone === "error" ? "alert" : "status"}>
              <Icon size={18} className="t-icon" aria-hidden />
              <div className="grow">
                <div className="t-title">
                  {t.title}
                  {t.count > 1 ? ` (${t.count})` : ""}
                </div>
                {t.body ? <div className="small muted">{t.body}</div> : null}
              </div>
              <button className="btn ghost sm icon" aria-label="Dismiss" onClick={() => dismiss(t.id)}>
                <X size={14} />
              </button>
            </div>
          );
        })}
      </div>
    </Ctx.Provider>
  );
}
