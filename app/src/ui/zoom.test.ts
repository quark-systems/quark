import { describe, expect, it } from "vitest";
import { clampPan, fitScale, fitView, MAX_SCALE, zoomStep, zoomTo, zoomWheel } from "./zoom";

const vp = { w: 1048, h: 848 };

describe("image preview zoom", () => {
  it("fits a large image down and a small one up", () => {
    expect(fitScale({ w: 2000, h: 400 }, vp)).toBeCloseTo(0.5);
    expect(fitScale({ w: 32, h: 32 }, vp)).toBeCloseTo(25);
    expect(fitScale({ w: 0, h: 0 }, vp)).toBe(1);
    expect(fitView({ w: 2000, h: 400 }, vp)).toEqual({ scale: 0.5, x: 0, y: 0 });
  });

  it("keeps the point under the cursor fixed while zooming", () => {
    const v = { scale: 1, x: 10, y: -20 };
    const at = { x: 200, y: 100 };
    const z = zoomTo(v, 2, 1, at);
    // The image point under `at`, in image pixels, before and after.
    expect((at.x - z.x) / z.scale).toBeCloseTo((at.x - v.x) / v.scale);
    expect((at.y - z.y) / z.scale).toBeCloseTo((at.y - v.y) / v.scale);
  });

  it("clamps between half of fit-or-100% and the maximum", () => {
    expect(zoomTo({ scale: 1, x: 0, y: 0 }, 1000, 0.5).scale).toBe(MAX_SCALE);
    expect(zoomTo({ scale: 1, x: 0, y: 0 }, 0.01, 0.5).scale).toBe(0.25);
    expect(zoomTo({ scale: 1, x: 0, y: 0 }, 0.01, 25).scale).toBe(0.5);
  });

  it("steps and wheels in both directions", () => {
    const v = { scale: 1, x: 0, y: 0 };
    expect(zoomStep(v, 1, 1).scale).toBeCloseTo(1.25);
    expect(zoomStep(zoomStep(v, 1, 1), -1, 1).scale).toBeCloseTo(1);
    expect(zoomWheel(v, -100, 1, { x: 0, y: 0 }).scale).toBeGreaterThan(1);
    expect(zoomWheel(v, 100, 1, { x: 0, y: 0 }).scale).toBeLessThan(1);
    // A huge mouse-wheel delta is one bounded step, not a jump to the limit.
    expect(zoomWheel(v, 5000, 1, { x: 0, y: 0 }).scale).toBeCloseTo(zoomWheel(v, 100, 1, { x: 0, y: 0 }).scale);
  });

  it("never pans the image fully off screen", () => {
    const img = { w: 400, h: 200 };
    const p = clampPan({ scale: 1, x: 99999, y: -99999 }, img, vp);
    expect(p.x).toBe((400 + 1048) / 2 - 48);
    expect(p.y).toBe(-((200 + 848) / 2 - 48));
    expect(clampPan({ scale: 1, x: 5, y: 5 }, img, vp)).toEqual({ scale: 1, x: 5, y: 5 });
  });
});
