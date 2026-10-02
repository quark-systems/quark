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

## Terminal renderer comparison (follow-up spike, 2026-10-02)

Selected with `?renderer=` (or `QUARK_QUERY` in the desktop app).
xterm.js WebGL stays the default.

| flag | implementation | license |
| --- | --- | --- |
| `webgl` (default) | xterm.js 6 with the WebGL addon | MIT |
| `dom` | xterm.js 6 with the DOM renderer | MIT |
| `wterm` | @wterm/dom 0.5.4 (DOM renderer) with the @wterm/ghostty core (libghostty compiled to WASM) | Apache-2.0 |
| `wterm-lite` | @wterm/dom 0.5.4 with its built-in Zig core (WASM) | Apache-2.0 |
| `ghostty-web` | ghostty-web 0.4.0: Ghostty's VT core in WASM, xterm.js-compatible API, canvas2d renderer | MIT |

All five run through one adapter interface (`src/term/`), so input, ordered POSTs, resize, the keydown capture for the echo probe, and the bench are identical for each.
"Rendered" means:

- **xterm:** the next `onRender` after the write callback, as before.
- **wterm:** a `requestAnimationFrame` registered right after `write()`. WTerm schedules its paint in rAF during `write()`, so ours runs after its DOM update in the same frame.
- **ghostty-web:** its write callback, which it runs in a rAF registered after its render loop's.

### Numbers: Tauri desktop app (WebKitGTK, Xvfb, software rendering)

Each figure is the median of 3 rounds, each round on a fresh stub (`h2h/run.sh` with `RENDERERS=... NATIVE=0`).
Raw JSON and logs are in `../h2h/results-renderers/`; the table comes from `scripts/summarize-renderers.py`.
The load average was about 2 to 5 during these runs, much quieter than the first round of benchmarks, so do not compare these absolute numbers with the earlier tables.
Echo is keydown to glyph painted, p50 / p99 ms.

| renderer | 4 terminals idle, typing: fps / echo p50 / p99 | 3 flooding + chat, typing: fps / echo p50 / p99 (echo bytes arrived p50) | 4 flooding + chat: fps / frame p50 | 4 flooding, chat visible: fps | terminal open ms |
| --- | --- | --- | --- | --- | --- |
| xterm webgl | 15.7 / 38 / 139 | 4.7 / 332 / 823 (101) | 4.4 / 212 | 27.4 | 264 |
| xterm dom | **27.9 / 27 / 62** | **7.2 / 126 / 360 (43)** | **6.7 / 147** | 21.1 | 158 |
| wterm (Ghostty core) | 18.4 / 33 / 85 | 4.2 / 909 / 1466 (812) | 3.2 / 307 | 10.1 | 226 |
| wterm-lite (Zig core) | 20.4 / 35 / 80 | 4.8 / 204 / 722 (63) | 3.7 / 263 | 16.3 | 114 |
| ghostty-web | 25.2 / 25 / 63 | 5.0 / 69 / 1619 (n=4 only) | **never finished** | **never finished** | – |

**ghostty-web never finished in WebKitGTK.**
In all 3 rounds it never got past the 4-pane flood phase, even with a 90 s limit, and once with 300 s.
It completed the idle and 3-pane phases, which are taken from the per-phase progress lines.
It falls behind the event stream, and WebKit's event loop then starves the phase timers.
In the 3-flood phase only 4 or 5 of about 30 keystrokes echoed within the phase.
The same run in Chromium completes, but at 1.7 to 2.5 fps under load.

**wterm with the Ghostty core falls behind under 3 flooding panes**, without stalling:

- Echo bytes arrived 812 ms after the keystroke (p50), against 43 ms for xterm DOM.
- It processed about 1,100 events/s, against about 1,500 for the other renderers in the same phase.
- It caught up once the terminals were hidden.

wterm-lite does not have this problem, so the cost is in the Ghostty core path, not the DOM renderer.

