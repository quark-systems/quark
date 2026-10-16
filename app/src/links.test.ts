import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { externalLink, openExternal, webUrl } from "./links";

// The unit tests run in node: a bare window stands in for the browser's.
let window: { open: ReturnType<typeof vi.fn>; __quarkOpenUrl?: (u: string) => Promise<void> };

describe("external links", () => {
  beforeEach(() => {
    window = { open: vi.fn() };
    vi.stubGlobal("window", window);
  });
  afterEach(() => { vi.unstubAllGlobals(); });

  it("opens only http and https addresses", () => {
    expect(webUrl("https://github.com/a/b/pull/1")).toBe("https://github.com/a/b/pull/1");
    expect(webUrl("http://localhost:3000/x")).toBe("http://localhost:3000/x");
    for (const bad of ["javascript:alert(1)", "file:///etc/passwd", "mailto:a@b.c", "/v1/x", "", null, undefined]) {
      expect(webUrl(bad)).toBeNull();
    }
  });

  it("goes through the desktop shell's opener when there is one", async () => {
    const seen: string[] = [];
    window.__quarkOpenUrl = async (u: string) => { seen.push(u); };
    expect(await openExternal("https://github.com/a/b/pull/7")).toBe(true);
    expect(await openExternal("javascript:alert(1)")).toBe(false);
    expect(seen).toEqual(["https://github.com/a/b/pull/7"]);
  });

  it("opens a new tab in a plain browser", async () => {
    await openExternal("https://github.com/a/b/pull/7");
    expect(window.open).toHaveBeenCalledWith("https://github.com/a/b/pull/7", "_blank", "noopener,noreferrer");
  });

  it("handles a plain click and leaves modified clicks to the browser", async () => {
    const seen: string[] = [];
    window.__quarkOpenUrl = async (u: string) => { seen.push(u); };
    const p = externalLink("https://github.com/a/b/pull/7");
    expect(p.href).toBe("https://github.com/a/b/pull/7");
    const click = (mods: Partial<{ metaKey: boolean; ctrlKey: boolean; shiftKey: boolean; button: number }> = {}) => {
      const e = { preventDefault: vi.fn(), metaKey: false, ctrlKey: false, shiftKey: false, button: 0, ...mods };
      p.onClick(e);
      return e.preventDefault.mock.calls.length;
    };
    expect(click()).toBe(1);
    expect(click({ metaKey: true })).toBe(0);
    expect(click({ button: 1 })).toBe(0);
    await Promise.resolve();
    expect(seen).toEqual(["https://github.com/a/b/pull/7"]);
  });
});
