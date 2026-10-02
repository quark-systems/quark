# Quark desktop UI POC: native Rust on warpui

One window, five screens, built on Warp's open-source UI framework (`warpui` + `warpui_core` from `warpdotdev/warp`, pinned by git rev in `Cargo.toml`).
It talks only to the stub daemon described in `../CONTRACT.md`.
Results and the framework assessment are in `RESULTS.md`; the time log is in `TIMELOG.md`.

No code was copied from Warp's AGPL app crates.
The app depends only on `warpui`/`warpui_core` (MIT) from the Warp repo, plus crates.io crates (`alacritty_terminal`, `pulldown-cmark`, `syntect`, `ureq`, `tungstenite`, ...) and, behind a feature flag, `libghostty-vt` from `Uzaaft/libghostty-rs`.

## Run

```sh
# 1. stub daemon (any port; 7420 is the default the app uses)
(cd ../stub-daemon && cargo run --release -- --port 7420)

# 2. the app
cargo run --release                                  # alacritty_terminal core
cargo run --release -- --daemon http://127.0.0.1:7422 # or QUARK_DAEMON=...

# libghostty-vt core (needs Zig 0.16 on PATH; the first build compiles libghostty-vt)
PATH=/path/to/zig-0.16:$PATH cargo run --release --features ghostty -- --term ghostty
```

Flags:

| Flag | Meaning |
| --- | --- |
| `--daemon URL` / `QUARK_DAEMON` | Daemon base URL (default `http://127.0.0.1:7420`). |
| `--term alacritty\|ghostty` / `QUARK_TERM` | Terminal core. `ghostty` needs the `ghostty` cargo feature; without it the app warns and uses alacritty. |
| `--bench N` | Non-interactive benchmark: N seconds (35% idle typing, 65% stress on all 4 panes + streaming chat), prints JSON to stdout, exits. |
| `--xdotool` | With `--bench`: inject probe keys as real X11 key events through `xdotool` instead of calling the input handler directly. |

Headless (how it was verified here): `Xvfb :97 -screen 0 1600x1000x24 &`, then `DISPLAY=:97 XDG_RUNTIME_DIR=/tmp/xdg cargo run --release`.
Under Xvfb wgpu uses lavapipe (software Vulkan); `WGPU_BACKEND=gl` selects llvmpipe GL instead.

## Web build

The same app also builds for `wasm32-unknown-unknown` and runs in a browser canvas (WebGL2 through wgpu), talking to the stub over `fetch` + `WebSocket`.
All five screens are compiled in; **terminals are excluded** (each pane shows a notice): `alacritty_terminal` unconditionally depends on `polling` for its PTY event loop, which does not build for wasm32, and libghostty-vt's wasm build was not attempted.
Board, chat, diff (with syntect highlighting and inline comments), decisions/PRs, the palette and the perf overlay all work.

```sh
# needs the wasm32-unknown-unknown target and wasm-bindgen-cli 0.2.117 (pinned to match)
cargo build --release --target wasm32-unknown-unknown --lib     # .cargo/config.toml sets the wasm rustflags
wasm-bindgen --target web --out-dir web/pkg target/wasm32-unknown-unknown/release/quark_ui_web.wasm
(cd web && python3 -m http.server 8471)
# open http://127.0.0.1:8471/?daemon=http://127.0.0.1:7420   (?daemon= defaults to http://127.0.0.1:7420)
```

- `src/lib.rs` is the wasm entry point (`#![cfg(target_family = "wasm")]`, so the lib target is empty on desktop); `src/main.rs` is the desktop entry. Both share every other module.
- `src/api.rs`: every daemon operation is an async fn over `get_text`/`post` plus the event stream. Desktop drives them on threads with blocking `ureq`/`tungstenite`; web uses `fetch`, `WebSocket` and `setTimeout` through web-sys on the browser event loop. Same seq cursor, resume and backoff logic.
- Fonts: warpui has no system fonts or fallback fonts on wasm, so DejaVu Sans and DejaVu Sans Mono (regular + bold, `assets/fonts/`, Bitstream Vera/DejaVu licence) are embedded with `include_bytes!`.
- `web/shots.mjs` (screens, live board update, chat send, clipboard copy) and `web/input-checks.mjs` (paste, non-ASCII keys, IME) drive headless Chromium with Playwright; screenshots are `screenshots/web-*.png`.

