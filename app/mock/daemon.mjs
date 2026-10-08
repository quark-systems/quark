#!/usr/bin/env node
// Demo daemon for developing and testing the desktop app without quarkd or an engine.
//
// It serves the slice of /v1 the app uses (app/CONTRACT.md), in the shapes quarkd and the
// Phase 1 PRs define, with demo Projects, tasks, transcripts, diffs and a fake terminal per
// task that echoes what you type.
//
//   node mock/daemon.mjs [--port 7380] [--quiet]
//
// --quiet turns off background activity (state changes, output) so tests are deterministic.
import crypto from "node:crypto";
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
const rules = new Map();
const withApplied = (rule) => ({ applied: [...decisions.values()].filter((d) => d.rule_id === rule.id).length, ...rule });
const chats = new Map(); // project id -> ChatMessage[]
const transcripts = new Map(); // task id -> TranscriptEntry[]
const terms = new Map(); // task id -> { cols, rows, screen: string, line: string }
const diffOf = new Map(); // task id -> unified diff text
const pulls = new Map(); // PR id -> PullRequest
const pullDiffs = new Map(); // PR id -> unified diff text
const artifacts = new Map(); // artifact id -> { content_type, body }
const prComments = []; // what the app sent, for tests: { pull_request_id, body, path, line, side }
const memoryProposals = new Map(); // proposal id -> MemoryProposal
const memoryEntries = new Map(); // project id -> MemoryEntry[] (the Project repo's memory/)
const accounts = new Map(); // account id -> Account, in the daemon's order (default first per harness)
const dispatches = new Map(); // task id -> DispatchRecord[], oldest first; kept after the task ends
const memoryCommits = new Map(); // commit id -> MemoryCommit
const userMemory = []; // UserMemoryEntry[] (user-level memory, shared by every Project)
// Persona packs, as quarkd ships them. The demo's default is plain, so it reads
// with neutral names unless a Project picks a pack.
const PERSONAS = [
  { id: "plain", name: "Plain", builtin: true, address: null, voice: "Write plainly and concisely.", vocabulary: [], roles: {}, ui_labels: {} },
  {
    id: "nautical", name: "Nautical", builtin: true, address: "captain", voice: "Address the user as captain.", vocabulary: ["aye", "shipshape"],
    roles: { user: "captain", coordinator: "first mate", worker: "crewmate", sub_coordinator: "second mate", investigation: "scout", decision: "captain's call" },
    ui_labels: { decisions: "Captain's calls", standing_approval: "Standing orders", memory: "Logbook" },
  },
  {
    id: "kitchen-brigade", name: "Kitchen brigade", builtin: true, address: "chef", voice: "Calm, brisk service talk.", vocabulary: ["heard", "all day"],
    roles: { user: "chef", coordinator: "expo", worker: "line cook", sub_coordinator: "sous chef", investigation: "tasting", decision: "chef's call" },
    ui_labels: { task: "Ticket", tasks: "Tickets", decisions: "Chef's calls", memory: "Recipe book", standing_approval: "Standing order" },
  },
];
let defaultPersona = "plain";
const personaOverrides = new Map(); // project id -> pack id

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
    repos: (p.repos ?? []).map((r) => ({ url: r.url, name: r.name ?? r.url.replace(/\.git$/, "").split(/[/:]/).pop() })),
    agent_config: p.agent_config ?? null, dispatch_preset: p.dispatch_preset ?? "single", delivery: p.delivery ?? "gated",
    status: p.status ?? "ready", status_detail: p.status_detail ?? null, standing_approval: p.standing_approval ?? false,
  };
  projects.set(proj.id, proj);
  chats.set(proj.id, []);
  return proj;
}

function addTask(projectId, t, { silent = false } = {}) {
  const task = {
    id: t.id ?? nextId("t"), project_id: projectId, title: t.title, state: t.state ?? "queued",
    kind: t.kind ?? "ship", state_note: t.state_note ?? null, harness: t.harness ?? "claude-code",
    pull_request_url: t.pull_request_url ?? null, created_at: t.created_at ?? now(), updated_at: t.updated_at ?? now(),
    account_id: t.account_id ?? (ACCOUNT_ENV[t.harness ?? "claude-code"] ? `default-${t.harness ?? "claude-code"}` : null),
    failovers: t.failovers ?? [], model: t.model ?? null, branch: t.branch ?? null,
  };
  tasks.set(task.id, task);
  transcripts.set(task.id, []);
  terms.set(task.id, { cols: 120, rows: 36, screen: "", line: "" });
  diffOf.set(task.id, t.diff ?? "");
  if (!silent) emit("task.created", task, projectId);
  return task;
}

// Why this agent (ADR-11): one record per worker spawn. `how` is "classifier" (a confident rule match
// whose selected profile was used), "coordinator" (no classifier: provider none) or "relaunch". The
// daemon also records "default_rule" (the classifier's on_failure: default fallback); the mock has no
// classifier failures, so it never produces one.
const DEMO_RULE = { id: "rule_1", when: "A focused change inside one crate with tests." };
const DEMO_CANDIDATES = [
  { harness: "claude-code", model: "claude-sonnet-5", passed: true, reason: "eligible",
    evidence: "provider=claude scope=all_models remaining=79% spendPriority=0.42 runway=through_reset" },
  { harness: "codex", model: "gpt-5.6-luna", passed: true, reason: "eligible",
    evidence: "provider=codex scope=all_models remaining=55% spendPriority=0.18 runway=through_reset" },
  { harness: "cursor", model: "cursor-grok-4.6-medium", passed: false, reason: "profile floor all_models below 15%",
    evidence: "provider=cursor scope=all_models remaining=9% spendPriority=-0.9 runway=projected_exhaustion" },
];
function recordDispatch(task, how, { silent = false, ts = now(), model = null, effort = null } = {}) {
  const chosen = { harness: task.harness, model, effort, account: task.account_id ?? null };
  const agent = task.harness + (model ? `:${model}` : "") + (effort ? ` (${effort} effort)` : "");
  const base = {
    id: nextId("dsp"), task_id: task.id, project_id: task.project_id, chosen, recorded_at: ts,
    trigger: how === "relaunch" ? "relaunch" : "spawn", decided_by: how, rule: null, candidates: [],
    classifier: { provider: "none", model: null, confidence: null }, failover: null,
  };
  const rec = how === "classifier" ? {
    ...base, rule: DEMO_RULE, candidates: DEMO_CANDIDATES,
    classifier: { provider: "system1", model: "jev-1.13.0", confidence: 0.91 },
    resolution: { status: "clear", reason: null, notes: ["rule matched"], output: "dispatch-resolve:\n  status: clear" },
    summary: `The classifier matched rule ${DEMO_RULE.id} (${DEMO_RULE.when}) at 0.91 confidence. The resolution selected ${agent}.`,
  } : how === "relaunch" ? {
    ...base, resolution: { status: "not_consulted", reason: "relaunched in the same worktree", notes: [], output: null },
    summary: `Relaunched in the same worktree with ${agent}; dispatch rules were not consulted again.`,
  } : {
    ...base, resolution: { status: "off", reason: null, notes: [], output: null },
    summary: `No classifier is configured (provider: none), so the coordinator picked ${agent}.`,
  };
  if (!dispatches.has(task.id)) dispatches.set(task.id, []);
  dispatches.get(task.id).push(rec);
  if (!silent) emit("dispatch.recorded", rec, task.project_id);
  return rec;
}

// GET and PUT /v1/projects/{id}/dispatch: each Project's dispatch.yaml, as its preset wrote it until it is saved.
const dispatchRules = new Map(); // project id -> DispatchRules
const hex40 = () => crypto.randomBytes(20).toString("hex");
const TRIVIAL_WHEN = "A trivial mechanical edit such as a rename, typo or one-line fix.";
function rulesOf(proj) {
  if (!dispatchRules.has(proj.id)) {
    const agent = proj.agent_config ?? { harness: "claude-code", model: "claude-sonnet-5", effort: "medium" };
    const profile = (effort) => ({ harness: agent.harness, model: agent.model ?? null, effort: effort ?? agent.effort ?? null, pool: agent.pool ?? null });
    dispatchRules.set(proj.id, {
      project_id: proj.id, revision: hex40(), commit: hex40(), classifier: { provider: "none" }, default_select: "ordered",
      rules: proj.dispatch_preset === "light_trivial" ? [{ name: "trivial-edit", when: TRIVIAL_WHEN, select: null, candidates: [profile("low")] }] : [],
      default: [profile()],
    });
  }
  return dispatchRules.get(proj.id);
}
const blank = (v) => (typeof v === "string" && v.trim() ? v.trim() : null);
const cleanProfile = (c) => ({
  harness: c?.harness, model: blank(c?.model), effort: blank(c?.effort), pool: blank(c?.pool),
  ...(blank(c?.provider) ? { provider: blank(c.provider) } : {}), ...(c?.floor ? { floor: c.floor } : {}), ...(blank(c?.pricing) ? { pricing: blank(c.pricing) } : {}),
});
// The draft as quarkd would write and read it back, or the compile error as { error }.
function compileDraft(b) {
  const fail = (m) => ({ error: `dispatch.yaml: ${m}` });
  if (!b || typeof b !== "object") return fail("expected a mapping");
  if (b.default_select != null && !["ordered", "quota-balanced"].includes(b.default_select)) return fail(`unknown variant \`${b.default_select}\`, expected \`quota-balanced\` or \`ordered\``);
  const profiles = (list, where) => {
    if (!Array.isArray(list) || !list.length) return `${where}: needs at least one profile`;
    for (const [i, c] of list.entries()) if (typeof c?.harness !== "string" || !c.harness.trim()) return `${where}: profile ${i + 1}: harness is empty`;
    return null;
  };
  const names = new Set();
  const rules = [];
  for (const [i, r] of (b.rules ?? []).entries()) {
    const name = blank(r?.name);
    const label = name ? `rule ${i + 1} (${name})` : `rule ${i + 1}`;
    if (name && names.has(name)) return fail(`${label}: another rule has this name`);
    if (name) names.add(name);
    if (typeof r?.when !== "string" || !r.when.trim()) return fail(`${label}: when is empty`);
    const bad = profiles(r.candidates, `${label}: use`);
    if (bad) return fail(bad);
    rules.push({
      name, when: r.when, candidates: r.candidates.map(cleanProfile), select: r.select ?? null,
      ...(blank(r.why) ? { why: r.why } : {}), ...(blank(r.approval) ? { approval: r.approval } : {}), ...(r.floor ? { floor: r.floor } : {}),
    });
  }
  const def = b.default ?? [];
  if (def.length) { const bad = profiles(def, "default"); if (bad) return fail(bad); }
  return { default_select: b.default_select ?? null, rules, default: def.map(cleanProfile) };
}
const sameRules = (a, b) => JSON.stringify([a.default_select, a.rules, a.default]) === JSON.stringify([b.default_select, b.rules, b.default]);

// GET and PATCH /v1/projects/{id}/settings: the dashboard's Settings tab. Gates per source, as
// project.yaml declares them; the mock's quark source has a check and one holdout category.
const verification = new Map(); // project id -> VerificationSettings
function verificationOf(proj) {
  if (!verification.has(proj.id)) {
    verification.set(proj.id, {
      revision: hex40(), error: null,
      sources: proj.repos.map((r) => ({
        source: r.name,
        checks: r.name === "quark" ? [{ name: "test", run: "cargo test --workspace", timeout_s: 1800 }] : [],
        journeys: null,
        holdout: { enabled: true, categories: r.name === "quark" ? ["daemon-api"] : [], timeout_s: null },
      })),
    });
  }
  return verification.get(proj.id);
}
// GET /v1/projects/{id}/metrics: the dashboard's Metrics tab. The mock derives a plausible history
// from the Project's tasks: each finished task lands on a day of the window by its index.
function metricsOf(proj, days) {
  days = Math.min(90, Math.max(1, days));
  const day = 86400000, end = new Date(), start = new Date(Date.UTC(end.getUTCFullYear(), end.getUTCMonth(), end.getUTCDate()) - (days - 1) * day);
  const per_day = Array.from({ length: days }, (_, i) => ({ date: new Date(start.getTime() + i * day).toISOString().slice(0, 10), done: 0, failed: 0 }));
  const mine = [...tasks.values()].filter((t) => t.project_id === proj.id);
  let done = 0, failed = 0, i = 0;
  for (const t of mine) {
    if (t.state !== "done" && t.state !== "failed") continue;
    const d = per_day[per_day.length - 1 - (i++ % Math.min(days, 5))];
    if (t.state === "done") { d.done++; done++; } else { d.failed++; failed++; }
  }
  const decisions = mine.filter((t) => t.state === "needs_decision").length;
  const blockers = mine.filter((t) => t.state === "blocked").length;
  const used = {};
  for (const t of mine) if (t.account_id) used[t.account_id] = (used[t.account_id] ?? 0) + 1;
  const finished = done + failed;
  return {
    project_id: proj.id, days, from: start.toISOString(), to: end.toISOString(),
    log_started_at: new Date(start.getTime() - day).toISOString(),
    throughput: { done, failed, per_day },
    lead_time: { tasks: done, median_s: done ? 2 * 3600 + 900 : null, p90_s: done ? 7 * 3600 : null },
    gates: { pass_rate: finished ? done / finished : null, first_time_green: Math.max(0, done - 1), first_time_green_rate: done ? Math.max(0, done - 1) / done : null },
    interventions: { decisions, blockers, per_finished_task: finished ? (decisions + blockers) / finished : null, relaunches: 1 },
    failovers: { relaunched: 1, no_healthy_account: 0, relaunch_failed: 0 },
    accounts: Object.entries(used).flatMap(([id, n]) => {
      const a = accounts.get(id);
      return a ? [{ account_id: id, harness: a.harness, label: a.label, tasks: n, quota: a.quota }] : [];
    }),
    coordinator: {
      tasks: mine.length,
      baseline: { turns: 14, ack_turns: 0, input_tokens: 1_840_000, output_tokens: 26_000, cache_read_tokens: 1_610_000,
        turns_per_task: mine.length ? 14 / mine.length : null, tokens_per_task: mine.length ? 1_866_000 / mine.length : null, ack_share: null },
      native: { turns: 0, ack_turns: 0, input_tokens: 0, output_tokens: 0, cache_read_tokens: 0 },
      would_wake_turns: 0,
    },
    spend: {
      workers: { turns: 62, input_tokens: 9_400_000, output_tokens: 310_000, cache_read_tokens: 8_200_000, usd: 18.42, unpriced_tokens: 0 },
      coordinator: { turns: 14, input_tokens: 1_840_000, output_tokens: 26_000, cache_read_tokens: 1_610_000, usd: 2.31, unpriced_tokens: 0 },
      by_model: [
        { model: "claude-opus-5-5", input_tokens: 6_100_000, output_tokens: 190_000, usd: 14.6 },
        { model: "claude-sonnet-5-5", input_tokens: 3_300_000, output_tokens: 120_000, usd: 3.82 },
        { model: "gpt-5-codex", input_tokens: 1_840_000, output_tokens: 26_000, usd: 2.31 },
      ],
      done_tasks: done, usd_per_done_task: done ? 4.75 : null,
    },
    unavailable: [],
  };
}
function settingsOf(proj) {
  const rules = rulesOf(proj);
  return {
    project_id: proj.id, standing_approval: proj.standing_approval, delivery: proj.delivery ?? "gated",
    agent_config: proj.agent_config, verification: verificationOf(proj),
    dispatch: { rules: rules.rules.length, default_candidates: rules.default.length, classifier: "none", error: null },
    memory: {
      entries: (memoryEntries.get(proj.id) ?? []).length,
      proposals_to_review: [...memoryProposals.values()].filter((m) => m.project_id === proj.id && m.state === "proposed").length,
    },
  };
}

