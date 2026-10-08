// "Next attention": one button (and ⌘J) that walks everything waiting on you, oldest first.
import React, { useMemo } from "react";
import { go, parseRoute, Route, useRoute } from "../nav";
import { useStore } from "../store";
import { Kbd } from "../ui";
import { attentionQueue, AttentionItem, nextAttention } from "./attention";

export function useAttention(): AttentionItem[] {
  const decisions = useStore((s) => s.decisions);
  const prs = useStore((s) => s.pullRequests);
  const tasks = useStore((s) => s.tasks);
  return useMemo(() => attentionQueue(decisions, prs, tasks), [decisions, prs, tasks]);
}

/** Opens the next item; returns false when nothing waits. */
export function goNextAttention(queue: AttentionItem[], route: Route): boolean {
  const next = nextAttention(queue, route);
  if (next) go(next.route);
  return !!next;
}

const KIND: Record<AttentionItem["kind"], string> = { decision: "decision", pr: "check failed", worker: "worker stuck" };

export function NextAttention({ mod }: { mod: string }) {
  const queue = useAttention();
  const route = useRoute();
  const next = nextAttention(queue, route);
  if (!next) return <div className="na-none" data-testid="next-attention">Nothing needs you</div>;
  return (
    <button type="button" className="na-btn" data-testid="next-attention" onClick={() => goNextAttention(queue, parseRoute(location.hash))}
      title={`${next.title} (${KIND[next.kind]})`}>
      <span className="ui-dot needs-you" aria-hidden="true" />
      <span className="na-text">Next: {next.title}</span>
      <span className="na-count" aria-label={`${queue.length} waiting`}>{queue.length}</span>
      <Kbd keys={`${mod}J`} />
    </button>
  );
}
