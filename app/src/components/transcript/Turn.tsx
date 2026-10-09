// One turn: the person's prompt as a bubble, the agent's work, its answer as
// unboxed prose, and a footer with how long it worked. Follows MonoCode's
// UserMessageBlock, TurnDuration and CopyTurnButton (https://github.com/hardbeat920/monocode, MIT).
import { memo, useMemo, useState } from "react";
import type { TranscriptItem } from "../../api";
import { renderMarkdown } from "../../markdown";
import { ago } from "../../util";
import { rise, useTranscript } from "./context";
import type { EngineEvent } from "./events";
import { useCopy, useElapsed, useNow } from "./hooks";
import { Icon } from "./Icon";
import { activity, Activity, duration, Turn, turnMarkdown } from "./turns";
import { Work } from "./Work";

export const TurnView = memo(function TurnView({ turn }: { turn: Turn }) {
  const { freshAfter } = useTranscript();
  const now = useNow(turn.live);
  const doing = activity(turn, now);
  return (
    <section className="tx-turn">
      {turn.event && turn.prompt ? <EventRow event={turn.event} id={turn.prompt.id} />
        : turn.prompt && <UserMessage item={turn.prompt} fresh={turn.prompt.id > freshAfter} />}
      {turn.parts.map((p, i) =>
        p.kind === "text"
          ? <AgentText key={p.item.id} item={p.item} />
          : <Work key={p.key} part={p} activity={i === turn.parts.length - 1 ? doing : null} />,
      )}
      <TurnFooter turn={turn} doing={doing} now={now} />
    </section>
  );
});

const EVENT_ICON: Record<EngineEvent["kind"], string> = { wake: "event", background: "event", engine: "event", warning: "warning" };

/** A message from the engine, not a person: one quiet line, with what the session recorded behind it. */
function EventRow({ event, id }: { event: EngineEvent; id: number }) {
  const { freshAfter } = useTranscript();
  const [open, setOpen] = useState(false);
  return (
    <div className={"tx-event tx-event-" + event.kind + rise(id, freshAfter)} data-testid="engine-event">
      <button className="tx-event-row" aria-expanded={open} onClick={() => setOpen(!open)} title="What the engine sent">
        <Icon kind={EVENT_ICON[event.kind]} />
        <span className="tx-event-title">{event.title}</span>
        <Icon kind={open ? "chevron-down" : "chevron-right"} />
      </button>
      {open && <pre className="tx-detail">{event.raw.trim()}</pre>}
    </div>
  );
}

export function UserMessage({ item, pending, note, fresh }: {
  item: Pick<TranscriptItem, "text" | "ts" | "truncated">; pending?: boolean; note?: string; fresh?: boolean;
}) {
  return (
    <div className={"tx-user msg" + (pending ? " pending" : "") + (pending || fresh ? " tx-rise" : "")}>
      <div className="tx-bubble">{item.text}</div>
      <div className="tx-meta">{note ?? ago(item.ts)}{item.truncated && " · truncated"}</div>
    </div>
  );
}

const AgentText = memo(function AgentText({ item }: { item: TranscriptItem }) {
  const { freshAfter } = useTranscript();
  const html = useMemo(() => renderMarkdown(item.text), [item.text]);
  return (
    <div className={"tx-text" + rise(item.id, freshAfter)}>
      <div className="md" dangerouslySetInnerHTML={{ __html: html }} />
      {item.truncated && <div className="tx-meta">Truncated by the daemon.</div>}
    </div>
  );
});

function TurnFooter({ turn, doing, now }: { turn: Turn; doing: Activity | null; now: number }) {
  const { agent } = useTranscript();
  const elapsed = useElapsed(doing === "working" ? turn.startedAt : undefined);
  const { copied, copy } = useCopy();
  const who = agent[0].toUpperCase() + agent.slice(1);
  if (doing === "listening") {
    // Waiting on the watcher costs nothing: no timer, no shimmer.
    return (
      <div className="tx-footer listening" data-testid="agent-activity">
        <span className="tx-listen" aria-hidden="true" />{who} is listening for project events
      </div>
    );
  }
  if (doing === "quiet") {
    return (
      <div className="tx-footer quiet" data-testid="agent-activity" title="Nothing has been recorded since then">
        {who} is idle · nothing new for {duration(now - turn.endedAt!)}
      </div>
    );
  }
  if (doing === "working") {
    return (
      <div className="tx-footer live" data-testid="agent-activity">
        <span className="shimmer">{who} is {turn.event ? "handling a project event" : "working"}</span>
        {elapsed !== undefined && <span className="faint"> · {duration(elapsed)}</span>}
      </div>
    );
  }
  const worked = turn.startedAt !== undefined && turn.endedAt !== undefined ? turn.endedAt - turn.startedAt : 0;
  // Engine events are routine: how long one took to handle is not worth a line.
  const showWorked = !!turn.prompt && !turn.event && worked >= 1000;
  const answered = turn.parts.some((p) => p.kind === "text");
  if (!showWorked && !answered) return null;
  return (
    <div className="tx-footer">
      {showWorked && <><Icon kind="check" /> Worked for {duration(worked)}</>}
      {answered && (
        <button className="tx-copy" onClick={() => copy(turnMarkdown(turn))} aria-label="Copy this turn" title="Copy this turn as Markdown">
          <Icon kind={copied ? "check" : "copy"} />{copied && <span>Copied</span>}
        </button>
      )}
    </div>
  );
}
