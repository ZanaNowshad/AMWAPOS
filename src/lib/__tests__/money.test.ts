import { describe, expect, it } from "vitest";
import {
  formatAmount,
  formatMoney,
  formatPercent,
  formatQty,
  parseDecimal,
  parseMoney,
  parsePercent,
  parseQty,
  mulDivRound,
} from "../money";

describe("money formatting", () => {
  it("formats BHD with three decimals and a real minus sign", () => {
    expect(formatMoney(18450)).toBe("BHD 18.450");
    expect(formatMoney(-4500)).toBe("−BHD 4.500");
    expect(formatMoney(5)).toBe("BHD 0.005");
    expect(formatMoney(0)).toBe("BHD 0.000");
    expect(formatMoney(null)).toBe("—");
    expect(formatAmount(1234567)).toBe("1234.567");
  });

  it("formats quantities and percentages", () => {
    expect(formatQty(2000)).toBe("2");
    expect(formatQty(750)).toBe("0.750");
    expect(formatQty(-1500)).toBe("-1.500");
    expect(formatPercent(1000)).toBe("10%");
    expect(formatPercent(1250)).toBe("12.5%");
    expect(formatPercent(0)).toBe("0%");
  });
});

describe("exact decimal parsing", () => {
  it("parses without floating point error", () => {
    expect(parseMoney("0.1")).toBe(100);
    expect(parseMoney("18.45")).toBe(18450);
    expect(parseMoney("1.005")).toBe(1005);
    expect(parseMoney("20")).toBe(20000);
    expect(parseMoney(".5")).toBe(500);
    expect(parseMoney(" 3.250 ")).toBe(3250);
    expect(parseMoney("-2.5")).toBe(-2500);
    expect(parseQty("0.750")).toBe(750);
    expect(parsePercent("12.5%")).toBe(1250);
  });

  it("rejects malformed or over-precise input", () => {
    for (const bad of ["", "-", ".", "1.2345", "abc", "1e3", "1,000", "1.2.3", "Infinity", "0x10"]) {
      expect(parseMoney(bad)).toBeNull();
    }
    expect(parseDecimal("99999999999999999999", 3)).toBeNull();
  });

  it("round-trips every fils value in a range", () => {
    for (let v = 0; v < 5000; v += 7) expect(parseMoney(formatAmount(v))).toBe(v);
  });
});

describe("mulDivRound matches the backend (half away from zero)", () => {
  it("rounds halves away from zero in both directions", () => {
    expect(mulDivRound(250, 1000, 10000)).toBe(25); // exact
    expect(mulDivRound(5, 5000, 10000)).toBe(3); // 2.5 -> 3
    expect(mulDivRound(5, -5000, 10000)).toBe(-3); // -2.5 -> -3 (Math.round gives -2)
    expect(mulDivRound(1234, 1500, 1000)).toBe(1851); // 1851.0
    expect(mulDivRound(333, 1, 2)).toBe(167); // 166.5 -> 167
    expect(mulDivRound(-333, 1, 2)).toBe(-167);
  });
  it("is exact beyond 2^53 intermediates", () => {
    expect(mulDivRound(9_000_000_000_000, 10_000, 10_000)).toBe(9_000_000_000_000);
  });
});
