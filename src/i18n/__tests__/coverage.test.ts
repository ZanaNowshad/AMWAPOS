import { describe, expect, it } from "vitest";
import { AR } from "../ar";

// Every UI source file, as text (Vite glob import; no Node APIs needed).
const files = import.meta.glob(["../../**/*.{ts,tsx}", "!../../**/__tests__/**", "!../../i18n/**"], {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

const keys = new Map<string, string>();
for (const [file, text] of Object.entries(files)) {
  for (const m of text.matchAll(/\bt\(\s*("(?:[^"\\]|\\.)*")/g)) keys.set(JSON.parse(m[1]) as string, file);
}
const holes = (s: string) => (s.match(/\{\d+\}/g) ?? []).sort().join(",");

describe("Arabic translations", () => {
  it("cover every UI string passed to t()", () => {
    expect(keys.size).toBeGreaterThan(1000);
    const missing = [...keys].filter(([k]) => !(k in AR)).map(([k, f]) => `${f}: ${k}`);
    expect(missing).toEqual([]);
  });

  it("keep every placeholder", () => {
    const broken = Object.entries(AR).filter(([en, ar]) => holes(en) !== holes(ar));
    expect(broken).toEqual([]);
  });

  it("are Arabic, not copies of the English", () => {
    const same = Object.entries(AR).filter(([en, ar]) => en === ar && /[a-z]{3}/.test(en));
    // Brand, printer model and file paths legitimately stay Latin.
    expect(same.map(([en]) => en).filter((en) => !/AMWAPOS|EPSON|\\/.test(en))).toEqual([]);
  });
});

describe("status and type labels", () => {
  it("all have Arabic translations", async () => {
    const { CODE_LABELS } = await import("../codes");
    expect(Object.values(CODE_LABELS).filter((en) => !(en in AR))).toEqual([]);
  });
});
