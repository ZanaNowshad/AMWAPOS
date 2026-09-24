export function Logo({ size = 32, light = false }: { size?: number; light?: boolean }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64" role="img" aria-label="AMWAPOS">
      <rect x="2" y="2" width="60" height="60" rx="14" fill={light ? "#2563EB" : "#2563EB"} />
      <path d="M16 46 L28 18 H36 L48 46 H40.5 L37.8 39 H26.2 L23.5 46 Z M28.4 33 H35.6 L32 23.6 Z" fill="#fff" />
      <rect x="16" y="49" width="32" height="3" rx="1.5" fill="#93C5FD" />
    </svg>
  );
}
