// The coordinator dock: on every screen but the coordinator's own conversation, a box that
// sends a message to the project's coordinator with what you are looking at as its context.
import React, { forwardRef, useEffect, useMemo, useState } from "react";
import { api } from "../api";
import { go, href, Route, useRoute } from "../nav";
import { useStore } from "../store";
import { useLabels } from "../persona";
import { errText } from "../util";
import { CoordinatorMark, Kbd } from "../ui";

export interface DockContext {
  projectId: string | null;
  /** Whether the route itself names the project; otherwise you pick one. */
  pinned: boolean;
  about: string | null; link: string | null;
}

type Lookup = {
  tasks: Record<string, { project_id: string; title: string }>;
  pullRequests: Record<string, { project_id: string; title?: string | null; repo: string; number: number }>;
  decisions: Record<string, { project_id: string; question: string }>;
};

/** The project and the thing a route shows, which a dock message carries as context. */
export function dockContext(route: Route, s: Lookup, fallbackProject: string | null): DockContext {
  const at = (projectId: string | undefined, about: string | null): DockContext =>
    projectId ? { projectId, pinned: true, about, link: about ? href(route) : null } : { projectId: fallbackProject, pinned: false, about: null, link: null };
  if (route.name === "task") { const t = s.tasks[route.id]; return at(t?.project_id, t?.title ?? null); }
  if (route.name === "pr") { const p = s.pullRequests[route.id]; return at(p?.project_id, p ? p.title ?? `${p.repo}#${p.number}` : null); }
  if (route.name === "inbox" && route.id) { const d = s.decisions[route.id]; return at(d?.project_id, d?.question ?? null); }
  if ("project" in route && typeof route.project === "string") return at(route.project, null);
  return at(undefined, null);
}

/** The text sent: the message, with what it is about on a first line the coordinator can follow. */
export function dockMessage(text: string, ctx: DockContext): string {
  return ctx.about ? `About "${ctx.about}" (${ctx.link}):\n${text}` : text;
}

const isMac = typeof navigator !== "undefined" && /mac/i.test(navigator.platform);
/** The shortcut modifier as shown in key hints. */
export const MOD = isMac ? "⌘" : "Ctrl+";

// The last project you opened, so app-level screens dock to it.
let lastProject: string | null = null;

export const Dock = forwardRef<HTMLInputElement, { mod?: string }>(function Dock({ mod = MOD }, ref) {
  const route = useRoute();
  const projects = useStore((s) => s.projects);
  const tasks = useStore((s) => s.tasks);
  const pullRequests = useStore((s) => s.pullRequests);
  const decisions = useStore((s) => s.decisions);
  const sorted = useMemo(() => Object.values(projects).sort((a, b) => a.name.localeCompare(b.name)), [projects]);
  const fallback = lastProject && projects[lastProject] ? lastProject : sorted[0]?.id ?? null;
  const ctx = dockContext(route, { tasks, pullRequests, decisions }, fallback);
  const [picked, setPicked] = useState<string | null>(null);
  const projectId = ctx.pinned ? ctx.projectId : picked && projects[picked] ? picked : ctx.projectId;
  useEffect(() => { if (ctx.pinned) lastProject = ctx.projectId; }, [ctx.pinned, ctx.projectId]);
  const l = useLabels(projectId ?? "");
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [sent, setSent] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const coordinator = l.role("coordinator");

  const send = async () => {
    const t = text.trim();
    if (!t || !projectId || sending) return;
    setSending(true); setErr(null); setSent(null);
    try { await api.sendChat(projectId, dockMessage(t, ctx)); setText(""); setSent(projectId); }
    catch (e) { setErr(errText(e)); }
    finally { setSending(false); }
  };

  // On a project's pages the dock names the project, as on a worker it names the worker.
  const scope = !ctx.about && ctx.pinned && projectId ? projects[projectId]?.name ?? null : null;

  if (!sorted.length) return null;
  const placeholder = ctx.about ? `Ask the ${coordinator} about ${ctx.about}…` : `Ask the ${coordinator} anything, or describe new work…`;
  return (
    <div className="dock" data-testid="dock">
      <div className="dock-box">
        <CoordinatorMark size={24} />
        {ctx.about ? <span className="dock-about" data-testid="dock-about" title={ctx.about}>about {ctx.about}</span>
          : scope ? <span className="dock-about" title={scope}>about {scope}</span>
          : !ctx.pinned && sorted.length > 1 && projectId ? (
            <select className="dock-project" aria-label="Project" value={projectId} onChange={(e) => setPicked(e.target.value)}>
              {sorted.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
            </select>
          ) : null}
        <input ref={ref} className="dock-input" aria-label={`Message the ${coordinator}`} placeholder={placeholder} value={text}
          onChange={(e) => { setText(e.target.value); setSent(null); }}
          onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing) { e.preventDefault(); void send(); } if (e.key === "Escape") (e.target as HTMLInputElement).blur(); }} />
        <Kbd keys={`${mod}K`} />
        {projectId && <button type="button" className="ui-btn secondary dock-open" onClick={() => go({ name: "project", id: projectId })}>Open</button>}
      </div>
      {(sent || err) && (
        <div className={"dock-note" + (err ? " bad" : "")} role="status" data-testid="dock-note">
          {err ?? <>Sent to the {coordinator} of {projects[sent!]?.name}. <a href={href({ name: "project", id: sent! })}>See the conversation</a></>}
        </div>
      )}
    </div>
  );
});
