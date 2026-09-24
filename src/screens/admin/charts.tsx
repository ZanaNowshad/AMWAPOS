import { formatMoney } from "../../lib/money";

/** Minimal accessible bar chart (SVG, no dependencies). Values are integers. */
export function BarChart({ data, height = 200, format = formatMoney, label }: { data: { label: string; value: number }[]; height?: number; format?: (v: number) => string; label: string }) {
  const w = 760;
  const pad = { l: 8, r: 8, t: 10, b: 24 };
  const max = Math.max(1, ...data.map((d) => d.value));
  const bw = (w - pad.l - pad.r) / Math.max(1, data.length);
  const h = height - pad.t - pad.b;
  return (
    <svg className="chart" viewBox={`0 0 ${w} ${height}`} role="img" aria-label={label} preserveAspectRatio="none" style={{ height }}>
      {[0.25, 0.5, 0.75, 1].map((f) => (
        <line key={f} className="grid" x1={pad.l} x2={w - pad.r} y1={pad.t + h - h * f} y2={pad.t + h - h * f} strokeDasharray="3 4" />
      ))}
      {data.map((d, i) => {
        const bh = Math.max(d.value > 0 ? 2 : 0, (d.value / max) * h);
        const showLabel = data.length <= 14 || i % Math.ceil(data.length / 12) === 0;
        return (
          <g key={i}>
            <rect className="bar" x={pad.l + i * bw + bw * 0.15} width={bw * 0.7} y={pad.t + h - bh} height={bh} rx={3}>
              <title>{`${d.label}: ${format(d.value)}`}</title>
            </rect>
            {showLabel ? (
              <text className="axis" x={pad.l + i * bw + bw / 2} y={height - 8} textAnchor="middle">
                {d.label.length > 12 ? d.label.slice(0, 11) + "…" : d.label}
              </text>
            ) : null}
          </g>
        );
      })}
    </svg>
  );
}
