// What the left list shows, as pure data: each project with its coordinator row and the
// workers under it, so the rules are testable without rendering.
import type { Decision, Project, Task } from "../api";
import { isActive, stateMeta } from "../util";
import { taskTone, Tone } from "../ui/tone";

export interface WorkerRowData { id: string; title: string; sub: string; tone: Tone; needsYou: boolean }
export interface ProjectGroup {
  project: Project;
  swatch: string;
  /** Open decisions in this project: the coordinator row's needs-you count. */
  waiting: number;
  /** The coordinator row's second line. */
  coordSub: string;
  workers: WorkerRowData[];
}

// Project colors, one per project, stable across reloads (hash of the id).
const SWATCHES = ["var(--accent)", "var(--yellow)", "var(--green)", "var(--cyan)", "var(--violet)", "var(--red)"];
export function swatchFor(id: string): string {
  let h = 0;
  for (const c of id) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return SWATCHES[h % SWATCHES.length];
}

const plural = (n: number, one: string, many = one + "s") => `${n} ${n === 1 ? one : many}`;

/** How long a failed worker stays in the list for you to look at. */
export const FAILED_SHOWN_MS = 3 * 86400_000;

/** Whether a worker has a row: unfinished, or failed recently. Done work leaves the list. */
export function listed(t: Task, now: number): boolean {
  return isActive(t.state) || (t.state === "failed" && now - Date.parse(t.updated_at) < FAILED_SHOWN_MS);
}

/** Projects by name, each with its listed workers, newest activity first. */
export function leftListGroups(projects: Record<string, Project>, tasks: Record<string, Task>, decisions: Record<string, Decision>, now = Date.now()): ProjectGroup[] {
  const byProject: Record<string, Task[]> = {};
  for (const t of Object.values(tasks)) if (listed(t, now)) (byProject[t.project_id] ??= []).push(t);
  const waitingBy: Record<string, number> = {};
  for (const d of Object.values(decisions)) if (d.state === "open") waitingBy[d.project_id] = (waitingBy[d.project_id] ?? 0) + 1;

  return Object.values(projects).sort((a, b) => a.name.localeCompare(b.name)).map((p) => {
    const ws = (byProject[p.id] ?? []).sort((a, b) => b.updated_at.localeCompare(a.updated_at));
    const waiting = waitingBy[p.id] ?? 0;
    const parts = [waiting ? plural(waiting, "decision") : null, ws.length ? plural(ws.length, "worker") : null].filter(Boolean);
    return {
      project: p,
      swatch: swatchFor(p.id),
      waiting,
      coordSub: p.status && p.status !== "ready" ? (p.status === "failed" ? "Setup failed" : "Setting up") : parts.length ? parts.join(" · ") : "Nothing waiting",
      workers: ws.map((t) => {
        const tone = taskTone(t.state);
        return { id: t.id, title: t.title, sub: t.state_note || stateMeta(t.state).label, tone, needsYou: tone === "needs-you" };
      }),
    };
  });
}
