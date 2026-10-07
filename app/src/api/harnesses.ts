// Agent harnesses and agent config validation.

import { req } from "./client";
import { AgentConfig } from "./projects";

export type Effort = "low" | "medium" | "high" | "xhigh" | "max";
export type AgentRole = "coordinator" | "worker";
export interface HarnessInfo {
  id: string; name: string; roles: AgentRole[];
  install: { installed: boolean; version?: string | null; path?: string | null; install_hint: string };
  models: { selection: "free_form" | "provider_qualified" | "automatic"; discovery?: string | null };
  efforts: Effort[];
  auth: HarnessAuth;
  transcript: boolean;
  /** Variable that selects an account's config directory; null when the harness has only its default account. */
  account_env?: string | null;
}
export type AuthState = "configured" | "not_configured" | "unknown";
export interface HarnessAuth { state: AuthState; detail?: string | null }
export interface ValidationIssue { field: string; code: string; message: string }
export interface HarnessValidation { valid: boolean; errors: ValidationIssue[]; warnings: ValidationIssue[] }

export const harnessesApi = {
  harnesses: () => req<HarnessInfo[]>("GET", "/v1/harnesses"),
  validateAgent: (config: AgentConfig, role: AgentRole) =>
    req<HarnessValidation>("POST", "/v1/harnesses:validate", { config, role }),
};
