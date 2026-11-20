// J3: the project's Agents tab (its coordinator) and Work tab (its board, live from the event stream).
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, CoordinatorCommand, Project, Task, TranscriptItem } from "../api";
import { clearWorkers, loadChat, noteOutbox, useStore } from "../store";
import { useOutboxPrompts } from "../components/CoordinatorOutbox";
import { errText, STATES } from "../util";
import { Unavailable } from "../components/Unavailable";
import { Composer } from "../components/Composer";
import { Button } from "../ui";
import { WorkerCard } from "../components/WorkerCard";
import { CommandCard, duration, groupTurns, lastId, liveStatus, LiveStatus, PendingMessage, PendingPrompt, stillPending, stillPendingCommands, Transcript, useNow } from "../components/transcript";
import { PersonaPicker } from "../components/PersonaPicker";
import { ContextMeter, CoordinatorAgent, useCoordinatorContext } from "../components/CoordinatorContext";
import { useLabels } from "../persona";
import { useDraft } from "../drafts";
import { readAttached, useAttachments, withAttachments } from "../attachments";
import { CommandButton, StopButton, usePendingCommands, useRunCommand, useStop, useStoppedThrough } from "../components/CoordinatorCommand";
import { commandLabel, fromSlash, HINTS, slashMatches } from "../commands";
import { Welcome } from "../components/Welcome";
import { ScreenPromptCard } from "../components/ScreenPromptCard";
import { coordinatorScreenPrompt, ScreenPrompt } from "../screenPrompt";
import { dockMessage } from "../shell/Dock";
import { BeadText } from "../ui/BeadText";
import { Agents } from "./Agents";
import type { Decision } from "../api";

const ALWAYS_SHOWN = new Set(["queued", "running", "needs_decision", "in_review", "done"]);

/** Prompts on the coordinator's screen that wait for a choice, oldest first: quarkd opens a
 * decision for each, and a button here picks the choice on that screen. */
function useCoordinatorPrompts(cid: string) {
  const decisions = useStore((s) => s.decisions);
  return useMemo(() => Object.values(decisions)
    .filter((d) => d.project_id === cid && d.state === "open")
    .map((d) => ({ d, p: coordinatorScreenPrompt(d) }))
    .filter((x): x is { d: Decision; p: ScreenPrompt } => x.p !== null)
    .sort((a, b) => a.d.opened_at.localeCompare(b.d.opened_at)), [decisions, cid]);
}

export function ProjectBoard({ id }: { id: string }) {
  const project = useStore((s) => s.projects[id]);
  const connected = useStore((s) => s.connected);
  const tasks = useStore((s) => s.tasks);
  const l = useLabels(id);
  const [notes, setNotes] = useState<Record<string, ClearResult | null>>({});

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
              <div className="col-head"><span className="swatch" style={{ background: c.color }} />{c.id === "needs_decision" ? `Needs ${l.role("decision")}` : c.label}<span className="n">{c.tasks.length}</span>
                {(c.id === "failed" || c.id === "done") && c.tasks.length > 0 && (
                  <ClearColumn pid={id} state={c.id} onNote={(n) => setNotes((m) => ({ ...m, [c.id]: n }))} />
                )}
              </div>
              <div className="col-body">
                {notes[c.id] && (c.id === "failed" || c.id === "done") && (
                  <ClearNote pid={id} state={c.id} note={notes[c.id]!} onNote={(n) => setNotes((m) => ({ ...m, [c.id]: n }))} />
                )}
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

/** The Agents tab, the project's home: its coordinator in the middle and the project panel beside it. */
export function Conversation({ id, worker }: { id: string; worker?: string }) {
  const project = useStore((s) => s.projects[id]);
  const connected = useStore((s) => s.connected);
  if (!project) return <Missing connected={connected} />;
  return (
    <>
      {project.status && project.status !== "ready" && <ProvisionBar project={project} />}
      <Agents pid={id} worker={worker} />
    </>
  );
}

/** Clears every worker in the done or failed column; ones whose working copy holds work stay, and the column says why. */
type ClearResult = { note: string; unlanded: number };

/** What Clear all did; when it kept workers for work that has not landed, a second, confirmed step discards that work. */
function ClearNote({ pid, state, note, onNote }: { pid: string; state: "done" | "failed"; note: ClearResult; onNote: (note: ClearResult | null) => void }) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!confirming) return;
    const t = setTimeout(() => setConfirming(false), 4000);
    return () => clearTimeout(t);
  }, [confirming]);
  const discard = async () => {
    setBusy(true); setConfirming(false);
    try { onNote(await clearWorkers(pid, state, true)); } catch (e) { onNote({ note: errText(e), unlanded: 0 }); } finally { setBusy(false); }
  };
  return (
    <div className="col-note faint" role="status" data-testid={`clear-${state}-note`}>
      {note.note}
      {note.unlanded > 0 && (
        <div>
          <button className="btn small danger" disabled={busy} data-testid={`discard-${state}`}
            title="Throw away their uncommitted changes, then clear them. Their commits stay on their branches."
            onClick={() => (confirming ? void discard() : setConfirming(true))}>
            {busy ? "Discarding…" : confirming ? "Confirm discard" : "Discard their work and clear"}
          </button>
        </div>
      )}
    </div>
  );
}

