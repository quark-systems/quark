// The attention queue: everything that waits on you, oldest first, built from what the
// app already holds (open decisions, red PRs, stuck workers). Next attention (⌘J) walks it.
import type { Decision, PullRequest, Task } from "../api";
import type { Route } from "../nav";

export type AttentionKind = "decision" | "pr" | "worker";
export interface AttentionItem { key: string; kind: AttentionKind; title: string; projectId: string; since: string; route: Route }

const time = (s: string | null | undefined) => s ?? "";

/** Open decisions, open PRs with a failing check, and failed or blocked workers, oldest first.
 * Items of projects in `hidden` (archived ones) are left out. */
export function attentionQueue(decisions: Record<string, Decision>, prs: Record<string, PullRequest>, tasks: Record<string, Task>,
  hidden: ReadonlySet<string> = new Set()): AttentionItem[] {
  const items: AttentionItem[] = [];
  const asked = new Set<string>();
  for (const d of Object.values(decisions)) {
    if (d.state !== "open") continue;
    if (d.task_id) asked.add(d.task_id);
    items.push({ key: "decision:" + d.id, kind: "decision", title: d.question, projectId: d.project_id, since: d.opened_at, route: { name: "decisions", project: d.project_id, id: d.id } });
  }
  for (const p of Object.values(prs)) {
    if ((p.state !== "open" && p.state !== "draft") || p.checks_state !== "failing") continue;
    items.push({ key: "pr:" + p.id, kind: "pr", title: p.title ?? `${p.repo}#${p.number}`, projectId: p.project_id, since: time(p.updated_at ?? p.opened_at), route: { name: "pulls", project: p.project_id, id: p.id } });
  }
  for (const t of Object.values(tasks)) {
    // A worker waiting on a decision is reached through that decision.
    if ((t.state !== "failed" && t.state !== "blocked") || asked.has(t.id)) continue;
    items.push({ key: "worker:" + t.id, kind: "worker", title: t.title, projectId: t.project_id, since: t.updated_at, route: { name: "task", id: t.id } });
  }
  return items.filter((i) => !hidden.has(i.projectId)).sort((a, b) => a.since.localeCompare(b.since) || a.key.localeCompare(b.key));
}

/** The item the current route shows, if any. */
export function currentIndex(queue: AttentionItem[], route: Route): number {
  // An item opens in its project; the all-projects inbox and PR center show the same thing.
  const same = (i: AttentionItem) => i.route.name === route.name || ALSO_AT[i.kind] === route.name;
  return queue.findIndex((i) => same(i) && "id" in route && (i.route as { id?: string }).id === route.id);
}

const ALSO_AT: Record<AttentionKind, Route["name"]> = { decision: "inbox", pr: "pr", worker: "task" };

/** Where Next attention goes from `route`: the item after the one shown, else the oldest. */
export function nextAttention(queue: AttentionItem[], route: Route): AttentionItem | null {
  if (!queue.length) return null;
  const i = currentIndex(queue, route);
  return queue[(i + 1) % queue.length];
}
