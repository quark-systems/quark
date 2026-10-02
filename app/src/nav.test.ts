import { describe, expect, it } from "vitest";
import { href, parseRoute } from "./nav";
import { validRepo } from "./screens/NewProject";

describe("routes", () => {
  it("round-trips every route", () => {
    for (const r of [{ name: "projects" }, { name: "new" }, { name: "project", id: "a b/c" }, { name: "task", id: "t-1" }] as const) {
      expect(parseRoute(href(r))).toEqual(r);
    }
  });
  it("falls back to the Projects list", () => {
    expect(parseRoute("")).toEqual({ name: "projects" });
    expect(parseRoute("#/nope")).toEqual({ name: "projects" });
  });
});

describe("validRepo", () => {
  it("accepts owner/name and clone URLs", () => {
    for (const r of ["quark-systems/quark", "https://github.com/a/b.git", "git@github.com:a/b.git"]) expect(validRepo(r)).toBe(true);
  });
  it("rejects anything else", () => {
    for (const r of ["quark", "a b/c", "a/b/c"]) expect(validRepo(r)).toBe(false);
  });
});
