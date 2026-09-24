// Extract user-facing English strings from the Rust backend (errors, report
// labels, diagnostics, dashboard notes). format! placeholders become {0}, {1}…
import fs from "node:fs";
import path from "node:path";
const dirs = ["crates/amwapos-core/src", "crates/amwapos-hub/src"];
const files = dirs.flatMap((d) => fs.readdirSync(d).filter((f) => f.endsWith(".rs")).map((f) => path.join(d, f)));
const SQL = /\b(SELECT|INSERT|UPDATE|DELETE FROM|CREATE|FROM|WHERE|JOIN|PRAGMA|VALUES|ORDER BY|GROUP BY|COALESCE|strftime)\b/;
const out = new Map();
for (const f of files) {
  if (f.endsWith("receipt.rs") || f.endsWith("raster.rs") || f.endsWith("channel.rs")) continue; // printed / crypto, not UI
  let src = fs.readFileSync(f, "utf8");
  const testAt = src.indexOf("#[cfg(test)]");
  if (testAt >= 0) src = src.slice(0, testAt);
  const lines = src.split("\n");
  // Tokenize string literals (normal "..." with escapes and \-newline continuations; raw strings skipped).
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (c === "/" && src[i + 1] === "/") { i = src.indexOf("\n", i); if (i < 0) break; continue; }
    if (c === "r" && (src[i + 1] === '"' || src[i + 1] === "#") && !/[A-Za-z0-9_]/.test(src[i - 1] ?? "")) {
      const m = src.slice(i).match(/^r(#*)"/);
      if (m) { const end = src.indexOf('"' + m[1], i + m[0].length); i = end + 1 + m[1].length; continue; }
    }
    if (c === "'" ) { const m = src.slice(i).match(/^'(\\.|[^\\'])'/); if (m) { i += m[0].length; continue; } }
    if (c === '"') {
      let j = i + 1, s = "";
      while (j < src.length && src[j] !== '"') {
        if (src[j] === "\\") {
          const n = src[j + 1];
          if (n === "\n") { j += 2; while (/\s/.test(src[j])) j++; continue; }
          s += n === "n" ? "\n" : n === "t" ? "\t" : n; j += 2; continue;
        }
        s += src[j++];
      }
      const lineNo = src.slice(0, i).split("\n").length;
      const line = lines[lineNo - 1] ?? "";
      const before = src.slice(Math.max(0, i - 40), i);
      i = j + 1;
      if (/tracing::|debug!|info!|warn!|error!|expect\(|panic!|unreachable!|assert/.test(line + before)) continue;
      if (!/[A-Za-z]{2,}/.test(s) || !/ /.test(s.trim()) && !/^[A-Z][a-z]+$/.test(s)) continue;
      if (SQL.test(s) || /\?\d|\b(ASC|DESC|AND|OR|NULL|IS NOT)\b|=/.test(s) || /^[a-z_.:]+$/.test(s) || /\{:\?\}/.test(s) && !/ /.test(s)) continue;
      if (!/^[A-Z0-9(—"'{]/.test(s.trim())) continue;
      let k = 0;
      const key = s.replace(/\{[A-Za-z_][A-Za-z0-9_.]*(:[^}]*)?\}|\{\}|\{:[^}]*\}/g, () => `{${k++}}`).replace(/\{\{/g, "{").replace(/\}\}/g, "}");
      if (!out.has(key)) out.set(key, `${f}:${lineNo}`);
    }
    i++;
  }
}
const keys = [...out.keys()].sort();
fs.writeFileSync(process.argv[2] ?? "/dev/stdout", JSON.stringify(keys, null, 1));
console.error(`${keys.length} backend strings`);
