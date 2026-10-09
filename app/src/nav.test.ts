import { describe, expect, it } from "vitest";
import { href, parseRoute } from "./nav";

describe("routes", () => {
  it("round-trips every route", () => {
    for (const r of [{ name: "projects" }, { name: "new" }, { name: "project", id: "a b/c" }, { name: "task", id: "t-1" }, { name: "inbox" }, { name: "inbox", id: "d 1" }, { name: "memory", project: "a b" }, { name: "memory", project: "p", id: "2026-10-01-x" }, { name: "issues", project: "a b" }, { name: "issues", project: "p", id: "qk-44" }, { name: "dispatch", project: "a b" }, { name: "settings", project: "a b" }, { name: "automation", project: "a b" }, { name: "prs" }, { name: "pr", id: "quark-systems/quark#12" }, { name: "accounts" }] as const) {
      expect(parseRoute(href(r))).toEqual(r);
    }
  });
  it("falls back to the Projects list", () => {
    expect(parseRoute("")).toEqual({ name: "projects" });
    expect(parseRoute("#/nope")).toEqual({ name: "projects" });
    expect(parseRoute("#/p/")).toEqual({ name: "projects" });
  });
  it("ignores extra trailing segments", () => {
    expect(parseRoute("#/p/x/nope")).toEqual({ name: "project", id: "x" });
    expect(parseRoute("#/t/t-1/more")).toEqual({ name: "task", id: "t-1" });
  });
});

