// Filtering and labels for the Issues tab and the New issue side chat, kept pure so they are easy to test.
import type { BeadsStatus, DraftIssue, Issue, IssueDraft, IssueRef, MemoryScope } from "./api";

/** The Issues tab's filter chips, in order. */
export type IssueChip = "ready" | "in_progress" | "blocked" | "closed";
export const CHIPS: { id: IssueChip; label: string }[] = [
  { id: "ready", label: "Ready" },
  { id: "in_progress", label: "In progress" },
  { id: "blocked", label: "Blocked" },
  { id: "closed", label: "Closed" },
];

/** A decision waits on a person, not a worker, so it is never ready work. */
export const isDecision = (i: { issue_type: string }) => i.issue_type === "decision";

/** Whether `i` belongs under a chip. Closed issues are only under Closed. */
export function inChip(i: Issue, chip: IssueChip): boolean {
  if (chip === "closed") return i.status === "closed";
  if (i.status === "closed") return false;
  if (chip === "in_progress") return i.status === "in_progress";
  if (chip === "blocked") return i.blocked;
  return i.ready && i.status !== "in_progress" && !isDecision(i);
}

/** Highest priority first, then the lower issue number, so the list reads like `bd ready`. */
export function chipList(all: Issue[], chip: IssueChip): Issue[] {
  const list = all.filter((i) => inChip(i, chip));
  if (chip === "closed") return list.sort((a, b) => (b.closed_at ?? b.updated_at).localeCompare(a.closed_at ?? a.updated_at) || byId(a, b));
  return list.sort((a, b) => a.priority - b.priority || byId(a, b));
}
const byId = (a: { id: string }, b: { id: string }) => a.id.localeCompare(b.id, undefined, { numeric: true });

export function chipCounts(all: Issue[]): Record<IssueChip, number> {
  const n = { ready: 0, in_progress: 0, blocked: 0, closed: 0 };
  for (const i of all) for (const c of CHIPS) if (inChip(i, c.id)) n[c.id]++;
  return n;
}

/** `#61` for a GitHub issue URL, or null for anything else. */
export function githubNumber(url: string | null | undefined): string | null {
  const m = url ? /^https?:\/\/github\.com\/[^/]+\/[^/]+\/issues\/(\d+)/.exec(url) : null;
  return m ? `#${m[1]}` : null;
}

export function statusLabel(s: string): string {
  return s === "in_progress" ? "in progress" : s.replace(/_/g, " ");
}

/** The row's second line: type, where it stands, who works on it, the GitHub issue it mirrors. */
export function issueMeta(i: Issue, known: Record<string, Issue>): string {
  const waiting = i.blocked_by.filter((b) => known[b]?.status !== "closed");
  const where = i.status === "closed" ? "closed" : i.blocked ? `blocked by ${waiting.join(", ")}`
    : i.ready && i.status === "open" ? (isDecision(i) ? "waiting on you" : "ready") : statusLabel(i.status);
  const gh = githubNumber(i.external_ref);
  return [i.issue_type, where, i.assignee, gh && `GitHub ${gh}`].filter(Boolean).join(" · ");
}

/** What the issue waits for that is still open, in its listed order. */
export const openBlockers = (refs: IssueRef[]) => refs.filter((r) => r.status !== "closed");

/** How a linked issue's row reads on the right: an open decision waits on you, the rest show their status. */
export function refState(r: IssueRef): { label: string; color: string; decision: boolean } {
  if (isDecision(r) && r.status !== "closed") return { label: "waiting on you", color: "var(--violet)", decision: true };
  const color = r.status === "closed" ? "var(--green)" : r.status === "in_progress" ? "var(--blue)"
    : r.status === "blocked" ? "var(--red)" : "var(--fg-faint)";
  return { label: r.status === "closed" ? "done" : statusLabel(r.status), color, decision: false };
}

/** The coordinator message "Start a worker" sends. */
export const startWorkerMessage = (i: { id: string; title: string }) => `Start a worker on ${i.id}: ${i.title}`;

/** The open draft the drawer reopens: the most recently updated one for the Project. */
export function openDraft(drafts: IssueDraft[], projectId: string): IssueDraft | undefined {
  return drafts.filter((d) => d.project_id === projectId && d.state === "open")
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at))[0];
}

/** A draft's own blockers: another draft's key reads `new · 1`, an existing issue its id. */
export function draftBlockers(d: DraftIssue, drafts: DraftIssue[]): string[] {
  return d.blocked_by.map((b) => (drafts.some((x) => x.key === b) ? `new · ${b}` : b));
}

/** Labels as typed, comma or space separated. */
export function parseLabels(s: string): string[] {
  return [...new Set(s.split(/[\s,]+/).map((x) => x.trim()).filter(Boolean))];
}

/** "Beads in owner/repo · mirrored both ways with GitHub Issues", without the sync time. */
export function beadsWhere(b: BeadsStatus): string {
  const where = b.github_repo ?? b.dir ?? "this Project";
  return `Beads in ${where}` + (b.github_repo ? " · mirrored both ways with GitHub Issues" : "");
}

/** The Memory screen's "Who should know" choices, with where each one keeps the learning. */
export function scopeChoices(beads: BeadsStatus | undefined): { scope: MemoryScope; label: string; hint: string }[] {
  const ready = beads?.state === "ready";
  return [
    { scope: "project", label: "This project", hint: ready ? `Beads in ${beads?.github_repo ?? beads?.dir ?? "the Project repo"}` : "memory/ in the Project repo" },
    { scope: "user", label: "All my projects", hint: "your own memory" },
    { scope: "repo", label: "Anyone who works in this repo", hint: "opens a PR to AGENTS.md" },
  ];
}
