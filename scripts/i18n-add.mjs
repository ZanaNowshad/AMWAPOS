// Merge {"English": "Arabic"} pairs (JSON file arg) into src/i18n/ar.ts, sorted.
import fs from "node:fs";
const file = "src/i18n/ar.ts";
const src = fs.readFileSync(file, "utf8");
const body = src.slice(src.indexOf("= {") + 3, src.lastIndexOf("}"));
const current = Function(`return {${body}}`)();
const add = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
const merged = { ...current, ...add };
const lines = Object.keys(merged)
  .sort()
  .map((k) => `  ${JSON.stringify(k)}: ${JSON.stringify(merged[k])},`)
  .join("\n");
const header = src.slice(0, src.indexOf("export const AR"));
fs.writeFileSync(file, `${header}export const AR: Record<string, string> = {\n${lines}\n};\n`);
console.log(`ar.ts: ${Object.keys(merged).length} entries (+${Object.keys(merged).length - Object.keys(current).length})`);
