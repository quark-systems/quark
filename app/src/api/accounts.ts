import { enc, req } from "./client";
import { HarnessAuth } from "./harnesses";

// Accounts and pools per harness, with per-account quota (quark#23, ADR-11).
export type QuotaState = "pending" | "known" | "unavailable" | "error" | "unsupported";
export interface QuotaWindow {
  id: string; label: string; percent_remaining?: number | null; resets_at?: string | null;
  /** Spent and allowed this window, in `unit` (Bob's Bobcoins, Kiro's credits). */
  used?: number | null; limit?: number | null; unit?: string | null;
  /** The part of `used` past `limit`, billed as overage. */
  overage?: number | null;
}
export interface AccountQuota {
  state: QuotaState;
  /** What limits the account now, 0 to 100. */
  remaining_percent?: number | null;
  plan?: string | null; windows: QuotaWindow[]; detail?: string | null; checked_at?: string | null;
  /** The reading needs a one-time macOS Keychain grant for this login; see `keychainGrant`. */
  keychain_grant?: boolean;
  /** A multi-provider login (Pi, OpenCode): one reading per provider it is signed in to. */
  providers?: ProviderQuota[];
}
/** One provider login's reading within a multi-provider account. */
export interface ProviderQuota {
  provider: string; label: string; state: QuotaState;
  remaining_percent?: number | null; plan?: string | null; windows: QuotaWindow[]; detail?: string | null;
}
export interface Account {
  /** `acc_...`, or `default-<harness>` for a harness's default account. */
  id: string; harness: string; label: string; config_dir?: string | null;
  /** Signs in with an API key kept in the Keychain (an added key account, or a default one given a key); the key is never returned. */
  api_key?: boolean;
  /** A key account's team. */
  team_id?: string | null;
  /** The harness's usual login; listed but cannot be removed. */
  default: boolean;
  pools: string[];
  /** Credential health from the harness adapter. */
  health: HarnessAuth;
  quota: AccountQuota;
  active_tasks: number;
  /** False when the engine cannot start this harness under another account yet. */
  launchable: boolean;
  created_at?: string | null;
  /** False once disabled for Quark: nothing new starts under it, in any project. */
  enabled?: boolean;
  /** `draining`: disabled, with work that started before still running under it. */
  availability?: AccountAvailability;
  /** Removed once the work running under it finishes. */
  removing?: boolean;
}
export type AccountAvailability = "enabled" | "draining" | "disabled";
export type AccountChangeAction = "disable" | "remove";
/** One task or coordinator running under an account. */
export interface AccountWork { project_id: string; project_name: string; task_id?: string | null; title: string }
export type AccountPlace = "rule" | "default" | "coordinator";
/** What to do with a profile left without an enabled account. */
export type AccountDisposition =
  | { kind: "use_pool"; pool: string; accounts?: string[] }
  | { kind: "use_default" }
  | { kind: "drop" }
  | { kind: "keep" };
/** A dispatch profile or coordinator agent that can run under the account. */
export interface AccountReference {
  id: string; project_id: string; project_name: string; place: AccountPlace;
  /** The rule's name, or "rule N". */
  rule?: string | null;
  harness: string; model?: string | null; effort?: string | null; pool?: string | null;
  /** Labels of the enabled accounts it keeps. */
  remaining: string[];
  /** No enabled account is left for it: one of `options` must be chosen. */
  stranded: boolean;
  /** Quark's proposal first. */
  options: AccountDisposition[];
}
export interface AccountChangePlan {
  account_id: string; action: AccountChangeAction;
  running: AccountWork[]; references: AccountReference[];
  /** Why the change cannot be made at all. */
  refused?: string | null;
}
export interface AccountChangeApplied { project_id: string; project_name: string; commit?: string | null; coordinator: boolean }
export interface AccountChangeOutcome { account?: Account | null; removed: boolean; changed: AccountChangeApplied[] }
/** A config directory, or for a key harness an API key (kept in the Keychain) and its team. */
export interface CreateAccount {
  harness: string; label?: string | null; config_dir?: string; api_key?: string | null; team_id?: string | null; pools?: string[];
}
export interface UpdateAccount { label?: string | null; pools?: string[] | null }
/** Payload of `account.quota_changed`. */
export interface AccountQuotaChanged { account_id: string; harness: string; quota: AccountQuota }

export const accountsApi = {
  /** Every account per harness; `refresh` reads each account's quota now. */
  accounts: (refresh = false) => req<Account[]>("GET", "/v1/accounts" + (refresh ? "?refresh=true" : "")),
  /** Its quota arrives shortly after as `account.quota_changed`. */
  addAccount: (a: CreateAccount) => req<Account>("POST", "/v1/accounts", a),
  updateAccount: (id: string, u: UpdateAccount) => req<Account>("PATCH", `/v1/accounts/${enc(id)}`, u),
  /** 409 for a default account or one a running task uses. */
  removeAccount: (id: string) => req<void>("DELETE", `/v1/accounts/${enc(id)}`),
  /** What disabling or removing touches, and Quark's proposal for each profile left without an account. Changes nothing. */
  planAccountChange: (id: string, action: AccountChangeAction) => req<AccountChangePlan>("POST", `/v1/accounts/${enc(id)}/plan`, { action }),
  /** Applies the chosen dispositions, then disables (or removes, once its work finishes) the account. */
  disableAccount: (id: string, dispositions: Record<string, AccountDisposition>) =>
    req<AccountChangeOutcome>("POST", `/v1/accounts/${enc(id)}/disable`, { dispositions }),
  removeAccountWhenIdle: (id: string, dispositions: Record<string, AccountDisposition>) =>
    req<AccountChangeOutcome>("POST", `/v1/accounts/${enc(id)}/remove`, { dispositions }),
  enableAccount: (id: string) => req<Account>("POST", `/v1/accounts/${enc(id)}/enable`),
  /** Reads quota letting macOS prompt for the login's Keychain item; answers once the prompt is. */
  keychainGrant: (id: string) => req<Account>("POST", `/v1/accounts/${enc(id)}/keychain-grant`),
  /** Keeps an API key in the Keychain for a default account to sign in with, wherever quarkd runs. */
  setAccountKey: (id: string, apiKey: string) => req<Account>("PUT", `/v1/accounts/${enc(id)}/key`, { api_key: apiKey }),
  /** Forgets a default account's kept key; it signs in from quarkd's environment again. */
  clearAccountKey: (id: string) => req<Account>("DELETE", `/v1/accounts/${enc(id)}/key`),
};
