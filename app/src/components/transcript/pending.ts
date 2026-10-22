import type { CoordinatorCommand, TranscriptItem } from "../../api";
import { engineEvent } from "./events";

/** A message the person sent that the agent's session has not recorded yet. */
export interface PendingMessage {
  key: number;
  text: string;
  /** Still on its way to the session, before the daemon answered. */
  sending?: boolean;
  confirmed: boolean;
  /** The largest entry id loaded when it was sent; only later entries can record it. */
  after: number;
}

/** The largest entry id in `items`, or 0. */
export const lastId = (items: TranscriptItem[]) => items.reduce((m, i) => Math.max(m, i.id), 0);

/** Text compared without its spacing: a terminal can turn line breaks into \r or re-wrap them. */
const same = (a: string) => a.replace(/\s+/g, " ").trim();

/**
 * The pending messages the log has not recorded yet. A user entry records the
 * oldest pending message with the same text that was sent before it arrived, so
 * an earlier identical message never clears a new one, and sending the same
 * text twice needs two entries.
 */
export function stillPending(pending: PendingMessage[], items: TranscriptItem[]): PendingMessage[] {
  if (!pending.length) return pending;
  const left = [...pending];
  for (const item of [...items].sort((a, b) => a.id - b.id)) {
    if (item.role !== "user") continue;
    const text = same(item.text);
    const i = left.findIndex((p) => item.id > p.after && same(p.text) === text);
    if (i >= 0) left.splice(i, 1);
  }
  return left.length === pending.length ? pending : left;
}

/** A command the person asked for whose note the session has not recorded yet. */
export interface PendingCommand {
  key: number;
  command: CoordinatorCommand;
  sending: boolean;
  confirmed: boolean;
  after: number;
}

/** The pending commands no later entry records: a command's note, by command, recorded after it was sent. */
export function stillPendingCommands(pending: PendingCommand[], items: TranscriptItem[]): PendingCommand[] {
  if (!pending.length) return pending;
  const left = [...pending];
  for (const item of [...items].sort((a, b) => a.id - b.id)) {
    if (item.role !== "user") continue;
    const c = engineEvent(item.text)?.command;
    if (!c || c.asked !== "you") continue;
    const i = left.findIndex((p) => item.id > p.after && p.command === c.command);
    if (i >= 0) left.splice(i, 1);
  }
  return left.length === pending.length ? pending : left;
}
