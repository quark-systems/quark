// Catch up, Status report and Wrap up: what you ask a coordinator for with one control,
// a button or a slash command typed as the whole message. quarkd turns either into the
// request and the facts the coordinator needs (`coordinator_commands.rs`).
import type { CoordinatorCommand } from "./api";
import type { Labels } from "./persona";

export const COMMANDS: CoordinatorCommand[] = ["catch_up", "status_report", "wrap_up"];

const NEUTRAL: Record<CoordinatorCommand, string> = {
  catch_up: "Catch up", status_report: "Status report", wrap_up: "Wrap up",
};

/** What each command does, for tooltips and the palette. */
export const HINTS: Record<CoordinatorCommand, string> = {
  catch_up: "Recap what happened since your last message, then go through open decisions one at a time",
  status_report: "What needs you, what landed, what is underway and what is next",
  wrap_up: "Save what the conversation learned to memory and file unfinished work as issues",
};

/** The slash commands that run each one, as quarkd accepts them (`CoordinatorCommand::aliases`). */
export const ALIASES: Record<CoordinatorCommand, string[]> = {
  catch_up: ["catchup", "catch-up", "ahoy"],
  status_report: ["status", "status-report", "bearings"],
  wrap_up: ["wrapup", "wrap-up", "stow"],
};

/** A command's name in the Project's persona: the nautical pack says Ahoy, Bearings and Stow. */
export function commandLabel(l: Labels, c: CoordinatorCommand): string {
  return l.ui(`command.${c}`, NEUTRAL[c]);
}

/** The command a message is, when the whole message is one of the slash commands. */
export function fromSlash(text: string): CoordinatorCommand | null {
  const m = /^\/(\S+)$/.exec(text.trim());
  if (!m) return null;
  const word = m[1].toLowerCase();
  return COMMANDS.find((c) => ALIASES[c].includes(word)) ?? null;
}

/** The commands a half-typed slash command could become, with the alias to show for each. */
export function slashMatches(text: string): { command: CoordinatorCommand; alias: string }[] {
  const m = /^\/(\S*)$/.exec(text);
  if (!m) return [];
  const typed = m[1].toLowerCase();
  return COMMANDS.flatMap((c) => {
    const alias = ALIASES[c].find((a) => a.startsWith(typed));
    return alias ? [{ command: c, alias }] : [];
  });
}

/** How the engine's note for a command reads in the conversation, or null when the text is not one. */
export function commandNoteTitle(note: string): string | null {
  const n = parseCommandNote(note);
  if (!n) return null;
  if (n.asked === "engine") return "Wrap up before the conversation is compacted";
  return { catch_up: "You asked to catch up", status_report: "You asked for a status report", wrap_up: "You asked to wrap up" }[n.command];
}

/** What the engine's note for a command says, read back for the conversation's command card. */
export interface CommandNote {
  command: CoordinatorCommand;
  /** You pressed or typed it, or the engine asked because the context filled. */
  asked: "you" | "engine";
  /** Catch up: when you last wrote to the coordinator. */
  since?: string;
  /** Catch up: notable events since then. */
  events?: number;
  /** Status report: tasks in the snapshot. */
  tasks?: number;
  /** Catch up and status report: open decisions. */
  decisions?: number;
  /** An automatic wrap up: the context size, in thousands of tokens. */
  contextK?: number;
}

/** The command an engine note (text after `Engine note: `) asks for, with the facts it carried; null when it is not one. */
export function parseCommandNote(note: string): CommandNote | null {
  const lines = note.split("\n");
  const head = lines[0];
  const quoted = lines.slice(1).map((l) => l.replace(/^ {2}/, ""));
  const decisions = quoted.some((l) => l === "No decisions are open.") ? 0
    : quoted.some((l) => l.startsWith("Open decisions")) ? quoted.filter((l) => /^- D-\d+ /.test(l)).length : undefined;
  if (head.startsWith("The user asked to catch up")) {
    const since = /Their last message to you was at (\S+?)\.? /.exec(head)?.[1];
    const n = quoted.map((l) => /^Since then, newest first \((\d+) events?\)/.exec(l)).find(Boolean);
    const events = n ? Number(n[1]) : quoted.includes("Nothing notable was logged since then.") ? 0 : undefined;
    return { command: "catch_up", asked: "you", since: since && since !== "unknown" ? since : undefined, events, decisions };
  }
  if (head.startsWith("The user asked for a status report")) {
    const tasks = quoted.includes("No tasks ran in the last day.") ? 0
      : quoted.some((l) => l.startsWith("Tasks,")) ? quoted.filter((l) => l.startsWith("- ") && !/^- D-\d+ /.test(l)).length : undefined;
    return { command: "status_report", asked: "you", tasks, decisions };
  }
  if (head.startsWith("The user asked you to wrap up")) return { command: "wrap_up", asked: "you" };
  const full = /^Your context holds about (\d+)k tokens/.exec(head);
  if (full) return { command: "wrap_up", asked: "engine", contextK: Number(full[1]) };
  return null;
}
