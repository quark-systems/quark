import { beforeEach, describe, expect, it } from "vitest";
import { isImage, pastedName, readAttached, withAttachments, writeAttached } from "./attachments";

class MemoryStorage {
  m = new Map<string, string>();
  getItem(k: string) { return this.m.get(k) ?? null; }
  setItem(k: string, v: string) { this.m.set(k, v); }
  removeItem(k: string) { this.m.delete(k); }
}

describe("attachments", () => {
  let store: MemoryStorage;
  beforeEach(() => { store = new MemoryStorage(); (globalThis as { localStorage?: unknown }).localStorage = store; });

  it("names each attached file after the text", () => {
    expect(withAttachments("  look at this ", [{ path: "/q/a/1/shot.png" }])).toBe("look at this\n\nAttached file:\n- /q/a/1/shot.png");
    expect(withAttachments("", [{ path: "/a" }, { path: "/b" }])).toBe("Attached files:\n- /a\n- /b");
    expect(withAttachments(" hi ", [])).toBe("hi");
  });
  it("gives pasted images a name with the time", () => {
    const at = new Date(2026, 9, 29, 21, 5, 9);
    expect(pastedName({ name: "image.png", type: "image/png" }, at)).toBe("pasted-20261029-210509.png");
    expect(pastedName({ name: "", type: "image/jpeg" }, at)).toBe("pasted-20261029-210509.jpg");
    expect(pastedName({ name: "", type: "image/svg+xml" }, at)).toBe("pasted-20261029-210509.svg");
    expect(pastedName({ name: "crash.log", type: "text/plain" }, at)).toBe("crash.log");
  });
  it("tells images by type or extension", () => {
    expect(isImage("x", "image/png")).toBe(true);
    expect(isImage("Shot.JPG")).toBe(true);
    expect(isImage("notes.md", "text/markdown")).toBe(false);
  });
  it("keeps a list per box, without thumbnails, and drops an empty one", () => {
    writeAttached("worker:t1", [{ path: "/a", name: "a.png", bytes: 3, image: true, preview: "blob:x" }]);
    expect(readAttached("worker:t1")).toEqual([{ path: "/a", name: "a.png", bytes: 3, image: true, preview: undefined }]);
    expect(store.m.get("quark.attachments.worker:t1")).not.toContain("blob:");
    expect(readAttached("worker:t2")).toEqual([]);
    writeAttached("worker:t1", []);
    expect(store.m.size).toBe(0);
  });
});