function ClearColumn({ pid, state, onNote }: { pid: string; state: "done" | "failed"; onNote: (note: ClearResult | null) => void }) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!confirming) return;
    const t = setTimeout(() => setConfirming(false), 4000);
    return () => clearTimeout(t);
  }, [confirming]);
  const run = async () => {
    setBusy(true); setConfirming(false); onNote(null);
    try { onNote(await clearWorkers(pid, state)); } catch (e) { onNote({ note: errText(e), unlanded: 0 }); } finally { setBusy(false); }
  };
  return (
    <button className={"btn small" + (confirming ? " danger" : "")} disabled={busy} data-testid={`clear-${state}`}
      title={`Close the terminals of every ${state} worker, give their working copies back and take them off the board. Their history is kept.`}
      onClick={() => (confirming ? void run() : setConfirming(true))}>
      {busy ? "Clearing…" : confirming ? "Confirm clear" : "Clear all"}
    </button>
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
    <div className={"state-note provision-bar" + (project.status === "failed" ? " bad" : "")} data-testid="provision-bar">
      <div className="provision-bar-text">
        {project.status === "failed" ? "Setting up this project failed" : "Setting up this project"}
        {project.status_detail && <>: {project.status_detail}</>}
        {err && <span className="bad"> {err}</span>}
      </div>
      {project.status === "failed" && (
        <Button kind="secondary" onClick={() => void retry()} disabled={busy} data-testid="provision-retry">
          {busy ? "Retrying…" : "Retry setup"}
        </Button>
      )}
    </div>
  );
}

const EMPTY: TranscriptItem[] = [];

/** The open worker as context on the coordinator's box: on, it names the worker in what you send; off, the message goes as typed. */
function AboutChip({ about, on, onToggle }: { about: ChatAbout; on: boolean; onToggle: () => void }) {
  return (
    <button type="button" className={"composer-tool about-chip" + (on ? " on" : "")} aria-pressed={on} onClick={onToggle} data-testid="coordinator-about"
      title={on ? "Messages say they are about this worker. Click to send without it." : "Click to say your message is about this worker"}>
      <span className="composer-tool-label ellipsis">{on ? "About " : "Not about "}<BeadText text={about.title} project={null} /></span>{on && <span aria-hidden="true"> ×</span>}
    </button>
  );
}

/** The coordinator's state as a pill in its header: working (with how long and on what), listening, idle, stopped, or your turn. */
function ChatStatus({ live, stopped }: { live: LiveStatus; stopped: boolean }) {
  const now = useNow(live.activity === "working", 1_000);
  const [tone, text, title] =
    stopped ? ["warn", "Stopped", "You pressed Stop; it reads your next message"]
    : live.activity === "working" ? ["busy", live.startedAt !== undefined ? `Working · ${duration(now - live.startedAt)}` : "Working", live.step ? `On: ${live.step}` : undefined]
    : live.activity === "listening" ? ["ok", "Listening", "Waiting for project events; costs nothing"]
    : live.activity === "quiet" ? ["idle", "Idle", "Nothing has been recorded for a while"]
    : ["idle", "Your turn", "It finished and waits for your next message"];
  return (
    <span className={"coord-status " + tone} data-testid="coordinator-status" title={title} role="status">
      {tone === "busy" ? <span className="spinner" aria-hidden="true" /> : <span className="coord-status-dot" aria-hidden="true" />}
      {text}
    </span>
  );
}

/** What the coordinator's box is about: the worker open beside it, with the link the coordinator follows. */
export type ChatAbout = { key: string; title: string; link: string };

