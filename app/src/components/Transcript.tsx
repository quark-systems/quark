// The transcript shared by the coordinator chat and the worker view: turns of a
// prompt, the agent's work folded into readable steps with edit previews, and its
// answer. Layout and interaction follow MonoCode's agent transcript
// (https://github.com/hardbeat920/monocode, MIT); the code is Quark's own.
import React, { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ToolInfo, TranscriptItem } from "../api";
import { renderMarkdown } from "../markdown";
import { ago } from "../util";
import {
  dayLabel, duration, groupTurns, newDay, Step, stepKind, stepTitle, summarize, Turn, TurnPart,
} from "../transcript";

export interface TranscriptProps {
  items: TranscriptItem[];
  /** Who the agent is, for the live line: "coordinator" or "worker". */
  agent: string;
  /** Shown after the turns, such as messages sent but not yet in the log. */
  children?: React.ReactNode;
  empty?: React.ReactNode;
  testId?: string;
  className?: string;
}

/** Follows new output while the reader is at the bottom; stays put once they scroll up. */
export function useStickToBottom<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const stick = useRef(true);
  useLayoutEffect(() => {
    if (stick.current && ref.current) ref.current.scrollTop = ref.current.scrollHeight;
  });
  const onScroll = (e: React.UIEvent<T>) => {
    const el = e.currentTarget;
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  };
  return { ref, onScroll, follow: () => { stick.current = true; } };
}

export function Transcript({ items, agent, children, empty, testId, className }: TranscriptProps) {
  const turns = useMemo(() => groupTurns([...items].sort((a, b) => a.id - b.id)), [items]);
  const { ref, onScroll } = useStickToBottom<HTMLDivElement>();
  return (
    <div className={"tx " + (className ?? "")} ref={ref} onScroll={onScroll} data-testid={testId}>
      {turns.map((t, i) => (
        <React.Fragment key={t.key}>
          {newDay(t.startedAt, turns[i - 1]?.startedAt ?? turns[i - 1]?.endedAt) && i > 0 && <DaySeparator at={t.startedAt!} />}
          <TurnView turn={t} agent={agent} />
        </React.Fragment>
      ))}
      {children}
      {!turns.length && !children && empty}
    </div>
  );
}

function DaySeparator({ at }: { at: number }) {
  return <div className="tx-day" role="separator"><span>{dayLabel(at)}</span></div>;
}

const TurnView = memo(function TurnView({ turn, agent }: { turn: Turn; agent: string }) {
  return (
    <section className="tx-turn">
      {turn.prompt && <UserMessage item={turn.prompt} />}
      {turn.parts.map((p, i) =>
        p.kind === "text"
          ? <AgentText key={p.item.id} item={p.item} />
          : <Work key={p.key} part={p} live={turn.live && i === turn.parts.length - 1} />,
      )}
      <TurnFooter turn={turn} agent={agent} />
    </section>
  );
});

export function UserMessage({ item, pending, note }: { item: Pick<TranscriptItem, "text" | "ts" | "truncated">; pending?: boolean; note?: string }) {
  return (
    <div className={"tx-user msg" + (pending ? " pending" : "")}>
      <div className="tx-bubble">{item.text}</div>
      <div className="tx-meta">{note ?? ago(item.ts)}{item.truncated && " · truncated"}</div>
    </div>
  );
}

const AgentText = memo(function AgentText({ item }: { item: TranscriptItem }) {
  const html = useMemo(() => renderMarkdown(item.text), [item.text]);
  return (
    <div className="tx-text">
      <div className="md" dangerouslySetInnerHTML={{ __html: html }} />
      {item.truncated && <div className="tx-meta">Truncated by the daemon.</div>}
    </div>
  );
});

function TurnFooter({ turn, agent }: { turn: Turn; agent: string }) {
  const elapsed = useElapsed(turn.live ? turn.startedAt : undefined);
  if (turn.live) {
    return (
      <div className="tx-footer live">
        <span className="shimmer">{agent[0].toUpperCase() + agent.slice(1)} is working</span>
        {elapsed !== undefined && <span className="faint"> · {duration(elapsed)}</span>}
      </div>
    );
  }
  const worked = turn.startedAt !== undefined && turn.endedAt !== undefined ? turn.endedAt - turn.startedAt : 0;
  if (!turn.prompt || worked < 1000) return null;
  return <div className="tx-footer"><Icon kind="check" /> Worked for {duration(worked)}</div>;
}

/** Milliseconds since `from`, ticking every second; undefined when not running. */
function useElapsed(from: number | undefined): number | undefined {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (from === undefined) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [from]);
  return from === undefined ? undefined : Math.max(0, now - from);
}

// ---- work ----

