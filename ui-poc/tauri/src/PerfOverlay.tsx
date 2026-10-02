import React, { useEffect, useRef, useState } from "react";
import { frames, latency } from "./perf";
import { getState } from "./store";
import { allTerms } from "./terms";
import { setNav } from "./nav";

export function PerfOverlay() {
  const [, tick] = useState(0);
  const canvas = useRef<HTMLCanvasElement>(null);
  const lastEvents = useRef({ t: performance.now(), n: getState().events, rate: 0 });

  useEffect(() => {
    frames.acquire();
    const id = window.setInterval(() => {
      const now = performance.now(), n = getState().events;
      const le = lastEvents.current;
      le.rate = ((n - le.n) * 1000) / (now - le.t);
      le.t = now; le.n = n;
      tick((x) => x + 1);
      // sparkline of frame times (last 2s); 16.7ms guide line
      const c = canvas.current;
      if (!c) return;
      const g = c.getContext("2d")!;
      const W = c.width, H = c.height, xs = frames.window.slice(-W / 2);
      g.clearRect(0, 0, W, H);
      g.fillStyle = "#ffffff18";
      const y16 = H - (16.7 / 50) * H;
      g.fillRect(0, y16, W, 1);
      xs.forEach((dt, i) => {
        const h = Math.min(H, (dt / 50) * H);
        g.fillStyle = dt > 33 ? "#f2777a" : dt > 17.5 ? "#f6c177" : "#7bd88f";
        g.fillRect(i * 2, H - h, 1.5, h);
      });
    }, 250);
    return () => { clearInterval(id); frames.release(); };
  }, []);

  const f = frames.stats();
  const l = latency.stats();
  const renderers = allTerms().map((t) => t.renderer);
  return (
    <div className="perf">
      <div className="row2"><span>fps</span><b>{f.fps.toFixed(0)}</b></div>
      <div className="row2"><span>frame p50 / p99</span><span>{fmt(f.frame_p50_ms)} / {fmt(f.frame_p99_ms)}</span></div>
      <div className="row2"><span>echo p50 / p99 (n={l.n})</span><span>{fmt(l.p50_ms)} / {fmt(l.p99_ms)}</span></div>
      <div className="row2"><span>events/s</span><span>{lastEvents.current.rate.toFixed(0)}</span></div>
      <div className="row2"><span>terminals</span><span>{renderers.length ? renderers.join(",") : "–"}</span></div>
      <canvas ref={canvas} width={220} height={36} />
      <div className="btns">
        <button className="btn" onClick={() => latency.reset()}>reset echo</button>
        <button className="btn" onClick={() => setNav({ perf: false })}>close</button>
      </div>
    </div>
  );
}
const fmt = (x: number) => (isFinite(x) ? x.toFixed(1) + "ms" : "–");
