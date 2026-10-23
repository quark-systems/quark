// One pull request: its description and conversation (quark-atn), the diff with line comments
// for the owning worker, and the verification evidence (ADR-15), on the left; gates, checks, reviews and the Project's standing approval on
// the right; approve and merge in the header.
import React, { useEffect, useMemo, useState } from "react";
import { api, ApiError, Evidence, NotAvailable, PullRequest } from "../api";
import { href } from "../nav";
import { externalLink } from "../links";
import { mergePullRequest, refreshPullRequest, useStore } from "../store";
import { ago, errText } from "../util";
import { DiffFile, parseUnifiedDiff } from "../components/diff/model";
import { DiffFocus, DiffView, LineComment, OnComment } from "../components/diff/DiffView";
import { DiffTarget, PrConversation } from "../components/PrConversation";
import { FileSource } from "../components/diff/ImageDiff";
import { StandingApproval } from "../components/StandingApproval";
import { EvidencePanel, EvidenceSummary } from "../components/Evidence";
import { Unavailable } from "../components/Unavailable";
import { evidenceOutcome, checkOutcome, checksMeta, mergeBlocker, prStateMeta, prTitle, reviewLabel, reviewMeta, sortChecks } from "../prs";
import { useDraft } from "../drafts";

export function PullRequestView({ id }: { id: string }) {
  const pr = useStore((s) => s.pullRequests[id]);
  const project = useStore((s) => (pr ? s.projects[pr.project_id] : undefined));
  const activity = useStore((s) => s.prActivity[id] ?? 0);
  const connected = useStore((s) => s.connected);
  const [status, setStatus] = useState<"loading" | "ok" | "missing" | "unavailable" | "error">(pr ? "ok" : "loading");
  const [err, setErr] = useState<string | null>(null);
  const [comments, setComments] = useState<LineComment[]>([]);
  const [tab, setTab] = useState<"overview" | "diff" | "evidence">("overview");
  const [focus, setFocus] = useState<DiffFocus | null>(null);
  const mergeNote = useStore((s) => s.merges[id]?.note ?? null);
  const openLine = (t: DiffTarget) => {
    setFocus((f) => ({ ...t, n: (f?.n ?? 0) + 1 }));
    setTab("diff");
  };

  // Refetch on open and whenever a check, review or partial PR event names this PR.
  useEffect(() => {
    refreshPullRequest(id).then(() => setStatus("ok")).catch((e) => {
      if (e instanceof NotAvailable) setStatus("unavailable");
      else if (e instanceof ApiError && e.status === 404) setStatus("missing");
      else { setStatus((s) => (s === "ok" ? s : "error")); setErr(errText(e)); }
    });
  }, [id, activity, connected]);

  const comment = (c: { text: string; path?: string; line?: number; side?: "new" | "old" }) =>
    api.commentPullRequest(id, c).catch((e) => {
      throw e instanceof NotAvailable ? new Error("Commenting is not available from this daemon yet") : e;
    });
  const onComment: OnComment = async ({ body, ...at }) => {
    await comment({ text: body, ...at });
    setComments((cs) => [...cs, { body, ...at }]);
  };

  if (!pr) {
    if (status === "unavailable") return <div className="screen scroll"><Unavailable what="The PR center" endpoint={`GET /v1/pull-requests/${id}`} /></div>;
    return (
      <div className="empty">
        {status === "missing" ? "This pull request does not exist on the daemon." : status === "error" ? err : connected ? "Loading…" : "Waiting for the daemon…"}
      </div>
    );
  }
  const st = prStateMeta[pr.state];

  return (
    <>
      <div className="header">
        <a className="crumb" href={href({ name: "prs" })}>Pull requests</a>
        <span className="faint">/</span>
        <h1 className="ellipsis" title={prTitle(pr)}>{prTitle(pr)}</h1>
        <span className={"pill " + st.cls} data-testid="pr-state">{st.label}</span>
        <a className="pill mono" {...externalLink(pr.url)} data-testid="pr-forge-link">{pr.repo}#{pr.number}</a>
        <span className="spacer" />
        <PrActions pr={pr} />
      </div>
      <div className="state-note">
        <span className="mono">{pr.head_ref ?? "?"} → {pr.base_ref ?? "?"}</span>
        {pr.author && <> · by {pr.author}</>}
        {project && <> · <a href={href({ name: "project", id: project.id })}>{project.name}</a></>}
        {pr.task_id && <> · <a href={href({ name: "task", id: pr.task_id })}>worker</a></>}
        {pr.updated_at && <span className="faint"> · updated {ago(pr.updated_at)}</span>}
      </div>
      {mergeNote && !(mergeNote.ok && pr.state === "merged") && (
        <div className={"state-note " + (mergeNote.ok ? "ok" : "bad")} role={mergeNote.ok ? "status" : "alert"} data-testid="merge-result">{mergeNote.text}</div>
      )}
      {pr.sync_error && <div className="state-note bad">Could not read the forge{pr.synced_at ? ` (last read ${ago(pr.synced_at)})` : ""}: {pr.sync_error}</div>}
      <div className="screen pr-detail">
        <section className="pr-diff">
          <div className="tabs" role="tablist">
            <button role="tab" aria-selected={tab === "overview"} className={tab === "overview" ? "on" : ""} onClick={() => setTab("overview")}>Overview</button>
            <button role="tab" aria-selected={tab === "diff"} className={tab === "diff" ? "on" : ""} onClick={() => setTab("diff")}>Diff</button>
            <button role="tab" aria-selected={tab === "evidence"} className={tab === "evidence" ? "on" : ""} onClick={() => setTab("evidence")}
              data-testid="tab-evidence">
              Evidence{pr.evidence && <EvidenceBadge ev={pr.evidence} />}
            </button>
          </div>
          {/* All stay mounted so the diff, its scroll and open comment boxes survive a tab switch. */}
          <div className="tab-body" hidden={tab !== "overview"}>
            <PrConversation pr={pr} onOpenLine={openLine} />
          </div>
          <div className="tab-body" hidden={tab !== "diff"}>
            <DiffPanel pr={pr} comments={comments} focus={focus}
              onComment={pr.state === "merged" || pr.state === "closed" ? undefined : onComment} />
            {pr.state !== "merged" && pr.state !== "closed" && <PrCommentBox prId={pr.id} onSend={(text) => comment({ text })} />}
          </div>
          <div className="tab-body" hidden={tab !== "evidence"}>
            <EvidencePanel prId={pr.id} ev={pr.evidence} />
          </div>
        </section>
        <aside className="pr-info">
          <div className="panel-head">Verification gates</div>
          <EvidenceSummary ev={pr.evidence} onOpen={() => setTab("evidence")} />
          <ChecksPanel pr={pr} />
          <ReviewsPanel pr={pr} />
          {project && (
            <>
              <div className="panel-head">Standing approval</div>
              <div className="side-pad"><StandingApproval project={project} /></div>
            </>
          )}
        </aside>
      </div>
    </>
  );
}

