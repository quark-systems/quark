import React, { useEffect, useRef, useState } from "react";
import { daemonUrl, setDaemonUrl } from "./api";
import { RouteView } from "./routes";
import { start, useStore } from "./store";
import { LeftList } from "./shell/LeftList";
import { Dock, MOD } from "./shell/Dock";
import { goNextAttention, useAttention } from "./shell/NextAttention";
import { parseRoute, useRoute } from "./nav";
import { Palette } from "./Palette";
import { checkFirstRun } from "./firstRun";
import { api } from "./api";
import { markInstallSeen, seenInstall, setUpdateStatus, updateBanner, useUpdateStatus } from "./appUpdate";
import { UpdateBannerView } from "./screens/Updates";


export function App() {
  const [palette, setPalette] = useState(false);
  const route = useRoute();
  const queue = useAttention();
  const dock = useRef<HTMLInputElement>(null);
  // The coordinator's own conversation (the project route) has its message box; every other screen has the dock.
  // The worker view places its own, under the worker's conversation.
  const hasDock = route.name !== "project" && route.name !== "task";
  const health = useStore((s) => s.health);
  useEffect(() => { if (health) void checkFirstRun(); }, [health]);
  const latest = useRef({ queue });
  latest.current = { queue };

  useEffect(() => {
    // Capture phase, so shortcuts work even when a terminal has focus.
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (!mod || e.shiftKey || e.altKey) return;
      const k = e.key.toLowerCase();
      if (k === "p") { e.preventDefault(); e.stopPropagation(); setPalette((p) => !p); }
      else if (k === "k") {
        e.preventDefault(); e.stopPropagation(); setPalette(false);
        const box = dock.current ?? document.querySelector<HTMLElement>('[data-testid="dock"] input, [data-testid="coordinator-chat"] textarea');
        box?.focus();
      } else if (k === "j") {
        e.preventDefault(); e.stopPropagation();
        // Read the route from the URL: a key pressed right after navigating can beat the re-render.
        goNextAttention(latest.current.queue, parseRoute(location.hash));
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  return (
    <div className="app">
      <LeftList mod={MOD} />
      <main className="main">
        <UpdateBar />
        <RouteView />
        {hasDock && <Dock ref={dock} mod={MOD} />}
      </main>
      <StatusBar />
      {palette && <Palette onClose={() => setPalette(false)} />}
    </div>
  );
}

/** Quark updates, above every screen but Updates itself. */
function UpdateBar() {
  const route = useRoute();
  const status = useUpdateStatus();
  const [seen, setSeen] = useState(seenInstall());
  const banner = route.name === "updates" ? null : updateBanner(status, seen);
  if (!banner) return null;
  const last = status?.last_install;
  const dismiss = banner.action === "open" && last ? () => { markInstallSeen(last.started_at); setSeen(last.started_at); } : undefined;
  return <UpdateBannerView banner={banner} onDismiss={dismiss}
    onInstall={() => { api.installUpdate().then(setUpdateStatus, () => { location.hash = "#/updates"; }); }} />;
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
