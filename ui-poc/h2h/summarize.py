#!/usr/bin/env python3
"""Summarize head-to-head bench JSON into a markdown table (median across rounds)."""
import glob, json, statistics as st, os
D = os.path.join(os.path.dirname(os.path.abspath(__file__)), "results")
def med(xs):
    xs = [x for x in xs if x is not None]
    return round(st.median(xs), 1) if xs else None
rows = []
nat = [json.load(open(f)) for f in sorted(glob.glob(f"{D}/native-*.json"))]
def nphase(d, name): return next(p for p in d["phases"] if p["phase"] == name)
for name, label in [("idle_typing", "4 terminals, light output, typing"), ("stress_4_terminals_plus_chat", "4 panes flooding + chat streaming, typing")]:
    ps = [nphase(d, name) for d in nat]
    rows.append(("native (warpui + alacritty)", label, med([p["fps_avg"] for p in ps]),
                 med([p["frame_interval_ms"]["p50"] for p in ps]),
                 med([p["key_to_echo_painted_ms"]["p50"] for p in ps]), med([p["key_to_echo_painted_ms"]["p99"] for p in ps])))
for rend in ["webgl", "dom"]:
    ts = [json.load(open(f))["phases"] for f in sorted(glob.glob(f"{D}/tauri-{rend}-*.json"))]
    for name, label in [("idle_terminals", "4 terminals, light output, typing"), ("stress3_chat_typing", "3 panes flooding + chat streaming, typing"), ("stress4_chat_terminals_visible", "4 panes flooding + chat streaming")]:
        ps = [t[name] for t in ts]
        e = [p.get("echo") or {} for p in ps]
        rows.append((f"tauri (xterm.js {rend})", label, med([p["fps"] for p in ps]), med([p["frame_p50_ms"] for p in ps]),
                     med([x.get("p50_ms") for x in e]), med([x.get("p99_ms") for x in e])))
print("| App | Scenario | fps | frame p50 ms | key->echo painted p50 ms | p99 ms |\n|---|---|---|---|---|---|")
for r in rows: print("| " + " | ".join("–" if v is None else str(v) for v in r) + " |")
