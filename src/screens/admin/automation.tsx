import { Bot, FileScan, MessageCircle, BadgeCheck } from "lucide-react";
import type { ComponentType } from "react";
import { Banner, PageHeader } from "../../components/ui";

function NotConfigured({ icon: Icon, title, summary, needs }: { icon: ComponentType<{ size?: number; color?: string }>; title: string; summary: string; needs: string[] }) {
  return (
    <div>
      <PageHeader title={title} />
      <div className="card card-pad col gap-16" style={{ maxWidth: 760 }}>
        <div className="row">
          <Icon size={28} color="var(--brand)" />
          <div>
            <h3>Not enabled on this installation</h3>
            <div className="muted">{summary}</div>
          </div>
        </div>
        <Banner tone="info" title="Checkout never depends on this feature">
          Selling, cash, refunds and reports work fully without it.
        </Banner>
        <div>
          <div className="field-label" style={{ marginBottom: 6 }}>
            Required before it can be enabled
          </div>
          <ul className="small" style={{ margin: 0, paddingLeft: 18 }}>
            {needs.map((n) => (
              <li key={n}>{n}</li>
            ))}
          </ul>
        </div>
      </div>
    </div>
  );
}

export const WhatsAppPage = () => (
  <NotConfigured
    icon={MessageCircle}
    title="WhatsApp"
    summary="Send receipts and delivery updates through a WhatsApp account linked by QR code."
    needs={["The AMWAPOS WhatsApp sidecar (bundled in a later release)", "A WhatsApp account to link from Linked Devices", "Owner approval of message templates"]}
  />
);

export const PaymentReviewsPage = () => (
  <NotConfigured
    icon={BadgeCheck}
    title="Payment Reviews"
    summary="Review BenefitPay screenshots received on WhatsApp against expected amounts. Screenshot analysis never counts as bank settlement."
    needs={["WhatsApp sidecar", "Local OCR models (English)"]}
  />
);

export const AiAssistantPage = () => (
  <NotConfigured
    icon={Bot}
    title="AI Assistant"
    summary="Ask questions about sales, stock and margins, and preview proposed changes before approving them."
    needs={["An AI provider API key stored in Windows Credential Manager", "Owner consent to send minimised business data to the provider"]}
  />
);

export const InvoiceScanPage = () => (
  <NotConfigured
    icon={FileScan}
    title="Invoice Scan"
    summary="Extract supplier invoice lines with local OCR and turn them into a purchase draft after human review. Nothing is posted automatically."
    needs={["Local OCR engine and bundled English models"]}
  />
);
