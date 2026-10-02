import React, { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useStore } from "../store";
import { useNav } from "../nav";
import { api, ChatMessage } from "../api";
import { renderMarkdown } from "../markdown";

export function Chat() {
  const nav = useNav();
  const projects = useStore((s) => s.projects);
  const cid = nav.project ?? projects[0]?.id ?? null;
  const msgs = useStore((s) => (cid ? s.chat[cid] : undefined)) ?? EMPTY;
  const streaming = useStore((s) => (cid ? s.streaming[cid] : undefined)) ?? EMPTY_S;
  const [text, setText] = useState("");
  const log = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const input = useRef<HTMLTextAreaElement>(null);

  useEffect(() => { input.current?.focus(); }, [cid]);
  useLayoutEffect(() => {
    if (stick.current && log.current) log.current.scrollTop = log.current.scrollHeight;
  });

  const sorted = useMemo(() => [...msgs].sort((a, b) => String(a.ts).localeCompare(String(b.ts), undefined, { numeric: true })), [msgs]);
  if (!cid) return <div className="empty">No project selected.</div>;

  const send = () => {
    const t = text.trim();
    if (!t) return;
    setText("");
    api.send(cid, t).catch((e) => alert(String(e)));
  };

  return (
    <div className="chat">
      <div className="chat-log" ref={log} onScroll={(e) => {
        const el = e.currentTarget;
        stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
      }}>
        {sorted.map((m) => <Message key={m.id} role={m.role} text={m.text} />)}
        {Object.entries(streaming).map(([id, t]) => <Message key={id} role="coordinator" text={t} live />)}
        {!sorted.length && !Object.keys(streaming).length && <div className="empty">No messages yet. Ask the coordinator something.</div>}
      </div>
      <div className="composer">
        <div className="composer-inner">
          <textarea ref={input} rows={1} value={text} placeholder="Message the coordinator — Enter to send, Shift+Enter for newline"
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); send(); }
            }} />
          <button className="btn" onClick={send}>Send</button>
        </div>
      </div>
    </div>
  );
}
const EMPTY: ChatMessage[] = [];
const EMPTY_S: Record<string, string> = {};

const Message = memo(function Message({ role, text, live }: { role: string; text: string; live?: boolean }) {
  const html = useMemo(() => renderMarkdown(text), [text]);
  return (
    <div className="msg">
      <div className={"avatar " + role}>{role === "user" ? "you" : "Q"}</div>
      <div>
        <div className="who">{role === "user" ? "you" : "coordinator"}{live && <span className="faint"> · streaming</span>}</div>
        <div className={"md" + (live ? " cursor-blink" : "")} dangerouslySetInnerHTML={{ __html: html }} />
      </div>
    </div>
  );
});
