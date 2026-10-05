// J9: a Project's dispatch rules. Each rule says when it applies and lists its candidates in
// order; the default covers every other task. Saving commits dispatch.yaml to the Project repo,
// and the test pane shows what the rules would do with a task description.
//
// Keys: r and t switch between rules and test, ⌘/Ctrl+Enter saves (rules) or runs the test.
// Rules: j/k or ↑/↓ move, Enter or e edits, n adds a rule, J/K move the rule down or up,
// x twice deletes it, Esc leaves a field.
import React, { useEffect, useMemo, useRef, useState } from "react";
import {
  Account, api, ApiError, DispatchProfile, DispatchRules, DispatchRulesDraft, DispatchRuleSpec, DispatchSelect, DispatchTest,
  HarnessInfo, HarnessValidation, NotAvailable,
} from "../api";
import { poolsFor } from "../accounts";
import { allProfiles, cleanDraft, cleanProfile, move, problems, profileKey, profileLabel, sameDraft } from "../dispatch";
import { href } from "../nav";
import { useStore } from "../store";
import { errText } from "../util";
import { Unavailable } from "../components/Unavailable";
import { DECIDER } from "../components/WhyThisAgent";

type Tab = "rules" | "test";
const VALID: HarnessValidation = { valid: true, errors: [], warnings: [] };
const CHECKS: Record<string, string> = {
  harness_installed: "Harness installed", model_accepted: "Model accepted", account_health: "Account health", quota_headroom: "Quota headroom",
};