function EvidenceBadge({ ev }: { ev: Evidence }) {
  const o = evidenceOutcome(ev.state);
  return <span className={"pr-glyph " + o.cls} title={ev.stale ? "For an older commit" : o.label}>{ev.stale ? "!" : o.glyph}</span>;
}

// ---- merge ----

/** The merge button. Its result shows under the header, where a long refusal has room. */
function PrActions({ pr }: { pr: PullRequest }) {
  const busy = useStore((s) => s.merges[pr.id]?.busy ?? false);
  const [confirming, setConfirming] = useState(false);
  const blocker = mergeBlocker(pr);

  useEffect(() => {
    if (!confirming) return;
    const t = setTimeout(() => setConfirming(false), 4000);
    return () => clearTimeout(t);
  }, [confirming]);

  const merge = () => { setConfirming(false); void mergePullRequest(pr.id); };

  if (pr.state === "merged") return <div className="controls"><span className="pill accent">merged {ago(pr.merged_at)}</span></div>;
  return (
    <div className="controls">
      {blocker && <span className="faint small-text ellipsis" data-testid="merge-blocker" title={blocker.reason}>{blocker.reason}</span>}
      {confirming ? (
        <button className="btn danger" onClick={merge} data-testid="confirm-merge">Merge anyway</button>
      ) : (
        <button className={"btn" + (blocker ? "" : " on")} disabled={busy || !!blocker?.hard} data-testid="merge"
          title="Approve and merge through the engine's guarded merge"
          onClick={() => (blocker ? setConfirming(true) : merge())}>
          {busy ? "Merging…" : "Approve and merge"}
        </button>
      )}
    </div>
  );
}

// ---- diff ----