/** `welcome`: show the welcome while the conversation is empty (the Agents tab; not the Issues side chat).
 * `about`: a worker open beside the chat; messages say they are about it until you remove the chip. */
export function CoordinatorChat({ cid, welcome = false, about = null }: { cid: string; welcome?: boolean; about?: ChatAbout | null }) {
  const project = useStore((s) => s.projects[cid]);
  const box = useRef<HTMLElement>(null);
  const loaded = useStore((s) => s.chat[cid]);
  const items = loaded ?? EMPTY;
  const [status, setStatus] = useState<"loading" | "ok" | "unavailable" | "error">("loading");
  const [err, setErr] = useState<string | null>(null);
  const [text, setText] = useDraft(`coordinator:${cid}`);
  const attach = useAttachments(cid, `coordinator:${cid}`);
  const typed = useRef(text);
  typed.current = text;
  // What you sent shows at once, then stays until the coordinator's session records it.
  const [pending, setPending] = useState<PendingMessage[]>([]);
  const [commandsPending, setCommandsPending] = usePendingCommands(cid);
  const l = useLabels(cid);
  const coordinator = l.role("coordinator");
  const commands = useRunCommand(cid);
  const stoppedThrough = useStoppedThrough(cid);
  const stopper = useStop(cid);
  const context = useCoordinatorContext(cid, items.length);
  const screenPrompts = useCoordinatorPrompts(cid);
  // What the coordinator is doing now, for the header's status and the composer's Stop.
  const turns = useMemo(() => groupTurns([...items].sort((a, b) => a.id - b.id), stoppedThrough), [items, stoppedThrough]);
  const now = useNow(true);
  const live = liveStatus(turns, now);
  const stopped = stoppedThrough !== undefined && lastId(items) === stoppedThrough;
  const busy = live.activity === "working";
  // Sent messages the daemon holds: queued while the coordinator works, being typed in, or not delivered.
  const outbox = useOutboxPrompts(cid, useStore((s) => s.outbox[cid]), items, pending, coordinator);
  const queueing = busy || outbox.waiting.some((m) => m.state === "queued");
  const placed = useMemo(() => [
    ...pending.map((p) => ({ after: p.after, text: p.text,
      note: p.sending ? "Sending…" : !p.confirmed ? "Typed, not confirmed. Check before sending again."
        : busy ? "Delivered · waiting for the current step to end" : "Delivered" })),
    ...outbox.prompts,
    // A command asked for shows as its card where it was asked, not after the replies.
    ...commandsPending.map((p): PendingPrompt => ({ after: p.after, text: commandLabel(l, p.command), note: null,
      card: <CommandCard key={p.key} note={{ command: p.command, asked: "you" }} state={p.sending ? "sending" : p.confirmed ? "sent" : "unconfirmed"} /> })),
  ].sort((a, b) => a.after - b.after), [pending, busy, outbox.prompts, commandsPending, l]);
  const [dropped, setDropped] = useState<string | null>(null);
  const aboutOn = about && dropped !== about.key ? about : null;
  const matches = slashMatches(text);
  const label = useCallback((c: CoordinatorCommand) => commandLabel(l, c), [l]);

  useEffect(() => {
    loadChat(cid).then(setStatus).catch((e) => { setStatus("error"); setErr(errText(e)); });
  }, [cid]);

  // Drop a pending message or command once the log records it.
  useEffect(() => {
    const left = stillPending(pending, items);
    if (left !== pending) setPending(left);
  }, [items, pending]);
  useEffect(() => {
    const left = stillPendingCommands(commandsPending, items);
    if (left !== commandsPending) setCommandsPending(left);
  }, [items, commandsPending, setCommandsPending]);

  const ask = async (command: CoordinatorCommand) => {
    const draft = text;
    setText("");
    if (!(await commands.run(command)) && !typed.current.trim()) setText(draft);
  };

  const send = async () => {
    const typedText = text.trim();
    const files = attach.items;
    if ((!typedText && !files.length) || attach.uploading) return;
    // A slash command is a request, not a message: it shows as its command card.
    const command = files.length ? null : fromSlash(typedText);
    if (command) return ask(command);
    const t = dockMessage(withAttachments(typedText, files), { projectId: cid, pinned: true, about: aboutOn?.title ?? null, link: aboutOn?.link ?? null });
    setErr(null);
    // Only entries recorded after this point can be this message.
    const key = Date.now() + Math.random();
    setPending((p) => [...p, { key, text: t, sending: true, confirmed: false, after: lastId(items) }]);
    setText("");
    attach.clear();
    try {
      // While the coordinator works it waits, editable, until the step ends.
      const r = await api.sendChat(cid, t, queueing);
      if (r?.message_id && r.state) {
        // The daemon recorded it: the transcript (or its outbox) shows it from here.
        noteOutbox({ id: r.message_id, coordinator_id: cid, text: t, state: r.state, created_at: r.accepted_at, updated_at: r.accepted_at });
        setPending((p) => p.filter((x) => x.key !== key));
        return;
      }
      setPending((p) => p.map((x) => x.key === key ? { ...x, sending: false, confirmed: r?.confirmed ?? true } : x));
    } catch (e) {
      setErr(errText(e));
      setPending((p) => p.filter((x) => x.key !== key));
      // Give the text and files back unless something new was typed or attached meanwhile.
      if (!typed.current.trim()) setText(typedText);
      if (!readAttached(`coordinator:${cid}`).length) attach.set(files);
    }
  };

  return (
    <aside className="side-chat" data-testid="coordinator-chat" ref={box}>
      <div className="coord-head">
        <span className="coord-avatar" aria-hidden="true">{l.Role("coordinator").slice(0, 1)}</span>
        <div className="coord-title">
          <span className="coord-name">{l.Role("coordinator")}</span>
          <CoordinatorAgent ctx={context.ctx} />
        </div>
        <ContextMeter cid={cid} ctx={context.ctx} load={context.load} />
        <ChatStatus live={live} stopped={stopped} />
        <span className="spacer" />
        <StopButton cid={cid} /><CommandButton cid={cid} command="wrap_up" /><PersonaPicker projectId={cid} />
      </div>
      {status === "unavailable" ? (
        <Unavailable what="Coordinator chat" endpoint={`GET /v1/coordinators/${cid}/messages`} />
      ) : (
        <>
          <Transcript key={cid} items={items} pending={placed} agent={coordinator} actions project={cid} className="chat-log" commandLabel={label} stoppedThrough={stoppedThrough}
            empty={status === "ok" ? (welcome && project
              ? <Welcome project={project} onSuggest={(t) => { setText(t); box.current?.querySelector("textarea")?.focus(); }} />
              : <div className="empty">Ask the {coordinator} to plan or delegate work.</div>)
              : status === "loading" ? <div className="empty">Loading…</div> : null} />
          {(err ?? outbox.err ?? commands.err ?? stopper.err) && <div className="form-error">{err ?? outbox.err ?? commands.err ?? stopper.err}</div>}
          {matches.length > 0 && (
            <div className="slash-menu" role="listbox" aria-label="Slash commands" data-testid="slash-menu">
              {matches.map((m) => (
                <button key={m.command} role="option" aria-selected={false} className="slash-item" disabled={commands.busy !== null}
                  onClick={() => void ask(m.command)}>
                  <span className="mono">/{m.alias}</span><b>{label(m.command)}</b><span className="faint">{HINTS[m.command]}</span>
                </button>
              ))}
            </div>
          )}
          <Composer value={text} onChange={setText} onSend={() => void send()} sending={false} attachments={attach}
            above={screenPrompts.length ? screenPrompts.map(({ d, p }) => <ScreenPromptCard key={d.id} d={d} p={p} who={coordinator} />) : null}
            label={`Message the ${coordinator}`} placeholder={aboutOn ? `Ask the ${coordinator} about this ${l.role("worker")}` : `Ask the ${coordinator} to plan, start or check work`}
            tools={<>{about && <AboutChip about={about} on={!!aboutOn} onToggle={() => setDropped(aboutOn ? about.key : null)} />}<button type="button" className="composer-tool" aria-label="Commands" title="Catch up, status report, wrap up" data-testid="composer-commands"
              onClick={() => { setText(text.startsWith("/") ? "" : "/"); box.current?.querySelector("textarea")?.focus(); }}>
              <span className="mono">/</span><span className="composer-tool-label">Commands</span></button></>}
            hint={<><kbd>↵</kbd> {queueing ? "queues it until this step ends" : "sends"} · <kbd>⇧↵</kbd> new line</>}
            onStop={busy || live.activity === "quiet" ? () => void stopper.stop() : undefined} stopping={stopper.resting} />
        </>
      )}
    </aside>
  );
}
