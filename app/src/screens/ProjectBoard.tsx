// J3: the Project's board, live from the event stream, beside the coordinator chat.
import React, { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { api, Project, Task, TranscriptItem } from "../api";
import { href } from "../nav";
import { loadChat, useStore } from "../store";
import { ago, errText, STATES } from "../util";
import { renderMarkdown } from "../markdown";
import { Unavailable } from "../components/Unavailable";

const ALWAYS_SHOWN = new Set(["queued", "running", "needs_decision", "in_review", "done"]);

export function ProjectBoard({ id }: { id: string }) {
  const project = useStore((s) => s.projects[id]);
  const connected = useStore((s) => s.connected);
  const tasks = useStore((s) => s.tasks);
  const [chatOpen, setChatOpen] = useState(true);
  const proposals = useStore((s) => s.memoryProposals);
  const toReview = useMemo(() => Object.values(proposals).filter((m) => m.project_id === id && m.state === "proposed").length, [proposals, id]);

  const mine = useMemo(() => Object.values(tasks).filter((t) => t.project_id === id), [tasks, id]);
  const columns = useMemo(() => {
    const by: Record<string, Task[]> = {};
    for (const t of mine) (by[t.state] ??= []).push(t);
    for (const k in by) by[k].sort((a, b) => b.updated_at.localeCompare(a.updated_at));
    return STATES.filter((c) => ALWAYS_SHOWN.has(c.id) || by[c.id]?.length).map((c) => ({ ...c, tasks: by[c.id] ?? [] }));
  }, [mine]);

  // Flash cards whose state changed since the last render.
  const prev = useRef<Record<string, string>>({});
  const changed = new Set<string>();
  for (const t of mine) if (prev.current[t.id] !== undefined && prev.current[t.id] !== t.state) changed.add(t.id);
  useEffect(() => { prev.current = Object.fromEntries(mine.map((t) => [t.id, t.state])); });

  if (!project) {
    return <div className="empty">{connected ? "This project does not exist on the daemon." : "Waiting for the daemon…"}</div>;
  }

  return (
    <>
      <div className="header">
        <h1>{project.name}</h1>
        {project.status && project.status !== "ready" && (
          <span className={"pill " + (project.status === "failed" ? "red" : "yellow")} data-testid="project-status">{project.status}</span>
        )}
        {project.goal && <span className="crumb ellipsis" title={project.goal}>{project.goal}</span>}
        <span className="spacer" />
        <a className="btn" href={href({ name: "memory", project: id })} data-testid="nav-memory" title="Review what finished tasks learned">
          Memory{toReview > 0 && <span className="pill accent" data-testid="memory-count">{toReview}</span>}
        </a>
        <a className="btn" href={href({ name: "dispatch", project: id })} data-testid="nav-dispatch" title="Which agent each kind of task gets">Dispatch</a>
        <button className={"btn" + (chatOpen ? " on" : "")} onClick={() => setChatOpen(!chatOpen)}>Coordinator</button>
      </div>
      {project.status && project.status !== "ready" && <ProvisionBar project={project} />}
      <div className={"screen board-layout" + (chatOpen ? " with-chat" : "")}>
        <div className="board" style={{ gridTemplateColumns: `repeat(${columns.length}, minmax(180px, 1fr))` }}>
          {columns.map((c) => (
            <div className="col" key={c.id} data-testid={`col-${c.id}`}>
              <div className="col-head"><span className="swatch" style={{ background: c.color }} />{c.label}<span className="n">{c.tasks.length}</span></div>
              <div className="col-body">
                {c.tasks.map((t) => (
                  <a className={"card" + (changed.has(t.id) ? " flash" : "")} key={t.id + (changed.has(t.id) ? ":" + t.state : "")}
                    href={href({ name: "task", id: t.id })} data-testid="task-card">
                    <div className="title">{t.title}</div>
                    {t.state_note && <div className="note">{t.state_note}</div>}
                    <div className="meta">
                      {t.harness && <span className="pill accent">{t.harness}</span>}
                      {t.kind && <span className="pill">{t.kind}</span>}
                      {t.pull_request_url && <span className="pill green">PR</span>}
                      <span className="spacer" />
                      <span>{ago(t.updated_at)}</span>
                    </div>
                  </a>
                ))}
                {c.tasks.length === 0 && <div className="faint col-empty">—</div>}
              </div>
            </div>
          ))}
        </div>
        {chatOpen && <CoordinatorChat cid={id} />}
      </div>
    </>
  );
}

function ProvisionBar({ project }: { project: Project }) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const retry = async () => {
    setBusy(true); setErr(null);
    try { await api.provision(project.id); } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };
  return (
    <div className={"state-note" + (project.status === "failed" ? " bad" : "")} data-testid="provision-bar">
      {project.status === "failed" ? "Setting up this project failed" : "Setting up this project"}
      {project.status_detail && <>: {project.status_detail}</>}
      {project.status === "failed" && <button className="btn small" style={{ marginLeft: 8 }} onClick={retry} disabled={busy}>Retry</button>}
      {err && <span className="bad"> {err}</span>}
    </div>
  );
}

