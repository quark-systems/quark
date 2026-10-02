#!/usr/bin/env node
// Demo daemon for developing and testing the desktop app without quarkd or an engine.
//
// It serves the slice of /v1 the app uses (app/CONTRACT.md): the shapes quarkd already
// serves, plus the proposed Phase 1 endpoints, with demo Projects, tasks, transcripts, diffs
// and a fake terminal per task that echoes what you type.
//
//   node mock/daemon.mjs [--port 7380] [--quiet]
//
// --quiet turns off background activity (state changes, output) so tests are deterministic.
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { WebSocketServer } from "ws";

const args = process.argv.slice(2);
const argVal = (name, def) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] ? args[i + 1] : def;
};
const PORT = Number(argVal("--port", process.env.PORT ?? 7380));
const QUIET = args.includes("--quiet");
const here = path.dirname(fileURLToPath(import.meta.url));
const DIFFS = fs.readdirSync(path.join(here, "fixtures")).filter((f) => f.endsWith(".diff")).sort()
  .map((f) => fs.readFileSync(path.join(here, "fixtures", f), "utf8"));

const now = () => new Date().toISOString();
const minutesAgo = (m) => new Date(Date.now() - m * 60_000).toISOString();
let idn = 0;
const nextId = (p) => `${p}-${(++idn).toString(36)}${Math.random().toString(36).slice(2, 6)}`;

// ---------------------------------------------------------------- state

const projects = new Map();
const tasks = new Map();
const decisions = new Map();
const chats = new Map(); // project id -> ChatMessage[]
const transcripts = new Map(); // task id -> TranscriptEntry[]
const terms = new Map(); // task id -> { cols, rows, screen: string, line: string }
const diffOf = new Map(); // task id -> unified diff text

const events = []; // { seq, ... } bounded
let seq = 0;
const sockets = new Set();

function emit(type, payload, project_id = null) {
  const e = { seq: ++seq, project_id, type, ts: now(), payload };
  events.push(e);
  if (events.length > 20000) events.splice(0, events.length - 20000);
  const text = JSON.stringify(e);
  for (const ws of sockets) if (ws.readyState === 1) ws.send(text);
  return e;
}

function addProject(p) {
  const proj = {
    id: p.id ?? nextId("prj"), name: p.name, goal: p.goal ?? null, workspace_path: p.workspace_path ?? null,
    created_at: p.created_at ?? now(), updated_at: p.updated_at ?? now(),
    repos: p.repos ?? [], agent_config: p.agent_config ?? null, dispatch_preset: p.dispatch_preset ?? "balanced",
    coordinator_state: p.coordinator_state ?? "running",
  };
  projects.set(proj.id, proj);
  chats.set(proj.id, []);
  return proj;
}

function addTask(projectId, t, { silent = false } = {}) {
  const task = {
    id: t.id ?? nextId("t"), project_id: projectId, title: t.title, state: t.state ?? "queued",
    kind: t.kind ?? "ship", state_note: t.state_note ?? null, harness: t.harness ?? "claude",
    pull_request_url: t.pull_request_url ?? null, created_at: t.created_at ?? now(), updated_at: t.updated_at ?? now(),
  };
  tasks.set(task.id, task);
  transcripts.set(task.id, []);
  terms.set(task.id, { cols: 120, rows: 36, screen: "", line: "" });
  diffOf.set(task.id, t.diff ?? "");
  if (!silent) emit("task.created", task, projectId);
  return task;
}

function setState(task, state, note = null) {
  task.state = state;
  task.state_note = note;
  task.updated_at = now();
  emit("task.state_changed", { ...task }, task.project_id);
}

function transcript(task, role, text, tool = null) {
  const entry = { id: nextId("tr"), ts: now(), role, text, tool };
  transcripts.get(task.id).push(entry);
  emit("worker.transcript", { task_id: task.id, entry }, task.project_id);
}

