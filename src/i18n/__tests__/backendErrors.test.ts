import { describe, expect, it } from "vitest";
import { AR } from "../ar";

// Every error message the Rust backend can show a person (AppError::validation /
// conflict / not_found / forbidden / new), as tb() looks it up: format!
// placeholders become {0}, {1}… Test modules are skipped.
const sources = import.meta.glob(["/crates/amwapos-core/src/**/*.rs", "/crates/amwapos-hub/src/**/*.rs"], {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

const CALL =
  /AppError::(?:validation|conflict|not_found|forbidden|new)\s*\(\s*(?:ErrorCode::\w+\s*,\s*)?(?:format!\(\s*)?"((?:[^"\\]|\\.)*)"/g;

function messages(): Map<string, string> {
  const out = new Map<string, string>();
  for (const [file, text] of Object.entries(sources)) {
    const cut = text.indexOf("#[cfg(test)]");
    const src = cut >= 0 ? text.slice(0, cut) : text;
    for (const m of src.matchAll(CALL)) {
      let n = 0;
      const key = m[1]
        .replace(/\\\n\s*/g, "")
        .replace(/\\"/g, '"')
        .replace(/\{[A-Za-z_][A-Za-z0-9_.]*(:[^}]*)?\}|\{\}|\{:[^}]*\}/g, () => `{${n++}}`);
      // Permission codes (forbidden("pos.sell")) are shown as "not allowed", not as text.
      if (!/^[a-z_.]+$/.test(key)) out.set(key, file);
    }
  }
  return out;
}

describe("backend error messages", () => {
  it("are found (the scan works)", () => {
    expect(messages().size).toBeGreaterThan(500);
  });

  it("all have Arabic", () => {
    const missing = [...messages()].filter(([k]) => !(k in AR)).map(([k, f]) => `${f}: ${k}`);
    expect(missing).toEqual([]);
  });
});
