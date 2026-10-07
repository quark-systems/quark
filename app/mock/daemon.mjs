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
    failovers: t.failovers ?? [],
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
  const a = T(quark, "Event stream: resync slow clients from the store", "running", { diff: DIFFS[0], harness: "claude-code", updated_at: minutesAgo(1) });
  const b = T(quark, "Terminal sessions over tmux control mode", "running", { diff: DIFFS[1], harness: "codex", updated_at: minutesAgo(4) });
  T(quark, "Decision records carry who answered", "needs_decision", { diff: DIFFS[2], state_note: "Asked: keep answer history per decision?", updated_at: minutesAgo(12) });
  T(quark, "Harness registry trait", "queued", { updated_at: minutesAgo(20) });
  T(quark, "OpenAPI check in CI", "in_review", { pull_request_url: "https://github.com/quark-systems/quark/pull/2", updated_at: minutesAgo(40) });
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
    id, project_id: p.id, task_id: null, question, state: "open", answer: null, answered_by: null, answered_at: null, ...extra,
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
    return send(res, 200, [...decisions.values()].filter((d) => !st || d.state === st));
  }
  if ((r = m(/^\/v1\/decisions\/([^/:]+):answer$/)) && req.method === "POST") {
    const d = decisions.get(decodeURIComponent(r[1]));
    if (!d) return notFound(res);
    if (d.state !== "open") return send(res, 409, { error: { code: "already_answered", message: "the decision is already answered" } });
    const b = await readJson(req);
    const answer = typeof b?.answer === "string" ? b.answer.trim() : "";
    if (!answer) return invalid(res, "answer is required");
    Object.assign(d, { state: "answered", answer, answered_by: b.answered_by?.trim() || "mock-user", answered_at: now() });
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
    const text = b.text?.trim() || mp.text;
    const slug = text.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean).join("-").slice(0, 48) || "entry";
    const id = `${mp.proposed_at.slice(0, 10)}-${slug}`;
    const entry = addMemoryEntry({
      id, project_id: mp.project_id, text, evidence: mp.evidence, source: mp.source, date: mp.proposed_at,
      accepted_at: now(), accepted_by: by, proposal_id: mp.id,
    });
    Object.assign(mp, { text, state: "accepted", decided_at: entry.accepted_at, decided_by: by, entry });
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
