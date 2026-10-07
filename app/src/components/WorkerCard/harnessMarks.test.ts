import { describe, expect, it } from "vitest";
import { harnessMark } from "./harnessMarks";

describe("harnessMark", () => {
  it("resolves ids and the manifests' aliases to the same mark", () => {
    expect(harnessMark("claude")).toBe(harnessMark("claude-code"));
    expect(harnessMark("cursor").name).toBe("Cursor Agent");
    expect(harnessMark("agy").name).toBe("Antigravity CLI");
    expect(harnessMark("Claude-Code").path).toBeTruthy();
  });

  it("draws harnesses without a published mark as a monogram", () => {
    expect(harnessMark("codex")).toMatchObject({ name: "Codex", monogram: "Cx" });
    expect(harnessMark("my-agent")).toEqual({ name: "my-agent", monogram: "MA" });
    expect(harnessMark("zed")).toEqual({ name: "zed", monogram: "Ze" });
  });
});
