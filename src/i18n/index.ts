// UI language. English source strings are the keys; `ar.ts` maps them to Arabic.
// A missing Arabic entry falls back to English (and fails the coverage test).
import { AR } from "./ar";

export type Lang = "en" | "ar";
const KEY = "amwapos.lang";

function initial(): Lang {
  try {
    return localStorage.getItem(KEY) === "ar" ? "ar" : "en";
  } catch {
    return "en";
  }
}

let lang: Lang = initial();
const listeners = new Set<() => void>();

export const getLang = () => lang;
export const isRtl = () => lang === "ar";

/** Apply `lang`/`dir` to the document (called at boot and on change). */
export function applyDocumentLang() {
  document.documentElement.lang = lang === "ar" ? "ar" : "en";
  document.documentElement.dir = lang === "ar" ? "rtl" : "ltr";
}

export function setLang(next: Lang) {
  if (next === lang) return;
  lang = next;
  try {
    localStorage.setItem(KEY, next);
  } catch {
    /* storage unavailable: language lasts for this session */
  }
  applyDocumentLang();
  listeners.forEach((fn) => fn());
}

/** Switch language and reload so every screen (and module-level label) re-renders. */
export function switchLang(next: Lang) {
  setLang(next);
  location.reload();
}

export function subscribeLang(fn: () => void) {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** Translate an English UI string. `{0}`, `{1}` … are replaced by `args`. */
export function t(s: string, ...args: (string | number | null | undefined)[]): string {
  const base = lang === "ar" ? (AR[s] ?? s) : s;
  return args.length ? base.replace(/\{(\d+)\}/g, (_, i: string) => String(args[Number(i)] ?? "")) : base;
}

type Pattern = { re: RegExp; order: number[]; ar: string };
let patterns: Pattern[] | null = null;
const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

function compilePatterns(): Pattern[] {
  return (
    Object.entries(AR)
      .filter(([en]) => /\{\d+\}/.test(en))
      .map(([en, ar]) => ({
        re: new RegExp(
          "^" +
            en
              .split(/(\{\d+\})/)
              .map((part) => (/^\{\d+\}$/.test(part) ? "([\\s\\S]+?)" : escapeRe(part)))
              .join("") +
            "$",
        ),
        order: [...en.matchAll(/\{(\d+)\}/g)].map((m) => Number(m[1])),
        ar,
      }))
      // Most specific (longest literal text) first.
      .sort((a, b) => b.re.source.length - a.re.source.length)
  );
}

/**
 * Translate text produced by the backend (error messages, report labels,
 * diagnostics). Exact matches first, then messages with values in them
 * ("Barcode 123 was not found."), whose values are themselves translated
 * when they are known phrases. Unknown text is returned unchanged.
 */
const MONTHS_EN = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_AR = [
  "يناير",
  "فبراير",
  "مارس",
  "أبريل",
  "مايو",
  "يونيو",
  "يوليو",
  "أغسطس",
  "سبتمبر",
  "أكتوبر",
  "نوفمبر",
  "ديسمبر",
];
/** "24 Sep 2026 19:42" (backend display format) → "24 سبتمبر 2026 19:42". */
const localDates = (s: string) =>
  s.replace(
    /\b(\d{1,2}) (Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) (\d{4})\b/g,
    (_, d: string, m: string, y: string) => `${d} ${MONTHS_AR[MONTHS_EN.indexOf(m)]} ${y}`,
  );

export function tb(s: string | null | undefined, depth = 0): string {
  if (s === null || s === undefined) return "";
  if (lang !== "ar" || !s) return s;
  const exact = AR[s];
  if (exact) return exact;
  if (depth === 0) return localDates(tbInner(s, depth));
  return tbInner(s, depth);
}

function tbInner(s: string, depth: number): string {
  patterns ??= compilePatterns();
  for (const p of patterns) {
    const m = p.re.exec(s);
    if (!m) continue;
    let out = p.ar;
    p.order.forEach((n, j) => {
      const v = m[j + 1];
      out = out.split(`{${n}}`).join(depth < 2 ? tb(v, depth + 1) : v);
    });
    return out;
  }
  return s;
}

/**
 * Keep a left-to-right fragment (money, quantities, codes) intact inside
 * right-to-left text: U+2066 LEFT-TO-RIGHT ISOLATE … U+2069 POP ISOLATE.
 * No-op in English so plain strings stay plain.
 */
export function ltr(s: string): string {
  return lang === "ar" ? `⁦${s}⁩` : s;
}