function Work({ part, live }: { part: Extract<TurnPart, { kind: "work" }>; live: boolean }) {
  const [open, setOpen] = useState(false);
  const edits = part.steps.filter((s) => s.call?.tool?.diff?.length);
  const failed = part.steps.filter((s) => s.status === "error").length;
  if (live) {
    // While the agent works, earlier steps fold behind one line and the latest stays in view.
    const earlier = part.steps.slice(0, -1);
    const latest = part.steps[part.steps.length - 1];
    return (
      <div className="tx-work">
        {earlier.length > 0 && (
          open ? earlier.map((s) => <StepRow key={s.key} step={s} />)
            : <button className="tx-fold" onClick={() => setOpen(true)}>+{earlier.length} earlier {earlier.length === 1 ? "step" : "steps"}</button>
        )}
        {latest && <StepRow step={latest} live />}
      </div>
    );
  }
  return (
    <div className="tx-work">
      <button className="tx-fold" aria-expanded={open} onClick={() => setOpen(!open)}>
        <Icon kind={open ? "chevron-down" : "chevron-right"} />
        {summarize(part.steps)}
        {failed > 0 && <span className="bad"> · {failed} failed</span>}
      </button>
      {open ? part.steps.map((s) => <StepRow key={s.key} step={s} />)
        : edits.map((s) => <DiffCard key={s.key} tool={s.call!.tool!} />)}
    </div>
  );
}

function StepRow({ step, live }: { step: Step; live?: boolean }) {
  const [open, setOpen] = useState(false);
  const kind = stepKind(step);
  const tool = step.call?.tool;
  const detail = step.thinking?.text ?? [tool?.command ?? (tool ? null : step.call?.text), step.result?.text].filter(Boolean).join("\n\n");
  const running = step.status === "running";
  return (
    <div className={"tx-step" + (step.status === "error" ? " error" : "")}>
      <button className="tx-step-row" aria-expanded={open} onClick={() => setOpen(!open)} disabled={!detail}>
        <Icon kind={kind} />
        <span className={"tx-step-title" + (live && running ? " shimmer" : "")}>{stepTitle(step)}</span>
        {tool?.additions !== undefined && <span className="adds">+{tool.additions}</span>}
        {tool?.deletions !== undefined && tool.deletions > 0 && <span className="dels">−{tool.deletions}</span>}
        <span className="tx-step-status">{running ? <span className="spinner" aria-label="running" /> : step.status === "error" ? <Icon kind="cross" /> : null}</span>
      </button>
      {tool?.diff?.length ? <DiffCard tool={tool} /> : null}
      {open && detail && <pre className="tx-detail">{detail}{step.result?.truncated ? "\n…" : ""}</pre>}
    </div>
  );
}

/** The first lines of an edit, as the agent made it. */
export function DiffCard({ tool }: { tool: ToolInfo }) {
  return (
    <div className="tx-diff">
      <div className="tx-diff-head">
        <span className="mono ellipsis">{tool.path ?? tool.title}</span>
        <span className="spacer" />
        {tool.additions !== undefined && <span className="adds">+{tool.additions}</span>}
        {tool.deletions !== undefined && <span className="dels">−{tool.deletions}</span>}
      </div>
      <div className="tx-diff-body">
        {tool.diff!.map((l, i) => (
          <div key={i} className={"tx-diff-line " + l.kind}>
            <span className="sign">{l.kind === "add" ? "+" : l.kind === "del" ? "−" : " "}</span>
            <span className="code">{l.text || " "}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

// ---- icons ----
// 16px outline glyphs drawn for Quark, one per kind of step.
const PATHS: Record<string, string> = {
  read: "M4 2h5l3 3v9H4zM9 2v3h3M6 8h4M6 10.5h4",
  edit: "M10.5 2.5l3 3L6 13H3v-3zM9 4l3 3",
  write: "M4 2h5l3 3v9H4zM9 2v3h3M8 7.5v4M6 9.5h4",
  shell: "M2 3h12v10H2zM4.5 6.5l2 1.5-2 1.5M8 10h3.5",
  search: "M7 3a4 4 0 1 1 0 8 4 4 0 0 1 0-8zM10 10l3.5 3.5",
  web: "M8 2a6 6 0 1 1 0 12A6 6 0 0 1 8 2zM2 8h12M8 2c2 2 2 10 0 12M8 2c-2 2-2 10 0 12",
  agent: "M4 5h8v7H4zM8 2v3M6 8h.01M10 8h.01M6.5 10.5h3",
  plan: "M6 4h7M6 8h7M6 12h7M3 4h.01M3 8h.01M3 12h.01",
  other: "M8 5.5v5M5.5 8h5",
  thinking: "M8 2.5a4 4 0 0 1 2.5 7.1V11h-5V9.6A4 4 0 0 1 8 2.5zM6 13.5h4",
  check: "M3.5 8.5l3 3 6-7",
  cross: "M4.5 4.5l7 7M11.5 4.5l-7 7",
  "chevron-right": "M6 4l4 4-4 4",
  "chevron-down": "M4 6l4 4 4-4",
};

function Icon({ kind }: { kind: string }) {
  return (
    <svg className={"tx-icon " + kind} viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
      <path d={PATHS[kind] ?? PATHS.other} fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}
