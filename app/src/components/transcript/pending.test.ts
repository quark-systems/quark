import { describe, expect, it } from "vitest";
import type { TranscriptItem } from "../../api";
import { lastId, PendingCommand, PendingMessage, placePending, stillPending, stillPendingCommands } from "./pending";
import { groupTurns } from "./turns";

const user = (id: number, text: string): TranscriptItem => ({ id, role: "user", text, is_error: false, truncated: false });
const sent = (key: number, text: string, after: number): PendingMessage => ({ key, text, confirmed: true, after });

describe("stillPending", () => {
  it("clears a message once the log records it", () => {
    const p = [sent(1, "Add tests", 5)];
    expect(stillPending(p, [user(5, "x")])).toBe(p);
    expect(stillPending(p, [user(5, "x"), user(6, " Add tests\n")])).toEqual([]);
  });

  it("ignores an identical message recorded before this one was sent", () => {
    const items = [user(3, "yes")];
    expect(stillPending([sent(1, "yes", lastId(items))], items)).toHaveLength(1);
  });

  it("needs one entry per message when the same text is sent twice", () => {
    const p = [sent(1, "yes", 3), sent(2, "yes", 3)];
    expect(stillPending(p, [user(4, "yes")]).map((m) => m.key)).toEqual([2]);
    expect(stillPending(p, [user(4, "yes"), user(7, "yes")])).toEqual([]);
  });
  it("matches a multi-line message whatever its line breaks became", () => {
    const p = [sent(1, "Build it.\nThen ship.", 0)];
    expect(stillPending(p, [user(3, "Build it.\r\n\r\nThen ship.\n")])).toEqual([]);
  });
});

describe("stillPendingCommands", () => {
  const cmd = (key: number, command: PendingCommand["command"], after: number): PendingCommand => ({ key, command, sending: false, confirmed: true, after });
  it("clears a command once its note is recorded after it was sent", () => {
    const p = [cmd(1, "catch_up", 5)];
    const note = user(6, "Engine note: The user asked to catch up (…). Their last message to you was at 2026-10-21T10:00:00Z. In…");
    expect(stillPendingCommands(p, [user(4, note.text)])).toBe(p);
    expect(stillPendingCommands(p, [user(6, "hello")])).toBe(p);
    expect(stillPendingCommands(p, [note])).toEqual([]);
    expect(stillPendingCommands([cmd(1, "wrap_up", 5)], [note])).toHaveLength(1);
  });
});

describe("placePending", () => {
  const reply = (id: number, text: string): TranscriptItem => ({ id, role: "assistant", text, is_error: false, truncated: false });

  it("keeps a sent message above the replies recorded after it, even when the log never records it", () => {
    // The coordinator answered "Start on the do next list", but its harness log
    // did not record the message (Codex 0.169): it must not drop below the replies.
    const items = [user(1, "Earlier"), reply(2, "Done."), reply(5, "Starting the refresh."), reply(9, "Started both tasks.")];
    const { items: merged, notes } = placePending(items, [{ after: 2, text: "Start on the do next list", note: "sent" }]);
    expect(merged.map((i) => i.text)).toEqual(["Earlier", "Done.", "Start on the do next list", "Starting the refresh.", "Started both tasks."]);
    const turns = groupTurns(merged);
    expect(turns).toHaveLength(2);
    expect(turns[1].prompt?.text).toBe("Start on the do next list");
    expect(notes.get(turns[1].prompt!.id)?.note).toBe("sent");
  });

  it("keeps messages sent at the same point in the order they were sent", () => {
    const { items: merged } = placePending([reply(4, "Hi")], [
      { after: 4, text: "one", note: "sent" },
      { after: 4, text: "two", note: "sending…" },
    ]);
    expect(merged.map((i) => i.text)).toEqual(["Hi", "one", "two"]);
  });

  it("keeps a command asked for above the replies after it, as its card", () => {
    // /status while the coordinator answered something else: the card belongs where it was asked.
    const card = "status card";
    const { items: merged, notes } = placePending([user(1, "Earlier"), reply(2, "Done."), reply(5, "Steered the worker.")],
      [{ after: 2, text: "Status report", note: null, card }]);
    expect(merged.map((i) => i.text)).toEqual(["Earlier", "Done.", "Status report", "Steered the worker."]);
    const turns = groupTurns(merged);
    expect(notes.get(turns[1].prompt!.id)?.card).toBe(card);
  });

  it("returns the log itself when nothing is pending", () => {
    const items = [user(1, "x")];
    expect(placePending(items, []).items).toBe(items);
  });
});
