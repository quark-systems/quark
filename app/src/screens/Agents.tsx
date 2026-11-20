// The Agents tab: the coordinator in the middle, always, and the project panel on the right. The
// panel's tabs are Work (this project's workers by state), Needs you, PRs and Activity, then one tab
// per worker you opened. A worker opens as a tab beside the coordinator; expanded, it takes the
// whole width. Layout after Cursor's projects view.
import React, { KeyboardEvent, PointerEvent as ReactPointerEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Task } from "../api";
import { go, href } from "../nav";
import { refreshTask, useStore } from "../store";
import { ago, STATES } from "../util";
import { useLabels } from "../persona";
import { CoordinatorMark, CountBadge, cx, StatusDot, taskTone } from "../ui";
import { BeadText } from "../ui/BeadText";
import { HarnessLogo, harnessMark } from "../components/WorkerCard";
import { ActivityPanel, NeedsYouPanel, OpenPrsPanel, useNeedsYou, useOpenPrs } from "../shell/SinceYouLooked";
import { afterClose, closeWorker, openWorker, PanelTab, setExpanded, setPanelTab, usePanel } from "../shell/projectPanel";
import { CoordinatorChat } from "./ProjectBoard";
import { WorkerView } from "./WorkerView";

const TAB_LABEL: Record<PanelTab, string> = { work: "Work", needs: "Needs you", prs: "PRs", activity: "Activity" };

/** `worker`: the worker open in the panel (from the URL), if any. */
export function Agents({ pid, worker }: { pid: string; worker?: string }) {
  const panel = usePanel(pid);
  const tasks = useStore((s) => s.tasks);
  const l = useLabels(pid);
  const [width, setWidth] = usePanelWidth(worker ? "worker" : "panel");

  useEffect(() => { if (worker) openWorker(pid, worker); }, [pid, worker]);

  const open = worker && !panel.workers.includes(worker) ? [...panel.workers, worker] : panel.workers;
  const expanded = !!worker && panel.expanded;
  const task = worker ? tasks[worker] : undefined;
  const about = useMemo(() => worker ? { key: worker, title: task?.title ?? `this ${l.role("worker")}`, link: href({ name: "task", id: worker }) } : null,
    [worker, task?.title, l]);

  const showTab = (t: PanelTab) => { setPanelTab(pid, t); if (worker) go({ name: "project", id: pid }); };
  const close = (id: string) => {
    const next = id === worker ? afterClose(open, id) : undefined;
    closeWorker(pid, id);
    if (next !== undefined) go(next ? { name: "task", id: next } : { name: "project", id: pid });
  };

  return (
    <div className={cx("agents", worker && "has-worker", expanded && "expanded")} data-testid="agents"
      style={width ? { ["--project-panel-width" as string]: `${width}px` } : undefined}>
      {expanded ? (
        <button className="agents-coord-rail" onClick={() => setExpanded(pid, false)} data-testid="show-coordinator"
          aria-label={`Show the ${l.role("coordinator")}`} title={`Show the ${l.role("coordinator")} beside this ${l.role("worker")}`}>
          <CoordinatorMark size={22} />
        </button>
      ) : (
        <CoordinatorChat cid={pid} welcome about={about} />
      )}
      <aside className="project-panel" aria-label="Project panel" data-testid="project-panel">
        {!expanded && <PanelResizer width={width} onWidth={setWidth} />}
        <div className="panel-tabs" role="tablist" aria-label="Project panel">
          {(Object.keys(TAB_LABEL) as PanelTab[]).map((t) => (
            <button key={t} role="tab" aria-selected={!worker && panel.tab === t} className={cx("panel-tab", !worker && panel.tab === t && "on")}
              onClick={() => showTab(t)} data-testid={`panel-tab-${t}`}>
              {TAB_LABEL[t]}<TabCount pid={pid} tab={t} />
            </button>
          ))}
          {open.map((id) => <WorkerTab key={id} id={id} pid={pid} on={id === worker} onClose={() => close(id)} />)}
        </div>
        <div className="panel-body">
          {worker ? <WorkerView key={worker} id={worker} fit={{ expanded, onExpand: (e) => setExpanded(pid, e) }} />
            : panel.tab === "work" ? <WorkList pid={pid} />
            : panel.tab === "needs" ? <NeedsYouPanel project={pid} />
            : panel.tab === "prs" ? <OpenPrsPanel project={pid} />
            : <ActivityPanel project={pid} />}
        </div>
      </aside>
    </div>
  );
}

