// J5: decisions held for a person, across every Project, answered from one place.
//
// Keys: j/k or ↑/↓ move, Enter or r answers, ⌘/Ctrl+Enter sends, Esc leaves the answer box,
// o and a switch between open and answered, t opens the decision's task.
import React, { useEffect, useMemo, useRef, useState } from "react";
import { api, Decision, DecisionState, NotAvailable } from "../api";
import { go, href } from "../nav";
import { upsertDecision, useStore } from "../store";
import { inboxList, nextAfterAnswer, step } from "../inbox";
import { ago, errText } from "../util";
import { Unavailable } from "../components/Unavailable";

const USER_KEY = "quark.user";
function savedUser(): string {
  try { return localStorage.getItem(USER_KEY) ?? ""; } catch { return ""; }
}

export function Inbox({ id }: { id?: string }) {
  const decisions = useStore((s) => s.decisions);
  const projects = useStore((s) => s.projects);
  const connected = useStore((s) => s.connected);
  const all = useMemo(() => Object.values(decisions), [decisions]);
  const selectedDecision = id ? decisions[id] : undefined;
  const [filter, setFilter] = useState<DecisionState>(selectedDecision?.state ?? "open");
  const list = useMemo(() => inboxList(all, filter), [all, filter]);
  const openCount = useMemo(() => all.filter((d) => d.state === "open").length, [all]);

  // A selection in the URL wins; otherwise the first row of the current list.
  const selected = id && list.some((d) => d.id === id) ? id : list[0]?.id;
  const current = selected ? decisions[selected] : undefined;
  const select = (next: string | undefined) => go({ name: "inbox", id: next });

  // Jumping to a decision from elsewhere (the palette) shows the list it is in.
  useEffect(() => {
    if (selectedDecision && selectedDecision.state !== filter) setFilter(selectedDecision.state);
    // Only when the URL changes, so switching lists by hand is not undone.
  }, [id]);

  const answerBox = useRef<HTMLTextAreaElement>(null);
  const rowRefs = useRef(new Map<string, HTMLElement>());
  useEffect(() => { if (selected) rowRefs.current.get(selected)?.scrollIntoView({ block: "nearest" }); }, [selected]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.closest("input, textarea, select, [contenteditable]") || t.closest("[cmdk-root]"))) return;
      const move = (delta: number) => { e.preventDefault(); select(step(list, selected, delta)); };
      switch (e.key) {
        case "j": case "ArrowDown": return move(1);
        case "k": case "ArrowUp": return move(-1);
        case "o": e.preventDefault(); setFilter("open"); return;
        case "a": e.preventDefault(); setFilter("answered"); return;
        case "t":
          if (current?.task_id) { e.preventDefault(); go({ name: "task", id: current.task_id }); }
          return;
        case "Enter": case "r":
          if (current?.state === "open") { e.preventDefault(); answerBox.current?.focus(); }
          return;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const onAnswered = (d: Decision) => {
    const next = nextAfterAnswer(inboxList(all, "open"), d.id);
    upsertDecision(d);
    select(next);
  };

  return (
    <>
      <div className="header">
        <h1>Decisions</h1>
        <span className="crumb">across all projects</span>
        <span className="spacer" />
        <div className="seg" role="tablist" aria-label="Decisions">
          <button role="tab" aria-selected={filter === "open"} className={filter === "open" ? "on" : ""} onClick={() => setFilter("open")}>
            Open <span className="n">{openCount}</span>
          </button>
          <button role="tab" aria-selected={filter === "answered"} className={filter === "answered" ? "on" : ""} onClick={() => setFilter("answered")}>
            Answered
          </button>
        </div>
      </div>
      <div className="screen inbox">
        <div className="inbox-list" data-testid="decision-list">
          {list.map((d) => (
            <a key={d.id} ref={(el) => { if (el) rowRefs.current.set(d.id, el); else rowRefs.current.delete(d.id); }}
              className={"decision-row" + (d.id === selected ? " on" : "")} href={href({ name: "inbox", id: d.id })}
              data-testid="decision-row" aria-current={d.id === selected ? "true" : undefined}>
              <div className="q">{d.question}</div>
              <div className="meta">
                <span className="pill">{projects[d.project_id]?.name ?? d.project_id}</span>
                {d.state === "answered" && d.answered_by && <span className="faint">answered by {d.answered_by}</span>}
                <span className="spacer" />
                <span>{ago(d.state === "answered" ? d.answered_at ?? d.opened_at : d.opened_at)}</span>
              </div>
            </a>
          ))}
          {!list.length && (
            <div className="empty">
              {!connected ? "Waiting for the daemon…" : filter === "open" ? "Nothing needs a decision right now." : "No answered decisions yet."}
            </div>
          )}
        </div>
        <div className="inbox-detail">
          {current ? <DecisionDetail key={current.id} d={current} answerBox={answerBox} onAnswered={onAnswered} />
            : <div className="empty">Select a decision.</div>}
          <div className="inbox-keys faint">
            <span className="kbd">j</span><span className="kbd">k</span> move · <span className="kbd">r</span> answer ·{" "}
            <span className="kbd">o</span><span className="kbd">a</span> open/answered · <span className="kbd">t</span> task
          </div>
        </div>
      </div>
    </>
  );
}