### Numbers: headless Chromium (web build, SwiftShader), 1 run each

| renderer | 4 terminals idle, typing: fps / echo p50 / p99 | 3 flooding + chat, typing: fps / echo p50 / p99 (arrived p50) | 4 flooding + chat: fps | 4 flooding, chat visible: fps |
| --- | --- | --- | --- | --- |
| xterm webgl | 15.2 / 38 / 81 | 3.0 / 290 / 1443 (25) | 1.7 | 20.9 |
| xterm dom | 45.1 / 15 / 36 | 7.4 / 147 / 618 (52) | 5.2 | 30.7 |
| wterm | 56.8 / 14 / 45 | 10.9 / 758 / 898 (675) | 8.3 | 18.2 |
| wterm-lite | **57.7 / 13 / 47** | **10.5 / 142 / 521 (61)** | **7.3** | 28.2 |
| ghostty-web | 18.7 / 33 / 197 | 2.5 / 760 / 2466 (47) | 1.8 | 1.7 |

- **Hidden ghostty-web terminals keep rendering.** With the chat screen visible, ghostty-web stays at 1.7 fps because its per-terminal rAF render loop keeps painting the detached canvases. xterm stops painting when its element is out of the document. wterm pauses only for a hidden document, or when the host sets `renderingPaused`; the POC doesn't set it.
- **wterm's DOM rendering does much better in Chromium than in WebKitGTK.** At idle it is the best renderer in Chromium (57 fps, 13 to 14 ms echo), but in Tauri on Linux it is behind xterm DOM. On macOS, Tauri uses WKWebView, which has a different DOM and paint performance profile, so re-measure there.

### Bundle and WASM cost (vite build; each renderer is its own lazy chunk)

| renderer | JS | WASM | CSS | gzip total |
| --- | --- | --- | --- | --- |
| xterm (webgl + dom + fit) | 447 KB | – | 4 KB | about 116 KB |
| wterm (Ghostty core) | 106 KB (DOM) + 21 KB (Ghostty bindings) | 592 KB `ghostty-vt.wasm`, a separate asset | 8 KB | about 236 KB |
| wterm-lite | 106 KB + 37 KB (Zig core inlined as base64) | (inlined, about 26 KB raw) | 8 KB | about 47 KB |
| ghostty-web | 637 KB (423 KB WASM inlined as base64) | (inlined) | – | about 187 KB |

### Feature checks

Checked with `scripts/renderer-check.mjs` (Chromium, trusted input plus CDP).
Raw output is in `bench/renderers/features-chromium-*.json`.
The Tauri screenshots confirm rendering in WebKitGTK.

| check | xterm (webgl/dom) | wterm / wterm-lite | ghostty-web |
| --- | --- | --- | --- |
| Mouse selection | yes | yes (native DOM selection) | yes |
| Copy | browser copy event (menu, Ctrl+Insert); Ctrl+C sends ^C | **Ctrl+C copies when there is a selection**, otherwise sends ^C; copy event also works | copy-on-select only; the copy event and Ctrl+Shift+C do nothing |
| Paste | Ctrl+Shift+V and the paste event, bracketed | paste event, bracketed; **Ctrl+V and Ctrl+Shift+V send ^V**, so Linux needs a host-side paste binding | Ctrl+V, Ctrl+Shift+V and the paste event, bracketed |
| IME (CDP composition) | preview stays local; one `日本語` on commit; echoed correctly | same | same |
| CJK / emoji rendering | wide cells, color emoji | wide cells, color emoji (DOM text, so browser font fallback) | wide cells; emoji drawn by canvas fillText |
| Kitty keyboard: query `CSI ? u` after `CSI > 1 u` | no reply (not supported) | replies `CSI ? 1 u` | no reply |
| Kitty mode: Ctrl+Shift+B / Shift+Enter / Esc / Ctrl+I | (nothing) / `\r` / `ESC` / `\t` | `CSI 98;6u` / `CSI 13;2u` / `CSI 27u` / `CSI 105;5u` | `^B` / `\r` / `ESC` / `CSI 105;5u` |
| Ctrl+Shift+A | nothing | swallowed by wterm's Select All binding, even in Kitty mode, so the requested `CSI 97;6u` check cannot pass; the encoder itself works (see Ctrl+Shift+B) | `^A` |
| Legacy-mode oddities | Ctrl+Shift+letter sends nothing | Shift+Enter sends `CSI 13;2u` even without negotiation | **Ctrl+I sends `CSI 105;5u` even without negotiation** (bash prints `[105;5u`), a correctness bug |
| WASM in Tauri asset protocol | n/a | works: the `.wasm` asset is fetched from `tauri://` with no changes | works: base64 `data:` URL |
| CSP | works under `default-src 'self'` | needs `'wasm-unsafe-eval'` | needs `'wasm-unsafe-eval'` **and `data:` in `connect-src`**; without it the panes stay blank with no visible error |
| React StrictMode | n/a | n/a | n/a |

