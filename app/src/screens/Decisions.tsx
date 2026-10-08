// A Project's decision log: every decision asked in it, what was answered and why, what happened
// next, and the standing rules answers became. Open decisions are answered here on the full card.
//
// Keys: j/k or ↑/↓ move, Enter or r starts answering, / searches.
import React, { useEffect, useMemo, useRef, useState } from "react";
import { go, href } from "../nav";
import { useStore } from "../store";
import { step } from "../inbox";
import { decidedByAgent, decisionLabel, logList, LogFilter, rowSummary, rowTone } from "../decisions";
import { ago } from "../util";
import { DecisionCard, DecisionEvidence } from "../ui";

const FILTERS: { id: LogFilter; label: string }[] = [
  { id: "open", label: "Open" },
  { id: "all", label: "All" },
  { id: "rules", label: "Standing rules" },
  { id: "agents", label: "Decided by agents" },
];

export function Decisions({ project: pid, id }: { project: string; id?: string }) {
  const project = useStore((s) => s.projects[pid]);
  const connected = useStore((s) => s.connected);
  const decisions = useStore((s) => s.decisions);
  const tasks = useStore((s) => s.tasks);
  const rules = useStore((s) => s.rules);
  const mine = useMemo(() => Object.values(decisions).filter((d) => d.project_id === pid), [decisions, pid]);
  const counts = useMemo(() => Object.fromEntries(FILTERS.map((f) => [f.id, logList(mine, f.id).length])) as Record<LogFilter, number>, [mine]);

  // Open decisions first when any wait; otherwise the whole log.
  const [chosen, setFilter] = useState<LogFilter | null>(null);
  // Until one is picked: the open ones when any wait, unless the URL names one that is not open.
  const linked = id ? decisions[id] : undefined;
  const filter: LogFilter = chosen ?? (counts.open && (!linked || linked.state === "open") ? "open" : "all");
  const [query, setQuery] = useState("");
  const list = useMemo(() => logList(mine, filter, query), [mine, filter, query]);

  // A selection in the URL wins, even when the filter hides it; otherwise the first row.
  const selected = id && decisions[id]?.project_id === pid ? id : list[0]?.id;
  const current = selected ? decisions[selected] : undefined;
  const select = (next: string | undefined) => go({ name: "decisions", project: pid, id: next });

  const answerBox = useRef<HTMLTextAreaElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const rowRefs = useRef(new Map<string, HTMLElement>());
  useEffect(() => { if (selected) rowRefs.current.get(selected)?.scrollIntoView({ block: "nearest" }); }, [selected]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.closest("input, textarea, select, [contenteditable]") || t.closest("[cmdk-root]"))) return;
      const move = (delta: number) => { e.preventDefault(); select(step(list, selected, delta)); };
      switch (e.key) {
        case "j": case "ArrowDown": return move(1);
        case "k": case "ArrowUp": return move(-1);
        case "/": e.preventDefault(); search.current?.focus(); return;
        case "Enter": case "r":
          if (current?.state === "open") { e.preventDefault(); answerBox.current?.focus(); }
          return;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  if (!project) {
    return <div className="empty">{connected ? "This project does not exist on the daemon." : "Waiting for the daemon…"}</div>;
  }
  const byId = decisions;
  return (
    <>
      <div className="screen dl">
        <section className="dl-list" aria-label="Decisions">
          <div className="dl-filters">
            {FILTERS.map((f) => (
              <button key={f.id} type="button" className="dl-chip" aria-pressed={filter === f.id} data-testid={`decisions-filter-${f.id}`}
                onClick={() => setFilter(f.id)}>{f.label} {counts[f.id]}</button>
            ))}
            <label htmlFor="dl-search" className="sr-only">Search decisions</label>
            <input id="dl-search" ref={search} className="dl-search" placeholder="Search" value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); setQuery(""); e.currentTarget.blur(); } }} />
          </div>
          <div className="dl-rows" data-testid="decision-log">
            {list.map((d) => (
              <a key={d.id} ref={(el) => { if (el) rowRefs.current.set(d.id, el); else rowRefs.current.delete(d.id); }}
                className={"dl-row" + (d.id === selected ? " on" : "")} href={href({ name: "decisions", project: pid, id: d.id })}
                data-testid="decision-log-row" aria-current={d.id === selected ? "true" : undefined}>
                <span className="dl-num">{decisionLabel(d)}</span>
                <span className="dl-text">
                  <span className="dl-q">{d.question}</span>
                  <span className={"dl-sub " + rowTone(d)}>{rowSummary(d, tasks, rules, byId)}</span>
                </span>
                <span className="dl-when">
                  {ago(d.state === "open" ? d.opened_at : d.answered_at ?? d.opened_at)}
                  {decidedByAgent(d) && <span className="sr-only">, decided by an agent</span>}
                </span>
              </a>
            ))}
            {!list.length && (
              <div className="empty">
                {!connected ? "Waiting for the daemon…"
                  : query ? "No decision matches that search."
                    : filter === "open" ? "Nothing in this Project needs a decision right now."
                      : filter === "rules" ? "No answer has become a standing rule yet."
                        : filter === "agents" ? "No agent has decided anything under a standing rule yet."
                          : "No decisions yet."}
              </div>
            )}
          </div>
        </section>
        {current ? (
          <div className="dl-detail">
            <DecisionCard key={current.id} d={current} size="full" answerBox={answerBox} onAnswered={(a) => select(a.id)} />
            <DecisionEvidence d={current} />
          </div>
        ) : <div className="dl-detail empty">Select a decision.</div>}
      </div>
    </>
  );
}
