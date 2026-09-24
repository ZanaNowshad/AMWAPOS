import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, setToken } from "../api";
import { ApiError } from "../api/transport";
import type { PosConfig, Session, SetupStatus } from "../api/types";
import { configureMoney } from "../lib/money";
import { configureTimezone } from "../lib/time";

export type Mode = "cashier" | "admin";

interface SessionCtx {
  status: SetupStatus;
  refreshStatus: () => Promise<void>;
  session: Session | null;
  config: PosConfig | null;
  locked: boolean;
  mode: Mode;
  setMode: (m: Mode) => void;
  has: (perm: string) => boolean;
  login: (userId: string, pin: string) => Promise<void>;
  logout: () => Promise<void>;
  lock: () => Promise<void>;
  unlock: (pin: string) => Promise<void>;
  switchUser: () => Promise<void>;
  reloadConfig: () => Promise<void>;
  handleAuthError: (e: unknown) => boolean;
}

const Ctx = createContext<SessionCtx | null>(null);

export function useSession(): SessionCtx {
  const c = useContext(Ctx);
  if (!c) throw new Error("SessionProvider missing");
  return c;
}

const TOKEN_KEY = "amwapos.session";

export function SessionProvider({ initialStatus, children }: { initialStatus: SetupStatus; children: ReactNode }) {
  const [status, setStatus] = useState(initialStatus);
  const [session, setSession] = useState<Session | null>(null);
  const [config, setConfig] = useState<PosConfig | null>(null);
  const [locked, setLocked] = useState(false);
  const [mode, setModeState] = useState<Mode>("cashier");
  const lastActivity = useRef(Date.now());

  const applyConfig = useCallback((c: PosConfig) => {
    configureMoney(c.currency, c.currency_digits);
    configureTimezone(c.timezone);
    document.documentElement.dataset.theme = c.appearance.theme === "dark" ? "dark" : c.appearance.theme === "system" && matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
    document.documentElement.dataset.density = c.appearance.density || "comfortable";
    document.documentElement.dataset.cashierFont = c.appearance.cashier_font || "normal";
    setConfig(c);
  }, []);

  const reloadConfig = useCallback(async () => {
    applyConfig(await api.pos.config());
  }, [applyConfig]);

  const clear = useCallback(() => {
    setToken(null);
    sessionStorage.removeItem(TOKEN_KEY);
    setSession(null);
    setLocked(false);
    setModeState("cashier");
  }, []);

  // Resume a session after a UI reload (the backend keeps sessions in memory).
  useEffect(() => {
    const t = sessionStorage.getItem(TOKEN_KEY);
    if (!t) return;
    setToken(t);
    api.auth
      .session()
      .then(async (s) => {
        setSession(s);
        setLocked(s.locked);
        await reloadConfig();
      })
      .catch(() => clear());
  }, [clear, reloadConfig]);

  const login = useCallback(
    async (userId: string, pin: string) => {
      const r = await api.auth.login(userId, pin);
      setToken(r.token);
      sessionStorage.setItem(TOKEN_KEY, r.token);
      setSession(r.session);
      setLocked(false);
      lastActivity.current = Date.now();
      await reloadConfig();
      setModeState("cashier");
    },
    [reloadConfig],
  );

  const logout = useCallback(async () => {
    try {
      await api.auth.logout();
    } finally {
      clear();
    }
  }, [clear]);

  const lock = useCallback(async () => {
    try {
      await api.auth.lock();
    } catch {
      /* lock locally anyway */
    }
    setLocked(true);
  }, []);

  const unlock = useCallback(async (pin: string) => {
    const s = await api.auth.unlock(pin);
    setSession(s);
    setLocked(false);
    lastActivity.current = Date.now();
  }, []);

  const switchUser = useCallback(async () => {
    await logout();
  }, [logout]);

  const has = useCallback((perm: string) => !!session?.permissions.includes(perm), [session]);

  const setMode = useCallback(
    (m: Mode) => {
      if (m === "admin" && !session?.permissions.includes("admin.access")) return;
      setModeState(m);
    },
    [session],
  );

  const handleAuthError = useCallback(
    (e: unknown) => {
      if (e instanceof ApiError && e.code === "unauthenticated") {
        if (e.details && (e.details as { locked?: boolean }).locked) {
          setLocked(true);
        } else {
          clear();
        }
        return true;
      }
      return false;
    },
    [clear],
  );

  // Client-side idle lock (the backend enforces the same policy).
  useEffect(() => {
    if (!session || locked) return;
    const minutes = config?.pos.idle_lock_minutes ?? 10;
    if (minutes <= 0) return;
    const bump = () => (lastActivity.current = Date.now());
    window.addEventListener("keydown", bump, true);
    window.addEventListener("pointerdown", bump, true);
    const t = setInterval(() => {
      if (Date.now() - lastActivity.current > minutes * 60_000) void lock();
    }, 5000);
    return () => {
      window.removeEventListener("keydown", bump, true);
      window.removeEventListener("pointerdown", bump, true);
      clearInterval(t);
    };
  }, [session, locked, config, lock]);

  const refreshStatus = useCallback(async () => {
    setStatus(await api.setup.status());
  }, []);

  const value = useMemo<SessionCtx>(
    () => ({ status, refreshStatus, session, config, locked, mode, setMode, has, login, logout, lock, unlock, switchUser, reloadConfig, handleAuthError }),
    [status, refreshStatus, session, config, locked, mode, setMode, has, login, logout, lock, unlock, switchUser, reloadConfig, handleAuthError],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
