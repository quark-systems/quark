// Number formatting and series helpers shared by the Hosts view and the Project host slice.
import type { HostHealthStatus, HostRuntime, WorktreeSlotState } from "../../api";

/** `1.5 GB`, binary units, one decimal under 10. */
export function bytes(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return "–";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n, i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return `${i === 0 || v >= 10 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** A 0 to 1 share as a whole percent; `<1%` for a small but nonzero share. */
export function pct(x: number | null | undefined): string {
  if (x == null || !Number.isFinite(x)) return "–";
  if (x > 0 && x < 0.01) return "<1%";
  return `${Math.round(x * 100)}%`;
}

/** `part` of `whole` as 0 to 1, or null when there is no whole. */
export function share(part: number, whole: number | null | undefined): number | null {
  return whole ? Math.min(1, Math.max(0, part / whole)) : null;
}

export const HEALTH: Record<HostHealthStatus, { label: string; color: string }> = {
  healthy: { label: "Healthy", color: "var(--green)" },
  degraded: { label: "Degraded", color: "var(--yellow)" },
  unreachable: { label: "Unreachable", color: "var(--red)" },
};

export const RUNTIME: Record<HostRuntime, string> = {
  local: "This machine", ssh: "SSH", hosted: "Hosted", private_cloud: "Private cloud",
};

export const SLOT: Record<WorktreeSlotState, string> = {
  idle: "Idle", in_use: "In use", dirty: "Dirty", leased: "Leased", quarantined: "Quarantined",
};

/** SVG polyline points for `values` in a `w` x `h` box, scaled to `max` (or the largest value). */
export function sparkPoints(values: number[], w: number, h: number, max?: number): string {
  if (values.length === 0) return "";
  const top = max ?? Math.max(...values);
  const scale = top > 0 ? top : 1;
  const step = values.length > 1 ? w / (values.length - 1) : 0;
  return values
    .map((v, i) => `${(i * step).toFixed(1)},${(h - (Math.min(v, scale) / scale) * h).toFixed(1)}`)
    .join(" ");
}

/** The last path segment, for long worktree paths. */
export function tail(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts.length ? parts[parts.length - 1] : path;
}