function output(task, text) {
  const term = terms.get(task.id);
  term.screen = (term.screen + text).slice(-16000);
  emit("worker.output", { task_id: task.id, data_b64: Buffer.from(text).toString("base64") }, task.project_id);
}

function chat(projectId, role, text) {
  const m = { id: nextId("m"), ts: now(), role, text };
  chats.get(projectId).push(m);
  emit("coordinator.message", m, projectId);
  return m;
}

// ---------------------------------------------------------------- seed

function seed() {
  const quark = addProject({
    id: "quark", name: "Quark MVP", goal: "Ship Phase 1: one Project end to end, from creation to a reviewed PR.",
    repos: ["quark-systems/quark", "quark-systems/firstmate"], agent_config: { harness: "claude", model: null, effort: "high" },
    created_at: minutesAgo(600), updated_at: minutesAgo(3),
  });
  const site = addProject({
    id: "website", name: "Website refresh", goal: "Move the marketing site to the new design system.",
    repos: ["quark-systems/website"], agent_config: { harness: "codex" }, created_at: minutesAgo(3000), updated_at: minutesAgo(90),
  });

  const T = (p, title, state, extra = {}) => addTask(p.id, { title, state, ...extra }, { silent: true });
  const a = T(quark, "Event stream: resync slow clients from the store", "running", { diff: DIFFS[0], harness: "claude", updated_at: minutesAgo(1) });
  const b = T(quark, "Terminal sessions over tmux control mode", "running", { diff: DIFFS[1], harness: "codex", updated_at: minutesAgo(4) });
  T(quark, "Decision records carry who answered", "needs_decision", { diff: DIFFS[2], state_note: "Asked: keep answer history per decision?", updated_at: minutesAgo(12) });
  T(quark, "Harness registry trait", "queued", { updated_at: minutesAgo(20) });
  T(quark, "OpenAPI check in CI", "in_review", { pull_request_url: "https://github.com/quark-systems/quark/pull/2", updated_at: minutesAgo(40) });
  T(quark, "Daemon skeleton", "done", { updated_at: minutesAgo(300) });
  T(site, "Pricing page on the new grid", "running", { diff: DIFFS[3], harness: "codex", updated_at: minutesAgo(8) });
  T(site, "Changelog feed", "failed", { state_note: "Build failed: missing RSS dependency", updated_at: minutesAgo(70) });

  decisions.set("d-1", {
    id: "d-1", project_id: quark.id, task_id: [...tasks.values()].find((t) => t.state === "needs_decision").id,
    question: "Keep the full answer history per decision, or only the latest answer?", state: "open", answer: null, opened_at: minutesAgo(12),
  });

  for (const t of [a, b]) {
    const tr = transcripts.get(t.id);
    tr.push({ id: nextId("tr"), ts: minutesAgo(30), role: "system", text: `Task contract loaded. Worktree ready on branch \`quark/${t.id}\`.` });
    tr.push({ id: nextId("tr"), ts: minutesAgo(29), role: "assistant", text: `I'll start by reading the current implementation and its tests.` });
    tr.push({ id: nextId("tr"), ts: minutesAgo(28), role: "tool", tool: "bash", text: "cargo test -p quarkd\n\nrunning 14 tests\n..............\ntest result: ok. 14 passed; 0 failed" });
    tr.push({ id: nextId("tr"), ts: minutesAgo(20), role: "assistant", text: "The tests pass on the base. Next I'll write the failing test for the new behaviour, then the change." });
    const term = terms.get(t.id);
    term.screen = `\x1b[1;35m${t.harness}\x1b[0m working on \x1b[1m${t.title}\x1b[0m\r\n\r\n` +
      `\x1b[32m✓\x1b[0m read the task contract\r\n\x1b[32m✓\x1b[0m cargo test -p quarkd (14 passed)\r\n` +
      `\x1b[33m…\x1b[0m writing the failing test\r\n\r\n$ `;
  }

  const ch = chats.get(quark.id);
  ch.push({ id: nextId("m"), ts: minutesAgo(35), role: "user", text: "Split Phase 1 into tasks and start the event stream and terminal work." });
  ch.push({ id: nextId("m"), ts: minutesAgo(34), role: "coordinator", text: "Dispatched two workers:\n\n- **Event stream**: resync slow clients from the store (Claude Code)\n- **Terminal sessions** over tmux control mode (Codex)\n\nThe decision-records task is waiting on a question for you." });
}

