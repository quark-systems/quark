// Frame timing (rAF based) and keystroke-to-echo latency probe.

export function percentile(xs: number[], p: number): number {
  if (!xs.length) return NaN;
  const s = [...xs].sort((a, b) => a - b);
  const i = Math.min(s.length - 1, Math.max(0, Math.ceil((p / 100) * s.length) - 1));
  return s[i];
}
const round = (x: number) => Math.round(x * 100) / 100;

// ---------- frame timing ----------
class FrameMeter {
  private last = 0;
  private running = 0;
  window: number[] = []; // last ~2s of frame deltas, for the overlay
  recording: number[] | null = null; // full capture during a bench phase
  private raf = 0;

  acquire() {
    if (this.running++ === 0) {
      this.last = performance.now();
      const tick = (t: number) => {
        const dt = t - this.last;
        this.last = t;
        this.window.push(dt);
        let sum = 0;
        for (let i = this.window.length - 1; i >= 0; i--) {
          sum += this.window[i];
          if (sum > 2000) { this.window.splice(0, i); break; }
        }
        this.recording?.push(dt);
        this.raf = requestAnimationFrame(tick);
      };
      this.raf = requestAnimationFrame(tick);
    }
  }
  release() {
    if (--this.running === 0) cancelAnimationFrame(this.raf);
  }
  stats(xs = this.window) {
    const total = xs.reduce((a, b) => a + b, 0);
    return {
      frames: xs.length,
      fps: total > 0 ? round((xs.length * 1000) / total) : 0,
      frame_p50_ms: round(percentile(xs, 50)),
      frame_p99_ms: round(percentile(xs, 99)),
      frame_max_ms: round(xs.length ? Math.max(...xs) : NaN),
    };
  }
}
export const frames = new FrameMeter();

// ---------- input latency probe ----------
// Flow: keydown (t0) -> POST input -> daemon/tmux echo -> worker.output event ->
// term.write(bytes, cb) parsed -> next xterm onRender (or rAF fallback) = t1.
export interface Pending { ch: string; t0: number }
class LatencyProbe {
  pending: Pending[] = [];
  samples: number[] = [];
  /** keydown -> echo bytes arrived over the WebSocket (daemon + network + JS dispatch), before xterm */
  netSamples: number[] = [];
  enabled = true;
  onSample = new Set<(ms: number) => void>();

  keydown(ch: string, t0 = performance.now()) {
    if (!this.enabled || ch.length !== 1) return;
    this.pending.push({ ch, t0 });
    if (this.pending.length > 64) this.pending.shift();
  }
  /** Called with decoded output text for the probed pane; returns the matched keystrokes. */
  match(text: string): Pending[] {
    const hits: Pending[] = [];
    let from = 0;
    while (this.pending.length) {
      const i = text.indexOf(this.pending[0].ch, from);
      if (i < 0) break;
      hits.push(this.pending.shift()!);
      from = i + 1;
    }
    // drop stale keystrokes that never echoed (e.g. typed into a non-echoing program)
    const now = performance.now();
    while (this.pending.length && now - this.pending[0].t0 > 3000) this.pending.shift();
    return hits;
  }
  record(ms: number) {
    this.samples.push(ms);
    if (this.samples.length > 2000) this.samples.shift();
    this.onSample.forEach((f) => f(ms));
  }
  recordNet(ms: number) {
    this.netSamples.push(ms);
    if (this.netSamples.length > 2000) this.netSamples.shift();
  }
  stats(xs = this.samples) {
    const ns = this.netSamples;
    return {
      echo_arrived_p50_ms: round(percentile(ns, 50)), echo_arrived_p99_ms: round(percentile(ns, 99)), n: xs.length, p50_ms: round(percentile(xs, 50)), p99_ms: round(percentile(xs, 99)), max_ms: round(xs.length ? Math.max(...xs) : NaN) };
  }
  reset() { this.samples = []; this.netSamples = []; this.pending = []; }
}
export const latency = new LatencyProbe();
