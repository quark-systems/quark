// What the coordinator's header says about its agent and how full its context is
// (`GET /v1/coordinators/{id}/context`), as words the header and its popover show.
import type { CoordinatorContext } from "./api";
import { harnessMark } from "./components/WorkerCard/harnessMarks";

/** "46k", "1.2M", "800". */
export function tokens(n: number): string {
  if (n >= 1_000_000) return `${+(n / 1_000_000).toFixed(n % 1_000_000 ? 1 : 0)}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

/** The agent in one line: "Claude Code · opus · high". Blanks say the harness picks. */
export function agentLine(c: CoordinatorContext): { harness: string; model: string; effort: string } {
  const a = c.agent_config;
  return {
    harness: a ? harnessMark(a.harness).name : "Unknown harness",
    model: a?.model ?? c.model_seen ?? "default model",
    effort: a?.effort ? `${a.effort} effort` : "default effort",
  };
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
  return `Compacts at ${tokens(c.compaction.at_tokens)} tokens, after a wrap up.`;
}
