import { ApiError } from "../api/transport";
import { t } from "../i18n";

/** Human explanation: what happened, whether data changed, what to do. */
export function explain(e: unknown): { title: string; message: string; action: string } {
  const err = e instanceof ApiError ? e : null;
  const message = err?.message ?? (e instanceof Error ? e.message : String(e));
  const changed = err?.data_changed ? t("Some data may have been saved.") : t("No changes were saved.");
  switch (err?.code) {
    case "validation":
      return { title: t("Please check the details"), message, action: "" };
    case "forbidden":
      return {
        title: t("Not allowed"),
        message: t("You do not have permission to perform this action."),
        action: t("Ask a manager."),
      };
    case "duplicate":
      return { title: t("Already exists"), message, action: changed };
    case "conflict":
      return { title: t("Action not possible"), message, action: changed };
    case "database_busy":
      return { title: t("The store database is busy"), message, action: t("Try again in a moment.") };
    case "transport":
      return {
        title: t("Could not reach the application service"),
        message,
        action: t("Try again. If it keeps failing, restart AMWAPOS."),
      };
    case "unauthenticated":
      return { title: t("Session ended"), message, action: "" };
    case "insufficient_disk":
      return { title: t("Not enough disk space"), message, action: changed };
    case "sync":
      return { title: t("Sync problem"), message, action: t("Local work continues.") };
    case "AI_NOT_ENABLED":
      return {
        title: t("The AI assistant is off"),
        message,
        action: t("An owner can turn it on in Settings → Features."),
      };
    case "AI_NO_KEY":
      return { title: t("No AI key stored"), message, action: t("An owner can add an API key in Settings → AI.") };
    case "AI_PROVIDER_ERROR":
      return { title: t("The AI provider returned an error"), message, action: t("No store data was changed.") };
    case "AI_TIMEOUT":
      return { title: t("The AI provider did not answer in time"), message, action: t("Try again in a moment.") };
    case "AI_MODEL_NOT_FOUND":
      return { title: t("AI model not found"), message, action: t("Choose another model in Settings → AI.") };
    default:
      return { title: t("Something needs attention"), message, action: changed };
  }
}

export function isCode(e: unknown, code: string): boolean {
  return e instanceof ApiError && e.code === code;
}
