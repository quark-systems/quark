import React, { useEffect } from "react";
import { useNav, setNav, getNav, SCREENS, Screen } from "./nav";
import { useStore, getState } from "./store";
import { setStress } from "./actions";
import { Board } from "./screens/Board";
import { Terminals } from "./screens/Terminals";
import { Chat } from "./screens/Chat";
import { DiffView } from "./screens/DiffView";
import { Inbox } from "./screens/Inbox";
import { Palette } from "./Palette";
import { PerfOverlay } from "./PerfOverlay";
import { runBenchIfRequested } from "./bench";

const isMac = navigator.platform.toLowerCase().includes("mac");
export const MOD = isMac ? "⌘" : "Ctrl+";
export const SHIFT = isMac ? "⇧" : "Shift+";

export function currentProjectId(): string | null {
  return getNav().project ?? getState().projects[0]?.id ?? null;
}

export function App() {
  const nav = useNav();
  const projects = useStore((s) => s.projects);
  const connected = useStore((s) => s.connected);
  const lastSeq = useStore((s) => s.lastSeq);
  const reconnects = useStore((s) => s.reconnects);
  const error = useStore((s) => s.error);
  const openDecisions = useStore((s) => Object.values(s.decisions).filter((d) => d.state === "open").length);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (mod && !e.shiftKey && e.key.toLowerCase() === "k") {
        e.preventDefault(); e.stopPropagation();
        setNav({ palette: !getNav().palette });
      } else if (mod && !e.shiftKey && e.key >= "1" && e.key <= "5") {
        e.preventDefault(); e.stopPropagation();
        setNav({ screen: SCREENS[Number(e.key) - 1].id, palette: false });
      } else if (mod && e.shiftKey && e.key.toLowerCase() === "p") {
        e.preventDefault(); e.stopPropagation();
        setNav({ perf: !getNav().perf });
      } else if (mod && e.shiftKey && e.key.toLowerCase() === "s") {
        e.preventDefault(); e.stopPropagation();
        setStress(!getNav().stress);
      }
    };
    // capture phase so the shortcuts win over a focused terminal
    window.addEventListener("keydown", onKey, true);
    runBenchIfRequested();
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  const cur = SCREENS.find((s) => s.id === nav.screen)!;
  const projName = nav.project ? projects.find((p) => p.id === nav.project)?.name ?? nav.project : "All projects";

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand"><span className="dot" />QUARK</div>
        <div className="side-section">
          {SCREENS.map((s, i) => (
            <button key={s.id} className={"side-item" + (nav.screen === s.id ? " active" : "")} onClick={() => setNav({ screen: s.id })}>
              <span className="glyph">{s.glyph}</span>{s.label}
              {s.id === "inbox" && openDecisions > 0 && <span className="pill accent" style={{ marginLeft: 4 }}>{openDecisions}</span>}
              <span className="kbd">{MOD}{i + 1}</span>
            </button>
          ))}
        </div>
        <div className="side-title">Projects</div>
        <div className="side-section" style={{ overflowY: "auto", flex: 1 }}>
          <button className={"side-item" + (nav.project === null ? " active" : "")} onClick={() => setNav({ project: null })}>
            <span className="glyph">∗</span>All projects
          </button>
          {projects.map((p) => (
            <button key={p.id} className={"side-item" + (nav.project === p.id ? " active" : "")} onClick={() => setNav({ project: p.id })} title={p.repo}>
              <span className="glyph">#</span>
              <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{p.name}</span>
              <span className="count">{p.active_tasks}</span>
            </button>
          ))}
        </div>
        <div className="side-foot">
          <div><span className="kbd">{MOD}K</span> command palette</div>
          <div style={{ marginTop: 4 }}><span className="kbd">{MOD}{SHIFT}P</span> perf · <span className="kbd">{MOD}{SHIFT}S</span> stress</div>
        </div>
      </aside>
      <main className="main">
        <div className="header">
          <h1>{cur.label}</h1>
          <span className="crumb">{projName}</span>
          <span className="spacer" />
          <button className={"btn" + (nav.stress ? " on" : "")} onClick={() => setStress(!nav.stress)}>stress {nav.stress ? "on" : "off"}</button>
          <button className={"btn" + (nav.perf ? " on" : "")} onClick={() => setNav({ perf: !nav.perf })}>perf</button>
          <button className="btn" onClick={() => setNav({ palette: true })}>{MOD}K</button>
        </div>
        <div className="screen">
          <ScreenView screen={nav.screen} />
        </div>
      </main>
      <footer className="statusbar">
        <span className={connected ? "ok" : "bad"}>● {connected ? "connected" : "disconnected"}</span>
        <span>seq {lastSeq}</span>
        <span>reconnects {reconnects}</span>
        {error && <span className="bad">{error}</span>}
        <span className="spacer" />
        <span>{typeof (window as any).__TAURI_INTERNALS__ !== "undefined" ? "tauri" : "web"}</span>
      </footer>
      {nav.palette && <Palette />}
      {nav.perf && <PerfOverlay />}
    </div>
  );
}

function ScreenView({ screen }: { screen: Screen }) {
  switch (screen) {
    case "board": return <Board />;
    case "terminals": return <Terminals />;
    case "chat": return <Chat />;
    case "diff": return <DiffView />;
    case "inbox": return <Inbox />;
  }
}
