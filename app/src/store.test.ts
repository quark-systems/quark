import { describe, expect, it } from "vitest";
import { AppState, applyEvent, initialState } from "./store";
import type { DaemonEvent, Task } from "./api";

const task = (over: Partial<Task> = {}): Task => ({
  id: "t1", project_id: "p1", title: "Fix the parser", state: "queued",
  created_at: "2026-10-02T10:00:00Z", updated_at: "2026-10-02T10:00:00Z", ...over,
});
const ev = (seq: number, type: string, payload: unknown, project_id: string | null = "p1"): DaemonEvent =>
  ({ seq, type, payload, project_id, ts: "2026-10-02T10:00:00Z" });

describe("applyEvent", () => {
  it("upserts tasks from created and state_changed events", () => {
    let s = applyEvent(initialState, ev(1, "task.created", task()));
    s = applyEvent(s, ev(2, "task.state_changed", task({ state: "running", state_note: "started" })));
    expect(s.tasks.t1.state).toBe("running");
    expect(s.tasks.t1.state_note).toBe("started");
    expect(Object.keys(s.tasks)).toEqual(["t1"]);
  });

  it("is idempotent when an event is replayed", () => {
    const e = ev(1, "task.created", task());
    const once = applyEvent(initialState, e);
    expect(applyEvent(once, e).tasks).toEqual(once.tasks);
  });

  it("merges partial project updates", () => {
    let s = applyEvent(initialState, ev(1, "project.updated", { id: "p1", name: "P", created_at: "", updated_at: "" }));
    s = applyEvent(s, ev(2, "project.updated", { id: "p1", coordinator_state: "running" }));
    expect(s.projects.p1.name).toBe("P");
    expect(s.projects.p1.coordinator_state).toBe("running");
  });

  it("appends coordinator messages only to a loaded chat, without duplicates", () => {
    const m = { id: "m1", ts: "2026-10-02T10:00:00Z", role: "coordinator", text: "hi" };
    expect(applyEvent(initialState, ev(1, "coordinator.message", m)).chat).toEqual({});
    let s: AppState = { ...initialState, chat: { p1: [] } };
    s = applyEvent(s, ev(1, "coordinator.message", m));
    s = applyEvent(s, ev(1, "coordinator.message", m));
    expect(s.chat.p1).toHaveLength(1);
  });

  it("routes coordinator messages by coordinator_id when the payload carries one", () => {
    const s = applyEvent({ ...initialState, chat: { cc: [] } },
      ev(1, "coordinator.message", { id: "m1", ts: "", role: "user", text: "x", coordinator_id: "cc" }, null));
    expect(s.chat.cc[0]).toEqual({ id: "m1", ts: "", role: "user", text: "x" });
  });

  it("appends transcript entries to a loaded transcript", () => {
    const entry = { id: "e1", ts: "", role: "assistant", text: "working" };
    expect(applyEvent(initialState, ev(1, "worker.transcript", { task_id: "t1", entry })).transcripts).toEqual({});
    const s = applyEvent({ ...initialState, transcripts: { t1: [] } }, ev(1, "worker.transcript", { task_id: "t1", entry }));
    expect(s.transcripts.t1).toEqual([entry]);
  });

  it("leaves state untouched for events it does not render", () => {
    expect(applyEvent(initialState, ev(1, "dispatch.recorded", {}))).toBe(initialState);
  });
});
