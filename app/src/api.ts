// Typed client for the quarkd v1 API. Shapes follow api/openapi.json and the Phase 1 PRs
// listed in app/CONTRACT.md.

export type TaskState =
  | "queued" | "running" | "needs_decision" | "blocked" | "paused"
  | "in_review" | "done" | "failed" | "unknown";
export type TaskKind = "ship" | "scout";
export type Effort = "low" | "medium" | "high" | "xhigh" | "max";

export interface AgentConfig {
  harness: string; model?: string | null; effort?: string | null;
  /** Account pool to run under (quark#23); absent runs under the harness's default account. */
  pool?: string | null;
}
export interface RepoSource { url: string; name?: string | null }
export type DispatchPreset = "single" | "light_trivial";
export type DeliveryPolicy = "gated" | "direct";
export type ProjectStatus = "provisioning" | "ready" | "failed";

export interface Project {
  id: string; name: string; goal?: string | null; workspace_path?: string | null;
  created_at: string; updated_at: string;
  // From the J2 work (quark#10); absent on older daemons.
  status?: ProjectStatus; status_detail?: string | null; repos?: RepoSource[];
  agent_config?: AgentConfig | null; dispatch_preset?: DispatchPreset | null; delivery?: DeliveryPolicy | null;
  /** PR center: merge this Project's green PRs without asking (the engine's yolo posture). */
  standing_approval?: boolean | null;
}

export interface CreateProject {
  name: string; goal?: string | null; workspace_path?: string | null;
  repos?: RepoSource[]; agent_config?: AgentConfig; dispatch_preset?: DispatchPreset; delivery?: DeliveryPolicy;
}

export interface Task {
  id: string; project_id: string; title: string; state: TaskState;
  kind?: TaskKind | null; state_note?: string | null; harness?: string | null;
  pull_request_url?: string | null; created_at: string; updated_at: string;
  /** The account the worker was started under (an `Account.id`), when its harness has accounts. */
  account_id?: string | null;
}

export type DecisionState = "open" | "answered";
export interface Decision {
  id: string; project_id: string; task_id?: string | null; question: string;
  state: DecisionState; answer?: string | null; opened_at: string;
  /** Who answered and when; null while open. */
  answered_by?: string | null; answered_at?: string | null;
}
export interface AnswerDecision { answer: string; answered_by?: string | null }

// Project memory (journey J8, quark#29): learnings from finished tasks, reviewed as proposals.
export type MemorySource = "worker" | "coordinator";
export type MemoryProposalState = "proposed" | "accepted" | "rejected";
export interface MemoryEvidence {
  task_id?: string | null; task_title?: string | null; pull_request_url?: string | null; files: string[];
}
/** One file under the Project repo's `memory/`. Hand-written files carry only `id`, `path` and `text`. */
export interface MemoryEntry {
  id: string; project_id: string; path: string; text: string; evidence: MemoryEvidence;
  source?: MemorySource | null; date?: string | null; accepted_at?: string | null; accepted_by?: string | null;
  proposal_id?: string | null; commit?: string | null;
}
export interface MemoryProposal {
  id: string; project_id: string; text: string; evidence: MemoryEvidence; source: MemorySource;
  state: MemoryProposalState; proposed_at: string; decided_at?: string | null; decided_by?: string | null;
  /** Set once accepted. */
  entry?: MemoryEntry | null;
}
export interface AcceptMemoryProposal { text?: string | null; decided_by?: string | null }
export interface RejectMemoryProposal { decided_by?: string | null }

export interface Health { status: string; version: string; engine: string; last_seq: number }

export type AgentRole = "coordinator" | "worker";
export interface HarnessInfo {
  id: string; name: string; roles: AgentRole[];
  install: { installed: boolean; version?: string | null; path?: string | null; install_hint: string };
  models: { selection: "free_form" | "provider_qualified" | "automatic"; discovery?: string | null };
  efforts: Effort[];
  auth: HarnessAuth;
  transcript: boolean;
  /** Variable that selects an account's config directory; null when the harness has only its default account. */
  account_env?: string | null;
}
export type AuthState = "configured" | "not_configured" | "unknown";
export interface HarnessAuth { state: AuthState; detail?: string | null }

