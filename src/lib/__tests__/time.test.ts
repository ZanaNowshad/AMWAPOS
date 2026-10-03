import { describe, expect, it } from "vitest";
import { formatDateTime, formatShort } from "../time";

describe("table dates", () => {
  it("never break inside a short or full date", () => {
    const iso = "2026-09-24T16:42:00Z";
    expect(formatShort(iso)).toBe("24 Sep 19:42");
    expect(formatDateTime(iso)).toBe("24 Sep 2026, 19:42");
    expect(formatShort(iso)).not.toMatch(/ /);
  });
});
