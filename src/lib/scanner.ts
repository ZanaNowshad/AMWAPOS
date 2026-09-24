// Distinguish barcode-scanner bursts from human typing. HID scanners emit
// characters every few milliseconds and finish with Enter.

export class BurstDetector {
  private times: number[] = [];
  constructor(private maxAvgGapMs = 35) {}
  key(now = performance.now()) {
    this.times.push(now);
    if (this.times.length > 64) this.times.shift();
  }
  reset() {
    this.times = [];
  }
  /** True when the last `n` keystrokes arrived at scanner speed. */
  isBurst(n: number): boolean {
    if (n < 4 || this.times.length < n) return false;
    const t = this.times.slice(-n);
    const avg = (t[t.length - 1] - t[0]) / (n - 1);
    return avg <= this.maxAvgGapMs;
  }
}

/** A value that should be looked up as a barcode rather than searched by name. */
export function looksLikeBarcode(v: string, burst: boolean): boolean {
  const s = v.trim();
  if (!s || /\s/.test(s)) return false;
  if (burst && s.length >= 4) return true;
  return /^\d{6,}$/.test(s);
}

/** Suppress an accidental double scan of the same code within `windowMs`. */
export class DuplicateGuard {
  private last = "";
  private at = 0;
  constructor(private windowMs: number) {}
  setWindow(ms: number) {
    this.windowMs = ms;
  }
  accept(code: string, now = Date.now()): boolean {
    if (this.windowMs > 0 && code === this.last && now - this.at < this.windowMs) return false;
    this.last = code;
    this.at = now;
    return true;
  }
}
