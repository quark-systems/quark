// Messages sent to a coordinator that its transcript doesn't hold yet: queued while
// it works (editable, cancellable, or sent at once), being typed in, or not
// delivered. The daemon records each one (its outbox) and adds it to the
// transcript, with its id, once typed in.
import { useMemo, useState } from "react";
import { api, OutboxMessage, TranscriptItem } from "../api";
import { lastId, PendingMessage, PendingPrompt } from "./transcript";
import { noteOutbox } from "../store";
import { errText } from "../util";

/** Text compared without its spacing, as the transcript's pending messages are. */
const same = (a: string) => a.replace(/\s+/g, " ").trim();

/**
 * The sent messages to show under the transcript, oldest first: queued, being typed
 * in, or failed, and not yet recorded in `items`. A message this window is still
 * sending (`local`) shows as that instead.
 */
export function waitingMessages(outbox: Record<string, OutboxMessage> | undefined, items: TranscriptItem[], local: PendingMessage[] = []): OutboxMessage[] {
  if (!outbox) return [];
  const recorded = new Set(items.map((i) => i.message_id).filter(Boolean));
  const mine = local.filter((p) => p.sending).map((p) => same(p.text));
  return Object.values(outbox)
    .filter((m) => (m.state === "queued" || m.state === "sending" || m.state === "failed") && !recorded.has(m.id))
    // A command's note: its card shows it while it is on its way.
    .filter((m) => !m.text.startsWith("Engine note: "))
    .filter((m) => {
      if (m.state !== "sending") return true;
      const i = mine.indexOf(same(m.text));
      if (i < 0) return true;
      mine.splice(i, 1);
      return false;
    })
    .sort((a, b) => a.created_at.localeCompare(b.created_at) || a.id.localeCompare(b.id));
}

/** The transcript's pending prompts for the coordinator's waiting messages, with their actions. */
export function useOutboxPrompts(cid: string, outbox: Record<string, OutboxMessage> | undefined, items: TranscriptItem[], local: PendingMessage[], agent: string) {
  const [err, setErr] = useState<string | null>(null);
  const [acting, setActing] = useState<string | null>(null);
  const waiting = useMemo(() => waitingMessages(outbox, items, local), [outbox, items, local]);

  const act = async (m: OutboxMessage, run: () => Promise<OutboxMessage>) => {
    setErr(null);
    setActing(m.id);
    try {
      noteOutbox(await run());
      return true;
    } catch (e) {
      setErr(errText(e));
      // What the daemon holds now, for a message that moved on meanwhile.
      api.outbox(cid).then((list) => list.forEach(noteOutbox)).catch(() => {});
      return false;
    } finally {
      setActing(null);
    }
  };

  const prompts = useMemo<PendingPrompt[]>(() => {
    const after = lastId(items);
    return waiting.map((m): PendingPrompt => {
      const busy = acting === m.id;
      const link = (label: string, testId: string, run: () => Promise<OutboxMessage>) => (
        <> · <button type="button" className="tx-link" data-testid={testId} disabled={busy} onClick={() => void act(m, run)}>{label}</button></>
      );
      const cancel = link("Cancel", "queued-cancel", () => api.cancelQueued(cid, m.id));
      if (m.state === "queued") {
        return {
          after, text: m.text, note: `Queued: the ${agent} reads it when this step ends`,
          onEdit: (text) => act(m, () => api.editQueued(cid, m.id, text)),
          actions: <>{link("Send now", "queued-send-now", () => api.sendQueuedNow(cid, m.id))}{cancel}</>,
        };
      }
      if (m.state === "failed") {
        return {
          after, text: m.text, tone: "error", note: `Not delivered${m.error ? `: ${m.error}` : ""}`,
          actions: <>{link("Retry", "queued-retry", () => api.sendQueuedNow(cid, m.id))}{cancel}</>,
        };
      }
      return { after, text: m.text, note: "Sending…" };
    });
    // `act` only closes over setters and `cid`.
  }, [waiting, items, acting, cid, agent]);

  return { prompts, err, waiting };
}
