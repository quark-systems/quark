import { describe, expect, it } from "vitest";
import { updateBanner, updateSummary } from "./appUpdate";
import type { UpdateStatus } from "./api";

const base: UpdateStatus = {
  state: "up_to_date",
  current_version: "0.2.0",
  can_roll_back: false,
  settings: { channel: "stable", check: true, auto_download: true, install_when_idle: false },
};
const offer = { version: "0.3.0", replaces_pulled: false };

describe("updateBanner", () => {
  it("says nothing when up to date", () => {
    expect(updateBanner(base)).toBeNull();
    expect(updateBanner(null)).toBeNull();
  });

  it("offers to install a ready update", () => {
    expect(updateBanner({ ...base, state: "ready", offer })).toEqual({
      text: "Quark 0.3.0 is ready to install.", tone: "ready", action: "install",
    });
    const pulled = updateBanner({ ...base, state: "ready", offer: { ...offer, replaces_pulled: true } });
    expect(pulled?.tone).toBe("needs-you");
    expect(pulled?.text).toContain("Quark 0.2.0, which was withdrawn");
  });

  it("reports an install under way", () => {
    const b = updateBanner({ ...base, state: "installing", last_install: { from: "0.2.0", to: "0.3.0", rollback: false, phase: "swapped", started_at: "t" } });
    expect(b).toEqual({ text: "Installing Quark 0.3.0. Quark restarts in a moment.", tone: "busy" });
  });

  it("reports an automatic rollback once", () => {
    const s: UpdateStatus = {
      ...base,
      last_install: { from: "0.2.0", to: "0.3.0", rollback: false, phase: "rolled_back", started_at: "t1", error: "Quark 0.3.0 didn't start: timeout" },
    };
    expect(updateBanner(s)?.tone).toBe("failed");
    expect(updateBanner(s)?.text).toContain("Quark 0.2.0 was put back");
    expect(updateBanner(s, "t1")).toBeNull();
    // A rollback asked for by hand isn't news.
    expect(updateBanner({ ...s, last_install: { ...s.last_install!, rollback: true, error: undefined } })).toBeNull();
  });
});

describe("updateSummary", () => {
  it("explains each state", () => {
    expect(updateSummary(base)).toBe("Quark 0.2.0 is up to date.");
    expect(updateSummary({ ...base, blocked: "Install 0.3.0 first." })).toBe("Install 0.3.0 first.");
    expect(updateSummary({ ...base, state: "disabled", disabled_reason: "no key" })).toBe("no key");
    expect(updateSummary({ ...base, state: "ready", offer })).toContain("ready to install");
  });
});
