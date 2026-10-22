import { describe, expect, it } from "vitest";
import { commandLabel, commandNoteTitle, fromSlash, parseCommandNote, slashMatches } from "./commands";
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

describe("command notes", () => {
  it("reads back what a catch up passed on", () => {
    const note = [
      "The user asked to catch up (with a button or a slash command in the app). Their last message to you was at 2026-10-21T10:00:00Z. In a few lines…",
      "",
      "  Since then, newest first (7 events):",
      "  - 2026-10-21T11:00:00Z finished: Login fix: Merged",
      "  ",
      "  Open decisions, oldest first:",
      "  - D-3 (dec_3): Which database?",
      "  - D-4 (dec_4): Ship it?",
    ].join("\n");
    expect(parseCommandNote(note)).toEqual({ command: "catch_up", asked: "you", since: "2026-10-21T10:00:00Z", events: 7, decisions: 2 });
  });

  it("reads a status report, a wrap up and an automatic one", () => {
    const status = "The user asked for a status report (…).\n\n  Tasks, open first, then newest first:\n  - Login: working since x\n  - Docs: done since y\n  \n  No decisions are open.";
    expect(parseCommandNote(status)).toEqual({ command: "status_report", asked: "you", tasks: 2, decisions: 0 });
    expect(parseCommandNote("The user asked you to wrap up (…). Make sure…")).toEqual({ command: "wrap_up", asked: "you" });
    expect(parseCommandNote("Your context holds about 151k tokens and will be compacted, so wrap up now.")).toEqual({ command: "wrap_up", asked: "engine", contextK: 151 });
    expect(parseCommandNote("Project memory: x")).toBeNull();
  });
});
