## tmux arm (stub daemon on port 7450, tmux control mode)

- workers: 4
- tmux server pid Some(433) RSS 19.5 MiB; stub daemon pid Some(32653) RSS 18.7 MiB
- WebSocket connect (live only): 0.9 ms

### Steady state (10 s, normal scripts)

| worker | events/s | KiB/s |
|---|---|---|
| w-1 | 4.7 | 0.2 |
| w-2 | 1.4 | 4.6 |
| w-3 | 11.8 | 5.8 |

tmux server CPU: 0.5% of one core

stub daemon CPU: 0.5% of one core

### Input-to-echo latency (w-4 bash, idle fleet)

- HTTP `POST /input` (tmux `send-keys -H`) -> WS `worker.output`: n=200 p50 0.93 ms, p95 1.83, p99 3.21, max 5.22

### Flood (w-1 stress for 10 s; echo latency measured on w-4 meanwhile)

- producer (flood.sh) rate, from counters in the delivered bytes: 34563 lines/s
- w-1 stream to client: 2689.3 events/s, 4.78 MiB/s
- w-4 echo latency during flood: n=260 p50 3.59 ms, p95 12.14, p99 14.68, max 21.71
- tmux server CPU during flood: 83.7% of one core
- stub daemon CPU during flood: 24.6% of one core

### Resize

- w-2 (top) resize -> first w-2 output after 5.1 ms

### Client reconnect with replay

- reconnect after 3 s with cursor: 85 missed `worker.output` events (38 KiB) replayed in 11.3 ms
- fresh client, cursor=0 (full retained replay): 7841 events, 6.0 MiB of `worker.output` in 63.8 ms
