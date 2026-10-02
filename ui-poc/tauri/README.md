# Quark UI POC: Tauri build

This is the Tauri v2 + React + TypeScript + Vite build of the Quark desktop UI proof of concept.
It talks only to the stub daemon described in [`../CONTRACT.md`](../CONTRACT.md).
The webview makes HTTP and WebSocket calls straight to the daemon, with no Rust proxy.

## Run

```sh
# 1. daemon (in ../stub-daemon)
cargo run --release                      # :7420, or: ./target/release/quark-ui-stub-daemon --port 7431

# 2a. web build (same frontend, plain browser)
npm install
npm run dev                              # http://127.0.0.1:1420/

# 2b. desktop app (dev, hot reload)
npm run tauri dev

# 2c. desktop app (release binary + .deb + .AppImage)
npm run tauri build                      # or: npx tauri build --no-bundle
./src-tauri/target/release/quark-ui-poc-tauri
```

On Linux the build needs these packages: `libwebkit2gtk-4.1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev librsvg2-dev libayatana-appindicator3-dev build-essential`.

### URL flags

The web build reads these from the query string.
The desktop app takes the same string through the `QUARK_QUERY` env var, for example `QUARK_QUERY="bench=1&exit=1" ./quark-ui-poc-tauri`.

| flag | effect |
| --- | --- |
| `daemon=http://host:port` | daemon base URL (default `http://127.0.0.1:7420`) |
| `screen=board\|terminals\|chat\|diff\|inbox` | initial screen |
| `project=<id>`, `pr=<id>` | initial project and PR |
| `perf` | open the perf overlay |
| `renderer=dom\|wterm\|wterm-lite\|ghostty-web` | terminal implementation (default xterm.js WebGL); see RESULTS.md "Terminal renderer comparison" |
| `probe=<worker id>` | pane used for the echo-latency probe (default: the worker titled `shell`/`bash`) |
| `bench` | run the scripted benchmark (see below) |
| `exit` | with `bench`: quit the desktop app when done |
| `phase_ms=N` | with `bench`: phase length in ms (default 4000, 5 phases = 20 s) |

### Keyboard

| keys | action |
| --- | --- |
| Ctrl/Cmd+K | command palette: screens, projects, PRs, "answer decision -> option", perf/stress toggles |
| Ctrl/Cmd+1..5 | board, terminals, coordinator, diff, decisions & PRs |
| Ctrl/Cmd+Shift+P | perf overlay |
| Ctrl/Cmd+Shift+S | stress mode (all 4 panes flood, and the coordinator gets a prompt so it streams) |
| decisions screen | `j`/`k` move, `Tab`/`h`/`l` switch list, `1`-`9` pick option, `Enter` accept recommended or open the PR diff, `g`/`G` top/bottom |
| diff | click a line to comment, Ctrl+Enter post, Esc cancel |
| chat | Enter send, Shift+Enter newline (an Enter while an IME is composing is ignored) |

The global shortcuts listen in the capture phase, so they work even when a terminal has focus.
That means a terminal never receives Ctrl+K (readline kill-line) or Ctrl+1..5.

## Benchmark

`?bench=1` runs a scripted scenario of five 4 s phases:

1. `board_idle`: the board alone, as an environment ceiling.
2. `idle_terminals`: 4 live terminals with background output, typing into the shell pane.
3. `stress3_chat_typing`: 3 panes flooding plus coordinator streaming, while typing into the quiet shell pane.
4. `stress4_chat_terminals_visible`: all 4 panes flooding plus chat streaming, with the terminals visible.
5. `stress4_chat_chat_visible`: the same load with the chat screen visible.

The result goes three places:

- the console, as `QUARK_BENCH_RESULT {json}`
- the process stdout in the desktop app, through the `bench_report` command
- `window.__QUARK_BENCH__`

Scripts in `scripts/`:

- `web-bench.mjs <url>`: runs the bench in headless Chromium (`/opt/pw-browsers`) and prints the JSON.
- `web-check.mjs`: screenshots every screen in Chromium.
- `interact.mjs`: drives real keystrokes and clicks and checks typing, decision answer, diff comment POST, chat streaming, and reconnect.
- `daemon-echo.mjs`: measures the daemon's own keystroke-to-echo latency with no UI, as a baseline.

Raw results from this environment are in `bench/`, and the analysis is in `RESULTS.md`.

## What's implemented

- **Sidebar and task board.**
  - Projects with live active counts and an "All projects" view.
  - Six kanban columns, one per task state.
  - Cards flash when their state changes.
  - All updates come from the event stream. The client keeps a `seq` cursor and drops duplicates. On reconnect it uses exponential backoff, resumes with `?cursor=lastSeq`, and refetches REST snapshots.
- **Terminals.**
  - 2x2 grid of xterm.js 6 terminals with the WebGL addon; the DOM renderer is a fallback and an option.
  - Each worker's xterm instance lives outside React and is re-parented when the screen remounts, so switching screens never replays scrollback.
  - Each worker keeps a 2 MB replay backlog.
  - Keyboard input is POSTed per worker, serialized and coalesced (see the input-order notes in RESULTS.md).
  - Fit-on-resize uses ResizeObserver and sends a debounced POST to `/resize`.
  - The shell pane shows a live echo-latency pill.
- **Coordinator chat.**
  - History plus `coordinator.delta` streaming, rendered as markdown with `marked`.
  - Code blocks are highlighted with highlight.js, using 14 registered languages.
  - Sticky autoscroll and an IME-safe Enter key.
- **Diff review.**
  - Hand-rolled unified-diff parser, per-line highlight.js highlighting, and a file list with jump-to-file.
  - Inline threads show existing comments and accept new ones by clicking a line and posting.
  - The PR picker reads state, checks, risk and +/- from the event stream.
- **Decisions and PRs.**
  - Two keyboard-driven lists. Answered decisions are dimmed and the chosen option is highlighted.
  - Enter on a PR opens its diff.
- **Command palette** built on `cmdk`: fuzzy search over screens, projects, PRs and every option of every open decision.
- **Perf overlay** (rAF based): fps, frame-time p50/p99, a frame-time sparkline, echo p50/p99, events/s and the renderer in use.

## Gaps and known issues

- **Diff view.** No virtualization, which is fine for the stub's diffs but not for a 10k-line diff. Highlighting runs per line, so multi-line constructs such as block comments are not tracked across lines. Comments anchor to the new-side line number, or the old-side number for deleted lines, because the contract only has `line`.
- **Chat.** Each delta re-parses the whole streaming message, coalesced to at most once per frame. That is O(n^2) over a long message; fine at chat sizes, but an incremental parser would be better.
- **Markdown.** Raw HTML is escaped rather than sanitized properly; links render as-is.
- **Workers.** Only the first 4 are shown. There is no pane zoom or layout persistence.
- **Inbox ordering.** Decisions sort open-first, then by id. The contract has no timestamp for decisions.
- **Security.** The Tauri CSP is `default-src 'self'`, with `'wasm-unsafe-eval'` for the WASM terminal cores, `connect-src` limited to the IPC bridge, `127.0.0.1` and `data:`, and `'unsafe-inline'` styles. `data:` is only needed by ghostty-web. The first round of benchmarks ran with `csp: null`; the CSP does not affect rendering.
- **Window management.** Under Xvfb there is no window manager, so the screenshots show no title bar.
