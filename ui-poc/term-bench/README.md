# quark-term-bench

A small, standalone harness that compares two terminal-emulation cores for Quark's desktop app:

- **libghostty-vt**: Ghostty's terminal core (Zig), through the Rust bindings at [Uzaaft/libghostty-rs](https://github.com/Uzaaft/libghostty-rs).
- **alacritty_terminal**: Alacritty's terminal core (pure Rust, crates.io).

Both cores get the same deterministic input and are driven through the same small trait (`src/cores.rs`).
The results and the recommendation are in [RESULTS.md](RESULTS.md).

## What it measures

| Subcommand | What it does |
|---|---|
| `throughput` | Ingest MB/s for four workloads at 80x24 and 200x50, with 10k lines of scrollback. Input is fed in 64 KiB chunks, like a PTY read loop. |
| `frame` | Cost of one render frame. A full read visits every visible cell (grapheme, resolved fg/bg RGB, attributes). A damage-driven read uses the core's own dirty tracking. Reports mean and p99. |
| `resize` | Reflow cost of a resize with full scrollback (narrowing and widening back). |
| `memory` | RSS delta with 10k scrollback lines filled. Each sample runs in a fresh process. Also measures libghostty-vt's scrollback compression. |
| `check` | Correctness spot checks (wide chars, ZWJ emoji, combining marks, flags, wrapping, TUI cursor position), a full visible-screen diff between the cores, and probes for key, mouse and Kitty graphics support. |
| `all` | Everything above, as Markdown (the default). |

The four workloads (`src/workload.rs`) are a colourful build log (SGR 16/256/truecolor, long wrapped lines), a full-screen TUI redraw stream (alt screen, CUP, scroll regions, like `top` and `vim`), Unicode-heavy text (CJK, ZWJ emoji, flags, combining marks), and plain ASCII `cat` output.

## Toolchain

libghostty-vt's build script compiles Ghostty from source with Zig, so you need:

1. **Rust** 1.90 or newer (edition 2024).
2. **Zig 0.16.0** on `PATH`.
   If ziglang.org is unreachable, PyPI has it:

   ```sh
   python3 -m venv ~/tools/zigenv
   ~/tools/zigenv/bin/pip install ziglang==0.16.0
   mkdir -p ~/tools/bin
   printf '#!/bin/sh\nexec ~/tools/zigenv/bin/python3 -m ziglang "$@"\n' > ~/tools/bin/zig
   chmod +x ~/tools/bin/zig
   export PATH=~/tools/bin:$PATH
   ```

3. **Network access** for two downloads:
   - `git clone` of github.com/ghostty-org/ghostty, done by the build script.
   - Ghostty's Zig package dependencies, which Zig downloads from `deps.files.ghostty.org`.

   If `deps.files.ghostty.org` is blocked, fetch the four packages the libghostty-vt build needs from GitHub and put them in Zig's global cache yourself.
   Run this from any directory that holds a Ghostty checkout, because `zig fetch` needs a `build.zig` nearby:

   ```sh
   cd /path/to/ghostty            # any checkout, e.g. the pinned commit 22d13172
   D=$(mktemp -d)
   git clone -q https://github.com/jacobsandlund/uucode "$D/uucode" && git -C "$D/uucode" checkout -q 2826a37a4562284fdacd8fa029d49509cc9bffcd && rm -rf "$D/uucode/.git"
   git clone -q https://github.com/google/highway "$D/highway" && git -C "$D/highway" checkout -q 66486a10623fa0d72fe91260f96c892e41aceb06 && rm -rf "$D/highway/.git"
   git clone -q --depth 1 --branch v1.3.1 https://github.com/madler/zlib "$D/zlib" && rm -rf "$D/zlib/.git"
   curl -sSL -o "$D/themes.tgz" https://github.com/mbadolato/iTerm2-Color-Schemes/releases/download/release-20260720-153658-97e244c/ghostty-themes.tgz
   for p in uucode highway zlib themes.tgz; do zig fetch "$D/$p"; done
   ```

   Each `zig fetch` prints a package hash.
   It must match the hash in Ghostty's `build.zig.zon` (or `pkg/*/build.zig.zon`), which it did for all four.

## Run

```sh
export PATH=~/tools/bin:$PATH          # zig
cargo build --release                  # first build ~3 min (Ghostty compiles from source)
./target/release/quark-term-bench all > results.md
```

Useful flags: `--runs N` (default 5), `--scale F` (shrinks or grows workload sizes, default 1.0), and `--frames N` (default 1000).
For example, `--runs 1 --scale 0.1` gives a run of about 10 seconds.

To profile one core on one workload:

```sh
./target/release/quark-term-bench ingest-one ghostty unicode 200 50 --runs 3
valgrind --tool=callgrind ./target/release/quark-term-bench ingest-one ghostty unicode 200 50 --runs 1 --scale 0.05
```

`cargo test --release` checks that the workload generators are deterministic and produce valid UTF-8.

## Notes

- The libghostty-vt dependency is pinned to a git revision of libghostty-rs, not to crates.io.
  RESULTS.md ("Build friction") explains why.
- `LIBGHOSTTY_VT_SYS_CPU=native` makes Zig build Ghostty for the host CPU.
  It made no measurable difference here, because Ghostty's SIMD paths already pick AVX2 at runtime.
- RSS figures assume Linux (`/proc/self/statm`).
  So do the CPU-time columns (`/proc/thread-self/{stat,schedstat}`).