// Accounts and pools per harness, with per-account quota (quark#23, ADR-11).
export type QuotaState = "pending" | "known" | "unavailable" | "error" | "unsupported";
export interface QuotaWindow { id: string; label: string; percent_remaining?: number | null; resets_at?: string | null }
export interface AccountQuota {
  state: QuotaState;
  /** What limits the account now, 0 to 100. */
  remaining_percent?: number | null;
  plan?: string | null; windows: QuotaWindow[]; detail?: string | null; checked_at?: string | null;
}
export interface Account {
  /** `acc_...`, or `default-<harness>` for a harness's default account. */
  id: string; harness: string; label: string; config_dir?: string | null;
  /** The harness's usual config directory; listed but cannot be removed. */
  default: boolean;
  pools: string[];
  /** Credential health from the harness adapter. */
  health: HarnessAuth;
  quota: AccountQuota;
  active_tasks: number;
  /** False when the engine cannot start this harness under another account yet. */
  launchable: boolean;
  created_at?: string | null;
}
export interface CreateAccount { harness: string; label?: string | null; config_dir: string; pools?: string[] }
export interface UpdateAccount { label?: string | null; pools?: string[] | null }
/** Payload of `account.quota_changed`. */
export interface AccountQuotaChanged { account_id: string; harness: string; quota: AccountQuota }
export interface ValidationIssue { field: string; code: string; message: string }
export interface HarnessValidation { valid: boolean; errors: ValidationIssue[]; warnings: ValidationIssue[] }

export type TranscriptRole = "user" | "assistant" | "thinking" | "tool_call" | "tool_result";
export interface TranscriptEntry {
  role: TranscriptRole; text: string; tool_name?: string | null; tool_call_id?: string | null;
  is_error: boolean; truncated: boolean; ts?: string | null;
}
/** A transcript entry with its id: the `seq` of the event that carried it. */
export interface TranscriptItem extends TranscriptEntry { id: number }
export interface MessageAccepted { coordinator_id: string; confirmed: boolean; accepted_at: string }

export interface Terminal {
  id: string; project_id: string; role: AgentRole; task_id?: string | null; title: string; cols: number; rows: number;
}
export interface TerminalOutput {
  terminal_id: string; role: AgentRole; task_id?: string | null; kind: "output" | "snapshot";
  data_b64: string; cols?: number | null; rows?: number | null;
}

export type ChangeStatus = "added" | "modified" | "deleted" | "renamed" | "copied" | "type_changed" | "untracked";
export interface ChangedFile {
  path: string; old_path?: string | null; status: ChangeStatus; additions?: number | null; deletions?: number | null;
}
// Why this agent (ADR-11, quark#27): one record per worker spawn, kept after the task ends.
export type DispatchTrigger = "spawn" | "relaunch";
export type DispatchDecider = "classifier" | "coordinator" | "relaunch";
export type DispatchStatus = "clear" | "ambiguous" | "escalate" | "error" | "off" | "not_consulted";
export interface DispatchCandidate {
  harness: string; model?: string | null; passed: boolean; reason: string; evidence?: string | null;
}
export interface DispatchRecord {
  id: string; task_id: string; project_id: string; trigger: DispatchTrigger; decided_by: DispatchDecider;
  summary: string;
  rule?: { id: string; when?: string | null } | null;
  resolution: { status: DispatchStatus; reason?: string | null; notes: string[]; output?: string | null };
  candidates: DispatchCandidate[];
  /** `account` is an `Account.id` (quark#23), the task's account when the spawn was recorded; null when unknown. */
  chosen: { harness: string; model?: string | null; effort?: string | null; account?: string | null };
  /** `provider` is "none" when no classifier was consulted (the coordinator picked). */
  classifier: { provider: string; model?: string | null; confidence?: number | null };
  recorded_at: string;
}

export interface TaskChanges { task_id: string; base_ref: string; base: string; head: string; files: ChangedFile[] }
export interface TaskDiff { task_id: string; base: string; path?: string | null; patch: string; truncated: boolean }

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

export interface DaemonEvent<T = unknown> {
  seq: number; project_id?: string | null; type: string; ts: string; payload: T;
}

/** The daemon answered, but does not serve this endpoint yet (404/405/501). */
export class NotAvailable extends Error {
  constructor(public path: string) { super(`${path} is not available on this daemon yet`); }
}
/** Any other non-2xx answer. `message` is the daemon's `error.message` when it sent one. */
export class ApiError extends Error {
  constructor(public status: number, public code: string | null, message: string) { super(message); }
}

