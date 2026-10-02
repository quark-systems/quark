// Non-interactive benchmark: load with ?bench=1 (optionally &exit=1 to quit the Tauri
// app when done, &phase_ms=5000 to change phase length). Results are logged as
// `QUARK_BENCH_RESULT {json}` to the console, printed to the Tauri process stdout,
// and stored on window.__QUARK_BENCH__.
import { api } from "./api";
import { getState } from "./store";
import { setNav } from "./nav";
import { frames, latency, percentile } from "./perf";
import { allTerms, getProbeWorker } from "./terms";
import { STRESS_PROMPT } from "./actions";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const inTauri = () => typeof (window as any).__TAURI_INTERNALS__ !== "undefined";

async function waitFor(cond: () => boolean, ms: number) {
  const t0 = performance.now();
  while (!cond()) {
    if (performance.now() - t0 > ms) return false;
    await sleep(50);
  }
  return true;
}

export function runBenchIfRequested() {
  const q = new URLSearchParams(location.search);
  if (!q.has("bench")) return;
  const phaseMs = Number(q.get("phase_ms") ?? 4000);
  run(phaseMs, q.has("exit")).catch((e) => report({ error: String(e) }, q.has("exit")));
}

async function report(result: unknown, exit: boolean) {
  const json = JSON.stringify(result);
  (window as any).__QUARK_BENCH__ = result;
  console.log("QUARK_BENCH_RESULT " + json);
  const pre = document.createElement("pre");
  pre.id = "bench-result";
  pre.style.cssText = "position:fixed;left:240px;top:44px;z-index:60;background:#000d;color:#9d8cff;font:11px monospace;padding:8px;max-width:60vw;white-space:pre-wrap;border:1px solid #333";
  pre.textContent = JSON.stringify(result, null, 1);
  document.body.appendChild(pre);
  if (inTauri()) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("bench_report", { json, exit });
  }
}

async function run(phaseMs: number, exit: boolean) {
  const startedAt = new Date().toISOString();
  await waitFor(() => getState().connected && getState().workers.length > 0 && getState().projects.length > 0, 15000);
  const s = getState();
  if (!s.workers.length) throw new Error("no workers from daemon");
  const phases: Record<string, unknown> = {};
  // environment ceiling: the board alone, no terminals mounted
  setNav({ screen: "board", perf: true, stress: false });
  await sleep(1000);
  frames.acquire();
  frames.recording = [];
  await sleep(phaseMs);
  phases.board_idle = { ...frames.stats(frames.recording), frames_over_33ms: frames.recording.filter((x) => x > 33.4).length };
  frames.recording = null;
  frames.release();
  setNav({ screen: "terminals" });
  await waitFor(() => allTerms().length >= Math.min(4, s.workers.length), 5000);
  const probe = getProbeWorker()!;
  const probeTerm = allTerms().find((t) => t.id === probe)!;
  const others = s.workers.filter((w) => w.id !== probe).slice(0, 3).map((w) => w.id);
  const pid = s.projects[0].id;
  setNav({ project: pid });
  await sleep(1500); // let replayed scrollback settle
  // clear the bash prompt line
  await api.input(probe, "\x15");
  await sleep(300);

  let typing = false;
  const typer = async () => {
    const alphabet = "abcdefghijklmnopqrstuvwxyz";
    let i = 0, sinceClear = 0;
    while (typing) {
      const ch = alphabet[i++ % alphabet.length];
      latency.keydown(ch);
      probeTerm.term.input(ch, true); // same path as a real keystroke: xterm onData -> POST input
      if (++sinceClear >= 30) { await sleep(120); latency.pending = []; await api.input(probe, "\x15"); sinceClear = 0; }
      await sleep(120);
    }
  };

  async function phase(name: string, opts: { type: boolean }) {
    latency.reset();
    const ev0 = getState().events;
    frames.recording = [];
    const t0 = performance.now();
    if (opts.type) { typing = true; void typer(); }
    await sleep(phaseMs);
    typing = false;
    const rec = frames.recording; frames.recording = null;
    const dur = (performance.now() - t0) / 1000;
    phases[name] = {
      ...frames.stats(rec),
      frames_over_33ms: rec.filter((x) => x > 33.4).length,
      echo: opts.type ? latency.stats() : null,
      events_per_s: Math.round((getState().events - ev0) / dur),
    };
    await sleep(250);
    await api.input(probe, "\x15");
  }

  frames.acquire();
  try {
    await phase("idle_terminals", { type: true });
    // 3 flooding panes + streaming chat, type in the quiet bash pane
    await Promise.all(others.map((id) => api.stress(id, true)));
    api.send(pid, STRESS_PROMPT).catch(() => {});
    await phase("stress3_chat_typing", { type: true });
    // all 4 panes flooding + chat streaming (no echo probe: bash pane is flooded)
    await api.stress(probe, true);
    api.send(pid, STRESS_PROMPT).catch(() => {});
    await phase("stress4_chat_terminals_visible", { type: false });
    // same load, chat screen visible: measures markdown streaming render while terminals keep parsing
    setNav({ screen: "chat" });
    api.send(pid, STRESS_PROMPT).catch(() => {});
    await phase("stress4_chat_chat_visible", { type: false });
  } finally {
    await Promise.all(s.workers.map((w) => api.stress(w.id, false).catch(() => {})));
    frames.release();
    setNav({ screen: "terminals" });
  }

  const result = {
    kind: "quark-ui-poc-bench",
    impl: "tauri-react-xterm",
    shell: inTauri() ? "tauri" : "browser",
    started_at: startedAt,
    phase_ms: phaseMs,
    user_agent: navigator.userAgent,
    dpr: devicePixelRatio,
    viewport: [innerWidth, innerHeight],
    terminal_renderers: allTerms().map((t) => t.renderer),
    probe_worker: probe,
    phases,
  };
  void percentile;
  await report(result, exit);
}
