import { describe, expect, it } from "vitest";
import { shouldOpenSetup } from "./firstRun";
import type { SetupReport } from "./api";

const report = (ready: boolean): SetupReport => ({ ready, checks: [] });

describe("shouldOpenSetup", () => {
  it("opens when checks fail on a starting screen and you never went past it", () => {
    expect(shouldOpenSetup(report(false), null, "")).toBe(true);
    expect(shouldOpenSetup(report(false), null, "#/new")).toBe(true);
  });
  it("stays shut when ready, dismissed, or deep-linked", () => {
    expect(shouldOpenSetup(report(true), null, "")).toBe(false);
    expect(shouldOpenSetup(report(false), "any", "")).toBe(false);
    expect(shouldOpenSetup(report(false), null, "#/t/task-1")).toBe(false);
  });
});