// The Automation tab (slice 7): inbox, trigger rules and the away policy. Like quarkd before slice 7
// switches on, the mock is not acting: rules only count would-be fires. A note lands in the inbox
// at once (quarkd mirrors firstmate's on its next pass).
const ROUTE_WAKE = new Set(["done", "decision", "blocked", "failed", "inbound", "trigger_failed"]);
const ROUTE_USER = {
  progress: ["silent", "silent", "silent"], paused: ["silent", "silent", "silent"], stale: ["silent", "silent", "silent"],
  done: ["notify", "digest", "digest"], decision: ["notify", "hold", "notify"], blocked: ["notify", "hold", "digest"],
  failed: ["notify", "digest", "notify"], inbound: ["silent", "silent", "silent"],
  trigger_fired: ["digest", "digest", "digest"], trigger_failed: ["notify", "digest", "notify"],
};
const POSTURES = ["present", "away", "quiet"];
const defaultRoute = (posture, occasion) => ({ wake: ROUTE_WAKE.has(occasion), user: ROUTE_USER[occasion][POSTURES.indexOf(posture)] });
const automation = new Map(); // project id -> { inbox, rules: Map, digest_secs, overrides: Map("posture/occasion" -> route) }
function automationState(proj) {
  if (!automation.has(proj.id)) {
    automation.set(proj.id, {
      inbox: [{ id: "1-seed", channel: "inbox", from: "cli", body: "Check whether the nightly build is still red.", at: now(), task_id: null }],
      rules: new Map([["upstream-quark-12", {
        id: "upstream-quark-12", description: "Follow up on the upstream issue", enabled: true, once: true,
        when: { source: "command", argv: ["gh", "issue", "view", "12", "--json", "state"], interval_secs: 3600, stable: 1, timeout_secs: 60, expect: { op: "differs", value: "OPEN" }, error_budget: 24 },
        then: { do: "wake", note: "The upstream issue changed." }, defined_at: now(), fires: 0,
      }]]),
      digest_secs: 3600, overrides: new Map(),
    });
  }
  return automation.get(proj.id);
}
function awayOf(a) {
  const routes = [];
  for (const posture of POSTURES) for (const occasion of Object.keys(ROUTE_USER)) {
    const o = a.overrides.get(`${posture}/${occasion}`);
    routes.push({ posture, occasion, ...(o ?? defaultRoute(posture, occasion)), overridden: !!o });
  }
  return { posture: "present", digest_secs: a.digest_secs, routes, waiting: 0, held: 0 };
}
const automationOf = (proj) => {
  const a = automationState(proj);
  return { project_id: proj.id, acting: false, inbox: a.inbox, rules: [...a.rules.values()], away: awayOf(a) };
};
function ruleProblem(id, b) {
  if (!/^[A-Za-z0-9_.-]{1,64}$/.test(id)) return "a rule id is 1 to 64 letters, digits, '-', '_' or '.'";
  const w = b?.when, t = b?.then;
  if (!w || !["event", "every", "at", "command"].includes(w.source)) return "when: unknown condition source";
  if (!t || !["wake", "inbox", "steer", "command"].includes(t.do)) return "then: unknown action";
  if (w.source === "event" && !w.kind) return "an event condition needs a kind";
  if (w.source === "event" && String(w.kind).startsWith("trigger.")) return "a rule cannot watch trigger events";
  if (w.source === "every" && !(w.secs > 0)) return "an interval must be at least one second";
  if (w.source === "at" && isNaN(Date.parse(w.at))) return "at must be an RFC 3339 time";
  if ((w.source === "command" && !w.argv?.length) || (t.do === "command" && !t.argv?.length)) return "a command needs at least one argument";
  return null;
}

// GET /v1/hosts and /v1/projects/{id}/hosts: the Hosts view and a Project's slice of its hosts. quarkd
// folds host registrations, telemetry samples and worktree pool reports from its event log; the mock
// makes up one healthy Mac whose running tasks each use a little of it.
const GB = 1024 ** 3, MB = 1024 ** 2;
function hostSamples(hours) {
  const n = Math.min(120, hours * 60), now = Date.now(), step = (hours * 3_600_000) / n;
  return Array.from({ length: n }, (_, i) => {
    const w = Math.sin(i / 9) * 0.5 + 0.5;
    return { at: new Date(now - (n - 1 - i) * step).toISOString(), cpu: 0.15 + 0.35 * w, memory_used_bytes: Math.round((14 + 6 * w) * GB), memory_pressure: 0.1 + 0.2 * w, disk_free_bytes: Math.round((180 - i * 0.05) * GB) };
  });
}
function hostUsage(proj) {
  const running = [...tasks.values()].filter((t) => t.project_id === proj.id && ["running", "in_review", "needs_decision", "blocked"].includes(t.state));
  const parts = [{ engine_task: null, task_id: null, title: null, cpu: 0.02, memory_bytes: 380 * MB, disk_bytes: 0 },
    ...running.map((t, i) => ({ engine_task: t.id, task_id: t.id, title: t.title, cpu: 0.04 + 0.03 * i, memory_bytes: (600 + 150 * i) * MB, disk_bytes: (220 + 40 * i) * MB }))];
  const sum = (k) => parts.reduce((a, p) => a + p[k], 0);
  return { project_id: proj.id, project_name: proj.name, cpu: sum("cpu"), memory_bytes: sum("memory_bytes"), disk_bytes: sum("disk_bytes"), parts };
}
function hostSlots() {
  const slots = [];
  for (const t of tasks.values()) if (["running", "in_review", "needs_decision", "blocked"].includes(t.state)) {
    slots.push({ path: `/Users/you/.quark/pools/${t.project_id}/wt-${slots.length + 1}`, repo: `/Users/you/.quark/projects/${t.project_id}/projects/quark`, state: "in_use", holder: t.id, project_id: t.project_id });
  }
  for (let i = 0; i < 2; i++) slots.push({ path: `/Users/you/.quark/pools/idle-${i + 1}`, repo: "/Users/you/.quark/projects/quark/projects/quark", state: "idle", holder: null, project_id: "quark" });
  return slots;
}
const MAC = { id: "mac", name: "MacBook Pro", runtime: "local", os: "macos", arch: "aarch64",
  capacity: { cpus: 12, memory_bytes: 36 * GB, disk_bytes: 0, max_workers: 6 }, health: { status: "healthy", reason: null, since: null } };
function hostReading(series) {
  const p = series.at(-1);
  return { ...p, memory_total_bytes: MAC.capacity.memory_bytes, quark_disk: { worktrees_bytes: 3.2 * GB, logs_bytes: 410 * MB, caches_bytes: 1.1 * GB, event_log_bytes: 64 * MB } };
}
function hostsOf(hours) {
  const series = hostSamples(hours);
  const slots = hostSlots();
  const count = (st) => slots.filter((s) => s.state === st).length;
  const projectsHere = [...projects.values()].map(hostUsage).filter((u) => u.parts.length > 1).sort((a, b) => b.memory_bytes - a.memory_bytes);
  return { hours, error: null, hosts: [{ ...MAC, latest: hostReading(series), series, projects: projectsHere,
    worktrees: { at: new Date().toISOString(), idle: count("idle"), in_use: count("in_use"), dirty: 0, leased: 0, quarantined: 0, error: null, slots } }] };
}
function projectHostsOf(proj, hours) {
  const series = hostSamples(hours);
  const now = hostUsage(proj);
  const slots = hostSlots().filter((s) => s.project_id === proj.id);
  if (now.parts.length <= 1 && slots.length === 0) return { project_id: proj.id, hours, hosts: [], error: null };
  const scale = (p) => 0.6 + 0.4 * (p.cpu - 0.15) / 0.35;
  return { project_id: proj.id, hours, error: null, hosts: [{ host_id: MAC.id, name: MAC.name, health: MAC.health, capacity: MAC.capacity, now, host: hostReading(series),
    series: series.map((p) => ({ at: p.at, cpu: now.cpu * scale(p), memory_bytes: Math.round(now.memory_bytes * scale(p)), disk_bytes: now.disk_bytes })), worktrees: slots }] };
}

// GET /v1/projects/{id}/overview: the dashboard's Overview tab. quarkd folds its event log; the mock
// folds the tasks it holds for live status and its own task events for the digest after `since`.
const PULSE = { queued: "working", running: "working", in_review: "working", unknown: "working", needs_decision: "needs_decision", blocked: "blocked", paused: "paused", done: "done", failed: "failed" };
function overviewOf(proj, since) {
  const mine = events.filter((e) => e.project_id === proj.id && e.type === "task.event");
  const counts = { working: 0, needs_decision: 0, blocked: 0, paused: 0, done: 0, failed: 0 };
  const open = new Set([...decisions.values()].filter((d) => d.project_id === proj.id && d.state === "open" && d.task_id).map((d) => d.task_id));
  const day = Date.now() - 86_400_000;
  const list = [...tasks.values()].filter((t) => t.project_id === proj.id)
    .map((t) => ({ t, state: open.has(t.id) ? "needs_decision" : PULSE[t.state] ?? "working" }))
    .filter(({ t, state }) => !(state === "done" || state === "failed") || Date.parse(t.updated_at) > day);
  const live = list.map(({ t, state }) => {
    counts[state]++;
    const d = dispatches.get(t.id)?.at(-1);
    return {
      engine_task: t.id, task_id: t.id, title: t.title, state, verb: t.state.replace("_", "-"), note: t.state_note ?? t.title, at: t.updated_at,
      harness: t.harness, model: d?.model ?? null, open_decisions: open.has(t.id) ? ["default"] : [], pull_request: t.pull_request_url,
    };
  }).sort((a, b) => Number(b.state !== "done" && b.state !== "failed") - Number(a.state !== "done" && a.state !== "failed") || b.at.localeCompare(a.at) || a.engine_task.localeCompare(b.engine_task));
  const head = mine.at(-1)?.seq ?? 0;
  let digest = null;
  if (since != null) {
    const after = mine.filter((e) => e.seq > since);
    digest = { since, from: after[0]?.ts ?? null, to: after.at(-1)?.ts ?? null, events: after.length, spawned: 0, done: 0, pull_requests: 0, failed: 0, decisions_opened: 0, decisions_resolved: 0, highlights: [], truncated: false };
    for (const e of [...after].reverse()) {
      const { kind: state, note, task_id } = e.payload;
      const url = state === "done" ? tasks.get(task_id)?.pull_request_url ?? null : null;
      const kind = state === "done" ? "done" : state === "failed" ? "failed" : state === "needs_decision" ? "decision_opened"
        : state === "running" && note.startsWith("Answered") ? "decision_resolved" : state === "running" ? "spawned" : null;
      if (!kind) continue;
      digest[{ done: "done", failed: "failed", decision_opened: "decisions_opened", decision_resolved: "decisions_resolved", spawned: "spawned" }[kind]]++;
      if (url) digest.pull_requests++;
      if (digest.highlights.length < 50) digest.highlights.push({ seq: e.seq, at: e.ts, engine_task: task_id, task_id, title: tasks.get(task_id)?.title ?? null, kind, text: note, url });
      else digest.truncated = true;
    }
  }
  return { project_id: proj.id, head, live: { counts, tasks: live, last_activity: mine.at(-1)?.ts ?? null }, digest, error: null };
}

