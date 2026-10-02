## herdr arm

- herdr server pid Some(31008), RSS 21.3 MiB; workspace `quark-spike` created
- w-1 -> pane `w1:p2`
- w-2 -> pane `w1:p3`
- w-3 -> pane `w1:p4`
- w-4 -> pane `w1:p5`

### Attach (one `terminal session control` per pane, 120x36)

- w-1: first frame 24.0 ms (120x36, full=true, 4800 B)
- w-2: first frame 20.0 ms (120x36, full=true, 5144 B)
- w-3: first frame 17.6 ms (120x36, full=true, 5947 B)
- w-4: first frame 19.7 ms (120x36, full=true, 4678 B)

### Steady state (10 s, normal scripts)

| worker | frames/s | KiB/s | full frames |
|---|---|---|---|
| w-1 | 5.9 | 11.3 | 0 |
| w-2 | 1.0 | 1.7 | 0 |
| w-3 | 11.8 | 2.6 | 0 |
| w-4 | 0.0 | 0.0 | 0 |

herdr server CPU: 11.6% of one core

### Input-to-echo latency (w-4 bash, idle fleet)

- controller stdin (`terminal.input`) -> frame: n=200 p50 13.17 ms, p95 17.26, p99 22.22, max 45.59
- JSON API `pane.send_input` -> frame: n=100 p50 8.60 ms, p95 20.06, p99 21.87, max 24.85

### Flood (w-1 runs flood.sh for 10 s; echo latency measured on w-4 meanwhile)

- producer (flood.sh) rate, from the counter on screen: 35761 lines/s
- w-1 stream to quarkd: 57.9 frames/s, 0.24 MiB/s (4402 B/frame)
- w-4 echo latency during flood: n=200 p50 14.61 ms, p95 23.12, p99 25.37, max 28.86
- herdr server CPU during flood: 80.2% of one core

### Resize

- w-2 (top) controller resize 120x36 -> 100x30: first 100x30 frame after 4.5 ms
- w-4 resized to 90x25 via controller; `stty size` in the pane prints `25 90`: true
- `pane.get` viewport_rows after resize: 25

### Two viewers, different sizes (w-3: controller 120x36, observer 80x20)

- controller frame sizes: {(120, 36): 41}; observer frame sizes: {(80, 20): 6}
- observer received 6 frames in 3 s (controller 41); PTY size stays the controller's
- second controller without --takeover: closed: terminal attach failed: terminal term_65cdac53abf474 already has an attached client; retry with --takeover
- second controller with --takeover: first frame 17.7 ms (100x30, full=true, 9744 B); original controller closed: Some("eof")

### Events

- `pane.output_matched` arrived 103.5 ms after typing: `{"data":{"matched_line":"worker@quark:~# echo spike-event-marker","pane_id":"w1:p5","read":{"format":"text","pane_id":"w1:p5","revision":0,"source":"recent_unwr…`

### quarkd crash and reattach

- pane shell pids before [31024, 31032, 31039, 31050], after [31024, 31032, 31039, 31050] (same = processes survived)
- w-1: re-attach first frame 63.6 ms (120x36, full=true, 5432 B)
- w-2: re-attach first frame 80.3 ms (120x36, full=true, 5166 B)
- w-3: re-attach first frame 81.8 ms (120x36, full=true, 12834 B)
- w-4: re-attach first frame 35.9 ms (120x36, full=true, 4782 B)
- `pane.read` recent history of w-4 still holds the pre-crash command: true
- `pane.read` visible screen as ANSI (snapshot for a fresh UI): 5603 bytes for w-3
- `pane.read` recent 5000 lines of w-1: 999 lines, 93 KiB in 10.3 ms