export function Dispatch({ project: pid }: { project: string }) {
  const project = useStore((s) => s.projects[pid]);
  const connected = useStore((s) => s.connected);

  const [saved, setSaved] = useState<DispatchRules | null>(null);
  const [draft, setDraft] = useState<DispatchRulesDraft | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const load = () => {
    setLoadErr(null);
    return api.dispatchRules(pid).then((r) => { setSaved(r); setDraft(cleanDraft(r)); setErr(null); setStale(false); })
      .catch((e) => { if (e instanceof NotAvailable) setUnavailable(true); else setLoadErr(errText(e)); });
  };
  // Loaded once; a reconnect tries again only while there is nothing to edit, so it never replaces an edit in progress.
  useEffect(() => { if (!saved) void load(); }, [pid, connected]);

  const [harnesses, setHarnesses] = useState<HarnessInfo[]>([]);
  const [accounts, setAccounts] = useState<Account[]>([]);
  useEffect(() => {
    api.harnesses().then(setHarnesses).catch(() => setHarnesses([]));
    api.accounts().then(setAccounts).catch(() => setAccounts([]));
  }, []);
  // A candidate runs a worker.
  const workers = useMemo(() => harnesses.filter((h) => h.roles.includes("worker")), [harnesses]);

  // Each distinct candidate is checked once with the harness; an older daemon without the check passes it.
  const asked = useRef(new Map<string, Promise<HarnessValidation>>());
  const [checks, setChecks] = useState<Record<string, HarnessValidation>>({});
  const validate = (p: DispatchProfile): Promise<HarnessValidation> => {
    const key = profileKey(p);
    let pending = asked.current.get(key);
    if (!pending) {
      const { harness, model, effort, pool } = cleanProfile(p);
      pending = api.validateAgent({ harness, model, effort, pool }, "worker")
        .catch((e) => { if (e instanceof NotAvailable) return VALID; asked.current.delete(key); throw e; });
      asked.current.set(key, pending);
      pending.then((v) => setChecks((c) => ({ ...c, [key]: v }))).catch(() => undefined);
    }
    return pending;
  };
  useEffect(() => {
    if (!draft) return;
    const t = setTimeout(() => { for (const p of allProfiles(draft)) if (p.harness.trim()) void validate(p).catch(() => undefined); }, 250);
    return () => clearTimeout(t);
  }, [draft]);

  const [tab, setTab] = useState<Tab>("rules");
  // The selected row: a rule's index, or the number of rules for the default.
  const [sel, setSel] = useState(0);
  const count = draft?.rules.length ?? 0;
  const at = Math.min(sel, count);
  const [armed, setArmed] = useState(false);
  useEffect(() => { setArmed(false); }, [at, tab]);

  const dirty = !!draft && !!saved && !sameDraft(draft, saved);
  const issues = useMemo(() => (draft ? problems(draft) : []), [draft]);
  const refused = !!draft && allProfiles(draft).some((p) => checks[profileKey(p)]?.valid === false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // Set when main moved on from the revision the edit started at.
  const [stale, setStale] = useState(false);
  const [commit, setCommit] = useState<string | null>(null);

  const edit = (f: (d: DispatchRulesDraft) => DispatchRulesDraft) => { setDraft((d) => (d ? f(d) : d)); setCommit(null); };
  const setRule = (i: number, r: DispatchRuleSpec) => edit((d) => ({ ...d, rules: d.rules.map((x, j) => (j === i ? r : x)) }));
  const addRule = () => {
    edit((d) => ({ ...d, rules: [...d.rules, { name: "", when: "", select: null, candidates: [{ ...(d.default[0] ?? { harness: workers[0]?.id ?? "" }) }] }] }));
    setTab("rules"); setSel(count); focusFirst.current = true;
  };
  const removeRule = (i: number) => { edit((d) => ({ ...d, rules: d.rules.filter((_, j) => j !== i) })); setArmed(false); };
  const moveRule = (i: number, delta: number) => {
    if (i + delta < 0 || i + delta >= count) return;
    edit((d) => ({ ...d, rules: move(d.rules, i, delta) }));
    setSel(i + delta);
  };

  const save = async () => {
    if (!draft || !saved || busy || !dirty || issues.length) return;
    setBusy(true); setErr(null); setStale(false);
    const next = cleanDraft(draft);
    try {
      const answers = await Promise.all(allProfiles(next).map(validate));
      if (answers.some((v) => !v.valid)) { setErr("A harness refuses one of the candidates. Fix it, then save."); return; }
      const now = await api.saveDispatchRules(pid, next, saved.revision ?? null);
      setSaved(now); setDraft(cleanDraft(now)); setCommit(now.commit ?? null);
    } catch (e) {
      setErr(errText(e));
      if (e instanceof ApiError && e.code === "dispatch_changed") setStale(true);
    } finally {
      setBusy(false);
    }
  };

  const [description, setDescription] = useState("");
  const [testing, setTesting] = useState(false);
  const [test, setTest] = useState<{ result: DispatchTest; draft: boolean } | null>(null);
  const [testErr, setTestErr] = useState<string | null>(null);
  const runTest = async () => {
    if (!draft || testing || !description.trim() || (dirty && issues.length > 0)) return;
    setTesting(true); setTestErr(null);
    try {
      // Edited rules are tested as they stand, without saving them.
      const result = await api.testDispatch(pid, description.trim(), dirty ? cleanDraft(draft) : undefined);
      setTest({ result, draft: dirty });
    } catch (e) {
      setTest(null);
      setTestErr(e instanceof NotAvailable ? "Testing dispatch rules is not available from this daemon yet." : errText(e));
    } finally {
      setTesting(false);
    }
  };

  const detail = useRef<HTMLDivElement>(null);
  const describe = useRef<HTMLTextAreaElement>(null);
  const focusFirst = useRef(false);
  const focusDetail = () => detail.current?.querySelector<HTMLElement>("input, textarea, select")?.focus();
  useEffect(() => { if (focusFirst.current) { focusFirst.current = false; focusDetail(); } });
  const show = (t: Tab) => { setTab(t); if (t === "test") setTimeout(() => describe.current?.focus()); };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.altKey || e.defaultPrevented) return;
      const t = e.target as HTMLElement | null;
      if (t?.closest("[cmdk-root]")) return;
      if (e.metaKey || e.ctrlKey) {
        if (e.key === "Enter") { e.preventDefault(); if (tab === "test") void runTest(); else void save(); }
        return;
      }
      if (t?.closest("input, textarea, select, [contenteditable]")) {
        if (e.key === "Escape") { e.preventDefault(); t.blur(); }
        return;
      }
      if (e.key === "r") { e.preventDefault(); show("rules"); return; }
      if (e.key === "t") { e.preventDefault(); show("test"); return; }
      if (tab !== "rules" || !draft) return;
      const to = (i: number) => { e.preventDefault(); setSel(Math.max(0, Math.min(count, i))); };
      switch (e.key) {
        case "j": case "ArrowDown": return to(at + 1);
        case "k": case "ArrowUp": return to(at - 1);
        case "J": if (at < count) { e.preventDefault(); moveRule(at, 1); } return;
        case "K": if (at < count) { e.preventDefault(); moveRule(at, -1); } return;
        case "n": e.preventDefault(); addRule(); return;
        case "Enter": case "e": e.preventDefault(); focusDetail(); return;
        case "x": if (at < count) { e.preventDefault(); if (armed) removeRule(at); else setArmed(true); } return;
        case "Escape": setArmed(false); return;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  if (!project) {
    return <div className="empty">{connected ? "This project does not exist on the daemon." : "Waiting for the daemon…"}</div>;
  }

  const provider = saved?.classifier?.provider;
  const noClassifier = !saved?.classifier || provider === "none";
  const rule = draft && at < count ? draft.rules[at] : undefined;
  const mine = (where: number | "default") => issues.filter((p) => p.at === where);
  const blocked = issues.length > 0 || refused;
  return (
    <>
      <div className="header">
        <h1>Dispatch</h1>
        <a className="crumb" href={href({ name: "project", id: pid })}>{project.name}</a>
        <span className="spacer" />
        {commit && !dirty && <span className="faint small-text" data-testid="dispatch-commit">Saved as commit <span className="mono">{commit.slice(0, 10)}</span></span>}
        {dirty && <span className="pill yellow" data-testid="dispatch-dirty">unsaved changes</span>}
        {dirty && <button className="link" type="button" onClick={() => { setDraft(cleanDraft(saved!)); setErr(null); }}>Revert</button>}
        <div className="seg" role="tablist" aria-label="Dispatch">
          <button role="tab" aria-selected={tab === "rules"} className={tab === "rules" ? "on" : ""} onClick={() => show("rules")}>
            Rules{draft && <> <span className="n">{count}</span></>}
          </button>
          <button role="tab" aria-selected={tab === "test"} className={tab === "test" ? "on" : ""} onClick={() => show("test")}>Test</button>
        </div>
        <button className="btn on" type="button" disabled={!dirty || busy || blocked} onClick={() => void save()} data-testid="dispatch-save"
          title={blocked ? "Fix what is marked before saving" : "Commit dispatch.yaml to the Project repo"}>
          {busy ? "Saving…" : "Save"}
        </button>
      </div>
      <div className="screen inbox">
        {unavailable ? <div className="dx-wide"><Unavailable what="The dispatch rule editor" endpoint={`GET /v1/projects/${pid}/dispatch`} /></div>
          : !draft ? (
            <div className="dx-wide empty">
              {loadErr ? <><div className="bad">{loadErr}</div><button className="btn" type="button" onClick={() => void load()}>Try again</button></>
                : connected ? "Loading…" : "Waiting for the daemon…"}
            </div>
          ) : tab === "rules" ? (
            <>
              <div className="inbox-list" data-testid="dispatch-list">
                {draft.rules.map((r, i) => (
                  <button key={i} type="button" className={"decision-row dx-row" + (i === at ? " on" : "")} onClick={() => setSel(i)}
                    data-testid="dispatch-row" aria-current={i === at ? "true" : undefined}>
                    <div className="q"><span className="faint mono">{i + 1}</span> {r.name?.trim() || <span className="faint">unnamed rule</span>}</div>
                    <div className="meta">
                      <span className="ellipsis">{r.when.trim() || "no condition yet"}</span>
                      <span className="spacer" />
                      {mine(i).length > 0 && <span className="pill red">fix</span>}
                      <span>{r.candidates.length} candidate{r.candidates.length === 1 ? "" : "s"}</span>
                    </div>
                  </button>
                ))}
                <button type="button" className={"decision-row dx-row" + (at === count ? " on" : "")} onClick={() => setSel(count)}
                  data-testid="dispatch-row" aria-current={at === count ? "true" : undefined}>
                  <div className="q">Default</div>
                  <div className="meta">
                    <span className="ellipsis">Every task no rule matches</span>
                    <span className="spacer" />
                    {mine("default").length > 0 && <span className="pill red">fix</span>}
                    <span>{draft.default.length} candidate{draft.default.length === 1 ? "" : "s"}</span>
                  </div>
                </button>
                <button type="button" className="btn add dx-add" onClick={addRule} data-testid="dispatch-add-rule">Add rule</button>
                <div className="dx-note faint small-text" data-testid="dispatch-classifier">
                  {noClassifier ? "No classifier is configured, so the coordinator picks the rule for each task."
                    : <>Classifier <span className="mono">{String(provider ?? "configured")}</span> matches each task to a rule.</>}
                  {" "}Rules are tried in order.
                </div>
              </div>
              <div className="inbox-detail">
                <div className="form dx-form" ref={detail} data-testid="dispatch-detail">
                  {rule ? (
                    <>
                      <label className="field">
                        <span>Name</span>
                        <input value={rule.name ?? ""} aria-label="Rule name" placeholder="trivial-edit"
                          onChange={(e) => setRule(at, { ...rule, name: e.target.value })} />
                      </label>
                      <label className="field">
                        <span>When</span>
                        <textarea rows={3} value={rule.when} aria-label="When" placeholder="The kind of task this rule is for, in plain words."
                          onChange={(e) => setRule(at, { ...rule, when: e.target.value })} />
                      </label>
                      <Candidates label="Candidates" list={rule.candidates} workers={workers} accounts={accounts} checks={checks}
                        onChange={(candidates) => setRule(at, { ...rule, candidates })} />
                      <label className="field">
                        <span>Select</span>
                        <select value={rule.select ?? ""} aria-label="Select"
                          onChange={(e) => setRule(at, { ...rule, select: (e.target.value || null) as DispatchSelect | null })}>
                          <option value="">As the default says ({selectWords(draft.default_select)})</option>
                          <option value="ordered">Ordered: the first candidate that can run</option>
                          <option value="quota-balanced">Quota-balanced: the candidate with the most quota to spend</option>
                        </select>
                      </label>
                      {kept(rule).length > 0 && <div className="hint-line">Also set in dispatch.yaml and kept as written: {kept(rule).join(", ")}.</div>}
                      {mine(at).map((p) => <div className="field-error" key={p.message}>{p.message}</div>)}
                      <div className="d-actions">
                        <button className="btn" type="button" disabled={at === 0} onClick={() => moveRule(at, -1)}>Move up</button>
                        <button className="btn" type="button" disabled={at === count - 1} onClick={() => moveRule(at, 1)}>Move down</button>
                        <span className="spacer" />
                        <button className={"btn" + (armed ? " danger" : "")} type="button" data-testid="dispatch-delete"
                          onClick={() => (armed ? removeRule(at) : setArmed(true))}>
                          {armed ? "Delete? Press x again" : "Delete rule"}
                        </button>
                      </div>
                    </>
                  ) : (
                    <>
                      <div className="hint-line">A task no rule matches gets one of these.</div>
                      <Candidates label="Default candidates" list={draft.default} workers={workers} accounts={accounts} checks={checks}
                        onChange={(list) => edit((d) => ({ ...d, default: list }))} />
                      <label className="field">
                        <span>Default select</span>
                        <select value={draft.default_select ?? ""} aria-label="Default select"
                          onChange={(e) => edit((d) => ({ ...d, default_select: (e.target.value || null) as DispatchSelect | null }))}>
                          <option value="">Not set (the engine's own: ordered)</option>
                          <option value="ordered">Ordered: the first candidate that can run</option>
                          <option value="quota-balanced">Quota-balanced: the candidate with the most quota to spend</option>
                        </select>
                        <div className="hint-line">How a list of candidates is resolved where a rule does not say.</div>
                      </label>
                      {mine("default").map((p) => <div className="field-error" key={p.message}>{p.message}</div>)}
                    </>
                  )}
                  {err && (
                    <div className="form-error" role="alert" data-testid="dispatch-error">
                      {err}{stale && <> <button className="btn small" type="button" onClick={() => void load()}>Load the rules on main</button></>}
                    </div>
                  )}
                </div>
                <div className="inbox-keys faint">
                  <span className="kbd">j</span><span className="kbd">k</span> move · <span className="kbd">e</span> edit · <span className="kbd">n</span> new rule ·{" "}
                  <span className="kbd">J</span><span className="kbd">K</span> reorder · <span className="kbd">x</span><span className="kbd">x</span> delete ·{" "}
                  <span className="kbd">⌘/Ctrl ↵</span> save · <span className="kbd">r</span><span className="kbd">t</span> rules/test
                </div>
              </div>
            </>
          ) : (
            <>
              <div className="inbox-list">
                <form className="form dx-form" onSubmit={(e) => { e.preventDefault(); void runTest(); }} data-testid="dispatch-test-form">
                  <label className="field">
                    <span>Task description</span>
                    <textarea ref={describe} rows={8} value={description} aria-label="Task description" onChange={(e) => setDescription(e.target.value)}
                      placeholder="The task as it would be briefed to a worker." />
                  </label>
                  <div className="hint-line" data-testid="dispatch-test-scope">
                    {dirty ? "Tests the rules as edited here. Nothing is saved or dispatched." : "Tests the saved rules. Nothing is dispatched."}
                  </div>
                  {dirty && issues.length > 0 && <div className="field-error">Fix what is marked in the rules before testing them.</div>}
                  <div className="form-actions">
                    <button className="btn on" type="submit" disabled={testing || !description.trim() || (dirty && issues.length > 0)}>
                      {testing ? "Testing…" : "Run test"}
                    </button>
                  </div>
                </form>
              </div>
              <div className="inbox-detail">
                {testErr ? <div className="empty bad" data-testid="dispatch-test-error">{testErr}</div>
                  : test ? <TestResult t={test.result} draft={test.draft} rules={draft} />
                  : <div className="empty">Describe a task to see the rule it matches and whether each candidate could run it now.</div>}
                <div className="inbox-keys faint">
                  <span className="kbd">⌘/Ctrl ↵</span> run the test · <span className="kbd">Esc</span> leave the box · <span className="kbd">r</span><span className="kbd">t</span> rules/test
                </div>
              </div>
            </>
          )}
      </div>
    </>
  );
}

function selectWords(s: DispatchSelect | null | undefined) { return s === "quota-balanced" ? "quota-balanced" : "ordered"; }

/** The engine's own fields a rule sets, which the editor keeps and does not edit. */
function kept(r: DispatchRuleSpec): string[] {
  return [r.why ? "why" : "", r.approval ? "approval" : "", r.floor ? "floor" : ""].filter(Boolean);
}

/** An ordered candidate list: harness, model, effort and account pool each, checked with the harness as it is edited. */
function Candidates({ label, list, workers, accounts, checks, onChange }: {
  label: string; list: DispatchProfile[]; workers: HarnessInfo[]; accounts: Account[];
  checks: Record<string, HarnessValidation>; onChange: (list: DispatchProfile[]) => void;
}) {
  const set = (i: number, p: DispatchProfile) => onChange(list.map((x, j) => (j === i ? p : x)));
  return (
    <fieldset className="field" data-testid="dispatch-candidates">
      <span>{label}</span>
      {list.map((c, i) => {
        const h = workers.find((x) => x.id === c.harness);
        const pools = poolsFor(accounts, c.harness);
        const check = checks[profileKey(c)];
        const n = `Candidate ${i + 1}`;
        return (
          <div className="dx-cand" key={i} data-testid="dispatch-candidate">
            <div className="dx-cand-row">
              <span className="faint mono dx-n">{i + 1}</span>
              <select value={c.harness} aria-label={`${n} harness`}
                // Another harness takes other models and efforts, and has its own pools.
                onChange={(e) => set(i, { harness: e.target.value })}>
                {!h && <option value={c.harness}>{c.harness || "Pick a harness"}</option>}
                {workers.map((x) => <option key={x.id} value={x.id}>{x.name}{x.install.installed ? "" : " (not installed)"}</option>)}
              </select>
              {h?.models.selection === "automatic" ? <span className="faint small-text dx-auto">picks its own model</span> : (
                <input value={c.model ?? ""} aria-label={`${n} model`} onChange={(e) => set(i, { ...c, model: e.target.value })}
                  placeholder={h?.models.selection === "provider_qualified" ? "provider/model" : "Model (optional)"} />
              )}
              <select value={c.effort ?? ""} aria-label={`${n} effort`} onChange={(e) => set(i, { ...c, effort: e.target.value || null })}>
                <option value="">Default effort</option>
                {[...new Set([...(h?.efforts ?? []), ...(c.effort ? [c.effort] : [])])].map((x) => <option key={x} value={x}>{x}</option>)}
              </select>
              <select value={c.pool ?? ""} aria-label={`${n} account pool`} onChange={(e) => set(i, { ...c, pool: e.target.value || null })}>
                <option value="">Default account</option>
                {[...new Set([...pools, ...(c.pool ? [c.pool] : [])])].map((p) => <option key={p} value={p}>Pool: {p}</option>)}
              </select>
              <button type="button" className="btn" aria-label={`Move ${n.toLowerCase()} up`} disabled={i === 0} onClick={() => onChange(move(list, i, -1))}>↑</button>
              <button type="button" className="btn" aria-label={`Move ${n.toLowerCase()} down`} disabled={i === list.length - 1} onClick={() => onChange(move(list, i, 1))}>↓</button>
              <button type="button" className="btn" aria-label={`Remove ${n.toLowerCase()}`} onClick={() => onChange(list.filter((_, j) => j !== i))}>×</button>
            </div>
            {check?.errors.map((x) => <div className="field-error dx-issue" key={x.field + x.code} data-testid="dispatch-issue">{x.message}</div>)}
            {check?.warnings.map((x) => <div className="hint-line dx-issue" key={x.field + x.code}>{x.message}</div>)}
            {h && !h.install.installed && <div className="hint-line dx-issue">{h.name} is not installed: <span className="mono">{h.install.install_hint}</span></div>}
          </div>
        );
      })}
      <button type="button" className="btn add" data-testid="dispatch-add-candidate"
        onClick={() => onChange([...list, { harness: (workers.find((x) => x.install.installed) ?? workers[0])?.id ?? "" }])}>Add candidate</button>
    </fieldset>
  );
}

/** What the rules would do with the description: the matched rule, then every candidate with its checks. */
function TestResult({ t, draft, rules }: { t: DispatchTest; draft: boolean; rules: DispatchRulesDraft }) {
  // The resolution names a rule by position; the rules give it its name.
  const nameOf = (id: string) => (id === "default" ? "default" : rules.rules[Number(id.replace(/^rule_/, "")) - 1]?.name?.trim() || id);
  const passing = t.candidates.filter((c) => c.passed).length;
  return (
    <div className="why dx-result" data-testid="dispatch-test-result">
      <p className="why-summary" data-testid="dispatch-test-summary">{t.summary}</p>
      <dl className="why-facts">
        <dt>Rules</dt>
        <dd>{draft ? <span className="pill yellow">as edited, not saved</span> : <span className="pill">saved</span>}</dd>
        <dt>Matched rule</dt>
        <dd data-testid="dispatch-test-rule">
          {t.rule ? <><span className="pill accent">{nameOf(t.rule.id)}</span>{t.rule.when && <span className="faint"> {t.rule.when}</span>}</>
            : <span className="faint">none: the coordinator would pick</span>}
        </dd>
        <dt>Decided by</dt>
        <dd><span className="pill accent">{DECIDER[t.decided_by] ?? t.decided_by}</span></dd>
        {t.chosen && <><dt>Would select</dt><dd className="mono" data-testid="dispatch-test-chosen">{profileLabel(t.chosen)}</dd></>}
        <dt>Classifier</dt>
        <dd>
          {t.classifier.provider === "none" ? <span className="faint">none</span> : (
            <>
              <span className="mono">{t.classifier.provider}</span>
              {t.classifier.model && <span className="mono"> · {t.classifier.model}</span>}
              {t.classifier.confidence != null && <> · {Math.round(t.classifier.confidence * 100)}% confidence</>}
            </>
          )}
        </dd>
      </dl>
      <div className="gate" data-testid="dispatch-test-candidates">
        <div className="gate-head"><b>Candidates</b><span className="faint">{passing} of {t.candidates.length} could start a worker now</span></div>
        {t.candidates.map((c, i) => {
          const chosen = !!t.chosen && c.harness === t.chosen.harness && (c.model ?? null) === (t.chosen.model ?? null);
          return (
            <div key={i} className="why-cand" data-testid="dispatch-test-candidate">
              <span className={c.passed ? "green" : "red"} aria-label={c.passed ? "passed" : "failed"}>{c.passed ? "✓" : "✗"}</span>
              <span className="mono">{profileLabel(c)}</span>
              {c.pool && <span className="pill">pool {c.pool}</span>}
              <span className="pill">{c.rule_name ?? nameOf(c.rule.id)}</span>
              {chosen && <span className="pill accent">would be chosen</span>}
              <span className={c.passed ? "" : "red"} data-testid="dispatch-test-reason">{c.reason}</span>
              <ul className="dx-checks">
                {c.checks.map((k) => (
                  <li key={k.check} data-testid="dispatch-test-check">
                    <span className={k.passed ? "green" : "red"} aria-label={k.passed ? "passed" : "failed"}>{k.passed ? "✓" : "✗"}</span>{" "}
                    <span className="faint">{CHECKS[k.check] ?? k.check}</span> <span className="small-text">{k.detail}</span>
                  </li>
                ))}
              </ul>
            </div>
          );
        })}
        {!t.candidates.length && <div className="empty">The rules list no candidates.</div>}
      </div>
    </div>
  );
}
