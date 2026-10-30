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
  /** When it was archived: its agents are stopped and it is hidden until unarchived. */
  archived_at?: string | null;
}

export interface CreateProject {
  name: string; goal?: string | null; workspace_path?: string | null;
  repos?: RepoSource[]; agent_config?: AgentConfig; dispatch_preset?: DispatchPreset; delivery?: DeliveryPolicy;
}
export interface Health {
  status: string; version: string; engine: string; last_seq: number;
  /** The `/v1` contract version; daemons from before it existed speak 1. */
  api_version?: number;
}

export const projectsApi = {
  /** Saves a file pasted or dropped into a prompt box where the Project's agents can read it. */
  attach: (pid: string, file: Blob, name: string) =>
    req<Attachment>("POST", `/v1/projects/${enc(pid)}/attachments?name=${enc(name)}`, file),
  forgeRepositories: (refresh = false) => req<ForgeRepository[]>("GET", `/v1/forge/repositories${refresh ? "?refresh=true" : ""}`),
  health: () => req<Health>("GET", "/v1/health"),
  /** Every project, archived ones too; screens hide those with `archived_at`. */
  projects: () => req<Project[]>("GET", "/v1/projects?include_archived=true"),
  project: (id: string) => req<Project>("GET", `/v1/projects/${enc(id)}`),
  createProject: (p: CreateProject) => req<Project>("POST", "/v1/projects", p),
  provision: (id: string) => req<void>("POST", `/v1/projects/${enc(id)}:provision`),
  archiveProject: (id: string) => req<Project>("POST", `/v1/projects/${enc(id)}:archive`),
  unarchiveProject: (id: string) => req<Project>("POST", `/v1/projects/${enc(id)}:unarchive`),
  /** Deletes the project and what Quark keeps for it; `confirmName` must be its name. */
  deleteProject: (id: string, confirmName: string) =>
    req<void>("DELETE", `/v1/projects/${enc(id)}`, { confirm_name: confirmName }),
  setStandingApproval: (projectId: string, on: boolean) =>
    req<Project>("PATCH", `/v1/projects/${enc(projectId)}`, { standing_approval: on }),
};

/** A GitHub repository the daemon's `gh` account can reach. */
export interface ForgeRepository {
  full_name: string; private: boolean; archived: boolean; description?: string | null;
  pushed_at?: string | null; ssh_url: string; clone_url: string;
}

/** A file saved by `POST /v1/projects/{id}/attachments`; `path` is on the daemon's machine. */
export interface Attachment { path: string; name: string; bytes: number }
