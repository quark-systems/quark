// Files pasted, dropped or picked into a prompt box. Each is saved by the daemon as soon as it is
// added, listed under the box, and named in the message so the agent opens it from disk. Like drafts,
// the list is kept per box in local storage, so it survives leaving the view and quitting the app.
import React, { useCallback, useState } from "react";
import { api, Attachment } from "./api";
import { errText } from "./util";

const PREFIX = "quark.attachments.";

export interface Attached extends Attachment {
  image: boolean;
  /** A blob: URL for the thumbnail, while this window still holds the file. */
  preview?: string;
}

// Thumbnails by saved path; they last as long as the window.
const previews = new Map<string, string>();

export function readAttached(key: string): Attached[] {
  try {
    const list = JSON.parse(localStorage.getItem(PREFIX + key) ?? "[]");
    return Array.isArray(list) ? list.map((a: Attached) => ({ ...a, preview: previews.get(a.path) })) : [];
  } catch { return []; }
}

/** An empty list is removed rather than stored. */
export function writeAttached(key: string, list: Attached[]) {
  try {
    if (list.length) localStorage.setItem(PREFIX + key, JSON.stringify(list.map(({ preview: _, ...a }) => a)));
    else localStorage.removeItem(PREFIX + key);
  } catch { /* storage unavailable */ }
}

const IMAGE = /\.(png|jpe?g|gif|webp|bmp|svg|heic|tiff?)$/i;
export const isImage = (name: string, type?: string) => !!type?.startsWith("image/") || IMAGE.test(name);

/** Clipboard images all arrive as image.png; give each a name that says when it was pasted. */
export function pastedName(file: { name: string; type: string }, at = new Date()): string {
  if (file.name && !/^image\.\w+$/i.test(file.name)) return file.name;
  const ext = file.type.split("/")[1]?.replace("jpeg", "jpg").replace(/\W.*$/, "") || "png";
  const p = (n: number) => String(n).padStart(2, "0");
  return `pasted-${at.getFullYear()}${p(at.getMonth() + 1)}${p(at.getDate())}-${p(at.getHours())}${p(at.getMinutes())}${p(at.getSeconds())}.${ext}`;
}

/** The message as sent: what was typed, then one line per attached file. */
export function withAttachments(text: string, files: Pick<Attachment, "path">[]): string {
  const t = text.trim();
  if (!files.length) return t;
  const list = files.map((f) => `- ${f.path}`).join("\n");
  return `${t}${t ? "\n\n" : ""}Attached ${files.length === 1 ? "file" : "files"}:\n${list}`;
}

const hasFiles = (e: React.DragEvent) => Array.from(e.dataTransfer.types).includes("Files");

/**
 * Attachments for one prompt box (`key`, as for its draft), saved to Project `pid`. Spread
 * `dropProps` on the box and pass `onPaste` to its text field; `dragging` is true while files
 * are held over it.
 */
export function useAttachments(pid: string | undefined, key: string) {
  const [state, setState] = useState(() => ({ key, items: readAttached(key) }));
  let current = state;
  // The same box can be shown for another key without remounting.
  if (state.key !== key) {
    current = { key, items: readAttached(key) };
    setState(current);
  }
  const [uploading, setUploading] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);

  const save = useCallback((items: Attached[]) => {
    writeAttached(key, items);
    setState((s) => (s.key === key ? { key, items } : s));
  }, [key]);

  const add = useCallback(async (files: File[]) => {
    if (!files.length) return;
    if (!pid) { setError("Files can be attached once the project is set up"); return; }
    setError(null);
    setUploading((n) => n + files.length);
    await Promise.all(files.map(async (file) => {
      const name = pastedName(file);
      try {
        const a = await api.attach(pid, file, name);
        const image = isImage(a.name, file.type);
        if (image) previews.set(a.path, URL.createObjectURL(file));
        save([...readAttached(key), { ...a, image, preview: previews.get(a.path) }]);
      } catch (e) {
        setError(`${name} was not attached: ${errText(e)}`);
      } finally { setUploading((n) => n - 1); }
    }));
  }, [pid, key, save]);

  const remove = useCallback((path: string) => save(readAttached(key).filter((a) => a.path !== path)), [key, save]);
  const clear = useCallback(() => { save([]); setError(null); }, [save]);

  const onPaste = useCallback((e: React.ClipboardEvent) => {
    const files = Array.from(e.clipboardData.files);
    if (!files.length) return;
    e.preventDefault();
    void add(files);
  }, [add]);

  const dropProps = {
    onDragOver: (e: React.DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "copy";
      setDragging(true);
    },
    onDragLeave: (e: React.DragEvent) => {
      if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDragging(false);
    },
    onDrop: (e: React.DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      setDragging(false);
      void add(Array.from(e.dataTransfer.files));
    },
  };

  return { items: current.items, uploading, error, dragging, add, remove, clear, set: save, onPaste, dropProps };
}

export type Attachments = ReturnType<typeof useAttachments>;
