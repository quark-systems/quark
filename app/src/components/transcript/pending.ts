import type { ReactNode } from "react";
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

/** A pending message as the transcript shows it: where it was sent, and the line under it. */
export interface PendingPrompt {
  /** The largest entry id loaded when it was sent; queued messages go after everything. */
  after: number;
  text: string;
  /** The line under it: where it is, and what can be done with it. */
  note: ReactNode;
  /** `error` when it was not delivered. */
  tone?: "error";
  /** Change its text in place, while it waits; resolves `false` when that failed. */
  onEdit?: (text: string) => Promise<boolean>;
  /** Links after the note and Edit, such as Send now and Cancel. */
  actions?: ReactNode;
  /** Shown in place of the message bubble, such as a command's card. */
  card?: ReactNode;
}

/**
 * The log with each pending message placed where it was sent: after the last entry
 * loaded at the time, so entries recorded later (the replies to it) show below it even
 * while the log has not recorded the message itself. A pending message gets an id
 * between two whole entry ids; `notes` holds it by that id. Pending messages are in
 * the order they were sent, and an earlier one never has a larger `after`.
 */
export function placePending(items: TranscriptItem[], pending: PendingPrompt[]): { items: TranscriptItem[]; notes: Map<number, PendingPrompt> } {
  const notes = new Map<number, PendingPrompt>();
  if (!pending.length) return { items, notes };
  const placed = pending.map((p, i): TranscriptItem => {
    const id = p.after + (i + 1) / (pending.length + 1);
    notes.set(id, p);
    return { id, role: "user", text: p.text, ts: null, is_error: false, truncated: false };
  });
  return { items: [...items, ...placed].sort((a, b) => a.id - b.id), notes };
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
