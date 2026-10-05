import { describe, expect, it } from "vitest";
import type { MemoryEntry, MemoryProposal, UserMemoryEntry } from "./api";
import { pendingProposals, prLabel, promotedCopy, sortEntries } from "./memory";

const evidence = { files: [] };
const proposal = (id: string, over: Partial<MemoryProposal> = {}): MemoryProposal => ({
  id, project_id: "p1", text: id, evidence, source: "worker", state: "proposed", proposed_at: "2026-10-02T10:00:00Z", ...over,
});
const entry = (id: string, over: Partial<MemoryEntry> = {}): MemoryEntry => ({
  id, project_id: "p1", path: `memory/${id}.md`, text: id, evidence, ...over,
});

describe("pendingProposals", () => {
  it("lists one Project's undecided proposals, oldest first", () => {
    const all = [
      proposal("b", { proposed_at: "2026-10-02T11:00:00Z" }), proposal("a"), proposal("c", { state: "rejected" }),
      proposal("d", { state: "accepted" }), proposal("e", { project_id: "p2" }),
    ];
    expect(pendingProposals(all, "p1").map((m) => m.id)).toEqual(["a", "b"]);
  });
});

describe("sortEntries", () => {
  it("puts the newest entry first and undated files last", () => {
    const list = [entry("by-hand"), entry("old", { accepted_at: "2026-10-01T00:00:00Z" }), entry("new", { date: "2026-10-03T00:00:00Z" })];
    expect(sortEntries(list).map((e) => e.id)).toEqual(["new", "old", "by-hand"]);
  });
});

describe("promotedCopy", () => {
  it("matches the Project and the entry, not the file name", () => {
    const shared: UserMemoryEntry[] = [
      { id: "x", path: "/m/x.md", text: "x", evidence, project_id: "p2", entry_id: "x" },
      { id: "x-2", path: "/m/x-2.md", text: "x", evidence, project_id: "p1", entry_id: "x" },
      { id: "by-hand", path: "/m/by-hand.md", text: "note", evidence },
    ];
    expect(promotedCopy(shared, entry("x"))?.id).toBe("x-2");
    expect(promotedCopy(shared, entry("by-hand"))).toBeUndefined();
  });
});

describe("prLabel", () => {
  it("shortens forge pull request URLs", () => {
    expect(prLabel("https://github.com/quark-systems/quark/pull/2")).toBe("quark-systems/quark#2");
    expect(prLabel("https://example.com/review/7")).toBe("https://example.com/review/7");
  });
});
