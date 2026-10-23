// Tasks: state, steering, failovers and the changes a worker made.

import type { Issue } from "./beads";
import { enc, req } from "./client";

export type TaskState =
  | "queued" | "running" | "needs_decision" | "blocked" | "paused"
  | "in_review" | "done" | "failed" | "unknown";
export type TaskKind = "ship" | "scout";
export interface Task {
  id: string; project_id: string; title: string; state: TaskState;
  kind?: TaskKind | null; state_note?: string | null; harness?: string | null;
  pull_request_url?: string | null; created_at: string; updated_at: string;
  /** The account the worker was started under (an `Account.id`), when its harness has accounts. */
  account_id?: string | null;
  /** Rate limits the worker hit, oldest first, and where it moved (quark#26). */
  failovers?: AccountFailover[];
  /** The model the worker runs: what its session log reports, else what it was started with. */
  model?: string | null;
  /** The git branch checked out in the task's working copy, while it exists. */
  branch?: string | null;
  /** When the finished worker was cleared off the board; its history is kept. */
  cleared_at?: string | null;
}

/** What clearing a project's finished workers did (`POST /v1/projects/{id}/tasks:clear`). */
export interface ClearedTasks {
  cleared: string[];
  /** Workers left on the board: `unlanded_work` (their worktree holds work) or `engine_failed`. */
  kept: { task_id: string; title: string; code: string; reason: string }[];
}

export type FailoverOutcome = "relaunched" | "no_healthy_account" | "relaunch_failed";
export interface AccountFailover {
  from_account_id: string; to_account_id?: string | null; pool?: string | null;
  outcome: FailoverOutcome; signal: string; detail?: string | null; at: string;
}
export type ChangeStatus = "added" | "modified" | "deleted" | "renamed" | "copied" | "type_changed" | "untracked";
export interface ChangedFile {
  path: string; old_path?: string | null; status: ChangeStatus; additions?: number | null; deletions?: number | null;
}
export interface TaskChanges { task_id: string; base_ref: string; base: string; head: string; files: ChangedFile[] }
export interface TaskDiff { task_id: string; base: string; path?: string | null; patch: string; truncated: boolean }

/** How a task's changes reach a pull request. */
export type TaskDelivery = "gated" | "direct" | "local_only";
/** A task's brief as the coordinator wrote it. */
export interface DispatchBrief {
  /** The whole brief, Markdown. */
  markdown: string;
  /** The section restating what you asked for, without its heading. */
  intent?: string | null;
  /** The section where the coordinator specifies the work, without its heading. */
  spec?: string | null;
  /** The brief as the worker was started with it, when the engine wraps the coordinator's. */
  launch?: string | null;
}
/** How and why a task's worker was dispatched (`GET /v1/tasks/{id}/brief`); unknown parts are null. */
export interface TaskBrief {
  task_id: string;
  /** The issue it was dispatched for: the first issue id in its title, then its brief. */
  issue?: Issue | null;
  brief?: DispatchBrief | null;
  dispatched_at?: string | null;
  dispatched_by?: string | null;
  delivery?: TaskDelivery | null;
  merges_without_asking?: boolean | null;
  pull_request_url?: string | null;
}

export const tasksApi = {
  tasks: (pid: string) => req<Task[]>("GET", `/v1/projects/${enc(pid)}/tasks`),
  task: (id: string) => req<Task>("GET", `/v1/tasks/${enc(id)}`),
  steer: (id: string, text: string) => req<void>("POST", `/v1/tasks/${enc(id)}/messages`, { text }),
  cancel: (id: string) => req<void>("POST", `/v1/tasks/${enc(id)}:cancel`),
  relaunch: (id: string) => req<void>("POST", `/v1/tasks/${enc(id)}:relaunch`),
  /** With `discard`, work that has not landed is thrown away; commits stay on the worker's branch. */
  clearTask: (id: string, discard = false) =>
    req<void>("POST", `/v1/tasks/${enc(id)}:clear`, discard ? { discard } : undefined),
  clearTasks: (pid: string, states: TaskState[] = ["done", "failed"], discard = false) =>
    req<ClearedTasks>("POST", `/v1/projects/${enc(pid)}/tasks:clear`, { states, discard }),
  brief: (id: string) => req<TaskBrief>("GET", `/v1/tasks/${enc(id)}/brief`),
  changes: (id: string) => req<TaskChanges>("GET", `/v1/tasks/${enc(id)}/changes`),
  diff: (id: string, path?: string) =>
    req<TaskDiff>("GET", `/v1/tasks/${enc(id)}/diff` + (path ? `?path=${enc(path)}` : "")),
};
