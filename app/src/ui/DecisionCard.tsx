// One decision card in three sizes: `inline` in a conversation (one tap answers), `full`
// in the Decisions tab (options with consequences, a reason, a standing rule), and `phone`
// (a compact card with the first two options). See GLOSSARY.md, "Asking and answering".
import React, { useState } from "react";
import { api, Decision, DecisionOption, NotAvailable } from "../api";
import { href } from "../nav";
import { upsertDecision, upsertRule, useStore } from "../store";
import { ago, errText, savedUser } from "../util";
import {
  answeredVia,
  askedBy, blockLabels, decidedByAgent, decisionLabel, joinAnd, LIFECYCLE, lifecycleStep, ruleDraft, usesOf,
} from "../decisions";
import { Button } from ".";

export type DecisionCardSize = "inline" | "full" | "phone";

const OTHER = "\u0000other";

/** The line over a question: its number, who asked and when, and what it blocks. */
function DecisionMeta({ d }: { d: Decision }) {
  const tasks = useStore((s) => s.tasks);
  const blocks = blockLabels(d, tasks);
  return (
    <span className="dc-meta" data-testid="decision-meta">
      {decisionLabel(d)} · asked by {askedBy(d, tasks)}{" "}
      <time dateTime={d.opened_at} title={d.opened_at}>{ago(d.opened_at)}</time>
      {blocks.length > 0 && <> · blocks {joinAnd(blocks)}</>}
    </span>
  );
}

/** The four lifecycle steps, with the one the decision has reached marked. */
function DecisionProgress({ d }: { d: Decision }) {
  const at = lifecycleStep(d);
  return (
    <ol className="dc-progress" aria-label="Decision progress">
      {LIFECYCLE.map((s, i) => (
        <li key={s} className={i === at ? "on" : i < at ? "done" : ""} aria-current={i === at ? "step" : undefined}>{i + 1} {s}</li>
      ))}
    </ol>
  );
}

export function DecisionCard({ d, size, onAnswered, answerBox }: {
  d: Decision; size: DecisionCardSize;
  /** Called with the answered decision; the card has already recorded it in the store. */
  onAnswered?: (d: Decision) => void;
  /** Focused by keyboard shortcuts to start answering. */
  answerBox?: React.RefObject<HTMLTextAreaElement>;
}) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const send = async (answer: string, extra: { why?: string; make_rule?: string } = {}) => {
    if (!answer.trim() || busy) return;
    setBusy(true); setErr(null);
    try {
      const r = await api.answerDecision(d.id, {
        answer: answer.trim(), answered_by: savedUser().trim() || null, via: size === "phone" ? "phone" : "app",
        why: extra.why?.trim() || null, make_rule: extra.make_rule?.trim() || null,
      });
      upsertDecision(r);
      onAnswered?.(r);
    } catch (e) {
      if (e instanceof NotAvailable) setUnavailable(true);
      else setErr(errText(e));
    } finally { setBusy(false); }
  };
  const status = { busy, err, unavailable };
  if (size === "phone") return <PhoneCard d={d} send={send} {...status} />;
  if (size === "inline") return <InlineCard d={d} send={send} {...status} />;
  return <FullCard d={d} send={send} answerBox={answerBox} {...status} />;
}

type Send = (answer: string, extra?: { why?: string; make_rule?: string }) => Promise<void>;
type Status = { busy: boolean; err: string | null; unavailable: boolean };

function options(d: Decision): DecisionOption[] {
  return d.brief?.options ?? [];
}

function isRecommended(d: Decision, o: DecisionOption) {
  return !!d.brief?.recommended && d.brief.recommended === o.label;
}

/** Options as buttons, the recommended one first and primary. */
function sortedOptions(d: Decision): DecisionOption[] {
  return [...options(d)].sort((a, b) => Number(isRecommended(d, b)) - Number(isRecommended(d, a)));
}

function Problem({ err, unavailable }: Status) {
  if (unavailable) return <span className="bad small-text">Answering is not available from this daemon yet.</span>;
  return err ? <span className="bad small-text" role="alert">{err}</span> : null;
}

function InlineCard({ d, send, ...status }: { d: Decision; send: Send } & Status) {
  const project = useStore((s) => s.projects[d.project_id]);
  const open = d.state === "open";
  return (
    <article className="dc dc-inline" data-testid="decision-card" aria-label={`Decision ${decisionLabel(d)}`}>
      <DecisionMeta d={d} />
      <div className="dc-q">{d.question}</div>
      {d.brief?.context && <p className="dc-context clamp">{d.brief.context}</p>}
      {open ? (
        <div className="dc-buttons">
          {sortedOptions(d).map((o) => (
            <Button key={o.label} kind={isRecommended(d, o) ? "primary" : "secondary"} disabled={status.busy}
              title={o.consequence ?? undefined} onClick={() => void send(o.label)}>{o.label}</Button>
          ))}
          <a className="ui-btn quiet" href={href({ name: "decisions", project: d.project_id, id: d.id })}>
            {options(d).length ? "Open with evidence" : "Answer"}
          </a>
          <Problem {...status} />
        </div>
      ) : (
        <AnswerLine d={d} />
      )}
      {project && <span className="sr-only">in {project.name}</span>}
    </article>
  );
}

