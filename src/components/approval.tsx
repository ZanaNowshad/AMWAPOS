import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react";
import { ShieldCheck } from "lucide-react";
import { api } from "../api";
import { ApiError } from "../api/transport";
import type { LoginUser } from "../api/types";
import { Banner, Button, Keypad, Modal } from "./ui";

export class ApprovalCancelled extends Error {
  constructor() {
    super("Approval cancelled");
  }
}

type Runner = <T>(fn: (approvalToken: string | null) => Promise<T>) => Promise<T>;
const Ctx = createContext<Runner>((fn) => fn(null));

/** Run `fn`; if the backend requires a manager approval, ask for it and retry once with the token. */
export const useApproval = () => useContext(Ctx);

interface Pending {
  permission: string;
  summary: string;
  resolve: (token: string) => void;
  reject: (e: unknown) => void;
}

export function ApprovalProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<Pending | null>(null);
  const run: Runner = useCallback(async (fn) => {
    try {
      return await fn(null);
    } catch (e) {
      const needs =
        e instanceof ApiError &&
        (e.code === "approval_required" || e.code === "insufficient_stock") &&
        typeof e.details?.permission === "string";
      if (!needs) throw e;
      const err = e as ApiError;
      const token = await new Promise<string>((resolve, reject) =>
        setPending({
          permission: err.details!.permission as string,
          summary: (err.details!.summary as string) || err.message,
          resolve,
          reject,
        }),
      );
      return fn(token);
    }
  }, []);
  return (
    <Ctx.Provider value={run}>
      {children}
      {pending ? (
        <ApprovalDialog
          pending={pending}
          onDone={(tok) => {
            setPending(null);
            pending.resolve(tok);
          }}
          onCancel={() => {
            setPending(null);
            pending.reject(new ApprovalCancelled());
          }}
        />
      ) : null}
    </Ctx.Provider>
  );
}

function ApprovalDialog({ pending, onDone, onCancel }: { pending: Pending; onDone: (t: string) => void; onCancel: () => void }) {
  const [approvers, setApprovers] = useState<LoginUser[] | null>(null);
  const [who, setWho] = useState<string>("");
  const [pin, setPin] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    api.auth
      .approvers(pending.permission)
      .then((a) => {
        setApprovers(a);
        if (a.length === 1) setWho(a[0].user_id);
      })
      .catch((e) => setError(e.message));
  }, [pending.permission]);
  const submit = async () => {
    if (!who || pin.length < 4) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.auth.approve(who, pin, pending.permission, pending.summary);
      onDone(r.approval_token);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setPin("");
    } finally {
      setBusy(false);
    }
  };
  const key = (k: string) => {
    if (k === "Backspace") setPin((p) => p.slice(0, -1));
    else if (/^\d$/.test(k) && pin.length < 12) setPin((p) => p + k);
  };
  return (
    <Modal
      size="sm"
      title={
        <span className="row">
          <ShieldCheck size={20} color="var(--brand)" /> Manager Approval
        </span>
      }
      onClose={onCancel}
      footer={
        <>
          <Button onClick={onCancel}>Cancel</Button>
          <Button variant="primary" className="right" onClick={submit} loading={busy} disabled={!who || pin.length < 4}>
            Approve
          </Button>
        </>
      }
    >
      <div className="col gap-16">
        <div className="banner info">
          <div>
            <div className="tiny">Action</div>
            <div style={{ fontWeight: 600 }}>{pending.summary}</div>
          </div>
        </div>
        {approvers && approvers.length === 0 ? (
          <Banner tone="warning">No active user is allowed to approve this action.</Banner>
        ) : null}
        <div className="field">
          <label htmlFor="approver">Approved by</label>
          <select id="approver" className="select" value={who} onChange={(e) => setWho(e.target.value)}>
            <option value="">Select manager…</option>
            {(approvers ?? []).map((a) => (
              <option key={a.user_id} value={a.user_id}>
                {a.display_name} · {a.role_name}
              </option>
            ))}
          </select>
        </div>
        <div className="field">
          <label htmlFor="approver-pin">Manager PIN</label>
          <input
            id="approver-pin"
            className="input lg"
            type="password"
            inputMode="numeric"
            autoComplete="off"
            value={pin}
            autoFocus
            onChange={(e) => setPin(e.target.value.replace(/\D/g, "").slice(0, 12))}
            onKeyDown={(e) => e.key === "Enter" && submit()}
          />
        </div>
        <Keypad onKey={key} extra="" />
        {error ? <Banner tone="danger">{error}</Banner> : null}
      </div>
    </Modal>
  );
}
