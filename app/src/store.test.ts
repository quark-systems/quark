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
  it("merges an account's new quota and ignores accounts not loaded yet", () => {
    const quota = { state: "known" as const, remaining_percent: 40, windows: [], plan: "max", detail: null, checked_at: null };
    const account = {
      id: "acc_2", harness: "claude-code", label: "Work", config_dir: "/a/work", default: false, pools: ["max"],
      health: { state: "configured" as const, detail: "found" }, quota: { ...quota, state: "pending" as const, remaining_percent: null },
      active_tasks: 0, launchable: true,
    };
    const loaded: AppState = { ...initialState, accounts: { acc_2: account }, accountOrder: ["acc_2"], accountsAvailable: true };
    const s = applyEvent(loaded, ev(1, "account.quota_changed", { account_id: "acc_2", harness: "claude-code", quota }, null));
    expect(s.accounts.acc_2.quota.remaining_percent).toBe(40);
    expect(s.accounts.acc_2.label).toBe("Work");
    const other = ev(2, "account.quota_changed", { account_id: "acc_9", harness: "codex", quota }, null);
    expect(applyEvent(s, other)).toBe(s);
  });

  it("upserts tasks from created and state_changed events", () => {
    let s = applyEvent(initialState, ev(1, "task.created", task()));
    s = applyEvent(s, ev(2, "task.state_changed", {
      task: task({ state: "running", state_note: "started" }), previous_state: "queued",
    }));
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
    s = applyEvent(s, ev(2, "project.updated", { id: "p1", status: "ready" }));
    expect(s.projects.p1.name).toBe("P");
    expect(s.projects.p1.status).toBe("ready");
  });

  const entry = { role: "assistant", text: "working", is_error: false, truncated: false, ts: null };

  it("appends coordinator entries only to a loaded chat, keyed by the event seq", () => {
    expect(applyEvent(initialState, ev(7, "coordinator.message", { coordinator_id: "p1", entry })).chat).toEqual({});
    let s: AppState = { ...initialState, chat: { p1: [] } };
    s = applyEvent(s, ev(7, "coordinator.message", { coordinator_id: "p1", entry }));
    s = applyEvent(s, ev(7, "coordinator.message", { coordinator_id: "p1", entry }));
    expect(s.chat.p1).toEqual([{ ...entry, id: 7 }]);
  });

  it("appends worker transcript entries to a loaded transcript", () => {
    expect(applyEvent(initialState, ev(3, "worker.transcript", { task_id: "t1", entry })).transcripts).toEqual({});
    const s = applyEvent({ ...initialState, transcripts: { t1: [] } }, ev(3, "worker.transcript", { task_id: "t1", entry }));
    expect(s.transcripts.t1).toEqual([{ ...entry, id: 3 }]);
  });

  it("counts task events so views can refetch", () => {
    let s = applyEvent(initialState, ev(1, "task.event", { id: 1, task_id: "t1", kind: "note", note: "", ts: "" }));
    s = applyEvent(s, ev(2, "task.event", { id: 2, task_id: "t1", kind: "note", note: "", ts: "" }));
    expect(s.taskActivity.t1).toBe(2);
  });

  it("appends dispatch records to a task's loaded records", () => {
    const rec = { id: "dsp_1", task_id: "t1", decided_by: "coordinator", recorded_at: "2026-10-02T10:00:00Z" };
    // Not loaded yet: the later fetch includes it.
    expect(applyEvent(initialState, ev(1, "dispatch.recorded", rec))).toBe(initialState);
    let s = applyEvent({ ...initialState, dispatch: { t1: [] } }, ev(1, "dispatch.recorded", rec));
    s = applyEvent(s, ev(2, "dispatch.recorded", rec));
    expect(s.dispatch.t1).toEqual([rec]);
  });

  it("leaves state untouched for events it does not render", () => {
    expect(applyEvent(initialState, ev(1, "some.future_event", {}))).toBe(initialState);
  });
});

describe("decisions", () => {
  const open = { id: "d1", project_id: "p1", question: "Keep history?", state: "open", opened_at: "2026-10-02T10:00:00Z" };
  it("replaces a decision when it is answered", () => {
    let s = applyEvent(initialState, ev(1, "decision.opened", open));
    s = applyEvent(s, ev(2, "decision.answered", { ...open, state: "answered", answer: "Latest only", answered_by: "matt", answered_at: "2026-10-02T10:05:00Z" }));
    expect(s.decisions.d1).toMatchObject({ state: "answered", answer: "Latest only", answered_by: "matt" });
  });
});

describe("memory proposals", () => {
  const proposed = { id: "m1", project_id: "p1", text: "Keep it small.", evidence: { files: [] }, source: "worker", state: "proposed", proposed_at: "2026-10-02T10:00:00Z" };
  it("replaces a proposal when it is decided", () => {
    let s = applyEvent(initialState, ev(1, "memory.proposed", proposed));
    expect(s.memoryProposals.m1.state).toBe("proposed");
    s = applyEvent(s, ev(2, "memory.accepted", { ...proposed, state: "accepted", text: "Keep it small, always.", decided_by: "matt" }));
    expect(s.memoryProposals.m1).toMatchObject({ state: "accepted", text: "Keep it small, always.", decided_by: "matt" });
    s = applyEvent(s, ev(3, "memory.rejected", { ...proposed, id: "m2", state: "rejected" }));
    expect(Object.keys(s.memoryProposals)).toEqual(["m1", "m2"]);
  });
});
