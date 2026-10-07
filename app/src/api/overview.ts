// The Project dashboard's Overview tab: live status and "since you last looked", read from the
// daemon's event log.

import { enc, req } from "./client";

export type PulseState = "working" | "needs_decision" | "blocked" | "paused" | "done" | "failed";
export interface StatusCounts { working: number; needs_decision: number; blocked: number; paused: number; done: number; failed: number }
export interface TaskPulse {
  /** The engine's id, as its events carry it; `task_id` and `title` when the daemon has the task. */
  engine_task: string; task_id?: string | null; title?: string | null; state: PulseState;
  /** The last status line's verb and note, as written. */
  verb: string; note: string; at: string;
  harness?: string | null; model?: string | null;
  open_decisions: string[]; pull_request?: string | null;
}
/** Open tasks first, newest first; finished tasks stay for a day. */
export interface LiveStatus { counts: StatusCounts; tasks: TaskPulse[]; last_activity?: string | null }
export type DigestKind = "spawned" | "decision_opened" | "decision_resolved" | "done" | "failed";
export interface DigestItem {
  seq: number; at: string; engine_task?: string | null; task_id?: string | null; title?: string | null; kind: DigestKind; text: string; url?: string | null;
}
export interface OverviewDigest {
  since: number; from?: string | null; to?: string | null; events: number;
  spawned: number; done: number; pull_requests: number; failed: number; decisions_opened: number; decisions_resolved: number;
  /** Newest first, at most 50. */
  highlights: DigestItem[]; truncated: boolean;
}
/** `head` goes back as `since` on the next visit. */
export interface ProjectOverview { project_id: string; head: number; live: LiveStatus; digest?: OverviewDigest | null; error?: string | null }

export const overviewApi = {
  projectOverview: (projectId: string, since?: number | null) =>
    req<ProjectOverview>("GET", `/v1/projects/${enc(projectId)}/overview${since == null ? "" : `?since=${since}`}`),
};