// ---------------------------------------------------------------- background activity

const LOG_LINES = [
  "\x1b[2m$\x1b[0m cargo check -p quarkd", "\x1b[32m    Checking\x1b[0m quarkd v0.0.1", "\x1b[32m    Finished\x1b[0m dev profile in 2.31s",
  "\x1b[2m$\x1b[0m cargo test -p quarkd events", "running 6 tests", "test events::replays_after_lag ... \x1b[32mok\x1b[0m",
  "test events::cursor_beyond_head ... \x1b[32mok\x1b[0m", "\x1b[36mEditing\x1b[0m crates/quarkd/src/api/events.rs",
  "\x1b[2m$\x1b[0m git diff --stat", " crates/quarkd/src/api/events.rs | 24 \x1b[32m++++++\x1b[31m--\x1b[0m",
];
const THOUGHTS = [
  "The replay loop can miss an event committed between the subscribe and the first page; subscribing first fixes it.",
  "Adding a test that drops the receiver mid-stream and checks no `seq` is skipped.",
  "Running the full test suite before the commit.",
];

function tick() {
  for (const t of tasks.values()) {
    if (t.state !== "running") continue;
    if (Math.random() < 0.5) output(t, LOG_LINES[Math.floor(Math.random() * LOG_LINES.length)] + "\r\n");
    if (Math.random() < 0.04) transcript(t, "assistant", THOUGHTS[Math.floor(Math.random() * THOUGHTS.length)]);
  }
}
function shuffleStates() {
  const queued = [...tasks.values()].filter((t) => t.state === "queued");
  if (queued.length && Math.random() < 0.5) {
    const t = queued[0];
    setState(t, "running", "Worker started");
    output(t, `\x1b[1;35m${t.harness}\x1b[0m starting \x1b[1m${t.title}\x1b[0m\r\n`);
  }
}

// ---------------------------------------------------------------- HTTP

function send(res, status, body, type = "application/json") {
  res.writeHead(status, { "content-type": type, ...cors });
  res.end(body === undefined ? "" : type === "application/json" ? JSON.stringify(body) : body);
}
const cors = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET, POST, PATCH, OPTIONS",
  "access-control-allow-headers": "content-type",
};
const notFound = (res) => send(res, 404, { error: { code: "not_found", message: "resource not found" } });
const invalid = (res, message) => send(res, 400, { error: { code: "invalid_request", message } });

async function readJson(req) {
  let raw = "";
  for await (const c of req) raw += c;
  if (!raw) return {};
  try { return JSON.parse(raw); } catch { return null; }
}

function filesOf(diff) {
  const files = [];
  let f = null;
  for (const line of diff.split("\n")) {
    const m = /^diff --git a\/(.*) b\/(.*)$/.exec(line);
    if (m) { f = { path: m[2], old_path: m[1] !== m[2] ? m[1] : null, status: "modified", additions: 0, deletions: 0 }; files.push(f); continue; }
    if (!f) continue;
    if (line.startsWith("new file")) f.status = "added";
    else if (line.startsWith("deleted file")) f.status = "deleted";
    else if (line.startsWith("rename from")) f.status = "renamed";
    else if (line.startsWith("+") && !line.startsWith("+++")) f.additions++;
    else if (line.startsWith("-") && !line.startsWith("---")) f.deletions++;
  }
  return files;
}
function diffFor(diff, p) {
  if (!p) return diff;
  const parts = diff.split(/^(?=diff --git )/m);
  return parts.find((x) => x.startsWith(`diff --git a/${p} `) || x.includes(` b/${p}\n`)) ?? "";
}

