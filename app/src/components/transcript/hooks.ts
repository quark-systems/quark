import React, { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

const NEAR_BOTTOM_PX = 40;

/**
 * Scroll anchoring for a transcript. Follows new output while the reader is at the
 * bottom and stays put once they scroll up, offering a jump back when entries
 * arrive meanwhile. `holdPosition` keeps the view still while content is added
 * above it, such as earlier turns. Logic follows MonoCode's `useTurnScrollAnchor`
 * (https://github.com/hardbeat920/monocode, MIT).
 */
export function useScrollAnchor<T extends HTMLElement>(count: number) {
  const ref = useRef<T>(null);
  const stick = useRef(true);
  const fromBottom = useRef<number | null>(null);
  const seen = useRef(count);
  const [unseen, setUnseen] = useState(false);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (fromBottom.current !== null) {
      el.scrollTop = el.scrollHeight - fromBottom.current;
      fromBottom.current = null;
    } else if (stick.current) {
      el.scrollTop = el.scrollHeight;
    }
  });

  useEffect(() => {
    if (count > seen.current && !stick.current) setUnseen(true);
    seen.current = count;
  }, [count]);

  const onScroll = useCallback((e: React.UIEvent<T>) => {
    const el = e.currentTarget;
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < NEAR_BOTTOM_PX;
    if (stick.current) setUnseen(false);
  }, []);

  const jump = useCallback(() => {
    stick.current = true;
    setUnseen(false);
    const el = ref.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, []);

  const holdPosition = useCallback(() => {
    const el = ref.current;
    if (el) fromBottom.current = el.scrollHeight - el.scrollTop;
  }, []);

  return { ref, onScroll, unseen, jump, holdPosition };
}

/** Milliseconds since `from`, ticking every second; undefined when not running. */
export function useElapsed(from: number | undefined): number | undefined {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (from === undefined) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [from]);
  return from === undefined ? undefined : Math.max(0, now - from);
}

/** Copies text to the clipboard; `copied` is true for a moment afterwards. */
export function useCopy(): { copied: boolean; copy: (text: string) => void } {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>();
  useEffect(() => () => clearTimeout(timer.current), []);
  const copy = useCallback((text: string) => {
    void navigator.clipboard?.writeText(text).then(() => {
      setCopied(true);
      clearTimeout(timer.current);
      timer.current = setTimeout(() => setCopied(false), 1500);
    }, () => { /* clipboard refused: nothing to show */ });
  }, []);
  return { copied, copy };
}

/** The current time, refreshed every `everyMs` while `active`. */
export function useNow(active: boolean, everyMs = 30_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), everyMs);
    return () => clearInterval(t);
  }, [active, everyMs]);
  return now;
}
