# ADR-3 POC findings: native Rust (warpui) vs Tauri

Date: 2026-10-02.
Warp pinned at `warpdotdev/warp@2a8604f`, libghostty-rs at `Uzaaft/libghostty-rs@8953a740` (Ghostty `22d13172`).

## Recommendation

**No-go on native warpui for the MVP. Build the desktop app with Tauri, React and xterm.js, one UI for desktop and web.**

The native prototype works and warpui itself is fast, but it fails three of the six criteria independently of performance:

1. Its dependency closure is not MIT (see Maintainability), so shipping it would make Quark AGPL until Warp relicenses eight helper crates.
2. The parts that make a Warp-like client expensive (text editor, terminal grid, markdown, IME) are not in the MIT crates, so Quark would own them outright.
3. Its web build renders, but without IME or font fallback, so it does not give us a usable second client for free.

Performance does not change the answer: neither option reaches 60 fps on this GPU-less VM, and the relative results are mixed (below).
Before closing ADR-3, run `h2h/run.sh` once on a Mac with a GPU; only a large native win there, combined with Warp relicensing the helper crates, would justify reopening this.

For terminals: render with xterm.js in the webview (DOM renderer on Linux until WebGL is measured on a GPU), run libghostty-vt in the daemon per tmux pane for snapshots, scrollback search and transcripts, and track wterm as the most promising xterm.js replacement (see Terminal in the webview).
Embedding a native Ghostty surface in the Tauri window is out: Ghostty has no stable embedding API, and layering a native view over a webview is fragile on every platform.

## Scorecard

| Criterion | Native (warpui) | Tauri |
| --- | --- | --- |
| Standalone build | Pass, with caveats: builds from a clean crate by git rev, mirroring 2 `[patch]` entries; pulls Warp's forks of winit, font-kit, cosmic-text, pathfinder | Pass |
| 60 fps, <100 ms input | Not measurable here (software rendering); see Performance | Not measurable here; see Performance |
| Dev speed ≤1.5× React | Fail: about 1.8× the total time for the same scope (69 vs 38 agent minutes) and far more on screen code alone, with an editor, markdown renderer, terminal grid and key capture written from scratch | Baseline |
| Selection, copy, IME, font fallback | Desktop: selection, copy, paste, CJK/emoji fallback pass; IME wired but untested. Web: no IME, no fallback fonts | Pass (IME simulated through Chromium devtools; real ibus/fcitx untested) |
| Web build | Partial: board, chat, diff, inbox and palette run in Chromium (4.9 MB gzipped wasm); terminals excluded; IME and fonts fail on web | Pass: same frontend in the browser |
| Maintainability | Fail: AGPL helpers, no releases, about one upstream commit a day, single vendor | Pass |

## Licensing

`warpui` and `warpui_core` are MIT, but they depend on eight workspace crates that inherit `license = "AGPL-3.0-only"`: warp_errors, markdown_parser, sum_tree, warp_util, command, settings_value, settings_value_derive and string-offset (about 18k lines).
Warp's FAQ says the UI crates are meant for use outside Warp, so this looks like an oversight, but until it is fixed a binary linking warpui is AGPL.
The crates are `publish = false` at version 0.0.0, so they can only be pinned by git rev; about 97 commits touched them from July to September 2026.

## Performance

All numbers come from a 4-core VM with no GPU: native renders through lavapipe (software Vulkan), Tauri through WebKitGTK's software path.
They are pessimistic in absolute terms and only the relative behaviour carries weight.
`h2h/run.sh` ran both apps back to back, 3 rounds, with a fresh stub daemon per run; medians are below and the raw JSON is in `h2h/results/`.

| App | Scenario | fps | frame p50 ms | key to echo painted, p50 / p99 ms |
| --- | --- | --- | --- | --- |
| Native, alacritty_terminal | 4 terminals, light output, typing | 20.8 | 42.8 | 70 / 109 |
| Native, alacritty_terminal | 4 panes flooding + chat streaming, typing | 10.9 | 90.5 | 169 / 235 |
| Tauri, xterm.js DOM | 4 terminals, light output, typing | 38.1 | 23 | 17 / 59 |
| Tauri, xterm.js DOM | 3 panes flooding + chat streaming, typing | 8.9 | 111 | 103 / 283 |
| Tauri, xterm.js DOM | 4 panes flooding + chat streaming | 8.6 | 113 | not probed |
| Tauri, xterm.js WebGL | 4 terminals, light output, typing | 23.2 | 41 | 23 / 88 |
| Tauri, xterm.js WebGL | 3 panes flooding + chat streaming, typing | 4.4 | 201 | 319 / 1132 |