Web-specific gaps: no terminals; CJK, Hangul and emoji render as boxes (no fallback fonts on wasm; would need embedding large fonts); IME composition and text inserted without a key event (`insertText`, dictation) are ignored because the canvas has no text-input path on desktop browsers (warpui's hidden `<input>` exists only for mobile soft keyboards); in a real browser some app shortcuts are also browser shortcuts that warpui deliberately lets through (`Ctrl+1..5` switch browser tabs, `Ctrl+L` focuses the address bar), so the screen shortcuts would need other keys there.

## Keys

Global: `Ctrl+1..5` screens, `Ctrl+K` command palette, `Alt+Up/Down` switch project, `F2` perf overlay, `Ctrl+Shift+S` stress mode (stub floods every terminal pane).

| Screen | Keys |
| --- | --- |
| 1 Board | `h j k l` move across columns/cards, `[ ]` previous/next project. |
| 2 Terminals | Click or `Alt+1..4` to focus a pane; all other keys go to the pane's shell through the terminal core's key encoder. Drag to select, `Ctrl+Shift+C` copy (whole pane if nothing selected), `Ctrl+Shift+V` paste, `Ctrl+J` toggle the coordinator chat dock, `Ctrl+L` focus the dock's input. |
| 3 Chat | `Enter` focus input, `Enter` send, `Esc` leave input, `Up/Down`, `PageUp/PageDown` scroll; drag-select text and `Ctrl+C` copy. |
| 4 Diff | `j k`/arrows move line cursor, `PageUp/PageDown`, `c` or `Enter` comment on the line (or click a line), `Enter` post, `Esc` cancel, `[ ]` previous/next PR. |
| 5 Decisions & PRs | `j k` move, `h l`/`Tab` switch list, `1..9` answer with that option, `Enter` answer with the recommended option (or open the PR's diff), `g` top. |

## What is implemented

- **Sidebar + board**: projects with open-task counts, kanban columns by task state, live updates from `/v1/events` over WebSocket with a `seq` cursor; on disconnect it reconnects with backoff and resumes from the last seq (duplicates `seq <= last` are dropped).
- **Terminals**: 2x2 grid fed by `worker.output`, through a `TerminalCore` trait (`src/term.rs`) with two backends: `alacritty_terminal` (default) and `libghostty-vt` (`--features ghostty`, `src/term_ghostty.rs`, using Ghostty's `key::Encoder` for key input, configured from the terminal's current modes).
  Only damaged/dirty rows are re-read into the paint snapshot.
  Keyboard input is POSTed to `/v1/workers/{id}/input` with at most one request in flight per worker; keys typed meanwhile are coalesced into the next request.
  Pane size changes (window resize, chat dock toggle) POST `/v1/workers/{id}/resize`.
  Custom grid element: background runs, ASCII text batched into runs, non-ASCII and wide cells placed per cell so fallback fonts cannot shift the grid; block cursor; mouse selection.
- **Coordinator chat**: history plus streamed deltas, re-parsed with `pulldown-cmark` while streaming (headings, lists, task lists, quotes, tables, inline code/bold/italic/links, fenced code with `syntect` highlighting, unterminated fences render while streaming), auto-follows the bottom, selectable text, single-line input.
- **Diff + inline comments**: unified diff with per-file headers, old/new line numbers, `syntect` highlighting by file extension, existing comment threads inline under their line, click or `c` to open an inline composer, POSTs `/v1/pull-requests/{id}/comments` and refreshes.
- **Decisions + PRs**: decisions with project chip, options shown for the selected one, number keys answer, PR list opens the diff screen; `Ctrl+K` palette with subsequence filtering over screens, projects, PRs, decision answers and actions.
- **Instrumentation**: `F2` overlay (fps, frame p50/p99, frame split into view render / layout+paint / GPU submit+present, key-to-painted-echo p50/p99 in the bash pane, rows rebuilt), stress toggle, `--bench`.
- **Daemon client boundary**: `src/api.rs` owns all HTTP/WS, and delivers everything to the view as `Net` messages over an `async_channel` consumed with `spawn_stream_local`; view code does no I/O. The transport is swapped per target (see "Web build").

## Gaps and known limitations

- The text inputs are a homegrown single-line editor (`TextBuf` + `EditorLine`): no multi-line input, no undo, no word wrap. warpui has no reusable editor; Warp's editor lives in its AGPL app crates.
- IME: the marked-text (preedit) path and caret position reporting are wired, but could not be exercised headlessly (no ibus/fcitx under Xvfb). Unicode was verified through clipboard paste instead.
- Terminal snapshot keeps one char per cell, so combining marks are dropped; no scrollback view (both cores keep scrollback, the UI does not show it); no mouse reporting to the application; no hyperlinks; underline style is a single style.
- Diff: no side-by-side view, no word-level intra-line highlights, comment threads cannot be replied to or resolved.
- Board is keyboard-navigable but cards cannot be dragged or edited.
- Some box-drawing in the stub's agent-log script is misaligned in the same way in tmux itself; it is the script, not the grid.
- macOS/Windows not built or tried; only Linux X11 (Xvfb) was exercised.
