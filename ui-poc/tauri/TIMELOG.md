# Tauri POC time log

All times UTC (`date -u`). Active minutes are wall-clock minutes spent on that item (agent time, including waiting on builds).

| Item | Start | End | Active min | Notes |
| --- | --- | --- | --- | --- |
| Setup (apt deps, npm, Tauri scaffold, icons, shared infra: api client, event store w/ seq cursor + reconnect, perf meters, terminal manager) | 23:03:39 | 23:08:03 | 4.5 | cold cargo build ran in background |
| Screen 1 code: sidebar + task board | 23:08:03 | 23:08:39 | 0.6 | first-pass code only, against the contract (stub not yet running) |
| Screen 2 code: 2x2 terminals (xterm + WebGL, input, resize, echo probe hook) | 23:08:39 | 23:08:54 | 0.3 | terminal manager already written in setup |
| Screen 3 code: coordinator chat (marked + highlight.js streaming) | 23:08:54 | 23:09:14 | 0.3 | |
| Screen 4 code: diff view with inline comments (hand-rolled unified diff parser) | 23:09:14 | 23:09:48 | 0.6 | |
| Screen 5 code: decisions + PR list (j/k/enter/1-9) + cmdk palette | 23:09:48 | 23:10:31 | 0.7 | |
| Instrumentation code: perf overlay, ?bench=1 scenario | 23:10:31 | 23:11:12 | 0.7 | |
| First typecheck + vite build | 23:11:12 | 23:11:40 | 0.5 | needed vite-env.d.ts for CSS imports; vite 6 vs plugin-react peer conflict at setup (moved to vite 8) |
| Browser verification of all 5 screens + interactions (Playwright: typing, j/2 answer, diff comment POST, chat send/stream, reconnect) | 23:11:40 | 23:17:00 | 5.3 | all interactions worked on first run; fixes were cosmetic (sidebar wrap, shortcut labels); added echo-arrival breakdown and ?renderer=dom |
| Private stub instance and daemon-only echo baseline; diagnosed the shared stub crash with its owner | 23:17:00 | 23:22:30 | 5.5 | the shared stub stopped emitting output (tmux crash, fixed upstream) |
| Tauri: QUARK_QUERY URL passthrough, release rebuilds, bench matrix (Tauri/Chromium x WebGL/DOM x 2), board_idle phase | 23:22:30 | 23:33:57 | 11.5 | each incremental release rebuild took about 140 s |
| Tauri screenshots via xdotool; found and fixed out-of-order terminal input; re-verified | 23:33:57 | 23:37:47 | 3.8 | input reorder only showed up with real X11 keys under load |
| Web screenshots, final bundle build, text-input checks (selection, IME via CDP, paste), README and RESULTS | 23:37:47 | 23:42:00 | 4.2 | |

## Totals

| Bucket | Active min |
| --- | --- |
| Setup (deps, scaffold, shared infra) | 4.5 |
| Screen 1 board (code) | 0.6 |
| Screen 2 terminals (code) | 0.3 (+ terminal manager in setup) |
| Screen 3 chat (code) | 0.3 |
| Screen 4 diff (code) | 0.6 |
| Screen 5 decisions/PRs + palette (code) | 0.7 |
| Instrumentation (code) | 0.7 |
| Verification and fixes across all screens (browser + Tauri) | 14.6 |
| Benchmarking, perf diagnosis, docs | 16.2 |
| **Total wall clock** | **about 38.5 min (23:03:39 to 23:42:00)** |

The agent wrote the code very quickly, so the per-screen code minutes are tiny and do not compare directly with a human's.
The useful comparison against the native build is the shape of the time:
- Every screen worked the first time it was run against the real stub.
- Most of the time went to waiting on builds and on measurement, not on making the UI work.
- The cold Rust build took 6.3 min; each incremental release rebuild took about 2.3 min.


## Follow-up spike: alternative terminal renderers (2026-10-02)

| Item | Start | End | Active min | Notes |
| --- | --- | --- | --- | --- |
| Read wterm / ghostty-web docs and dist code (APIs, WASM loading, render scheduling) | 04:05:24 | 04:07:30 | 2.1 | both cloned sources had enough README to wire without reading much code |
| Adapter layer (`src/term/`), refactor terms.ts so input, resize, echo probe and bench go through one interface; xterm adapter | 04:07:30 | 04:08:06 | 0.6 | each renderer is a dynamic-import chunk |
| wterm + wterm-lite adapter | (in the block above) | | ~0.3 | theme via CSS custom properties; `onData`/`onResize` options; render = rAF after `write()` |
| ghostty-web adapter | (in the block above) | | ~0.2 | xterm-compatible API; `init()` once; write callback runs after the frame's paint |
| Feature check script (selection, copy, paste, IME, Kitty) + runs in Chromium for all 5 | 04:08:06 | 04:11:35 | 3.5 | no adapter bugs found; differences below are library behavior |
| Tauri rebuild + per-renderer screenshots under Xvfb | 04:11:35 | 04:16:32 | 4.9 | rebuild 173 s; all WASM loaded through Tauri's asset protocol without changes |
| Extend h2h/run.sh (RENDERERS, NATIVE, PORT); start bench | 04:16:32 | 04:17:00 | 0.5 | |
| ghostty-web never finished the bench in Tauri: diagnosed it (fine in Chromium; WebKit event loop starves under the 4-pane flood), added per-phase progress output and timer-lag metric, rebuilt | 04:17:00 | 04:32:30 | 15.5 | the only real friction in this spike; rebuild 109 s |
| Bench matrix in Tauri (5 renderers x 3 rounds, fresh stub per run, via h2h/run.sh) + summary script | 04:33:38 | 04:45:20 | 11.7 | mostly waiting; ghostty-web runs time out at 90 s by design and leave partial results |
| Chromium bench once per renderer (vite preview + fresh stub per run) | 04:45:20 | 04:49:00 | 3.7 | |
| CSP: a Playwright meta-injection harness didn't work (dropped); tested a strict CSP in the real Tauri build instead (2 rebuilds) | 04:49:00 | 04:59:37 | 10.6 | ghostty-web needs `data:` in connect-src; others work with `'wasm-unsafe-eval'` only |
| RESULTS "Terminal renderer comparison", README, cleanup | 04:59:37 | 05:01:30 | 1.9 | |

Spike total: 04:05:24 to 05:01:30, about 56 min of wall clock.
Per renderer, excluding shared work:
- xterm adapter refactor: under 1 min
- wterm and wterm-lite: about 3 min, including the CSS-variable theming
- ghostty-web: about 1 min to wire, then about 16 min diagnosing why it never finished the bench in Tauri, plus most of the 11 min of CSP work

Shared work:
- benchmarking: about 15 min, mostly waiting
- feature checks: about 3.5 min