function PhoneCard({ d, send, ...status }: { d: Decision; send: Send } & Status) {
  const project = useStore((s) => s.projects[d.project_id]);
  const tasks = useStore((s) => s.tasks);
  const first = d.brief?.context?.split(/(?<=\.)\s/)[0];
  return (
    <article className="dc dc-phone" data-testid="decision-card">
      <span className="dc-kicker">{project?.name ?? d.project_id} · {askedBy(d, tasks)} · decision</span>
      <span className="dc-q">{d.question}</span>
      {first && <span className="dc-context">{first}</span>}
      {d.state === "open" ? (
        <span className="dc-buttons">
          {sortedOptions(d).slice(0, 2).map((o) => (
            <Button key={o.label} kind={isRecommended(d, o) ? "primary" : "secondary"} disabled={status.busy}
              onClick={() => void send(o.label)}>{o.label}</Button>
          ))}
          {!options(d).length && (
            <a className="ui-btn secondary" href={href({ name: "decisions", project: d.project_id, id: d.id })}>Answer</a>
          )}
          <Problem {...status} />
        </span>
      ) : <AnswerLine d={d} />}
    </article>
  );
}

function AnswerLine({ d }: { d: Decision }) {
  return (
    <div className="dc-answered" data-testid="decision-answer">
      <b>{d.answer ?? "Answered elsewhere"}</b>
      {d.answered_by && <span className="faint"> · {d.answered_by}</span>}
      {d.outcome && <div className="faint">{d.outcome}</div>}
    </div>
  );
}

function FullCard({ d, send, answerBox, ...status }: {
  d: Decision; send: Send; answerBox?: React.RefObject<HTMLTextAreaElement>;
} & Status) {
  return (
    <article className="dc dc-full" data-testid="decision-detail" aria-label={`Decision ${decisionLabel(d)}`}>
      {d.state === "acted" ? null : <DecisionProgress d={d} />}
      <div className="dc-head">
        {d.state === "open" ? <DecisionMeta d={d} /> : <LogTags d={d} />}
        <h2 className="dc-q">{d.question}</h2>
        {d.brief?.context && <p className="dc-context">{d.brief.context}</p>}
      </div>
      {d.state === "open"
        ? <AnswerForm d={d} send={send} answerBox={answerBox} {...status} />
        : <LogEntry d={d} />}
    </article>
  );
}

