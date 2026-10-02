# Terminal core comparison: libghostty-vt vs alacritty_terminal

Date: 2026-10-01.
Harness: this crate (`quark-term-bench`); the run instructions are in [README.md](README.md).
The raw output of the run quoted below is in [results/2026-10-01-full-run.md](results/2026-10-01-full-run.md).
An earlier full run gave the same picture.
Between the two runs the ingest ratios moved by less than 10%, and the frame-time means by up to about 25%, because of host load.

## TL;DR

- **Ingest speed is a wash in practice.**
  libghostty-vt is about 2x faster on full-screen TUI redraws.
  It is 0.7-0.9x on scrolling logs and ASCII, and about 0.4x on emoji- and grapheme-heavy text.
  Both cores ingest 20-260 MB/s, far above what agents and builds write to a PTY.
- **libghostty-vt uses 2.6x less memory for scrollback.**
  With its scrollback compression API it uses about 11x less: 4 MiB vs 47 MiB for a 200x50 pane with 10k lines.
  Quark runs many worker panes, so this matters.
- **Reading a full frame costs about 3x more with libghostty-vt.**
  At 200x50 that is 0.33-0.59 ms vs 0.10-0.16 ms.
  The cause is one FFI call per cell attribute.
  Damage-driven redraws are a few µs on both cores, and both fit easily in a 120 Hz frame.
- **libghostty-vt ships much more terminal functionality.**
  It includes key and mouse encoders (with the Kitty keyboard protocol), Kitty graphics, grapheme clustering (mode 2027), state snapshots, and OSC 133, OSC 52 and notification callbacks.
  With alacritty_terminal the embedder writes the key and mouse encoders, and there are no graphics at all.
- **libghostty-vt costs much more to build.**
  It needs Zig 0.16, a Ghostty source checkout, Zig package downloads from a CDN that was blocked here, and a 3-minute cold build.
  It adds 2 MB to the binary, its API is pre-1.0 and changes between releases, and its types are `!Send`.
  Neither core builds for wasm32 through Rust today.
