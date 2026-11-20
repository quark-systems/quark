import { describe, expect, it } from "vitest";
import { harnessMark } from "./harnessMarks";

describe("harnessMark", () => {
  it("resolves ids and the manifests' aliases to the same mark", () => {
    expect(harnessMark("claude")).toBe(harnessMark("claude-code"));
    expect(harnessMark("cursor").name).toBe("Cursor Agent");
    expect(harnessMark("agy").name).toBe("Antigravity CLI");
    expect(harnessMark("Claude-Code").path).toBeTruthy();
  });

  it("gives every shipped harness a vendor mark", () => {
    for (const id of ["claude-code", "codex", "cursor-agent", "gemini", "kimi", "opencode", "pi", "pi-signed",
      "grok", "bob", "rovo", "kiro", "antigravity", "omp", "muse"]) {
      const m = harnessMark(id);
      expect(Boolean(m.path || m.src), id).toBe(true);
      expect(m.monogram, id).toBeUndefined();
    }
    expect(harnessMark("codex").viewBox).toBe("180 180 356 356");
    expect(harnessMark("kiro").src).toMatch(/kiro\.svg|^data:image\/svg/);
  });

  it("draws unknown harnesses as a monogram", () => {
    expect(harnessMark("my-agent")).toEqual({ name: "my-agent", monogram: "MA" });
    expect(harnessMark("zed")).toEqual({ name: "zed", monogram: "Ze" });
  });
});
