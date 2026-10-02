// Accounts screen helpers (quark#23): health and quota labels, grouping and pool parsing.
import type { Account, AccountQuota, AuthState, HarnessInfo } from "./api";

export const healthMeta: Record<AuthState, { label: string; cls: string }> = {
  configured: { label: "Logged in", cls: "green" },
  not_configured: { label: "Not logged in", cls: "red" },
  unknown: { label: "Login unknown", cls: "yellow" },
};

/** One short line for an account's quota, e.g. "62% left". */
export function quotaText(q: AccountQuota): string {
  switch (q.state) {
    case "known": return q.remaining_percent != null ? `${Math.round(q.remaining_percent)}% left` : "Known";
    case "pending": return "Reading quota…";
    case "unavailable": return "No quota reading";
    case "error": return "Quota read failed";
    case "unsupported": return "Quota not tracked";
  }
}

/** Pill colour for the quota: plenty, getting low, out, or no reading. */
export function quotaCls(q: AccountQuota): string {
  if (q.state === "error") return "red";
  if (q.state !== "known" || q.remaining_percent == null) return "";
  if (q.remaining_percent <= 0) return "red";
  return q.remaining_percent < 20 ? "yellow" : "green";
}

/** Accounts grouped by harness, in the daemon's order, with the harness's display name. */
export function groupByHarness(accounts: Account[], harnesses: HarnessInfo[]): { harness: string; name: string; accounts: Account[] }[] {
  const groups = new Map<string, Account[]>();
  for (const a of accounts) {
    const g = groups.get(a.harness);
    if (g) g.push(a); else groups.set(a.harness, [a]);
  }
  return [...groups].map(([harness, list]) => ({
    harness, name: harnesses.find((h) => h.id === harness)?.name ?? harness, accounts: list,
  }));
}

/** Pools typed as "max, batch": trimmed, lowercased, unique. */
export function parsePools(s: string): string[] {
  const out: string[] = [];
  for (const p of s.split(/[,\s]+/)) {
    const t = p.trim().toLowerCase();
    if (t && !out.includes(t)) out.push(t);
  }
  return out;
}

export const POOL_RE = /^[a-z0-9][a-z0-9-]{0,63}$/;

/** The pools named by a harness's accounts, sorted. */
export function poolsFor(accounts: Account[], harness: string): string[] {
  return [...new Set(accounts.filter((a) => a.harness === harness).flatMap((a) => a.pools))].sort();
}

/** The shell line that logs a harness in under a new config directory. */
export function loginHint(h: HarnessInfo | undefined, dir: string): string | null {
  if (!h?.account_env) return null;
  const bin = h.id === "claude-code" ? "claude" : h.id;
  return `${h.account_env}=${dir || "<dir>"} ${bin}`;
}
