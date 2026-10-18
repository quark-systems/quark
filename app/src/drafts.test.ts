import { beforeEach, describe, expect, it } from "vitest";
import { readDraft, writeDraft } from "./drafts";

class MemoryStorage {
  m = new Map<string, string>();
  getItem(k: string) { return this.m.get(k) ?? null; }
  setItem(k: string, v: string) { this.m.set(k, v); }
  removeItem(k: string) { this.m.delete(k); }
}

describe("drafts", () => {
  let store: MemoryStorage;
  beforeEach(() => { store = new MemoryStorage(); (globalThis as { localStorage?: unknown }).localStorage = store; });

  it("keeps a draft per box", () => {
    writeDraft("coordinator:a", "plan the release");
    writeDraft("worker:t1", "rebase first");
    expect(readDraft("coordinator:a")).toBe("plan the release");
    expect(readDraft("worker:t1")).toBe("rebase first");
    expect(readDraft("coordinator:b")).toBe("");
  });
  it("removes a draft once it is cleared", () => {
    writeDraft("coordinator:a", "hi");
    writeDraft("coordinator:a", "");
    expect(store.m.size).toBe(0);
  });
  it("reads nothing when storage is unavailable", () => {
    delete (globalThis as { localStorage?: unknown }).localStorage;
    expect(readDraft("x")).toBe("");
    expect(() => writeDraft("x", "y")).not.toThrow();
  });
});
