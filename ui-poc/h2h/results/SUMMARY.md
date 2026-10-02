| App | Scenario | fps | frame p50 ms | key->echo painted p50 ms | p99 ms |
|---|---|---|---|---|---|
| native (warpui + alacritty) | 4 terminals, light output, typing | 20.8 | 42.8 | 69.7 | 108.8 |
| native (warpui + alacritty) | 4 panes flooding + chat streaming, typing | 10.9 | 90.5 | 168.9 | 234.5 |
| tauri (xterm.js webgl) | 4 terminals, light output, typing | 23.2 | 41 | 23 | 88 |
| tauri (xterm.js webgl) | 3 panes flooding + chat streaming, typing | 4.4 | 201 | 319 | 1132 |
| tauri (xterm.js webgl) | 4 panes flooding + chat streaming | 5.1 | 176 | – | – |
| tauri (xterm.js dom) | 4 terminals, light output, typing | 38.1 | 23 | 17 | 59 |
| tauri (xterm.js dom) | 3 panes flooding + chat streaming, typing | 8.9 | 111 | 103 | 283 |
| tauri (xterm.js dom) | 4 panes flooding + chat streaming | 8.6 | 113 | – | – |
