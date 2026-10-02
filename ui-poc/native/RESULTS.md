# Native (warpui) POC results

Machine: 4-core Linux VM shared with other agents (load average 4-6 during the final runs, up to 13 earlier), no GPU.
Rendering ran under Xvfb with wgpu on **lavapipe (software Vulkan)**, so every frame-time number below is dominated by a CPU rasterizer and is pessimistic for real hardware.
I did not have a GPU to confirm how much faster it is there, so treat the GPU-present column as "software rendering cost", not a prediction.
Raw bench JSON is in `bench-results/`.

## How the bench works

`--bench 30` waits for the daemon and the four panes, settles 3 s, then runs two phases and prints JSON:

- `idle_typing` (35%): the stub's normal pane output, and a probe that types one uppercase letter at a time into the bash pane (one outstanding, at most ~8/s, `^U` every 40 keys).
- `stress_4_terminals_plus_chat` (65%): stub stress flood on all four panes (~4.7 MB/s total reached the UI) while the coordinator chat streams markdown with code blocks into the docked chat panel, probe still typing.

Measured per frame: **frame** = layout start to after present (a probe element records layout start and paint end; `on_frame_drawn` records present), split into **view render** (`render()` building the element tree), **layout+paint** (warpui layout + scene building), **GPU submit+present**.
Measured per probe key: key event to echo bytes read off the WebSocket (network thread), WebSocket thread to UI thread (queueing), and **key to painted echo** (key event to the end of the first frame presented after the echo was applied).
Probe keys are uppercase because the stress flood contains no uppercase ASCII, so the echo is unambiguous under load.
"Internal" runs call the app's key handler directly; `--xdotool` runs send real X11 key events.

## Results: alacritty_terminal vs libghostty-vt

Interleaved runs (alacritty, ghostty, alacritty, ghostty), fresh stub for every run, same binary built with `--features ghostty`, Vulkan (lavapipe). Times in ms, `p50 / p99`.

### Idle + typing

| Run | fps | frame | GPU submit+present p50 | layout+paint p50 | view render p50 | key to echo on WS | WS to UI | key to painted echo |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| alacritty #1 | 22.3 | 34.3 / 48.9 | 33.3 | 1.02 | 0.14 | 0.9 / 3.0 | 2.1 / 50.6 | 45.4 / 89.8 |
| alacritty #2 | 22.3 | 34.7 / 46.1 | 33.6 | 1.07 | 0.15 | 0.9 / 2.5 | 1.2 / 49.6 | 45.8 / 93.5 |
| ghostty #1 | 22.0 | 34.6 / 51.7 | 33.1 | 1.11 | 0.15 | 1.0 / 3.7 | 33.1 / 53.6 | 71.5 / 104.8 |
| ghostty #2 | 22.5 | 33.9 / 45.8 | 32.8 | 1.08 | 0.14 | 1.0 / 2.1 | 3.2 / 43.6 | 48.3 / 87.3 |
| alacritty, xdotool | 22.2 | 36.2 / 50.2 | 35.0 | 1.10 | 0.15 | 1.0 / 2.9 | 0.5 / 47.0 | 41.8 / 93.2 |
| ghostty, xdotool | 22.4 | 35.0 / 50.7 | 33.6 | 1.05 | 0.14 | 0.9 / 4.6 | 0.2 / 45.9 | 41.1 / 86.7 |

~70 probe keys per run. Ghostty #1's higher queueing p50 looks like noise (its repeat and the xdotool run do not show it).

### Stress: 4 flooding terminals + streaming chat

| Run | fps | frame | GPU submit+present p50 | layout+paint p50 | view render p50 | key to echo on WS | WS to UI | key to painted echo |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| alacritty #1 | 11.1 | 73.2 / 113.1 | 69.7 | 2.60 | 0.32 | 21.9 / 48.4 | 62.3 / 105.6 | 169.3 / 233.5 |
| alacritty #2 | 11.3 | 72.2 / 112.3 | 69.3 | 2.49 | 0.30 | 21.3 / 44.4 | 60.3 / 105.2 | 165.1 / 233.7 |
| ghostty #1 | 7.8 | 70.8 / 103.8 | 68.5 | 2.42 | 0.33 | 21.5 / 39.0 | 20.6 / 95.2 | 142.2 / 278.3 |
| ghostty #2 | 8.0 | 71.6 / 95.1 | 68.6 | 2.54 | 0.35 | 21.5 / 37.9 | 20.1 / 86.1 | 135.3 / 257.9 |
| alacritty, xdotool | 10.8 | 75.4 / 104.0 | 71.7 | 3.12 | 0.40 | 22.6 / 40.5 | 64.9 / 99.1 | 171.6 / 268.0 |
| ghostty, xdotool | 7.9 | 73.5 / 110.4 | 70.8 | 2.42 | 0.32 | 24.1 / 43.4 | 22.0 / 82.4 | 138.5 / 268.7 |

