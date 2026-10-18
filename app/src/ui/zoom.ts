// Zoom and pan arithmetic for ImagePreview, kept apart from the component so it can be tested.
// A view is a scale plus the offset of the image's centre from the viewport's centre, in pixels.

export interface View { scale: number; x: number; y: number }
export interface Size { w: number; h: number }

export const MAX_SCALE = 32;
const STEP = 1.25;

/** The scale that shows the whole image inside the viewport, leaving `pad` pixels around it. */
export function fitScale(image: Size, viewport: Size, pad = 24): number {
  if (image.w <= 0 || image.h <= 0) return 1;
  const s = Math.min((viewport.w - 2 * pad) / image.w, (viewport.h - 2 * pad) / image.h);
  return s > 0 ? s : 1;
}

/** The smallest scale allowed: half of fit or of 100%, whichever is smaller. */
export function minScale(fit: number): number {
  return Math.min(fit, 1) / 2;
}

export function clampScale(s: number, fit: number): number {
  return Math.min(MAX_SCALE, Math.max(minScale(fit), s));
}

/** The view that shows the whole image, centred. */
export function fitView(image: Size, viewport: Size): View {
  return { scale: fitScale(image, viewport), x: 0, y: 0 };
}

/**
 * Zoom to `scale`, keeping the image point under `at` (relative to the viewport's centre) where it is.
 * Without `at` the zoom is about the viewport's centre.
 */
export function zoomTo(v: View, scale: number, fit: number, at: { x: number; y: number } = { x: 0, y: 0 }): View {
  const s = clampScale(scale, fit);
  const k = s / v.scale;
  return { scale: s, x: at.x - (at.x - v.x) * k, y: at.y - (at.y - v.y) * k };
}

/** One zoom step in (`dir` 1) or out (`dir` -1). */
export function zoomStep(v: View, dir: 1 | -1, fit: number, at?: { x: number; y: number }): View {
  return zoomTo(v, v.scale * (dir > 0 ? STEP : 1 / STEP), fit, at);
}

/** Zoom for a wheel or trackpad pinch event's deltaY; a pinch arrives as small deltas. */
export function zoomWheel(v: View, deltaY: number, fit: number, at: { x: number; y: number }): View {
  const d = Math.max(-100, Math.min(100, deltaY));
  return zoomTo(v, v.scale * Math.exp(-d / 300), fit, at);
}

/**
 * Keep at least part of the image on screen after a pan: its centre may move no further than
 * half the image plus half the viewport, less a 48px sliver that stays visible.
 */
export function clampPan(v: View, image: Size, viewport: Size): View {
  const keep = 48;
  const lim = (img: number, vp: number) => Math.max(0, (img * v.scale + vp) / 2 - keep);
  const lx = lim(image.w, viewport.w), ly = lim(image.h, viewport.h);
  return { scale: v.scale, x: Math.max(-lx, Math.min(lx, v.x)), y: Math.max(-ly, Math.min(ly, v.y)) };
}