export const DEFAULT_DAEMON = "http://127.0.0.1:7380";
const STORAGE_KEY = "quark.daemon";

function initialDaemon(): string {
  const fromQuery = typeof location !== "undefined" ? new URLSearchParams(location.search).get("daemon") : null;
  let saved: string | null = null;
  try { saved = localStorage.getItem(STORAGE_KEY); } catch { /* storage unavailable */ }
  return (fromQuery ?? saved ?? DEFAULT_DAEMON).replace(/\/$/, "");
}

let daemon = initialDaemon();
export function daemonUrl() { return daemon; }
export function wsUrl(path: string) { return daemon.replace(/^http/, "ws") + path; }
/** Point the app at another daemon and remember it for this viewer. */
export function setDaemonUrl(url: string) {
  daemon = url.trim().replace(/\/$/, "") || DEFAULT_DAEMON;
  try { localStorage.setItem(STORAGE_KEY, daemon); } catch { /* storage unavailable */ }
}

const enc = (s: string) => encodeURIComponent(s);

async function req<T>(method: string, path: string, body?: unknown): Promise<T> {
  const r = await fetch(daemon + path, {
    method,
    headers: body !== undefined ? { "content-type": "application/json" } : undefined,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (r.status === 404 || r.status === 405 || r.status === 501) {
    // A 404 with a daemon error body naming a missing entity is a real "not found", not a missing route.
    const err = await r.json().catch(() => null);
    if (r.status === 404 && err?.error?.code && err.error.code !== "route_not_found") {
      throw new ApiError(404, err.error.code, err.error.message ?? "not found");
    }
    throw new NotAvailable(path);
  }
  if (!r.ok) {
    const text = await r.text().catch(() => "");
    let code: string | null = null, message = text || r.statusText;
    try {
      const j = JSON.parse(text);
      code = j?.error?.code ?? null;
      message = j?.error?.message ?? (typeof j?.error === "string" ? j.error : message);
    } catch { /* not JSON */ }
    throw new ApiError(r.status, code, `${message} (${r.status})`);
  }
  if (r.status === 202 || r.status === 204) return undefined as T;
  const ct = r.headers.get("content-type") ?? "";
  if (ct.includes("json")) return (await r.json()) as T;
  return (await r.text()) as unknown as T;
}

export const api = {
  health: () => req<Health>("GET", "/v1/health"),
  projects: () => req<Project[]>("GET", "/v1/projects"),
  project: (id: string) => req<Project>("GET", `/v1/projects/${enc(id)}`),
  createProject: (p: CreateProject) => req<Project>("POST", "/v1/projects", p),
  provision: (id: string) => req<void>("POST", `/v1/projects/${enc(id)}:provision`),
  tasks: (pid: string) => req<Task[]>("GET", `/v1/projects/${enc(pid)}/tasks`),
  task: (id: string) => req<Task>("GET", `/v1/tasks/${enc(id)}`),
  /** Open and answered decisions, so the inbox can show who answered. */
  decisions: () => req<Decision[]>("GET", "/v1/decisions"),
  /** Answers an open decision and returns it answered. `answered_by` defaults to the daemon's user. */
  answerDecision: (id: string, body: AnswerDecision) => req<Decision>("POST", `/v1/decisions/${enc(id)}:answer`, body),
  memoryProposals: (pid: string, state?: MemoryProposalState) =>
    req<MemoryProposal[]>("GET", `/v1/projects/${enc(pid)}/memory/proposals` + (state ? `?state=${state}` : "")),
  /** Commits the entry (edited when `text` is given) to the Project repo and returns the accepted proposal. */
  acceptMemoryProposal: (pid: string, id: string, body: AcceptMemoryProposal = {}) =>
    req<MemoryProposal>("POST", `/v1/projects/${enc(pid)}/memory/proposals/${enc(id)}:accept`, body),
  rejectMemoryProposal: (pid: string, id: string, body: RejectMemoryProposal = {}) =>
    req<MemoryProposal>("POST", `/v1/projects/${enc(pid)}/memory/proposals/${enc(id)}:reject`, body),
  memory: (pid: string) => req<MemoryEntry[]>("GET", `/v1/projects/${enc(pid)}/memory`),
  harnesses: () => req<HarnessInfo[]>("GET", "/v1/harnesses"),
  validateAgent: (config: AgentConfig, role: AgentRole) =>
    req<HarnessValidation>("POST", "/v1/harnesses:validate", { config, role }),

  /** Every account per harness; `refresh` reads each account's quota now. */
  accounts: (refresh = false) => req<Account[]>("GET", "/v1/accounts" + (refresh ? "?refresh=true" : "")),
  /** Its quota arrives shortly after as `account.quota_changed`. */
  addAccount: (a: CreateAccount) => req<Account>("POST", "/v1/accounts", a),
  updateAccount: (id: string, u: UpdateAccount) => req<Account>("PATCH", `/v1/accounts/${enc(id)}`, u),
  /** 409 for a default account or one a running task uses. */
  removeAccount: (id: string) => req<void>("DELETE", `/v1/accounts/${enc(id)}`),

  chat: (cid: string) => req<TranscriptItem[]>("GET", `/v1/coordinators/${enc(cid)}/messages?limit=1000`),
  sendChat: (cid: string, text: string) => req<MessageAccepted | undefined>("POST", `/v1/coordinators/${enc(cid)}/messages`, { text }),

  steer: (id: string, text: string) => req<void>("POST", `/v1/tasks/${enc(id)}/messages`, { text }),
  cancel: (id: string) => req<void>("POST", `/v1/tasks/${enc(id)}:cancel`),
  relaunch: (id: string) => req<void>("POST", `/v1/tasks/${enc(id)}:relaunch`),

  transcript: (id: string) => req<TranscriptItem[]>("GET", `/v1/tasks/${enc(id)}/transcript?limit=1000`),
  /** Every dispatch of the task, oldest first: its first spawn, then each relaunch. */
  dispatch: (id: string) => req<DispatchRecord[]>("GET", `/v1/tasks/${enc(id)}/dispatch`),
  changes: (id: string) => req<TaskChanges>("GET", `/v1/tasks/${enc(id)}/changes`),
  diff: (id: string, path?: string) =>
    req<TaskDiff>("GET", `/v1/tasks/${enc(id)}/diff` + (path ? `?path=${enc(path)}` : "")),

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
      : a.url.startsWith("/") ? daemon + a.url
      : `${daemon}/v1/pull-requests/${enc(prId)}/evidence/artifacts/${enc(a.id)}`,
  setStandingApproval: (projectId: string, on: boolean) =>
    req<Project>("PATCH", `/v1/projects/${enc(projectId)}`, { standing_approval: on }),

  terminal: (id: string) => req<Terminal>("GET", `/v1/terminals/${enc(id)}`),
  /** Appends a snapshot `worker.output` event for the terminal and returns it. */
  terminalSnapshot: (id: string) => req<DaemonEvent<TerminalOutput>>("POST", `/v1/terminals/${enc(id)}/snapshot`),
  terminalResize: (id: string, cols: number, rows: number) =>
    req<Terminal>("POST", `/v1/terminals/${enc(id)}/resize`, { cols, rows }),
  /** Ordered input: one request in flight per terminal; keys typed meanwhile are coalesced. */
  terminalInput: (id: string, data: string) => enqueueInput(id, data),
};

const te = new TextEncoder();
export function utf8ToB64(s: string): string {
  const bytes = te.encode(s);
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}
export function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// Concurrent fetches can reach the daemon out of order (POC: "echo" arrived as "ecoh"
// when typing fast), so input is serialized per task and coalesced while a POST is in flight.
interface InputQueue { buf: string; busy: boolean; waiters: ((e?: unknown) => void)[] }
const inputQ = new Map<string, InputQueue>();
function enqueueInput(id: string, data: string): Promise<void> {
  let q = inputQ.get(id);
  if (!q) inputQ.set(id, (q = { buf: "", busy: false, waiters: [] }));
  q.buf += data;
  const done = new Promise<void>((resolve, reject) => q!.waiters.push((e) => (e ? reject(e) : resolve())));
  if (!q.busy) void pump(id, q);
  return done;
}
async function pump(id: string, q: InputQueue) {
  q.busy = true;
  while (q.buf) {
    const chunk = q.buf, waiters = q.waiters;
    q.buf = ""; q.waiters = [];
    let err: unknown;
    try {
      await req<void>("POST", `/v1/terminals/${enc(id)}/input`, { data_b64: utf8ToB64(chunk) });
    } catch (e) { err = e; }
    waiters.forEach((w) => w(err));
  }
  q.busy = false;
}
