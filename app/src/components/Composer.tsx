// Message box: grows with its text up to a limit; Enter sends, Shift+Enter adds a line.
// With `attachments`, images can be pasted and files picked or dropped onto it.
import { useLayoutEffect, useRef, type RefObject } from "react";
import type { Attachments } from "../attachments";
import { AttachButton, AttachmentList, cx } from "../ui";

export function Composer({ value, onChange, onSend, sending, label, placeholder, attachments }: {
  value: string; onChange: (v: string) => void; onSend: () => void; sending: boolean; label: string; placeholder: string;
  attachments?: Attachments;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  useAutoGrow(ref, value);
  const a = attachments;
  const ready = !!value.trim() || !!a?.items.length;
  return (
    <div className={cx("composer", a?.dragging && "dropping")} {...a?.dropProps}>
      {a && <AttachmentList items={a.items} uploading={a.uploading} error={a.error} onRemove={a.remove} />}
      {a && <AttachButton onFiles={(f) => void a.add(f)} />}
      <textarea ref={ref} rows={1} value={value} placeholder={placeholder} aria-label={label}
        onChange={(e) => onChange(e.target.value)} onPaste={a?.onPaste}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); onSend(); }
        }} />
      <button className="btn send" onClick={onSend} disabled={sending || !ready || !!a?.uploading} aria-label="Send">
        <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
          <path d="M8 13V3M4 7l4-4 4 4" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </button>
    </div>
  );
}

/** Grows a one-line textarea with its text, up to 200px, so it lines up with the buttons beside it. */
export function useAutoGrow(ref: RefObject<HTMLTextAreaElement | null>, value: string) {
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
  }, [ref, value]);
}