function AnswerForm({ d, send, answerBox, ...status }: {
  d: Decision; send: Send; answerBox?: React.RefObject<HTMLTextAreaElement>;
} & Status) {
  const opts = options(d);
  const [pick, setPick] = useState<string>(d.brief?.recommended && opts.some((o) => o.label === d.brief?.recommended)
    ? d.brief.recommended : opts[0]?.label ?? OTHER);
  const [other, setOther] = useState("");
  const [why, setWhy] = useState("");
  const [makeRule, setMakeRule] = useState(false);
  const [rule, setRule] = useState("");
  const answer = pick === OTHER ? other : pick;
  const submit = () => void send(answer, { why, make_rule: makeRule ? rule || ruleDraft(d, answer) : undefined });
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); submit(); }
  };
  return (
    <form className="dc-form" onSubmit={(e) => { e.preventDefault(); submit(); }} onKeyDown={onKey}>
      {opts.length > 0 && (
        <fieldset className="dc-options">
          <legend>Options</legend>
          {opts.map((o) => (
            <label key={o.label} className={"dc-option" + (pick === o.label ? " on" : "")}>
              <input type="radio" name={`opt-${d.id}`} checked={pick === o.label} onChange={() => setPick(o.label)} />
              <span className="dc-option-text">
                <span className="dc-option-label">{o.label}{isRecommended(d, o) && <span className="dc-badge">Recommended</span>}</span>
                {o.consequence && <span className="dc-consequence">{o.consequence}</span>}
                {isRecommended(d, o) && d.brief?.recommended_why && <span className="dc-why">Why: {d.brief.recommended_why}</span>}
              </span>
            </label>
          ))}
          <label className={"dc-option" + (pick === OTHER ? " on" : "")}>
            <input type="radio" name={`opt-${d.id}`} checked={pick === OTHER} onChange={() => setPick(OTHER)} />
            <span className="dc-option-text">
              <span className="dc-option-label">Something else</span>
              <span className="dc-consequence">Write your own answer below.</span>
            </span>
          </label>
        </fieldset>
      )}
      {pick === OTHER && (
        <div className="dc-field">
          <label htmlFor={`answer-${d.id}`}>Your answer</label>
          <textarea id={`answer-${d.id}`} ref={answerBox} aria-label="Answer" rows={3} value={other}
            placeholder="What should happen?" onChange={(e) => setOther(e.target.value)} />
        </div>
      )}
      <div className="dc-field">
        <label htmlFor={`why-${d.id}`}>Why (optional, goes in the log)</label>
        <textarea id={`why-${d.id}`} ref={pick === OTHER ? undefined : answerBox} rows={2} value={why}
          placeholder="Anything future you or the agents should know about this call" onChange={(e) => setWhy(e.target.value)} />
      </div>
      <label className="dc-rule">
        <input type="checkbox" checked={makeRule} onChange={(e) => {
          setMakeRule(e.target.checked);
          if (e.target.checked && !rule) setRule(ruleDraft(d, answer));
        }} />
        <span>
          <span>Make this a standing rule</span>
          <span className="faint small-text">Matching questions are decided without asking, and each one is logged under this decision. You can revoke it in the log.</span>
        </span>
      </label>
      {makeRule && (
        <div className="dc-field">
          <label htmlFor={`rule-${d.id}`}>The rule</label>
          <input id={`rule-${d.id}`} value={rule} onChange={(e) => setRule(e.target.value)} />
        </div>
      )}
      <div className="dc-send">
        <Button kind="primary" type="submit" disabled={status.busy || !answer.trim()}>{status.busy ? "Sending…" : "Send answer"}</Button>
        <span className="faint small-text">⌘↵ · the coordinator acts on it right away, and the log records that you answered</span>
        <Problem {...status} />
      </div>
    </form>
  );
}

/** The line over a logged decision's question: its number, what kind of call it was, its bead, how often its rule applied. */
function LogTags({ d }: { d: Decision }) {
  const rule = useStore((s) => (d.made_rule_id ? s.rules[d.made_rule_id] : undefined));
  const meta = [
    d.bead_id && `Beads ${d.bead_id}`,
    rule && `applied ${rule.applied} ${rule.applied === 1 ? "time" : "times"}`,
  ].filter(Boolean).join(" · ");
  return (
    <div className="dc-tags" data-testid="decision-tags">
      <span className="dc-num">{decisionLabel(d)}</span>
      {rule && <span className={"dc-badge " + (rule.revoked_at ? "revoked" : "rule")}>{rule.revoked_at ? "Rule revoked" : "Standing rule"}</span>}
      {decidedByAgent(d) && <span className="dc-badge agent">Decided by an agent</span>}
      {meta && <span className="dc-meta">{meta}</span>}
    </div>
  );
}