// POST /v1/projects/{id}/dispatch:test. The mock has no classifier, except that a description
// sharing a word with a rule's condition (a rename or a typo, for the trivial-edit rule) matches
// that rule, so both outcomes can be seen. Like quarkd, it does not resolve rules that are not saved.
const COMMON = new Set(["such", "that", "this", "with", "from", "task", "tasks", "file", "files", "change", "changes", "line"]);
const words = (s) => new Set((s.toLowerCase().match(/[a-z]{4,}/g) ?? []).filter((w) => !COMMON.has(w)));
function testDispatch(proj, description, draft = null) {
  const saved = rulesOf(proj);
  const rules = draft ?? saved;
  const unsaved = !!draft && !sameRules(draft, saved);
  const listed = [
    ...rules.rules.flatMap((r, i) => r.candidates.map((c) => ({ rule: { id: `rule_${i + 1}`, when: r.when }, rule_name: r.name ?? null, ...c }))),
    ...rules.default.map((c) => ({ rule: { id: "default", when: null }, rule_name: null, ...c })),
  ].map(({ rule, rule_name, harness, model, effort, pool }) => ({ rule, rule_name, harness, model: model ?? null, effort: effort ?? null, pool: pool ?? null }));
  const said = words(description);
  const hit = unsaved ? -1 : rules.rules.findIndex((r) => [...words(r.when)].some((w) => said.has(w)));
  const matched = hit >= 0;
  const rule = matched ? { id: `rule_${hit + 1}`, when: rules.rules[hit].when } : null;
  const candidates = (matched ? listed.filter((l) => l.rule.id === rule.id) : listed).map((l) => {
    const h = HARNESSES.find((x) => x.id === l.harness);
    const mine = accountList().filter((a) => a.harness === l.harness && (l.pool ? a.pools.includes(l.pool) : a.default));
    const quota = mine.map((a) => `${a.label}: ${a.quota.remaining_percent == null ? "quota not read" : `${a.quota.remaining_percent}% remaining`}`).join("; ");
    const evidence = matched && h?.install.installed ? `provider=${l.harness} scope=all_models remaining=79% spendPriority=0.42 runway=through_reset` : null;
    const checks = [
      { check: "harness_installed", passed: !!h?.install.installed,
        detail: !h ? `unknown harness \`${l.harness}\`` : h.install.installed ? `${h.name} ${h.install.version}` : `${h.name} is not installed: ${h.install.install_hint}` },
      { check: "model_accepted", passed: !!h, detail: !h ? `unknown harness \`${l.harness}\`` : `${l.model ? `model ${l.model}` : "the harness's default model"}${l.effort ? `, ${l.effort} effort` : ""}` },
      { check: "account_health", passed: mine.length ? mine.some((a) => a.health.state !== "not_configured") : !l.pool,
        detail: mine.length ? mine.map((a) => `${a.label}: ${a.health.state === "configured" ? "logged in" : a.health.state === "unknown" ? "login unknown" : "not logged in"}`).join("; ")
          : l.pool ? `pool \`${l.pool}\` has no ${h?.name ?? l.harness} accounts` : `${h?.name ?? l.harness}: logged in` },
      { check: "quota_headroom", passed: !mine.length || mine.some((a) => a.quota.remaining_percent == null || a.quota.remaining_percent > 0),
        detail: (evidence ? `resolution: eligible (${evidence}); ` : "") + (quota || `quota is not read per account for ${h?.name ?? l.harness}`) },
    ];
    const failed = checks.find((c) => !c.passed);
    return { ...l, passed: !failed, reason: failed ? failed.detail : "eligible", evidence: evidence ?? (quota || null), checks };
  });
  const passing = candidates.filter((c) => c.passed).length;
  const tail = `${passing} of ${candidates.length} candidate${candidates.length === 1 ? "" : "s"} could start a worker now.`;
  const first = candidates.find((c) => c.passed);
  if (matched && first) {
    const label = first.harness + (first.model ? `:${first.model}` : "") + (first.effort ? ` (${first.effort} effort)` : "");
    return {
      project_id: proj.id, decided_by: "classifier", rule, candidates,
      summary: `The classifier matched rule ${rule.id} (${rule.when}) at 0.91 confidence. The resolution would select ${label}. ${tail}`,
      resolution: { status: "clear", reason: null, notes: ["rule matched"], output: "dispatch-resolve:\n  status: clear" },
      classifier: { provider: "system1", model: "jev-1.13.0", confidence: 0.91 },
      chosen: { harness: first.harness, model: first.model, effort: first.effort, account: null },
    };
  }
  if (unsaved) {
    const reason = "These rules are not saved, and the dispatch resolution reads the saved ones, so it was not run";
    return {
      project_id: proj.id, decided_by: "coordinator", rule: null, chosen: null, candidates,
      summary: `${reason}; the coordinator would pick. ${tail}`,
      resolution: { status: "not_consulted", reason, notes: [], output: null },
      classifier: { provider: "none", model: null, confidence: null },
    };
  }
  return {
    project_id: proj.id, decided_by: "coordinator", rule: null, chosen: null, candidates,
    summary: `No classifier is configured (provider: none), so the coordinator would pick. ${tail}`,
    resolution: { status: "off", reason: null, notes: [], output: null },
    classifier: { provider: "none", model: null, confidence: null },
  };
}

let taskEventId = 0;
function setState(task, state, note = null) {
  const previous = task.state;
  task.state = state;
  task.state_note = note;
  task.updated_at = now();
  emit("task.state_changed", { task: { ...task }, previous_state: previous }, task.project_id);
  emit("task.event", { id: ++taskEventId, task_id: task.id, project_id: task.project_id, kind: state, note: note ?? "", ts: now() }, task.project_id);
}

// Transcript entries are identified by the seq of the event that carried them.
const entryOf = (role, text, extra = {}) =>
  ({ role, text, tool_name: null, tool_call_id: null, is_error: false, truncated: false, ts: now(), ...extra });

function transcript(task, role, text, extra) {
  const entry = entryOf(role, text, extra);
  const e = emit("worker.transcript", { task_id: task.id, entry }, task.project_id);
  transcripts.get(task.id).push({ ...entry, id: e.seq });
}

function chat(projectId, role, text, extra) {
  const entry = entryOf(role, text, extra);
  const e = emit("coordinator.message", { coordinator_id: projectId, entry }, projectId);
  chats.get(projectId).push({ ...entry, id: e.seq });
}

function outputEvent(task, kind, bytes) {
  const term = terms.get(task.id);
  return emit("worker.output", {
    terminal_id: task.id, role: "worker", task_id: task.id, kind, data_b64: Buffer.from(bytes).toString("base64"),
    ...(kind === "snapshot" ? { cols: term.cols, rows: term.rows } : {}),
  }, task.project_id);
}

function output(task, text) {
  const term = terms.get(task.id);
  term.screen = (term.screen + text).slice(-16000);
  outputEvent(task, "output", text);
}

/** Records a Project memory entry with the commit that adds its file, as quarkd does on accept. */
function addMemoryEntry(e) {
  const sha = Array.from({ length: 40 }, () => Math.floor(Math.random() * 16).toString(16)).join("");
  const entry = { accepted_at: null, accepted_by: null, proposal_id: null, source: null, date: null, ...e, path: `memory/${e.id}.md`, commit: sha };
  const q = (v) => (v == null ? "null" : JSON.stringify(v));
  const file = [
    "---", `date: ${q(entry.date)}`, `source: ${q(entry.source)}`, `task: ${q(entry.evidence.task_id)}`, `task_title: ${q(entry.evidence.task_title)}`,
    `pull_request: ${q(entry.evidence.pull_request_url)}`, `files: ${JSON.stringify(entry.evidence.files)}`, `proposal: ${q(entry.proposal_id)}`,
    `accepted_at: ${q(entry.accepted_at)}`, `accepted_by: ${q(entry.accepted_by)}`, "---", "", ...entry.text.split("\n"),
  ];
  memoryCommits.set(sha, {
    commit: sha, subject: `Remember: ${entry.text.split("\n")[0].slice(0, 60)}`, author: entry.accepted_by ?? "mock-user", date: entry.accepted_at ?? now(),
    patch: `diff --git a/${entry.path} b/${entry.path}\nnew file mode 100644\n--- /dev/null\n+++ b/${entry.path}\n@@ -0,0 +1,${file.length} @@\n` +
      file.map((l) => "+" + l).join("\n"),
  });
  if (!memoryEntries.has(entry.project_id)) memoryEntries.set(entry.project_id, []);
  memoryEntries.get(entry.project_id).push(entry);
  return entry;
}

// ---------------------------------------------------------------- beads

// One Beads database per Project (app/src/api/beads.ts): its status, issues, memories and the New
// issue side chats. The mock plays `bd` and the coordinator: drafts come back after a short delay.
const beadsStatus = new Map(); // project id -> BeadsStatus
const beadsIssues = new Map(); // project id -> Map(issue id -> stored issue: Issue fields minus the computed ones, plus `related`, `comments`)
const beadsMemories = new Map(); // project id -> BeadsMemory[]
const issueDrafts = new Map(); // draft id -> IssueDraft

function beadsOf(pid) {
  if (!beadsStatus.has(pid)) {
    beadsStatus.set(pid, { project_id: pid, state: "missing", detail: null, dir: null, prefix: null, remote: null, github_repo: null, last_sync: null });
  }
  return beadsStatus.get(pid);
}
const issuesOf = (pid) => beadsIssues.get(pid) ?? new Map();
const ghRepo = (proj) => {
  const m = /github\.com[/:]([^/]+\/[^/.]+)/.exec(proj.repos[0]?.url ?? "");
  return m ? m[1] : null;
};

function addIssue(pid, i) {
  if (!beadsIssues.has(pid)) beadsIssues.set(pid, new Map());
  const issue = {
    description: "", status: "open", priority: 2, issue_type: "task", labels: [], assignee: null, owner: null, created_by: "matt",
    created_at: now(), updated_at: now(), closed_at: null, external_ref: null, blocked_by: [], related: [], comments: [],
    notes: null, design: null, acceptance_criteria: null, ...i,
  };
  beadsIssues.get(pid).set(issue.id, issue);
  return issue;
}
/** The issue as the API shows it: `ready` and `blocked` from what it waits for. */
function issueView(pid, i) {
  const all = issuesOf(pid);
  const blocked = i.status !== "closed" && i.blocked_by.some((b) => all.get(b) && all.get(b).status !== "closed");
  const { related, comments, notes, design, acceptance_criteria, ...rest } = i;
  return { ...rest, blocked, ready: i.status === "open" && !blocked };
}
const refOf = (i) => ({ id: i.id, title: i.title, status: i.status, issue_type: i.issue_type });
function issueDetail(pid, i) {
  const all = issuesOf(pid);
  return {
    ...issueView(pid, i), notes: i.notes, design: i.design, acceptance_criteria: i.acceptance_criteria, comments: i.comments,
    blocked_by_issues: i.blocked_by.map((b) => all.get(b)).filter(Boolean).map(refOf),
    blocks_issues: [...all.values()].filter((x) => x.blocked_by.includes(i.id)).map(refOf),
    related_issues: i.related.map((r) => all.get(r.id) && { ...refOf(all.get(r.id)), kind: r.kind }).filter(Boolean),
  };
}
function nextIssueId(pid) {
  const prefix = beadsOf(pid).prefix ?? "bd";
  const n = Math.max(0, ...[...issuesOf(pid).keys()].map((k) => Number(/-(\d+)$/.exec(k)?.[1] ?? 0)));
  return `${prefix}-${n + 1}`;
}
const beadsChanged = (pid, op, issue_id = null) => emit("beads.changed", { project_id: pid, issue_id, op }, pid);

function setupBeads(proj) {
  const b = beadsOf(proj.id);
  const steps = ["Installing the Dolt server", "Creating the database", "Importing GitHub Issues"];
  Object.assign(b, { state: "setting_up", detail: steps[0] });
  emit("beads.status", { ...b }, proj.id);
  steps.slice(1).forEach((detail, i) => setTimeout(() => {
    Object.assign(b, { detail });
    emit("beads.status", { ...b }, proj.id);
  }, (QUIET ? 60 : 900) * (i + 1)));
  setTimeout(() => {
    const repo = ghRepo(proj);
    Object.assign(b, {
      state: "ready", detail: null, dir: `/home/mock/.quark/projects/${proj.id}/projects/${proj.repos[0]?.name ?? proj.id}`,
      prefix: (proj.repos[0]?.name ?? proj.id).slice(0, 2), remote: proj.repos[0]?.url ?? null, github_repo: repo,
      last_sync: repo ? { at: now(), ok: true, pulled: 0, pushed: 0, message: "nothing to sync" } : null,
    });
    emit("beads.status", { ...b }, proj.id);
  }, (QUIET ? 60 : 900) * steps.length);
}

