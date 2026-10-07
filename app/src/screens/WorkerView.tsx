// J4: one worker in one view. Live terminal and steering on the left; transcript, changed
// files with their diff, and why this agent on the right; cancel and relaunch in the header.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError, NotAvailable, TaskChanges, TranscriptItem } from "../api";
import { href } from "../nav";
import { loadTranscript, refreshTask, useStore } from "../store";
import { attach, getTerm, resetTerm, TermHandle } from "../terminal";
import { ago, errText, isActive, stateMeta } from "../util";
import { DiffFile, parseUnifiedDiff } from "../components/diff/model";
import { FileDiff } from "../components/diff/DiffView";
import { Unavailable } from "../components/Unavailable";
import { Transcript } from "../components/transcript";
import { WhyThisAgent } from "../components/WhyThisAgent";
import { HarnessLogo, harnessMark } from "../components/WorkerCard";

export function WorkerView({ id }: { id: string }) {
  const task = useStore((s) => s.tasks[id]);
  const project = useStore((s) => (task ? s.projects[task.project_id] : undefined));
  const connected = useStore((s) => s.connected);
  const activity = useStore((s) => s.taskActivity[id] ?? 0);
  const [tab, setTab] = useState<"transcript" | "changes" | "why">("transcript");
  const [missing, setMissing] = useState(false);

  useEffect(() => {
    if (!task) refreshTask(id).catch(() => setMissing(true));
  }, [id, task]);

  if (!task) {
    return <div className="empty">{missing ? "This task does not exist on the daemon." : connected ? "Loading…" : "Waiting for the daemon…"}</div>;
  }
  const st = stateMeta(task.state);

  return (
    <>
      <div className="header">
        <a className="crumb" href={href({ name: "project", id: task.project_id })}>{project?.name ?? task.project_id}</a>
        <span className="faint">/</span>
        <h1 className="ellipsis" title={task.title}>{task.title}</h1>
        <span className="pill" style={{ color: st.color }} data-testid="task-state">{st.label}</span>
        {task.harness && <span className="harness-chip" data-testid="task-harness"><HarnessLogo harness={task.harness} size={14} />{harnessMark(task.harness).name}</span>}
        {task.pull_request_url && <a className="pill green" href={task.pull_request_url} target="_blank" rel="noreferrer">pull request</a>}
        <span className="spacer" />
        <Controls taskId={id} active={isActive(task.state)} />
      </div>
      {task.state_note && <div className="state-note">{task.state_note} <span className="faint">· {ago(task.updated_at)}</span></div>}
      <div className="screen worker">
        <section className="worker-left">
          <TerminalPanel taskId={id} taskState={task.state} />
          <SteerBox taskId={id} />
        </section>
        <section className="worker-right">
          <div className="tabs" role="tablist">
            <button role="tab" aria-selected={tab === "transcript"} className={tab === "transcript" ? "on" : ""} onClick={() => setTab("transcript")}>Transcript</button>
            <button role="tab" aria-selected={tab === "changes"} className={tab === "changes" ? "on" : ""} onClick={() => setTab("changes")}>Changes</button>
            <button role="tab" aria-selected={tab === "why"} className={tab === "why" ? "on" : ""} onClick={() => setTab("why")}>Why this agent</button>
          </div>
          <div className="tab-body">
            {tab === "transcript" && <TranscriptPanel taskId={id} />}
            {tab === "changes" && <ChangesPanel taskId={id} stateKey={task.state + task.updated_at + ":" + activity} />}
            {tab === "why" && <WhyThisAgent taskId={id} />}
          </div>
        </section>
      </div>
    </>
  );
}

// ---- cancel / relaunch ----

