// Exact money and quantity helpers. Values are integers (minor units / thousandths);
// strings are parsed digit by digit — never through floating point.

let CURRENCY = "BHD";
let DIGITS = 3;

export function configureMoney(currency: string, digits: number) {
  CURRENCY = currency;
  DIGITS = digits;
}
export const currency = () => CURRENCY;
export const digits = () => DIGITS;

function fixed(v: number, d: number): string {
  const neg = v < 0;
  const a = Math.abs(Math.trunc(v));
  if (d === 0) return (neg ? "-" : "") + a.toString();
  const s = a.toString().padStart(d + 1, "0");
  return (neg ? "-" : "") + s.slice(0, s.length - d) + "." + s.slice(s.length - d);
}

/** "18.450" */
export function formatAmount(minor: number): string {
  return fixed(minor, DIGITS);
}

/** "BHD 18.450" / "−BHD 4.500" (U+2212) */
export function formatMoney(minor: number | null | undefined): string {
  if (minor === null || minor === undefined) return "—";
  const body = fixed(Math.abs(minor), DIGITS);
  return minor < 0 ? `−${CURRENCY} ${body}` : `${CURRENCY} ${body}`;
}

/** Parse "1.25" -> 1250 (3 digits). Returns null if invalid or too precise. */
export function parseDecimal(input: string, d: number): number | null {
  const s = input.trim();
  if (!/^-?\d*(\.\d*)?$/.test(s) || s === "" || s === "-" || s === "." || s === "-.") return null;
  const neg = s.startsWith("-");
  const body = neg ? s.slice(1) : s;
  const [ip, fp = ""] = body.split(".");
  if (fp.length > d) return null;
  const v = Number(ip || "0") * 10 ** d + Number((fp + "0".repeat(d)).slice(0, d) || "0");
  if (!Number.isSafeInteger(v)) return null;
  return neg ? -v : v;
}

export const parseMoney = (s: string) => parseDecimal(s, DIGITS);
export const parseQty = (s: string) => parseDecimal(s, 3);

/** 2000 -> "2", 750 -> "0.750" */
export function formatQty(milli: number): string {
  if (milli % 1000 === 0) return (milli / 1000).toString();
  return fixed(milli, 3);
}

/** basis points -> "10%" / "12.5%" */
export function formatPercent(bp: number | null | undefined): string {
  if (bp === null || bp === undefined) return "—";
  const s = fixed(bp, 2).replace(/\.?0+$/, "");
  return `${s}%`;
}

export function parsePercent(s: string): number | null {
  return parseDecimal(s.replace("%", ""), 2);
}
