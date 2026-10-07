// The Project dashboard's Settings tab: every per-Project switch in one read.

import { enc, req } from "./client";
import { AgentConfig, DeliveryPolicy } from "./projects";

export interface GateCheck { name: string; run: string; timeout_s?: number | null }
export interface GateJourneys { start: string; url: string; dir?: string | null }
/** Holdout tests run when `enabled` and there is at least one category under `holdout/<source>/`. */
export interface HoldoutSettings { enabled: boolean; categories: string[]; timeout_s?: number | null }
export interface SourceVerification { source: string; checks: GateCheck[]; journeys?: GateJourneys | null; holdout: HoldoutSettings }
/** `project.yaml` on the Project repo's `main`. `revision` goes back with a holdout change. */
export interface VerificationSettings { revision?: string | null; sources: SourceVerification[]; error?: string | null }
export interface DispatchSummary { rules: number; default_candidates: number; classifier?: string | null; error?: string | null }
export interface MemorySummary { entries: number; proposals_to_review: number }
export interface ProjectSettings {
  project_id: string; standing_approval: boolean;
  /** Chosen at creation; read-only for now. */
  delivery: DeliveryPolicy;
  agent_config?: AgentConfig | null;
  verification: VerificationSettings; dispatch: DispatchSummary; memory: MemorySummary;
}
export interface HoldoutChange { source: string; enabled: boolean }
export interface UpdateProjectSettings { standing_approval?: boolean; holdout?: HoldoutChange[]; revision?: string | null }

export const settingsApi = {
  projectSettings: (projectId: string) => req<ProjectSettings>("GET", `/v1/projects/${enc(projectId)}/settings`),
  updateProjectSettings: (projectId: string, u: UpdateProjectSettings) =>
    req<ProjectSettings>("PATCH", `/v1/projects/${enc(projectId)}/settings`, u),
};
