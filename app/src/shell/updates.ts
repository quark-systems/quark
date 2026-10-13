// How one "now" update reads in Since you looked: which worker, what happened, and the detail.
import type { AwayOccasion, Notice } from "../api";
import type { Tone } from "../ui/tone";

export interface UpdateView {
  /** The worker it is about, readable: its title, else its name with dashes as spaces. */
  who?: string;
  /** What happened, in a few words. */
  label: string;
  tone: Tone;
  /** The rest of the line, without the worker's name or the status word. */
  detail: string;
}

const WORDS: Record<AwayOccasion, { label: string; tone: Tone }> = {
  progress: { label: "Progress", tone: "busy" },
  paused: { label: "Waiting", tone: "parked" },
  done: { label: "Finished", tone: "ready" },
  decision: { label: "Needs your decision", tone: "needs-you" },
  blocked: { label: "Blocked", tone: "needs-you" },
  failed: { label: "Failed", tone: "failed" },
  stale: { label: "Stopped responding", tone: "failed" },
  inbound: { label: "Message", tone: "idle" },
  trigger_fired: { label: "Rule ran", tone: "idle" },
  trigger_failed: { label: "Rule failed", tone: "failed" },
};

// The status words the engine puts in front of a worker's line (crates/quark-triggers occasion()).
const VERB = /^(done|needs-decision|blocked|failed|paused|working|resolved|captain-held|asks)\b:?\s*/;
const VERB_OCCASION: Record<string, AwayOccasion> = {
  done: "done", "needs-decision": "decision", asks: "decision", blocked: "blocked", failed: "failed", paused: "paused",
};

/** A worker's name as words: `m6-self-host-install` -> `M6 self host install`. */
export function humanize(name: string): string {
  const words = name.replace(/[-_]+/g, " ").trim();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** Older updates carry no occasion: read it from the line itself. */
function inferOccasion(line: string): AwayOccasion {
  const verb = VERB.exec(line)?.[1];
  if (verb && VERB_OCCASION[verb]) return VERB_OCCASION[verb];
  if (line.startsWith("no activity for a while")) return "stale";
  if (line.startsWith("rule ")) return "trigger_fired";
  return "progress";
}

export function describeUpdate(n: Notice, title?: string): UpdateView {
  let line = n.text.trim();
  if (n.task_id && line.startsWith(`${n.task_id}: `)) line = line.slice(n.task_id.length + 2);
  const occasion = n.occasion ?? inferOccasion(line);
  const detail = line.replace(VERB, "").replace(/^no activity for a while$/, "");
  const who = title?.trim() || (n.task_id ? humanize(n.task_id) : undefined);
  return { who, ...WORDS[occasion], detail };
}
