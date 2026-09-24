// Display helpers. Stored timestamps are UTC; display uses the store timezone.
let TZ = "Asia/Bahrain";
export function configureTimezone(tz: string) {
  TZ = tz;
}
export const timezone = () => TZ;

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function parts(iso: string) {
  const d = new Date(iso);
  const f = new Intl.DateTimeFormat("en-GB", {
    timeZone: TZ,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).formatToParts(d);
  const get = (t: string) => f.find((p) => p.type === t)?.value ?? "";
  return { y: get("year"), m: Number(get("month")), d: get("day"), hh: get("hour"), mm: get("minute") };
}

/** "24 Sep 2026, 19:42" */
export function formatDateTime(iso: string | null | undefined): string {
  if (!iso) return "—";
  const p = parts(iso);
  return `${p.d} ${MONTHS[p.m - 1]} ${p.y}, ${p.hh}:${p.mm}`;
}

/** "24 Sep 19:42" */
export function formatShort(iso: string | null | undefined): string {
  if (!iso) return "—";
  const p = parts(iso);
  return `${p.d} ${MONTHS[p.m - 1]} ${p.hh}:${p.mm}`;
}

/** "24 Sep 2026" */
export function formatDate(iso: string | null | undefined): string {
  if (!iso) return "—";
  if (/^\d{4}-\d{2}-\d{2}$/.test(iso)) {
    const [y, m, d] = iso.split("-");
    return `${d} ${MONTHS[Number(m) - 1]} ${y}`;
  }
  const p = parts(iso);
  return `${p.d} ${MONTHS[p.m - 1]} ${p.y}`;
}

export function formatClock(date: Date): string {
  const p = parts(date.toISOString());
  return `${p.hh}:${p.mm}`;
}

/** Local business date (YYYY-MM-DD) in the store timezone. */
export function todayLocal(offsetDays = 0): string {
  const d = new Date(Date.now() + offsetDays * 86400000);
  const f = new Intl.DateTimeFormat("en-CA", { timeZone: TZ, year: "numeric", month: "2-digit", day: "2-digit" });
  return f.format(d);
}

export function relative(iso: string | null | undefined): string {
  if (!iso) return "never";
  const s = Math.round((Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86400)} d ago`;
}
