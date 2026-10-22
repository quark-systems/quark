// A catch up, status report or wrap up in the conversation: what you asked for, the
// moment you ask, then what the engine handed the coordinator. The coordinator's
// answer follows it like any reply.
import { useState } from "react";
import type { CoordinatorCommand } from "../../api";
import { ALIASES, CommandNote, HINTS } from "../../commands";
import { ago } from "../../util";
import { rise, useTranscript } from "./context";
import { Icon } from "./Icon";

const ICON: Record<CoordinatorCommand, string> = { catch_up: "catch-up", status_report: "status", wrap_up: "wrap-up" };

/** The second line: what the engine passed on, else what the command does. */
export function commandDetail(n: Pick<CommandNote, "command"> & Partial<CommandNote>): string {
  const count = (k: number | undefined, one: string, many: string) => (k === undefined ? null : `${k === 0 ? "no" : k} ${k === 1 ? one : many}`);
  const parts = n.asked === "engine"
    ? [`The conversation holds about ${n.contextK}k tokens, so the coordinator saves what it learned before it is compacted`]
    : n.command === "catch_up"
      ? [n.since ? `Since your last message ${ago(n.since)}` : null, count(n.events, "event", "events"), count(n.decisions, "open decision", "open decisions")]
      : n.command === "status_report"
        ? [count(n.tasks, "worker", "workers"), count(n.decisions, "open decision", "open decisions")]
        : [];
  const shown = parts.filter(Boolean);
  return shown.length ? shown.join(" · ") : HINTS[n.command];
}

export function CommandCard({ note, id, raw, state }: {
  note: Pick<CommandNote, "command"> & Partial<CommandNote>;
  /** The entry that recorded it; absent while it is on its way. */
  id?: number;
  raw?: string;
  /** While it is on its way: being sent, or typed but not yet seen in the conversation. */
  state?: "sending" | "sent" | "unconfirmed";
}) {
  const { freshAfter, commandLabel } = useTranscript();
  const [open, setOpen] = useState(false);
  const label = commandLabel(note.command);
  const status = state === "sending" ? "Asking…" : state === "sent" ? "Asked" : state === "unconfirmed" ? "Typed, not confirmed yet" : null;
  return (
    <div className={"tx-command" + (note.asked === "engine" ? " by-engine" : "") + (state ? " pending tx-rise" : id !== undefined ? rise(id, freshAfter) : "")}
      data-testid="command-card" data-command={note.command}>
      <button className="tx-command-head" aria-expanded={open} disabled={!raw} onClick={() => setOpen(!open)}
        title={raw ? "What the coordinator was given" : undefined}>
        <span className="tx-command-mark"><Icon kind={ICON[note.command]} /></span>
        <span className="tx-command-body">
          <span className="tx-command-title">
            <b>{label}</b>
            {note.asked === "engine" ? <span className="tx-command-by">automatic</span> : <span className="mono tx-command-slash">/{ALIASES[note.command][0]}</span>}
          </span>
          <span className="tx-command-detail">{commandDetail(note)}</span>
        </span>
        {status ? <span className="tx-command-status">{status}</span> : raw && <Icon kind={open ? "chevron-down" : "chevron-right"} />}
      </button>
      {open && raw && <pre className="tx-detail">{raw.trim()}</pre>}
    </div>
  );
}
