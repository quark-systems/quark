// One turn: the person's prompt as a bubble, the agent's work, its answer as
// unboxed prose, and a footer with how long it worked. Follows MonoCode's
// UserMessageBlock, TurnDuration and CopyTurnButton (https://github.com/hardbeat920/monocode, MIT).
import { memo, useMemo } from "react";
import type { TranscriptItem } from "../../api";
import { renderMarkdown } from "../../markdown";
import { ago } from "../../util";
import { rise, useTranscript } from "./context";
import { useCopy, useElapsed } from "./hooks";
import { Icon } from "./Icon";
import { duration, Turn, turnMarkdown } from "./turns";
import { Work } from "./Work";

export const TurnView = memo(function TurnView({ turn }: { turn: Turn }) {
  const { freshAfter } = useTranscript();
  return (
    <section className="tx-turn">
      {turn.prompt && <UserMessage item={turn.prompt} fresh={turn.prompt.id > freshAfter} />}
      {turn.parts.map((p, i) =>
        p.kind === "text"
          ? <AgentText key={p.item.id} item={p.item} />
          : <Work key={p.key} part={p} live={turn.live && i === turn.parts.length - 1} />,
      )}
      <TurnFooter turn={turn} />
    </section>
  );
});

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

function TurnFooter({ turn }: { turn: Turn }) {
  const { agent } = useTranscript();
  const elapsed = useElapsed(turn.live ? turn.startedAt : undefined);
  const { copied, copy } = useCopy();
  if (turn.live) {
    return (
      <div className="tx-footer live">
        <span className="shimmer">{agent[0].toUpperCase() + agent.slice(1)} is working</span>
        {elapsed !== undefined && <span className="faint"> · {duration(elapsed)}</span>}
      </div>
    );
  }
  const worked = turn.startedAt !== undefined && turn.endedAt !== undefined ? turn.endedAt - turn.startedAt : 0;
  const showWorked = !!turn.prompt && worked >= 1000;
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
