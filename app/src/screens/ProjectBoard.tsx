// J3: the project's Conversation tab (its coordinator) and Work tab (its board, live from the event stream).
import { useEffect, useMemo, useRef, useState } from "react";
import { api, Project, Task, TranscriptItem } from "../api";
import { SinceYouLooked } from "../shell/SinceYouLooked";
import { loadChat, useStore } from "../store";
import { errText, STATES } from "../util";
import { Unavailable } from "../components/Unavailable";
import { Composer } from "../components/Composer";
import { WorkerCard } from "../components/WorkerCard";
import { lastId, PendingMessage, stillPending, Transcript, UserMessage } from "../components/transcript";
import { PersonaPicker } from "../components/PersonaPicker";
import { useLabels } from "../persona";

const ALWAYS_SHOWN = new Set(["queued", "running", "needs_decision", "in_review", "done"]);

export function ProjectBoard({ id }: { id: string }) {
  const project = useStore((s) => s.projects[id]);
  const connected = useStore((s) => s.connected);
  const tasks = useStore((s) => s.tasks);
  const l = useLabels(id);

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

  if (!project) return <Missing connected={connected} />;

  return (
    <>
      {project.status && project.status !== "ready" && <ProvisionBar project={project} />}
      <div className="screen board-layout">
        <div className="board" style={{ gridTemplateColumns: `repeat(${columns.length}, minmax(180px, 1fr))` }}>
          {columns.map((c) => (
            <div className="col" key={c.id} data-testid={`col-${c.id}`}>
              <div className="col-head"><span className="swatch" style={{ background: c.color }} />{c.id === "needs_decision" ? `Needs ${l.role("decision")}` : c.label}<span className="n">{c.tasks.length}</span></div>
              <div className="col-body">
                {c.tasks.map((t) => (
                  <WorkerCard key={t.id + (changed.has(t.id) ? ":" + t.state : "")} task={t} flash={changed.has(t.id)} />
                ))}
                {c.tasks.length === 0 && <div className="faint col-empty">—</div>}
              </div>
            </div>
          ))}
        </div>
      </div>
    </>
  );
}

/** The Conversation tab, the project's home: its coordinator, and what changed since you looked. */
export function Conversation({ id }: { id: string }) {
  const project = useStore((s) => s.projects[id]);
  const connected = useStore((s) => s.connected);
  if (!project) return <Missing connected={connected} />;
  return (
    <>
      {project.status && project.status !== "ready" && <ProvisionBar project={project} />}
      <div className="conversation">
        <CoordinatorChat cid={id} />
        <SinceYouLooked project={id} />
      </div>
    </>
  );
}

function Missing({ connected }: { connected: boolean }) {
  return <div className="empty">{connected ? "This project does not exist on the daemon." : "Waiting for the daemon…"}</div>;
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

export function CoordinatorChat({ cid }: { cid: string }) {
  const loaded = useStore((s) => s.chat[cid]);
  const items = loaded ?? EMPTY;
  const [status, setStatus] = useState<"loading" | "ok" | "unavailable" | "error">("loading");
  const [err, setErr] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  // Sent messages are not echoed: they appear once the coordinator's session records them.
  const [pending, setPending] = useState<PendingMessage[]>([]);
  const l = useLabels(cid);
  const coordinator = l.role("coordinator");

  useEffect(() => {
    loadChat(cid).then(setStatus).catch((e) => { setStatus("error"); setErr(errText(e)); });
  }, [cid]);

  // Drop a pending message once the log records it.
  useEffect(() => {
    const left = stillPending(pending, items);
    if (left !== pending) setPending(left);
  }, [items, pending]);

  const send = async () => {
    const t = text.trim();
    if (!t || sending) return;
    setSending(true); setErr(null);
    // Only entries recorded after this point can be this message.
    const after = lastId(items);
    try {
      const r = await api.sendChat(cid, t);
      setText("");
      setPending((p) => [...p, { key: Date.now(), text: t, confirmed: r?.confirmed ?? true, after }]);
    } catch (e) { setErr(errText(e)); }
    finally { setSending(false); }
  };

  return (
    <aside className="side-chat" data-testid="coordinator-chat">
      <div className="panel-head">{l.Role("coordinator")}<span className="spacer" /><PersonaPicker projectId={cid} /></div>
      {status === "unavailable" ? (
        <Unavailable what="Coordinator chat" endpoint={`GET /v1/coordinators/${cid}/messages`} />
      ) : (
        <>
          <Transcript key={cid} items={items} agent="coordinator" actions className="chat-log"
            empty={status === "ok" ? <div className="empty">Ask the {coordinator} to plan or delegate work.</div>
              : status === "loading" ? <div className="empty">Loading…</div> : null}>
            {pending.length > 0 && pending.map((p) => (
              <UserMessage key={p.key} pending item={{ text: p.text, ts: null, truncated: false }}
                note={p.confirmed ? "sent" : "typed, not confirmed; check before sending again"} />
            ))}
          </Transcript>
          {err && <div className="form-error">{err}</div>}
          <Composer value={text} onChange={setText} onSend={() => void send()} sending={sending}
            label={`Message the ${coordinator}`} placeholder={`Message the ${coordinator} (Enter to send, Shift+Enter for a newline)`} />
        </>
      )}
    </aside>
  );
}
