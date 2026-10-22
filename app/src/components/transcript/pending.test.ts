import { describe, expect, it } from "vitest";
import type { TranscriptItem } from "../../api";
import { lastId, PendingCommand, PendingMessage, stillPending, stillPendingCommands } from "./pending";

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
