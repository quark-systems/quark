// A full-screen preview for one image or a set (before and after), with zoom and pan.
// Wheel or pinch zooms about the cursor, drag pans, double-click toggles fit and 100%.
// Keys: + and - zoom, 0 fits, 1 is 100%, left and right switch images, Esc closes.
import React, { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Dialog } from "radix-ui";
import { Button } from "./button";
import { cx } from "./cx";
import { clampPan, fitView, Size, View, zoomStep, zoomTo, zoomWheel } from "./zoom";

export interface PreviewImage {
  src: string;
  /** A short name shown in the switcher and the title, e.g. Before. */
  label: string;
  alt: string;
}

// An SVG without width and height has no natural size; show it at the size browsers default to.
const NO_SIZE: Size = { w: 300, h: 150 };

export function ImagePreview({ images, initial = 0, title, open, onOpenChange }: {
  images: PreviewImage[];
  initial?: number;
  /** Names the dialog, e.g. the file's path. */
  title: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [index, setIndex] = useState(initial);
  useEffect(() => { if (open) setIndex(initial); }, [open, initial]);
  const image = images[Math.min(index, images.length - 1)];

  const viewport = useRef<HTMLDivElement | null>(null);
  const [vp, setVp] = useState<Size>({ w: 0, h: 0 });
  const [natural, setNatural] = useState<Size | null>(null);
  const [view, setView] = useState<View>({ scale: 1, x: 0, y: 0 });
  // While true the image follows the viewport's size and refits when it switches.
  const [fitted, setFitted] = useState(true);
  const fit = natural && vp.w ? fitView(natural, vp).scale : 1;

  const [el, setEl] = useState<HTMLDivElement | null>(null);
  const setViewport = useCallback((node: HTMLDivElement | null) => { viewport.current = node; setEl(node); }, []);
  useLayoutEffect(() => {
    if (!el) return;
    const measure = () => setVp({ w: el.clientWidth, h: el.clientHeight });
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [el]);

  useEffect(() => { setNatural(null); }, [image?.src]);
  useEffect(() => { if (fitted && natural && vp.w) setView(fitView(natural, vp)); }, [fitted, natural, vp]);

  const apply = useCallback((next: View) => {
    setFitted(false);
    setView(natural ? clampPan(next, natural, vp) : next);
  }, [natural, vp]);

  // The cursor's position relative to the viewport's centre.
  const fromCentre = (clientX: number, clientY: number) => {
    const r = viewport.current!.getBoundingClientRect();
    return { x: clientX - r.left - r.width / 2, y: clientY - r.top - r.height / 2 };
  };

  // React's wheel listener is passive, so the page would scroll too; listen natively instead.
  const wheel = useRef<(e: WheelEvent) => void>(() => {});
  wheel.current = (e) => { e.preventDefault(); apply(zoomWheel(view, e.deltaY * (e.deltaMode === 1 ? 33 : 1), fit, fromCentre(e.clientX, e.clientY))); };
  useEffect(() => {
    if (!el) return;
    const on = (e: WheelEvent) => wheel.current(e);
    el.addEventListener("wheel", on, { passive: false });
    return () => el.removeEventListener("wheel", on);
  }, [el]);

  const drag = useRef<{ id: number; x: number; y: number; from: View } | null>(null);
  const [dragging, setDragging] = useState(false);

  const showFit = () => setFitted(true);
  const showActual = (at?: { x: number; y: number }) => apply(zoomTo(view, 1, fit, at));
  const atFit = fitted || Math.abs(view.scale - fit) < 1e-3;

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const k = e.key;
    if (k === "+" || k === "=") apply(zoomStep(view, 1, fit));
    else if (k === "-" || k === "_") apply(zoomStep(view, -1, fit));
    else if (k === "0") showFit();
    else if (k === "1") showActual();
    else if (k === "ArrowLeft" && images.length > 1) setIndex((i) => (i + images.length - 1) % images.length);
    else if (k === "ArrowRight" && images.length > 1) setIndex((i) => (i + 1) % images.length);
    else return;
    e.preventDefault();
  };

  if (!image) return null;
  const size = natural ?? NO_SIZE;
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-[var(--scrim)] backdrop-blur-[2px]" />
        <Dialog.Content data-testid="image-preview" aria-describedby={undefined} onKeyDown={onKeyDown}
          className="fixed inset-4 z-50 flex flex-col overflow-hidden rounded-l bg-popover text-fg shadow-popover outline-none">
          <div className="flex shrink-0 items-center gap-2 border-b border-line px-3 py-2">
            <Dialog.Title className="m-0 min-w-0 flex-1 truncate font-sans text-m font-medium">
              {title}
              {images.length > 1 && <span className="font-normal text-dim"> · {image.label}</span>}
              {natural && <span className="ml-2 font-normal text-s text-faint">{natural.w} × {natural.h}</span>}
            </Dialog.Title>
            {images.length > 1 && (
              <div className="flex gap-1" role="group" aria-label="Version">
                {images.map((im, i) => (
                  <Button key={im.label} kind={i === index ? "secondary" : "quiet"} aria-pressed={i === index} onClick={() => setIndex(i)}>{im.label}</Button>
                ))}
              </div>
            )}
            <div className="flex items-center gap-1" role="group" aria-label="Zoom">
              <Button kind="quiet" aria-label="Zoom out" title="Zoom out (−)" onClick={() => apply(zoomStep(view, -1, fit))}>−</Button>
              <span className="w-14 text-center font-mono text-s tabular-nums text-dim" data-testid="image-preview-zoom" aria-live="polite">{Math.round(view.scale * 100)}%</span>
              <Button kind="quiet" aria-label="Zoom in" title="Zoom in (+)" onClick={() => apply(zoomStep(view, 1, fit))}>+</Button>
              <Button kind={atFit ? "secondary" : "quiet"} aria-pressed={atFit} title="Fit to window (0)" onClick={showFit}>Fit</Button>
              <Button kind={!atFit && Math.abs(view.scale - 1) < 1e-3 ? "secondary" : "quiet"} title="Actual size (1)" onClick={() => showActual()}>100%</Button>
            </div>
            <Dialog.Close asChild><Button kind="quiet" aria-label="Close" title="Close (Esc)">✕</Button></Dialog.Close>
          </div>
          <div ref={setViewport} data-testid="image-preview-viewport"
            className={cx("ui-preview-viewport relative min-h-0 flex-1 touch-none select-none overflow-hidden", dragging ? "cursor-grabbing" : "cursor-grab")}
            onPointerDown={(e) => {
              if (e.button !== 0) return;
              e.currentTarget.setPointerCapture(e.pointerId);
              drag.current = { id: e.pointerId, x: e.clientX, y: e.clientY, from: view };
              setDragging(true);
            }}
            onPointerMove={(e) => {
              const d = drag.current;
              if (!d || d.id !== e.pointerId) return;
              apply({ ...d.from, x: d.from.x + e.clientX - d.x, y: d.from.y + e.clientY - d.y });
            }}
            onPointerUp={() => { drag.current = null; setDragging(false); }}
            onPointerCancel={() => { drag.current = null; setDragging(false); }}
            onDoubleClick={(e) => { if (atFit) showActual(fromCentre(e.clientX, e.clientY)); else showFit(); }}>
            <img key={image.src} src={image.src} alt={image.alt} draggable={false}
              onLoad={(e) => { const t = e.currentTarget; setNatural(t.naturalWidth && t.naturalHeight ? { w: t.naturalWidth, h: t.naturalHeight } : NO_SIZE); }}
              // Sized rather than scaled with a transform, so an SVG is redrawn sharp at every zoom.
              style={{
                position: "absolute", left: "50%", top: "50%", maxWidth: "none",
                width: size.w * view.scale, height: size.h * view.scale,
                transform: `translate(calc(-50% + ${view.x}px), calc(-50% + ${view.y}px))`,
                visibility: natural ? "visible" : "hidden",
                imageRendering: view.scale >= 2 ? "pixelated" : "auto",
              }} />
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
