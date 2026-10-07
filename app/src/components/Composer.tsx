// Message box: grows with its text up to a limit; Enter sends, Shift+Enter adds a line.
import { useLayoutEffect, useRef } from "react";

export function Composer({ value, onChange, onSend, sending, label, placeholder }: {
  value: string; onChange: (v: string) => void; onSend: () => void; sending: boolean; label: string; placeholder: string;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
  }, [value]);
  return (
    <div className="composer">
      <textarea ref={ref} rows={1} value={value} placeholder={placeholder} aria-label={label}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); onSend(); }
        }} />
      <button className="btn send" onClick={onSend} disabled={sending || !value.trim()} aria-label="Send">
        <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
          <path d="M8 13V3M4 7l4-4 4 4" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </button>
    </div>
  );
}
