// Baseline: keystroke->echo latency of the daemon alone (no UI), for subtracting from UI numbers.
const D = process.env.DAEMON_HOST ?? "127.0.0.1:7420";
const wid = process.argv[2] ?? "w-4";
const N = Number(process.argv[3] ?? 40);
const ws = new WebSocket(`ws://${D}/v1/events?cursor=999999999`);
await new Promise((r) => (ws.onopen = r));
let waiting = null;
ws.onmessage = (m) => {
  const e = JSON.parse(m.data);
  if (e.type !== "worker.output" || e.payload.worker_id !== wid || !waiting) return;
  const txt = Buffer.from(e.payload.data_b64, "base64").toString();
  if (txt.includes(waiting.ch)) { const w = waiting; waiting = null; w.res(performance.now() - w.t0); }
};
const post = (s) => fetch(`http://${D}/v1/workers/${wid}/input`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ data_b64: Buffer.from(s).toString("base64") }) });
await post("\x15"); await new Promise((r) => setTimeout(r, 300));
const xs = [];
for (let i = 0; i < N; i++) {
  const ch = "abcdefghij"[i % 10];
  const p = new Promise((res) => (waiting = { ch, t0: performance.now(), res }));
  const t = setTimeout(() => waiting?.res(NaN), 2000);
  await post(ch);
  xs.push(await p); clearTimeout(t);
  await new Promise((r) => setTimeout(r, 100));
}
await post("\x15");
const v = xs.filter(Number.isFinite).sort((a, b) => a - b);
const pc = (p) => v[Math.min(v.length - 1, Math.ceil(p / 100 * v.length) - 1)];
console.log(JSON.stringify({ worker: wid, n: v.length, p50_ms: +pc(50).toFixed(2), p99_ms: +pc(99).toFixed(2), max_ms: +v[v.length - 1].toFixed(2) }));
ws.close();