- Tauri is faster when terminals are quiet; native degrades less when all four panes flood.
- In the native app warpui's own work is small (under 0.4 ms view build, 1-3 ms layout and paint); nearly all of each frame is the software rasterizer presenting the full window.
- In Tauri one flooding pane slows every pane, because all terminals share the webview's main thread.
- xterm.js's WebGL renderer loses to its DOM renderer here only because WebGL is emulated in software; on a GPU that should reverse.
- With libghostty-vt instead of alacritty_terminal, native stress fps fell from 11 to 8, mostly from reading cells through several C calls each (see `native/RESULTS.md`).

## Terminal core

From `term-bench/RESULTS.md`: libghostty-vt ingests TUI redraws about 2× faster than alacritty_terminal, uses 17.7 MiB vs 46.8 MiB for 10k lines of scrollback (4.1 MiB after compression), and ships Kitty keyboard, mouse and graphics support that alacritty_terminal lacks.
It is slower on plain logs and emoji-heavy text, its full-frame reads cost 3-4× more, and its types are not `Send`.
Building it needs Zig 0.16 and four Zig packages fetched by hand behind this network policy; the crates.io release (0.2.1) pins an older Ghostty, so we used the bindings' master branch.
Neither core builds for wasm32 as a Rust crate; Ghostty's standalone `ghostty-vt.wasm` (832 KB) does.

## Development speed

Both apps were built by agents with the same brief, so the time logs compare framework friction more than human effort.
The Tauri app took about 38 minutes end to end; the native app took about 69 minutes, plus 24 for the libghostty backend and 25 for the web build.
The gap comes from what warpui lacks: a reusable text editor (Warp's is in its AGPL app), a terminal grid element, markdown, typed-text and IME hooks, and any documentation beyond the examples.
It also hit `em_width` returning ink width instead of advance, a `UniformList` panic on zero items, and no exposed frame timing.

## Terminal in the webview

After ADR-3 settled on Tauri, two Ghostty-based web terminals were swapped into the Tauri prototype in place of xterm.js and run through the same bench (`tauri/RESULTS.md`, "Terminal renderer comparison"; raw data in `h2h/results-renderers/`).
Medians of 3 rounds in the Tauri desktop app, software rendering:

| Renderer | 4 terminals, typing: fps / echo p50 ms | 3 panes flooding + chat: fps / echo p50 ms | 4 panes flooding: fps | Added gzip |
| --- | --- | --- | --- | --- |
| xterm.js DOM | 27.9 / 27 | 7.2 / 126 | 6.7 | ~116 KB |
| xterm.js WebGL | 15.7 / 38 | 4.7 / 332 | 4.4 | ~116 KB |
| wterm, built-in Zig core | 20.4 / 35 | 4.8 / 204 | 3.7 | ~47 KB |
| wterm, Ghostty core | 18.4 / 33 | 4.2 / 909 | 3.2 | ~236 KB |
| ghostty-web | 25.2 / 25 | 5.0 / 69, most keys never echoed | did not finish | ~187 KB |

- **wterm** (vercel-labs/wterm, Apache-2.0, very active, labelled a Vercel Labs experiment): native DOM selection, IME, Kitty keyboard protocol, Kitty images with the Ghostty core. Its Ghostty core falls behind on parsing under flood; its lightweight core does not, and was the fastest renderer in headless Chromium. On Linux its Ctrl+V sends ^V, so Quark needs its own paste binding, and Shift+Enter sends a Kitty sequence outside Kitty mode.
- **ghostty-web** (coder/ghostty-web, MIT, no commits since June 2026): stalls in the desktop app when all four panes flood, keeps painting hidden terminals, sends a Kitty sequence for Ctrl+I outside Kitty mode, and renders blank under a strict CSP unless `data:` is allowed. Not recommended.
- Chromium and WebKitGTK disagree sharply about wterm's DOM rendering, so the choice between xterm.js and wterm should be re-measured on macOS (WKWebView), which is where Quark will mostly run.

## What is in this directory

| Path | What |
| --- | --- |
| `CONTRACT.md` | The stub daemon API both apps use (subset of the spec's `/v1`, plus POC-only endpoints) |
| `stub-daemon/` | Rust stub of that API, with four live tmux panes in control mode |
| `native/` | warpui app: five screens, alacritty_terminal and libghostty-vt backends, wasm web build |
| `tauri/` | Tauri v2 + React + xterm.js app: five screens, desktop and browser |
| `term-bench/` | libghostty-vt vs alacritty_terminal benchmark |
| `h2h/` | Head-to-head bench script, raw results and summary |

Each subdirectory has a README with run instructions and a RESULTS.md with its own numbers.

## Things the real daemon should take from this

- Input POSTs from one client can arrive out of order; clients keep one input request in flight per worker, or the API should accept a sequence number.
- tmux 3.4 crashes on a global `window-size manual` and on `respawn-pane -k` against a flooding pane.
- Panes need a UTF-8 locale or non-ASCII input breaks.
- Two clients resizing the same tmux window fight each other; the daemon needs a policy for which client's size wins.
