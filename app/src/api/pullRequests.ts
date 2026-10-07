import { daemonUrl, enc, req } from "./client";

// PR center (Phase 2 workstream 3, quark#16).
export type PullRequestState = "open" | "draft" | "merged" | "closed";
export type ChecksState = "passing" | "failing" | "pending" | "none";
export type ReviewDecision = "approved" | "changes_requested" | "review_required" | "none";
export type Mergeability = "mergeable" | "conflicting" | "unknown";
export type CheckStatus = "pending" | "success" | "failure" | "neutral" | "cancelled";
export interface Check {
  name: string; status: CheckStatus;
  /** The forge's own conclusion, e.g. `timed_out`, when it says more than `status`. */
  conclusion?: string | null; details_url?: string | null; started_at?: string | null; completed_at?: string | null;
}
export interface Review {
  id: string; author?: string | null; state: "approved" | "changes_requested" | "commented" | "dismissed" | "pending";
  body: string; submitted_at?: string | null; commit?: string | null;
}
// Verification evidence (ADR-15): repo-native checks, Playwright journeys and holdout tests.
export type GateKind = "checks" | "journeys" | "holdout";
export type GateState = "pending" | "running" | "passed" | "failed" | "skipped";
export type ArtifactKind = "trace" | "screenshot" | "video" | "log" | "report";
export interface EvidenceArtifact {
  id: string; kind: ArtifactKind; name: string; content_type: string; size_bytes?: number | null;
  /** Served by the daemon at `GET /v1/pull-requests/{id}/evidence/artifacts/{artifact_id}`. */
  url: string;
}
export interface EvidenceCase {
  name: string; state: GateState; duration_ms?: number | null; message?: string | null; artifacts: EvidenceArtifact[];
}
export interface EvidenceGate {
  kind: GateKind; state: GateState; summary?: string | null; started_at?: string | null; completed_at?: string | null;
  cases: EvidenceCase[];
}
export interface Evidence {
  head_sha?: string | null; state: GateState;
  /** True when `head_sha` is not the PR's current head. */
  stale: boolean;
  started_at?: string | null; completed_at?: string | null; gates: EvidenceGate[];
}
export interface PullRequest {
  id: string; project_id: string; task_id?: string | null; url: string; provider: string; repo: string; number: number;
  title?: string | null; author?: string | null; state: PullRequestState;
  head_ref?: string | null; base_ref?: string | null; head_sha?: string | null;
  mergeable: Mergeability; checks_state: ChecksState; review_decision: ReviewDecision;
  additions?: number | null; deletions?: number | null; changed_files?: number | null;
  checks: Check[]; reviews: Review[]; evidence?: Evidence | null;
  opened_at?: string | null; updated_at?: string | null; merged_at?: string | null; closed_at?: string | null;
  synced_at?: string | null; sync_error?: string | null;
}
export interface CheckUpdated { pull_request_id: string; task_id?: string | null; head_sha?: string | null; check: Check }
export interface ReviewUpdated { pull_request_id: string; task_id?: string | null; review: Review }
export interface PullRequestDiff { pull_request_id: string; path?: string | null; patch: string; truncated: boolean }
export type DiffSide = "new" | "old";
export interface PullRequestComment { text: string; path?: string | null; line?: number | null; side?: DiffSide | null }
export type MergeMethod = "squash" | "merge" | "rebase";

export const pullRequestsApi = {
  pullRequests: () => req<PullRequest[]>("GET", "/v1/pull-requests"),
  pullRequest: (id: string) => req<PullRequest>("GET", `/v1/pull-requests/${enc(id)}`),
  pullRequestDiff: (id: string) => req<PullRequestDiff>("GET", `/v1/pull-requests/${enc(id)}/diff`),
  /** Delivered to the PR's owning worker as a steering message; not posted on the forge. */
  commentPullRequest: (id: string, c: PullRequestComment) => req<void>("POST", `/v1/pull-requests/${enc(id)}/comments`, c),
  /** The engine's guarded merge; 409 `merge_refused` unless open, green and conflict-free. */
  mergePullRequest: (id: string, method?: MergeMethod) =>
    req<PullRequest>("POST", `/v1/pull-requests/${enc(id)}:merge`, method ? { method } : {}),
  /** Absolute URL of an evidence artifact; the daemon sends it relative to itself. */
  artifactUrl: (prId: string, a: EvidenceArtifact) =>
    /^https?:\/\//.test(a.url) ? a.url
      : a.url.startsWith("/") ? daemonUrl() + a.url
      : `${daemonUrl()}/v1/pull-requests/${enc(prId)}/evidence/artifacts/${enc(a.id)}`,
};
