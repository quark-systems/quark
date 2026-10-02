## herdr arm

- herdr server pid Some(6662), RSS 25.0 MiB; workspace `quark-spike` **reused from an earlier run** (panes survived the spike process exiting)
- w-1 -> pane `w2:p2`
- w-2 -> pane `w2:p3`
- w-3 -> pane `w2:p4`
- w-4 -> pane `w2:p5`

### Attach (one `terminal session control` per pane, 120x36)

- w-1: first frame 7.9 ms (120x36, full=true, 5432 B)
- w-2: first frame 78.0 ms (120x36, full=true, 5144 B)
- w-3: first frame 46.8 ms (120x36, full=true, 12335 B)
- w-4: first frame 48.5 ms (120x36, full=true, 4730 B)

### Steady state (10 s, normal scripts)

| worker | frames/s | KiB/s | full frames |
|---|---|---|---|
| w-1 | 7.4 | 16.1 | 0 |
| w-2 | 1.0 | 1.6 | 0 |
| w-3 | 10.0 | 10.7 | 0 |
| w-4 | 0.0 | 0.0 | 0 |

herdr server CPU: 12.6% of one core

### Input-to-echo latency (w-4 bash, idle fleet)

- controller stdin (`terminal.input`) -> frame: n=200 p50 13.55 ms, p95 17.57, p99 21.35, max 24.43
- JSON API `pane.send_input` -> frame: n=100 p50 7.68 ms, p95 18.29, p99 22.47, max 22.81

### Flood (w-1 runs flood.sh for 10 s; echo latency measured on w-4 meanwhile)

- producer (flood.sh) rate, from the counter on screen: 32565 lines/s
- w-1 stream to quarkd: 57.5 frames/s, 0.24 MiB/s (4402 B/frame)
- w-4 echo latency during flood: n=200 p50 14.31 ms, p95 19.03, p99 25.10, max 43.11
- herdr server CPU during flood: 83.5% of one core

### Resize

- w-2 (top) controller resize 120x36 -> 100x30: first 100x30 frame after 17.7 ms
- w-4 resized to 90x25 via controller; `stty size` in the pane prints `25 90`: true
- `pane.get` viewport_rows after resize: 25

### Two viewers, different sizes (w-3: controller 120x36, observer 80x20)

- controller frame sizes: {(120, 36): 29}; observer frame sizes: {(80, 20): 5}
- observer received 5 frames in 3 s (controller 29); PTY size stays the controller's
- second controller without --takeover: closed: terminal attach failed: terminal term_65cda7ed9afbc7 already has an attached client; retry with --takeover
- second controller with --takeover: first frame 16.2 ms (100x30, full=true, 8764 B); original controller closed: Some("eof")

### Events

- `pane.output_matched` arrived 102.6 ms after typing: `{"data":{"matched_line":"worker@quark:~# echo spike-event-marker","pane_id":"w2:p5","read":{"format":"text","pane_id":"w2:p5","revision":0,"source":"recent_unwr…`

### quarkd crash and reattach

- pane shell pids before [8436, 8442, 8451, 8459], after [8436, 8442, 8451, 8459] (same = processes survived)
- w-1: re-attach first frame 26.3 ms (120x36, full=true, 5432 B)
- w-2: re-attach first frame 74.5 ms (120x36, full=true, 5188 B)
- w-3: re-attach first frame 79.4 ms (120x36, full=true, 12542 B)
- w-4: re-attach first frame 97.0 ms (120x36, full=true, 4782 B)
- `pane.read` recent history of w-4 still holds the pre-crash command: true
- `pane.read` visible screen as ANSI (snapshot for a fresh UI): 5403 bytes for w-3
- `pane.read` recent 5000 lines of w-1: 998 lines, 94 KiB in 8.8 ms