~90 MB of terminal output and ~400 chat deltas reached the UI per stress phase (19.5 s).

### Terminal core cost on the UI thread

Time inside the core (parsing output + rebuilding dirty snapshot rows), summed over the 19.5 s stress phase:

| Core | parse | snapshot | total | share of UI thread | ms per MB of output | per rebuilt row |
| --- | --- | --- | --- | --- | --- | --- |
| alacritty_terminal | 1.8-1.9 s | 0.52-0.59 s | 2.4 s | ~12% | 26.5 | ~0.65 µs |
| libghostty-vt | 3.2-3.4 s | 4.2-4.3 s | 7.5-7.7 s | ~39% | 79-81 | ~4.9 µs |

Idle: 6 ms (alacritty) vs 15-17 ms (ghostty) per 10.5 s.

### What the numbers say

- **warpui's own CPU cost is small.** Building the element tree is 0.15-0.4 ms and layout+scene building is 1-3 ms per frame even with four flooding 64x25 terminals (the panes' size with the chat dock open) plus a streaming markdown chat with highlighted code. Nearly all of the frame is GPU submit+present on the software rasterizer (33 ms idle, ~70 ms under stress, for a 1560x960 window).
- **fps is capped by the software rasterizer**, not by the app: idle fps (~22) is the redraw rate for the live panes at ~34 ms per present; under stress the present gets slower (more glyphs and colour runs) and fps halves.
- **Key to painted echo** is ~45 ms p50 idle, which is roughly 1 network round trip (~1 ms) + waiting for the frame in progress + one ~34 ms software frame. Under stress it is 135-170 ms p50 because the echo waits behind ~70 ms frames and 20-60 ms of queued output. On a real GPU this should mostly collapse, but I could not verify it.
- **libghostty-vt costs about 3x the UI-thread time of alacritty_terminal in this integration**, mostly in the snapshot step: the libghostty-rs API reads cells through several C calls each (graphemes, style, fg, bg, wide), so a 64-cell row is ~320 FFI calls. Only dirty rows are read, which helps when idle but not under a full-screen flood. That cost shows up as lower stress fps (8 vs 11), while frame time itself is unchanged (the core runs between frames). Key-to-paint p50 under stress was actually lower with ghostty (UI-thread queueing 20 ms vs 60 ms), which I did not investigate; p99 is similar.
- An obvious next step for the ghostty path, not done here: skip the style/colour calls for cells with the default style, and read single-codepoint cells from the raw cell instead of the grapheme buffer. The sibling `../term-bench/` measures the cores in isolation and is the better source for raw parser throughput.
- **Vulkan vs GL** (earlier session, alacritty only, single runs, machine more loaded): lavapipe Vulkan idle 20.8 fps / frame 38.2 / 54.7; llvmpipe GL (`WGPU_BACKEND=gl`) 18.4 fps / 44.9 / 63.6. Under stress 11.0 fps / 74 / 110 vs 10.0 fps / 84 / 125. GL is ~15% slower in software; not meaningful for real GPUs.

An early run (before the fixes listed in TIMELOG.md) showed view render at 14 ms per frame: re-highlighting every code block with syntect on every frame. Caching highlighted blocks by `(lang, code)` fixed it. That was the only app-side hot spot found.

## Size, build, dependencies

| Item | Value |
| --- | --- |
| Release binary (alacritty only) | 31,919,440 bytes; 23,414,232 stripped |
| Release binary with `--features ghostty` | 34,318,824 bytes; 25,550,304 stripped (libghostty-vt linked statically, +2.1 MB stripped) |
| Cold release build (empty target dir, deps already downloaded) | 513 s (8 min 33 s), 4 cores, load average 7-10, overlapping another build for ~3 min. Expect less on an idle machine; I had no clean slot to repeat it. |
| First build with `ghostty` on top of an existing target dir | 2 min 56 s (Zig compiles libghostty-vt from the pre-loaded package cache; Zig 0.16 required) |
| Incremental release build after editing 1-2 app files | 12.6-17 s at load ~5; 20-40 s earlier under heavier load |
| `Cargo.lock` packages | 695 (includes the ghostty optional deps and the wasm-only deps) |
| `cargo tree -e normal` unique crates | 576 desktop (579 with `ghostty`), 438 for wasm32 |
| App source | 4,510 lines of Rust (`app.rs` 2,274; `elements.rs` 638; `api.rs` 465; `term.rs` 321; `term_ghostty.rs` 303) |

