// Message box: a card with the text on top and its tools along the bottom, the way
// Claude desktop, Cursor and T3 Code lay theirs out. It grows with its text up to a limit;
// Enter sends, Shift+Enter adds a line. With `attachments`, images can be pasted and files
// picked or dropped onto it. With `onStop`, Send turns into Stop while the box is empty.
import { useLayoutEffect, useRef, type ReactNode, type RefObject } from "react";
import type { Attachments } from "../attachments";
import { AttachButton, AttachmentList, cx } from "../ui";

export function Composer({ value, onChange, onSend, sending, label, placeholder, attachments, tools, hint, onStop, stopping }: {
  value: string; onChange: (v: string) => void; onSend: () => void; sending: boolean; label: string; placeholder: string;
  attachments?: Attachments;
  /** More tools after the attach button, such as the commands button. */
  tools?: ReactNode;
  /** A short line before the send button, such as what Enter does now. */
  hint?: ReactNode;
  /** Shown in place of Send while the box is empty: stops what the agent is doing. */
  onStop?: () => void;
  stopping?: boolean;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  useAutoGrow(ref, value);
  const a = attachments;
  const ready = !!value.trim() || !!a?.items.length;
  const stop = onStop && !ready;
  return (
    <div className={cx("composer", a?.dragging && "dropping")} {...a?.dropProps}>
      {a && <AttachmentList items={a.items} uploading={a.uploading} error={a.error} onRemove={a.remove} />}
      <textarea ref={ref} rows={1} value={value} placeholder={placeholder} aria-label={label}
        onChange={(e) => onChange(e.target.value)} onPaste={a?.onPaste}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); onSend(); }
        }} />
      <div className="composer-tools">
        {a && <AttachButton onFiles={(f) => void a.add(f)} />}
        {tools}
        <span className="composer-spacer" />
        {hint && <span className="composer-hint">{hint}</span>}
        {stop ? (
          <button type="button" className="btn send stop" onClick={onStop} disabled={stopping} aria-label="Stop" title="Stop what it is doing; it reads your next message" data-testid="composer-stop">
            <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true"><rect x="3" y="3" width="10" height="10" rx="1.5" fill="currentColor" /></svg>
          </button>
        ) : (
          <button type="button" className="btn send" onClick={onSend} disabled={sending || !ready || !!a?.uploading} aria-label="Send">
            <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
              <path d="M8 13V3M4 7l4-4 4 4" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          </button>
        )}
      </div>
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
