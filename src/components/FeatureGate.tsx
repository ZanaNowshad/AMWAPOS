import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import type { FeatureName } from "../api/types";
import { useSession } from "../state/session";
import { Banner } from "./ui";
import { t } from "../i18n";

/** True when the owner has switched the module on in Settings → Features. */
export function useFeature(name: FeatureName): boolean {
  const { config } = useSession();
  const f = config?.features;
  if (!f) return false;
  // A sub-feature counts only when every module above it is on too.
  let n: FeatureName | undefined = name;
  while (n) {
    if (!f[n] || !(FEATURE_ALSO[n] ?? []).every((x) => !!f[x])) return false;
    n = FEATURE_PARENT[n];
  }
  return true;
}

/** Other modules a feature needs besides its parent. */
export const FEATURE_ALSO: Partial<Record<FeatureName, FeatureName[]>> = {
  "orders.whatsapp_ai": ["whatsapp.enabled"],
};

/** A sub-feature counts only when its parent module is on. */
export const FEATURE_PARENT: Partial<Record<FeatureName, FeatureName>> = {
  "ai.mutations": "ai.enabled",
  "ai.dual_control": "ai.mutations",
  "ocr.ai_parse": "ocr.supplier_invoices",
  "whatsapp.send_receipts": "whatsapp.enabled",
  "whatsapp.delivery_notices": "whatsapp.enabled",
  "ocr.payment_screenshots": "ocr.enabled",
  "ocr.supplier_invoices": "ocr.enabled",
  "orders.whatsapp_ai": "orders.digital",
  "orders.whatsapp_upsell": "orders.whatsapp_ai",
};

export const FEATURE_LABELS: Record<FeatureName, () => string> = {
  hub: () => t("Hub (multi-terminal)"),
  "whatsapp.enabled": () => t("WhatsApp (unofficial Web client)"),
  "whatsapp.send_receipts": () => t("WhatsApp: send receipts after a sale"),
  "whatsapp.delivery_notices": () => t("WhatsApp: delivery notices"),
  "ocr.enabled": () => t("Local OCR"),
  "ocr.payment_screenshots": () => t("OCR: payment screenshot reviews"),
  "ocr.supplier_invoices": () => t("OCR: supplier documents (invoices, credit notes, delivery notes)"),
  "ocr.ai_parse": () => t("OCR: AI help for unresolved document values"),
  "ai.enabled": () => t("AI assistant (read-only questions)"),
  "ai.mutations": () => t("AI proposed changes (preview and confirm)"),
  "ai.dual_control": () => t("AI: two people confirm high-risk changes"),
  "customers.credit": () => t("Customer credit accounts"),
  windows_hello: () => t("Windows Hello step-up"),
  pdf_receipts: () => t("PDF receipts"),
  updates: () => t("Automatic update checks"),
  "inventory.locations": () => t("Stock locations and transfers"),
  "loyalty.enabled": () => t("Loyalty points"),
  "orders.digital": () => t("Digital orders (phone, WhatsApp, web)"),
  "orders.whatsapp_ai": () => t("WhatsApp orders: read chats into draft orders"),
  "orders.whatsapp_upsell": () => t("WhatsApp orders: suggest related items to staff"),
  "org.multi_branch": () => t("Multiple branches"),
  "pwa.companion": () => t("Owner phone view"),
};

/** Shows `children` only when the feature is on; otherwise a "not enabled" notice. */
export function FeatureGate({ feature, children }: { feature: FeatureName; children: ReactNode }) {
  const on = useFeature(feature);
  const { has } = useSession();
  if (on) return <>{children}</>;
  return (
    <Banner tone="info" title={t("Not enabled on this installation")}>
      <div className="col" style={{ gap: 8 }}>
        <div>
          {t("This module is switched off")}: {FEATURE_LABELS[feature]()}.{" "}
          {t("Selling, cash, refunds and reports work fully without it.")}
        </div>
        {has("settings.manage") ? (
          <div>
            <Link to="/admin/settings?section=features">{t("Open Settings → Features")}</Link>
          </div>
        ) : (
          <div className="small muted">{t("Ask the owner to enable it in Settings → Features.")}</div>
        )}
      </div>
    </Banner>
  );
}
