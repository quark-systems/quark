// A Project's automation: the inbox, trigger rules and the away policy (slice 7).

import { enc, req } from "./client";

/** A message from any channel, waiting until the coordinator handles it. */
export interface InboxMessage { id: string; channel: string; from: string; body: string; at: string; task_id?: string | null }

/** `when`: `{source: "event", kind, task?, fields?}`, `{source: "every", secs}`, `{source: "at", at}` or
 *  `{source: "command", argv, interval_secs?, stable?, timeout_secs?, expect?, error_budget?}`. */
export type TriggerCondition = { source: "event" | "every" | "at" | "command"; [k: string]: unknown };
/** `then`: `{do: "wake", note}`, `{do: "inbox", body}`, `{do: "steer", task, text}` or `{do: "command", argv, timeout_secs?}`. */
export type TriggerAction = { do: "wake" | "inbox" | "steer" | "command"; [k: string]: unknown };
export interface TriggerRule {
  id: string; description: string; enabled: boolean;
  /** Stops after its first fire; `at` and `command` conditions always fire once. */
  once: boolean;
  when: TriggerCondition; then: TriggerAction;
  defined_at: string;
  /** Fires so far, or would-be fires when the engine is not acting. */
  fires: number;
}
export interface PutTriggerRule { description?: string; enabled?: boolean; once?: boolean; when: TriggerCondition; then: TriggerAction }

export type AwayPosture = "present" | "away" | "quiet";
export type AwayOccasion = "progress" | "paused" | "done" | "decision" | "blocked" | "failed" | "stale" | "inbound" | "trigger_fired" | "trigger_failed";
export type AwayReach = "silent" | "digest" | "notify" | "hold";
export interface AwayRoute { posture: AwayPosture; occasion: AwayOccasion; wake: boolean; user: AwayReach; overridden?: boolean }
export interface AwaySettings { posture: AwayPosture; digest_secs: number; routes: AwayRoute[]; waiting: number; held: number }
export interface UpdateAwayPolicy { digest_secs: number; routes: AwayRoute[] }

export interface ProjectAutomation {
  project_id: string;
  /** The engine acts on rules and routes; otherwise it records what it would do beside the current engine. */
  acting: boolean;
  inbox: InboxMessage[]; rules: TriggerRule[]; away: AwaySettings;
}

const base = (p: string) => `/v1/projects/${enc(p)}`;
export const automationApi = {
  projectAutomation: (projectId: string) => req<ProjectAutomation>("GET", `${base(projectId)}/automation`),
  postInboxNote: (projectId: string, body: string) => req<void>("POST", `${base(projectId)}/inbox`, { body }),
  putTriggerRule: (projectId: string, id: string, rule: PutTriggerRule) =>
    req<TriggerRule>("PUT", `${base(projectId)}/triggers/${enc(id)}`, rule),
  deleteTriggerRule: (projectId: string, id: string) => req<void>("DELETE", `${base(projectId)}/triggers/${enc(id)}`),
  putAwayPolicy: (projectId: string, u: UpdateAwayPolicy) => req<AwaySettings>("PUT", `${base(projectId)}/away/policy`, u),
};
