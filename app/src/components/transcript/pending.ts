import type { TranscriptItem } from "../../api";

/** A message the person sent that the agent's session has not recorded yet. */
export interface PendingMessage {
  key: number;
  text: string;
  confirmed: boolean;
  /** The largest entry id loaded when it was sent; only later entries can record it. */
  after: number;
}

/** The largest entry id in `items`, or 0. */
export const lastId = (items: TranscriptItem[]) => items.reduce((m, i) => Math.max(m, i.id), 0);

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
    const i = left.findIndex((p) => item.id > p.after && p.text === item.text.trim());
    if (i >= 0) left.splice(i, 1);
  }
  return left.length === pending.length ? pending : left;
}
