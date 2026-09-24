// Subtle audio feedback for the cashier (configurable). No sound on ordinary clicks.
let ctx: AudioContext | null = null;
let enabled = true;
export function setSoundEnabled(v: boolean) {
  enabled = v;
}
function tone(freq: number, ms: number, gain = 0.06, type: OscillatorType = "sine") {
  if (!enabled) return;
  try {
    ctx ??= new AudioContext();
    const o = ctx.createOscillator();
    const g = ctx.createGain();
    o.type = type;
    o.frequency.value = freq;
    g.gain.value = gain;
    o.connect(g).connect(ctx.destination);
    const t = ctx.currentTime;
    g.gain.setValueAtTime(gain, t);
    g.gain.exponentialRampToValueAtTime(0.0001, t + ms / 1000);
    o.start(t);
    o.stop(t + ms / 1000);
  } catch {
    /* audio unavailable */
  }
}
export const sounds = {
  scan: () => tone(1760, 70),
  unknown: () => tone(240, 220, 0.08, "square"),
  success: () => {
    tone(880, 90);
    setTimeout(() => tone(1320, 120), 90);
  },
  error: () => tone(180, 260, 0.08, "sawtooth"),
};
