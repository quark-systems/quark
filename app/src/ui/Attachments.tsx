// What goes with a prompt besides its text: a paperclip that opens the file picker, and the files
// already attached, shown as removable chips (images with a thumbnail that opens full screen).
import React, { useRef, useState } from "react";
import { cx } from "./cx";
import { ImagePreview } from "./ImagePreview";

export interface AttachmentChip {
  path: string;
  name: string;
  bytes: number;
  image: boolean;
  /** A thumbnail URL, when the window still has the image. */
  preview?: string;
}

/** A square quiet button with a paperclip; picks one or more files of any type. */
export function AttachButton({ onFiles, disabled, label = "Attach files" }: {
  onFiles: (files: File[]) => void; disabled?: boolean; label?: string;
}) {
  const input = useRef<HTMLInputElement>(null);
  return (
    <>
      <button type="button" aria-label={label} title={`${label} (or paste or drop them here)`} disabled={disabled}
        onClick={() => input.current?.click()}
        className="ui-attach grid size-7 flex-none cursor-pointer place-items-center rounded-m border-0 bg-transparent text-dim transition-colors hover:not-disabled:bg-selection hover:not-disabled:text-fg disabled:cursor-default disabled:opacity-45 focus-visible:outline-2 focus-visible:outline-accent">
        <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
          <path d="M13.5 7.5l-5.6 5.6a3.3 3.3 0 0 1-4.7-4.7l5.9-5.9a2.2 2.2 0 0 1 3.1 3.1l-5.9 5.9a1.1 1.1 0 0 1-1.6-1.6L10.2 4.4"
            fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </button>
      <input ref={input} type="file" multiple hidden aria-hidden="true" tabIndex={-1}
        onChange={(e) => { const files = Array.from(e.target.files ?? []); e.target.value = ""; if (files.length) onFiles(files); }} />
    </>
  );
}

function size(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** The files attached to a prompt, one chip each; renders nothing when there are none and nothing is uploading. */
export function AttachmentList({ items, uploading = 0, error, onRemove, className }: {
  items: AttachmentChip[]; uploading?: number; error?: string | null;
  onRemove: (path: string) => void; className?: string;
}) {
  const [open, setOpen] = useState<number | null>(null);
  const images = items.filter((a) => a.image && a.preview);
  if (!items.length && !uploading && !error) return null;
  return (
    <div className={cx("ui-attachments flex w-full flex-wrap items-center gap-1.5", className)} data-testid="attachments">
      {items.map((a) => {
        const shown = a.image && a.preview ? images.indexOf(a) : -1;
        return (
          <span key={a.path} title={a.path}
            className="inline-flex h-8 max-w-72 items-center gap-1.5 rounded-m border border-line-2 bg-surface-1 py-0.5 pr-0.5 pl-1 text-s text-fg">
            {shown >= 0 ? (
              <button type="button" aria-label={`Preview ${a.name}`} onClick={() => setOpen(shown)}
                className="size-6 flex-none cursor-zoom-in overflow-hidden rounded-s border-0 bg-surface-3 p-0">
                <img src={a.preview} alt="" className="block size-full object-cover" />
              </button>
            ) : (
              <span aria-hidden="true" className="grid size-6 flex-none place-items-center rounded-s bg-surface-3 text-dim">
                <svg viewBox="0 0 16 16" width="13" height="13">
                  {a.image
                    ? <path d="M2.5 3.5h11v9h-11zM2.5 10.5l3-3 3 3 2-2 3 3M10 6.2h.01" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round" strokeLinecap="round" />
                    : <path d="M4 1.8h5.2L12 4.6v9.6H4zM9 1.8v3h3M6 8h4M6 10.5h4" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round" strokeLinecap="round" />}
                </svg>
              </span>
            )}
            <span className="min-w-0 truncate">{a.name}</span>
            <span className="flex-none text-xs text-faint">{size(a.bytes)}</span>
            <button type="button" aria-label={`Remove ${a.name}`} onClick={() => onRemove(a.path)}
              className="grid size-6 flex-none cursor-pointer place-items-center rounded-s border-0 bg-transparent text-dim hover:bg-selection hover:text-fg">
              <svg viewBox="0 0 16 16" width="10" height="10" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" /></svg>
            </button>
          </span>
        );
      })}
      {uploading > 0 && <span className="text-s text-dim" role="status">Attaching {uploading === 1 ? "a file" : `${uploading} files`}…</span>}
      {error && <span className="text-s text-danger-fg" role="alert">{error}</span>}
      {open !== null && images[open] && (
        <ImagePreview open onOpenChange={(o) => { if (!o) setOpen(null); }} initial={open} title={images[open].path}
          images={images.map((a) => ({ src: a.preview!, label: a.name, alt: a.name }))} />
      )}
    </div>
  );
}
