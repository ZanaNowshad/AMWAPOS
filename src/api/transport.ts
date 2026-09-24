import type { AppErrorShape } from "./types";

/** Error thrown by every API call. Mirrors the Rust AppError. */
export class ApiError extends Error implements AppErrorShape {
  code: AppErrorShape["code"];
  data_changed: boolean;
  retryable: boolean;
  details?: Record<string, unknown>;
  constructor(e: AppErrorShape) {
    super(e.message);
    this.code = e.code;
    this.data_changed = e.data_changed;
    this.retryable = e.retryable;
    this.details = e.details;
  }
}

type Invoke = (cmd: string, args: Record<string, unknown>) => Promise<unknown>;

let tauriInvoke: Invoke | null | undefined;

async function getTauriInvoke(): Promise<Invoke | null> {
  if (tauriInvoke !== undefined) return tauriInvoke;
  const w = window as unknown as { __TAURI_INTERNALS__?: unknown };
  if (w.__TAURI_INTERNALS__) {
    const mod = await import("@tauri-apps/api/core");
    tauriInvoke = mod.invoke as Invoke;
  } else {
    tauriInvoke = null;
  }
  return tauriInvoke;
}

export const isDesktop = () => Boolean((window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__);

function asApiError(e: unknown): ApiError {
  if (e instanceof ApiError) return e;
  if (e && typeof e === "object" && "code" in e && "message" in e) return new ApiError(e as AppErrorShape);
  const msg = e instanceof Error ? e.message : String(e);
  return new ApiError({
    code: "transport",
    message: `The application backend did not respond (${msg}). No change was confirmed.`,
    data_changed: false,
    retryable: true,
  });
}

/**
 * Send one command to the backend. Uses Tauri IPC in the desktop app and the
 * loopback development bridge in a browser.
 */
export async function send<T>(cmd: string, token: string | null, args: object = {}): Promise<T> {
  const inv = await getTauriInvoke();
  try {
    if (inv) {
      return (await inv("rpc", { cmd, token, args })) as T;
    }
    const res = await fetch("/rpc", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ cmd, token, args }),
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const body = (await res.json()) as { ok: boolean; data?: T; error?: AppErrorShape };
    if (!body.ok) throw new ApiError(body.error!);
    return body.data as T;
  } catch (e) {
    throw asApiError(e);
  }
}
