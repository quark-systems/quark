import { describe, expect, it } from "vitest";
import { bytes, pct, share, sparkPoints, tail } from "./format";

describe("host formatting", () => {
  it("formats bytes in binary units", () => {
    expect(bytes(0)).toBe("0 B");
    expect(bytes(1536)).toBe("1.5 KB");
    expect(bytes(12 * 1024 ** 3)).toBe("12 GB");
    expect(bytes(null)).toBe("–");
  });
  it("formats shares as percents", () => {
    expect(pct(0.256)).toBe("26%");
    expect(pct(0.004)).toBe("<1%");
    expect(pct(0)).toBe("0%");
    expect(pct(undefined)).toBe("–");
    expect(share(5, 10)).toBe(0.5);
    expect(share(5, 0)).toBeNull();
    expect(share(20, 10)).toBe(1);
  });
  it("draws spark points across the box", () => {
    expect(sparkPoints([], 100, 20)).toBe("");
    expect(sparkPoints([0, 1], 100, 20)).toBe("0.0,20.0 100.0,0.0");
    expect(sparkPoints([0.5], 100, 20, 1)).toBe("0.0,10.0");
    expect(sparkPoints([0, 0], 100, 20)).toBe("0.0,20.0 100.0,20.0");
  });
  it("keeps a path's last segment", () => {
    expect(tail("/a/b/wt-3/")).toBe("wt-3");
  });
});
