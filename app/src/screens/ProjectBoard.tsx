// J3: the Project's board, live from the event stream, beside the coordinator chat.
import { useEffect, useMemo, useRef, useState } from "react";
import { api, Project, Task, TranscriptItem } from "../api";
import { href } from "../nav";
import { loadChat, useStore } from "../store";
import { errText, STATES } from "../util";
import { Unavailable } from "../components/Unavailable";
import { Composer } from "../components/Composer";
import { WorkerCard } from "../components/WorkerCard";
import { Transcript, UserMessage } from "../components/transcript";

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
        <a className="btn" href={href({ name: "settings", project: id })} data-testid="nav-settings" title="Every switch for this Project">Settings</a>
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
                  <WorkerCard key={t.id + (changed.has(t.id) ? ":" + t.state : "")} task={t} flash={changed.has(t.id)} />
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

export function CoordinatorChat({ cid }: { cid: string }) {
  const loaded = useStore((s) => s.chat[cid]);
  const items = loaded ?? EMPTY;
  const [status, setStatus] = useState<"loading" | "ok" | "unavailable" | "error">("loading");
  const [err, setErr] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  // Sent messages are not echoed: they appear once the coordinator's session records them.
  const [pending, setPending] = useState<{ key: number; text: string; confirmed: boolean }[]>([]);

  useEffect(() => {
    loadChat(cid).then(setStatus).catch((e) => { setStatus("error"); setErr(errText(e)); });
  }, [cid]);

  // Drop a pending message once a user entry with the same text arrives.
  useEffect(() => {
    if (!pending.length) return;
    const seen = new Set(items.filter((m) => m.role === "user").map((m) => m.text.trim()));
    const left = pending.filter((p) => !seen.has(p.text));
    if (left.length !== pending.length) setPending(left);
  }, [items, pending]);

  const send = async () => {
    const t = text.trim();
    if (!t || sending) return;
    setSending(true); setErr(null);
    try {
      const r = await api.sendChat(cid, t);
      setText("");
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
          <Transcript key={cid} items={items} agent="coordinator" actions className="chat-log"
            empty={status === "ok" ? <div className="empty">Ask the coordinator to plan or delegate work.</div>
              : status === "loading" ? <div className="empty">Loading…</div> : null}>
            {pending.length > 0 && pending.map((p) => (
              <UserMessage key={p.key} pending item={{ text: p.text, ts: null, truncated: false }}
                note={p.confirmed ? "sent" : "typed, not confirmed; check before sending again"} />
            ))}
          </Transcript>
          {err && <div className="form-error">{err}</div>}
          <Composer value={text} onChange={setText} onSend={() => void send()} sending={sending}
            label="Message the coordinator" placeholder="Message the coordinator (Enter to send, Shift+Enter for a newline)" />
        </>
      )}
    </aside>
  );
}
