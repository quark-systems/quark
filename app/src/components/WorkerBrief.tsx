// The worker view's Brief tab (quark-ouo): the issue a worker was dispatched for, the brief it
// received, and how its work is delivered. Finished workers keep theirs.
import { useEffect, useState } from "react";
import { api, NotAvailable, TaskBrief, TaskDelivery } from "../api";
import { githubNumber, statusLabel } from "../issues";
import { renderMarkdown } from "../markdown";
import { externalLink } from "../links";
import { href } from "../nav";
import { useLabels } from "../persona";
import { useStore } from "../store";
import { ago, errText } from "../util";
import { Unavailable } from "./Unavailable";

export const DELIVERY: Record<TaskDelivery, string> = {
  gated: "Checked by the verification gate before its PR opens",
  direct: "Opens its PR directly; checks are the only gate",
  local_only: "Stays in its working copy; no PR",
};

export function WorkerBrief({ taskId }: { taskId: string }) {
  const projectId = useStore((s) => s.tasks[taskId]?.project_id);
  const L = useLabels(projectId);
  const [b, setB] = useState<TaskBrief | null>(null);
  const [status, setStatus] = useState<"loading" | "ok" | "unavailable" | "error">("loading");
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setStatus("loading");
    api.brief(taskId).then(
      (r) => { if (live) { setB(r); setStatus("ok"); } },
      (e) => { if (live) { setStatus(e instanceof NotAvailable ? "unavailable" : "error"); setErr(errText(e)); } },
    );
    return () => { live = false; };
  }, [taskId]);

  if (status === "unavailable") return <Unavailable what="The brief" endpoint={`GET /v1/tasks/${taskId}/brief`} />;
  if (status === "loading") return <div className="brief"><div className="empty">Loading…</div></div>;
  if (status === "error" || !b) return <div className="brief"><div className="form-error">{err}</div></div>;

  const issue = b.issue;
  const gh = githubNumber(issue?.external_ref);
  const brief = b.brief;
  const coordinator = L.role("coordinator");
  return (
    <div className="brief" data-testid="worker-brief">
      <section className="brief-section" aria-label="Issue">
        <div className="brief-label">Issue</div>
        {issue ? (
          <div className="brief-issue" data-testid="brief-issue">
            <div className="issue-facts">
              {projectId
                ? <a className="mono" href={href({ name: "issues", project: projectId, id: issue.id })}>{issue.id}</a>
                : <span className="mono">{issue.id}</span>}
              <span>{issue.issue_type} · P{issue.priority}</span>
              <span>{statusLabel(issue.status)}</span>
              {gh && <span>GitHub <a href={issue.external_ref!} target="_blank" rel="noreferrer">{gh}</a></span>}
              {issue.labels.map((l) => <span key={l} className="pill">{l}</span>)}
            </div>
            <h3>{issue.title}</h3>
            {issue.description && <div className="md brief-md" dangerouslySetInnerHTML={{ __html: renderMarkdown(issue.description) }} />}
          </div>
        ) : (
          <div className="empty-inline faint" data-testid="brief-no-issue">No issue: this worker was not started for one.</div>
        )}
      </section>

      <section className="brief-section" aria-label="Dispatch">
        <div className="brief-label">Dispatch</div>
        <dl className="why-facts" data-testid="brief-dispatch">
          <dt>Started</dt>
          <dd>{b.dispatched_at ? <span title={new Date(b.dispatched_at).toLocaleString()}>{ago(b.dispatched_at)}</span> : <span className="faint">not recorded</span>}
            {b.dispatched_by && <span className="faint"> · by the {b.dispatched_by === "coordinator" ? coordinator : b.dispatched_by}</span>}</dd>
          <dt>Delivery</dt>
          <dd>{b.delivery ? DELIVERY[b.delivery] ?? b.delivery : <span className="faint">not recorded</span>}</dd>
          <dt>Merging</dt>
          <dd>{b.merges_without_asking == null ? <span className="faint">not recorded</span>
            : b.merges_without_asking ? "Merges once green, without asking you" : "Asks you before merging"}</dd>
          {b.pull_request_url && <><dt>PR</dt><dd><a {...externalLink(b.pull_request_url)}>{b.pull_request_url.replace(/^https:\/\/github\.com\//, "")}</a></dd></>}
        </dl>
      </section>

      <section className="brief-section" aria-label="Brief">
        <div className="brief-label">Brief</div>
        {!brief ? (
          <div className="empty-inline faint" data-testid="brief-none">No brief recorded for this worker.</div>
        ) : (
          <>
            {brief.intent && (
              <div className="brief-part" data-testid="brief-intent">
                <div className="brief-part-head">What you asked for</div>
                <div className="md brief-md" dangerouslySetInnerHTML={{ __html: renderMarkdown(brief.intent) }} />
              </div>
            )}
            {brief.spec && (
              <div className="brief-part" data-testid="brief-spec">
                <div className="brief-part-head">The {coordinator}'s spec</div>
                <div className="md brief-md" dangerouslySetInnerHTML={{ __html: renderMarkdown(brief.spec) }} />
              </div>
            )}
            <details className="brief-full" open={!brief.intent && !brief.spec} data-testid="brief-full">
              <summary className="faint">{brief.intent || brief.spec ? "Whole brief" : "Brief"}</summary>
              <div className="md brief-md" dangerouslySetInnerHTML={{ __html: renderMarkdown(brief.markdown) }} />
            </details>
            {brief.launch && (
              <details className="brief-full" data-testid="brief-launch">
                <summary className="faint">As the worker was started</summary>
                <pre className="brief-launch">{brief.launch}</pre>
              </details>
            )}
          </>
        )}
      </section>
    </div>
  );
}
