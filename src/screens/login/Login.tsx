import { useEffect, useRef, useState } from "react";
import { ArrowLeft, Lock, Search } from "lucide-react";
import { api } from "../../api";
import type { LoginUser } from "../../api/types";
import { useSession } from "../../state/session";
import { explain } from "../../lib/errors";
import { Banner, Button, Keypad } from "../../components/ui";
import { Logo } from "../../components/Logo";
import { ConnectionPill } from "../pos/ConnectionPill";

export function initials(name: string) {
  return name
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((p) => p[0]!.toUpperCase())
    .join("");
}

export function PinEntry({ onSubmit, busy, error, title, autoFocus = true }: { onSubmit: (pin: string) => void; busy: boolean; error: string | null; title: string; autoFocus?: boolean }) {
  const [pin, setPin] = useState("");
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (error) setPin("");
  }, [error]);
  const key = (k: string) => {
    if (k === "Backspace") setPin((p) => p.slice(0, -1));
    else if (/^\d$/.test(k)) setPin((p) => (p.length < 12 ? p + k : p));
    ref.current?.focus();
  };
  return (
    <div className="col gap-16">
      <div style={{ textAlign: "center", fontWeight: 600 }}>{title}</div>
      <input
        ref={ref}
        aria-label="PIN"
        className="input lg"
        style={{ textAlign: "center", letterSpacing: "0.4em" }}
        type="password"
        inputMode="numeric"
        autoComplete="off"
        value={pin}
        autoFocus={autoFocus}
        onChange={(e) => setPin(e.target.value.replace(/\D/g, "").slice(0, 12))}
        onKeyDown={(e) => {
          if (e.key === "Enter" && pin.length >= 4) onSubmit(pin);
        }}
      />
      <Keypad onKey={key} extra="" />
      {error ? <Banner tone="danger">{error}</Banner> : null}
      <Button variant="primary" size="lg" block onClick={() => onSubmit(pin)} disabled={pin.length < 4} loading={busy}>
        Log in
      </Button>
    </div>
  );
}

export function LoginScreen() {
  const { status, login } = useSession();
  const [users, setUsers] = useState<LoginUser[] | null>(null);
  const [selected, setSelected] = useState<LoginUser | null>(null);
  const [filter, setFilter] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const load = () =>
    api.auth
      .users()
      .then(setUsers)
      .catch((e) => setError(explain(e).message));
  useEffect(() => {
    void load();
  }, []);
  const submit = async (pin: string) => {
    if (!selected) return;
    setBusy(true);
    setError(null);
    try {
      await login(selected.user_id, pin);
    } catch (e) {
      setError(explain(e).message);
      void load();
    } finally {
      setBusy(false);
    }
  };
  const shown = (users ?? []).filter((u) => u.display_name.toLowerCase().includes(filter.toLowerCase()));
  return (
    <div className="login">
      <section className="login-brand">
        <div className="row">
          <Logo size={36} />
          <strong style={{ fontSize: 18 }}>AMWAPOS</strong>
        </div>
        <h1>Fast retail. Accurate operations.</h1>
        <div className="meta">
          <div>AMWAPOS Terminal</div>
          <div>
            {status.business_name} · {status.device?.name} ({status.device?.device_code})
          </div>
          <div>Version {status.app_version}</div>
          <div style={{ marginTop: 8 }}>
            <ConnectionPill />
          </div>
        </div>
      </section>
      <section className="login-main">
        <div className="login-panel">
          {!selected ? (
            <div className="stack-24">
              <div>
                <h1>Who is signing in?</h1>
                <div className="muted">Select your name, then enter your PIN.</div>
              </div>
              {users && users.length > 8 ? (
                <div className="scan-box">
                  <Search size={18} className="scan-icon" />
                  <input className="input" placeholder="Search staff…" value={filter} onChange={(e) => setFilter(e.target.value)} autoFocus />
                </div>
              ) : null}
              <div className="user-tiles">
                {shown.map((u) => (
                  <button key={u.user_id} className={`user-tile ${u.locked ? "locked" : ""}`} onClick={() => (setSelected(u), setError(null))}>
                    <span className="avatar">{initials(u.display_name)}</span>
                    <span style={{ fontWeight: 600 }}>{u.display_name}</span>
                    <span className="tiny">
                      {u.locked ? (
                        <span className="row" style={{ gap: 4 }}>
                          <Lock size={12} /> Locked
                        </span>
                      ) : (
                        u.role_name
                      )}
                    </span>
                  </button>
                ))}
              </div>
              {error ? <Banner tone="danger">{error}</Banner> : null}
            </div>
          ) : (
            <div className="pin-panel">
              <Button variant="ghost" icon={<ArrowLeft size={16} />} onClick={() => (setSelected(null), setError(null))}>
                Back
              </Button>
              <div className="col" style={{ alignItems: "center", margin: "12px 0 16px" }}>
                <span className="avatar">{initials(selected.display_name)}</span>
                <strong>{selected.display_name}</strong>
                <span className="tiny">{selected.role_name}</span>
              </div>
              {selected.locked ? <Banner tone="danger">This account is locked after too many incorrect PINs. Ask a manager to unlock it, or wait.</Banner> : null}
              <PinEntry title="Enter your PIN" onSubmit={submit} busy={busy} error={error} />
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