function LogEntry({ d }: { d: Decision }) {
  const tasks = useStore((s) => s.tasks);
  const rules = useStore((s) => s.rules);
  const all = useStore((s) => s.decisions);
  const rule = d.made_rule_id ? rules[d.made_rule_id] : undefined;
  const under = d.rule_id ? rules[d.rule_id] : undefined;
  const underFrom = under?.decision_id ? all[under.decision_id] : undefined;
  const uses = usesOf(d, Object.values(all));
  const [err, setErr] = useState<string | null>(null);
  // The rule's new words while it is being changed.
  const [draft, setDraft] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const revoke = async () => {
    if (!rule) return;
    setErr(null);
    try { upsertRule(await api.revokeRule(rule.id)); setDraft(null); } catch (e) { setErr(errText(e)); }
  };
  const change = async () => {
    if (!rule || !draft?.trim() || busy) return;
    setErr(null); setBusy(true);
    try { upsertRule(await api.changeRule(rule.id, draft.trim())); setDraft(null); }
    catch (e) { setErr(errText(e)); }
    finally { setBusy(false); }
  };
  const via = answeredVia(d.answered_via);
  return (
    <div className="dc-log" data-testid="decision-answer">
      <dl className="dc-dl">
        <dt>Asked</dt><dd>by {askedBy(d, tasks)}, {ago(d.opened_at)}</dd>
        <dt>Answer</dt><dd>{d.answer ?? <span className="faint">Answered outside Quark</span>}</dd>
        {d.answered_by && <><dt>By</dt><dd>Answered by {d.answered_by}{via && <>, {via}</>}{d.answered_at && <> · {ago(d.answered_at)}</>}</dd></>}
        {d.answer_why && <><dt>Why</dt><dd>{d.answer_why}</dd></>}
        <dt>What happened</dt><dd>{d.outcome ?? <span className="faint">{d.state === "answered" ? "Waiting for the asker to act on it." : "Not recorded."}</span>}</dd>
        {under && <><dt>Under rule</dt><dd>{underFrom
          ? <a href={href({ name: "decisions", project: underFrom.project_id, id: underFrom.id })}>{decisionLabel(underFrom)}</a>
          : under.text}</dd></>}
        {rule && <><dt>The rule</dt><dd>{rule.text}{rule.changed_at && <span className="faint">{" "}· changed{rule.changed_by && <> by {rule.changed_by}</>} {ago(rule.changed_at)}</span>}</dd></>}
        {d.bead_id && <><dt>Where it lives</dt><dd>
          Beads record <a href={href({ name: "issues", project: d.project_id, id: d.bead_id })}>{d.bead_id}</a> in the Project's database,
          so every agent sees it with the Project's issues{rule && !rule.revoked_at && ", and the rule as a memory"}.
        </dd></>}
      </dl>
      {uses.length > 0 && (
        <div className="dc-uses">
          <span className="ui-section-label"><span>Recent uses</span></span>
          {uses.slice(0, 5).map((u) => (
            <a key={u.id} href={href({ name: "decisions", project: u.project_id, id: u.id })}>
              {decisionLabel(u)} · {u.question} · {ago(u.answered_at ?? u.opened_at)}
            </a>
          ))}
        </div>
      )}
      {rule && !rule.revoked_at && draft !== null && (
        <form className="dc-field" onSubmit={(e) => { e.preventDefault(); void change(); }}>
          <label htmlFor={`change-${rule.id}`}>The rule, in new words</label>
          <input id={`change-${rule.id}`} value={draft} autoFocus onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Escape") setDraft(null); }} />
          <span className="faint small-text">Agents decide by the new words from now on. Decisions already made under it keep theirs.</span>
          <div className="dc-send">
            <Button kind="primary" type="submit" disabled={busy || !draft.trim() || draft.trim() === rule.text}>{busy ? "Saving…" : "Save the rule"}</Button>
            <Button onClick={() => setDraft(null)}>Cancel</Button>
          </div>
        </form>
      )}
      {rule && !rule.revoked_at && (
        <div className="dc-send">
          {draft === null && <Button onClick={() => setDraft(rule.text)}>Change the rule</Button>}
          <Button onClick={() => void revoke()} className="danger">Revoke the rule</Button>
          {err && <span className="bad small-text" role="alert">{err}</span>}
        </div>
      )}
    </div>
  );
}

/** The evidence column beside a full card: links, what it blocks, and earlier calls under the same rule. */
export function DecisionEvidence({ d }: { d: Decision }) {
  const tasks = useStore((s) => s.tasks);
  const all = useStore((s) => s.decisions);
  const links = d.brief?.evidence ?? [];
  const blocks = d.brief?.blocks ?? [];
  const related = Object.values(all).filter((x) => x.id !== d.id && x.project_id === d.project_id && x.state !== "open"
    && ((d.task_id && x.task_id === d.task_id) || (d.rule_id && (x.made_rule_id === d.rule_id || x.rule_id === d.rule_id))))
    .slice(0, 5);
  if (d.state !== "open" && !links.length && !blocks.length && !related.length) return null;
  return (
    <aside className="dc-evidence" aria-label="Evidence" data-testid="decision-evidence">
      <span className="ui-section-label"><span>Evidence</span></span>
      {links.length ? links.map((l) => (
        <div key={l.label + (l.url ?? "")} className="dc-evidence-row">
          {l.url ? <a href={l.url} target="_blank" rel="noreferrer">{l.label}</a> : <span>{l.label}</span>}
        </div>
      )) : <span className="faint small-text">The asker attached no evidence.</span>}
      {blocks.length > 0 && <>
        <span className="ui-section-label"><span>Waiting on the answer</span></span>
        {blocks.map((b) => (
          <div key={b} className="dc-evidence-row">
            {/^https?:/.test(b) ? <a href={b} target="_blank" rel="noreferrer">{blockLabels({ ...d, brief: { blocks: [b] } }, tasks)[0]}</a>
              : tasks[b] ? <a href={href({ name: "task", id: b })}>{tasks[b].title}</a> : <span>{b}</span>}
          </div>
        ))}
      </>}
      {related.length > 0 && <>
        <span className="ui-section-label"><span>Earlier calls like this</span></span>
        {related.map((x) => (
          <a key={x.id} href={href({ name: "decisions", project: x.project_id, id: x.id })}>
            {decisionLabel(x)} · {x.question} <span className="faint">· {x.answer ?? "answered"}</span>
          </a>
        ))}
      </>}
    </aside>
  );
}
