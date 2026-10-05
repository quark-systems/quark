import React, { useEffect, useMemo, useState } from "react";
import { daemonUrl, setDaemonUrl } from "./api";
import { go, href, useRoute } from "./nav";
import { start, useStore } from "./store";
import { isActive } from "./util";
import { Palette } from "./Palette";
import { Projects } from "./screens/Projects";
import { NewProject } from "./screens/NewProject";
import { ProjectBoard } from "./screens/ProjectBoard";
import { Memory } from "./screens/Memory";
import { WorkerView } from "./screens/WorkerView";
import { Inbox } from "./screens/Inbox";
import { PullRequests } from "./screens/PullRequests";
import { PullRequestView } from "./screens/PullRequestView";
import { Accounts } from "./screens/Accounts";

const isMac = typeof navigator !== "undefined" && /mac/i.test(navigator.platform);
export const MOD = isMac ? "⌘" : "Ctrl+";

export function App() {
  const route = useRoute();
  const projects = useStore((s) => s.projects);
  const tasks = useStore((s) => s.tasks);
  const decisions = useStore((s) => s.decisions);
  const prs = useStore((s) => s.pullRequests);
  const openPrs = useMemo(() => Object.values(prs).filter((p) => p.state === "open").length, [prs]);
  const [palette, setPalette] = useState(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (mod && !e.shiftKey && e.key.toLowerCase() === "k") {
        // Capture phase, so the palette opens even when a terminal has focus.
        e.preventDefault(); e.stopPropagation();
        setPalette((p) => !p);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  const counts = useMemo(() => {
    const m: Record<string, { active: number; decide: number }> = {};
    for (const t of Object.values(tasks)) {
      const c = (m[t.project_id] ??= { active: 0, decide: 0 });
      if (isActive(t.state)) c.active++;
      if (t.state === "needs_decision") c.decide++;
    }
    return m;
  }, [tasks]);

  const openDecisions = useMemo(() => Object.values(decisions).filter((d) => d.state === "open").length, [decisions]);

  const currentProject =
    route.name === "project" ? route.id : route.name === "memory" ? route.project : route.name === "task" ? tasks[route.id]?.project_id ?? null
      : route.name === "pr" ? prs[route.id]?.project_id ?? null : null;
  const sorted = Object.values(projects).sort((a, b) => a.name.localeCompare(b.name));

  return (
    <div className="app">
      <aside className="sidebar">
        <a className="brand" href={href({ name: "projects" })}><span className="dot" />QUARK</a>
        <div className="side-section">
          <a className={"side-item" + (route.name === "projects" ? " active" : "")} href={href({ name: "projects" })}>
            <span className="glyph">▦</span>Projects
          </a>
          <a className={"side-item" + (route.name === "inbox" ? " active" : "")} href={href({ name: "inbox" })} data-testid="nav-inbox">
            <span className="glyph">?</span>Decisions
            {openDecisions > 0 && <span className="pill accent" data-testid="inbox-count">{openDecisions}</span>}
          </a>
          <a className={"side-item" + (route.name === "prs" || route.name === "pr" ? " active" : "")} href={href({ name: "prs" })} data-testid="nav-prs">
            <span className="glyph">⇄</span>Pull requests
            <span className="count">{openPrs}</span>
          </a>
          <a className={"side-item" + (route.name === "accounts" ? " active" : "")} href={href({ name: "accounts" })} data-testid="nav-accounts">
            <span className="glyph">@</span>Accounts
          </a>
          <a className={"side-item" + (route.name === "new" ? " active" : "")} href={href({ name: "new" })} data-testid="nav-new-project">
            <span className="glyph">+</span>New project
          </a>
        </div>
        <div className="side-title">Projects</div>
        <div className="side-section side-scroll">
          {sorted.map((p) => (
            <a key={p.id} className={"side-item" + (currentProject === p.id ? " active" : "")}
              href={href({ name: "project", id: p.id })} title={p.goal ?? p.name}>
              <span className="glyph">#</span>
              <span className="ellipsis">{p.name}</span>
              {(counts[p.id]?.decide ?? 0) > 0 && <span className="pill accent">{counts[p.id].decide}</span>}
              <span className="count">{counts[p.id]?.active ?? 0}</span>
            </a>
          ))}
          {!sorted.length && <div className="faint side-empty">No projects yet</div>}
        </div>
        <div className="side-foot"><span className="kbd">{MOD}K</span> jump to a project, task, decision or PR</div>
      </aside>
      <main className="main">
        <RouteView />
      </main>
      <StatusBar />
      {palette && <Palette onClose={() => setPalette(false)} />}
    </div>
  );
}

function RouteView() {
  const route = useRoute();
  switch (route.name) {
    case "projects": return <Projects />;
    case "new": return <NewProject onCreated={(id) => go({ name: "project", id })} />;
    case "project": return <ProjectBoard key={route.id} id={route.id} />;
    case "memory": return <Memory key={route.project} project={route.project} id={route.id} />;
    case "task": return <WorkerView key={route.id} id={route.id} />;
    case "inbox": return <Inbox id={route.id} />;
    case "prs": return <PullRequests />;
    case "pr": return <PullRequestView key={route.id} id={route.id} />;
    case "accounts": return <Accounts />;
  }
}

function StatusBar() {
  const connected = useStore((s) => s.connected);
  const error = useStore((s) => s.error);
  const health = useStore((s) => s.health);
  const [editing, setEditing] = useState(false);
  const [url, setUrl] = useState(daemonUrl());

  const save = () => {
    setDaemonUrl(url);
    setEditing(false);
    void start();
  };

  return (
    <footer className="statusbar">
      <span className={connected ? "ok" : "bad"} data-testid="connection">● {connected ? "connected" : "disconnected"}</span>
      {editing ? (
        <form className="daemon-form" onSubmit={(e) => { e.preventDefault(); save(); }}>
          <input autoFocus value={url} onChange={(e) => setUrl(e.target.value)} aria-label="Daemon URL"
            onKeyDown={(e) => { if (e.key === "Escape") setEditing(false); }} />
          <button className="btn small" type="submit">Connect</button>
        </form>
      ) : (
        <button className="link mono" title="Change daemon" onClick={() => { setUrl(daemonUrl()); setEditing(true); }}>{daemonUrl()}</button>
      )}
      {health && <span>engine {health.engine} · quarkd {health.version}</span>}
      {error && <span className="bad ellipsis">{error}</span>}
      <span className="spacer" />
      <span>{typeof (window as any).__TAURI_INTERNALS__ !== "undefined" ? "desktop" : "web"}</span>
    </footer>
  );
}
