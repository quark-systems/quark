import { describe, expect, it } from "vitest";
import * as ui from ".";
import { CATALOGUE } from "./catalogue";
import { taskTone } from "./tone";

describe("component catalogue", () => {
  it("has exactly one entry per shared component", () => {
    const components = Object.entries(ui).filter(([n, v]) => typeof v === "function" && /^[A-Z]/.test(n)).map(([n]) => n).sort();
    expect(CATALOGUE.map((e) => e.name).sort()).toEqual(components);
  });
  it("says when to use each part and what it takes", () => {
    for (const e of CATALOGUE) { expect(e.when.length).toBeGreaterThan(10); expect(e.contract.length).toBeGreaterThan(5); }
  });
});

describe("taskTone", () => {
  it("maps worker states to the glossary's state words", () => {
    expect(taskTone("running")).toBe("busy");
    expect(taskTone("needs_decision")).toBe("needs-you");
    expect(taskTone("in_review")).toBe("ready");
    expect(taskTone("failed")).toBe("failed");
    expect(taskTone("paused")).toBe("parked");
    expect(taskTone("done")).toBe("idle");
  });
});
