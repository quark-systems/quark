// The transcript shared by the coordinator chat and the worker view: turns of a
// prompt, the agent's work folded into readable steps with edit previews, and its
// answer. Layout and interaction follow MonoCode's agent transcript
// (https://github.com/hardbeat920/monocode, MIT); the code is Quark's own.
import React, { useMemo, useRef, useState } from "react";
import type { TranscriptItem } from "../../api";
import { TranscriptContext, TranscriptSettings } from "./context";
import { useScrollAnchor } from "./hooks";
import { Icon } from "./Icon";
import { TurnView } from "./Turn";
import { breakLabel, groupTurns } from "./turns";
import "./transcript.css";

/** Turns rendered at first, and added each time the reader asks for earlier ones. */
export const TURN_PAGE = 40;

export interface TranscriptProps {
  items: TranscriptItem[];
  /** Who the agent is, for the live line: "coordinator" or "worker". */
  agent: string;
  /** Show the workers the agent started and the questions it asked as cards (the coordinator chat). */
  actions?: boolean;
  /** Shown after the turns, such as messages sent but not yet in the log. */
  children?: React.ReactNode;
  empty?: React.ReactNode;
  testId?: string;
  className?: string;
}

/**
 * Give it a `key` per source (task or coordinator), so paging and the entries that
 * count as new start over when the source changes.
 */
export function Transcript({ items, agent, actions = false, children, empty, testId, className }: TranscriptProps) {
  const turns = useMemo(() => groupTurns([...items].sort((a, b) => a.id - b.id)), [items]);
  const { ref, onScroll, unseen, jump, holdPosition } = useScrollAnchor<HTMLDivElement>(items.length);

  // The first turn shown is fixed at the first load, so turns arriving later are
  // added below rather than pushing earlier ones out of view.
  const firstLoad = useRef<{ start: number; maxId: number } | null>(null);
  if (firstLoad.current === null && turns.length) {
    firstLoad.current = { start: Math.max(0, turns.length - TURN_PAGE), maxId: Math.max(...items.map((i) => i.id)) };
  }
  const [more, setMore] = useState(0);
  const start = Math.min(turns.length, Math.max(0, (firstLoad.current?.start ?? 0) - more));
  const shown = turns.slice(start);

  const freshAfter = firstLoad.current?.maxId ?? Infinity;
  const settings = useMemo<TranscriptSettings>(() => ({ agent, actions, freshAfter }), [agent, actions, freshAfter]);

  const showEarlier = () => {
    holdPosition();
    setMore((m) => m + TURN_PAGE);
  };

  return (
    <TranscriptContext.Provider value={settings}>
      <div className={"tx " + (className ?? "")} ref={ref} onScroll={onScroll} data-testid={testId}>
        {start > 0 && (
          <button className="tx-earlier" onClick={showEarlier}>
            Show {Math.min(start, TURN_PAGE)} earlier {Math.min(start, TURN_PAGE) === 1 ? "turn" : "turns"}
          </button>
        )}
        {shown.map((t, i) => {
          const prev = shown[i - 1];
          const label = prev ? breakLabel(t.startedAt, prev.endedAt ?? prev.startedAt) : null;
          return (
            <React.Fragment key={t.key}>
              {label && <div className="tx-break" role="separator"><span>{label}</span></div>}
              <TurnView turn={t} />
            </React.Fragment>
          );
        })}
        {children}
        {!turns.length && !children && empty}
        {unseen && (
          <button className="tx-jump" onClick={jump}>
            <Icon kind="arrow-down" /> New activity
          </button>
        )}
      </div>
    </TranscriptContext.Provider>
  );
}
