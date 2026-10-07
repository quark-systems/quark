// The Project dashboard's Metrics tab: how the Project's work has gone, from the event log.

import { enc, req } from "./client";
import { AccountQuota } from "./accounts";

export interface DayCount { date: string; done: number; failed: number }
/** Tasks that finished in the window, with one entry per UTC day, oldest first. */
export interface Throughput { done: number; failed: number; per_day: DayCount[] }
/** From a task's first spawn to its `done`. */
export interface LeadTime { tasks: number; median_s?: number | null; p90_s?: number | null }
/** Rates are 0 to 1, absent when there is nothing to divide by. */
export interface GateMetrics { pass_rate?: number | null; first_time_green: number; first_time_green_rate?: number | null }
export interface Interventions { decisions: number; blockers: number; per_finished_task?: number | null; relaunches: number }
export interface Failovers { relaunched: number; no_healthy_account: number; relaunch_failed: number }
export interface AccountUse { account_id: string; harness: string; label: string; tasks: number; quota: AccountQuota }
export interface UnavailableMetric { metric: string; reason: string }
/** One coordinator's turns and tokens; shares are 0 to 1. A turn that changed nothing only acknowledged status. */
export interface CoordinatorTurns {
  turns: number; ack_turns: number; input_tokens: number; output_tokens: number; cache_read_tokens: number;
  turns_per_task?: number | null; tokens_per_task?: number | null; ack_share?: number | null;
}
/** firstmate's coordinator (read from its transcript) beside the native one. */
export interface CoordinatorMetrics { tasks: number; baseline: CoordinatorTurns; native: CoordinatorTurns; would_wake_turns: number }
/** One kind of agent's tokens and cost. Cost is in US dollars at API list price, or the harness's own figure. */
export interface TokenSpend {
  turns: number; input_tokens: number; output_tokens: number; cache_read_tokens: number;
  usd?: number | null;
  /** Tokens of models with no known price, left out of `usd`. */
  unpriced_tokens: number;
}
export interface ModelSpend { model: string; input_tokens: number; output_tokens: number; usd?: number | null }
export interface SpendMetrics {
  workers: TokenSpend; coordinator: TokenSpend;
  /** Most expensive first. */
  by_model: ModelSpend[];
  done_tasks: number;
  /** Lifetime worker spend per task done in the window. */
  usd_per_done_task?: number | null;
}
export interface ProjectMetrics {
  project_id: string; days: number; from: string; to: string;
  /** The Project's first event in the log; numbers reach no further back. */
  log_started_at?: string | null;
  throughput: Throughput; lead_time: LeadTime; gates: GateMetrics; interventions: Interventions;
  failovers: Failovers; accounts: AccountUse[]; unavailable: UnavailableMetric[];
  /** Absent from daemons older than the native coordinator. */
  coordinator?: CoordinatorMetrics;
  /** Absent from daemons older than token capture. */
  spend?: SpendMetrics;
}

export const metricsApi = {
  projectMetrics: (projectId: string, days?: number) =>
    req<ProjectMetrics>("GET", `/v1/projects/${enc(projectId)}/metrics${days ? `?days=${days}` : ""}`),
};