const EMPTY: TranscriptItem[] = [];
const CHAT_ROLES = new Set(["user", "assistant"]);

export function CoordinatorChat({ cid }: { cid: string }) {
  const loaded = useStore((s) => s.chat[cid]);
  const items = loaded ?? EMPTY;
  const [status, setStatus] = useState<"loading" | "ok" | "unavailable" | "error">("loading");
  const [err, setErr] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  // Sent messages are not echoed: they appear once the coordinator's session records them.
  const [pending, setPending] = useState<{ key: number; text: string; confirmed: boolean }[]>([]);
  const log = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

  useEffect(() => {
    loadChat(cid).then(setStatus).catch((e) => { setStatus("error"); setErr(errText(e)); });
  }, [cid]);
  useLayoutEffect(() => {
    if (stick.current && log.current) log.current.scrollTop = log.current.scrollHeight;
  });

  const sorted = useMemo(() => [...items].sort((a, b) => a.id - b.id), [items]);
  // Drop a pending message once a user entry with the same text arrives.
  useEffect(() => {
    if (!pending.length) return;
    const seen = new Set(sorted.filter((m) => m.role === "user").map((m) => m.text.trim()));
    const left = pending.filter((p) => !seen.has(p.text));
    if (left.length !== pending.length) setPending(left);
  }, [sorted, pending]);

  const send = async () => {
    const t = text.trim();
    if (!t || sending) return;
    setSending(true); setErr(null);
    try {
      const r = await api.sendChat(cid, t);
      setText(""); stick.current = true;
      setPending((p) => [...p, { key: Date.now(), text: t, confirmed: r?.confirmed ?? true }]);
    } catch (e) { setErr(errText(e)); }
    finally { setSending(false); }
  };

  return (
    <aside className="side-chat" data-testid="coordinator-chat">
      <div className="panel-head">Coordinator</div>
      {status === "unavailable" ? (
        <Unavailable what="Coordinator chat" endpoint={`GET /v1/coordinators/${cid}/messages`} />
      ) : (
        <>
          <div className="chat-log" ref={log} onScroll={(e) => {
            const el = e.currentTarget;
            stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
          }}>
            {groupForChat(sorted).map((g) =>
              g.kind === "msg" ? <Message key={g.item.id} m={g.item} /> : <Activity key={g.items[0].id} items={g.items} />,
            )}
            {pending.map((p) => (
              <div className="msg pending" key={p.key}>
                <div className="avatar user">you</div>
                <div>
                  <div className="who">you <span className="faint">{p.confirmed ? "sent" : "typed, not confirmed; check before sending again"}</span></div>
                  <div className="md">{p.text}</div>
                </div>
              </div>
            ))}
            {status === "ok" && !sorted.length && !pending.length && <div className="empty">Ask the coordinator to plan or delegate work.</div>}
            {status === "loading" && <div className="empty">Loading…</div>}
          </div>
          {err && <div className="form-error">{err}</div>}
          <div className="composer">
            <textarea rows={2} value={text} placeholder="Message the coordinator (Enter to send, Shift+Enter for a newline)"
              aria-label="Message the coordinator" onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); void send(); }
              }} />
            <button className="btn" onClick={() => void send()} disabled={sending || !text.trim()}>Send</button>
          </div>
        </>
      )}
    </aside>
  );
}

type ChatGroup = { kind: "msg"; item: TranscriptItem } | { kind: "activity"; items: TranscriptItem[] };

/** Chat shows user and assistant entries; runs of thinking and tool entries collapse into one line. */
export function groupForChat(items: TranscriptItem[]): ChatGroup[] {
  const out: ChatGroup[] = [];
  for (const item of items) {
    if (CHAT_ROLES.has(item.role)) { out.push({ kind: "msg", item }); continue; }
    const last = out[out.length - 1];
    if (last?.kind === "activity") last.items.push(item);
    else out.push({ kind: "activity", items: [item] });
  }
  return out;
}

function Activity({ items }: { items: TranscriptItem[] }) {
  const tools = items.filter((i) => i.role === "tool_call").length;
  const errors = items.filter((i) => i.is_error).length;
  return (
    <details className="activity">
      <summary>
        {tools ? `${tools} tool ${tools === 1 ? "call" : "calls"}` : "Thinking"}
        {errors > 0 && <span className="bad"> · {errors} failed</span>}
      </summary>
      {items.map((i) => (
        <pre key={i.id} className={i.is_error ? "bad" : ""}>{i.tool ? i.tool.title : (i.tool_name ? i.tool_name + ": " : "") + i.text}</pre>
      ))}
    </details>
  );
}

const Message = memo(function Message({ m }: { m: TranscriptItem }) {
  const html = useMemo(() => renderMarkdown(m.text), [m.text]);
  return (
    <div className="msg">
      <div className={"avatar " + (m.role === "user" ? "user" : "coordinator")}>{m.role === "user" ? "you" : "Q"}</div>
      <div>
        <div className="who">{m.role === "user" ? "you" : "coordinator"} <span className="faint">{ago(m.ts)}</span></div>
        <div className="md" dangerouslySetInnerHTML={{ __html: html }} />
      </div>
    </div>
  );
});