function Controls({ taskId, active }: { taskId: string; active: boolean }) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState<null | "cancel" | "relaunch">(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    if (!confirming) return;
    const t = setTimeout(() => setConfirming(false), 4000);
    return () => clearTimeout(t);
  }, [confirming]);

  const run = async (what: "cancel" | "relaunch") => {
    setBusy(what); setErr(null); setConfirming(false);
    try {
      await (what === "cancel" ? api.cancel(taskId) : api.relaunch(taskId));
    } catch (e) {
      setErr(e instanceof NotAvailable ? `${what === "cancel" ? "Cancel" : "Relaunch"} is not available from this daemon yet` : errText(e));
    } finally { setBusy(null); }
  };

  return (
    <div className="controls">
      {err && <span className="bad small-text" role="alert">{err}</span>}
      {confirming ? (
        <button className="btn danger" onClick={() => run("cancel")} data-testid="confirm-cancel">Confirm cancel</button>
      ) : (
        <button className="btn" disabled={!active || busy !== null} onClick={() => setConfirming(true)} data-testid="cancel">
          {busy === "cancel" ? "Cancelling…" : "Cancel"}
        </button>
      )}
      <button className="btn" disabled={busy !== null} onClick={() => run("relaunch")} data-testid="relaunch">
        {busy === "relaunch" ? "Relaunching…" : "Relaunch"}
      </button>
    </div>
  );
}

// ---- terminal ----

function useTermStatus(h: TermHandle) {
  const [, force] = useState(0);
  useEffect(() => {
    const f = () => force((n) => n + 1);
    h.onStatus.add(f);
    return () => { h.onStatus.delete(f); };
  }, [h]);
  return h.status;
}

function TerminalPanel({ taskId, taskState }: { taskId: string; taskState: string }) {
  const [gen, setGen] = useState(0);
  const h = useMemo(() => getTerm(taskId), [taskId, gen]);
  const status = useTermStatus(h);
  const body = useRef<HTMLDivElement>(null);

  // A task with no terminal gets one when its worker starts: look again
  // whenever its state changes.
  useEffect(() => {
    if (h.status === "none") { resetTerm(taskId); setGen((g) => g + 1); }
  }, [taskState]);

  useEffect(() => {
    if (!body.current) return;
    attach(h, body.current);
    const ro = new ResizeObserver(() => h.adapter?.fit());
    ro.observe(body.current);
    return () => ro.disconnect();
  }, [h]);

  return (
    <div className="pane" data-testid="terminal">
      <div className="pane-head">
        <span>terminal</span>
        {h.info && <span className="faint ellipsis">{h.info.title}</span>}
        <span className="spacer" />
        {status === "connecting" && <span className="faint">connecting…</span>}
        {status === "error" && (
          <>
            <span className="bad ellipsis" title={h.error ?? ""}>{h.error}</span>
            <button className="btn small" onClick={() => { resetTerm(taskId); setGen((g) => g + 1); }}>Retry</button>
          </>
        )}
      </div>
      {status === "none" ? (
        <div className="unavailable" data-testid="no-terminal"><div className="u-title">{h.error}</div></div>
      ) : status === "unavailable" ? (
        <div className="unavailable"><div className="u-title">{h.error}</div></div>
      ) : (
        <div className="pane-body" ref={body} />
      )}
    </div>
  );
}

// ---- steering ----

function SteerBox({ taskId }: { taskId: string }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);

  const send = async () => {
    const t = text.trim();
    if (!t || busy) return;
    setBusy(true); setNote(null);
    try {
      await api.steer(taskId, t);
      setText("");
      setNote({ ok: true, text: "Delivered to the worker's inbox" });
    } catch (e) {
      setNote({ ok: false, text: e instanceof NotAvailable ? "Messaging a worker is not available from this daemon yet" : errText(e) });
    } finally { setBusy(false); }
  };

  return (
    <div className="composer steer">
      <textarea rows={2} value={text} aria-label="Message the worker"
        placeholder="Message the worker (Enter to send, Shift+Enter for a newline)"
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); void send(); }
        }} />
      <div className="steer-side">
        <button className="btn" onClick={() => void send()} disabled={busy || !text.trim()}>{busy ? "Sending…" : "Send"}</button>
        {note && <span className={(note.ok ? "ok" : "bad") + " small-text"} role="status">{note.text}</span>}
      </div>
    </div>
  );
}

// ---- transcript ----

const NO_ENTRIES: TranscriptItem[] = [];

