# herdr vs tmux control mode as quarkd's session layer

Date: 2026-10-02. herdr v0.9.3 (official release binary, protocol 22, endpoint generation 1), tmux 3.4. Host: shared 4-vCPU KVM guest, Linux 6.18. Raw output: [results/](results/). Reproduce: [README.md](README.md).

## Recommendation: tmux now, herdr later only if upstream adds a lossless stream

Keep tmux control mode for quarkd's worker terminals, and look at herdr again if it ships a documented per-pane output stream, raw or lossless.

1. **herdr's public interfaces give quarkd screen frames, not the byte stream.**
   The documented live stream (`herdr terminal session observe|control`) sends re-rendered ANSI screen diffs, at most about 60 a second.
   Under a 33k lines/s flood, 0.24 MiB/s of frames reached quarkd against 4.8 MiB/s of real output, so about 95% of lines never arrive.
   quarkd therefore cannot run its own libghostty-vt per pane for transcripts and search, which is the current plan.
   History is only available through `pane.read`, hard-capped at 1,000 lines per call with no paging (`src/app/api_helpers.rs:117`).
2. **Input-to-echo latency is about 10x worse.**
   herdr median is 13.5 ms through a controller and 7.7–9.2 ms through the JSON API; tmux is 1.0 ms including the stub's HTTP and WebSocket hops.
   The cause is herdr's 16 ms minimum render interval (`MIN_RENDER_INTERVAL`, `src/app/mod.rs:37`).
3. **The usable interfaces are a CLI subprocess or an internal protocol.**
   The frame stream exists only as a CLI bridge, one `herdr` child per pane, speaking herdr's private binary protocol, which must match the server version exactly (it went from version 5 to 22 in six months).
   The endpoint protocol herdr's own UI uses has no published spec, its types are `pub(crate)`, and it describes a whole herdr UI session.
4. **What herdr adds over tmux, quarkd can mostly do without.**
   Server-held terminal state, a size-ownership model and agent state detection.
   Process survival is the same as tmux: survives client and daemon crashes, not a server restart.
   Its agent detection rules are Apache-2.0 TOML files (`distribution/agent-detection/*.toml`) quarkd could reuse without running herdr.

Worth asking upstream for a raw-output subscription in the API, or a documented per-pane surface stream with a library crate; either would make herdr a strong option.

## Capability table

| Need | tmux control mode (stub daemon) | herdr (public API + CLI bridge) |
|---|---|---|
| Create panes running a command | `new-window ... <cmd>` | `workspace.create` then `layout.apply` with an argv `command`; `pane.split` has no command field |
| Live per-pane stream | `%output`: raw PTY bytes, lossless, one client for all panes | `terminal.frame` NDJSON: re-rendered ANSI diffs at ≤60 fps, intermediate output merged away; one CLI child per pane; JSON API has no byte stream |
| Screen on (re)attach | None pushed; `capture-pane -p -e` (36 rows in 7 ms, 1,869 history rows in 9 ms) | First frame is a full screen (8–98 ms, 4–13 KB); `pane.read` also works |
| Scrollback / transcript | `capture-pane -S -` returns all history; raw bytes allow a complete transcript | `pane.read recent` ≤1,000 lines, no paging (999 of 8,824 rows readable); frames carry no scrollback |
| Input | `send-keys -H` raw bytes | Controller `terminal.input` raw; `pane.send_input` text is a paste, so `\r` does not submit at a bash prompt |
| Resize | `resize-window` per window | Controller `terminal.resize` sets and locks the exact PTY size; `pane.resize` only nudges a split ratio |
| Two viewers, different sizes | One size per window | One controller owns input and size; other viewers are read-only and see a crop, not a reflow; a second controller needs `--takeover` |
| Pane and agent state | None | `pane.agent_status_changed`, `pane.agent_detected`, `pane.exited`, `pane.process_info`; detection rules update from herdr.dev by default |
| Survives client or daemon crash | Yes | Yes |
| Survives server restart | No | No: layout returns but panes are bare `/bin/bash` at 80x24 |
| API transport | Commands on the control client's stdin | NDJSON over a Unix socket, one request per connection except `events.subscribe`; event history not durable |
| Windows | No tmux | Server runs; frame bridge only on Linux and macOS |

## Measurements

Four panes at 120x36 running the stub's scripts; ranges cover separate runs.

| Metric | tmux via stub (HTTP + WS) | herdr controller | herdr JSON API input |
|---|---|---|---|
| Echo latency idle, p50 / p95 / p99 | 0.93–1.04 / 1.8–2.2 / 2.9–3.3 ms | 13.5 / 17.6–21.3 / 21–26 ms | 7.7–9.2 / 18–22 / 22–27 ms |
| Echo latency while another pane floods | 3.6–4.4 / 12.1–12.2 / 14.7–15.7 ms | 14.3–14.7 / 19–24 / 25–29 ms | n/a |
| Flood stream to the daemon | 4.8 MiB/s, nothing lost | 0.24 MiB/s, 58 frames/s, lines dropped by design | |
| Server CPU during flood (one core = 100%) | tmux 83–84% + stub 22–25% | herdr 81–87% | |
| Steady CPU, 4 normal panes | tmux 0.5% + stub 0.5% | herdr 12.6–13.4% with 4 controllers; 1.1% with none | |
| Resize to first resized output | 5.1–5.4 ms | 4.3–17.7 ms | |
| Reattach | Stub replay: 85 missed events in 11 ms; 6 MiB in 64 ms | Full screen in first frame, 26–97 ms per pane | |
| Memory (RSS) | tmux 4.5–19 MiB; stub 5–19 MiB | herdr 21–25 MiB | |

With tmux, quarkd must absorb 4.8 MiB/s from one flooding pane; at libghostty-vt's ~55 MB/s on build logs (`term-bench/RESULTS.md`) that is roughly 9% of a core.

## Build and packaging

- Official v0.9.3 binaries: 30 MB static linux-x86_64 (25 MB stripped), macOS x86_64/aarch64, linux aarch64, Windows zip; SHA-256 published in `distribution/latest.json` and verified.
- quarkd would bundle one pinned binary per platform and run a private server; CLI bridge and server must be the same build, so each herdr bump means a quarkd release (59 releases since 2026-03-27).
- Source build failed here: herdr's vendored Ghostty (`44f2a44d`, 2 local patches, Zig 0.16) now needs the `translate_c` Zig package, hosted only on codeberg.org, which this network blocks; a pipeline would need to vendor it.
- herdr's Ghostty pin differs from the one Quark would use through libghostty-rs (`22d13172`).

## Risks

- Single vendor: one author wrote 1,131 of 1,792 commits; about 6 months old, pre-1.0.
- API churn: schema changed in 35 commits since v0.6.0; breaking changes in 0.7.5, 0.9.0 and 0.9.2; binary protocol 5 → 22.
- License: Apache-2.0 since 2026-07-22 (AGPL-3.0 or commercial before); firstmate's `docs/herdr-backend.md` still says AGPL or commercial.
- Strategic overlap: herdr is building a multi-machine UI with agent views, so it may drift towards Quark's product.
- Calls home by default for updates and detection rules; quarkd would turn both off.
- tmux has its own risks: two tmux 3.4 crashes found by the stub, no screen on attach, quarkd absorbs floods itself, and macOS has no tmux by default, so quarkd would bundle it (ISC; needs libevent and ncurses).
