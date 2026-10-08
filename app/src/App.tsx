import React, { useEffect, useRef, useState } from "react";
import { daemonUrl, setDaemonUrl } from "./api";
import { RouteView } from "./routes";
import { start, useStore } from "./store";
import { LeftList } from "./shell/LeftList";
import { Dock } from "./shell/Dock";
import { goNextAttention, useAttention } from "./shell/NextAttention";
import { useRoute } from "./nav";
import { Palette } from "./Palette";

const isMac = typeof navigator !== "undefined" && /mac/i.test(navigator.platform);
export const MOD = isMac ? "⌘" : "Ctrl+";

export function App() {
  const [palette, setPalette] = useState(false);
  const route = useRoute();
  const queue = useAttention();
  const dock = useRef<HTMLInputElement>(null);
  // The coordinator's own conversation (the project route) has its message box; every other screen has the dock.
  const hasDock = route.name !== "project";
  const latest = useRef({ queue, route });
  latest.current = { queue, route };

  useEffect(() => {
    // Capture phase, so shortcuts work even when a terminal has focus.
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (!mod || e.shiftKey || e.altKey) return;
      const k = e.key.toLowerCase();
      if (k === "p") { e.preventDefault(); e.stopPropagation(); setPalette((p) => !p); }
      else if (k === "k") {
        e.preventDefault(); e.stopPropagation(); setPalette(false);
        const box = dock.current ?? document.querySelector<HTMLTextAreaElement>('[data-testid="coordinator-chat"] textarea');
        box?.focus();
      } else if (k === "j") {
        e.preventDefault(); e.stopPropagation();
        goNextAttention(latest.current.queue, latest.current.route);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  return (
    <div className="app">
      <LeftList mod={MOD} />
      <main className="main">
        <RouteView />
        {hasDock && <Dock ref={dock} mod={MOD} />}
      </main>
      <StatusBar />
      {palette && <Palette onClose={() => setPalette(false)} />}
    </div>
  );
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
