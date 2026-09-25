import { useEffect, useState } from "react";
import { LogOut, RefreshCw } from "lucide-react";
import { api } from "../../api";
import type { DeliveryRow } from "../../api/types";
import { useSession } from "../../state/session";
import { explain } from "../../lib/errors";
import { formatMoney } from "../../lib/money";
import { formatShort } from "../../lib/time";
import { Banner, Button, Chip, Empty } from "../../components/ui";
import { Logo } from "../../components/Logo";
import { t } from "../../i18n";
import { codeLabel } from "../../i18n/codes";
import { useFeature } from "../../components/FeatureGate";
import { OrdersList } from "../orders";

/** Minimal workspace for delivery staff: assigned deliveries and status only. */
export function DeliveryDesk() {
  const { session, logout, has } = useSession();
  const ordersOn = useFeature("orders.digital") && has("orders.manage");
  const [tab, setTab] = useState<"deliveries" | "orders">("deliveries");
  const [rows, setRows] = useState<DeliveryRow[]>([]);
  const [error, setError] = useState<string | null>(null);
  const load = () =>
    api.deliveries
      .list()
      .then(setRows)
      .catch((e) => setError(explain(e).message));
  useEffect(() => {
    void load();
    // New assignments arrive without the rider having to reload.
    const id = setInterval(() => void load(), 30000);
    return () => clearInterval(id);
  }, []);
  const next: Record<string, string> = { pending: "preparing", preparing: "dispatched", dispatched: "delivered" };
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
              {t("Digital orders")}
            </button>
          </div>
        ) : null}
        {tab === "orders" && ordersOn ? <OrdersList /> : null}
        {tab === "deliveries" && error ? <Banner tone="danger">{error}</Banner> : null}
        {tab !== "deliveries" ? null : rows.length === 0 ? (
          <Empty title={t("No deliveries assigned")}>
            {t("New deliveries appear here when a manager assigns them to you.")}
          </Empty>
        ) : null}
        <div className="col" hidden={tab !== "deliveries"}>
          {rows.map((d) => (
            <div key={d.delivery_id} className="card card-pad row">
              <div className="grow">
                <div style={{ fontWeight: 650 }}>
                  {d.delivery_number} · {d.customer_name ?? t("Customer")}
                </div>
                <div className="small muted">
                  {[d.area, d.address].filter(Boolean).join(", ")} · {d.phone ?? t("no phone")} ·{" "}
                  {formatShort(d.created_at)}
                </div>
              </div>
              <Chip tone={d.payment_status === "paid" ? "success" : "warning"}>
                {d.payment_status === "cod"
                  ? t("Cash on delivery")
                  : d.payment_status === "paid"
                    ? t("Paid")
                    : t("Payment pending")}
              </Chip>
              <span className="money">{formatMoney(d.amount_minor)}</span>
              <Chip tone="info">{codeLabel(d.status)}</Chip>
              {next[d.status] ? (
                <Button
                  variant="primary"
                  onClick={async () => {
                    try {
                      await api.deliveries.update({ delivery_id: d.delivery_id, status: next[d.status] });
                      void load();
                    } catch (e) {
                      setError(explain(e).message);
                    }
                  }}
                >
                  {t("Mark {0}", codeLabel(next[d.status]))}
                </Button>
              ) : null}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
