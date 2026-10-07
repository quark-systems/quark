// The Overview tab's logic, kept apart from the screen so it can be tested: where the last visit
// is remembered, and the one-line summary of a digest.
import type { OverviewDigest, PulseState } from "../../api";

// The event log position of the last visit, per Project, remembered per viewer.
const seenKey = (project: string) => `quark.overview.seen.${project}`;

export function lastSeen(project: string): number | null {
  try {
    const v = localStorage.getItem(seenKey(project));
    const n = v == null ? NaN : Number(v);
    return Number.isSafeInteger(n) && n >= 0 ? n : null;
  } catch { return null; }
}

export function saveSeen(project: string, head: number) {
  try { localStorage.setItem(seenKey(project), String(head)); } catch { /* storage unavailable */ }
}

export const PULSE: Record<PulseState, { label: string; color: string }> = {
  working: { label: "Working", color: "var(--blue)" },
  needs_decision: { label: "Needs decision", color: "var(--violet)" },
  blocked: { label: "Blocked", color: "var(--red)" },
  paused: { label: "Paused", color: "var(--yellow)" },
  done: { label: "Done today", color: "var(--green)" },
  failed: { label: "Failed today", color: "var(--red)" },
};

const n = (k: number, one: string, many = one + "s") => `${k} ${k === 1 ? one : many}`;

/** One sentence: what changed, most important first. */
export function summarize(d: OverviewDigest): string {
  if (d.events === 0) return "Nothing new.";
  const parts: string[] = [];
  if (d.decisions_opened) parts.push(`${n(d.decisions_opened, "decision")} raised`);
  if (d.done) parts.push(`${n(d.done, "task")} done` + (d.pull_requests ? ` (${n(d.pull_requests, "pull request")})` : ""));
  if (d.failed) parts.push(`${d.failed} failed`);
  if (d.decisions_resolved) parts.push(`${n(d.decisions_resolved, "decision")} resolved`);
  if (d.spawned) parts.push(`${n(d.spawned, "worker")} started`);
  if (!parts.length) return `${n(d.events, "update")}, nothing that needs you.`;
  const s = parts.length === 1 ? parts[0] : parts.slice(0, -1).join(", ") + " and " + parts[parts.length - 1];
  return s[0].toUpperCase() + s.slice(1) + ".";
}
