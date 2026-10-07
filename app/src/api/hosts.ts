// The Hosts view and each Project's slice of its hosts, read from the daemon's event log.

import { enc, req } from "./client";

export type HostRuntime = "local" | "ssh" | "hosted" | "private_cloud";
export type HostHealthStatus = "healthy" | "degraded" | "unreachable";
export interface HostHealth { status: HostHealthStatus; reason?: string | null; since?: string | null }
export interface HostCapacity { cpus: number; memory_bytes: number; disk_bytes: number; max_workers: number }
export interface QuarkDiskUse { worktrees_bytes: number; logs_bytes: number; caches_bytes: number; event_log_bytes: number }
/** `cpu` and `memory_pressure` are 0 to 1. */
export interface HostReading {
  at: string; cpu: number; memory_used_bytes: number; memory_total_bytes: number; memory_pressure: number;
  disk_free_bytes: number; quark_disk: QuarkDiskUse;
}
export interface HostPoint { at: string; cpu: number; memory_used_bytes: number; memory_pressure: number; disk_free_bytes: number }
/** A coordinator's (`engine_task` absent) or task's share. */
export interface UsagePart {
  engine_task?: string | null; task_id?: string | null; title?: string | null;
  cpu: number; memory_bytes: number; disk_bytes: number;
}
export interface ProjectUsage {
  project_id: string; project_name?: string | null; cpu: number; memory_bytes: number; disk_bytes: number; parts: UsagePart[];
}
export type WorktreeSlotState = "idle" | "in_use" | "dirty" | "leased" | "quarantined";
export interface WorktreeSlotView { path: string; repo: string; state: WorktreeSlotState; holder?: string | null; project_id?: string | null }
export interface HostWorktrees {
  at: string; idle: number; in_use: number; dirty: number; leased: number; quarantined: number;
  error?: string | null; slots: WorktreeSlotView[];
}
export interface HostView {
  id: string; name: string; runtime: HostRuntime; os: string; arch: string;
  capacity: HostCapacity; health: HostHealth; latest?: HostReading | null;
  /** Oldest first, at most 120 points. */
  series: HostPoint[]; projects: ProjectUsage[]; worktrees?: HostWorktrees | null;
}
export interface HostsView { hours: number; hosts: HostView[]; error?: string | null }
export interface UsagePoint { at: string; cpu: number; memory_bytes: number; disk_bytes: number }
export interface ProjectHostSlice {
  host_id: string; name: string; health: HostHealth; capacity: HostCapacity;
  now?: ProjectUsage | null; host?: HostReading | null; series: UsagePoint[]; worktrees: WorktreeSlotView[];
}
export interface ProjectHosts { project_id: string; hours: number; hosts: ProjectHostSlice[]; error?: string | null }

const hrs = (hours?: number) => (hours ? `?hours=${hours}` : "");

export const hostsApi = {
  hosts: (hours?: number) => req<HostsView>("GET", `/v1/hosts${hrs(hours)}`),
  projectHosts: (projectId: string, hours?: number) => req<ProjectHosts>("GET", `/v1/projects/${enc(projectId)}/hosts${hrs(hours)}`),
};
