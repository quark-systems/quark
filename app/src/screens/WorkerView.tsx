// J4: one worker, opened as a tab in the Agents tab's project panel beside the coordinator. In the
// panel its views are tabs: its live terminal (view only until you take it over), transcript,
// changed files with their diff, PR, brief and why this agent; its message box sits under them and
// prompts on the worker's screen show as buttons above it. Expanded to the full width, the terminal
// and box take the middle and the other views a work pane on the right. Cancel, relaunch and clear
// are in the header. A cleared worker has no terminal: its main view is its whole transcript, read only.
import { KeyboardEvent, PointerEvent as ReactPointerEvent, ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError, Decision, NotAvailable, Task, TaskChanges, TranscriptItem } from "../api";
import { href } from "../nav";
import { externalLink } from "../links";
import { loadFullTranscript, loadTranscript, refreshTask, useStore } from "../store";
import { attach, getTerm, resetTerm, setTyping, TermHandle } from "../terminal";
import { ago, errText, isActive, stateMeta } from "../util";
import { DiffFile, parseUnifiedDiff } from "../components/diff/model";
import { FileDiff } from "../components/diff/DiffView";
import { Unavailable } from "../components/Unavailable";
import { Composer } from "../components/Composer";
import { screenPrompt, ScreenPrompt } from "../screenPrompt";
import { ScreenPromptCard } from "../components/ScreenPromptCard";
import { Transcript } from "../components/transcript";
import { WhyThisAgent } from "../components/WhyThisAgent";
import { WorkerBrief } from "../components/WorkerBrief";
import { HarnessLogo, harnessMark } from "../components/WorkerCard";
import { useLabels } from "../persona";
import { MOD } from "../shell/Dock";
import { cx, StatusDot, taskTone, TONE_LABEL } from "../ui";
import { useDraft } from "../drafts";
import { useAttachments, withAttachments } from "../attachments";
import { BeadText } from "../ui/BeadText";

/** How the worker sits in the project panel: `expanded` gives it the full width; `onExpand` toggles that. */
type Fit = { expanded: boolean; onExpand: (expanded: boolean) => void };

export function WorkerView({ id, fit }: { id: string; fit: Fit }) {
  const task = useStore((s) => s.tasks[id]);
  const connected = useStore((s) => s.connected);
  const [gone, setGone] = useState<null | "missing" | Task>(null);

  useEffect(() => {
    if (!task) refreshTask(id).then((t) => setGone(t.cleared_at ? t : "missing")).catch(() => setGone("missing"));
    else setGone(null);
  }, [id, task]);

  if (task) return <LiveWorker key={String(fit.expanded)} task={task} fit={fit} />;
  if (gone && gone !== "missing" && gone.id === id) return <ClearedWorker key={String(fit.expanded)} task={gone} fit={fit} />;
  const why = gone === "missing" ? "This task does not exist on the daemon." : connected ? "Loading…" : "Waiting for the daemon…";
  return <div className="empty">{why}</div>;
}

type PaneTab = "terminal" | "transcript" | "changes" | "pr" | "brief" | "why";

