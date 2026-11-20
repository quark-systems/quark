// A harness's mark as a small square: the vendor logo when there is one, else a monogram tile.
import { harnessMark } from "./harnessMarks";
import "./HarnessLogo.css";

export function HarnessLogo({ harness, size = 16, title }: { harness: string; size?: number; title?: string }) {
  const m = harnessMark(harness);
  const label = title ?? m.name;
  if (m.src) {
    return (
      <img className={m.tile ? "harness-logo harness-tile" : "harness-logo"} src={m.src} width={size} height={size}
        alt={label} title={label} data-harness={harness} draggable={false}
        style={m.tile ? { background: m.tile } : undefined} />
    );
  }
  if (m.path) {
    return (
      <svg className="harness-logo" width={size} height={size} viewBox={m.viewBox ?? "0 0 24 24"} role="img" aria-label={label}
        data-harness={harness} style={m.color ? { color: m.color } : undefined}>
        <title>{label}</title>
        <path d={m.path} fill="currentColor" />
      </svg>
    );
  }
  return (
    <span className="harness-logo harness-mono" role="img" aria-label={label} title={label} data-harness={harness}
      style={{ width: size, height: size, fontSize: Math.round(size * (m.monogram && m.monogram.length > 1 ? 0.48 : 0.6)) }}>
      {m.monogram}
    </span>
  );
}
