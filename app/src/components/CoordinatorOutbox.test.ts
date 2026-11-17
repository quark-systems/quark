import { describe, expect, it } from "vitest";
import type { OutboxMessage, TranscriptItem } from "../api";
import { waitingMessages } from "./CoordinatorOutbox";

const msg = (id: string, state: OutboxMessage["state"], text = id, created_at = id): OutboxMessage =>
  ({ id, coordinator_id: "p1", text, state, created_at, updated_at: created_at });
const user = (id: number, text: string, message_id?: string): TranscriptItem =>
  ({ id, role: "user", text, is_error: false, truncated: false, ts: null, message_id });

describe("waitingMessages", () => {
  it("shows queued, sending and failed messages the transcript has not recorded, oldest first", () => {
    const outbox = Object.fromEntries([
      msg("b", "queued"), msg("a", "failed"), msg("c", "sent"), msg("d", "cancelled"), msg("e", "sending"), msg("f", "unconfirmed"),
    ].map((m) => [m.id, m]));
    expect(waitingMessages(outbox, []).map((m) => m.id)).toEqual(["a", "b", "e"]);
    // Once the transcript records one, it shows there instead.
    expect(waitingMessages(outbox, [user(4, "e", "e")]).map((m) => m.id)).toEqual(["a", "b"]);
    expect(waitingMessages(undefined, [])).toEqual([]);
  });

  it("leaves a message this window is still sending to its own bubble", () => {
    const outbox = { s: msg("s", "sending", "fix  the\nparser") };
    const local = [{ key: 1, text: "fix the\nparser", sending: true, confirmed: false, after: 0 }];
    expect(waitingMessages(outbox, [], local)).toEqual([]);
    expect(waitingMessages(outbox, [], [{ ...local[0], sending: false }]).map((m) => m.id)).toEqual(["s"]);
  });

  it("leaves a command's note to its card", () => {
    const outbox = { n: msg("n", "sending", "Engine note: The user asked for a status report.") };
    expect(waitingMessages(outbox, [])).toEqual([]);
  });
});
