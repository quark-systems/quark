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
  if (note.startsWith("The user asked to catch up")) return "You asked to catch up";
  if (note.startsWith("The user asked for a status report")) return "You asked for a status report";
  if (note.startsWith("The user asked you to wrap up")) return "You asked to wrap up";
  if (/^Your context holds about \d+k tokens/.test(note)) return "Wrap up before the conversation is compacted";
  return null;
}