/** Expand to the full width, or back into the panel beside the coordinator. */
function ExpandButton({ fit }: { fit: Fit }) {
  return (
    <button className="btn icon-btn" onClick={() => fit.onExpand(!fit.expanded)} data-testid="worker-expand"
      aria-label={fit.expanded ? "Show the coordinator" : "Expand"} title={fit.expanded ? "Back beside the coordinator" : "Give this worker the full width"}>
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
        {fit.expanded ? <path d="M4 14h6v6M20 10h-6V4M14 10l7-7M3 21l7-7" /> : <path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7" />}
      </svg>
    </button>
  );
}

function LiveWorker({ task, fit }: { task: Task; fit: Fit }) {
  const id = task.id;
  const activity = useStore((s) => s.taskActivity[id] ?? 0);
  const [tab, setTab] = useState<PaneTab>(fit.expanded ? "transcript" : "terminal");
  const pr = useStore((s) => Object.values(s.pullRequests).find((p) => p.task_id === id));
  const [paneWidth, setPaneWidth] = useWorkPaneWidth();
  const views: PaneTab[] = ["transcript", "changes", ...(pr || task.pull_request_url ? ["pr" as const] : []), "brief", "why"];
  const body = (t: PaneTab) => <>
    {t === "transcript" && <div className="tab-body"><TranscriptPanel taskId={id} /></div>}
    {t === "changes" && <div className="tab-body"><ChangesPanel taskId={id} stateKey={task.state + task.updated_at + ":" + activity} /></div>}
    {t === "pr" && <div className="tab-body"><PrPanel prId={pr?.id ?? null} url={task.pull_request_url ?? pr?.url ?? null} /></div>}
    {t === "brief" && <div className="tab-body"><WorkerBrief taskId={id} /></div>}
    {t === "why" && <div className="tab-body"><WhyThisAgent taskId={id} /></div>}
  </>;

  if (!fit.expanded) {
    return (
      <div className="worker-panel" data-testid="worker-view">
        <WorkerHead task={task}><Controls taskId={id} active={isActive(task.state)} /><ExpandButton fit={fit} /></WorkerHead>
        {task.state_note && <div className="state-note">{task.state_note} <span className="faint">· {ago(task.updated_at)}</span></div>}
        <PaneTabs tab={tab} setTab={setTab} tabs={["terminal", ...views]} />
        {tab === "terminal" ? <TerminalPanel taskId={id} taskState={task.state} /> : body(tab)}
        <WorkerComposer taskId={id} />
      </div>
    );
  }

  const paneTab = tab === "terminal" ? "transcript" : tab;
  return (
    <div className="screen worker" data-testid="worker-view" style={paneWidth ? { ["--work-pane-width" as string]: `${paneWidth}px` } : undefined}>
      <section className="worker-center">
        <WorkerHead task={task}><Controls taskId={id} active={isActive(task.state)} /><ExpandButton fit={fit} /></WorkerHead>
        {task.state_note && <div className="state-note">{task.state_note} <span className="faint">· {ago(task.updated_at)}</span></div>}
        <TerminalPanel taskId={id} taskState={task.state} />
        {/* One box under the worker's terminal: to the worker, or to the coordinator about it. */}
        <WorkerComposer taskId={id} />
      </section>
      <aside className="worker-pane" aria-label="Work">
        <PaneResizer width={paneWidth} onWidth={setPaneWidth} />
        <PaneTabs tab={paneTab} setTab={setTab} tabs={views} />
        {body(paneTab)}
      </aside>
    </div>
  );
}

/**
 * A cleared worker: its terminal is closed and its working copy given back, so the center is its
 * whole transcript, read only. The work pane keeps what outlives the working copy.
 */
function ClearedWorker({ task, fit }: { task: Task; fit: Fit }) {
  const id = task.id;
  const [tab, setTab] = useState<PaneTab>(fit.expanded ? "brief" : "transcript");
  const pr = useStore((s) => Object.values(s.pullRequests).find((p) => p.task_id === id));
  const [paneWidth, setPaneWidth] = useWorkPaneWidth();
  const cleared = (
    <div className="state-note" data-testid="worker-cleared">
      Cleared{task.cleared_at ? ` ${ago(task.cleared_at)}` : ""}. Its terminal was closed and its working copy given back; its transcript is kept.
    </div>
  );
  const transcript = (
    <div className="pane" data-testid="cleared-transcript">
      <div className="pane-head"><span>transcript</span><span className="spacer" /><span className="faint">read only</span></div>
      <TranscriptPanel taskId={id} full projectId={task.project_id} />
    </div>
  );
  const views: PaneTab[] = [...(pr || task.pull_request_url ? ["pr" as const] : []), "brief", "why"];

  if (!fit.expanded) {
    return (
      <div className="worker-panel" data-testid="worker-view">
        <WorkerHead task={task}><ExpandButton fit={fit} /></WorkerHead>
        {cleared}
        <PaneTabs tab={tab} setTab={setTab} tabs={["transcript", ...views]} />
        {tab === "transcript" ? transcript : <>
          {tab === "pr" && <div className="tab-body"><PrPanel prId={pr?.id ?? null} url={task.pull_request_url ?? pr?.url ?? null} /></div>}
          {tab === "brief" && <div className="tab-body"><WorkerBrief taskId={id} /></div>}
          {tab === "why" && <div className="tab-body"><WhyThisAgent taskId={id} /></div>}
        </>}
      </div>
    );
  }

  const paneTab = tab === "transcript" ? "brief" : tab;
  return (
    <div className="screen worker" data-testid="worker-view" style={paneWidth ? { ["--work-pane-width" as string]: `${paneWidth}px` } : undefined}>
      <section className="worker-center">
        <WorkerHead task={task}><ExpandButton fit={fit} /></WorkerHead>
        {cleared}
        {transcript}
      </section>
      <aside className="worker-pane" aria-label="Work">
        <PaneResizer width={paneWidth} onWidth={setPaneWidth} />
        <PaneTabs tab={paneTab} setTab={setTab} tabs={views} />
        {paneTab === "pr" && <div className="tab-body"><PrPanel prId={pr?.id ?? null} url={task.pull_request_url ?? pr?.url ?? null} /></div>}
        {paneTab === "brief" && <div className="tab-body"><WorkerBrief taskId={id} /></div>}
        {paneTab === "why" && <div className="tab-body"><WhyThisAgent taskId={id} /></div>}
      </aside>
    </div>
  );
}

const TAB_LABEL: Record<PaneTab, string> = { terminal: "Terminal", transcript: "Transcript", changes: "Changes", pr: "PR", brief: "Brief", why: "Why this agent" };

function PaneTabs({ tab, setTab, tabs }: { tab: PaneTab; setTab: (t: PaneTab) => void; tabs: PaneTab[] }) {
  return (
    <div className="tabs" role="tablist">
      {tabs.map((t) => (
        <button key={t} role="tab" aria-selected={tab === t} className={tab === t ? "on" : ""} onClick={() => setTab(t)}>{TAB_LABEL[t]}</button>
      ))}
    </div>
  );
}

function WorkerHead({ task, children }: { task: Task; children?: ReactNode }) {
  const st = stateMeta(task.state);
  return (
    <div className="header worker-head">
      <StatusDot tone={taskTone(task.state)} label={TONE_LABEL[taskTone(task.state)]} />
      <div className="wh-text">
        <h1 className="ellipsis" title={task.title}><BeadText text={task.title} project={task.project_id} link /></h1>
        <span className="wh-sub ellipsis">
          {task.branch ? <>branch <span className="mono">{task.branch}</span> · </> : null}
          started {ago(task.created_at)}
        </span>
      </div>
      <span className="pill" style={{ color: st.color }} data-testid="task-state">{st.label}</span>
      {task.harness && (
        <span className="harness-chip" data-testid="task-harness"><HarnessLogo harness={task.harness} size={14} />{harnessMark(task.harness).name}{task.model && <span className="faint"> · {task.model}</span>}</span>
      )}
      {task.pull_request_url && <a className="pill green" {...externalLink(task.pull_request_url)} data-testid="worker-pr-link">pull request</a>}
      {children}
    </div>
  );
}

// ---- work pane width ----

const PANE_WIDTH_KEY = "quark.workPaneWidth";
const MIN_PANE = 360;
const MIN_CENTER = 360;

function useWorkPaneWidth(): [number | null, (w: number | null) => void] {
  const [w, setW] = useState<number | null>(() => {
    try {
      const v = Number(localStorage.getItem(PANE_WIDTH_KEY));
      return v >= MIN_PANE ? v : null;
    } catch { return null; }
  });
  const set = useCallback((v: number | null) => {
    setW(v);
    try {
      if (v == null) localStorage.removeItem(PANE_WIDTH_KEY);
      else localStorage.setItem(PANE_WIDTH_KEY, String(Math.round(v)));
    } catch { /* not remembered */ }
  }, []);
  return [w, set];
}

/** Drag the work pane's left edge to widen it; double-click to reset. The width is remembered. */
function PaneResizer({ width, onWidth }: { width: number | null; onWidth: (w: number | null) => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const clamp = (w: number) => {
    const screen = ref.current?.closest(".worker")?.getBoundingClientRect();
    const max = screen ? screen.width - MIN_CENTER : w;
    return Math.max(MIN_PANE, Math.min(w, max));
  };
  const start = (e: ReactPointerEvent) => {
    const pane = ref.current?.parentElement;
    if (!pane) return;
    e.preventDefault();
    const right = pane.getBoundingClientRect().right;
    const move = (ev: PointerEvent) => onWidth(clamp(right - ev.clientX));
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      document.body.classList.remove("resizing");
    };
    document.body.classList.add("resizing");
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };
  const key = (e: KeyboardEvent) => {
    const cur = width ?? ref.current?.parentElement?.getBoundingClientRect().width ?? MIN_PANE;
    if (e.key === "ArrowLeft") { e.preventDefault(); onWidth(clamp(cur + 40)); }
    else if (e.key === "ArrowRight") { e.preventDefault(); onWidth(clamp(cur - 40)); }
  };
  return (
    <div ref={ref} className="pane-resizer" role="separator" aria-orientation="vertical" aria-label="Resize the work pane"
      tabIndex={0} title="Drag to resize · double-click to reset" data-testid="pane-resizer"
      onPointerDown={start} onDoubleClick={() => onWidth(null)} onKeyDown={key} />
  );
}

/** The worker's PR in brief, with a link to its full view in the PR center. */
function PrPanel({ prId, url }: { prId: string | null; url: string | null }) {
  const pr = useStore((s) => (prId ? s.pullRequests[prId] : undefined));
  return (
    <div className="side-pad pr-brief" data-testid="worker-pr">
      {pr ? (
        <>
          <div className="pr-brief-title">{pr.title ?? `${pr.repo}#${pr.number}`} <span className="faint mono">#{pr.number}</span></div>
          <div className="faint">{pr.state} · checks {pr.checks_state} · {pr.repo}</div>
          <a className="btn" href={href({ name: "pulls", project: pr.project_id, id: pr.id })}>Open the PR</a>
        </>
      ) : url ? <a href={url} target="_blank" rel="noreferrer">{url}</a> : null}
    </div>
  );
}

// ---- cancel / relaunch / clear ----

type Control = "cancel" | "relaunch" | "clear" | "discard";
const CONTROL_LABEL: Record<Control, string> = { cancel: "Cancel", relaunch: "Relaunch", clear: "Clear", discard: "Discard work and clear" };

function Controls({ taskId, active }: { taskId: string; active: boolean }) {
  const [confirming, setConfirming] = useState<null | "cancel" | "clear" | "discard">(null);
  const [busy, setBusy] = useState<null | Control>(null);
  const [err, setErr] = useState<string | null>(null);
  // Clear refused because the working copy holds work that has not landed.
  const [unlanded, setUnlanded] = useState(false);
  useEffect(() => { if (active) setUnlanded(false); }, [active]);

  useEffect(() => {
    if (!confirming) return;
    const t = setTimeout(() => setConfirming(null), 4000);
    return () => clearTimeout(t);
  }, [confirming]);

  const run = async (what: Control) => {
    setBusy(what); setErr(null); setConfirming(null);
    try {
      await (what === "cancel" ? api.cancel(taskId) : what === "clear" || what === "discard" ? api.clearTask(taskId, what === "discard") : api.relaunch(taskId));
    } catch (e) {
      if (what === "clear" && e instanceof ApiError && e.code === "unlanded_work") setUnlanded(true);
      setErr(e instanceof NotAvailable ? `${CONTROL_LABEL[what]} is not available from this daemon yet` : errText(e));
    } finally { setBusy(null); }
  };

  return (
    <div className="controls">
      {err && <span className="bad small-text" role="alert">{err}</span>}
      {active ? confirming === "cancel" ? (
        <button className="btn danger" onClick={() => run("cancel")} data-testid="confirm-cancel">Confirm cancel</button>
      ) : (
        <button className="btn" disabled={busy !== null} onClick={() => setConfirming("cancel")} data-testid="cancel">
          {busy === "cancel" ? "Cancelling…" : "Cancel"}
        </button>
      ) : confirming === "clear" ? (
        <button className="btn danger" onClick={() => run("clear")} data-testid="confirm-clear"
          title="Closes its terminal, gives its working copy back and takes it off the board">Confirm clear</button>
      ) : (
        <button className="btn" disabled={busy !== null} onClick={() => setConfirming("clear")} data-testid="clear"
          title="Close its terminal, give its working copy back and take it off the board. Its history is kept.">
          {busy === "clear" ? "Clearing…" : "Clear"}
        </button>
      )}
      {!active && unlanded && (confirming === "discard" ? (
        <button className="btn danger" onClick={() => run("discard")} data-testid="confirm-discard">Confirm discard</button>
      ) : (
        <button className="btn danger" disabled={busy !== null} onClick={() => setConfirming("discard")} data-testid="discard"
          title="Throw away its uncommitted changes, then clear it. Its commits stay on its branch.">
          {busy === "discard" ? "Discarding…" : "Discard work and clear"}
        </button>
      ))}
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

  // Leaving the worker gives the keyboard back: the terminal opens view only every time.
  useEffect(() => () => setTyping(h, false), [h]);

  useEffect(() => {
    if (!body.current) return;
    attach(h, body.current);
    const ro = new ResizeObserver(() => h.adapter?.fit());
    ro.observe(body.current);
    return () => ro.disconnect();
  }, [h]);

  return (
    <div className={cx("pane", h.typing && "term-typing")} data-testid="terminal">
      <div className="pane-head">
        <span>terminal</span>
        {h.info && <span className="faint ellipsis">{h.info.title}</span>}
        <span className="spacer" />
        {status === "live" && (h.typing ? (
          <>
            <span className="term-typing-note" data-testid="term-typing-note">Typing straight to the agent · not recorded</span>
            <button className="btn small" onClick={() => setTyping(h, false)} data-testid="term-release">Done</button>
          </>
        ) : (
          <button className="btn small quiet" onClick={() => setTyping(h, true)} data-testid="term-takeover"
            title="Type into the terminal yourself, for a dialog or menu that needs real keys. Use the message box below for everything else.">
            View only · Take over
          </button>
        ))}
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

// ---- the worker's message box ----

const STOP_REST_MS = 3_000;

/** The worker's open screen prompts: permission requests or menus quarkd read off its terminal. */
function useScreenPrompts(taskId: string) {
  const decisions = useStore((s) => s.decisions);
  return useMemo(() => Object.values(decisions)
    .filter((d) => d.task_id === taskId && d.state === "open")
    .map((d) => ({ d, p: screenPrompt(d.question) }))
    .filter((x): x is { d: Decision; p: ScreenPrompt } => x.p !== null)
    .sort((a, b) => a.d.opened_at.localeCompare(b.d.opened_at)), [decisions, taskId]);
}

/** Writes to the worker; the coordinator's own box sits beside it and already knows which worker is open. */
function WorkerComposer({ taskId }: { taskId: string }) {
  const task = useStore((s) => s.tasks[taskId]);
  const pid = task?.project_id;
  const labels = useLabels(pid);
  const worker = labels.role("worker");
  const coordinator = labels.role("coordinator");
  const [text, setText] = useDraft(`worker:${taskId}`);
  const attach = useAttachments(pid, `worker:${taskId}`);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ ok: boolean; text: ReactNode } | null>(null);
  const [resting, setResting] = useState(false);
  const prompts = useScreenPrompts(taskId);
  const active = !!task && isActive(task.state);

  const send = async () => {
    const t = text.trim();
    if ((!t && !attach.items.length) || busy || attach.uploading || !pid) return;
    setBusy(true); setNote(null);
    try {
      await api.steer(taskId, withAttachments(t, attach.items));
      setNote({ ok: true, text: prompts.length ? `Sent. The ${worker} gets it once the prompt above is answered` : `Delivered to the ${worker}'s inbox` });
      setText(""); attach.clear();
    } catch (e) {
      setNote({ ok: false, text: e instanceof NotAvailable ? `Messaging a ${worker} is not available from this daemon yet` : errText(e) });
    } finally { setBusy(false); }
  };

  const stopWorker = async () => {
    setResting(true); setNote(null);
    try { await api.interruptTask(taskId); }
    catch (e) { setNote({ ok: false, text: e instanceof NotAvailable ? `Stopping a ${worker} is not available from this daemon yet` : errText(e) }); }
    finally { setTimeout(() => setResting(false), STOP_REST_MS); }
  };

  const placeholder = prompts.length ? `Message the ${worker}… it waits until the prompt above is answered` : `Message the ${worker}…`;

  return (
    <div className="worker-composer" data-testid="worker-composer">
      <Composer value={text} onChange={(v) => { setText(v); setNote(null); }} onSend={() => void send()} sending={busy}
        className="steer" label={`Message the ${worker}`} placeholder={placeholder} attachments={attach}
        above={prompts.length ? prompts.map(({ d, p }) => <ScreenPromptCard key={d.id} d={d} p={p} who={worker} />) : null}
        hint={note ? <span className={note.ok ? "ok" : "bad"} role="status">{note.text}</span>
          : <>Enter sends · <kbd>{MOD}K</kbd> asks the {coordinator} about it</>}
        onStop={active ? () => void stopWorker() : undefined} stopping={resting} />
    </div>
  );
}

// ---- transcript ----

const NO_ENTRIES: TranscriptItem[] = [];

/** A worker's transcript; `full` reads it back from its first entry, for a cleared worker. */
function TranscriptPanel({ taskId, full = false, projectId }: { taskId: string; full?: boolean; projectId?: string }) {
  const entries = useStore((s) => s.transcripts[taskId]) ?? NO_ENTRIES;
  const project = useStore((s) => s.tasks[taskId]?.project_id) ?? projectId;
  const [status, setStatus] = useState<"loading" | "ok" | "unavailable" | "error">("loading");
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    (full ? loadFullTranscript : loadTranscript)(taskId).then(setStatus).catch((e) => { setStatus("error"); setErr(errText(e)); });
  }, [taskId, full]);

  if (status === "unavailable") return <Unavailable what="The transcript" endpoint={`GET /v1/tasks/${taskId}/transcript`} />;
  return (
    <Transcript key={taskId} items={entries} agent="worker" project={project} testId="transcript" className="transcript"
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
