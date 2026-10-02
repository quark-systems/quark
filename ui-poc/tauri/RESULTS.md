# Tauri POC results

All results come from a cloud VM: 4 vCPU, no GPU, Xvfb, Mesa llvmpipe in WebKitGTK and SwiftShader in Chromium.
Other agents were building Rust at the same time, so the load average stayed at **9 to 15 on 4 cores** for every run.
Treat every absolute number as pessimistic and noisy: these are software-rendering-under-contention numbers.
The comparison that carries signal is the relative one: renderer against renderer, and phase against phase, in the same environment.
Repeat on real hardware before go/no-go.

## Benchmark (`?bench=1`, 5 phases x 4 s)

Each figure is the mean of 2 runs; the raw JSON is in `bench/`.
Every run used a private stub instance (`--port 7431`) so other agents' stress toggles could not interfere.
Frame times come from requestAnimationFrame deltas.
The echo column is keydown to echoed glyph rendered: the xterm `write` callback, then the next `onRender`.
"arrived" is keydown until the echo bytes reach the JS WebSocket handler, before xterm sees them.

| config | phase | fps | frame p50 / p99 ms | echo p50 / p99 ms | echo arrived p50 ms |
| --- | --- | --- | --- | --- | --- |
| tauri-webgl | board_idle | 47.7 | 16 / 80 | – | – |
| tauri-webgl | idle_terminals | 7.3 | 108 / 540 | 84 / 1156 | 13 |
| tauri-webgl | stress3_chat_typing | 4.7 | 214 / 310 | 192 / 406 | 45 |
| tauri-webgl | stress4_chat_terminals_visible | 5.2 | 198 / 274 | – | – |
| tauri-webgl | stress4_chat_chat_visible | 23.5 | 18 / 368 | – | – |
| **tauri-dom** | board_idle | 52.0 | 16 / 62 | – | – |
| **tauri-dom** | idle_terminals | 16.4 | 62 / 226 | **46 / 396** | 10 |
| **tauri-dom** | stress3_chat_typing | 8.2 | 118 / 216 | 127 / 224 | 46 |
| **tauri-dom** | stress4_chat_terminals_visible | 6.6 | 148 / 245 | – | – |
| **tauri-dom** | stress4_chat_chat_visible | 26.9 | 17 / 345 | – | – |
| chromium-webgl (web build) | board_idle | 41.8 | 17 / 117 | – | – |
| chromium-webgl | idle_terminals | 4.8 | 217 / 433 | 184 / 441 | 8 |
| chromium-webgl | stress3_chat_typing | 3.0 | 250 / 833 | 325 / 1634 | 34 |
| chromium-webgl | stress4_chat_terminals_visible | 2.1 | 517 / 892 | – | – |
| chromium-webgl | stress4_chat_chat_visible | 24.6 | 17 / 725 | – | – |
| chromium-dom (web build) | board_idle | 43.6 | 17 / 92 | – | – |
| chromium-dom | idle_terminals | 25.5 | 17 / 175 | 39 / 153 | 7 |
| chromium-dom | stress3_chat_typing | 7.3 | 158 / 325 | 127 / 336 | 29 |
| chromium-dom | stress4_chat_terminals_visible | 4.5 | 208 / 392 | – | – |
| chromium-dom | stress4_chat_chat_visible | 20.5 | 33 / 475 | – | – |

Event rates in the stress phases reached about 900 to 2000 events/s of `worker.output` (see `events_per_s` in the JSON).

### Other latency points

- **Daemon alone, no UI** (`scripts/daemon-echo.mjs`, a Node WebSocket client): p50 3.3 ms, p99 17 ms over 40 keys.
  The stub is not the bottleneck: almost all of the UI echo time is spent in the webview.
- **Real X11 keystrokes into the Tauri app** (xdotool into WebKitGTK, WebGL renderer, light background output): echo p50 61 ms, p99 161 ms, n=76.
- **Trusted Playwright keystrokes in Chromium** (WebGL): p50 108 to 143 ms during the first runs, before the private stub. Same story as above.

### Reading the numbers