function DecisionDetail({ d, answerBox, onAnswered }: {
  d: Decision; answerBox: React.RefObject<HTMLTextAreaElement>; onAnswered: (d: Decision) => void;
}) {
  const project = useStore((s) => s.projects[d.project_id]);
  const task = useStore((s) => (d.task_id ? s.tasks[d.task_id] : undefined));
  const [text, setText] = useState("");
  const [user, setUser] = useState(savedUser);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);

  const send = async () => {
    const answer = text.trim();
    if (!answer || busy) return;
    setBusy(true); setErr(null);
    const by = user.trim();
    try { localStorage.setItem(USER_KEY, by); } catch { /* storage unavailable */ }
    try {
      const r = await api.answerDecision(d.id, { answer, answered_by: by || null });
      setText("");
      onAnswered(r);
    } catch (e) {
      if (e instanceof NotAvailable) setUnavailable(true);
      else setErr(errText(e));
    } finally { setBusy(false); }
  };

  return (
    <div className="decision" data-testid="decision-detail">
      <div className="d-context">
        <a href={href({ name: "project", id: d.project_id })}>{project?.name ?? d.project_id}</a>
        {d.task_id && <> · <a href={href({ name: "task", id: d.task_id })}>{task?.title ?? d.task_id}</a></>}
        <span className="faint"> · asked {ago(d.opened_at)}</span>
      </div>
      <div className="d-question">{d.question}</div>
      {d.state === "answered" ? (
        <div className="d-answer" data-testid="decision-answer">
          <div className="who">
            Answered{d.answered_by ? <> by <b>{d.answered_by}</b></> : ""}{d.answered_at ? <> · {ago(d.answered_at)}</> : ""}
          </div>
          <div className="text">{d.answer}</div>
        </div>
      ) : unavailable ? (
        <Unavailable what="Answering decisions" endpoint={`POST /v1/decisions/${d.id}:answer`} />
      ) : (
        <form className="d-form" onSubmit={(e) => { e.preventDefault(); void send(); }}>
          <textarea ref={answerBox} aria-label="Answer" placeholder="Your answer…" value={text} rows={4}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); void send(); }
              if (e.key === "Escape") { e.preventDefault(); e.currentTarget.blur(); }
            }} />
          <div className="d-actions">
            <label className="faint small-text">Answering as{" "}
              <input aria-label="Answering as" placeholder="you" value={user} onChange={(e) => setUser(e.target.value)} />
            </label>
            {err && <span className="bad small-text">{err}</span>}
            <span className="spacer" />
            <span className="faint small-text"><span className="kbd">⌘/Ctrl ↵</span></span>
            <button className="btn on" type="submit" disabled={busy || !text.trim()}>{busy ? "Sending…" : "Answer"}</button>
          </div>
        </form>
      )}
    </div>
  );
}
