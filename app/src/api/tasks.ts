// Tasks: state, steering, failovers and the changes a worker made.

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

export const tasksApi = {
  tasks: (pid: string) => req<Task[]>("GET", `/v1/projects/${enc(pid)}/tasks`),
  task: (id: string) => req<Task>("GET", `/v1/tasks/${enc(id)}`),
  steer: (id: string, text: string) => req<void>("POST", `/v1/tasks/${enc(id)}/messages`, { text }),
  cancel: (id: string) => req<void>("POST", `/v1/tasks/${enc(id)}:cancel`),
  relaunch: (id: string) => req<void>("POST", `/v1/tasks/${enc(id)}:relaunch`),
  changes: (id: string) => req<TaskChanges>("GET", `/v1/tasks/${enc(id)}/changes`),
  diff: (id: string, path?: string) =>
    req<TaskDiff>("GET", `/v1/tasks/${enc(id)}/diff` + (path ? `?path=${enc(path)}` : "")),
};