function DiffPanel({ pr, comments, onComment, focus }: {
  pr: PullRequest; comments: LineComment[]; onComment?: OnComment; focus?: DiffFocus | null;
}) {
  const [files, setFiles] = useState<DiffFile[] | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [status, setStatus] = useState<"loading" | "ok" | "unavailable" | "error">("loading");
  const [err, setErr] = useState<string | null>(null);

  const source = useMemo<FileSource>(() => ({
    url: (path, side) => api.pullRequestFileUrl(pr.id, path, side),
    // GitHub shows the head version at its commit; the base one is on the PR's files page.
    external: (path, side) => pr.provider !== "github" ? null
      : side === "head" && pr.head_sha ? `https://github.com/${pr.repo}/blob/${pr.head_sha}/${path.split("/").map(encodeURIComponent).join("/")}`
      : `${pr.url}/files`,
  }), [pr.id, pr.provider, pr.repo, pr.head_sha, pr.url]);

  // The diff only changes with the head commit.
  useEffect(() => {
    let live = true;
    api.pullRequestDiff(pr.id)
      .then((d) => { if (live) { setFiles(parseUnifiedDiff(d.patch)); setTruncated(d.truncated); setStatus("ok"); } })
      .catch((e) => {
        if (!live) return;
        if (e instanceof NotAvailable) setStatus("unavailable");
        else { setStatus("error"); setErr(errText(e)); }
      });
    return () => { live = false; };
  }, [pr.id, pr.head_sha, pr.state]);

  if (status === "unavailable") return <Unavailable what="The PR diff" endpoint={`GET /v1/pull-requests/${pr.id}/diff`} />;
  return (
    <div className="diff-body" data-testid="pr-diff">
      {status === "error" && <div className="form-error">{err}</div>}
      {status === "loading" && <div className="empty">Loading the diff…</div>}
      {truncated && <div className="state-note">This diff was cut at the daemon's size limit.</div>}
      {onComment && files && files.length > 0 && <div className="faint small-text diff-hint">Click a line number to comment for the worker.</div>}
      {files && files.length > 0 && <DiffView files={files} comments={comments} onComment={onComment} focus={focus} source={source} />}
      {files && !files.length && <div className="empty">No textual changes.</div>}
    </div>
  );
}

function PrCommentBox({ prId, onSend }: { prId: string; onSend: (body: string) => Promise<void> }) {
  const [text, setText] = useDraft(`pr-comment:${prId}`);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  const send = async () => {
    const t = text.trim();
    if (!t || busy) return;
    setBusy(true); setNote(null);
    try { await onSend(t); setText(""); setNote({ ok: true, text: "Sent to the worker" }); }
    catch (e) { setNote({ ok: false, text: errText(e) }); }
    finally { setBusy(false); }
  };
  return (
    <div className="composer steer">
      <textarea rows={2} value={text} aria-label="Comment on the pull request"
        placeholder="Comment on the whole PR for the worker (Enter to send, Shift+Enter for a newline)"
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); void send(); }
        }} />
      <div className="steer-side">
        <button className="btn" onClick={() => void send()} disabled={busy || !text.trim()}>{busy ? "Sending…" : "Comment"}</button>
        {note && <span className={(note.ok ? "ok" : "bad") + " small-text"} role="status">{note.text}</span>}
      </div>
    </div>
  );
}

// ---- checks and reviews ----

function ChecksPanel({ pr }: { pr: PullRequest }) {
  const sorted = useMemo(() => sortChecks(pr.checks), [pr.checks]);
  const m = checksMeta[pr.checks_state];
  return (
    <>
      <div className="panel-head">Checks <span className={"pill " + m.cls}>{m.label}</span></div>
      <div className="check-list" data-testid="pr-checks">
        {sorted.map((c) => {
          const o = checkOutcome(c);
          return (
            <div className="check-row" key={c.name}>
              <span className={"pr-glyph " + o.cls}>{o.glyph}</span>
              {c.details_url ? <a className="ellipsis" href={c.details_url} target="_blank" rel="noreferrer">{c.name}</a> : <span className="ellipsis">{c.name}</span>}
              <span className={"faint small-text " + o.cls}>{o.label}</span>
            </div>
          );
        })}
        {!sorted.length && <div className="side-note faint">No checks reported.</div>}
      </div>
    </>
  );
}

function ReviewsPanel({ pr }: { pr: PullRequest }) {
  const m = reviewMeta[pr.review_decision];
  const sorted = useMemo(() => [...pr.reviews].sort((a, b) => (b.submitted_at ?? "").localeCompare(a.submitted_at ?? "")), [pr.reviews]);
  return (
    <>
      <div className="panel-head">Reviews <span className={"pill " + m.cls}>{m.label}</span></div>
      <div className="review-list" data-testid="pr-reviews">
        {sorted.map((r) => {
          const l = reviewLabel[r.state];
          return (
            <div className="review" key={r.id}>
              <div><b>{r.author ?? "someone"}</b> <span className={l.cls}>{l.label}</span> <span className="faint small-text">{ago(r.submitted_at)}</span></div>
              {r.body && <div className="review-body">{r.body}</div>}
            </div>
          );
        })}
        {!sorted.length && <div className="side-note faint">No reviews yet.</div>}
      </div>
    </>
  );
}
