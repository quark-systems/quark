import { enc, req } from "./client";
import { AccountFailover } from "./tasks";

// Why this agent (ADR-11, quark#27): one record per worker spawn, kept after the task ends.
export type DispatchTrigger = "spawn" | "relaunch";
export type DispatchDecider = "classifier" | "coordinator" | "default_rule" | "relaunch";
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
  /** `provider` is "none" when no classifier is configured. One that did not answer has no model or confidence. */
  classifier: { provider: string; model?: string | null; confidence?: number | null };
  /** The rate limit this relaunch answered, when the daemon moved the worker to another account (quark#26). */
  failover?: AccountFailover | null;
  recorded_at: string;
}
// Testing a description against the dispatch rules (ADR-11, quark#24). Nothing is dispatched or recorded.
export type DispatchCheckKind = "harness_installed" | "model_accepted" | "account_health" | "quota_headroom";
export interface DispatchCheck { check: DispatchCheckKind; passed: boolean; detail: string }
/** One profile of the rules; `passed` is true when every check passed. `harness` is a Quark harness id. */
export interface DispatchTestCandidate extends DispatchCandidate {
  /** The rule listing the profile; its id is "default" for a default profile. */
  rule: { id: string; when?: string | null };
  rule_name?: string | null; effort?: string | null; pool?: string | null;
  /** One per `DispatchCheckKind`, in that order. */
  checks: DispatchCheck[];
}
export interface DispatchTest {
  project_id: string;
  /** "classifier" when the resolution selects a profile, else "coordinator". */
  decided_by: DispatchDecider; summary: string;
  rule?: DispatchRecord["rule"]; resolution: DispatchRecord["resolution"];
  /** `provider` is "none" when no classifier is configured (the coordinator would pick). */
  classifier: DispatchRecord["classifier"];
  /** The profile the resolution selected; null when the coordinator would pick. */
  chosen?: DispatchRecord["chosen"] | null;
  /** The matched rule's profiles when the resolution weighed them, else every profile of every rule and the default. */
  candidates: DispatchTestCandidate[];
}

// The rule editor (quark#25): `dispatch.yaml` on the Project repo's `main`, without its classifier block.
export type DispatchSelect = "ordered" | "quota-balanced";
export interface DispatchFloor { scope: string; min_percent: number; provider?: string | null }
/** One candidate of a rule or of the default. `provider`, `floor` and `pricing` are the engine's, kept as written. */
export interface DispatchProfile {
  harness: string; model?: string | null; effort?: string | null; pool?: string | null;
  provider?: string | null; floor?: DispatchFloor | null; pricing?: string | null;
}
/** `why`, `approval` and `floor` are the engine's, kept as written. */
export interface DispatchRuleSpec {
  name?: string | null; when: string;
  /** The rule's `use` list, in order. */
  candidates: DispatchProfile[];
  /** Absent uses `default_select`. */
  select?: DispatchSelect | null;
  why?: string | null; approval?: string | null; floor?: DispatchFloor | null;
}
export interface DispatchRulesDraft { default_select?: DispatchSelect | null; rules: DispatchRuleSpec[]; default: DispatchProfile[] }
export interface DispatchRules extends DispatchRulesDraft {
  project_id: string;
  /** Names this version of the file; sent back when saving. Null when `main` has no `dispatch.yaml`. */
  revision?: string | null;
  /** The commit on `main` that last changed the file. */
  commit?: string | null;
  /** The classifier in effect: the file's block over the user-level default. Saving keeps the file's own block. */
  classifier?: Record<string, unknown> | null;
}

export const dispatchApi = {
  /** What the Project's dispatch rules would do with a task description; with a `draft`, what those unsaved rules would do. 409 `dispatch_invalid` when `dispatch.yaml` does not compile. */
  testDispatch: (pid: string, description: string, draft?: DispatchRulesDraft) =>
    req<DispatchTest>("POST", `/v1/projects/${enc(pid)}/dispatch:test`, { description, ...(draft ? { draft } : {}) }),
  /** 409 `dispatch_invalid` when `dispatch.yaml` does not compile, `no_project_repo` when the Project has none. */
  dispatchRules: (pid: string) => req<DispatchRules>("GET", `/v1/projects/${enc(pid)}/dispatch`),
  /** Commits `dispatch.yaml` to the Project repo. 400 `dispatch_invalid` with the compile error; 409 `dispatch_changed` when `main` moved on from `revision`. */
  saveDispatchRules: (pid: string, draft: DispatchRulesDraft, revision?: string | null) =>
    req<DispatchRules>("PUT", `/v1/projects/${enc(pid)}/dispatch`, { revision: revision ?? null, ...draft }),
  /** Every dispatch of the task, oldest first: its first spawn, then each relaunch. */
  dispatch: (id: string) => req<DispatchRecord[]>("GET", `/v1/tasks/${enc(id)}/dispatch`),
};
