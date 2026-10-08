import { enc, req } from "./client";

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
  /** Its key in the Project's Beads memories (`bd remember`), when it was accepted into Beads. */
  beads_key?: string | null;
}
export interface MemoryProposal {
  id: string; project_id: string; text: string; evidence: MemoryEvidence; source: MemorySource;
  state: MemoryProposalState; proposed_at: string; decided_at?: string | null; decided_by?: string | null;
  /** Set once accepted. */
  entry?: MemoryEntry | null;
}
/** Who should know: this Project (its Beads memories, or `memory/` without Beads), all of the user's
 * Projects (user-level memory), or anyone working in the repo (also asks the coordinator for a PR to AGENTS.md). */
export type MemoryScope = "project" | "user" | "repo";
export interface AcceptMemoryProposal { text?: string | null; decided_by?: string | null; scope?: MemoryScope | null }
export interface RejectMemoryProposal { decided_by?: string | null }
/** A Project repo commit that touched `memory/`, with its diff there. */
export interface MemoryCommit { commit: string; subject: string; author?: string | null; date?: string | null; patch: string }
/** One file of user-level memory (`~/.quark/memory/`), which every Project's coordinator reads. */
export interface UserMemoryEntry {
  id: string; path: string; text: string; evidence: MemoryEvidence; source?: MemorySource | null; date?: string | null;
  /** Where it was promoted from; absent on a hand-written file. */
  project_id?: string | null; project_name?: string | null; entry_id?: string | null; commit?: string | null;
  promoted_at?: string | null; promoted_by?: string | null;
}

export const memoryApi = {
  memoryProposals: (pid: string, state?: MemoryProposalState) =>
    req<MemoryProposal[]>("GET", `/v1/projects/${enc(pid)}/memory/proposals` + (state ? `?state=${state}` : "")),
  /** Commits the entry (edited when `text` is given) to the Project repo and returns the accepted proposal. */
  acceptMemoryProposal: (pid: string, id: string, body: AcceptMemoryProposal = {}) =>
    req<MemoryProposal>("POST", `/v1/projects/${enc(pid)}/memory/proposals/${enc(id)}:accept`, body),
  rejectMemoryProposal: (pid: string, id: string, body: RejectMemoryProposal = {}) =>
    req<MemoryProposal>("POST", `/v1/projects/${enc(pid)}/memory/proposals/${enc(id)}:reject`, body),
  memory: (pid: string) => req<MemoryEntry[]>("GET", `/v1/projects/${enc(pid)}/memory`),
  /** What an entry's `commit` links to. */
  memoryCommit: (pid: string, commit: string) => req<MemoryCommit>("GET", `/v1/projects/${enc(pid)}/memory/commits/${enc(commit)}`),
  /** Copies the entry into user-level memory; promoting it again returns the same copy. */
  promoteMemoryEntry: (pid: string, entryId: string, promoted_by?: string | null) =>
    req<UserMemoryEntry>("POST", `/v1/projects/${enc(pid)}/memory/${enc(entryId)}:promote`, { promoted_by: promoted_by ?? null }),
  userMemory: () => req<UserMemoryEntry[]>("GET", "/v1/memory"),
};
