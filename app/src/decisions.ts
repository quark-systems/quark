// The decision log's words and lists, kept pure so they are easy to test. The card
// (`ui/DecisionCard.tsx`) and the Decisions tab (`screens/Decisions.tsx`) read them.
import type { Decision, StandingRule, Task } from "./api";

/** `D-14`, or the id's tail for a decision recorded before numbering. */
export function decisionLabel(d: Pick<Decision, "id" | "number">): string {
  return d.number ? `D-${d.number}` : d.id.slice(-6);
}

/** Who asked, in the glossary's words: the coordinator, a worker by title, or Quark itself. */
export function askedBy(d: Decision, tasks: Record<string, Task>): string {
  const who = d.brief?.asked_by;
  if (!who || who === "coordinator") return "the coordinator";
  if (who === "quarkd") return "Quark";
  return tasks[who]?.title ?? who;
}

/** What waits on the answer, shortened: `PR #95`, worker titles, other ids as given. */
export function blockLabels(d: Decision, tasks: Record<string, Task>): string[] {
  return (d.brief?.blocks ?? []).map((b) => {
    const pr = b.match(/\/pull\/(\d+)/);
    if (pr) return `PR #${pr[1]}`;
    return tasks[b]?.title ?? b;
  });
}

/** `a`, `a and b`, `a, b and c`. */
export function joinAnd(items: string[]): string {
  if (items.length <= 1) return items[0] ?? "";
  return `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;
}

/** The lifecycle step a decision has reached: 0 asked, 1 answered, 2 acted on, 3 in the log. */
export function lifecycleStep(d: Decision): 0 | 1 | 2 | 3 {
  if (d.state === "open") return 0;
  if (d.state === "answered") return 1;
  return 3;
}

export const LIFECYCLE = ["Asked", "Answered", "Acted on", "In the log"] as const;

/** Whether an agent decided it under a standing rule rather than a person answering. */
export function decidedByAgent(d: Decision): boolean {
  return !!d.rule_id || d.answered_via === "rule";
}

export type LogFilter = "open" | "all" | "rules" | "agents";

/** The log's rows for a filter and search: open ones oldest first (they wait longest), the rest newest first. */
export function logList(all: Decision[], filter: LogFilter, query = ""): Decision[] {
  const q = query.trim().toLowerCase();
  const matches = (d: Decision) => !q || [d.question, d.answer, d.answer_why, d.outcome, d.brief?.context, decisionLabel(d)]
    .some((t) => t?.toLowerCase().includes(q));
  const pick = all.filter((d) => matches(d) && (
    filter === "open" ? d.state === "open"
      : filter === "rules" ? !!d.made_rule_id
        : filter === "agents" ? decidedByAgent(d)
          : true));
  const when = (d: Decision) => d.state === "open" ? d.opened_at : d.answered_at ?? d.opened_at;
  return filter === "open"
    ? pick.sort((a, b) => a.opened_at.localeCompare(b.opened_at) || a.id.localeCompare(b.id))
    : pick.sort((a, b) => when(b).localeCompare(when(a)) || (b.number ?? 0) - (a.number ?? 0));
}

/** A row's second line: who asked for an open one, else the answer, who gave it and any rule. */
export function rowSummary(d: Decision, tasks: Record<string, Task>, rules: Record<string, StandingRule>, byId: Record<string, Decision>): string {
  if (d.state === "open") {
    const blocks = blockLabels(d, tasks);
    return blocks.length ? `Open · blocks ${joinAnd(blocks)}` : `Open · asked by ${askedBy(d, tasks)}`;
  }
  const parts = [firstLine(d.answer ?? "Answered elsewhere")];
  if (d.rule_id) {
    const rule = rules[d.rule_id];
    const from = rule?.decision_id ? byId[rule.decision_id] : undefined;
    parts.push(`${d.answered_by ?? "an agent"}, under rule ${from ? decisionLabel(from) : "in force"}`);
  } else if (d.answered_by) {
    parts.push(d.answered_by);
  }
  if (d.made_rule_id) parts.push("standing rule");
  return parts.join(" · ");
}

/** The tone a row's second line takes: needs-you while open, accent for a rule, else plain. */
export function rowTone(d: Decision): "needs-you" | "accent" | "plain" {
  if (d.state === "open") return "needs-you";
  if (d.made_rule_id) return "accent";
  return "plain";
}

/** Decisions logged under the rule `d` made, newest first. */
export function usesOf(d: Decision, all: Decision[]): Decision[] {
  if (!d.made_rule_id) return [];
  return all.filter((x) => x.rule_id === d.made_rule_id)
    .sort((a, b) => (b.answered_at ?? b.opened_at).localeCompare(a.answered_at ?? a.opened_at));
}

/** A suggested standing-rule text for an answer, which the person edits before sending. */
export function ruleDraft(d: Decision, answer: string): string {
  const a = firstLine(answer).replace(/[.\s]+$/, "");
  return a ? `For questions like "${firstLine(d.question)}": ${a}.` : "";
}

function firstLine(s: string): string {
  return s.split("\n")[0].trim();
}

/** How the log says where an answer came from. */
export function answeredVia(via?: string | null): string | null {
  switch (via) {
    case null: case undefined: case "": return null;
    case "rule": return "under a standing rule";
    case "beads": return "in Beads";
    default: return `in the ${via}`;
  }
}