- **Software WebGL is the dominant cost in this environment.**
  With 4 xterm WebGL contexts on llvmpipe or SwiftShader, fps drops from about 48 on the board to about 5 to 7 with terminals visible, even with only light output.
  xterm's DOM renderer is 2 to 5x better here: 16 to 25 fps idle, echo p50 about 40 to 46 ms.
  On a real GPU the WebGL renderer should win; this needs re-measuring on a Mac or a Linux box with a GPU before go/no-go.
  `?renderer=dom` exists so that comparison is one flag.
- **Echo latency splits into two parts:**
  - Getting the bytes into JS took about 10 ms idle and about 30 to 50 ms under stress. That covers event-loop contention, decoding the base64 JSON frames, and xterm parsing the other panes' floods on the same main thread.
  - Getting from those bytes to pixels took the rest: under contention it waits behind frames that take 100 to 200 ms.
- **Every pane shares one main thread.**
  When 3 panes flood, the quiet shell pane's echo slows down too: p50 goes from 46 to 127 ms on the DOM renderer.
  A native renderer could isolate that work; in a webview the mitigations are throttling xterm writes for hidden or unfocused panes, or moving parsing to a worker.
- **Chat streaming is cheap.**
  With the chat screen visible during a 4-pane flood, fps recovers to 23 to 27, since the terminals still parse but stop painting.
  The markdown re-render per frame is not the bottleneck.
- **Tauri (WebKitGTK) against Chromium on the same frontend:**
  - WebKitGTK had steadier frame times under stress.
  - Chromium had better idle fps on the DOM renderer.
  - Chromium was worse on WebGL, because SwiftShader is slower than llvmpipe here.
  - WebKit rounds `performance.now()` to 1 ms, so its percentiles are integers.

## Bundle size

| artifact | size |
| --- | --- |
| frontend JS (`dist/assets/index-*.js`) | 787 KB (224 KB gzip): React 18, xterm 6 + WebGL addon + fit addon, highlight.js core + 14 languages, marked, cmdk |
| frontend CSS | 17 KB |
| release binary (`opt-level="s"`, LTO, strip) | 4.5 MB |
| `.deb` | 2.0 MB |
| `.AppImage` | 77 MB (bundles WebKitGTK and GStreamer) |
| `node_modules` | 130 MB |
| `src-tauri/target` | 2.0 GB |

The binary links the system's WebKitGTK on Linux, WebView2 on Windows and WKWebView on macOS, so the `.deb` and `.dmg` stay small.
The AppImage is the outlier because it carries its own WebKit.

## Build times (under the 9 to 15 load average described above)

| step | time |
| --- | --- |
| `apt-get install` WebKitGTK 4.1 dev deps | about 1.5 min |
| `npm install` (all deps) | about 30 s |
| cold `cargo build --release` of the Tauri shell (417 crates in the lockfile) | 378 s |
| `tauri build` with bundles, warm deps (first time downloads AppImage tooling) | 213 s |
| `tauri build --no-bundle` incremental (Rust side unchanged, frontend changed) | 137 to 145 s |
| `vite build` | 1.4 to 2.2 s |
| `vite` dev server start | 0.45 s; HMR is instant for frontend changes |

The release profile (LTO, `codegen-units=1`) makes every desktop rebuild cost more than 2 minutes, even when only the frontend changed, because the frontend is embedded in the binary.
In day-to-day work you would use `tauri dev`, which serves the frontend from Vite, or the plain browser, and rebuild Rust only when Rust changes.
`tauri dev` itself was not exercised here.
All UI iteration ran against the Vite dev server in Chromium, which has the same code path.

## Dev-experience notes

- **All five screens worked the first time they were loaded against the real stub.**
  Every interaction check passed in a single Playwright run:
  - typing
  - j then 2 answers a decision
  - diff line comment POST
  - chat send and stream
  - drop and resume the socket at the last seq
  After that pass, the fixes were cosmetic, apart from one real bug, input ordering (below).
