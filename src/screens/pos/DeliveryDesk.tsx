import { useEffect, useState } from "react";
import { LogOut, MessageCircle, Phone, RefreshCw } from "lucide-react";
import { api } from "../../api";
import type { DeliveryRow } from "../../api/types";
import { useSession } from "../../state/session";
import { explain } from "../../lib/errors";
import { formatMoney } from "../../lib/money";
import { formatShort } from "../../lib/time";
import { Banner, Button, Chip, Empty } from "../../components/ui";
import { Logo } from "../../components/Logo";
import { t } from "../../i18n";
import { useFeature } from "../../components/FeatureGate";
import { OrdersList } from "../orders";
import { StatusChip, TicketSheet } from "./SendLoop";

/** The one next step for a drop, as the rider's big button. */
export function riderNext(status: string): { to: string; label: string } | null {
  switch (status) {
    case "pending":
      return { to: "preparing", label: t("Start packing") };
    case "preparing":
      return { to: "dispatched", label: t("Picked up, on the way") };
    case "dispatched":
      return { to: "delivered", label: t("Delivered") };
    default:
      return null;
  }
}

/** Minimal workspace for delivery staff: their deliveries, one big next step each. */
export function DeliveryDesk() {
  const { session, logout, has } = useSession();
  const ordersOn = useFeature("orders.digital") && has("orders.manage");
  const [tab, setTab] = useState<"deliveries" | "orders">("deliveries");
  const [rows, setRows] = useState<DeliveryRow[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = () =>
    api.deliveries
      .list()
      .then((r) => (setRows(r), setError(null)))
      .catch((e) => setError(explain(e).message));
  useEffect(() => {
    void load();
    // New assignments arrive without the rider having to reload.
    const id = setInterval(() => void load(), 30000);
    return () => clearInterval(id);
  }, []);
  const step = async (d: DeliveryRow) => {
    const n = riderNext(d.status);
    if (!n) return;
    // Handing over an unpaid order goes through the sheet, which asks about the money.
    if (n.to === "delivered" && d.payment_status !== "paid") return setOpen(d.delivery_id);
    try {
      await api.deliveries.update({ delivery_id: d.delivery_id, status: n.to });
      void load();
    } catch (e) {
      setError(explain(e).message);
    }
  };
  return (
    <div className="pos-root">
      <header className="pos-header">
        <div className="brand">
          <Logo size={28} /> {t("AMWAPOS · Deliveries")}
        </div>
        <div className="grow" />
        <div className="hitem">{session?.display_name}</div>
        <Button size="sm" icon={<RefreshCw size={15} />} onClick={() => void load()}>
          {t("Refresh")}
        </Button>
        <Button size="sm" icon={<LogOut size={15} />} onClick={() => void logout()}>
          {t("Logout")}
        </Button>
      </header>
      <div className="content">
        {ordersOn ? (
          <div className="row" style={{ marginBottom: 12 }}>
            <button
              className={`filter-chip ${tab === "deliveries" ? "active" : ""}`}
              onClick={() => setTab("deliveries")}
            >
              {t("Deliveries")}
            </button>
            <button className={`filter-chip ${tab === "orders" ? "active" : ""}`} onClick={() => setTab("orders")}>
              {t("Orders")}
            </button>
          </div>
        ) : null}
        {error ? <Banner tone="danger">{error}</Banner> : null}
        {tab === "orders" && ordersOn ? <OrdersList /> : null}
        {tab !== "deliveries" ? null : rows.length === 0 ? (
          <Empty title={t("No deliveries assigned")}>
            {t("New deliveries appear here when a manager assigns them to you.")}
          </Empty>
        ) : null}
        <div className="col" hidden={tab !== "deliveries"}>
          {rows.map((d) => {
            const n = riderNext(d.status);
            const digits = (d.phone ?? "").replace(/\D/g, "");
            return (
              <div key={d.delivery_id} className="card card-pad col gap-8" data-testid="rider-drop">
                <button type="button" className="rider-drop-head" onClick={() => setOpen(d.delivery_id)}>
                  <span className="grow">
                    <span className="strong" dir="auto">
                      {d.customer_name ?? t("Customer")}
                    </span>{" "}
                    <span className="num tiny muted">{d.delivery_number}</span>
                    <span className="small muted" dir="auto" style={{ display: "block" }}>
                      {[d.area, d.address].filter(Boolean).join(", ") || t("No address")} · {formatShort(d.created_at)}
                    </span>
                  </span>
                  <span className="money">{formatMoney(d.amount_minor)}</span>
                  <Chip tone={d.payment_status === "paid" ? "success" : "warning"}>
                    {d.payment_status === "cod"
                      ? t("Cash on delivery")
                      : d.payment_status === "paid"
                        ? t("Paid")
                        : t("Payment pending")}
                  </Chip>
                  <StatusChip status={d.status} />
                </button>
                <div className="row wrap gap-8">
                  {digits ? (
                    <>
                      <a className="btn" href={`tel:+${digits}`} aria-label={t("Call the customer")}>
                        <Phone size={18} aria-hidden /> {t("Call")}
                      </a>
                      <a
                        className="btn"
                        href={`https://wa.me/${digits}`}
                        target="_blank"
                        rel="noreferrer"
                        aria-label={t("Open WhatsApp chat")}
                      >
                        <MessageCircle size={18} aria-hidden /> {t("WhatsApp")}
                      </a>
                    </>
                  ) : null}
                  <span className="grow" />
                  {n ? (
                    <Button variant="primary" size="lg" onClick={() => void step(d)} data-testid="rider-next">
                      {n.label}
                    </Button>
                  ) : null}
                </div>
              </div>
            );
          })}
        </div>
      </div>
      {open ? <TicketSheet ticketId={open} onClose={() => setOpen(null)} onChanged={() => void load()} /> : null}
    </div>
  );
}
