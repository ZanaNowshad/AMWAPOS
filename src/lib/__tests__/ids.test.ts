import { describe, expect, it } from "vitest";
import { newOperationId } from "../ids";

describe("operation ids", () => {
  it("are 26-char Crockford base32, unique and time-sortable", () => {
    const ids = Array.from({ length: 500 }, () => newOperationId());
    for (const id of ids) expect(id).toMatch(/^[0-9A-HJKMNP-TV-Z]{26}$/);
    expect(new Set(ids).size).toBe(ids.length);
    const a = newOperationId();
    const later = new Date(Date.now() + 5000);
    const orig = Date.now;
    Date.now = () => later.getTime();
    try {
      expect(newOperationId().slice(0, 10) > a.slice(0, 10)).toBe(true);
    } finally {
      Date.now = orig;
    }
  });
});
