import React, { useEffect, useRef, useState } from "react";
import { useStore } from "../store";
import { Worker } from "../api";
import { attach, chooseProbe, getTerm } from "../terms";
import { latency } from "../perf";

export function Terminals() {
  const workers = useStore((s) => s.workers);
  const tasks = useStore((s) => s.tasks);
  const [focused, setFocused] = useState<string | null>(null);
  const probe = chooseProbe(workers);
  const four = workers.slice(0, 4);

  if (!four.length) return <div className="empty">No live workers yet — waiting for the daemon.</div>;
  return (
    <div className="grid4">
      {four.map((w) => (
        <Pane key={w.id} w={w} probe={w.id === probe} focused={focused === w.id} onFocus={() => setFocused(w.id)}
          taskTitle={tasks[w.task_id]?.title} />
      ))}
    </div>
  );
}

function Pane({ w, probe, focused, onFocus, taskTitle }: { w: Worker; probe: boolean; focused: boolean; onFocus: () => void; taskTitle?: string }) {
  const body = useRef<HTMLDivElement>(null);
  const [renderer, setRenderer] = useState("");
  const [lat, setLat] = useState(latency.stats());

  useEffect(() => {
    const h = getTerm(w);
    h.probe = probe;
    attach(h, body.current!);
    setRenderer(h.renderer);
    let live = true;
    let inputEl: HTMLElement | null = null;
    const f = () => onFocus();
    h.ready.then(() => {
      if (!live) return;
      setRenderer(h.renderer);
      inputEl = h.adapter!.inputElement();
      inputEl?.addEventListener("focus", f);
      if (probe) h.adapter!.focus();
    });
    const ro = new ResizeObserver(() => h.adapter?.fit());
    ro.observe(body.current!);
    return () => { live = false; ro.disconnect(); inputEl?.removeEventListener("focus", f); };
  }, [w.id, probe]);

  useEffect(() => {
    if (!probe) return;
    let t = 0;
    const on = () => { if (!t) t = window.setTimeout(() => { t = 0; setLat(latency.stats()); }, 250); };
    latency.onSample.add(on);
    return () => { latency.onSample.delete(on); clearTimeout(t); };
  }, [probe]);

  return (
    <div className={"pane" + (focused ? " focused" : "")} onMouseDown={onFocus}>
      <div className="pane-head">
        <span style={{ color: "var(--fg)" }}>{w.title}</span>
        {taskTitle && <span className="faint" style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{taskTitle}</span>}
        <span className="spacer" />
        {probe && <span className="pill accent" title="keydown → echo rendered">echo p50 {fmt(lat.p50_ms)} p99 {fmt(lat.p99_ms)} n={lat.n}</span>}
        <span className="pill">{renderer}</span>
      </div>
      <div className="pane-body" ref={body} />
    </div>
  );
}
const fmt = (x: number) => (isFinite(x) ? x.toFixed(1) + "ms" : "–");
