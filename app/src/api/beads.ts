import { enc, req } from "./client";
import type { MemoryEvidence, MemorySource } from "./memory";

// Beads (https://beads.gascity.com): the Project's issues, decision beads and memories, in one
// Beads database per Project that quarkd runs in server mode under its home, outside every repo.
// It syncs with an issue tracker only through the Project's sync rules, and has none by default.

/** `missing`: no database yet (`:setup` creates one). `unavailable`: `bd` or `dolt` is not installed. */
export type BeadsState = "missing" | "setting_up" | "ready" | "failed" | "unavailable";
export interface TrackerSync {
  at: string; ok: boolean;
  /** What happened, or why it failed. */
  message: string;
}
/** Trackers a rule can sync with; Quark syncs GitHub Issues so far. */
export type SyncTracker = "github";
/** `pull` brings the tracker's issues in, `push` sends this rule's issues out. */
export type SyncDirection = "pull" | "push" | "both";
/** One explicit link between the Project's Beads and a tracker. A rule pushes only the issues
 *  that came from its repository plus new ones carrying its `label`; decisions never sync. */
export interface SyncRule {
  id: string; tracker: SyncTracker;
  /** `owner/repo` for GitHub. */
  repository: string;
  direction: SyncDirection; label: string;
  /** A paused rule is kept but does not run. */
  enabled: boolean;
  last_sync?: TrackerSync | null;
}
/** A rule as written; no id makes a new one, no label means `repo:<name>`. */
export interface WriteSyncRule {
  id?: string | null; tracker?: SyncTracker; repository: string; direction: SyncDirection;
  label?: string | null; enabled?: boolean;
}
export interface BeadsStatus {
  project_id: string; state: BeadsState;
  /** Why it is unavailable or failed, or what setup is doing now. */
  detail?: string | null;
  /** The Beads directory quarkd uses (`.beads` lives here); agents get it as `BEADS_DIR`. */
  dir?: string | null;
  /** Issue id prefix, as in `qk-44`. */
  prefix?: string | null;
  /** Trackers it syncs with; none by default. */
  sync_rules?: SyncRule[];
  /** The latest sync run, over every rule. */
  last_sync?: TrackerSync | null;
}

/** Beads' own values; `deferred`, `pinned` and `hooked` also occur. */
export type IssueStatus = "open" | "in_progress" | "blocked" | "closed" | (string & {});
/** bug, feature, task, epic, chore, decision, spike, story, milestone, or a custom type. */
export type IssueType = string;
/** An issue linked to another, with enough to label the link. */
export interface IssueRef { id: string; title: string; status: IssueStatus; issue_type: IssueType }
export interface Issue {
  id: string; title: string; description: string; status: IssueStatus;
  /** 0 (highest) to 4. */
  priority: number;
  issue_type: IssueType; labels: string[];
  assignee?: string | null; owner?: string | null; created_by?: string | null;
  created_at: string; updated_at: string; closed_at?: string | null;
  /** The GitHub issue it mirrors, as a URL, when synced. */
  external_ref?: string | null;
  /** Issues this one waits for (`blocks` dependencies), closed ones included. */
  blocked_by: string[];
  /** Open and nothing it waits for is still open. */
  ready: boolean;
  /** Something it waits for is still open. */
  blocked: boolean;
}
export interface IssueComment { author: string; text: string; created_at: string }
export interface IssueDetail extends Issue {
  notes?: string | null; design?: string | null; acceptance_criteria?: string | null;
  blocked_by_issues: IssueRef[];
  /** Issues waiting for this one. */
  blocks_issues: IssueRef[];
  /** Non-blocking links (relates-to, discovered-from, supersedes, parent and children). */
  related_issues: (IssueRef & { kind: string })[];
  comments: IssueComment[];
}
/** The Issues tab's filters. */
export type IssueFilter = "ready" | "in_progress" | "blocked" | "closed" | "open" | "all";