Release profile is the default plus `debug = false`; no LTO, no `codegen-units = 1`, no `strip`, so the binary can shrink further.
Nearly all of the dependency count comes from warpui (wgpu, winit, font loading and shaping, image, async runtime pieces); this app adds about a dozen direct dependencies.
Building warpui from git also needs two `[patch.crates-io]` entries copied from Warp's root `Cargo.toml` (forks of `pathfinder_simd` and `yaml-rust`), because a git dependency does not inherit its workspace's patches.

## Web build (wasm32, headless Chromium)

Board, chat, diff, decisions/PRs, palette and perf overlay run in the browser from the same source; only the daemon transport and font loading differ (see README "Web build").
Terminals are excluded: `alacritty_terminal` depends on `polling` unconditionally, which has no wasm32 support, so panes show a notice.

Verified in headless Chromium (Playwright, WebGL2 on SwiftShader) against the stub on `--port 7422`:

| Check | Result |
| --- | --- |
| Render + connect | Rendered correctly and connected over WebSocket on the first launch; the only web-specific fix afterwards was paste (below). |
| Live updates | `web-1-board-a.png` and `web-2-board-b.png`, 8 s apart: "Add backpressure to event fan-out" moved Review -> Done from a `task.state_changed` event; status bar seq advanced. |
| Chat | Typed with Playwright keyboard into the chat input, Enter POSTed it, the streamed markdown reply rendered (`web-3`, `web-4`). |
| Diff, decisions/PRs, palette, perf overlay | Render and respond to keys as on desktop (`web-6` to `web-9`). |
| Copy | Works: drag-select in chat + Ctrl+C reached `navigator.clipboard` (read back in the test). |
| Paste | Works after a fix: warpui never delivers Ctrl+V as a key on the web; it lets the browser fire `paste` and dispatches `StandardAction::Paste`, which I had to bind to an app action. |
| Non-ASCII keys | Work when they arrive as real key events (`é ß ж` via CDP keyDown, as a non-US layout produces). |
| IME / inserted text | **Do not work**: IME composition and `insertText` (no key event) never reach the app; the canvas has no text-input/IME path on desktop browsers (warpui's hidden `<input>` is only used for mobile soft keyboards). |
| Fonts | Only the embedded DejaVu fonts exist; CJK, Hangul and emoji render as boxes (warpui logs "No fallback_font_source_provider registered"). |
| Browser shortcuts | warpui lets `Ctrl+1..9`, `Ctrl+L`, `Ctrl+T/W/N/R` and others through to the browser, so in a real browser `Ctrl+1..5` would switch browser tabs. Headless Chromium has one tab, so the app still received them in the tests. |
| Reconnect/resume | Same logic as desktop but not exercised on web (restarting the stub resets its seq space). |

Size and build:

| Item | Value |
| --- | --- |
| `.wasm` after wasm-bindgen (release, no LTO) | 13,743,920 bytes; 4,887,856 gzip -9 (brotli not available here) |
| of which embedded fonts | 2,146,048 bytes (1,142,854 gzipped) for DejaVu Sans + Mono, regular + bold |
| JS glue | 115,905 bytes |
| wasm-opt | Not done: binaryen 116 (the newest via `cargo install wasm-opt`) cannot parse rustc 1.97's output ("invalid code after misc prefix: 17"); a newer binaryen release would be needed. |
| Cold wasm release build (empty target dir) | 169 s at load ~4-5 |
| Incremental wasm build after editing app files | 11 s; `wasm-bindgen` ~2 s |
| Crates in the wasm build | 438 (`cargo tree --target wasm32-unknown-unknown -e normal`) |

Performance on web was not benchmarked (`--bench` is desktop-only).
The perf overlay showed idle frames of 2.2 ms p50 / 18 ms p99 CPU time on the board, but on WebGL "GPU submit+present" only covers handing commands to the browser, which rasterizes asynchronously (here in SwiftShader), so these numbers are not comparable with the desktop ones.

## Text handling checks

| Check | Result |
| --- | --- |
| Selection + copy in chat | Works: drag-select across messages (`SelectableArea`), `Ctrl+C`, verified with `xclip -o`. |
| Selection + copy in terminals | Works: drag-select cells, `Ctrl+Shift+C`; whole pane when nothing is selected. Verified with `xclip` on both cores. |
| Paste | Works into chat, comment composer and terminals (`Ctrl+V` / `Ctrl+Shift+V`). |
| Font fallback | Works: CJK, Hangul, emoji and box drawing render in chat (from pasted text) and in terminals (from stub output). In the terminal grid each non-ASCII cell is positioned individually so a fallback font's different advance cannot shift the row. |
| Unicode typed input | Not reliably testable headlessly: `xdotool type` with non-ASCII under Xvfb drops or duplicates characters (keymap remapping artifact; "日本語" arrived as "本本"). Verified via clipboard paste instead. |
| IME preedit | Wired but untested: `SetMarkedText`/`ClearMarkedText` are handled and drawn underlined at the caret, and `active_cursor_position` reports the caret rect for the candidate window. No input method runs under Xvfb, so the path never fired. |
| Combining characters in terminals | Dropped (snapshot stores one char per cell). |
| Right-to-left text | Not tested. |

## warpui API: ergonomics and pain points

Good:

- The `View` / `TypedActionView` / `ViewContext` model is small and consistent. `spawn_stream_local` plus an `async_channel` is a clean way to feed background network results into a view, and kept every bit of I/O out of view code; the web build later swapped only the client's transport (fetch/WebSocket) and no view code changed.
- Writing custom elements is direct: the `Element` trait (layout / after_layout / paint / dispatch_event) and the scene API (`draw_rect`, layers with clip bounds, `Line::paint`) were enough for a terminal grid, an editor line and a frame probe in a few hundred lines.
- Text layout is cached per (text, style), so re-rendering a mostly unchanged tree every frame is cheap; the measured element-tree build is well under 1 ms.
- `Flex`, `Container`, `ConstrainedBox`, `Stack` with positioned overlays, `ClippedScrollable` + `SavePosition` + `scroll_to_position` cover dense app layouts with little code.
- Rendering through wgpu worked on lavapipe and llvmpipe without any configuration.

Pain points, in order of cost:

1. **No reusable text editor.** warpui's `TextInput` element wraps an editor view that lives in Warp's AGPL app crates. I wrote a single-line editor element (char-indexed buffer, selection, word motions, clipboard, horizontal scroll, marked text). A real multi-line editor with undo, wrapping and good IME would be a large piece of work.
2. **No terminal grid element.** Expected for a UI toolkit, but it means the terminal renderer (runs batching, wide cells, fallback-font alignment, cursor, selection) is app code.
3. **No markdown support usable here.** Warp's markdown parser is AGPL; I mapped `pulldown-cmark` events onto `Text` with highlight ranges, which works but has no inline images or rich layout.
4. **Undocumented sharp edges**, each found by crashing or misrendering: `FontCache::em_width` is the ink width of "m", not the advance (terminal cells overlapped until I measured the advance of "M" myself); `UniformList` panics with zero items (it builds item 0 to measure); `Text::new_inline` ignores newlines; key names arrive shifted (`"S"` for Ctrl+Shift+S).
5. **Key input plumbing.** `EventHandler` has no hooks for typed characters or IME, so I wrote a `KeyCapture` element; keybindings dispatch to the focused view first, then the element tree, and typed characters come as a separate event only when a key-down is unhandled. IME needs `active_cursor_position` plumbing from the view.
6. **Hover needs persistent state.** `Hoverable` wants a `MouseStateHandle` per item kept across renders; for dynamic lists I used stateless `EventHandler` clicks and no hover effects.
7. **No exposed frame timing.** The `traces` feature only prints; I measured frames with a probe element plus the global `on_frame_drawn` callback.
8. **Web text input is thin.** On wasm there are no system or fallback fonts (you embed them), no IME or composition on desktop browsers, and paste arrives as `StandardAction::Paste` instead of a key. Building for wasm itself was easy: the app compiled for wasm32 on the first try once the desktop-only crates were behind target cfgs.
9. **Documentation is the source code.** No guide or API docs beyond examples; learning took 11 minutes here only because the examples are good and the code is readable.

## Honest assessment

warpui is a capable, fast immediate-style retained-tree UI framework: everything the five screens needed was buildable in about 34 active minutes including learning the API (plus 12 for instrumentation), and the framework's own per-frame CPU cost stayed at a few milliseconds under heavy load.
What it does not give you is the expensive part of a Warp-like app: the text editor, terminal rendering, markdown and their input-method handling are either missing or live in Warp's AGPL crates, so a Quark client on warpui would own those components outright.
It is also a single-company framework without versioned releases, docs or a stability promise (we pin a git rev and copy two crate patches from its workspace), so upgrades are on us.
Rendering on this VM was software-only, which makes the latency numbers look poor (45 ms idle key-to-paint, ~170 ms under extreme load); I expect a real GPU to cut most of that, but that is unverified.
The web build was cheap (about 25 minutes, one shared codebase, 4.9 MB gzipped) and the non-terminal screens behave the same in Chromium, but international text on the web (fallback fonts, IME) is missing in warpui today and would be Quark's problem to solve.
Between the two terminal cores, alacritty_terminal was ~3x cheaper per byte on the UI thread in this integration; libghostty-vt's key encoder was easy to adopt and correct (arrows, Ctrl, vim's alternate screen all worked first try), and its cost is mostly in the per-cell FFI reads, which can likely be reduced.