- **Recommendation: use libghostty-vt as Quark's terminal core.**
  Pin it to a git revision, vendor its Zig packages, and hide it behind a small Quark trait so alacritty_terminal stays a drop-in fallback.
  The details are in the [Recommendation](#recommendation) section.

## Environment

| Item | Value |
|---|---|
| CPU | Intel Xeon @ 2.10 GHz (family 6, model 207), 4 vCPU, KVM guest (Firecracker kernel `6.18.44-fc-v51`), 16 GB RAM |
| Host load | Shared with other agents' builds and browsers: loadavg 8-13 during the runs. Wall-clock numbers and p99s are noisy, and the `cpu` columns are the more reliable figures. |
| Rust | rustc / cargo 1.97.0, release profile with `lto = "thin"` and `codegen-units = 1` |
| Zig | 0.16.0 from PyPI `ziglang==0.16.0`, run through a `zig` wrapper script |
| libghostty-rs | git `Uzaaft/libghostty-rs` @ `8953a740bc378cec3e07e1f6ca949f0595eab19b` (crates `libghostty-vt` / `libghostty-vt-sys`; the workspace version says 0.2.1) |
| Ghostty | `ghostty-org/ghostty` @ `22d13172cde98a0a4dda05d3d6a3fcb0dd8ed018` (2026-08-06), pinned by the libghostty-vt-sys build script; built `ReleaseFast`, `-Dcpu=baseline` |
| alacritty_terminal | 0.26.0 (crates.io, `default-features = false`), vte 0.15.0 |
| Settings | 10,000 scrollback lines on both cores (libghostty-vt has its byte limit off and its line limit at 10k); input fed in 64 KiB chunks |

## Results

### Ingest throughput (MB/s, median of 5 interleaved runs)

`cpu` is bytes divided by the feeding thread's on-CPU time.
`sys` is the share of that CPU time spent in the kernel.

| Workload | Geometry | libghostty-vt (cpu) | ghostty sys | alacritty_terminal (cpu) | alacritty sys | ghostty / alacritty |
|---|---|---|---|---|---|---|
| build log (52 MB, SGR 16/256/truecolor) | 80x24 | 62 | 37% | 71 | 0% | 0.87x |
| build log | 200x50 | 55 | 48% | 75 | 3% | 0.73x |
| TUI redraw (21 MB, alt screen, CUP, DECSTBM) | 80x24 | **260** | 0% | 125 | 0% | **2.08x** |
| TUI redraw | 200x50 | **226** | 0% | 111 | 0% | **2.04x** |
| Unicode (21 MB: CJK, ZWJ, flags, combining) | 80x24 | 22 | 26% | **51** | 2% | 0.43x |
| Unicode | 200x50 | 20 | 31% | **48** | 0% | 0.41x |
| ASCII cat (53 MB) | 80x24 | 72 | 65% | 76 | 0% | 0.94x |
| ASCII cat | 200x50 | 49 | 73% | 72 | 1% | 0.68x |

Wall-clock MB/s ran 5-35% below the `cpu` figures because of host load, and showed the same ordering.

What drives the numbers:

- **On scrolling workloads, libghostty-vt spends 37-73% of its CPU time in the kernel.**
  When a scrollback page is recycled, Ghostty releases its memory with `madvise(MADV_DONTNEED)` (`src/terminal/mem.zig`).
  The next write to that page then takes zero-fill page faults.
  `strace -c` shows the madvise calls, and the faults appear as system time.
  Page faults are unusually expensive in a Firecracker guest, so bare-metal Linux and macOS (which uses `MADV_FREE_REUSABLE`) are likely to look better for Ghostty.
  We did not measure that here.
  User-space time alone puts libghostty-vt ahead on ASCII.
- **The TUI stream does not scroll scrollback.**
  It is pure cursor movement, SGR and erase work, and there libghostty-vt's parser and printing path are 2x faster.
- **Unicode-heavy text hits page re-layout in libghostty-vt.**
  Callgrind shows 35% of instructions in `Page.clonePartialRowFrom`.
  Ghostty re-lays out a page when the page's grapheme or style storage fills up.
  Our Unicode workload is deliberately dense (every few cells is a multi-codepoint grapheme), so real output will hit this less.
- **`LIBGHOSTTY_VT_SYS_CPU=native` made no measurable difference** (within ±10% noise).
  Ghostty's SIMD paths (highway and simdutf) already select AVX2 at runtime even in a `baseline` build.

### Render frame cost (µs, mean over 1000 frames, median of 5 runs)

A full read visits every visible cell and reads its grapheme, resolved fg/bg RGB and attributes.
A damage-driven read visits only the rows the core reports as dirty.

| Workload | Geometry | Core | busy full (mean / p99) | idle full | idle damage-driven | 1 keystroke damage-driven (mean / p99) |
|---|---|---|---|---|---|---|
| build log | 80x24 | libghostty-vt | 140 / 1340 | 114 | 0.2 | 2.0 / 7.4 |
| build log | 80x24 | alacritty_terminal | 31 / 211 | 30 | 0.7 | 0.8 / 1.2 |
| build log | 200x50 | libghostty-vt | 589 / 4420 | 553 | 0.5 | 3.6 / 14.1 |
| build log | 200x50 | alacritty_terminal | 158 / 1582 | 157 | 1.6 | 1.6 / 2.8 |
| TUI redraw | 80x24 | libghostty-vt | 79 / 121 | 75 | 0.2 | 1.8 / 6.4 |
| TUI redraw | 80x24 | alacritty_terminal | 20 / 32 | 20 | 0.6 | 0.7 / 0.9 |
| TUI redraw | 200x50 | libghostty-vt | 359 / 554 | 353 | 0.2 | 3.7 / 13.0 |
| TUI redraw | 200x50 | alacritty_terminal | 104 / 155 | 105 | 1.6 | 1.6 / 2.8 |
| Unicode | 80x24 | libghostty-vt | 90 / 144 | 74 | 0.1 | 3.0 / 8.0 |
| Unicode | 80x24 | alacritty_terminal | 24 / 46 | 24 | 0.7 | 0.9 / 5.5 |
| Unicode | 200x50 | libghostty-vt | 448 / 1213 | 423 | 0.3 | 5.8 / 16.2 |
| Unicode | 200x50 | alacritty_terminal | 131 / 592 | 130 | 1.7 | 1.7 / 3.1 |
| ASCII cat | 80x24 | libghostty-vt | 74 / 134 | 71 | 0.1 | 1.9 / 5.3 |
| ASCII cat | 80x24 | alacritty_terminal | 20 / 43 | 20 | 0.6 | 0.7 / 1.1 |
| ASCII cat | 200x50 | libghostty-vt | 330 / 451 | 324 | 0.2 | 3.9 / 11.3 |
| ASCII cat | 200x50 | alacritty_terminal | 108 / 206 | 105 | 1.5 | 1.5 / 2.3 |

The p99 values in the millisecond range are host-contention spikes that hit both cores; they varied a lot between runs.
The means were stable.

To find where libghostty-vt's full-read cost goes, a throwaway variant of the harness read only some attributes at 200x50 (ASCII).
These figures are noisy:

- Iterating cells and reading only graphemes took about 210 µs, which already roughly equals alacritty_terminal's whole read.
- Adding the resolved fg/bg getters raised it to about 350 µs.
- Adding `style()` as well raised it to about 430 µs.

The core itself is not slow; the cost is the C API's cell-at-a-time getter calls (about 5 per cell).
A real renderer can cut the cost in three ways:

- Read `raw_cell()` once per cell.
- Cache resolved styles by style id.
- Resolve palette colours in Rust.

Above all, it can rely on the dirty rows.

### Resize reflow (ms per resize, with ~10k lines of build-log scrollback)

| From → to | Core | Narrow | Widen back |
|---|---|---|---|
| 80x24 → 48x19 | libghostty-vt | 4.0 | 4.2 |
| 80x24 → 48x19 | alacritty_terminal | 3.5 | 3.0 |
| 200x50 → 120x40 | libghostty-vt | 8.6 | 8.3 |
| 200x50 → 120x40 | alacritty_terminal | 7.4 | 7.6 |

The two cores are close; libghostty-vt is 10-40% slower.
Both reflow the full history in one call (8-9 ms at 200x50 with 10k lines).
Quark should debounce resize during window drags with either core.

### Memory (RSS delta in MiB, fresh process per sample, 10k scrollback lines filled)

| Scenario | Geometry | libghostty-vt | alacritty_terminal |
|---|---|---|---|
| empty terminal | 80x24 | 1.1 | 0.1 |
| empty terminal | 200x50 | 1.3 | 0.5 |
| ASCII cat, scrollback full | 80x24 | **7.6** | 18.9 |
| ASCII cat, scrollback full | 200x50 | **17.3** | 46.7 |
| build log, scrollback full | 80x24 | **7.8** | 18.9 |
| build log, scrollback full | 200x50 | **17.7** | 46.8 |
| build log + `Terminal::compress(Full)` | 80x24 | **3.0** (compress took 8.5 ms) | n/a (no equivalent) |
| build log + `Terminal::compress(Full)` | 200x50 | **4.1** (compress took 15 ms) | n/a |

alacritty_terminal stores a 24-byte `Cell` for every column of every row.
libghostty-vt uses compact cells, deduplicated styles and page-based storage.
Its compression is designed to run incrementally from an idle callback (`CompressionMode::Incremental`), and reading compressed history decompresses it transparently.
Twenty worker panes at 200x50 with full scrollback would take about 940 MiB with alacritty_terminal, 350 MiB with libghostty-vt, and about 80 MiB with libghostty-vt plus compression.

libghostty-vt kept slightly fewer history lines than the 10k configured: 9,584-9,985, because it prunes whole pages.
Its own docs say the retained count is "almost always higher" than configured; here it was lower.
alacritty_terminal kept exactly 10,000.

## Correctness

The same input went into both cores, and `check` compared visible cells and screens.

**The cores agree on:**

- CJK wide characters: 2 cells plus a spacer tail.
- Combining marks: stored in one cell.
- A wide character that wraps at the right margin: both leave a spacer-head cell.
- Skin-tone and VS15 sequences.
- The cursor position and alt-screen state after the 2 MB TUI stream: `(6,2)` and `alt=true` at both sizes.
- Restoring the primary screen on `?1049l`.
- Every visible row after 4 MB of build log, TUI or ASCII input, at both geometries and after a reflowing resize (200x50 → 120x40).

**The cores disagree on:**

| Input | libghostty-vt | alacritty_terminal |
|---|---|---|
| Regional-indicator flag 🇯🇵, default mode | each indicator is wide (2+2 cells, cursor at 4) | each indicator is narrow (1+1 cells, cursor at 2) |
| ZWJ family 👨‍👩‍👧‍👦, after `CSI ? 2027 h` | one wide cell holding the whole cluster | ignores mode 2027: 4 wide cells, with the ZWJ attached to the previous cell |
| Flag 🇯🇵, after `?2027h` | one wide cell holding both indicators | 2 narrow cells |
| ❤️ (VS16), after `?2027h` | wide (2 cells) | narrow (1 cell) |

Without mode 2027 the two cores behave the same for ZWJ sequences: both split the cluster into per-emoji cells.
Only libghostty-vt supports grapheme clustering (DEC mode 2027, the Contour/Ghostty/WezTerm convention).
Modern TUIs and shells enable that mode, and without it emoji widths drift from what applications expect.
The visible-screen diff for the Unicode workload differs only on rows containing flags, where widths diverge and wrapping follows.

**Protocol probes:**

| Probe | libghostty-vt | alacritty_terminal |
|---|---|---|
| Kitty keyboard flags after `CSI > 3 u` | tracked: `DISAMBIGUATE \| REPORT_EVENTS` | tracked: `DISAMBIGUATE_ESC_CODES \| REPORT_EVENT_TYPES` (needs `Config::kitty_keyboard = true`) |
| Key encoding | `key::Encoder`: Ctrl+Shift+A press/release → `\x1b[97;6u` / `\x1b[97;6:3u`; legacy Up / Ctrl+Up → `\x1b[A` / `\x1b[1;5A`; also supports modifyOtherKeys, cursor and keypad application modes, macOS option-as-alt, and `set_options_from_terminal` | none; the encoder lives in the Alacritty application (`alacritty/src/input/keyboard.rs`, Apache-2.0), so the embedder ports or writes one |
| Mouse encoding | `mouse::Encoder`: X10, UTF-8, SGR, urxvt and SGR-pixels; a left press at cell (10,5) → `\x1b[<0;11;6M` | exposes the mode bits only (`MOUSE_REPORT_CLICK`, `SGR_MOUSE`, …); the embedder encodes |
| Kitty graphics | yes: `a=T` 1x1 RGB stored as image id 7; image storage, placements, layers and PNG decode (feature `png`) | no: the APC sequence is ignored |
| Sixel | no | no |
| Other | focus-event encoding, paste-safety check, OSC parser, selection with click/drag gestures, formatting to plain/VT/HTML, binary state snapshot/restore, OSC 133 semantic prompts, hyperlinks, callbacks for PTY replies, title, pwd, OSC 52 clipboard, notifications and progress | selection, regex search, vi mode, hyperlinks, events for PTY replies, title, OSC 52 clipboard and bell; ships a PTY spawner and an I/O event loop (`tty`, `event_loop`) |

## Build friction

Every hurdle hit while building libghostty-vt here, in order:

1. **No Zig on the machine, and ziglang.org is blocked.**
   `pip install ziglang==0.16.0` (into a venv) plus a two-line `zig` wrapper script around `python3 -m ziglang` worked first time.
   PyPI also carries 0.15.2.
2. **The crates.io release doesn't match the brief's toolchain.**
   crates.io `libghostty-vt` 0.2.1 (and 0.2.2) pin Ghostty `a887df42` (2026-07-11), which requires **Zig 0.15.2**, not 0.16.
   Only libghostty-rs master pins a Zig-0.16 Ghostty (`22d13172`).
   Master's workspace version still says 0.2.1, but its API differs from the crates.io 0.2.1:
   - `Terminal::new(cols, rows)` replaces `Terminal::new(TerminalOptions { .. })`.
   - It has line-based scrollback limits.
   - It adds a snapshot module.

   We benchmarked master at a pinned revision.
3. **The crates.io 0.2.1 scrollback option is mislabelled.**
   Its `TerminalOptions::max_scrollback` is documented as "lines" but is passed through to Ghostty's `PageList` as a **byte** budget.
   An embedder following the docs would get far less scrollback than intended.
   Master exposes both `set_scrollback_max_bytes` and `set_scrollback_max_lines`.
4. **Semver lets the `-sys` crate drift.**
   `libghostty-vt = "=0.2.1"` still resolves `libghostty-vt-sys` to 0.2.2, because of the `^0.2.1` requirement.
   Pin both crates if you use crates.io.
5. **The Ghostty source fetch worked.**
   The build script runs `git clone --filter=blob:none` of github.com/ghostty-org/ghostty into `OUT_DIR`, which succeeded through the proxy in about 4 s.
   `GHOSTTY_SOURCE_DIR` lets you point at a local checkout instead.
6. **Zig package downloads were blocked.**
   `deps.files.ghostty.org` returns 403/405 through the egress proxy.
   `zig fetch git+https://…` also failed: Zig's own HTTP client reported "unable to discover remote git server capabilities: ReadFailed" through the proxy.
   The fix was to fetch each package with system `git` or `curl` from GitHub and seed Zig's global cache with `zig fetch <dir-or-tarball>`; the steps are in the README.
   All hashes matched.
   With Ghostty `22d13172`, the libghostty-vt build needs exactly four packages, two of them lazy dependencies that the build graph still resolves:

   | Package | Source |
   |---|---|
   | `uucode` | jacobsandlund/uucode @ `2826a37` |
   | `highway` | google/highway @ `66486a1` |
   | `zlib` | madler/zlib `v1.3.1` |
   | `iterm2_themes` | iTerm2-Color-Schemes release `release-20260720-153658-97e244c` asset `ghostty-themes.tgz` |

   After seeding, the build script's own path (clone, then `zig build`) works unmodified.
   `GHOSTTY_ZIG_SYSTEM_DIR` exists for package-manager-style offline builds, but we didn't need it.
7. **The crates.io Ghostty pin needs many more packages.**
   Building the crates.io 0.2.1 release (Ghostty `a887df42`, Zig 0.15.2) to compare, its build graph pulled in about 10 more packages even for lib-vt only.
   They included libxev, vaxis, z2d, zf, JetBrainsMono, NerdFonts, glslang, spirv-cross and wuffs, all from the blocked CDN.
   We stopped there: that path is much worse behind a proxy, and newer Ghostty trimmed the lib-vt dependency graph.
   (For the uucode dependency we did find the matching commit for that pin, `54d650cf`.)
8. **Build time.**
   A cold Ghostty build takes about 3 minutes on 4 vCPU (2m55s for this crate's first `cargo build --release`; 2m24s for a minimal binary).
   alacritty_terminal builds cold in 34 s.
   The Zig cache lives in `OUT_DIR`, so every new target dir, profile or target triple rebuilds Ghostty from scratch.
9. **Disk.**
   Each build directory uses about 430 MB (Ghostty source plus Zig cache), plus 150 MB of global Zig package cache.
10. **Binary size** (stripped, thin LTO):
    - A minimal "feed + render" binary is 0.33 MB with no terminal core, 0.51 MB with alacritty_terminal (+0.18 MB), and **2.40 MB with libghostty-vt (+2.07 MB)**.
    - The added size is mostly Unicode tables, simdutf and highway.
    - `libghostty-vt.a` itself is 19.5 MB before linking.
11. **API bug: `key::Encoder::encode_to_vec` returns `OutOfSpace { required: 9 }` when started from an empty `Vec`.**
    Its grow-and-retry path fails, while `encode()` into a 9-byte buffer succeeds with 7 bytes.
    The probe code uses fixed buffers to work around it.
12. **Licences are compatible with MIT Quark.**
    Ghostty is MIT, libghostty-rs is MIT OR Apache-2.0, and alacritty_terminal is Apache-2.0.

For alacritty_terminal the only build friction was the `cargo add`.

### wasm32 (time-boxed, about 20 minutes)

- **Through the Rust crate: it fails.**
  `cargo build --target wasm32-unknown-unknown -p libghostty-vt` panics in the `-sys` build script with "unsupported Rust target for vendored build".
  The target-triple map has no wasm entry.
  We then patched the build script in a scratch copy in two ways: we mapped `wasm32-unknown-unknown` to Zig's `wasm32-freestanding`, and we copied headers, because the wasm install step installs none.
  With those patches Zig *did* produce a wasm32 static `libghostty-vt.a`.
  The Rust side then failed on the checked-in bindgen bindings, which are generated for 64-bit layouts: 29 struct size and offset assertions fail on wasm32.
  Making it work would need per-target bindings (bindgen at build time, or a second checked-in set) plus the build-script fixes.
  Upstream does not support this today.
- **Ghostty's own wasm module builds.**
  `zig build -Demit-lib-vt=true -Dtarget=wasm32-freestanding -Doptimize=ReleaseSmall` produced a standalone **`ghostty-vt.wasm` of 832 KB** in about 50 s.
  It exports the C ABI and a growable function table for JS callbacks.
  JS in a Tauri webview could load it directly, without Rust in between.
- **alacritty_terminal does not build for wasm32 either.**
  Its unconditional `polling` dependency, pulled in by the PTY and event-loop modules, errors with "polling does not support this target OS", and no feature turns it off.

## API ergonomics

### libghostty-vt

**Driving a renderer.**
Create a `RenderState` once, then call `update(&terminal)` each frame to get a snapshot.
From the snapshot you read:

- the dirty level (`Clean` / `Partial` / `Full`)
- colours (background, foreground, the 256-colour palette) and the cursor (position, style, visibility, blink)
- rows, through a lending `RowIterator`, each with a `dirty()` flag
- cells, through a lending `CellIterator`, each with `graphemes_*`, `fg_color` / `bg_color` already resolved to RGB, `style()` and `raw_cell()`

The renderer clears row-level and global dirty flags itself, and the two layers are independent, which is easy to get wrong.
The lending iterators and `Result` on every getter make the code verbose (see `src/cores.rs`), but it is straightforward.

**Splitting the update.**
`begin_update(&terminal)` / `end()` split the update so that only the copy step needs the terminal.
A renderer can hold the terminal lock only for `begin_update`.

**Thread safety.**
Every libghostty-vt type is `!Send + !Sync`: the C API makes no thread-safety promises, so the bindings are conservative.
The `Terminal`, its `RenderState` and the encoders must therefore live on one thread.
The intended pattern is a dedicated terminal thread fed by channels.
To render on a UI thread, that terminal thread has to copy the dirty rows into a Quark-owned, `Send` frame structure.
That is extra code, but it also gives a clean seam between the daemon or PTY side and the UI.

**Dirty-region tracking.**
Tracking is two-level: global plus per row, with no column ranges.
Idle frames cost 0.1-0.5 µs, and a keystroke redraws one row in 2-6 µs.

**Extras.**
These are relevant to Quark's architecture:

- Binary **state snapshots** (`encode_snapshot` / `snapshot::Decoder`, with incremental history streaming).
  These let a daemon-side terminal hand its state to a freshly attached UI.
- **Scrollback compression.**
- **Tracked grid references** for selections that survive scrolling.
- Callbacks for PTY replies (DA, DSR, XTVERSION, size reports), title and pwd changes, clipboard, notifications and progress.

**Maturity.**
The API is pre-1.0 and changes across releases: master already broke `Terminal::new` compared with 0.2.1.
There is one maintainer, and Ghostty upstream owns the C API.

### alacritty_terminal

**Driving a renderer.**
Call `term.renderable_content()` to get a `display_iter` over `Indexed<&Cell>`.
Each cell exposes:

- `c` and `zerowidth()` for combining characters
- `fg` and `bg` as `Color::{Named, Indexed, Spec}`, which the embedder resolves against `term.colors()` and its own default palette (alacritty_terminal leaves palette entries unset)
- `flags`

Plain Rust structs make this the fastest and simplest full read.

**Thread safety.**
`Term<T>` is `Send` when `T: Send`.
The usual pattern is `Arc<FairMutex<Term>>`, shared between the bundled PTY `EventLoop` thread and the renderer.
The renderer holds the lock while it iterates, or copies the cells out under the lock, which is what Alacritty does.

**Dirty-region tracking.**
`term.damage()` returns `Full` or per-line `LineDamageBounds` with left and right column bounds, which is finer than libghostty-vt's per-row flags.
Call `reset_damage()` after drawing.
Idle frames cost about 1.5 µs and a keystroke about 1.6 µs.

**What's missing.**
There is no key or mouse encoder, no graphics, no grapheme clustering (mode 2027), and no state serialization.
It also wraps the PTY and event loop into the core crate, which blocks wasm and is extra weight if Quark's daemon owns the PTY.

**Maturity.**
It is stable and widely embedded (Zed, Lapce and others) and builds as pure Rust, but its API is tuned for Alacritty's own needs.

## Recommendation

**Adopt libghostty-vt as Quark's terminal core, with guard rails.**

Why:

1. **Memory.**
   Quark shows many agent and worker panes at once, and scrollback memory grows with pane count.
   libghostty-vt uses 2.6x less memory, about 11x less with idle-time compression.
2. **Correctness and protocol coverage.**
   It has the features a modern terminal is judged on: grapheme clustering, the Kitty keyboard protocol with a real encoder, mouse encoding and Kitty graphics.
   With alacritty_terminal, Quark would have to port or write the key and mouse encoders and would still have no graphics or mode 2027.
3. **Performance is not a deciding factor.**
   Where libghostty-vt trails (scrolling ingest in this VM, Unicode-dense text, full-frame reads), both cores are orders of magnitude above real PTY rates and well inside a frame budget.
   The full-read gap goes away with damage-driven rendering, which Quark should do anyway.
4. **State snapshots map onto Quark's daemon/UI split.**
   The terminal can live next to the PTY in the daemon, and a UI can attach by receiving a snapshot and then the live stream.

Guard rails:

- **Pin libghostty-rs to a git revision.**
  Do not use crates.io `0.2.x`: those releases pin an old Ghostty, need Zig 0.15, and carry the scrollback-units bug.
- **Install Zig 0.16 in CI** (the PyPI `ziglang` package works on every platform).
- **Vendor the four Zig packages**, through `GHOSTTY_ZIG_SYSTEM_DIR` or by seeding the cache, so builds don't depend on `deps.files.ghostty.org`.
- **Cache the Ghostty build** across CI jobs to avoid the 3-minute rebuild.
- **Put the core behind a small Quark trait**, like `Core` in `src/cores.rs`, so alacritty_terminal remains a cheap fallback if libghostty's churn, `!Send` constraints or Zig toolchain become a problem.
  This harness already implements both cores behind that trait.
- **Renderer:**
  - Run the terminal on its own thread and copy only the dirty rows into a `Send` frame.
  - Cache resolved styles by style id.
  - Run `compress(Incremental)` from an idle timer.
  - Debounce resize.
- **Tauri / web UI:** the Rust crate does not build for wasm32 today.
  If the webview needs a terminal core, use Ghostty's standalone `ghostty-vt.wasm` (832 KB) from JS, or keep the core native and stream frames to the webview.
  alacritty_terminal is no better here.

**Choose alacritty_terminal instead if** a Zig toolchain in the build, or a pre-1.0 C-backed dependency, is unacceptable for Quark.
It is pure Rust, quick to build, stable and faster to read per frame.
In exchange Quark would write the key and mouse encoders and accept higher memory use, no graphics and no grapheme clustering.

## Reproduce

```sh
cd ui-poc/term-bench
export PATH=~/tools/bin:$PATH     # zig 0.16.0 (see README for the PyPI wrapper and offline package seeding)
cargo build --release
./target/release/quark-term-bench all > results/$(date +%F)-full-run.md
```

A full run takes about 2 minutes on 4 vCPU.
`--runs`, `--scale` and `--frames` trade accuracy for time.