function terminalInput(task, data) {
  // A tiny line discipline: echo printable input, run "commands" on Enter.
  const term = terms.get(task.id);
  let out = "";
  for (const ch of data) {
    if (ch === "\r" || ch === "\n") {
      const cmd = term.line.trim();
      term.line = "";
      out += "\r\n";
      if (cmd) out += `you typed: ${cmd}\r\n`;
      out += "$ ";
    } else if (ch === "\x7f") {
      if (term.line) { term.line = term.line.slice(0, -1); out += "\b \b"; }
    } else if (ch >= " ") {
      term.line += ch;
      out += ch;
    }
  }
  if (out) output(task, out);
}

const server = http.createServer(async (req, res) => {
  if (req.method === "OPTIONS") return send(res, 204);
  const url = new URL(req.url, "http://x");
  const p = url.pathname;
  const m = (re) => re.exec(p);
  let r;

  if (req.method === "GET" && p === "/v1/health") return send(res, 200, { status: "ok", version: "mock", engine: "mock", last_seq: seq });
  if (p === "/v1/projects" && req.method === "GET") return send(res, 200, [...projects.values()]);
  if (p === "/v1/projects" && req.method === "POST") {
    const b = await readJson(req);
    if (!b || typeof b.name !== "string" || !b.name.trim()) return invalid(res, "name is required");
    const proj = addProject({ ...b, name: b.name.trim(), coordinator_state: "starting" });
    emit("project.updated", proj, proj.id);
    if (!QUIET) setTimeout(() => {
      proj.coordinator_state = "running"; proj.updated_at = now();
      emit("project.updated", { ...proj }, proj.id);
      chat(proj.id, "coordinator", `Workspace ready${proj.repos.length ? ` with ${proj.repos.join(", ")}` : ""}. What should we work on first?`);
    }, 1500);
    return send(res, 201, proj);
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)$/)) && req.method === "GET") {
    const proj = projects.get(decodeURIComponent(r[1]));
    return proj ? send(res, 200, proj) : notFound(res);
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/tasks$/)) && req.method === "GET") {
    const id = decodeURIComponent(r[1]);
    if (!projects.has(id)) return notFound(res);
    return send(res, 200, [...tasks.values()].filter((t) => t.project_id === id));
  }
  if (p === "/v1/decisions" && req.method === "GET") {
    const st = url.searchParams.get("state");
    return send(res, 200, [...decisions.values()].filter((d) => !st || d.state === st));
  }
  if (p === "/v1/harnesses" && req.method === "GET") {
    return send(res, 200, [
      { id: "claude", name: "Claude Code", installed: true, version: "2.1.0", models: ["opus", "sonnet"], efforts: ["low", "medium", "high", "xhigh"] },
      { id: "codex", name: "Codex", installed: true, version: "0.50.0", models: ["gpt-5-codex"], efforts: ["low", "medium", "high"] },
      { id: "pi", name: "Pi", installed: false, models: [], efforts: [], install_hint: "npm install -g @mariozechner/pi-coding-agent" },
    ]);
  }

  if ((r = m(/^\/v1\/coordinators\/([^/]+)\/messages$/))) {
    const cid = decodeURIComponent(r[1]);
    if (!chats.has(cid)) return notFound(res);
    if (req.method === "GET") return send(res, 200, chats.get(cid));
    if (req.method === "POST") {
      const b = await readJson(req);
      if (!b?.text?.trim()) return invalid(res, "text is required");
      chat(cid, "user", b.text.trim());
      setTimeout(() => {
        const t = addTask(cid, { title: b.text.trim().slice(0, 70), state: "queued", harness: projects.get(cid).agent_config?.harness ?? "claude" });
        chat(cid, "coordinator", `On it. I wrote a task contract and queued **${t.title}**.`);
      }, QUIET ? 50 : 700);
      return send(res, 202);
    }
  }

  if ((r = m(/^\/v1\/tasks\/([^/:]+)(.*)$/))) {
    const task = tasks.get(decodeURIComponent(r[1]));
    const rest = r[2];
    if (!task) return notFound(res);
    if (rest === "" && req.method === "GET") return send(res, 200, task);
    if (rest === "/messages" && req.method === "POST") {
      const b = await readJson(req);
      if (!b?.text?.trim()) return invalid(res, "text is required");
      transcript(task, "user", b.text.trim());
      setTimeout(() => transcript(task, "assistant", `Noted: "${b.text.trim()}". Adjusting the plan.`), QUIET ? 50 : 600);
      return send(res, 202);
    }
    if (rest === ":cancel" && req.method === "POST") {
      if (task.state === "done" || task.state === "failed") return send(res, 409, { error: { code: "conflict", message: "task already finished" } });
      setState(task, "failed", "Cancelled from the app");
      output(task, "\r\n\x1b[31m[worker exited: cancelled]\x1b[0m\r\n");
      return send(res, 202);
    }
    if (rest === ":relaunch" && req.method === "POST") {
      setState(task, "running", "Relaunched from the app");
      terms.get(task.id).screen = "";
      output(task, `\x1b[1;35m${task.harness}\x1b[0m relaunched on \x1b[1m${task.title}\x1b[0m\r\n$ `);
      return send(res, 202);
    }
    if (rest === "/transcript" && req.method === "GET") return send(res, 200, transcripts.get(task.id));
    if (rest === "/changes" && req.method === "GET") {
      return send(res, 200, { base: "main", head: `quark/${task.id}`, files: filesOf(diffOf.get(task.id)) });
    }
    if (rest === "/diff" && req.method === "GET") return send(res, 200, diffFor(diffOf.get(task.id), url.searchParams.get("path")), "text/plain; charset=utf-8");
    if (rest === "/terminal" && req.method === "GET") {
      const term = terms.get(task.id);
      return send(res, 200, {
        task_id: task.id, cols: term.cols, rows: term.rows, attached: task.state === "running", seq,
        snapshot_b64: Buffer.from("\x1b[2J\x1b[H" + term.screen).toString("base64"),
      });
    }
    if (rest === "/terminal/input" && req.method === "POST") {
      const b = await readJson(req);
      if (typeof b?.data_b64 !== "string") return invalid(res, "data_b64 is required");
      terminalInput(task, Buffer.from(b.data_b64, "base64").toString("utf8"));
      return send(res, 204);
    }
    if (rest === "/terminal/resize" && req.method === "POST") {
      const b = await readJson(req);
      const term = terms.get(task.id);
      if (Number.isInteger(b?.cols) && Number.isInteger(b?.rows)) { term.cols = b.cols; term.rows = b.rows; }
      return send(res, 204);
    }
  }
  // Unknown route: an empty 404, like axum's default, which the app reads as "not available yet".
  res.writeHead(404, cors);
  res.end();
});

const wss = new WebSocketServer({ noServer: true });
server.on("upgrade", (req, socket, head) => {
  const url = new URL(req.url, "http://x");
  if (url.pathname !== "/v1/events") return socket.destroy();
  wss.handleUpgrade(req, socket, head, (ws) => {
    const c = url.searchParams.get("cursor");
    // Like quarkd: no cursor means live only; a cursor replays every later event first.
    if (c !== null) for (const e of events) if (e.seq > Number(c)) ws.send(JSON.stringify(e));
    sockets.add(ws);
    ws.on("close", () => sockets.delete(ws));
  });
});

seed();
if (!QUIET) {
  setInterval(tick, 400);
  setInterval(shuffleStates, 5000);
}
server.listen(PORT, "127.0.0.1", () => console.log(`mock quarkd on http://127.0.0.1:${PORT}${QUIET ? " (quiet)" : ""}`));
