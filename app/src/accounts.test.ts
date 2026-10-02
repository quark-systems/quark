import { describe, expect, it } from "vitest";
import { groupByHarness, loginHint, parsePools, poolsFor, POOL_RE, quotaCls, quotaText } from "./accounts";
import type { Account, AccountQuota, HarnessInfo } from "./api";

const quota = (over: Partial<AccountQuota> = {}): AccountQuota => ({ state: "known", remaining_percent: 62.4, windows: [], ...over });
const account = (id: string, harness: string, pools: string[] = []): Account => ({
  id, harness, label: id, config_dir: `/a/${id}`, default: id.startsWith("default-"), pools,
  health: { state: "configured" }, quota: quota(), active_tasks: 0, launchable: true,
});
const harness = (id: string, name: string, account_env: string | null): HarnessInfo => ({
  id, name, roles: ["worker"], install: { installed: true, install_hint: "" }, models: { selection: "free_form" },
  efforts: [], auth: { state: "configured" }, transcript: true, account_env,
});

describe("quota labels", () => {
  it("says how much is left, or why there is no number", () => {
    expect(quotaText(quota())).toBe("62% left");
    expect(quotaText(quota({ state: "pending", remaining_percent: null }))).toBe("Reading quota…");
    expect(quotaText(quota({ state: "error", remaining_percent: null }))).toBe("Quota read failed");
    expect(quotaText(quota({ state: "unsupported", remaining_percent: null }))).toBe("Quota not tracked");
  });
  it("colours plenty, low and out", () => {
    expect(quotaCls(quota())).toBe("green");
    expect(quotaCls(quota({ remaining_percent: 10 }))).toBe("yellow");
    expect(quotaCls(quota({ remaining_percent: 0 }))).toBe("red");
    expect(quotaCls(quota({ state: "pending", remaining_percent: null }))).toBe("");
  });
});

describe("accounts", () => {
  it("groups by harness in the daemon's order with display names", () => {
    const g = groupByHarness(
      [account("default-claude-code", "claude-code"), account("default-codex", "codex"), account("acc_2", "claude-code")],
      [harness("claude-code", "Claude Code", "CLAUDE_CONFIG_DIR")],
    );
    expect(g.map((x) => [x.name, x.accounts.map((a) => a.id)])).toEqual([
      ["Claude Code", ["default-claude-code", "acc_2"]],
      ["codex", ["default-codex"]],
    ]);
  });
  it("parses and lists pools", () => {
    expect(parsePools(" Max, batch max ")).toEqual(["max", "batch"]);
    expect(POOL_RE.test("max-2")).toBe(true);
    expect(POOL_RE.test("-max")).toBe(false);
    expect(poolsFor([account("a", "claude-code", ["max"]), account("b", "claude-code", ["batch", "max"]), account("c", "codex", ["x"])], "claude-code"))
      .toEqual(["batch", "max"]);
  });
  it("shows how to log in under a new directory", () => {
    expect(loginHint(harness("claude-code", "Claude Code", "CLAUDE_CONFIG_DIR"), "/a/work")).toBe("CLAUDE_CONFIG_DIR=/a/work claude");
    expect(loginHint(harness("codex", "Codex", "CODEX_HOME"), "")).toBe("CODEX_HOME=<dir> codex");
    expect(loginHint(harness("gemini", "Gemini CLI", null), "/x")).toBeNull();
  });
});
