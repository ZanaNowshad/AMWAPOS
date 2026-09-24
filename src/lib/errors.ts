import { ApiError } from "../api/transport";

/** Human explanation: what happened, whether data changed, what to do. */
export function explain(e: unknown): { title: string; message: string; action: string } {
  const err = e instanceof ApiError ? e : null;
  const message = err?.message ?? (e instanceof Error ? e.message : String(e));
  const changed = err?.data_changed ? "Some data may have been saved." : "No changes were saved.";
  switch (err?.code) {
    case "validation":
      return { title: "Please check the details", message, action: "" };
    case "forbidden":
      return { title: "Not allowed", message: "You do not have permission to perform this action.", action: "Ask a manager." };
    case "duplicate":
      return { title: "Already exists", message, action: changed };
    case "conflict":
      return { title: "Action not possible", message, action: changed };
    case "database_busy":
      return { title: "The store database is busy", message, action: "Try again in a moment." };
    case "transport":
      return { title: "Could not reach the application service", message, action: "Try again. If it keeps failing, restart AMWAPOS." };
    case "unauthenticated":
      return { title: "Session ended", message, action: "" };
    case "insufficient_disk":
      return { title: "Not enough disk space", message, action: changed };
    case "sync":
      return { title: "Sync problem", message, action: "Local work continues." };
    default:
      return { title: "Something needs attention", message, action: changed };
  }
}

export function isCode(e: unknown, code: string): boolean {
  return e instanceof ApiError && e.code === code;
}