// The coordinator's side of the New issue chat, played naively: one or two drafts from the first
// message, and simple edits ("make the first one P0 and label both attention") from later ones.
const WORDS_SKIP = new Set(["when", "after", "that", "this", "with", "into", "until", "should", "there", "their", "about", "already", "would", "could"]);
const wordsOf = (s) => new Set((s.toLowerCase().match(/[a-z]{4,}/g) ?? []).filter((w) => !WORDS_SKIP.has(w)));
function titleOf(sentence) {
  let t = sentence.replace(/^(when|if|so|and|but|also)\s+/i, "").split(/[,;:]/)[0].replace(/[.!?]+$/, "").trim();
  if (t.length > 64) t = t.slice(0, 64).replace(/\s+\S*$/, "") + "…";
  return t.charAt(0).toUpperCase() + t.slice(1);
}
function relatedTo(pid, text) {
  const said = wordsOf(text);
  return [...issuesOf(pid).values()].filter((i) => i.status !== "closed" && i.issue_type !== "decision" && [...wordsOf(i.title)].some((w) => said.has(w)))
    .slice(0, 2).map((i) => i.id);
}
function firstDrafts(pid, text) {
  // The demo's own example reads as the mock design drew it.
  if (/\bred\b/i.test(text) && /phone/i.test(text)) {
    return {
      reply: "I read this as two pieces of work, the second depending on the first. I checked: nothing open covers it, and qk-41 is related but separate.",
      related: issuesOf(pid).has("qk-41") ? ["qk-41"] : [],
      issues: [
        { key: "1", title: "A PR that goes red asks for attention again", issue_type: "bug", priority: 1, labels: [], blocked_by: [],
          description: "When CI fails on a PR the user has already seen, set its worker's attention flag again so it re-enters Needs you, oldest first." },
        { key: "2", title: "Push a phone notification when a PR goes red", issue_type: "feature", priority: 2, labels: [], blocked_by: ["1"],
          description: "Follows the project's away policy. Depends on the attention change above." },
      ],
    };
  }
  const sentences = text.split(/(?<=[.!?])\s+/).map((s) => s.trim()).filter(Boolean);
  const type = /\b(red|broken|fails?|bug|crash|wrong|doesn't|don't|can't)\b/i.test(text) ? "bug" : "feature";
  const related = relatedTo(pid, text);
  const checked = related.length ? `${related.join(" and ")} ${related.length === 1 ? "is" : "are"} related but separate.` : "nothing open covers it.";
  if (sentences.length < 2) {
    return { reply: `I read this as one piece of work. I checked: ${checked}`, related,
      issues: [{ key: "1", title: titleOf(text), issue_type: type, priority: 2, labels: [], blocked_by: [], description: text }] };
  }
  return {
    reply: `I read this as two pieces of work, the second depending on the first. I checked: ${checked}`, related,
    issues: [
      { key: "1", title: titleOf(sentences[0]), issue_type: type, priority: 1, labels: [], blocked_by: [], description: sentences[0] },
      { key: "2", title: titleOf(sentences.slice(1).join(" ")), issue_type: "feature", priority: 2, labels: [], blocked_by: ["1"], description: sentences.slice(1).join(" ") },
    ],
  };
}
function refineDrafts(drafts, text) {
  const pick = (w) => (/^(both|all|them|each)$/i.test(w) ? drafts : /^(first|1)$/i.test(w) ? drafts.slice(0, 1) : /^(second|2)$/i.test(w) ? drafts.slice(1, 2)
    : /^(last)$/i.test(w) ? drafts.slice(-1) : []);
  const done = [];
  for (const m of text.matchAll(/\b(first|second|last|both|all|1|2)\b(?:\s+one)?\s+(?:to\s+|as\s+)?P([0-4])\b/gi)) {
    for (const d of pick(m[1])) d.priority = Number(m[2]);
    done.push(`${m[1].toLowerCase()} is P${m[2]}`);
  }
  for (const m of text.matchAll(/\blabel\s+(?:(?:the\s+)?(first|second|last|both|all|them|each)(?:\s+one)?\s+)?(?:as\s+|with\s+)?["']?([\w-]+)/gi)) {
    for (const d of pick(m[1] ?? "all")) if (!d.labels.includes(m[2])) d.labels.push(m[2]);
    done.push(`labelled ${m[2]}`);
  }
  for (const m of text.matchAll(/\bmake\s+(?:the\s+)?(first|second|last|both)\s+(?:one\s+)?an?\s+(bug|feature|task|chore|spike)\b/gi)) {
    for (const d of pick(m[1])) d.issue_type = m[2].toLowerCase();
    done.push(`${m[1].toLowerCase()} is a ${m[2].toLowerCase()}`);
  }
  if (!done.length) {
    const last = drafts.at(-1);
    if (last) last.description = `${last.description}\n\n${text}`.trim();
    return "Added that to the last draft.";
  }
  return `Done: ${done.join(", ")}.`;
}
/** PUT /v1/issue-drafts/{id}: the coordinator's drafts replace the previous ones, with its reply. */
function writeDraft(draft, { reply = null, issues, related = [] }) {
  if (reply?.trim()) draft.messages.push({ role: "coordinator", text: reply.trim(), at: now() });
  Object.assign(draft, { issues, related, waiting: false, updated_at: now() });
  emit("issue_draft.updated", structuredClone(draft), draft.project_id);
}
function draftMessage(draft, text) {
  draft.messages.push({ role: "user", text, at: now() });
  draft.waiting = true;
  draft.updated_at = now();
  emit("issue_draft.updated", structuredClone(draft), draft.project_id);
  const first = !draft.issues.length;
  setTimeout(() => {
    if (draft.state !== "open") return;
    if (first) {
      writeDraft(draft, firstDrafts(draft.project_id, text));
    } else {
      const issues = structuredClone(draft.issues);
      writeDraft(draft, { reply: refineDrafts(issues, text), issues, related: draft.related });
    }
  }, QUIET ? 150 : 1200);
}
function acceptDraft(draft, b) {
  const pid = draft.project_id;
  const list = Array.isArray(b.issues) && b.issues.length ? b.issues : draft.issues;
  const created = {};
  for (const d of list) created[d.key] = addIssue(pid, { id: nextIssueId(pid), title: d.title }).id;
  const repo = beadsOf(pid).github_repo;
  let gh = 70 + issuesOf(pid).size;
  for (const d of list) {
    const i = issuesOf(pid).get(created[d.key]);
    Object.assign(i, {
      description: d.description, priority: d.priority, issue_type: d.issue_type, labels: [...d.labels], created_by: "coordinator",
      blocked_by: d.blocked_by.map((x) => created[x] ?? x),
      related: draft.related.map((id) => ({ id, kind: "relates-to" })), external_ref: repo ? `https://github.com/${repo}/issues/${gh++}` : null,
      comments: [{ author: "coordinator", text: "Drafted in the New issue chat.", created_at: now() }],
    });
  }
  Object.assign(draft, { state: "accepted", created, issues: list, waiting: false, updated_at: now() });
  emit("issue_draft.updated", structuredClone(draft), pid);
  beadsChanged(pid, "create", created[list[0]?.key] ?? null);
  if (b.start_worker && list[0]) {
    setTimeout(() => chat(pid, "assistant", `Starting a worker on ${created[list[0].key]}: ${list[0].title}.`), QUIET ? 50 : 700);
  }
}

// ---------------------------------------------------------------- seed

function seed() {
  addAccount({ id: "default-claude-code", harness: "claude-code", label: "Default", config_dir: "/home/demo/.claude", default: true,
    quota: quotaReading(62, "max") });
  addAccount({ id: "default-codex", harness: "codex", label: "Default", config_dir: "/home/demo/.codex", default: true,
    health: { state: "configured", detail: "found /home/demo/.codex/auth.json" }, quota: quotaReading(88, "plus") });

  const quark = addProject({
    id: "quark", name: "Quark MVP", goal: "Ship Phase 1: one Project end to end, from creation to a reviewed PR.",
    repos: [{ url: "https://github.com/quark-systems/quark.git" }, { url: "https://github.com/quark-systems/firstmate.git" }],
    agent_config: { harness: "claude-code", model: null, effort: "high" },
    created_at: minutesAgo(600), updated_at: minutesAgo(3),
  });
  // One rule with a second candidate that cannot run here, so the editor and the test have something to show.
  rulesOf(quark).rules.push({
    name: "trivial-edit", when: TRIVIAL_WHEN, select: null,
    candidates: [{ harness: "claude-code", model: "claude-sonnet-5", effort: "low", pool: null }, { harness: "pi", model: "openai/gpt-5.5", effort: null, pool: null }],
  });
  const site = addProject({
    id: "website", name: "Website refresh", goal: "Move the marketing site to the new design system.",
    repos: [{ url: "https://github.com/quark-systems/website.git" }], agent_config: { harness: "codex" }, created_at: minutesAgo(3000), updated_at: minutesAgo(90),
  });

  const T = (p, title, state, extra = {}) => addTask(p.id, { title, state, ...extra }, { silent: true });
  const a = T(quark, "Event stream: resync slow clients from the store", "running", { diff: DIFFS[0], harness: "claude-code", model: "claude-sonnet-5-5", branch: "claude/event-stream-resync", updated_at: minutesAgo(1) });
  const b = T(quark, "Terminal sessions over tmux control mode", "running", { diff: DIFFS[1], harness: "codex", model: "gpt-5-codex", branch: "codex/tmux-control-mode", updated_at: minutesAgo(4) });
  T(quark, "Decision records carry who answered", "needs_decision", { diff: DIFFS[2], harness: "cursor", state_note: "Asked: keep answer history per decision?", updated_at: minutesAgo(12) });
  T(quark, "Harness registry trait", "queued", { updated_at: minutesAgo(20) });
  T(quark, "OpenAPI check in CI", "in_review", { harness: "gemini", pull_request_url: "https://github.com/quark-systems/quark/pull/2", updated_at: minutesAgo(40) });
  T(quark, "Daemon skeleton", "done", { updated_at: minutesAgo(300) });
  T(site, "Pricing page on the new grid", "running", { diff: DIFFS[3], harness: "codex", updated_at: minutesAgo(8) });
  T(site, "Changelog feed", "failed", { state_note: "Build failed: missing RSS dependency", updated_at: minutesAgo(70) });

  // Every spawned task has its dispatch record, finished ones included.
  recordDispatch(a, "classifier", { silent: true, ts: minutesAgo(31), model: "claude-sonnet-5", effort: "high" });
  recordDispatch(b, "coordinator", { silent: true, ts: minutesAgo(31) });
  for (const t of tasks.values()) {
    if (t.state !== "queued" && !dispatches.has(t.id)) recordDispatch(t, "coordinator", { silent: true, ts: t.updated_at });
  }

  const D = (id, p, question, extra = {}) => decisions.set(id, {
    id, number: [...decisions.values()].filter((d) => d.project_id === p.id).length + 1, project_id: p.id, task_id: null, question,
    state: "open", brief: {}, answer: null, answered_by: null, answered_at: null, answered_via: null, answer_why: null,
    outcome: null, acted_at: null, rule_id: null, made_rule_id: null, ...extra,
  });
  D("d-1", quark, "Keep the full answer history per decision, or only the latest answer?", {
    task_id: [...tasks.values()].find((t) => t.state === "needs_decision").id, opened_at: minutesAgo(12),
  });
  D("d-2", site, "Launch the new design behind a flag, or replace the old site directly?", { opened_at: minutesAgo(6) });
  D("d-3", quark, "Use SQLite WAL mode for the projection store?", {
    opened_at: minutesAgo(200), state: "answered", answer: "Yes, WAL with a busy timeout.", answered_by: "matt", answered_at: minutesAgo(180),
  });


  // PR center (quark#16): one PR per state, across both Projects.
  const PR = (id, project, task, extra) => {
    const repo = project.repos[0].url.replace(/^https:\/\/github.com\//, "").replace(/\.git$/, "");
    const pr = {
      id, project_id: project.id, task_id: task?.id ?? null, provider: "github", repo, author: "quark-bot", state: "open",
      head_sha: "9f1e2d3c4b", mergeable: "mergeable", checks_state: "passing", review_decision: "review_required",
      checks: [], reviews: [], evidence: null, opened_at: minutesAgo(120), updated_at: minutesAgo(10),
      merged_at: null, closed_at: null, synced_at: minutesAgo(1), sync_error: null, ...extra,
    };
    pr.url = `https://github.com/${repo}/pull/${pr.number}`;
    pullDiffs.set(id, pr.diff ?? "");
    delete pr.diff;
    const files = filesOf(pullDiffs.get(id));
    Object.assign(pr, {
      additions: files.reduce((n, f) => n + f.additions, 0), deletions: files.reduce((n, f) => n + f.deletions, 0), changed_files: files.length,
    });
    pulls.set(id, pr);
  };
  // ADR-15 evidence: repo checks, Playwright journeys (with traces and screenshots), holdout tests.
  const art = (id, kind, path, content_type, body) => {
    artifacts.set(id, { content_type, body });
    return { id, kind, name: path, content_type, size_bytes: Buffer.byteLength(body), url: `/v1/pull-requests/{pr}/evidence/artifacts/${id}` };
  };
  const shot = (label, color) => `<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="800"><rect width="100%" height="100%" fill="#0b0c10"/>` +
    `<rect x="0" y="0" width="244" height="800" fill="#111218"/><rect x="260" y="20" width="1000" height="40" rx="6" fill="#171920"/>` +
    `<text x="640" y="420" fill="${color}" font-family="sans-serif" font-size="40" text-anchor="middle">${label}</text></svg>`;
  const evidence = (prId, failing) => {
    const fix = (a) => ({ ...a, url: a.url.replace("{pr}", prId) });
    const journeys = [
      { name: "creates a project and lands on its board", state: "passed", duration_ms: 1320, message: null,
        artifacts: [fix(art(`${prId}-a1`, "screenshot", "journeys/create-project/final.png", "image/svg+xml", shot("Board: Parser rewrite", "#7bd88f")))] },
      failing
        ? { name: "worker view: terminal resize keeps the prompt", state: "failed", duration_ms: 30000,
          message: "Error: expect(locator).toContainText(expected) failed\n\nLocator: getByTestId('terminal').locator('.xterm-rows')\nExpected substring: \"$ \"\nTimeout: 30000ms",
          artifacts: [
            fix(art(`${prId}-a2`, "screenshot", "journeys/terminal-resize/failure.png", "image/svg+xml", shot("Terminal pane is blank after resize", "#f2777a"))),
            fix(art(`${prId}-a3`, "trace", "journeys/terminal-resize/trace.zip", "application/zip", "PK\x05\x06" + "\0".repeat(18))),
            fix(art(`${prId}-a4`, "log", "journeys/terminal-resize/console.log", "text/plain", "[quarkd] resize 120x36 -> 80x24\n[app] terminal snapshot timed out\n")),
          ] }
        : { name: "worker view: terminal resize keeps the prompt", state: "passed", duration_ms: 2210, message: null, artifacts: [] },
    ];
    const holdout = ["steer-then-cancel", "decision-roundtrip", "pr-merge-refused"].map((name, i) =>
      ({ name, state: failing && i === 2 ? "failed" : "passed", duration_ms: null, message: null, artifacts: [] }));
    const st = (cases) => (cases.some((c) => c.state === "failed") ? "failed" : "passed");
    return {
      head_sha: "9f1e2d3c4b", state: failing ? "failed" : "passed", stale: false, started_at: minutesAgo(20), completed_at: minutesAgo(12),
      gates: [
        { kind: "checks", state: failing ? "failed" : "passed", summary: null, started_at: minutesAgo(20), completed_at: minutesAgo(18),
          cases: [{ name: "CI / rust", state: "passed", artifacts: [] }, { name: "CI / desktop-app", state: failing ? "failed" : "passed", artifacts: [] }] },
        { kind: "journeys", state: st(journeys), summary: `${journeys.filter((c) => c.state === "passed").length} of ${journeys.length} journeys passed`,
          started_at: minutesAgo(18), completed_at: minutesAgo(15), cases: journeys },
        { kind: "holdout", state: st(holdout), summary: null, started_at: minutesAgo(15), completed_at: minutesAgo(12), cases: holdout },
      ],
    };
  };
  const ok = (name) => ({ name, status: "success", conclusion: null, details_url: `https://github.com/quark-systems/quark/actions/runs/1#${name}`, started_at: minutesAgo(50), completed_at: minutesAgo(45) });
  const inReview = [...tasks.values()].find((t) => t.title === "OpenAPI check in CI");
  PR("pr-1", quark, inReview, {
    number: 2, title: "OpenAPI check in CI", head_ref: "quark/openapi-check", base_ref: "main", diff: DIFFS[0],
    checks: [ok("CI / rust"), ok("CI / desktop-app")], updated_at: minutesAgo(40), evidence: evidence("pr-1", false),
  });
  PR("pr-2", quark, b, {
    number: 15, title: "Terminal sessions over tmux control mode", head_ref: "quark/tmux-control", base_ref: "main", diff: DIFFS[1],
    checks: [ok("CI / rust"), { name: "CI / desktop-app", status: "failure", conclusion: "timed_out", details_url: null }], checks_state: "failing",
    review_decision: "changes_requested", updated_at: minutesAgo(6), evidence: evidence("pr-2", true),
    reviews: [{ id: "r-1", author: "mattsanchez", state: "changes_requested", body: "Resize should debounce; the pane flickers.", submitted_at: minutesAgo(15), commit: "9f1e2d3c4b" }],
  });
  PR("pr-3", site, null, {
    number: 41, title: "Pricing page on the new grid", head_ref: "quark/pricing-grid", base_ref: "main", diff: DIFFS[3],
    state: "draft", checks: [{ name: "build", status: "pending", started_at: minutesAgo(2) }], checks_state: "pending", review_decision: "none",
  });
  PR("pr-4", quark, null, {
    number: 1, title: "Daemon skeleton", head_ref: "quark/daemon", base_ref: "main", diff: DIFFS[2], state: "merged",
    checks: [ok("CI / rust")], review_decision: "approved", merged_at: minutesAgo(300), closed_at: minutesAgo(300), updated_at: minutesAgo(300),
    reviews: [{ id: "r-2", author: "mattsanchez", state: "approved", body: "", submitted_at: minutesAgo(310), commit: "9f1e2d3c4b" }],
  });

  for (const t of [a, b]) {
    const at = (m) => ({ ts: minutesAgo(m) });
    transcript(t, "user", `Task contract: ${t.title}. Worktree ready on branch \`quark/${t.id}\`.`, at(30));
    transcript(t, "assistant", "I'll start by reading the current implementation and its tests.", at(29));
    transcript(t, "tool_call", "cargo test -p quarkd", { ...at(28), tool_name: "bash", tool_call_id: "c1", tool: { kind: "shell", title: "Run cargo test -p quarkd", command: "cargo test -p quarkd" } });
    transcript(t, "tool_result", "running 14 tests\n..............\ntest result: ok. 14 passed; 0 failed", { ...at(28), tool_name: "bash", tool_call_id: "c1" });
    transcript(t, "assistant", "The tests pass on the base. Next I'll write the failing test for the new behaviour, then the change.", at(20));
    transcript(t, "user", "Please keep the change inside the events module.", at(19));
    transcript(t, "thinking", "The resync path lives in events.rs; the test goes beside it.", at(19));
    transcript(t, "tool_call", '{"file_path":"crates/quarkd/src/events.rs"}', { ...at(18), tool_name: "Read", tool_call_id: "c2", tool: { kind: "read", title: "Read crates/quarkd/src/events.rs", path: "crates/quarkd/src/events.rs" } });
    transcript(t, "tool_result", "pub fn resync(…)", { ...at(18), tool_call_id: "c2" });
    transcript(t, "tool_call", "{}", { ...at(16), tool_name: "Edit", tool_call_id: "c3", tool: {
      kind: "edit", title: "Edit crates/quarkd/src/events.rs", path: "crates/quarkd/src/events.rs", additions: 3, deletions: 1,
      diff: [{ kind: "context", text: "    let after = cursor.seq;" }, { kind: "del", text: "    store.events_after(after)" },
        { kind: "add", text: "    match store.events_after(after) {" }, { kind: "add", text: "        Err(e) if e.is_lagged() => resync(store)," },
        { kind: "add", text: "        r => r," }] } });
    transcript(t, "tool_result", "ok", { ...at(16), tool_call_id: "c3" });
    transcript(t, "tool_call", '{"command":"cargo test -p quarkd events"}', { ...at(15), tool_name: "Bash", tool_call_id: "c4", tool: { kind: "shell", title: "Run cargo test -p quarkd events", command: "cargo test -p quarkd events" } });
    transcript(t, "tool_result", "test events::lagged_receiver_resyncs ... FAILED", { ...at(14), tool_call_id: "c4", is_error: true });
    transcript(t, "tool_call", '{"command":"cargo test -p quarkd events"}', { ...at(13), tool_name: "Bash", tool_call_id: "c5", tool: { kind: "shell", title: "Run cargo test -p quarkd events", command: "cargo test -p quarkd events" } });
    transcript(t, "tool_result", "test result: ok. 6 passed", { ...at(12), tool_call_id: "c5" });
    transcript(t, "assistant", "Kept it in `events.rs`. The lagged receiver now resyncs from the store, and the new test covers it.", at(12));
    const term = terms.get(t.id);
    term.screen = `\x1b[1;35m${t.harness}\x1b[0m working on \x1b[1m${t.title}\x1b[0m\r\n\r\n` +
      `\x1b[32m✓\x1b[0m read the task contract\r\n\x1b[32m✓\x1b[0m cargo test -p quarkd (14 passed)\r\n` +
      `\x1b[33m…\x1b[0m writing the failing test\r\n\r\n$ `;
  }

  // Project memory (quark#29): learnings from finished tasks awaiting review, and one accepted entry.
  const done = [...tasks.values()].find((t) => t.title === "Daemon skeleton");
  const MP = (id, p, task, text, extra = {}) => memoryProposals.set(id, {
    id, project_id: p.id, text, source: "worker", state: "proposed", proposed_at: minutesAgo(30), decided_at: null, decided_by: null, entry: null,
    evidence: { task_id: task?.id ?? null, task_title: task?.title ?? null, pull_request_url: task?.pull_request_url ?? null, files: [] }, ...extra,
  });
  MP("mp-1", quark, inReview, "Regenerate api/openapi.json with `cargo run -p quarkd -- openapi` whenever a route or shape changes; CI compares it.", {
    evidence: { task_id: inReview.id, task_title: inReview.title, pull_request_url: inReview.pull_request_url, files: ["api/openapi.json", "crates/quarkd/tests/api.rs"] },
  });
  MP("mp-2", quark, done, "SQLite runs in WAL mode with a 5s busy timeout; keep long work outside store transactions.", { source: "coordinator", proposed_at: minutesAgo(25) });
  addMemoryEntry({
    id: "2026-10-01-one-task-one-pr-against-main", project_id: quark.id,
    text: "One task, one PR against main; never stack PRs.", source: "coordinator", date: minutesAgo(900), accepted_at: minutesAgo(880),
    accepted_by: "matt", evidence: { task_id: done.id, task_title: done.title, pull_request_url: null, files: [] },
  });

  chat(quark.id, "user", "What's left before Phase 1 can start?", { ts: minutesAgo(200) });
  chat(quark.id, "assistant", "The spec is settled. Once you say go I'll split it into tasks and start the first workers.", { ts: minutesAgo(199) });
  chat(quark.id, "user", "Split Phase 1 into tasks and start the event stream and terminal work.", { ts: minutesAgo(35) });
  chat(quark.id, "thinking", "Two independent workstreams; dispatch both. Decision records need Matt's call first.", { ts: minutesAgo(35) });
  const spawnCall = (id, task, at) => {
    const command = `bin/fm-spawn.sh ${task} projects/quark --mode no-mistakes --yolo off`;
    chat(quark.id, "tool_call", JSON.stringify({ command }), { ts: minutesAgo(at), tool_name: "Bash", tool_call_id: id, tool: { kind: "shell", title: `Run ${command}`, command } });
    chat(quark.id, "tool_result", `spawned ${task}`, { ts: minutesAgo(at), tool_call_id: id });
  };
  spawnCall("k1", "event-stream", 35);
  spawnCall("k2", "terminals", 34);
  const hold = 'bin/fm-captain-hold.sh hold decision-records --reason "Keep decision records in docs/adr or in the wiki?"';
  chat(quark.id, "tool_call", JSON.stringify({ command: hold }), { ts: minutesAgo(34), tool_name: "Bash", tool_call_id: "k3", tool: { kind: "shell", title: `Run ${hold}`, command: hold } });
  chat(quark.id, "tool_result", "held", { ts: minutesAgo(34), tool_call_id: "k3" });
  chat(quark.id, "assistant", "Dispatched two workers:\n\n- **Event stream**: resync slow clients from the store (Claude Code)\n- **Terminal sessions** over tmux control mode (Codex)\n\nThe decision-records task is waiting on a question for you.", { ts: minutesAgo(34) });

  // The website has no Beads database yet, so its memory is files under memory/ in its repo.
  MP("mp-3", site, null, "The marketing site's images go through the CDN's resize endpoint; never commit originals over 500 KB.", {
    source: "coordinator", proposed_at: minutesAgo(50),
    evidence: { task_id: null, task_title: "Hero image audit", pull_request_url: "https://github.com/quark-systems/website/pull/14", files: ["public/images/"] },
  });
  addMemoryEntry({
    id: "2026-09-28-the-design-tokens-live-in-tokens-css", project_id: site.id,
    text: "The design tokens live in tokens.css; components never use raw hex.", source: "worker", date: minutesAgo(9000), accepted_at: minutesAgo(8900),
    accepted_by: "matt", evidence: { task_id: null, task_title: "Move buttons to the new tokens", pull_request_url: null, files: ["src/styles/tokens.css"] },
  });
  beadsOf(site.id);

  // Quark's Beads database (app/src/api/beads.ts), mirrored with GitHub Issues.
  Object.assign(beadsOf(quark.id), {
    state: "ready", dir: "/home/mock/.quark/projects/quark/projects/quark", prefix: "qk", remote: "https://github.com/quark-systems/quark.git",
    github_repo: "quark-systems/quark", last_sync: { at: minutesAgo(1), ok: true, pulled: 2, pushed: 1, message: "pulled 2, pushed 1" },
  });
  const gh = (n) => `https://github.com/quark-systems/quark/issues/${n}`;
  const I = (id, title, extra = {}) => addIssue(quark.id, { id, title, external_ref: gh(Number(id.replace(/\D/g, "")) + 17), created_by: "coordinator", ...extra });
  I("qk-30", "Event stream resyncs slow clients from the store", { issue_type: "bug", priority: 1, status: "closed", created_at: minutesAgo(9000), closed_at: minutesAgo(600), updated_at: minutesAgo(600) });
  I("qk-33", "OpenAPI check in CI", { issue_type: "task", priority: 2, status: "closed", created_at: minutesAgo(8000), closed_at: minutesAgo(300), updated_at: minutesAgo(300) });
  I("qk-35", "Keep decision records in docs/adr?", { issue_type: "decision", priority: 2, status: "closed", external_ref: null, created_at: minutesAgo(7000), closed_at: minutesAgo(2000), updated_at: minutesAgo(2000),
    description: "Answered: docs/adr, one file per decision, so the history is in git." });
  I("qk-37", "Serve the web build on the tailnet", { issue_type: "feature", priority: 2, created_at: minutesAgo(5000),
    description: "Let the phone open the app over Tailscale: quarkd serves the web build and accepts the tailnet origin." });
  I("qk-39", "Transcript search", { issue_type: "feature", priority: 2, status: "in_progress", assignee: "transcript-search", created_at: minutesAgo(4000),
    description: "Search every worker and coordinator transcript in a Project, with the turn each hit is in." });
  I("qk-41", "Next-attention shortcut in the app", { issue_type: "feature", priority: 1, labels: ["attention"], created_at: minutesAgo(3000),
    description: "One key that jumps to the oldest thing that needs you: a decision, a red PR or a stopped worker. From the AgentsInTheCloud analysis." });
  I("qk-42", "Treehouse real-binary test", { issue_type: "task", priority: 3, labels: ["needs-mac"], created_by: "matt", created_at: minutesAgo(2900),
    description: "Run the slice tests against the real treehouse binary. Needs a Mac." });
  I("qk-d14", "Switch slice 2 to native?", { issue_type: "decision", priority: 1, external_ref: null, created_at: minutesAgo(30),
    description: "Slice 2 has run in shadow for seven days with no differences. Switch it to native, or keep comparing?" });
  I("qk-43", "Slice 2 switch PR", { issue_type: "task", priority: 1, status: "in_progress", assignee: "slice-2-switch", created_at: minutesAgo(28),
    description: "Flip slice 2 to native behind the decision, with the shadow comparison kept for a day." });
  I("qk-44", "Start the slice 3 shadow window", { issue_type: "feature", priority: 1, blocked_by: ["qk-d14", "qk-43"], created_at: minutesAgo(2200),
    description: "Turn on shadow comparison for slice 3 (sessions) and start the 7-day window.", related: [{ id: "qk-30", kind: "discovered-from" }],
    comments: [{ author: "coordinator", text: "Filed from the parallel tracks plan.", created_at: minutesAgo(2200) }, { author: "coordinator", text: "Linked to decision qk-d14.", created_at: minutesAgo(4) }] });
  I("qk-45", "Start the slice 4 shadow window", { issue_type: "feature", priority: 2, blocked_by: ["qk-44"], created_at: minutesAgo(2100),
    description: "Shadow comparison for slice 4 (worktrees), after slice 3's window." });
  I("qk-46", "Glossary and copy rules for the app", { issue_type: "chore", priority: 2, created_by: "attention-model", created_at: minutesAgo(60),
    description: "One page of the words the app uses and the ones it avoids, so every screen reads the same." });
  beadsMemories.set(quark.id, [
    { key: "one-task-one-pr", value: "One task, one PR against main; never stack PRs.", evidence: null, source: "coordinator", accepted_at: minutesAgo(880), accepted_by: "matt" },
    { key: "e2e-serially-on-ci", value: "Run the app's e2e suite serially on CI; the demo daemon is shared between tests.", source: "worker", accepted_at: minutesAgo(400), accepted_by: "matt",
      evidence: { task_id: null, task_title: "Flaky e2e on CI", pull_request_url: "https://github.com/quark-systems/quark/pull/94", files: ["app/playwright.config.ts"] } },
  ]);
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
    recordDispatch(t, "coordinator");
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
  "access-control-allow-methods": "GET, POST, PUT, PATCH, DELETE, OPTIONS",
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

// Harnesses that take an account directory, like quarkd's harness registry.
const ACCOUNT_ENV = { "claude-code": "CLAUDE_CONFIG_DIR", codex: "CODEX_HOME", pi: "PI_CODING_AGENT_DIR" };
const QUOTA_PROVIDER = { "claude-code": "claude", codex: "codex" };

const quotaReading = (remaining, plan, minutesToReset = 140) => ({
  state: "known", remaining_percent: remaining, plan, detail: null, checked_at: now(),
  windows: [
    { id: "five_hour", label: "session", percent_remaining: remaining, resets_at: new Date(Date.now() + minutesToReset * 60_000).toISOString() },
    { id: "weekly", label: "week", percent_remaining: Math.min(100, remaining + 21), resets_at: new Date(Date.now() + 5 * 86_400_000).toISOString() },
  ],
});

function addAccount(a) {
  const account = {
    id: a.id ?? nextId("acc"), harness: a.harness, label: a.label, config_dir: a.config_dir, default: !!a.default,
    pools: [...new Set(a.pools ?? [])].sort(), health: a.health ?? { state: "configured", detail: `found ${a.config_dir}/.credentials.json` },
    quota: a.quota ?? (QUOTA_PROVIDER[a.harness]
      ? { state: "pending", remaining_percent: null, plan: null, windows: [], detail: null, checked_at: null }
      : { state: "unsupported", remaining_percent: null, plan: null, windows: [], detail: "quota is read per account for Claude Code and Codex only", checked_at: null }),
    active_tasks: 0, launchable: a.launchable ?? (a.default || a.harness === "claude-code" || a.harness === "codex"), created_at: a.default ? null : now(),
  };
  accounts.set(account.id, account);
  return account;
}

// Like quarkd's one quota-axi --profile-only read per account, then account.quota_changed.
function readQuota(account, reading) {
  account.quota = reading;
  emit("account.quota_changed", { account_id: account.id, harness: account.harness, quota: reading });
}

function accountList() {
  const busy = {};
  for (const t of tasks.values()) if (t.account_id && t.state !== "done" && t.state !== "failed") busy[t.account_id] = (busy[t.account_id] ?? 0) + 1;
  // Grouped by harness in the registry's order, default first, like quarkd.
  const order = Object.keys(ACCOUNT_ENV);
  return [...accounts.values()]
    .map((a) => ({ ...a, active_tasks: busy[a.id] ?? 0 }))
    .sort((a, b) => order.indexOf(a.harness) - order.indexOf(b.harness) || Number(b.default) - Number(a.default));
}

const HARNESSES = [
  { id: "claude-code", name: "Claude Code", roles: ["coordinator", "worker"], efforts: ["low", "medium", "high", "xhigh", "max"],
    install: { installed: true, version: "2.1.0", install_hint: "npm install -g @anthropic-ai/claude-code" },
    models: { selection: "free_form", discovery: "claude --help" } },
  { id: "codex", name: "Codex", roles: ["coordinator", "worker"], efforts: ["low", "medium", "high"],
    install: { installed: true, version: "0.50.0", install_hint: "npm install -g @openai/codex" },
    models: { selection: "free_form", discovery: null } },
  { id: "pi", name: "Pi", roles: ["coordinator", "worker"], efforts: [],
    install: { installed: false, install_hint: "npm install -g @mariozechner/pi-coding-agent" },
    models: { selection: "provider_qualified", discovery: "pi --list-models" } },
  { id: "bob", name: "IBM Bob", roles: ["worker"], efforts: [],
    install: { installed: true, version: "1.0", install_hint: "" }, models: { selection: "automatic" } },
].map((h) => ({
  auth: { state: "configured", detail: null }, supervision: { confidence: "high", source: "hooks" }, transcript: true,
  account_env: ACCOUNT_ENV[h.id] ?? null, ...h,
}));

function provision(proj) {
  proj.status = "provisioning"; proj.status_detail = "Cloning repositories"; proj.updated_at = now();
  emit("project.updated", { ...proj }, proj.id);
  setTimeout(() => {
    proj.status = "ready"; proj.status_detail = null; proj.updated_at = now();
    emit("project.updated", { ...proj }, proj.id);
    chat(proj.id, "assistant", `Workspace ready${proj.repos.length ? ` with ${proj.repos.map((r) => r.name).join(", ")}` : ""}. What should we work on first?`);
  }, QUIET ? 200 : 1500);
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
    if (b.repos?.length && !b.agent_config) return invalid(res, "agent_config is required with repos");
    const proj = addProject({ ...b, name: b.name.trim() });
    if (proj.repos.length) provision(proj);
    return send(res, 201, proj);
  }
  if ((r = m(/^\/v1\/projects\/([^/:]+)(:provision)?$/))) {
    const proj = projects.get(decodeURIComponent(r[1]));
    if (!proj) return notFound(res);
    if (!r[2] && req.method === "GET") return send(res, 200, proj);
    if (!r[2] && req.method === "PATCH") {
      const b = await readJson(req);
      if (!b || (b.standing_approval !== undefined && typeof b.standing_approval !== "boolean")) return invalid(res, "standing_approval must be a boolean");
      if (b.standing_approval !== undefined) proj.standing_approval = b.standing_approval;
      proj.updated_at = now();
      emit("project.updated", { ...proj }, proj.id);
      return send(res, 200, proj);
    }
    if (r[2] && req.method === "POST") {
      if (proj.status !== "failed") return send(res, 409, { error: { code: "conflict", message: "project is not failed" } });
      provision(proj);
      return send(res, 202);
    }
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/automation$/)) && req.method === "GET") {
    const proj = projects.get(decodeURIComponent(r[1]));
    if (!proj) return notFound(res);
    return send(res, 200, automationOf(proj));
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/inbox$/)) && req.method === "POST") {
    const proj = projects.get(decodeURIComponent(r[1]));
    if (!proj) return notFound(res);
    const b = await readJson(req);
    if (!b || typeof b.body !== "string" || !b.body.trim()) return invalid(res, "a note needs a body");
    const a = automationState(proj);
    a.inbox.push({ id: `${Date.now()}-${a.inbox.length}`, channel: "inbox", from: "app", body: b.body.trim(), at: now(), task_id: null });
    return send(res, 202);
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/triggers\/([^/]+)$/)) && (req.method === "PUT" || req.method === "DELETE")) {
    const proj = projects.get(decodeURIComponent(r[1]));
    if (!proj) return notFound(res);
    const a = automationState(proj), id = decodeURIComponent(r[2]);
    if (req.method === "DELETE") {
      if (!a.rules.delete(id)) return send(res, 404, { error: { code: "not_found", message: `no rule ${id}` } });
      return send(res, 204);
    }
    const b = await readJson(req);
    const problem = ruleProblem(id, b);
    if (problem) return invalid(res, problem);
    const was = a.rules.get(id);
    const rule = { id, description: b.description ?? "", enabled: b.enabled ?? true, once: !!b.once, when: b.when, then: b.then, defined_at: was?.defined_at ?? now(), fires: was?.fires ?? 0 };
    a.rules.set(id, rule);
    return send(res, 200, rule);
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/away\/policy$/)) && req.method === "PUT") {
    const proj = projects.get(decodeURIComponent(r[1]));
    if (!proj) return notFound(res);
    const b = await readJson(req);
    if (!b || !(b.digest_secs > 0)) return invalid(res, "digest interval must be at least one second");
    const overrides = new Map();
    for (const c of b.routes ?? []) {
      if (!POSTURES.includes(c.posture) || !ROUTE_USER[c.occasion]) return invalid(res, "unknown posture or occasion");
      const d = defaultRoute(c.posture, c.occasion);
      if (d.wake !== c.wake || d.user !== c.user) overrides.set(`${c.posture}/${c.occasion}`, { wake: !!c.wake, user: c.user });
    }
    for (const posture of POSTURES) for (const occasion of ["decision", "failed", "trigger_failed"]) {
      const c = overrides.get(`${posture}/${occasion}`) ?? defaultRoute(posture, occasion);
      if (!c.wake && c.user === "silent") return invalid(res, `while ${posture}, a ${occasion.replace("_", " ")} would reach no one`);
    }
    const a = automationState(proj);
    a.digest_secs = b.digest_secs; a.overrides = overrides;
    return send(res, 200, awayOf(a));
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/metrics$/)) && req.method === "GET") {
    const proj = projects.get(decodeURIComponent(r[1]));
    if (!proj) return notFound(res);
    return send(res, 200, metricsOf(proj, Number(url.searchParams.get("days")) || 7));
  }
  if (url.pathname === "/v1/hosts" && req.method === "GET") {
    return send(res, 200, hostsOf(Math.min(48, Math.max(1, Number(url.searchParams.get("hours")) || 6))));
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/hosts$/)) && req.method === "GET") {
    const proj = projects.get(decodeURIComponent(r[1]));
    if (!proj) return notFound(res);
    return send(res, 200, projectHostsOf(proj, Math.min(48, Math.max(1, Number(url.searchParams.get("hours")) || 6))));
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/overview$/)) && req.method === "GET") {
    const proj = projects.get(decodeURIComponent(r[1]));
    if (!proj) return notFound(res);
    const since = url.searchParams.get("since");
    return send(res, 200, overviewOf(proj, since == null ? null : Number(since)));
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/settings$/)) && (req.method === "GET" || req.method === "PATCH")) {
    const proj = projects.get(decodeURIComponent(r[1]));
    if (!proj) return notFound(res);
    if (req.method === "GET") return send(res, 200, settingsOf(proj));
    const b = await readJson(req);
    if (!b || (b.standing_approval !== undefined && typeof b.standing_approval !== "boolean")) return invalid(res, "standing_approval must be a boolean");
    const v = verificationOf(proj);
    const holdout = b.holdout ?? [];
    if (holdout.length) {
      if (b.revision != null && b.revision !== v.revision) {
        return send(res, 409, { error: { code: "settings_changed", message: "project.yaml changed on main since these settings were loaded; load them again and repeat the change" } });
      }
      const unknown = holdout.find((c) => !v.sources.some((s) => s.source === c.source));
      if (unknown) return send(res, 400, { error: { code: "settings_invalid", message: `project.yaml has no source named "${unknown.source}"` } });
      let changed = false;
      for (const c of holdout) {
        const s = v.sources.find((x) => x.source === c.source);
        if (s.holdout.enabled !== c.enabled) { s.holdout.enabled = c.enabled; changed = true; }
      }
      if (changed) v.revision = hex40();
    }
    if (b.standing_approval !== undefined && b.standing_approval !== proj.standing_approval) {
      proj.standing_approval = b.standing_approval;
      proj.updated_at = now();
      emit("project.updated", { ...proj }, proj.id);
    }
    return send(res, 200, settingsOf(proj));
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/dispatch:test$/)) && req.method === "POST") {
    const proj = projects.get(decodeURIComponent(r[1]));
    if (!proj) return notFound(res);
    const b = await readJson(req);
    const description = b?.description;
    if (typeof description !== "string" || !description.trim()) return invalid(res, "description is empty");
    const draft = b.draft == null ? null : compileDraft(b.draft);
    if (draft?.error) return send(res, 400, { error: { code: "dispatch_invalid", message: draft.error } });
    return send(res, 200, testDispatch(proj, description.trim(), draft));
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/dispatch$/)) && (req.method === "GET" || req.method === "PUT")) {
    const proj = projects.get(decodeURIComponent(r[1]));
    if (!proj) return notFound(res);
    const current = rulesOf(proj);
    if (req.method === "GET") return send(res, 200, current);
    const b = await readJson(req);
    if (b?.revision != null && b.revision !== current.revision) {
      return send(res, 409, { error: { code: "dispatch_changed", message: "dispatch.yaml changed on main since these rules were loaded; load them again and repeat the edit" } });
    }
    const next = compileDraft(b);
    if (next.error) return send(res, 400, { error: { code: "dispatch_invalid", message: next.error } });
    // Like quarkd: rules that are what main has already make no commit.
    if (!sameRules(next, current)) dispatchRules.set(proj.id, { ...current, ...next, revision: hex40(), commit: hex40() });
    return send(res, 200, dispatchRules.get(proj.id));
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/tasks$/)) && req.method === "GET") {
    const id = decodeURIComponent(r[1]);
    if (!projects.has(id)) return notFound(res);
    return send(res, 200, [...tasks.values()].filter((t) => t.project_id === id));
  }
  if (p === "/v1/decisions" && req.method === "GET") {
    const st = url.searchParams.get("state");
    const pid = url.searchParams.get("project_id");
    return send(res, 200, [...decisions.values()].filter((d) => (!st || d.state === st) && (!pid || d.project_id === pid)));
  }
  if ((r = m(/^\/v1\/decisions\/([^/:]+)$/)) && req.method === "GET") {
    const d = decisions.get(decodeURIComponent(r[1]));
    return d ? send(res, 200, d) : notFound(res);
  }
  if (p === "/v1/rules" && req.method === "GET") {
    const all = url.searchParams.get("include_revoked") === "true";
    const pid = url.searchParams.get("project_id");
    const merge = [...projects.values()].filter((x) => x.standing_approval).map((x) => ({
      id: `merge-approval:${x.id}`, project_id: x.id, kind: "merge_approval", text: "Merge green pull requests without asking.",
      decision_id: null, created_by: null, created_at: null, revoked_at: null, revoked_by: null, applied: 0,
    }));
    return send(res, 200, [...rules.values(), ...merge].map(withApplied)
      .filter((x) => (all || !x.revoked_at) && (!pid || x.project_id === pid)));
  }
  if ((r = m(/^\/v1\/rules\/([^/]+):revoke$/)) && req.method === "POST") {
    const id = decodeURIComponent(r[1]);
    if (id.startsWith("merge-approval:")) {
      const proj = projects.get(id.slice("merge-approval:".length));
      if (!proj?.standing_approval) return notFound(res);
      proj.standing_approval = false;
      emit("project.updated", { ...proj }, proj.id);
      return send(res, 200, { id, project_id: proj.id, kind: "merge_approval", text: "Merge green pull requests without asking.", revoked_at: now(), revoked_by: "mock-user", applied: 0 });
    }
    const rule = rules.get(id);
    if (!rule) return notFound(res);
    if (!rule.revoked_at) Object.assign(rule, { revoked_at: now(), revoked_by: "mock-user" });
    emit("rule.updated", withApplied(rule), rule.project_id);
    return send(res, 200, withApplied(rule));
  }
  if ((r = m(/^\/v1\/decisions\/([^/:]+):answer$/)) && req.method === "POST") {
    const d = decisions.get(decodeURIComponent(r[1]));
    if (!d) return notFound(res);
    if (d.state !== "open") return send(res, 409, { error: { code: "already_answered", message: "the decision is already answered" } });
    const b = await readJson(req);
    const answer = typeof b?.answer === "string" ? b.answer.trim() : "";
    if (!answer) return invalid(res, "answer is required");
    const via = b.via ?? "app";
    if (!["app", "phone", "chat"].includes(via)) return invalid(res, "via must be app, phone or chat");
    Object.assign(d, { state: "answered", answer, answered_by: b.answered_by?.trim() || "mock-user", answered_at: now(),
      answered_via: via, answer_why: b.why?.trim() || null });
    if (b.make_rule?.trim()) {
      const rule = { id: `rule-${rules.size + 1}`, project_id: d.project_id, kind: "answer", text: b.make_rule.trim(), decision_id: d.id,
        created_by: d.answered_by, created_at: now(), revoked_at: null, revoked_by: null };
      rules.set(rule.id, rule);
      d.made_rule_id = rule.id;
      emit("rule.updated", withApplied(rule), d.project_id);
    }
    emit("decision.answered", { ...d }, d.project_id);
    const task = d.task_id && tasks.get(d.task_id);
    if (task && task.state === "needs_decision") setState(task, "running", `Answered: ${answer.slice(0, 60)}`);
    return send(res, 200, d);
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/memory$/)) && req.method === "GET") {
    const pid = decodeURIComponent(r[1]);
    if (!projects.has(pid)) return notFound(res);
    return send(res, 200, memoryEntries.get(pid) ?? []);
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/memory\/proposals$/)) && req.method === "GET") {
    const pid = decodeURIComponent(r[1]), st = url.searchParams.get("state");
    if (!projects.has(pid)) return notFound(res);
    return send(res, 200, [...memoryProposals.values()].filter((x) => x.project_id === pid && (!st || x.state === st)));
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/memory\/proposals\/([^/:]+):(accept|reject)$/)) && req.method === "POST") {
    const mp = memoryProposals.get(decodeURIComponent(r[2]));
    if (!mp || mp.project_id !== decodeURIComponent(r[1])) return notFound(res);
    if (mp.state !== "proposed") return send(res, 409, { error: { code: "already_decided", message: "the proposal is already decided" } });
    const b = (await readJson(req)) ?? {};
    const by = b.decided_by?.trim() || "mock-user";
    if (r[3] === "reject") {
      Object.assign(mp, { state: "rejected", decided_at: now(), decided_by: by });
      emit("memory.rejected", { ...mp }, mp.project_id);
      return send(res, 200, mp);
    }
    if (b.text !== undefined && b.text !== null && !String(b.text).trim()) return invalid(res, "text must not be empty");
    const scope = b.scope ?? "project";
    if (!["project", "user", "repo"].includes(scope)) return invalid(res, "scope is project, user or repo");
    const text = b.text?.trim() || mp.text;
    const slug = text.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean).join("-").slice(0, 48) || "entry";
    const id = `${mp.proposed_at.slice(0, 10)}-${slug}`;
    const proj = projects.get(mp.project_id);
    let entry = null;
    if (scope === "user") {
      // All of the user's Projects: user-level memory, named after the proposal like quarkd does.
      const path = `/home/mock/.quark/memory/${mp.id}.md`;
      userMemory.push({
        id: mp.id, path, text, evidence: mp.evidence, source: mp.source, date: mp.proposed_at,
        project_id: proj.id, project_name: proj.name, entry_id: mp.id, commit: null, promoted_at: now(), promoted_by: by,
      });
      entry = { id: mp.id, project_id: mp.project_id, path, text, evidence: mp.evidence, source: mp.source, date: mp.proposed_at,
        accepted_at: now(), accepted_by: by, proposal_id: mp.id, commit: null, beads_key: null };
    } else if (beadsOf(mp.project_id).state === "ready") {
      // With Beads, the Project's memory is a Beads memory (`bd remember`).
      const key = slug.slice(0, 40);
      const list = (beadsMemories.get(mp.project_id) ?? []).filter((x) => x.key !== key);
      beadsMemories.set(mp.project_id, [...list, { key, value: text, evidence: mp.evidence, source: mp.source, accepted_at: now(), accepted_by: by }]);
      entry = { id: key, project_id: mp.project_id, path: `beads:${key}`, text, evidence: mp.evidence, source: mp.source, date: mp.proposed_at,
        accepted_at: now(), accepted_by: by, proposal_id: mp.id, commit: null, beads_key: key };
      beadsChanged(mp.project_id, "remember");
    } else {
      entry = addMemoryEntry({
        id, project_id: mp.project_id, text, evidence: mp.evidence, source: mp.source, date: mp.proposed_at,
        accepted_at: now(), accepted_by: by, proposal_id: mp.id,
      });
    }
    // Anyone working in the repo: the coordinator also opens a PR adding it to AGENTS.md.
    if (scope === "repo") setTimeout(() => chat(mp.project_id, "assistant", `I'll open a PR adding this to AGENTS.md: ${text}`), QUIET ? 50 : 700);
    Object.assign(mp, { text, state: "accepted", decided_at: now(), decided_by: by, entry });
    emit("memory.accepted", { ...mp }, mp.project_id);
    return send(res, 200, mp);
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/memory\/commits\/([^/]+)$/)) && req.method === "GET") {
    const list = memoryEntries.get(decodeURIComponent(r[1])) ?? [];
    const c = memoryCommits.get(decodeURIComponent(r[2]));
    if (!c || !list.some((e) => e.commit === c.commit)) return notFound(res);
    return send(res, 200, c);
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/memory\/([^/:]+):promote$/)) && req.method === "POST") {
    const proj = projects.get(decodeURIComponent(r[1]));
    const entry = proj && (memoryEntries.get(proj.id) ?? []).find((e) => e.id === decodeURIComponent(r[2]));
    if (!entry) return notFound(res);
    const b = (await readJson(req)) ?? {};
    let promoted = userMemory.find((u) => u.project_id === proj.id && u.entry_id === entry.id);
    if (!promoted) {
      // Like quarkd: a name another Project's entry took gets a numeric suffix.
      let id = entry.id;
      for (let n = 2; userMemory.some((u) => u.id === id); n++) id = `${entry.id}-${n}`;
      promoted = {
        id, path: `/home/mock/.quark/memory/${id}.md`, text: entry.text, evidence: entry.evidence, source: entry.source, date: entry.date,
        project_id: proj.id, project_name: proj.name, entry_id: entry.id, commit: entry.commit,
        promoted_at: now(), promoted_by: b.promoted_by?.trim() || "mock-user",
      };
      userMemory.push(promoted);
    }
    return send(res, 200, promoted);
  }
  if (p === "/v1/memory" && req.method === "GET") return send(res, 200, userMemory);
  if ((r = m(/^\/v1\/projects\/([^/]+)\/beads(:setup|:sync)?$/))) {
    const proj = projects.get(decodeURIComponent(r[1]));
    if (!proj) return notFound(res);
    const b = beadsOf(proj.id);
    if (!r[2] && req.method === "GET") return send(res, 200, b);
    if (r[2] === ":setup" && req.method === "POST") {
      if (b.state === "ready" || b.state === "setting_up") return send(res, 200, b);
      setupBeads(proj);
      return send(res, 200, { ...b });
    }
    if (r[2] === ":sync" && req.method === "POST") {
      if (b.state !== "ready") return send(res, 409, { error: { code: "beads_not_ready", message: `Beads is ${b.state}` } });
      if (!b.github_repo) return send(res, 409, { error: { code: "no_github_repo", message: "this database does not mirror a GitHub repository" } });
      b.last_sync = { at: now(), ok: true, pulled: 0, pushed: 0, message: "nothing to sync" };
      emit("beads.status", { ...b }, proj.id);
      return send(res, 200, b);
    }
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/(issues|issues\/[^/]+|issue-drafts|beads\/memories|beads\/memories\/[^/]+)$/))) {
    const proj = projects.get(decodeURIComponent(r[1]));
    if (!proj) return notFound(res);
    const pid = proj.id, rest = r[2];
    if (beadsOf(pid).state !== "ready") return send(res, 409, { error: { code: "beads_not_ready", message: `Beads is ${beadsOf(pid).state}` } });
    if (rest === "issues" && req.method === "GET") {
      const f = url.searchParams.get("filter") ?? "all";
      const list = [...issuesOf(pid).values()].map((i) => issueView(pid, i)).filter((i) => f === "all" ? true : f === "ready" ? i.ready
        : f === "blocked" ? i.blocked : f === "open" ? i.status !== "closed" : i.status === f);
      return send(res, 200, list);
    }
    if (rest.startsWith("issues/") && req.method === "GET") {
      const i = issuesOf(pid).get(decodeURIComponent(rest.slice(7)));
      if (!i) return send(res, 404, { error: { code: "not_found", message: "no such issue" } });
      return send(res, 200, issueDetail(pid, i));
    }
    if (rest === "issue-drafts" && req.method === "GET") return send(res, 200, [...issueDrafts.values()].filter((d) => d.project_id === pid));
    if (rest === "issue-drafts" && req.method === "POST") {
      const b = await readJson(req);
      if (!b?.text?.trim()) return invalid(res, "text is required");
      const d = { id: nextId("idr"), project_id: pid, state: "open", messages: [], issues: [], related: [], waiting: false, created: {}, created_at: now(), updated_at: now() };
      issueDrafts.set(d.id, d);
      draftMessage(d, b.text.trim());
      return send(res, 201, d);
    }
    if (rest === "beads/memories" && req.method === "GET") return send(res, 200, beadsMemories.get(pid) ?? []);
    if (rest.startsWith("beads/memories/") && req.method === "DELETE") {
      const key = decodeURIComponent(rest.slice(15)), list = beadsMemories.get(pid) ?? [];
      if (!list.some((x) => x.key === key)) return send(res, 404, { error: { code: "not_found", message: `no memory ${key}` } });
      beadsMemories.set(pid, list.filter((x) => x.key !== key));
      beadsChanged(pid, "forget");
      return send(res, 204);
    }
  }
  if ((r = m(/^\/v1\/issue-drafts\/([^/:]+)$/)) && req.method === "PUT") {
    const d = issueDrafts.get(decodeURIComponent(r[1]));
    if (!d) return send(res, 404, { error: { code: "not_found", message: "no such draft" } });
    if (d.state !== "open") return send(res, 409, { error: { code: "draft_closed", message: `the draft is ${d.state}` } });
    const b = await readJson(req);
    if (!Array.isArray(b?.issues) || b.issues.some((x) => !x?.key || !x?.title?.trim())) return invalid(res, "issues need a key and a title");
    writeDraft(d, { reply: b.reply ?? null, related: b.related ?? [],
      issues: b.issues.map((x) => ({ issue_type: "task", priority: 2, labels: [], description: "", blocked_by: [], ...x })) });
    return send(res, 200, d);
  }
  if ((r = m(/^\/v1\/issue-drafts\/([^/:]+)(\/messages|:accept|:discard)$/)) && req.method === "POST") {
    const d = issueDrafts.get(decodeURIComponent(r[1]));
    if (!d) return send(res, 404, { error: { code: "not_found", message: "no such draft" } });
    if (d.state !== "open") return send(res, 409, { error: { code: "draft_closed", message: `the draft is ${d.state}` } });
    const b = (await readJson(req)) ?? {};
    if (r[2] === "/messages") {
      if (!b.text?.trim()) return invalid(res, "text is required");
      draftMessage(d, b.text.trim());
      return send(res, 200, d);
    }
    if (r[2] === ":discard") {
      Object.assign(d, { state: "discarded", waiting: false, updated_at: now() });
      emit("issue_draft.updated", structuredClone(d), d.project_id);
      return send(res, 200, d);
    }
    if (d.waiting) return send(res, 409, { error: { code: "draft_waiting", message: "the coordinator is still drafting" } });
    if (!(b.issues?.length || d.issues.length)) return invalid(res, "nothing to create");
    acceptDraft(d, b);
    return send(res, 200, d);
  }
  if (p === "/v1/personas" && req.method === "GET") return send(res, 200, { default: defaultPersona, packs: PERSONAS, errors: [] });
  if (p === "/v1/personas/default" && req.method === "PUT") {
    const b = (await readJson(req)) ?? {};
    if (!PERSONAS.some((x) => x.id === b.persona)) return invalid(res, `no persona pack ${JSON.stringify(b.persona)}`);
    defaultPersona = b.persona;
    return send(res, 200, { default: defaultPersona, packs: PERSONAS, errors: [] });
  }
  if ((r = m(/^\/v1\/projects\/([^/]+)\/persona$/)) && (req.method === "GET" || req.method === "PUT")) {
    const id = decodeURIComponent(r[1]);
    if (!projects.has(id)) return notFound(res);
    if (req.method === "PUT") {
      const b = (await readJson(req)) ?? {};
      if (b.persona == null) personaOverrides.delete(id);
      else if (PERSONAS.some((x) => x.id === b.persona)) personaOverrides.set(id, b.persona);
      else return invalid(res, `no persona pack ${JSON.stringify(b.persona)}`);
    }
    const chosen = personaOverrides.get(id) ?? defaultPersona;
    return send(res, 200, {
      project_id: id, persona: PERSONAS.find((x) => x.id === chosen),
      project_override: personaOverrides.get(id) ?? null, default: defaultPersona, fallback: null,
    });
  }
  if (p === "/v1/harnesses" && req.method === "GET") return send(res, 200, HARNESSES);
  if (p === "/v1/harnesses:validate" && req.method === "POST") {
    const b = await readJson(req);
    const h = HARNESSES.find((x) => x.id === b?.config?.harness);
    const errors = [];
    if (!h) errors.push({ field: "harness", code: "unknown_harness", message: `Unknown harness "${b?.config?.harness}".` });
    else {
      if (b.role && !h.roles.includes(b.role)) errors.push({ field: "harness", code: "role_unsupported", message: `${h.name} cannot run as a ${b.role}.` });
      if (b.config.effort && !h.efforts.includes(b.config.effort)) errors.push({ field: "effort", code: "effort_unsupported", message: `${h.name} does not accept effort "${b.config.effort}".` });
      if (b.config.model && h.models.selection === "provider_qualified" && !b.config.model.includes("/"))
        errors.push({ field: "model", code: "model_format", message: `${h.name} needs a provider/model id.` });
    }
    return send(res, 200, { valid: errors.length === 0, errors, warnings: [] });
  }

  if (p === "/v1/accounts" && req.method === "GET") {
    if (url.searchParams.get("refresh") === "true") {
      for (const a of accounts.values()) if (a.quota.state === "known") readQuota(a, { ...a.quota, checked_at: now() });
    }
    const h = url.searchParams.get("harness");
    return send(res, 200, accountList().filter((a) => !h || a.harness === h));
  }
  if (p === "/v1/accounts" && req.method === "POST") {
    const b = await readJson(req);
    const h = HARNESSES.find((x) => x.id === b?.harness);
    if (!h) return invalid(res, `unknown harness \`${b?.harness}\``);
    if (!ACCOUNT_ENV[h.id]) return invalid(res, `${h.name} supports only its default account`);
    const dir = typeof b.config_dir === "string" ? b.config_dir.trim().replace(/(.)\/+$/, "$1") : "";
    if (!dir.startsWith("/") || dir.split("/").includes("..")) return invalid(res, `config_dir must be an absolute directory path, got "${b.config_dir ?? ""}"`);
    const pools = [...new Set((b.pools ?? []).map((x) => String(x).trim()))];
    const bad = pools.find((x) => !/^[a-z0-9][a-z0-9-]{0,63}$/.test(x));
    if (bad !== undefined) return invalid(res, `pool \`${bad}\` must be lowercase letters, digits and dashes`);
    const taken = [...accounts.values()].find((a) => a.harness === h.id && a.config_dir === dir);
    if (taken) return send(res, 409, { error: { code: "conflict", message: taken.default ? `${dir} is ${h.name}'s default account` : `${dir} is already an account for ${h.id}` } });
    const label = b.label?.trim() || dir.split("/").pop();
    const account = addAccount({ harness: h.id, label, config_dir: dir, pools });
    // Read its quota shortly after, like quarkd does in the background.
    if (QUOTA_PROVIDER[h.id]) setTimeout(() => readQuota(account, quotaReading(100, "max", 300)), QUIET ? 150 : 900);
    return send(res, 201, accountList().find((a) => a.id === account.id));
  }
  if ((r = m(/^\/v1\/accounts\/([^/]+)$/))) {
    const a = accounts.get(decodeURIComponent(r[1]));
    if (!a) return notFound(res);
    if (req.method === "GET") return send(res, 200, accountList().find((x) => x.id === a.id));
    if (req.method === "PATCH") {
      const b = await readJson(req);
      if (b?.label != null) {
        if (a.default) return invalid(res, "a default account's label cannot change");
        if (!String(b.label).trim()) return invalid(res, "label must be one line of 1 to 100 characters");
      }
      if (b?.pools != null) {
        const pools = [...new Set(b.pools.map((x) => String(x).trim()))];
        const bad = pools.find((x) => !/^[a-z0-9][a-z0-9-]{0,63}$/.test(x));
        if (bad !== undefined) return invalid(res, `pool \`${bad}\` must be lowercase letters, digits and dashes`);
        a.pools = pools.sort();
      }
      if (b?.label != null) a.label = String(b.label).trim();
      return send(res, 200, accountList().find((x) => x.id === a.id));
    }
    if (req.method === "DELETE") {
      if (a.default) return send(res, 409, { error: { code: "conflict", message: "a harness's default account cannot be removed" } });
      const busy = accountList().find((x) => x.id === a.id).active_tasks;
      if (busy) return send(res, 409, { error: { code: "conflict", message: `the account is in use by ${busy} running task(s)` } });
      accounts.delete(a.id);
      return send(res, 204);
    }
  }

  if ((r = m(/^\/v1\/coordinators\/([^/]+)\/messages$/))) {
    const cid = decodeURIComponent(r[1]);
    if (!chats.has(cid)) return notFound(res);
    if (req.method === "GET") return send(res, 200, chats.get(cid));
    if (req.method === "POST") {
      const b = await readJson(req);
      if (!b?.text?.trim()) return invalid(res, "text is required");
      const text = b.text.trim();
      // Like quarkd: not echoed; the session records it shortly after.
      setTimeout(() => {
        chat(cid, "user", text);
        const t = addTask(cid, { title: text.slice(0, 70), state: "queued", harness: projects.get(cid).agent_config?.harness ?? "claude-code" });
        const command = `bin/fm-spawn.sh ${t.id} projects/${cid} --mode no-mistakes --yolo off`;
        chat(cid, "tool_call", JSON.stringify({ command }), { tool_name: "bash", tool_call_id: `spawn-${t.id}`, tool: { kind: "shell", title: `Run ${command}`, command } });
        chat(cid, "tool_result", "spawned", { tool_name: "bash", tool_call_id: `spawn-${t.id}` });
        chat(cid, "assistant", `On it. I wrote a task contract and queued **${t.title}**.`);
      }, QUIET ? 50 : 700);
      return send(res, 202, { coordinator_id: cid, confirmed: true, accepted_at: now() });
    }
  }

  if ((r = m(/^\/v1\/terminals\/([^/]+)(\/\w+)?$/))) {
    const task = tasks.get(decodeURIComponent(r[1]));
    const rest = r[2] ?? "";
    // Like quarkd, a task without a worker window (queued) has no terminal.
    if (!task || task.state === "queued") return notFound(res);
    const term = terms.get(task.id);
    const info = () => ({ id: task.id, project_id: task.project_id, role: "worker", task_id: task.id, title: task.title, cols: term.cols, rows: term.rows });
    if (rest === "" && req.method === "GET") return send(res, 200, info());
    if (rest === "/snapshot" && req.method === "POST") return send(res, 200, outputEvent(task, "snapshot", "\x1b[2J\x1b[H" + term.screen));
    if (rest === "/input" && req.method === "POST") {
      const b = await readJson(req);
      if (typeof b?.data_b64 !== "string") return invalid(res, "data_b64 is required");
      terminalInput(task, Buffer.from(b.data_b64, "base64").toString("utf8"));
      return send(res, 204);
    }
    if (rest === "/resize" && req.method === "POST") {
      const b = await readJson(req);
      if (Number.isInteger(b?.cols) && Number.isInteger(b?.rows)) { term.cols = b.cols; term.rows = b.rows; }
      return send(res, 200, info());
    }
  }

  if ((r = m(/^\/v1\/tasks\/([^/:]+)(.*)$/))) {
    const task = tasks.get(decodeURIComponent(r[1]));
    const rest = r[2];
    if (!task) return notFound(res);
    if (rest === "" && req.method === "GET") return send(res, 200, task);
    if (rest === "/messages" && req.method === "POST") {
      const b = await readJson(req);
      const text = b?.text?.trim();
      if (!text) return invalid(res, "text is required");
      if (/^(--|\/|\$)/.test(text)) return invalid(res, "messages cannot start with --, / or $");
      transcript(task, "user", text);
      setTimeout(() => transcript(task, "assistant", `Noted: "${text}". Adjusting the plan.`), QUIET ? 50 : 600);
      return send(res, 204);
    }
    if (rest === ":cancel" && req.method === "POST") {
      if (task.state === "done" || task.state === "failed") return send(res, 409, { error: { code: "conflict", message: "task already finished" } });
      setState(task, "failed", "Cancelled from the app");
      output(task, "\r\n\x1b[31m[worker exited: cancelled]\x1b[0m\r\n");
      return send(res, 204);
    }
    if (rest === ":relaunch" && req.method === "POST") {
      setState(task, "running", "Relaunched from the app");
      recordDispatch(task, dispatches.has(task.id) ? "relaunch" : "coordinator");
      terms.get(task.id).screen = "";
      outputEvent(task, "snapshot", "\x1b[2J\x1b[H");
      output(task, `\x1b[1;35m${task.harness}\x1b[0m relaunched on \x1b[1m${task.title}\x1b[0m\r\n$ `);
      return send(res, 204);
    }
    if (rest === "/transcript" && req.method === "GET") return send(res, 200, transcripts.get(task.id));
    if (rest === "/dispatch" && req.method === "GET") return send(res, 200, dispatches.get(task.id) ?? []);
    if ((rest === "/changes" || rest === "/diff") && task.state === "queued") {
      return send(res, 409, { error: { code: "no_worktree", message: "the task has no worktree yet" } });
    }
    if (rest === "/changes" && req.method === "GET") {
      return send(res, 200, { task_id: task.id, base_ref: "origin/main", base: "0a28113f4c", head: "9f1e2d3c4b", files: filesOf(diffOf.get(task.id)) });
    }
    if (rest === "/diff" && req.method === "GET") {
      const path = url.searchParams.get("path");
      if (path && !filesOf(diffOf.get(task.id)).some((f) => f.path === path)) return notFound(res);
      return send(res, 200, { task_id: task.id, base: "0a28113f4c", path, patch: diffFor(diffOf.get(task.id), path), truncated: false });
    }
  }

  if (p === "/v1/pull-requests" && req.method === "GET") {
    const st = url.searchParams.get("state"), pid = url.searchParams.get("project_id");
    return send(res, 200, [...pulls.values()].filter((x) => (!st || x.state === st) && (!pid || x.project_id === pid)));
  }
  if ((r = m(/^\/v1\/pull-requests\/([^/:]+)(.*)$/))) {
    const pr = pulls.get(decodeURIComponent(r[1]));
    const rest = r[2];
    if (!pr) return notFound(res);
    if (rest === "" && req.method === "GET") return send(res, 200, pr);
    if (rest === "/diff" && req.method === "GET") {
      const path = url.searchParams.get("path");
      if (path && !filesOf(pullDiffs.get(pr.id)).some((f) => f.path === path)) return notFound(res);
      return send(res, 200, { pull_request_id: pr.id, path, patch: diffFor(pullDiffs.get(pr.id), path), truncated: false });
    }
    if ((r = /^\/evidence\/artifacts\/([^/]+)$/.exec(rest)) && req.method === "GET") {
      const a = artifacts.get(decodeURIComponent(r[1]));
      if (!a) return notFound(res);
      res.writeHead(200, { "content-type": a.content_type, ...cors });
      return res.end(a.body);
    }
    if (rest === "/comments" && req.method === "POST") {
      const b = await readJson(req);
      if (!b?.text?.trim()) return invalid(res, "text is required");
      if (b.line != null && !b.path) return invalid(res, "line requires path");
      const task = pr.task_id && tasks.get(pr.task_id);
      if (!task) return send(res, 409, { error: { code: "no_task", message: "no task owns this pull request" } });
      prComments.push({ pull_request_id: pr.id, text: b.text.trim(), path: b.path ?? null, line: b.line ?? null, side: b.side ?? null });
      transcript(task, "user", `Review comment${b.path ? ` on ${b.path}${b.line != null ? `:${b.line}` : ""}` : ""}: ${b.text.trim()}`);
      return send(res, 204);
    }
    if (rest === ":merge" && req.method === "POST") {
      // Like the engine's guarded merge: open, not a draft, conflict-free and green, or refused.
      const why = pr.state !== "open" ? `pull request is ${pr.state}` : pr.mergeable === "conflicting" ? "pull request has conflicts"
        : pr.checks_state !== "passing" && pr.checks_state !== "none" ? "checks are not green" : null;
      if (why) return send(res, 409, { error: { code: "merge_refused", message: why } });
      pr.state = "merged"; pr.merged_at = pr.closed_at = pr.updated_at = now();
      emit("pr.updated", { ...pr }, pr.project_id);
      const task = pr.task_id && tasks.get(pr.task_id);
      if (task) setState(task, "done", "Merged from the PR center");
      return send(res, 200, pr);
    }
  }
  if (p === "/mock/pr-comments" && req.method === "GET") return send(res, 200, prComments);
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
