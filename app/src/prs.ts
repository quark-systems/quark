// PR center helpers: labels, colors and ordering for pull requests, checks and reviews.
import type { Check, ChecksState, PullRequest, PullRequestState, Review, ReviewDecision } from "./api";

export const PR_STATES: { id: PullRequestState; label: string }[] = [
  { id: "open", label: "Open" },
  { id: "draft", label: "Draft" },
  { id: "merged", label: "Merged" },
  { id: "closed", label: "Closed" },
];

export const prStateMeta: Record<PullRequestState, { label: string; cls: string }> = {
  open: { label: "Open", cls: "green" },
  draft: { label: "Draft", cls: "" },
  merged: { label: "Merged", cls: "accent" },
  closed: { label: "Closed", cls: "red" },
};

export const checksMeta: Record<ChecksState, { label: string; cls: string; glyph: string }> = {
  passing: { label: "Checks passing", cls: "green", glyph: "✓" },
  failing: { label: "Checks failing", cls: "red", glyph: "✗" },
  pending: { label: "Checks running", cls: "yellow", glyph: "●" },
  none: { label: "No checks", cls: "", glyph: "–" },
};

export const reviewMeta: Record<ReviewDecision, { label: string; cls: string }> = {
  approved: { label: "Approved", cls: "green" },
  changes_requested: { label: "Changes requested", cls: "red" },
  review_required: { label: "Review required", cls: "yellow" },
  none: { label: "No review", cls: "" },
};

/** One check's outcome as shown in the checks list; the forge's own conclusion wins when it says more. */
export function checkOutcome(c: Check): { label: string; cls: string; glyph: string } {
  const said = c.conclusion ? c.conclusion.replace(/_/g, " ") : null;
  switch (c.status) {
    case "pending": return { label: said ?? (c.started_at ? "running" : "queued"), cls: "yellow", glyph: "●" };
    case "success": return { label: said ?? "passed", cls: "green", glyph: "✓" };
    case "neutral": return { label: said ?? "neutral", cls: "", glyph: "–" };
    case "cancelled": return { label: said ?? "cancelled", cls: "red", glyph: "✗" };
    default: return { label: said ?? "failed", cls: "red", glyph: "✗" };
  }
}

export const reviewLabel: Record<Review["state"], { label: string; cls: string }> = {
  approved: { label: "approved", cls: "green" },
  changes_requested: { label: "requested changes", cls: "red" },
  commented: { label: "commented", cls: "" },
  dismissed: { label: "dismissed", cls: "" },
  pending: { label: "is reviewing", cls: "" },
};

/** Failing checks first, then running, then the rest; by name within each group. */
export function sortChecks(checks: Check[]): Check[] {
  const rank = (c: Check) => {
    const o = checkOutcome(c).cls;
    return o === "red" ? 0 : o === "yellow" ? 1 : 2;
  };
  return [...checks].sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name));
}

/** PRs in one state, optionally one Project, most recently updated first. */
export function filterPrs(prs: PullRequest[], state: PullRequestState, projectId: string | null): PullRequest[] {
  return prs
    .filter((p) => p.state === state && (!projectId || p.project_id === projectId))
    .sort((a, b) => (b.updated_at ?? "").localeCompare(a.updated_at ?? "") || b.number - a.number);
}

export function countByState(prs: PullRequest[], projectId: string | null): Record<PullRequestState, number> {
  const out: Record<PullRequestState, number> = { open: 0, draft: 0, merged: 0, closed: 0 };
  for (const p of prs) if (!projectId || p.project_id === projectId) out[p.state]++;
  return out;
}

/**
 * Why Merge is not a plain click, or null when the PR is ready. `hard` blockers are ones the
 * daemon's guarded merge refuses (not open, conflicting, not green); a soft one needs a second click.
 */
export function mergeBlocker(pr: PullRequest): { reason: string; hard: boolean } | null {
  if (pr.state === "draft") return { reason: "This PR is a draft.", hard: true };
  if (pr.state !== "open") return { reason: `This PR is ${pr.state}.`, hard: true };
  if (pr.mergeable === "conflicting") return { reason: "This PR has merge conflicts.", hard: true };
  if (pr.checks_state === "failing") return { reason: "Checks are failing.", hard: true };
  if (pr.checks_state === "pending") return { reason: "Checks are still running.", hard: true };
  if (pr.review_decision === "changes_requested") return { reason: "A reviewer requested changes.", hard: false };
  return null;
}

/** The PR's title, or its number until the forge has been read. */
export const prTitle = (pr: PullRequest) => pr.title ?? `${pr.repo}#${pr.number}`;

// ---- verification evidence (ADR-15) ----

export const GATE_LABEL: Record<string, string> = { checks: "Repository checks", journeys: "Playwright journeys", holdout: "Holdout tests" };

/** Colors an evidence state (run, gate or case); unknown states read as in progress. */
export function evidenceOutcome(state: string): { cls: string; glyph: string; label: string } {
  const s = state.toLowerCase();
  if (["passed", "pass", "success", "succeeded", "ok"].includes(s)) return { cls: "green", glyph: "✓", label: "passed" };
  if (["failed", "fail", "failure", "error", "errored", "timed_out", "cancelled"].includes(s)) return { cls: "red", glyph: "✗", label: s.replace(/_/g, " ") };
  if (["skipped", "neutral", "not_run"].includes(s)) return { cls: "", glyph: "–", label: s.replace(/_/g, " ") };
  return { cls: "yellow", glyph: "●", label: s.replace(/_/g, " ") };
}

export function caseCounts(cases: { state: string }[]): { passed: number; failed: number; other: number } {
  const out = { passed: 0, failed: 0, other: 0 };
  for (const c of cases) {
    const o = evidenceOutcome(c.state).cls;
    if (o === "green") out.passed++; else if (o === "red") out.failed++; else out.other++;
  }
  return out;
}

/** Failing cases first, then running, then the rest, keeping the engine's order within each. */
export function sortCases<T extends { state: string }>(cases: T[]): T[] {
  const rank = (c: T) => ({ red: 0, yellow: 1 } as Record<string, number>)[evidenceOutcome(c.state).cls] ?? 2;
  return cases.map((c, i) => [c, i] as const).sort((a, b) => rank(a[0]) - rank(b[0]) || a[1] - b[1]).map(([c]) => c);
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10240 ? 1 : 0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function formatMs(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)} s`;
  return `${Math.floor(ms / 60_000)}m ${Math.round((ms % 60_000) / 1000)}s`;
}

/** Playwright's hosted trace viewer loads a trace.zip by URL, in the browser. */
export const traceViewerUrl = (traceUrl: string) => `https://trace.playwright.dev/?trace=${encodeURIComponent(traceUrl)}`;
