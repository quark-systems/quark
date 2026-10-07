import { describe, expect, it } from "vitest";
import type { TranscriptItem } from "../../api";
import { lastId, PendingMessage, stillPending } from "./pending";

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
});
