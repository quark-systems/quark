// Importing projects from another tool (POST /v1/imports/firstmate).

import { req } from "./client";

export interface ImportFirstmateRequest {
  home: string;
  dry_run?: boolean;
  projects?: string[];
  /** Id prefix to firstmate project, for work nothing else assigns. */
  routes?: Record<string, string>;
  include_archive?: boolean;
  wait_secs?: number | null;
}
export type ImportOutcome = "imported" | "present" | "pending" | "not_imported";
export interface ImportInventoryItem {
  category: string; source: string; target: string; outcome: ImportOutcome; count: number; detail: string;
}
export interface ImportCount { imported: number; present: number }
export interface ImportedProject {
  name: string; project_id?: string | null; project: "created" | "existing"; repo_url?: string | null;
  outcome: ImportOutcome; detail?: string | null; dispatch: ImportOutcome; tasks: ImportCount; holds: ImportCount;
}
export interface FirstmateImportReport {
  home: string; dry_run: boolean; inventory: ImportInventoryItem[]; projects: ImportedProject[];
  unrouted: string[]; notes: string[];
}

export const importsApi = {
  importFirstmate: (r: ImportFirstmateRequest) => req<FirstmateImportReport>("POST", "/v1/imports/firstmate", r),
};
