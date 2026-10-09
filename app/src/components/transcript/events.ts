// Messages the engine, not a person, put into an agent's session: watcher wakes
// delivered through Claude Code's Stop hook, background-task notices, and
// firstmate's marked operational inputs. The transcript shows these as one quiet
// row in plain words instead of a chat bubble of raw XML. Firstmate's own
// commands get readable titles too, and the ones that only wait for events
// mark the agent as listening rather than working.
import type { Step } from "./turns";

export type EventKind = "wake" | "background" | "engine" | "warning";

/** An engine message, said in plain words; `raw` is what the session recorded. */
export interface EngineEvent {
  kind: EventKind;
  title: string;
  raw: string;
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
