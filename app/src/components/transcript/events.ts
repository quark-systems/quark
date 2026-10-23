// Messages the engine, not a person, put into an agent's session: watcher wakes
// delivered through Claude Code's Stop hook, background-task notices, and
// firstmate's marked operational inputs. The transcript shows these as one quiet
// row in plain words instead of a chat bubble of raw XML. Firstmate's own
// commands get readable titles too, and the ones that only wait for events
// mark the agent as listening rather than working.
import type { Step } from "./turns";
import { CommandNote, commandNoteTitle, parseCommandNote } from "../../commands";

export type EventKind = "wake" | "background" | "engine" | "warning" | "command";

/** One thing a wake asks the coordinator to judge. */
export interface WakeItem {
  /** Why it woke the coordinator, as the engine names it: `merged`, `done`, `decision`. */
  need: string;
  /** The need in the app's words. */
  label: string;
  /** How the row reads at a glance. */
  tone: "busy" | "needs-you" | "ready" | "failed" | "idle";
  /** The worker it is about, by the engine's name for it. */
  worker?: string;
  text: string;
}

/** An engine message, said in plain words; `raw` is what the session recorded. */
export interface EngineEvent {
  kind: EventKind;
  title: string;
  raw: string;
  /** What a native wake lists, one row each. */
  items?: WakeItem[];
  /** A catch up, status report or wrap up you asked for (or the engine did). */
  command?: CommandNote;
}

/** firstmate's operational-input prefix: U+2063 then `FIRSTMATE_OP: ` (bin/fm-operational-input.sh). */
const OP = /^\u2063?FIRSTMATE_OP: (?:v1 )?([\w-]+): ?/;

const OP_TITLES: Record<string, string> = {
  "session-start": "Session started",
  watcher: "Project event",
  "turn-end-guard": "Checked monitoring before pausing",
  "away-supervisor": "Update while you were away",
  "branch-outcome": "Supervision update",
};

/** The engine event a user entry carries, or null when a person wrote it. */
export function engineEvent(text: string): EngineEvent | null {
  const raw = text;
  const t = text.trim();
  const op = OP.exec(t);
  // A task's instructions are meant to be read, so they stay a message.
  if (op && op[1] !== "launch-brief") return { kind: "engine", title: OP_TITLES[op[1]] ?? "Engine message", raw };

  // `questions::NUDGE` in quark-coordinator, sent back by quarkd's Stop hook when a
  // reply asked something without filing it as a decision.
  if (/(^|\n|: )Quark decision check: /.test(t)) return { kind: "engine", title: "Reminded to ask with decisions", raw };

  const native = nativeWake(t);
  if (native) return { ...native, raw };
  // `ENGINE_NOTE` in quarkd's chat.rs: the engine telling the coordinator something outside a wake.
  if (t.startsWith("Engine note: ")) {
    const what = t.slice("Engine note: ".length);
    const command = parseCommandNote(what);
    if (command) return { kind: "command", title: commandNoteTitle(what) ?? "Command", raw, command };
    const title = (/^Project memory:/.test(what) ? "Project memory updated"
      : /^Memory shared by every Project:/.test(what) ? "Shared memory updated"
      : /^the user set this Project's persona to /.test(what) ? "Persona changed" : "Engine note");
    return { kind: "engine", title, raw };
  }

  const reminder = tagBody(t, "system-reminder");
  const notification = tagBody(t, "task-notification");
  if (notification === null && reminder === null) {
    if (/^<(local-command-stdout|local-command-stderr|command-name|command-message|bash-stdout|bash-stderr)>/.test(t)) {
      return { kind: "engine", title: "Local command output", raw };
    }
    return null;
  }
  // Text outside the engine's tags was typed by a person: keep it a prompt.
  const rest = t.replace(/<(task-notification|system-reminder)>[\s\S]*?<\/\1>/g, "").trim();
  if (rest) return null;

  const body = reminder ?? "";
  const wake = watcherWake(body);
  if (wake) return { ...wake, raw };
  if (notification !== null) {
    const summary = tagBody(notification, "summary")?.trim();
    const status = tagBody(notification, "status")?.trim();
    if (summary && summary !== "Stop hook feedback") {
      return { kind: "background", title: `Background task: ${summary}${status ? ` (${status})` : ""}`, raw };
    }
    return { kind: "background", title: "Background task finished", raw };
  }
  return { kind: "engine", title: "Engine note", raw };
}

/** The needs a native wake names (`Need::as_str` in quark-coordinator), in the app's words. */
const NEEDS: Record<string, [label: string, tone: WakeItem["tone"]]> = {
  decision: ["Question", "needs-you"],
  blocked: ["Blocked", "needs-you"],
  failed: ["Failed", "failed"],
  done: ["Finished", "ready"],
  merged: ["Merged", "ready"],
  message: ["Message", "busy"],
  answered: ["You answered", "busy"],
  requested: ["Requested", "busy"],
  "rule failed": ["Rule failed", "failed"],
  "pick an agent": ["Pick an agent", "needs-you"],
  "spawn failed": ["Could not start", "failed"],
  "host unhealthy": ["Host unhealthy", "failed"],
  policy: ["Update", "idle"],
};

/** What the engine repeats after a worker's name, which the label already says. */
const ECHO = /^(merged|done|asks|pick an agent|could not start|blocked|failed):\s*/;

/** A native engine wake (`Turn::briefing` in quark-coordinator): a count line, then `- [need] summary` rows. */
function nativeWake(t: string): Omit<EngineEvent, "raw"> | null {
  if (!/^Wake \S+: \d+ items? needs? your judgment\./.test(t)) return null;
  // Items start at the margin; indented lines continue the item above (`inset` in quark-coordinator).
  const rows: [need: string, summary: string][] = [];
  for (const l of t.split("\n").slice(1)) {
    const m = /^- \[([^\]]+)\] (.*)$/.exec(l);
    if (m) rows.push([m[1], m[2]]);
    else if (rows.length && l.trim()) rows[rows.length - 1][1] += "\n" + l.trim();
  }
  const items = rows.map(([need, summary]) => wakeItem(need, summary));
  if (!items.length) return { kind: "wake", title: "Project event" };
  const [one] = items;
  const title = items.length === 1
    ? `${one.label}: ${one.worker ?? clip(one.text, 80)}`
    : `${items.length} things for the coordinator`;
  return { kind: "wake", title, items };
}

