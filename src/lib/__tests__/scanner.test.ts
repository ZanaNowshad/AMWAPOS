import { describe, expect, it } from "vitest";
import { BurstDetector, DuplicateGuard, looksLikeBarcode } from "../scanner";

describe("scanner heuristics", () => {
  it("detects scanner-speed bursts but not human typing", () => {
    const fast = new BurstDetector(35);
    for (let i = 0; i < 13; i++) fast.key(1000 + i * 8);
    expect(fast.isBurst(13)).toBe(true);
    const slow = new BurstDetector(35);
    for (let i = 0; i < 6; i++) slow.key(1000 + i * 150);
    expect(slow.isBurst(6)).toBe(false);
    expect(fast.isBurst(3)).toBe(false);
  });

  it("classifies barcode-like values", () => {
    expect(looksLikeBarcode("6291041500213", false)).toBe(true);
    expect(looksLikeBarcode("ABC-12", true)).toBe(true);
    expect(looksLikeBarcode("ABC-12", false)).toBe(false);
    expect(looksLikeBarcode("milk 1l", true)).toBe(false);
    expect(looksLikeBarcode("12345", false)).toBe(false);
  });

  it("suppresses duplicate scans inside the window only", () => {
    const g = new DuplicateGuard(500);
    expect(g.accept("A", 0)).toBe(true);
    expect(g.accept("A", 200)).toBe(false);
    expect(g.accept("B", 250)).toBe(true);
    expect(g.accept("B", 900)).toBe(true);
    g.setWindow(0);
    expect(g.accept("B", 901)).toBe(true);
  });
});
