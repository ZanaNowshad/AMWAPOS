import { describe, expect, it } from "vitest";
import { ApiError } from "../../api/transport";
import type { OrderFlow, WaOrderDetail } from "../../api/types";
import { ackFor, warningOf } from "../orderConfirm";
import { waNextStep } from "../admin/waOrders";
import { riderNext } from "../pos/DeliveryDesk";
import { visibleSteps } from "../../components/OrderFlow";

type Line = WaOrderDetail["order"] extends infer O ? (O extends { lines: (infer L)[] } ? L : never) : never;

function line(p: Partial<Line> = {}): Line {
  return {
    line_no: 1,
    product_id: "P1",
    name: "Milk",
    requested: "milk",
    qty_milli: 1000,
    unit_price_minor: 500,
    line_total_minor: 500,
    resolution: "resolved",
    availability: "available",
    locked: false,
    note: null,
    candidates: null,
    ...p,
  };
}

function detail(s: Partial<WaOrderDetail["session"]> = {}, lines: Line[] = [line()]): WaOrderDetail {
  return {
    session: {
      session_id: "S",
      chat: "c",
      phone: "+97333112233",
      customer_id: null,
      customer_name: null,
      customer_state: "known",
      customer_candidates: null,
      order_id: "O",
      state: "ready",
      intent: "new_order",
      priority: "normal",
      priority_reasons: [],
      delivery_mode: "pickup",
      address: null,
      address_raw: null,
      address_source: null,
      zone_id: null,
      delivery_fee_minor: null,
      fee_state: "unresolved",
      ai_status: "rules",
      staff_takeover: false,
      handled: false,
      assigned_to: null,
      questions: [],
      revision: 1,
      created_at: "",
      updated_at: "",
      ...s,
    },
    messages: [],
    order: {
      order_id: "O",
      order_number: "O-1",
      status: "draft",
      payment_state: "unpaid",
      lines,
      subtotal_minor: 500,
      delivery_fee_minor: null,
      total_minor: 500,
      complete: true,
    },
    events: [],
    suggested_reply: "",
    summary: "",
  };
}

describe("WhatsApp order next step", () => {
  it("walks a first-time user through items, delivery, customer, confirm", () => {
    expect(waNextStep(detail({}, []))?.title).toBe("Waiting for the items");
    const q = detail({
      questions: [
        {
          id: "q",
          kind: "choose",
          line_no: 1,
          text: "Which?",
          options: [{ product_id: "A", name: "A", name_ar: null, price_minor: 1, availability: "available" }],
        },
      ],
    } as Partial<WaOrderDetail["session"]>);
    expect(waNextStep(q)?.target).toBe("wa-questions");
    expect(waNextStep(detail({}, [line({ product_id: null, resolution: "unmatched" })]))?.target).toBe(
      "wa-order-lines",
    );
    expect(waNextStep(detail({}, [line({ resolution: "unavailable" })]))?.title).toMatch(/out of stock/);
    expect(waNextStep(detail({ delivery_mode: "unknown" }))?.target).toBe("wa-delivery");
    expect(waNextStep(detail({ delivery_mode: "delivery", fee_state: "unresolved" }))?.title).toBe(
      "Complete the address",
    );
    expect(waNextStep(detail({ customer_state: "ambiguous" }))?.target).toBe("wa-customer");
    expect(waNextStep(detail())?.target).toBe("confirm");
  });

  it("says what happens after confirming, and stays quiet on closed chats", () => {
    expect(waNextStep(detail({ state: "confirmed" }))?.done).toBe(true);
    expect(waNextStep(detail({ state: "cancelled" }))).toBeNull();
    expect(waNextStep(detail({ staff_takeover: true }))?.target).toBe("wa-reply");
  });
});

describe("confirm warnings", () => {
  const err = (details: Record<string, unknown>, code = "conflict") =>
    new ApiError({ code: code as never, message: "x", data_changed: false, retryable: false, details });

  it("turn a shortage or a changed total into a question, not a dead end", () => {
    const w = warningOf(
      err({ kind: "stock_shortage", lines: [{ name: "Milk", wanted_milli: 3000, free_milli: 1000 }] }),
    );
    expect(w?.kind).toBe("stock_shortage");
    expect(ackFor(w!)).toEqual({ acknowledge_shortage: true });
    const p = warningOf(err({ kind: "price_changed_since_quote", quoted_minor: 1000, current_minor: 1200 }));
    expect(p).toEqual({ kind: "price_changed_since_quote", quoted_minor: 1000, current_minor: 1200 });
    expect(ackFor(p!)).toEqual({ acknowledge_price_change: true });
  });

  it("leave every other error alone", () => {
    expect(warningOf(err({ kind: "stock_shortage", lines: [] }, "validation"))).toBeNull();
    expect(warningOf(err({ kind: "something_else" }))).toBeNull();
    expect(warningOf(new Error("x"))).toBeNull();
  });
});

describe("order journey bar", () => {
  it("shows only the steps this person can act on, in journey order", () => {
    const f: OrderFlow = { chats: 2, waiting: 1, to_confirm: 1, to_pack: null, out: null, payments: 3 };
    expect(visibleSteps(f).map((s) => s.key)).toEqual(["chats", "to_confirm", "payments"]);
    expect(visibleSteps(null)).toEqual([]);
  });
});

describe("rider next step", () => {
  it("is one plain action per status", () => {
    expect(riderNext("pending")?.to).toBe("preparing");
    expect(riderNext("preparing")?.label).toBe("Picked up, on the way");
    expect(riderNext("dispatched")?.to).toBe("delivered");
    expect(riderNext("delivered")).toBeNull();
  });
});
