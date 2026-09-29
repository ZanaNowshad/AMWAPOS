import { describe, expect, it } from "vitest";
import type { WaCatalogRun } from "../../../api/types";
import { capabilityText, estimateMinutes, itemStatusText, runProgress } from "../waCatalog";

const run = (over: Partial<WaCatalogRun> = {}): WaCatalogRun => ({
  run_id: "r1",
  started_at: "2026-09-29T10:00:00Z",
  finished_at: null,
  total: 182,
  processed: 47,
  unchanged: 12,
  synced: 45,
  hidden: 0,
  removed: 0,
  failed: 2,
  verify: "done",
  verify_note: null,
  ...over,
});

describe("WhatsApp catalogue wording", () => {
  it("never presents an unsupported account as ready", () => {
    expect(capabilityText("supported")).toContain("with a catalogue");
    expect(capabilityText("personal")).toContain("need WhatsApp Business");
    expect(capabilityText("business_no_catalog")).toContain("could not be read");
    expect(capabilityText("disconnected")).toContain("not connected");
    expect(capabilityText("terminal")).toContain("hub computer");
  });

  it("names every product state", () => {
    expect(itemStatusText("synced")).toBe("On WhatsApp");
    expect(itemStatusText("failed")).toBe("Could not publish");
    expect(itemStatusText("remote_missing")).toBe("Deleted on WhatsApp");
    expect(itemStatusText("queued")).toBe(itemStatusText("syncing"));
    expect(itemStatusText(null)).toBe("Not on WhatsApp");
  });
});

describe("full sync progress", () => {
  it("reads as processed / total", () => {
    const p = runProgress(run());
    expect(p.label).toBe("47 / 182 products processed");
    expect(p.pct).toBe(25);
    expect(p.done).toBe(false);
  });

  it("is only done once the remote check is done too", () => {
    expect(runProgress(run({ processed: 182, verify: "pending" })).done).toBe(false);
    expect(runProgress(run({ processed: 182, verify: "skipped" })).done).toBe(true);
    expect(runProgress(run({ processed: 182 })).pct).toBe(100);
  });

  it("never shows more than 100% or divides by zero", () => {
    expect(runProgress(run({ total: 0, processed: 0 })).pct).toBe(100);
    const over = runProgress(run({ total: 3, processed: 5 }));
    expect(over.pct).toBe(100);
    expect(over.label).toBe("3 / 3 products processed");
  });

  it("estimates the first sync time", () => {
    expect(estimateMinutes(0)).toBe(1);
    expect(estimateMinutes(40)).toBe(1);
    expect(estimateMinutes(182)).toBe(5);
  });
});
