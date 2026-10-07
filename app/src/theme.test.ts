import { describe, expect, it } from "vitest";
import { parsePreference, resolveScheme, shellFlags } from "./theme";

describe("theme", () => {
  it("defaults unknown preferences to the system scheme", () => {
    expect(parsePreference(null)).toBe("system");
    expect(parsePreference("sepia")).toBe("system");
    expect(parsePreference("light")).toBe("light");
  });
  it("follows the system only when asked to", () => {
    expect(resolveScheme("system", true)).toBe("light");
    expect(resolveScheme("system", false)).toBe("dark");
    expect(resolveScheme("dark", true)).toBe("dark");
    expect(resolveScheme("light", false)).toBe("light");
  });
  it("reads the desktop shell's flags from the page query", () => {
    expect(shellFlags("?glass=1&platform=macos&daemon=x")).toEqual({ glass: true, mac: true });
    expect(shellFlags("")).toEqual({ glass: false, mac: false });
  });
});
