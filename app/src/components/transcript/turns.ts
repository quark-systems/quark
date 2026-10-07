// Groups transcript entries into turns for the chat and worker transcript views:
// a prompt, then the agent's work (thinking and tool calls, each paired with its
// result), then what it wrote back. The turn layout follows MonoCode's agent
// transcript (https://github.com/hardbeat920/monocode, MIT).
import type { ToolKind, TranscriptItem } from "../../api";

export type StepStatus = "running" | "ok" | "error";

/** One unit of work: a tool call with its result, or a stretch of thinking. */
export interface Step {
  key: number;
  call?: TranscriptItem;
  result?: TranscriptItem;
  thinking?: TranscriptItem;
  status: StepStatus;
}

export type TurnPart =
  | { kind: "text"; item: TranscriptItem }
  | { kind: "work"; key: number; steps: Step[] };

type WorkPart = Extract<TurnPart, { kind: "work" }>;

export interface Turn {
  key: number;
  /** The user entry that opened the turn; absent for entries before the first prompt. */
  prompt?: TranscriptItem;
  parts: TurnPart[];
  /** Epoch ms of the first and last entry, when the harness recorded times. */
  startedAt?: number;
  endedAt?: number;
  /** The agent is still working: the last turn, and it hasn't written its answer yet. */
  live: boolean;
}

const ms = (ts?: string | null) => {
  const t = ts ? Date.parse(ts) : NaN;
  return Number.isNaN(t) ? undefined : t;
};

export function groupTurns(items: TranscriptItem[]): Turn[] {
  const turns: Turn[] = [];
  let cur: Turn | undefined;
  const open = (prompt?: TranscriptItem, key = prompt?.id ?? 0) => {
    cur = { key, prompt, parts: [], live: false };
    turns.push(cur);
    return cur;
  };

  for (const item of items) {
    if (item.role === "user") {
      open(item);
    } else {
      const t = cur ?? open(undefined, item.id);
      if (item.role === "assistant") t.parts.push({ kind: "text", item });
      else if (item.role === "thinking") work(t, item.id).steps.push({ key: item.id, thinking: item, status: "ok" });
      else if (item.role === "tool_call") work(t, item.id).steps.push({ key: item.id, call: item, status: "running" });
      else attachResult(t, item);
    }
    const at = ms(item.ts);
    if (at !== undefined && cur) {
      cur.startedAt ??= at;
      cur.endedAt = at;
    }
  }
  const last = turns[turns.length - 1];
  if (last) {
    const end = last.parts[last.parts.length - 1];
    last.live = end?.kind === "work" || (end === undefined && !!last.prompt);
  }
  return turns;
}

/** The turn's trailing work part, opened if the turn last ended in text. */
function work(t: Turn, key: number): WorkPart {
  const last = t.parts[t.parts.length - 1];
  if (last?.kind === "work") return last;
  const w: WorkPart = { kind: "work", key, steps: [] };
  t.parts.push(w);
  return w;
}

/** Pairs a tool result with its call: by call id, else the latest call still waiting. */
function attachResult(t: Turn, result: TranscriptItem) {
  const steps = t.parts.flatMap((p) => (p.kind === "work" ? p.steps : []));
  const waiting = steps.filter((s) => s.call && !s.result);
  const step =
    (result.tool_call_id && waiting.find((s) => s.call!.tool_call_id === result.tool_call_id)) ||
    (!result.tool_call_id ? waiting[waiting.length - 1] : undefined);
  const status: StepStatus = result.is_error ? "error" : "ok";
  if (step) {
    step.result = result;
    step.status = status;
  } else {
    // A result whose call is outside the loaded history.
    work(t, result.id).steps.push({ key: result.id, result, status });
  }
}

/** A step's title: the daemon's summary, else the tool name and the first line of its input. */
export function stepTitle(s: Step): string {
  if (s.thinking) return "Thinking";
  const call = s.call;
  if (call?.tool) return call.tool.title;
  const name = call?.tool_name ?? s.result?.tool_name ?? "tool";
  const first = (call?.text ?? "").split("\n")[0].trim();
  return first ? `${name}: ${first}` : name;
}

export function stepKind(s: Step): ToolKind | "thinking" {
  if (s.thinking) return "thinking";
  return s.call?.tool?.kind ?? (/^(bash|shell)$/i.test(s.call?.tool_name ?? "") ? "shell" : "other");
}

const VERBS: [ToolKind, string, string][] = [
  ["read", "read", "file"],
  ["edit", "edited", "file"],
  ["write", "wrote", "file"],
  ["shell", "ran", "command"],
  ["search", "searched", "time"],
  ["web", "fetched", "page"],
  ["agent", "started", "agent"],
];

/** "Read 3 files, ran 2 commands" for a folded run of steps. */
export function summarize(steps: Step[]): string {
  const counts = new Map<string, number>();
  for (const s of steps) {
    const k = stepKind(s);
    if (k !== "thinking") counts.set(k, (counts.get(k) ?? 0) + 1);
  }
  const parts: string[] = [];
  for (const [kind, verb, noun] of VERBS) {
    const n = counts.get(kind);
    if (!n) continue;
    parts.push(noun === "time" ? `${verb} ${n === 1 ? "once" : `${n} times`}` : `${verb} ${n} ${noun}${n === 1 ? "" : "s"}`);
    counts.delete(kind);
  }
  const rest = [...counts.values()].reduce((a, b) => a + b, 0);
  if (rest) parts.push(`${rest} other ${rest === 1 ? "step" : "steps"}`);
  if (!parts.length) return "Thought";
  const text = parts.join(", ");
  return text[0].toUpperCase() + text.slice(1);
}

/** "2m 39s", "55s", "1h 4m". */
export function duration(msTotal: number): string {
  const s = Math.max(0, Math.round(msTotal / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

/** Idle time after which a turn opens a new stretch, marked with its time of day. */
export const STRETCH_GAP_MS = 60 * 60 * 1000;

/**
 * The separator before a turn starting at `at` when the previous one was at `prev`:
 * the day when it opens a new day, the time after an hour idle, else none.
 */
export function breakLabel(at: number | undefined, prev: number | undefined, now = Date.now()): string | null {
  if (at === undefined || prev === undefined) return null;
  if (newDay(at, prev)) return dayLabel(at, now);
  if (at - prev >= STRETCH_GAP_MS) return new Date(at).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  return null;
}

/** A finished turn as Markdown, for copying: the prompt quoted, then the agent's answer. */
export function turnMarkdown(t: Turn): string {
  const out: string[] = [];
  if (t.prompt) out.push(t.prompt.text.split("\n").map((l) => `> ${l}`).join("\n"));
  for (const p of t.parts) if (p.kind === "text") out.push(p.item.text);
  return out.join("\n\n");
}

/** Whether a turn starting at `at` opens a new day after one at `prev`. */
export function newDay(at: number | undefined, prev: number | undefined): boolean {
  if (at === undefined) return false;
  if (prev === undefined) return true;
  return new Date(at).toDateString() !== new Date(prev).toDateString();
}

export function dayLabel(at: number, now = Date.now()): string {
  const d = new Date(at);
  const today = new Date(now);
  const yesterday = new Date(now - 86_400_000);
  if (d.toDateString() === today.toDateString()) return "Today";
  if (d.toDateString() === yesterday.toDateString()) return "Yesterday";
  return d.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" });
}
