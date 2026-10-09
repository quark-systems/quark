// Projects and daemon health.

import { enc, req } from "./client";

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
export interface Health { status: string; version: string; engine: string; last_seq: number }

export const projectsApi = {
  forgeRepositories: (refresh = false) => req<ForgeRepository[]>("GET", `/v1/forge/repositories${refresh ? "?refresh=true" : ""}`),
  health: () => req<Health>("GET", "/v1/health"),
  projects: () => req<Project[]>("GET", "/v1/projects"),
  project: (id: string) => req<Project>("GET", `/v1/projects/${enc(id)}`),
  createProject: (p: CreateProject) => req<Project>("POST", "/v1/projects", p),
  provision: (id: string) => req<void>("POST", `/v1/projects/${enc(id)}:provision`),
  setStandingApproval: (projectId: string, on: boolean) =>
    req<Project>("PATCH", `/v1/projects/${enc(projectId)}`, { standing_approval: on }),
};

/** A GitHub repository the daemon's `gh` account can reach. */
export interface ForgeRepository {
  full_name: string; private: boolean; archived: boolean; description?: string | null;
  pushed_at?: string | null; ssh_url: string; clone_url: string;
}
