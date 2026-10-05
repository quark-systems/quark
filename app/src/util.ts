import type { TaskState } from "./api";

export const STATES: { id: TaskState; label: string; color: string }[] = [
  { id: "queued", label: "Queued", color: "var(--fg-faint)" },
  { id: "running", label: "Running", color: "var(--blue)" },
  { id: "needs_decision", label: "Needs decision", color: "var(--accent)" },
  { id: "blocked", label: "Blocked", color: "var(--red)" },
  { id: "paused", label: "Paused", color: "var(--yellow)" },
  { id: "in_review", label: "In review", color: "var(--yellow)" },
  { id: "done", label: "Done", color: "var(--green)" },
  { id: "failed", label: "Failed", color: "var(--red)" },
  { id: "unknown", label: "Unknown", color: "var(--fg-faint)" },
];
export const stateMeta = (s: TaskState) => STATES.find((x) => x.id === s) ?? STATES[STATES.length - 1];
export const isActive = (s: TaskState) => s !== "done" && s !== "failed";

export function ago(ts: string | undefined | null): string {
  const t = ts ? Date.parse(ts) : NaN;
  if (!isFinite(t)) return "";
  const s = Math.max(0, Math.round((Date.now() - t) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

// The name a person answers decisions and reviews memory as, remembered per viewer.
const USER_KEY = "quark.user";
export function savedUser(): string {
  try { return localStorage.getItem(USER_KEY) ?? ""; } catch { return ""; }
}
export function saveUser(name: string) {
  try { localStorage.setItem(USER_KEY, name); } catch { /* storage unavailable */ }
}

export function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
