// "Why this agent" (ADR-11, quark#27): the dispatch record of each spawn of a task's worker,
// newest first. Records stay after the task ends.
import React, { useEffect, useState } from "react";
import { DispatchRecord, DispatchStatus } from "../api";
import { getState, loadAccounts, loadDispatch, useStore } from "../store";
import { href } from "../nav";
import { ago, errText } from "../util";
import { Unavailable } from "./Unavailable";

const NONE: DispatchRecord[] = [];

export const DECIDER: Record<DispatchRecord["decided_by"], string> = {
  classifier: "classifier",
  coordinator: "coordinator",
  default_rule: "default rule",
  relaunch: "relaunch",
};

const STATUS: Record<DispatchStatus, { label: string; cls: string }> = {
  clear: { label: "clear", cls: "green" },
  ambiguous: { label: "ambiguous", cls: "yellow" },
  escalate: { label: "escalated", cls: "yellow" },
  error: { label: "error", cls: "red" },
  off: { label: "no classifier", cls: "" },
  not_consulted: { label: "not consulted", cls: "" },
};

export function WhyThisAgent({ taskId }: { taskId: string }) {
  const records = useStore((s) => s.dispatch[taskId]) ?? NONE;
  const projectId = useStore((s) => s.tasks[taskId]?.project_id);
  const [status, setStatus] = useState<"loading" | "ok" | "unavailable" | "error">("loading");
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    loadDispatch(taskId).then(setStatus).catch((e) => { setStatus("error"); setErr(errText(e)); });
  }, [taskId]);
  // Account labels, once per session; the panel falls back to the account id.
  useEffect(() => {
    if (getState().accountsAvailable === null) loadAccounts().catch(() => undefined);
  }, []);

  if (status === "unavailable") return <Unavailable what="Why this agent" endpoint={`GET /v1/tasks/${taskId}/dispatch`} />;
  const newest = [...records].reverse();
  return (
    <div className="why" data-testid="why-this-agent">
      {status === "error" && <div className="form-error">{err}</div>}
      {status === "loading" && <div className="empty">Loading…</div>}
      {status === "ok" && !records.length && <div className="empty">No dispatch recorded yet: this task's worker has not started.</div>}
      {newest.map((r, i) => <RecordView key={r.id} r={r} latest={i === 0} projectId={projectId} />)}
    </div>
  );
}

function RecordView({ r, latest, projectId }: { r: DispatchRecord; latest: boolean; projectId?: string }) {
  const account = useStore((s) => (r.chosen.account ? s.accounts[r.chosen.account] : undefined));
  const st = STATUS[r.resolution.status] ?? { label: r.resolution.status, cls: "" };
  const body = (
    <>
      <span className="why-chosen">{[r.chosen.harness, r.chosen.model, r.chosen.effort && `${r.chosen.effort} effort`].filter(Boolean).join(" · ")}</span>
      <p className="why-summary" data-testid="why-summary">{r.summary}</p>
      <dl className="why-facts">
        <dt>Agent</dt>
        <dd className="mono" data-testid="why-agent">
          {r.chosen.harness}
          {r.chosen.model && <> · {r.chosen.model}</>}
          {r.chosen.effort && <> · {r.chosen.effort} effort</>}
          <span className="faint"> · account {account?.label ?? r.chosen.account ?? "default"}</span>
        </dd>
        <dt>Decided by</dt>
        <dd><span className="pill accent">{DECIDER[r.decided_by] ?? r.decided_by}</span></dd>
        <dt>Rule</dt>
        <dd>
          {r.rule ? <><span className="mono">{r.rule.id}</span>{r.rule.when && <span className="faint"> · {r.rule.when}</span>}</> : <span className="faint">none matched</span>}
          {projectId && <> · <a href={href({ name: "dispatch", project: projectId })} data-testid="why-edit-rule">{r.rule ? "Edit this rule" : "Edit the rules"}</a></>}
        </dd>
        <dt>Classifier</dt>
        <dd data-testid="why-classifier">
          {r.classifier.provider === "none" ? <span className="faint">none (the coordinator picked)</span> : (
            <>
              <span className="mono">{r.classifier.provider}</span>
              {r.classifier.model && <span className="mono"> · {r.classifier.model}</span>}
              {r.classifier.confidence != null && <> · {Math.round(r.classifier.confidence * 100)}% confidence</>}
            </>
          )}
        </dd>
        <dt>Resolution</dt>
        <dd>
          <span className={"pill " + st.cls}>{st.label}</span>
          {r.resolution.reason && <span className="faint"> {r.resolution.reason}</span>}
        </dd>
      </dl>
      {r.candidates.length > 0 && (
        <div className="gate" data-testid="why-candidates">
          <div className="gate-head"><b>Candidates</b><span className="faint">{r.candidates.filter((c) => c.passed).length} of {r.candidates.length} passed</span></div>
          {r.candidates.map((c, i) => {
            const chosen = c.harness === r.chosen.harness && (c.model ?? null) === (r.chosen.model ?? null);
            return (
              <div key={i} className="why-cand" data-testid="why-candidate">
                <span className={c.passed ? "green" : "red"} aria-label={c.passed ? "passed" : "failed"}>{c.passed ? "✓" : "✗"}</span>
                <span className="mono">{c.harness}{c.model ? `:${c.model}` : ""}</span>
                {chosen && <span className="pill accent">chosen</span>}
                <span className={c.passed ? "" : "red"}>{c.reason}</span>
                {c.evidence && <span className="faint mono small-text why-evidence">{c.evidence}</span>}
              </div>
            );
          })}
        </div>
      )}
      {r.resolution.notes.length > 0 && (
        <ul className="why-notes">{r.resolution.notes.map((n, i) => <li key={i}>{n}</li>)}</ul>
      )}
      {r.resolution.output && (
        <details className="why-output">
          <summary className="faint">Resolution output</summary>
          <pre>{r.resolution.output}</pre>
        </details>
      )}
    </>
  );
  const head = (
    <>
      <b>{r.trigger === "relaunch" ? "Relaunch" : "Spawn"}</b>
      <span className="faint">{ago(r.recorded_at)}</span>
    </>
  );
  if (latest) return <section className="why-record"><div className="why-head">{head}</div>{body}</section>;
  return (
    <details className="why-record earlier">
      <summary className="why-head">{head}<span className="faint ellipsis">{r.summary}</span></summary>
      {body}
    </details>
  );
}