- **Leverage from off-the-shelf parts:**
  - xterm.js gives a complete terminal for free, including CJK and emoji width handling, selection, bracketed paste, mouse modes, and the WebGL and DOM renderers.
  - `cmdk` gives the palette in about 80 lines.
  - `marked` with highlight.js gives streamed markdown in about 50 lines.
  - The diff parser and view are hand-rolled, about 200 lines.
- **Total source:** about 1,550 lines of TS/TSX/CSS and 36 lines of Rust.
  Only two things needed Rust:
  - `bench_report`, which prints JSON to stdout
  - the `QUARK_QUERY` passthrough
- **Problem: input ordering.**
  The naive version, one `fetch` per keystroke, delivered keys out of order under load: `echo` arrived as `ecoh`.
  The fix keeps one request in flight per worker and coalesces keys typed in the meantime (`src/api.ts`).
  The native build will hit the same issue if it issues concurrent requests.
  A WebSocket input channel in the real daemon would remove it.
- **Problem: vite and its React plugin.**
  vite 6 conflicts with the current `@vitejs/plugin-react` peer range, so the project moved to vite 8.
- **Problem: Tauri setup on Linux.**
  It needs the WebKitGTK 4.1 system packages.
  There is no window manager under Xvfb, but that was harmless.
- **Problem: libEGL DRI3 warnings.**
  They appear under Xvfb and do not affect function.
- **Problem: the shared stub crashed during development** (a tmux `respawn-pane` crash, since fixed upstream).
  Running a private stub instance on another port avoided interference from other agents.
- **What worked well:**
  - Keyboard handling, including capture-phase global shortcuts over a focused terminal.
  - Focus management.
  - CSS-grid layout and resize via ResizeObserver plus fit.
  - Sticky autoscroll.
  None of these needed custom work.

## Text input

Every check below was run.
In the method column, Chromium means the web build driven by Playwright, and Tauri means the desktop app driven by xdotool under Xvfb.

| area | result | method |
| --- | --- | --- |
| Typing into a terminal | Works, including fast typing, after the ordering fix. | Tauri (xdotool real X11 keys) and Chromium (trusted keys) |
| Terminal selection | Mouse drag selects multi-line text; `term.getSelection()` returns it. | Chromium |
| Paste into a terminal | Ctrl+Shift+V pastes the clipboard as bracketed input: `echo pasted-ok` arrived. | Chromium |
| Copy out of a terminal | xterm copies the selection through the browser copy event. The OS clipboard round-trip was not verified: no clipboard manager under Xvfb. | not fully tested |
| IME in a terminal | xterm handles composition (CDP `imeSetComposition` / `insertText`). The preview stays local and one `onData("日本語")` is sent on commit. The first stub's bash had no UTF-8 locale and mangled the bytes; after the stub fix, the shell line reads `worker@quark:~# 日本語` end to end. | Chromium CDP |
| IME in text fields | The chat textarea receives committed "漢字テスト". Enter during composition is ignored (`isComposing` check) so it does not send early. | Chromium CDP |
| Real OS IME (ibus/fcitx) and dead keys | Untested: no IME daemon in this headless environment. WebKitGTK's ibus integration is the thing to check on a real desktop. | – |
| Font fallback | CJK (Japanese, Chinese, Korean) and color emoji render in xterm cells and in the DOM, in both WebKitGTK and Chromium. Fonts are DejaVu Sans Mono, then IPA Gothic / WenQuanYi, then Noto Color Emoji. See `screenshots/tauri-2-terminals-typed.png` and `web-terminals.png`. | Tauri and Chromium screenshots |
| Text selection and copy in chat and diff | Native DOM selection works. The diff line-number gutter is `user-select: none`, so copied code has no line numbers. | Chromium |
| Accessibility | Not evaluated. xterm has a screen-reader mode, which is not enabled. | – |

## Screenshots

In `screenshots/`:

- `tauri-*.png`: the real Tauri binary under Xvfb against the stub.
- `web-*.png`: the same frontend in headless Chromium via the Vite dev server, which is the "web build" story.
- `tauri-2-terminals-before-input-order-fix.png`: shows the `ecoh` reorder bug.
