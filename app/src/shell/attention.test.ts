import { describe, expect, it } from "vitest";
import { attentionQueue, nextAttention } from "./attention";

const dec = (id: string, opened_at: string, task_id: string | null = null, state: any = "open") => ({ id, project_id: "p", task_id, question: "Q " + id, state, opened_at });
const pr = (id: string, checks_state: any, updated_at: string, state: any = "open") =>
  ({ id, project_id: "p", repo: "o/r", number: 1, title: "PR " + id, state, checks_state, updated_at, url: "", provider: "github", mergeable: "mergeable", review_decision: "none", checks: [], reviews: [] }) as any;
const task = (id: string, state: any, updated_at: string) => ({ id, project_id: "p", title: "T " + id, state, created_at: "", updated_at });

describe("attentionQueue", () => {
  const q = attentionQueue(
    { d1: dec("d1", "2026-10-08T03:00:00Z", "t2"), d2: dec("d2", "2026-10-08T01:00:00Z", null, "answered") },
    { p1: pr("p1", "failing", "2026-10-08T02:00:00Z"), p2: pr("p2", "passing", "2026-10-08T00:00:00Z"), p3: pr("p3", "failing", "2026-10-08T00:00:00Z", "merged") },
    { t1: task("t1", "failed", "2026-10-08T04:00:00Z"), t2: task("t2", "blocked", "2026-10-08T00:30:00Z"), t3: task("t3", "running", "2026-10-08T00:00:00Z") },
  );
  it("holds open decisions, red open PRs and stuck workers, oldest first", () => {
    expect(q.map((i) => i.key)).toEqual(["pr:p1", "decision:d1", "worker:t1"]);
  });
  it("walks the queue from the item on screen, wrapping to the oldest", () => {
    expect(nextAttention(q, { name: "projects" })?.key).toBe("pr:p1");
    expect(nextAttention(q, { name: "pr", id: "p1" })?.key).toBe("decision:d1");
    expect(nextAttention(q, { name: "task", id: "t1" })?.key).toBe("pr:p1");
    expect(nextAttention([], { name: "projects" })).toBeNull();
  });
});
