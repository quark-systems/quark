import { describe, expect, it } from "vitest";
import { pickedOf } from "./ProjectRepos";

describe("pickedOf", () => {
  it("shows GitHub repos by owner/name, paths as local folders and the rest as clone URLs", () => {
    expect(pickedOf({ url: "git@github.com:quark-systems/quark.git" })).toEqual({ url: "git@github.com:quark-systems/quark.git", label: "quark-systems/quark", kind: "github" });
    expect(pickedOf({ url: "https://github.com/acme/app" }).label).toBe("acme/app");
    expect(pickedOf({ url: "/Users/matt/notes" }).kind).toBe("folder");
    expect(pickedOf({ url: "~/src/x" }).kind).toBe("folder");
    expect(pickedOf({ url: "https://gitlab.com/acme/tools.git" }).kind).toBe("url");
  });
});
