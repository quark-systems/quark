// Accounts screen helpers (quark#23): health and quota labels, grouping and pool parsing.
import type { Account, AccountDisposition, AccountQuota, AccountReference, AuthState, HarnessInfo, QuotaWindow } from "./api";

export const healthMeta: Record<AuthState, { label: string; cls: string }> = {
  configured: { label: "Logged in", cls: "green" },
  not_configured: { label: "Not logged in", cls: "red" },
  unknown: { label: "Login unknown", cls: "yellow" },
};

/** One short line for an account's quota, e.g. "62% left". */
type Reading = Pick<AccountQuota, "state" | "remaining_percent" | "providers">;

export function quotaText(q: Reading): string {
  const n = q.providers?.length ?? 0;
  switch (q.state) {
    case "known":
      if (q.remaining_percent != null) return `${Math.round(q.remaining_percent)}% left`;
      return n ? `${n} provider${n === 1 ? "" : "s"}` : "Known";
    case "pending": return "Reading quota…";
    case "unavailable": return "No quota reading";
    case "error": return "Quota read failed";
    case "unsupported": return "Quota not tracked";
  }
}

const amount = (n: number) => n.toLocaleString("en-US", { maximumFractionDigits: 2 });

/** What a window has spent, e.g. "435 of 1,500 Bobcoins used", with any overage; "" when not counted. */
export function usageText(w: QuotaWindow): string {
  if (w.used == null || w.limit == null) return "";
  const unit = w.unit ? ` ${w.unit}` : "";
  const over = w.overage ? `, ${amount(w.overage)} over (overage)` : "";
  return `${amount(w.used)} of ${amount(w.limit)}${unit} used${over}`;
}

/** Pill colour for the quota: plenty, getting low, out, or no reading. */
export function quotaCls(q: Reading): string {
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

/** Harnesses that take accounts beyond the default: config directories or API keys. */
export function addable(harnesses: HarnessInfo[]): HarnessInfo[] {
  return harnesses.filter((h) => h.account_env || h.key_env);
}

/** Where an account's login is, for its row. */
export function whereText(a: Account): string {
  if (a.api_key) return a.team_id ? `API key in the Keychain, team ${a.team_id}` : "API key in the Keychain";
  return a.config_dir ?? (a.default ? "the harness's own login" : "no config directory");
}

/** The shell line that logs a harness in under a new config directory. */
export function loginHint(h: HarnessInfo | undefined, dir: string): string | null {
  if (!h?.account_env) return null;
  const bin = h.id === "claude-code" ? "claude" : h.id;
  return `${h.account_env}=${dir || "<dir>"} ${bin}`;
}

/** Where an account stands with Quark, for its row: null while it is simply enabled. */
export function availabilityMeta(a: Account): { label: string; cls: string; title: string } | null {
  const running = a.active_tasks;
  const work = `${running} ${running === 1 ? "worker or coordinator" : "workers or coordinators"}`;
  if (a.removing) {
    return { label: "Removing", cls: "yellow", title: `Removed once the ${work} still running under it finish. Nothing new starts under it.` };
  }
  switch (a.availability ?? (a.enabled === false ? "disabled" : "enabled")) {
    case "draining":
      return { label: `Draining · ${running} running`, cls: "yellow", title: `Disabled: nothing new starts under it. The ${work} already running finish first.` };
    case "disabled":
      return { label: "Disabled", cls: "", title: "Quark starts nothing under this account, in any project." };
    default:
      return null;
  }
}

/** The key a disposition is chosen by in a Select. */
export function dispositionKey(d: AccountDisposition): string {
  return d.kind === "use_pool" ? `use_pool:${d.pool}` : d.kind;
}

/** One line for a disposition, e.g. "Use pool team (Work, Batch)". */
export function dispositionText(d: AccountDisposition, r: Pick<AccountReference, "place" | "harness">): string {
  switch (d.kind) {
    case "use_pool": return `Use pool ${d.pool}${d.accounts?.length ? ` (${d.accounts.join(", ")})` : ""}`;
    case "use_default": return `Use ${r.harness}'s default account`;
    case "drop": return r.place === "default" ? "Take it out of the default" : "Take it out of the rule";
    case "keep": return r.place === "coordinator"
      ? "Leave it: the coordinator won't start until you change its agent"
      : "Leave it: matching work goes to the coordinator to place";
  }
}

/** Where a reference sits, e.g. "Rule trivial-edit" or "Coordinator". */
export function placeText(r: Pick<AccountReference, "place" | "rule">): string {
  switch (r.place) {
    case "rule": return `Rule ${r.rule ?? ""}`.trim();
    case "default": return "Default rule";
    case "coordinator": return "Coordinator";
  }
}

/** The proposal for every stranded reference, by id: what the plan dialog starts with. */
export function proposedDispositions(refs: AccountReference[]): Record<string, AccountDisposition> {
  const out: Record<string, AccountDisposition> = {};
  for (const r of refs) if (r.stranded && r.options[0]) out[r.id] = r.options[0];
  return out;
}
