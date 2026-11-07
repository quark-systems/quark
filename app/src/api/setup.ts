// First-run setup: what this machine still needs, and the fixes quarkd can run (crate::setup).

import { enc, req } from "./client";

export type SetupStatus = "ok" | "warn" | "missing";
export interface SetupFix { label: string; command: string; runnable: boolean }
export interface SetupCheck {
  id: string; title: string; status: SetupStatus; required: boolean;
  detail: string; why: string; fix?: SetupFix | null;
}
export interface SetupReport { ready: boolean; checks: SetupCheck[] }
export interface SetupFixOutcome { id: string; ok: boolean; output: string; report: SetupReport }

export const setupApi = {
  setup: () => req<SetupReport>("GET", "/v1/setup"),
  fixSetup: (id: string) => req<SetupFixOutcome>("POST", `/v1/setup/${enc(id)}`),
};