function wakeItem(need: string, summary: string): WakeItem {
  const [label, tone] = NEEDS[need] ?? [need[0].toUpperCase() + need.slice(1), "idle"];
  const m = /^([a-z0-9][a-z0-9-]*): ([\s\S]*)$/.exec(summary);
  const text = (m ? m[2] : summary).replace(ECHO, "").replace(/ Start any work that was waiting on it\.$/, "").replace(/\.$/, "");
  return { need, label, tone, worker: m?.[1], text };
}

const clip = (s: string, n: number) => (s.length > n ? s.slice(0, n - 1) + "…" : s);

/** A firstmate watcher wake, from the Stop hook's message (bin/fm-claude-stop-autoarm.sh). */
function watcherWake(body: string): Omit<EngineEvent, "raw"> | null {
  if (/watcher auto-arm FAILED/i.test(body)) return { kind: "warning", title: "Project monitoring could not restart" };
  if (!/firstmate watcher wake/i.test(body)) return null;
  const line = body.split("\n").map((l) => l.trim()).find((l) => /^(signal|stale|check|heartbeat)\b/.test(l));
  return { kind: "wake", title: line ? wakeTitle(line) : "Project event" };
}

/** "stale: quark:fm-q-task" -> "fm-q-task stopped responding". */
export function wakeTitle(line: string): string {
  const m = /^(\w+):?\s*(.*)$/.exec(line.trim());
  const reason = m?.[1] ?? "";
  const what = (m?.[2] ?? "").trim();
  const task = what.replace(/^[\w.-]+:(?=\S)/, "").split(/\s+/)[0];
  switch (reason) {
    case "signal": return task ? `Update from ${task}` : "Update from a worker";
    case "stale": return task ? `${task} stopped responding` : "A worker stopped responding";
    case "check": return what ? `Check: ${what.length > 80 ? what.slice(0, 79) + "…" : what}` : "A scheduled check came back";
    case "heartbeat": return "Routine project check";
    default: return "Project event";
  }
}

function tagBody(text: string, tag: string): string | null {
  const m = new RegExp(`<${tag}>([\\s\\S]*?)</${tag}>`).exec(text);
  return m ? m[1] : null;
}

/** Firstmate commands with a plain title. The watcher ones only wait for events. */
const COMMANDS: [RegExp, string, boolean?][] = [
  [/fm-watch(-arm|-checkpoint)?\.sh\b|fm-supervise-daemon\.sh\b/, "Listening for project events", true],
  [/fm-wake-drain\.sh\b[^\n;&|]*--ack-through/, "Marked project events handled"],
  [/fm-wake-drain\.sh\b/, "Read new project events"],
  [/fm-session-start\.sh\b/, "Started the session"],
  [/fm-crew-state\.sh\b/, "Checked a worker's state"],
  [/fm-pr-check\.sh\b/, "Started watching a PR"],
];

function command(step: Step): string | null {
  const call = step.call;
  if (!call || (call.tool && call.tool.kind !== "shell")) return null;
  return call.tool?.command ?? call.text ?? null;
}

/** A plain title for a firstmate command, or null for any other step. */
export function engineCommandTitle(step: Step): string | null {
  const c = command(step);
  if (!c) return null;
  return COMMANDS.find(([re]) => re.test(c))?.[1] ?? null;
}

/** The step only waits for project events: the agent is listening, not working. */
export function isListening(step: Step): boolean {
  const c = command(step);
  return !!c && COMMANDS.some(([re, , waits]) => waits && re.test(c));
}
