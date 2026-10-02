#!/usr/bin/env python3
"""Median-of-rounds table for the terminal renderer comparison.
usage: summarize-renderers.py <results dir> [prefix=tauri]"""
import glob, json, statistics as st, sys, os
D = sys.argv[1]; pre = sys.argv[2] if len(sys.argv) > 2 else "tauri"
def med(xs):
    xs = [x for x in xs if isinstance(x, (int, float))]
    return st.median(xs) if xs else None
def f(x, d=0): return "–" if x is None else f"{x:.{d}f}"
PH = [("board_idle", "board"), ("idle_terminals", "4 term idle+typing"), ("stress3_chat_typing", "3 flooding+chat, typing"),
      ("stress4_chat_terminals_visible", "4 flooding+chat"), ("stress4_chat_chat_visible", "4 flooding, chat visible")]
print("| renderer | runs | scenario | fps | frame p50 / p99 ms | echo p50 / p99 ms | echo arrived p50 ms | open ms |")
print("|---|---|---|---|---|---|---|---|")
for r in ["webgl", "dom", "wterm", "wterm-lite", "ghostty-web"]:
    runs, partial = [], 0
    for p in sorted(glob.glob(f"{D}/{pre}-{r}-[0-9]*.json")):
        try: runs.append(json.load(open(p))); continue
        except Exception: pass
        # run never finished: fall back to the per-phase progress lines in the .log
        log = p[:-5] + ".log"
        if os.path.exists(log):
            ph = {}
            for line in open(log):
                if line.startswith("QUARK_BENCH_PROGRESS "):
                    d = json.loads(line.split(" ", 1)[1]); ph[d["phase"]] = d
            if ph: runs.append({"phases": ph}); partial += 1
    runs = [x for x in runs if "phases" in x]
    if not runs: print(f"| {r} | 0 | (no valid runs) |||||"); continue
    om = med([med(x.get("terminal_open_ms", [])) for x in runs])
    for k, label in PH:
        ps = [x["phases"].get(k, {}) for x in runs]
        e = [p.get("echo") or {} for p in ps]
        print(f"| {r} | {len(runs)}{' (' + str(partial) + ' partial)' if partial else ''} | {label} | {f(med([p.get('fps') for p in ps]),1)} | {f(med([p.get('frame_p50_ms') for p in ps]))} / {f(med([p.get('frame_p99_ms') for p in ps]))} | "
              f"{f(med([x.get('p50_ms') for x in e]))} / {f(med([x.get('p99_ms') for x in e]))} | {f(med([x.get('echo_arrived_p50_ms') for x in e]))} | {f(om) if k=='board_idle' else ''} |")