function TabCount({ pid, tab }: { pid: string; tab: PanelTab }) {
  const needs = useNeedsYou(pid).length;
  const prs = useOpenPrs(pid).length;
  if (tab === "needs") return <CountBadge n={needs} tone="needs-you" label={`${needs} need you`} />;
  if (tab === "prs") return <CountBadge n={prs} label={`${prs} open`} />;
  return null;
}

/** An open worker's tab: its state, its title, and a close button. */
function WorkerTab({ id, pid, on, onClose }: { id: string; pid: string; on: boolean; onClose: () => void }) {
  const task = useStore((s) => s.tasks[id]);
  // Cleared workers are not in the store; fetch the title once.
  const [cleared, setCleared] = useState<Task | null>(null);
  useEffect(() => { if (!task) refreshTask(id).then((t) => setCleared(t), () => setCleared(null)); }, [id, task]);
  const t = task ?? cleared;
  const title = t?.title ?? "Worker";
  return (
    <span className={cx("panel-tab worker-tab", on && "on")} data-testid="panel-worker-tab">
      <a href={href({ name: "task", id })} role="tab" aria-selected={on} title={title}>
        {t && <StatusDot tone={taskTone(t.state)} />}
        <span className="ellipsis"><BeadText text={title} project={pid} /></span>
      </a>
      <button className="worker-tab-close" onClick={onClose} aria-label={`Close ${title}`} title="Close this tab" data-testid="panel-worker-close">×</button>
    </span>
  );
}

/** The Work tab: this project's workers grouped by state, each opening as a tab. The Work page has the full board. */
export function WorkList({ pid }: { pid: string }) {
  const tasks = useStore((s) => s.tasks);
  const l = useLabels(pid);
  const groups = useMemo(() => {
    const by: Record<string, Task[]> = {};
    for (const t of Object.values(tasks)) if (t.project_id === pid && !t.cleared_at) (by[t.state] ??= []).push(t);
    for (const k in by) by[k].sort((a, b) => b.updated_at.localeCompare(a.updated_at));
    // What needs you first, then the rest in board order.
    const order = ["needs_decision", "blocked", ...STATES.map((s) => s.id).filter((s) => s !== "needs_decision" && s !== "blocked")];
    return order.filter((s) => by[s]?.length).map((s) => ({ state: STATES.find((x) => x.id === s)!, tasks: by[s] }));
  }, [tasks, pid]);

  return (
    <div className="work-list" data-testid="panel-work">
      <div className="work-list-head">
        <span className="faint">{groups.reduce((n, g) => n + g.tasks.length, 0)} {l.role("worker")}s</span>
        <span className="spacer" />
        <a href={href({ name: "work", project: pid })} data-testid="panel-board">Board</a>
      </div>
      {groups.map((g) => (
        <section key={g.state.id} className="work-group" data-testid={`work-group-${g.state.id}`}>
          <div className="work-group-head"><StatusDot tone={taskTone(g.state.id)} />{g.state.id === "needs_decision" ? `Needs ${l.role("decision")}` : g.state.label}<span className="faint">{g.tasks.length}</span></div>
          {g.tasks.map((t) => (
            <a key={t.id} className={cx("work-row", (t.state === "done" || t.state === "failed") && "ended")} href={href({ name: "task", id: t.id })} data-testid="work-row">
              {t.harness ? <HarnessLogo harness={t.harness} size={14} /> : <span className="harness-logo harness-none" aria-hidden />}
              <span className="work-row-text">
                <span className="work-row-title ellipsis"><BeadText text={t.title} project={pid} /></span>
                {t.state_note && <span className="work-row-note ellipsis"><BeadText text={t.state_note} project={pid} /></span>}
              </span>
              <span className="work-chip">
                {t.pull_request_url ? "PR" : t.harness ? harnessMark(t.harness).name : null}
                <span className="faint"> · {ago(t.updated_at)}</span>
              </span>
            </a>
          ))}
        </section>
      ))}
      {!groups.length && <p className="since-none">No {l.role("worker")}s yet. Ask the {l.role("coordinator")} to start some.</p>}
    </div>
  );
}

