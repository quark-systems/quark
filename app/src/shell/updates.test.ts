import { describe, expect, it } from "vitest";
import { describeUpdate, humanize } from "./updates";

const now = (text: string, extra = {}) => ({ project_id: "p", kind: "now" as const, text, ...extra });

describe("describeUpdate", () => {
  it("names the worker and says what happened in words", () => {
    const n = now("m6-self-host-install: done PR https://github.com/o/r/pull/7 checks green", { task_id: "m6-self-host-install", occasion: "done" });
    expect(describeUpdate(n, "Self-host install and owner accounts")).toEqual({
      who: "Self-host install and owner accounts", label: "Finished", tone: "ready", detail: "PR https://github.com/o/r/pull/7 checks green",
    });
  });

  it("falls back to the worker's name as words when it is gone", () => {
    expect(describeUpdate(now("m4-search: blocked: needs a token", { task_id: "m4-search", occasion: "blocked" })))
      .toMatchObject({ who: "M4 search", label: "Blocked", tone: "needs-you", detail: "needs a token" });
  });

  it("reads the occasion from older updates that carry none", () => {
    expect(describeUpdate(now("t1: needs-decision which db?", { task_id: "t1" }))).toMatchObject({ label: "Needs your decision", detail: "which db?" });
    expect(describeUpdate(now("t1: asks: which db?", { task_id: "t1" }))).toMatchObject({ label: "Needs your decision", detail: "which db?" });
    expect(describeUpdate(now("t1: no activity for a while", { task_id: "t1" }))).toMatchObject({ label: "Stopped responding", detail: "" });
    expect(describeUpdate(now("rule nightly: ran"))).toMatchObject({ who: undefined, label: "Rule ran", detail: "rule nightly: ran" });
  });

  it("humanizes names", () => {
    expect(humanize("store-the-web_s-mid")).toBe("Store the web s mid");
  });
});