// The New issue side chat: Matt describes the work, the coordinator drafts beads, nothing is created
// until the draft is accepted.
export type IssueDraftState = "open" | "accepted" | "discarded";
export interface DraftMessage { role: "user" | "coordinator"; text: string; at: string }
export interface DraftIssue {
  /** Draft-local key ("1", "2"); `blocked_by` may name another draft's key or an existing issue id. */
  key: string; title: string; issue_type: IssueType; priority: number; labels: string[]; description: string;
  blocked_by: string[];
}
export interface IssueDraft {
  id: string; project_id: string; state: IssueDraftState;
  messages: DraftMessage[]; issues: DraftIssue[];
  /** Existing issues the coordinator judged related but separate. */
  related: string[];
  /** A message went to the coordinator and its drafts have not come back yet. */
  waiting: boolean;
  /** Issue ids created on accept, by draft key. */
  created: Record<string, string>;
  created_at: string; updated_at: string;
}
export interface AcceptIssueDraft {
  /** The drafts as edited; the stored drafts when absent. */
  issues?: DraftIssue[] | null;
  /** Ask the coordinator to start a worker on the first created issue. */
  start_worker?: boolean;
}

export interface BeadsMemory {
  key: string; value: string;
  /** Where it came from, when it was accepted from a memory proposal in Quark. */
  evidence?: MemoryEvidence | null; source?: MemorySource | null;
  accepted_at?: string | null; accepted_by?: string | null;
}

export const beadsApi = {
  beads: (pid: string) => req<BeadsStatus>("GET", `/v1/projects/${enc(pid)}/beads`),
  /** Creates the Project's Beads database (normally done at Project creation; this retries);
   *  returns at once with `setting_up`, then `beads.status` events. */
  setupBeads: (pid: string) => req<BeadsStatus>("POST", `/v1/projects/${enc(pid)}/beads:setup`),
  /** Runs the Project's enabled sync rules now. */
  syncBeads: (pid: string) => req<BeadsStatus>("POST", `/v1/projects/${enc(pid)}/beads:sync`),
  /** Replaces the Project's sync rules; an empty list turns sync off. */
  setSyncRules: (pid: string, rules: WriteSyncRule[]) =>
    req<BeadsStatus>("PUT", `/v1/projects/${enc(pid)}/beads/sync-rules`, { rules }),
  issues: (pid: string, filter: IssueFilter = "all") =>
    req<Issue[]>("GET", `/v1/projects/${enc(pid)}/issues?filter=${filter}`),
  issue: (pid: string, id: string) => req<IssueDetail>("GET", `/v1/projects/${enc(pid)}/issues/${enc(id)}`),
  issueDrafts: (pid: string) => req<IssueDraft[]>("GET", `/v1/projects/${enc(pid)}/issue-drafts`),
  /** Opens a draft with Matt's first message and sends it to the coordinator. */
  startIssueDraft: (pid: string, text: string) => req<IssueDraft>("POST", `/v1/projects/${enc(pid)}/issue-drafts`, { text }),
  /** Another message in the side chat ("make the first one P0"). */
  refineIssueDraft: (id: string, text: string) => req<IssueDraft>("POST", `/v1/issue-drafts/${enc(id)}/messages`, { text }),
  acceptIssueDraft: (id: string, body: AcceptIssueDraft = {}) => req<IssueDraft>("POST", `/v1/issue-drafts/${enc(id)}:accept`, body),
  discardIssueDraft: (id: string) => req<IssueDraft>("POST", `/v1/issue-drafts/${enc(id)}:discard`),
  memories: (pid: string) => req<BeadsMemory[]>("GET", `/v1/projects/${enc(pid)}/beads/memories`),
  forgetMemory: (pid: string, key: string) => req<void>("DELETE", `/v1/projects/${enc(pid)}/beads/memories/${enc(key)}`),
};
