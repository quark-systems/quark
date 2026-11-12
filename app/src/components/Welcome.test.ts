import { describe, expect, it } from "vitest";
import { repoName, suggestions } from "./Welcome";

describe("welcome", () => {
  it("names a repository by its name or the end of its URL", () => {
    expect(repoName({ url: "git@github.com:quark-systems/quark.git" })).toBe("quark");
    expect(repoName({ url: "https://github.com/a/b/", name: null })).toBe("b");
    expect(repoName({ url: "x", name: "named" })).toBe("named");
  });

  it("offers a plan only with a goal, and issue picking only with open issues", () => {
    expect(suggestions({ goal: null }, 0).map((s) => s.label)).toEqual(["Get your bearings"]);
    const all = suggestions({ goal: "Ship it" }, 2);
    expect(all.map((s) => s.label)).toEqual(["Get your bearings", "Plan the goal", "Pick a first issue"]);
    expect(all[1].text).toContain("Ship it");
  });
});
