// What the coordinator's header says about its agent and how full its context is
// (`GET /v1/coordinators/{id}/context`), as words the header and its popover show.
import type { AgentConfig, CoordinatorContext, CoordinatorSwitch } from "./api";
import { harnessMark } from "./components/WorkerCard/harnessMarks";

/** "46k", "1.2M", "800". */
export function tokens(n: number): string {
  if (n >= 1_000_000) return `${+(n / 1_000_000).toFixed(n % 1_000_000 ? 1 : 0)}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

/** Harnesses that pick the model and effort themselves, with no choosing either (IBM Bob). */
const PICKS_ITS_OWN = new Set(["bob"]);

/** Whether `harness` takes an effort; one that picks its own says nothing about effort. */
export function hasEffort(harness: string | undefined): boolean {
  return !harness || !PICKS_ITS_OWN.has(harness);
}

/** The agent in one line: "Claude Code · opus · high". Blanks say the harness picks; `effort`
 * is null for a harness with no effort to choose. */
export function agentLine(c: CoordinatorContext): { harness: string; model: string; effort: string | null } {
  const a = c.agent_config;
  return {
    harness: a ? harnessMark(a.harness).name : "Unknown harness",
    model: a?.model ?? c.model_seen ?? "default model",
    effort: !hasEffort(a?.harness) ? null : a?.effort ? `${a.effort} effort` : "default effort",
  };
}

/** The agent's parts that apply, joined with `sep`. */
export function agentParts(c: CoordinatorContext, sep: string): string {
  const a = agentLine(c);
  return [a.harness, a.model, a.effort].filter(Boolean).join(sep);
}

/** How full the context is, for the meter's label: "23%", or "46k" when the window is unknown, or "–". */
export function fullness(c: CoordinatorContext): string {
  if (c.context_tokens == null) return "–";
  if (!c.window_tokens) return tokens(c.context_tokens);
  return `${Math.round((c.context_tokens / c.window_tokens) * 100)}%`;
}

/** The context line of the popover: "46k of 200k tokens". */
export function contextLine(c: CoordinatorContext): string {
  if (c.context_tokens == null) {
    return c.last_compaction ? "Not measured since it was compacted" : "Not measured yet";
  }
  return c.window_tokens ? `${tokens(c.context_tokens)} of ${tokens(c.window_tokens)} tokens` : `${tokens(c.context_tokens)} tokens`;
}

/** What happens next about compaction, in a sentence. */
export function compactionLine(c: CoordinatorContext): string {
  if (c.compacting === "wrapping_up") return "Wrapping up, then compacting.";
  if (c.compacting === "compacting") return "Compacting now.";
  const harness = c.agent_config ? harnessMark(c.agent_config.harness).name : "The harness";
  if (!c.compaction.auto) {
    return `Automatic compaction is off. ${harness} compacts on its own when the context is nearly full.`;
  }
  const at = c.compact_at_tokens ? ` (${tokens(c.compact_at_tokens)} tokens)` : "";
  return `Compacts when the context is ${c.compaction.at_percent}% full${at}, after a wrap up.`;
}

/** An agent in words: "Claude Code, opus, high effort". */
export function agentWords(a: AgentConfig | null | undefined): string {
  if (!a) return "no agent";
  if (!hasEffort(a.harness)) {
    return [harnessMark(a.harness).name, ...(a.model ? [a.model] : []), ...(a.pool ? [`pool ${a.pool}`] : [])].join(", ");
  }
  return [harnessMark(a.harness).name, a.model ?? "default model", a.effort ? `${a.effort} effort` : "default effort",
    ...(a.pool ? [`pool ${a.pool}`] : [])].join(", ");
}

/** Whether a change of agent still has steps left. */
export function switching(s: CoordinatorSwitch | null | undefined): boolean {
  return !!s && s.state !== "done" && s.state !== "failed";
}

/** How far a change of agent got, in a sentence. */
export function switchLine(s: CoordinatorSwitch): string {
  const to = agentWords(s.to);
  if (isRestart(s)) return restartLine(s, to);
  switch (s.state) {
    case "requested": return `Changing to ${to}.`;
    case "wrapping_up": return `Changing to ${to}. The coordinator is saving its work and writing a handoff.`;
    case "switching":
    case "starting": return `Changing to ${to}. Starting the new coordinator.`;
    case "briefing": return `Changing to ${to}. Telling the new coordinator what changed.`;
    case "done": return s.kind === "resume"
      ? `Changed from ${agentWords(s.from)} to ${to}, in the same conversation.`
      : `Changed from ${agentWords(s.from)} to ${to}, with a handoff.`;
    case "failed": return `Could not change to ${to}: ${s.error ?? "it did not start"}. The coordinator kept ${agentWords(s.from)}.`;
  }
}

/** A switch to the agent the coordinator already ran: a restart. */
export function isRestart(s: CoordinatorSwitch): boolean {
  const a = s.from, b = s.to;
  return !!a && a.harness === b.harness && (a.model ?? null) === (b.model ?? null)
    && (a.effort ?? null) === (b.effort ?? null) && (a.pool ?? null) === (b.pool ?? null);
}

function restartLine(s: CoordinatorSwitch, to: string): string {
  switch (s.state) {
    case "done": return `Restarted ${to}, in the same conversation.`;
    case "failed": return `Could not restart ${to}: ${s.error ?? "it did not start"}.`;
    default: return `Restarting ${to}.`;
  }
}
