import { describe, expect, it } from "vitest";
import { commandLabel, commandNoteTitle, fromSlash, slashMatches } from "./commands";
import { labels } from "./persona";

describe("coordinator commands", () => {
  it("knows a whole-message slash command, firstmate's names included", () => {
    expect(fromSlash(" /Ahoy ")).toBe("catch_up");
    expect(fromSlash("/bearings")).toBe("status_report");
    expect(fromSlash("/wrap-up")).toBe("wrap_up");
    expect(fromSlash("/stow the logs")).toBeNull();
    expect(fromSlash("stow")).toBeNull();
    expect(fromSlash("/compact")).toBeNull();
  });

  it("suggests commands while a slash command is typed", () => {
    expect(slashMatches("/")).toEqual([
      { command: "catch_up", alias: "catchup" },
      { command: "status_report", alias: "status" },
      { command: "wrap_up", alias: "wrapup" },
    ]);
    expect(slashMatches("/st")).toEqual([
      { command: "status_report", alias: "status" },
      { command: "wrap_up", alias: "stow" },
    ]);
    expect(slashMatches("/stow ")).toEqual([]);
    expect(slashMatches("hello")).toEqual([]);
  });

  it("names commands in the Project's persona", () => {
    expect(commandLabel(labels(null), "catch_up")).toBe("Catch up");
    const nautical = labels({
      id: "nautical", name: "Nautical", builtin: true, address: "captain", voice: "", vocabulary: [], roles: {},
      ui_labels: { "command.catch_up": "Ahoy", "command.wrap_up": "Stow" },
    });
    expect(commandLabel(nautical, "catch_up")).toBe("Ahoy");
    expect(commandLabel(nautical, "status_report")).toBe("Status report");
  });

  it("titles the engine's notes for commands", () => {
    expect(commandNoteTitle("The user asked to catch up (with a button…)")).toBe("You asked to catch up");
    expect(commandNoteTitle("Your context holds about 151k tokens and will be compacted")).toMatch(/^Wrap up/);
    expect(commandNoteTitle("Project memory: x")).toBeNull();
  });
});
