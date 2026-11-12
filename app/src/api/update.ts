// quarkd's updater (crates/quarkd/src/update.rs): what the feed offers, the download, and
// installing or rolling back the whole Quark.app (quark-updater, update_install.rs).

import { req } from "./client";

export type UpdateState =
  | "disabled" | "idle" | "checking" | "up_to_date" | "available"
  | "downloading" | "ready" | "failed" | "installing";
export type UpdateChannel = "stable" | "beta";
export interface UpdateSettings {
  channel: UpdateChannel;
  check: boolean;
  auto_download: boolean;
  install_when_idle: boolean;
}
export interface UpdateOffer {
  version: string;
  notes?: string;
  pub_date?: string;
  replaces_pulled: boolean;
  rollback_needs_restore?: boolean;
}
export type InstallPhase = "pending" | "staged" | "swapped" | "done" | "rolled_back" | "failed";
export interface UpdateInstall {
  from: string;
  to: string;
  rollback: boolean;
  phase: InstallPhase;
  started_at: string;
  finished_at?: string;
  error?: string;
}
export interface UpdateStatus {
  state: UpdateState;
  current_version: string;
  disabled_reason?: string;
  offer?: UpdateOffer;
  blocked?: string;
  error?: string;
  checked_at?: string;
  bundle?: string;
  last_install?: UpdateInstall;
  can_roll_back: boolean;
  settings: UpdateSettings;
}

export const updateApi = {
  update: () => req<UpdateStatus>("GET", "/v1/update"),
  checkUpdate: () => req<UpdateStatus>("POST", "/v1/update/check"),
  downloadUpdate: () => req<UpdateStatus>("POST", "/v1/update/download"),
  installUpdate: () => req<UpdateStatus>("POST", "/v1/update/install"),
  rollBackUpdate: () => req<UpdateStatus>("POST", "/v1/update/rollback"),
  putUpdateSettings: (s: UpdateSettings) => req<UpdateStatus>("PUT", "/v1/update/settings", s),
};