The POC keeps terminal instances outside React (one per worker, re-parented into the grid), so a StrictMode double-mount cannot create or destroy terminals.
That is also why it uses `@wterm/dom` rather than the `@wterm/react` component: the component's lifecycle would tie terminals to mounts.

### Integration friction

- **wterm.** About 50 lines.
  - The theme maps cleanly onto CSS custom properties.
  - The default `.wterm` card styling (padding, radius, shadow) had to be zeroed.
  - `readText()` is async, so the "cursor line" test helper is async.
  - No problems loading WASM in Tauri.
- **ghostty-web.** About 45 lines; it really is xterm-API compatible: `onData`, `onResize`, `write(data, cb)`, `input()`, FitAddon.
  - Its inlined `data:` WASM collides with a strict CSP, silently.
  - It keeps a per-terminal rAF render loop running while hidden.
  - It is the only renderer that never completed the full bench in WebKitGTK.
- **Bench adjustments made for these renderers.**
  - The bench now records `timer_lag_ms` per phase.
  - Tauri prints `QUARK_BENCH_PROGRESS` per phase, so a run that never finishes still leaves numbers.
  - `h2h/run.sh` gained `RENDERERS`, `NATIVE`, `PORT` and `TAURI_TIMEOUT`, and keeps a `.log` per run.

### Recommendation

**Keep xterm.js as the default, switch it to the DOM renderer on Linux/WebKitGTK until WebGL is measured on real GPUs, and treat wterm-lite as the one alternative worth tracking.**
Don't adopt ghostty-web.

- **xterm DOM** was the most consistent under load in WebKitGTK (best fps and echo in every loaded phase). xterm WebGL may win on a real GPU; this box has none.
- **wterm-lite** is the strongest alternative.
  - It was the fastest renderer in Chromium (idle 58 fps, echo 13 ms) and the smallest (about 47 KB gzip).
  - Its DOM selection, Ctrl+C-copies-when-selected behavior and Kitty keyboard support are features xterm lacks.
  - The cost: it trails xterm DOM in WebKitGTK under load, and Linux paste needs a host-side binding.
  - Re-measure it on macOS WKWebView before deciding; Chromium and WebKitGTK disagree here.
- **wterm with the Ghostty core** adds the 592 KB WASM file and was slower to parse floods than the lite core (echo bytes arrived 0.7 to 0.8 s late under 3 floods). That is only worth it if the full libghostty VT feature set is needed: Kitty graphics, grapheme clusters, reflow.
- **ghostty-web** ruled itself out:
  - It never finished the flood bench in WebKitGTK.
  - Hidden terminals keep rendering.
  - Ctrl+I is wrong in legacy mode.
  - It has no Kitty negotiation and no copy-event support.
  - Its inlined WASM fights a strict CSP.
