// An image file in a diff: the added image, the removed one, or before and after side by side.
// Bytes come from the daemon as blobs and show in <img>, so an SVG renders as a picture and its
// markup never reaches the page.
import { useEffect, useState } from "react";
import { externalLink } from "../../links";
import { DiffFile, imageSides } from "./model";

/** Where each version of a file is served, and where to open one that is too large to show. */
export interface FileSource {
  url: (path: string, side: "base" | "head") => string;
  /** A page that shows the file outside the app, if there is one. */
  external?: (path: string, side: "base" | "head") => string | null;
}

export function ImageDiff({ f, source }: { f: DiffFile; source: FileSource }) {
  const sides = imageSides(f);
  const both = sides.base && sides.head;
  return (
    <div className={"dv-images" + (both ? " both" : "")} data-testid="image-diff">
      {sides.base && <Side label={both ? "Before" : "Removed"} path={sides.base} side="base" source={source} />}
      {sides.head && <Side label={both ? "After" : "Added"} path={sides.head} side="head" source={source} />}
    </div>
  );
}

type Loaded =
  | { state: "loading" }
  | { state: "ok"; src: string; size: number }
  | { state: "too_large"; message: string }
  | { state: "error"; message: string };

function Side({ label, path, side, source }: { label: string; path: string; side: "base" | "head"; source: FileSource }) {
  const [img, setImg] = useState<Loaded>({ state: "loading" });
  const [dims, setDims] = useState<string | null>(null);
  const url = source.url(path, side);

  useEffect(() => {
    let live = true;
    let objectUrl: string | null = null;
    setImg({ state: "loading" });
    (async () => {
      try {
        const res = await fetch(url);
        if (!res.ok) {
          const body = await res.json().catch(() => null);
          const message = body?.error?.message ?? `The daemon answered ${res.status}`;
          if (live) setImg(res.status === 413 ? { state: "too_large", message } : { state: "error", message });
          return;
        }
        const blob = await res.blob();
        objectUrl = URL.createObjectURL(blob);
        if (live) setImg({ state: "ok", src: objectUrl, size: blob.size });
      } catch (e) {
        if (live) setImg({ state: "error", message: String(e) });
      }
    })();
    return () => { live = false; if (objectUrl) URL.revokeObjectURL(objectUrl); };
  }, [url]);

  const external = source.external?.(path, side) ?? null;
  return (
    <figure className={"dv-image " + side} data-testid={`image-${side}`}>
      <figcaption>
        <span className={side === "head" ? "adds" : "dels"}>{label}</span>
        {img.state === "ok" && <span className="faint">{[dims, formatBytes(img.size)].filter(Boolean).join(" · ")}</span>}
      </figcaption>
      <div className="dv-image-frame">
        {img.state === "loading" && <span className="faint">Loading…</span>}
        {img.state === "ok" && (
          <img src={img.src} alt={`${path}, ${label.toLowerCase()}`}
            onLoad={(e) => setDims(`${e.currentTarget.naturalWidth} × ${e.currentTarget.naturalHeight}`)} />
        )}
        {img.state === "too_large" && (
          <span className="faint" data-testid="image-too-large">
            Too large to show here.{" "}
            {external && <a {...externalLink(external)}>Open it</a>}
          </span>
        )}
        {img.state === "error" && <span className="bad small-text" role="alert">{img.message}</span>}
      </div>
    </figure>
  );
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}
