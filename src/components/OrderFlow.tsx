// The order journey in one line, at the top of every order page:
// Messages → To confirm → To pack & send → On the way → Payments to check.
// Each step shows how many are waiting and takes you there. Steps a person
// cannot act on are not shown, so a first-time user always sees where they
// are, what comes next, and where the work is.
import { useEffect, useState } from "react";
import { NavLink, useLocation } from "react-router-dom";
import { BadgeCheck, ChevronRight, ClipboardCheck, MessageCircle, PackageOpen, Truck } from "lucide-react";
import type { ComponentType } from "react";
import { api } from "../api";
import type { OrderFlow as Flow } from "../api/types";
import { t } from "../i18n";

export interface FlowStep {
  key: keyof Flow;
  path: string;
  label: string;
  hint: string;
  icon: ComponentType<{ size?: number; "aria-hidden"?: boolean }>;
}

export function flowSteps(): FlowStep[] {
  return [
    {
      key: "chats",
      path: "whatsapp-orders",
      label: t("Messages"),
      hint: t("WhatsApp chats read into draft orders."),
      icon: MessageCircle,
    },
    {
      key: "to_confirm",
      path: "orders",
      label: t("To confirm"),
      hint: t("Check the items and confirm."),
      icon: ClipboardCheck,
    },
    {
      key: "to_pack",
      path: "deliveries",
      label: t("To pack & send"),
      hint: t("Ring up, pack and hand to a rider."),
      icon: PackageOpen,
    },
    { key: "out", path: "deliveries", label: t("On the way"), hint: t("With the rider."), icon: Truck },
    {
      key: "payments",
      path: "payment-reviews",
      label: t("Payments to check"),
      hint: t("Transfer screenshots waiting for a person."),
      icon: BadgeCheck,
    },
  ];
}

/** Steps this person can see, in order (null counts are hidden). */
export function visibleSteps(flow: Flow | null): FlowStep[] {
  if (!flow) return [];
  return flowSteps().filter((s) => flow[s.key] !== null && flow[s.key] !== undefined);
}

export function OrderFlowBar() {
  const [flow, setFlow] = useState<Flow | null>(null);
  const loc = useLocation();
  useEffect(() => {
    let live = true;
    const load = () =>
      api.orders
        .flow()
        .then((f) => live && setFlow(f))
        .catch(() => undefined);
    void load();
    const iv = window.setInterval(load, 15000);
    return () => {
      live = false;
      window.clearInterval(iv);
    };
  }, [loc.pathname]);
  const steps = visibleSteps(flow);
  if (steps.length < 2) return null;
  const here = steps.find((s) => loc.pathname === `/admin/${s.path}`);
  return (
    <nav className="order-flow" aria-label={t("Order journey")} data-testid="order-flow">
      <ol>
        {steps.map((s, i) => {
          const n = flow?.[s.key] ?? 0;
          const active = here?.path === s.path;
          return (
            <li key={s.key}>
              {i ? <ChevronRight size={16} className="flow-sep" aria-hidden /> : null}
              <NavLink
                to={`/admin/${s.path}`}
                className={`flow-step ${active ? "active" : ""} ${n ? "busy" : ""}`}
                aria-current={active ? "page" : undefined}
                // On narrow screens only the active step shows its words, so the
                // name is spelled out for screen readers and as a tooltip.
                aria-label={`${s.label}: ${t("{0} waiting", n)}`}
                title={`${s.label} — ${s.hint}`}
                data-testid={`flow-${s.key}`}
              >
                <s.icon size={18} aria-hidden />
                <span className="flow-label">{s.label}</span>
                <span className="flow-n num" aria-hidden>
                  {n}
                </span>
              </NavLink>
            </li>
          );
        })}
      </ol>
      {here ? <p className="flow-hint">{here.hint}</p> : null}
    </nav>
  );
}
