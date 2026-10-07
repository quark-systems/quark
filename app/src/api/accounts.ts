import { enc, req } from "./client";
import { HarnessAuth } from "./harnesses";

// Accounts and pools per harness, with per-account quota (quark#23, ADR-11).
export type QuotaState = "pending" | "known" | "unavailable" | "error" | "unsupported";
export interface QuotaWindow { id: string; label: string; percent_remaining?: number | null; resets_at?: string | null }
export interface AccountQuota {
  state: QuotaState;
  /** What limits the account now, 0 to 100. */
  remaining_percent?: number | null;
  plan?: string | null; windows: QuotaWindow[]; detail?: string | null; checked_at?: string | null;
}
export interface Account {
  /** `acc_...`, or `default-<harness>` for a harness's default account. */
  id: string; harness: string; label: string; config_dir?: string | null;
  /** The harness's usual config directory; listed but cannot be removed. */
  default: boolean;
  pools: string[];
  /** Credential health from the harness adapter. */
  health: HarnessAuth;
  quota: AccountQuota;
  active_tasks: number;
  /** False when the engine cannot start this harness under another account yet. */
  launchable: boolean;
  created_at?: string | null;
}
export interface CreateAccount { harness: string; label?: string | null; config_dir: string; pools?: string[] }
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
};