function TranscriptPanel({ taskId }: { taskId: string }) {
  const entries = useStore((s) => s.transcripts[taskId]) ?? NO_ENTRIES;
  const [status, setStatus] = useState<"loading" | "ok" | "unavailable" | "error">("loading");
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    loadTranscript(taskId).then(setStatus).catch((e) => { setStatus("error"); setErr(errText(e)); });
  }, [taskId]);

  if (status === "unavailable") return <Unavailable what="The transcript" endpoint={`GET /v1/tasks/${taskId}/transcript`} />;
  return (
    <Transcript key={taskId} items={entries} agent="worker" testId="transcript" className="transcript"
      empty={status === "ok" ? <div className="empty">No transcript yet.</div> : status === "loading" ? <div className="empty">Loading…</div> : null}>
      {status === "error" && <div className="form-error">{err}</div>}
    </Transcript>
  );
}

// ---- changes and diff ----

function ChangesPanel({ taskId, stateKey }: { taskId: string; stateKey: string }) {
  const [changes, setChanges] = useState<TaskChanges | null>(null);
  const [status, setStatus] = useState<"loading" | "ok" | "unavailable" | "no-worktree" | "error">("loading");
  const [err, setErr] = useState<string | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [diff, setDiff] = useState<DiffFile[] | null>(null);
  const [diffErr, setDiffErr] = useState<string | null>(null);

  const load = useCallback(() => {
    api.changes(taskId)
      .then((c) => { setChanges(c); setStatus("ok"); })
      .catch((e) => {
        if (e instanceof NotAvailable) setStatus("unavailable");
        else if (e instanceof ApiError && e.code === "no_worktree") { setStatus("no-worktree"); setChanges(null); }
        else { setStatus("error"); setErr(errText(e)); }
      });
  }, [taskId]);
  // Refetch on every state change of the task, and on demand.
  useEffect(load, [load, stateKey]);

  const files = changes?.files ?? [];
  const path = selected && files.some((f) => f.path === selected) ? selected : files[0]?.path ?? null;

  useEffect(() => {
    if (!path) { setDiff(null); return; }
    let live = true;
    setDiffErr(null);
    api.diff(taskId, path)
      .then((d) => { if (live) { setDiff(parseUnifiedDiff(d.patch)); setTruncated(d.truncated); } })
      .catch((e) => { if (live) { setDiff(null); setDiffErr(e instanceof NotAvailable ? "The diff is not available from this daemon yet" : errText(e)); } });
    return () => { live = false; };
  }, [taskId, path, changes]);

  if (status === "unavailable") return <Unavailable what="Changed files" endpoint={`GET /v1/tasks/${taskId}/changes`} />;
  return (
    <div className="changes" data-testid="changes">
      <div className="change-files">
        <div className="panel-head">
          {files.length} changed {files.length === 1 ? "file" : "files"}
          {changes && <span className="faint mono"> vs {changes.base_ref} ({changes.base.slice(0, 7)})</span>}
          <span className="spacer" />
          <button className="btn small" onClick={load}>Refresh</button>
        </div>
        {status === "error" && <div className="form-error">{err}</div>}
        {files.map((f) => (
          <button key={f.path} className={"file-row" + (f.path === path ? " on" : "")} onClick={() => setSelected(f.path)} title={f.path}>
            <span className={"st st-" + f.status} title={f.status.replace("_", " ")}>{STATUS_LETTER[f.status] ?? "?"}</span>
            <span className="ellipsis mono">{f.path}</span>
            {f.additions == null ? <span className="faint small-text">binary</span>
              : <><span className="adds">+{f.additions}</span><span className="dels">−{f.deletions ?? 0}</span></>}
          </button>
        ))}
        {status === "ok" && !files.length && <div className="empty">No changes yet.</div>}
        {status === "no-worktree" && <div className="empty">This task has no working copy yet.</div>}
      </div>
      <div className="diff-body">
        {diffErr && <div className="form-error">{diffErr}</div>}
        {truncated && <div className="state-note">This diff was cut at the daemon's size limit.</div>}
        {diff?.map((f, i) => <FileDiff key={f.path + i} f={f} />)}
        {diff && !diff.length && <div className="empty">No textual changes.</div>}
      </div>
    </div>
  );
}

const STATUS_LETTER: Record<string, string> = {
  added: "A", modified: "M", deleted: "D", renamed: "R", copied: "C", type_changed: "T", untracked: "U",
};