// ---- panel width ----

const MIN_PANEL = 320;
const MIN_COORD = 360;

/** The panel's width, remembered separately for the lists and for an open worker. */
function usePanelWidth(mode: "panel" | "worker"): [number | null, (w: number | null) => void] {
  const key = `quark.projectPanelWidth.${mode}`;
  const readW = useCallback(() => {
    try { const v = Number(localStorage.getItem(key)); return v >= MIN_PANEL ? v : null; } catch { return null; }
  }, [key]);
  const [w, setW] = useState<number | null>(readW);
  useEffect(() => setW(readW()), [readW]);
  const set = useCallback((v: number | null) => {
    setW(v);
    try { if (v == null) localStorage.removeItem(key); else localStorage.setItem(key, String(Math.round(v))); } catch { /* not remembered */ }
  }, [key]);
  return [w, set];
}

/** Drag the panel's left edge to resize it; double-click to reset. */
function PanelResizer({ width, onWidth }: { width: number | null; onWidth: (w: number | null) => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const clamp = (w: number) => {
    const box = ref.current?.closest(".agents")?.getBoundingClientRect();
    const max = box ? box.width - MIN_COORD : w;
    return Math.max(MIN_PANEL, Math.min(w, max));
  };
  const start = (e: ReactPointerEvent) => {
    const panel = ref.current?.parentElement;
    if (!panel) return;
    e.preventDefault();
    const right = panel.getBoundingClientRect().right;
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
    const cur = width ?? ref.current?.parentElement?.getBoundingClientRect().width ?? MIN_PANEL;
    if (e.key === "ArrowLeft") { e.preventDefault(); onWidth(clamp(cur + 40)); }
    else if (e.key === "ArrowRight") { e.preventDefault(); onWidth(clamp(cur - 40)); }
  };
  return (
    <div ref={ref} className="pane-resizer" role="separator" aria-orientation="vertical" aria-label="Resize the project panel"
      tabIndex={0} title="Drag to resize · double-click to reset" data-testid="panel-resizer"
      onPointerDown={start} onDoubleClick={() => onWidth(null)} onKeyDown={key} />
  );
}

/** A worker's own link (`#/t/<id>`): its project's Agents tab with the worker open in the panel. */
export function WorkerRoute({ id, children }: { id: string; children: (project: string) => React.ReactNode }) {
  const task = useStore((s) => s.tasks[id]);
  const connected = useStore((s) => s.connected);
  const [found, setFound] = useState<null | "missing" | string>(null);
  useEffect(() => {
    if (task) { setFound(null); return; }
    let live = true;
    refreshTask(id).then((t) => { if (live) setFound(t.project_id); }, () => { if (live) setFound("missing"); });
    return () => { live = false; };
  }, [id, task]);
  const pid = task?.project_id ?? (found && found !== "missing" ? found : null);
  if (pid) return <>{children(pid)}</>;
  return <div className="empty">{found === "missing" ? "This worker does not exist on the daemon." : connected ? "Loading…" : "Waiting for the daemon…"}</div>;
}
