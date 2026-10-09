import { describe, expect, it } from "vitest";
import type { ForgeRepository } from "../api";
import { matchRepos, typedRepo } from "./RepoPicker";

const repo = (full_name: string, extra: Partial<ForgeRepository> = {}): ForgeRepository => ({
  full_name, private: false, archived: false, description: null, pushed_at: null,
  ssh_url: `git@github.com:${full_name}.git`, clone_url: `https://github.com/${full_name}.git`, ...extra,
});

describe("typedRepo", () => {
  it("takes clone URLs, owner/name as GitHub over SSH, and paths", () => {
    expect(typedRepo(" https://gitlab.com/a/b.git ")).toEqual({ url: "https://gitlab.com/a/b.git", label: "https://gitlab.com/a/b.git", kind: "url" });
    expect(typedRepo("git@github.com:a/b.git")?.kind).toBe("url");
    expect(typedRepo("quark-systems/quark")).toEqual({ url: "git@github.com:quark-systems/quark.git", label: "quark-systems/quark", kind: "github" });
    expect(typedRepo("/srv/repo")?.kind).toBe("folder");
    expect(typedRepo("~/code/repo")?.kind).toBe("folder");
  });
  it("refuses anything else", () => {
    for (const r of ["quark", "a b/c", "a/b/c", ""]) expect(typedRepo(r)).toBeNull();
  });
});

describe("matchRepos", () => {
  const repos = [
    repo("quark-systems/old", { archived: true, pushed_at: "2026-10-09T00:00:00Z" }),
    repo("quark-systems/quark", { pushed_at: "2026-10-08T00:00:00Z", description: "Agent workspace" }),
    repo("mattsanchez/dotfiles", { pushed_at: "2026-10-09T00:00:00Z" }),
  ];
  it("matches every word in the name or description, archived last, newest first", () => {
    expect(matchRepos(repos, "").map((r) => r.full_name)).toEqual(["mattsanchez/dotfiles", "quark-systems/quark", "quark-systems/old"]);
    expect(matchRepos(repos, "QUARK").map((r) => r.full_name)).toEqual(["quark-systems/quark", "quark-systems/old"]);
    expect(matchRepos(repos, "agent quark").map((r) => r.full_name)).toEqual(["quark-systems/quark"]);
    expect(matchRepos(repos, "nothing")).toEqual([]);
    expect(matchRepos(repos, "", 1)).toHaveLength(1);
  });
});
