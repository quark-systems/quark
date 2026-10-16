// The PR view's Overview tab (quark-atn): the PR's description rendered as Markdown, then its
// comments and reviews in time order, with inline review comments as threads that link into the
// Diff tab at their line. It refetches on every comment, review or PR event for the PR.
import { useEffect, useState } from "react";
import { api, NotAvailable, PrComment, PrConversation as Conversation, PrConversationEntry, PrThread, PullRequest } from "../api";
import { renderMarkdown } from "../markdown";
import { reviewLabel } from "../prs";
import { useStore } from "../store";
import { ago, errText } from "../util";
import { Unavailable } from "./Unavailable";

export interface DiffTarget { path: string; line: number; side: "new" | "old" }

export function PrConversation({ pr, onOpenLine }: { pr: PullRequest; onOpenLine: (t: DiffTarget) => void }) {
  const activity = useStore((s) => s.prActivity[pr.id] ?? 0);
  const [conv, setConv] = useState<Conversation | null>(null);
  const [status, setStatus] = useState<"loading" | "ok" | "unavailable" | "error">("loading");
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api.pullRequestConversation(pr.id).then(
      (c) => { if (live) { setConv(c); setStatus("ok"); } },
      (e) => {
        if (!live) return;
        if (e instanceof NotAvailable) setStatus("unavailable");
        else { setStatus((s) => (s === "ok" ? s : "error")); setErr(errText(e)); }
      },
    );
    return () => { live = false; };
  }, [pr.id, activity]);

  const body = pr.body?.trim();
  return (
    <div className="pr-overview" data-testid="pr-overview">
      <section className="pr-description" aria-label="Description" data-testid="pr-body">
        <div className="pr-entry-head"><b>{pr.author ?? "someone"}</b> <span className="faint">opened this {pr.opened_at && <Time at={pr.opened_at} />}</span></div>
        {body
          ? <div className="md" dangerouslySetInnerHTML={{ __html: renderMarkdown(body) }} />
          : <div className="faint" data-testid="pr-no-body">{pr.body == null ? "The description has not been read yet." : "No description."}</div>}
      </section>
      {status === "unavailable" && <Unavailable what="The PR conversation" endpoint={`GET /v1/pull-requests/${pr.id}/comments`} />}
      {status === "error" && <div className="form-error">{err}</div>}
      {status === "loading" && <div className="empty">Loading comments…</div>}
      {conv && (
        <div className="pr-timeline" data-testid="pr-conversation">
          {conv.entries.map((e, i) => <Entry key={entryKey(e, i)} e={e} onOpenLine={onOpenLine} />)}
          {!conv.entries.length && <div className="faint pr-quiet" data-testid="pr-no-comments">No comments or reviews yet.</div>}
        </div>
      )}
    </div>
  );
}

const entryKey = (e: PrConversationEntry, i: number) => e.comment?.id ?? e.review?.id ?? e.threads[0]?.id ?? String(i);

function Entry({ e, onOpenLine }: { e: PrConversationEntry; onOpenLine: (t: DiffTarget) => void }) {
  if (e.kind === "comment" && e.comment) {
    return <section className="pr-entry" data-testid="pr-comment"><Comment c={e.comment} /></section>;
  }
  if (e.kind === "review" && e.review) {
    const r = e.review;
    const l = reviewLabel[r.state] ?? { label: r.state, cls: "" };
    return (
      <section className={"pr-entry review-entry " + l.cls} data-testid="pr-review">
        <div className="pr-entry-head">
          <b>{r.author ?? "someone"}</b> <span className={"pr-review-state " + l.cls}>{l.label}</span>
          {r.submitted_at && <span className="faint"> <Time at={r.submitted_at} /></span>}
        </div>
        {r.body.trim() && <div className="md" dangerouslySetInnerHTML={{ __html: renderMarkdown(r.body) }} />}
        {e.threads.map((t) => <Thread key={t.id} t={t} onOpenLine={onOpenLine} />)}
      </section>
    );
  }
  return <section className="pr-entry">{e.threads.map((t) => <Thread key={t.id} t={t} onOpenLine={onOpenLine} />)}</section>;
}

function Thread({ t, onOpenLine }: { t: PrThread; onOpenLine: (t: DiffTarget) => void }) {
  const line = t.line ?? t.original_line;
  return (
    <div className={"pr-thread" + (t.outdated ? " outdated" : "")} data-testid="pr-thread">
      <div className="pr-thread-head">
        {t.line != null ? (
          <button className="linklike mono" onClick={() => onOpenLine({ path: t.path, line: t.line!, side: t.side })}
            title="Show this line in the diff" data-testid="pr-thread-link">
            {t.path}:{t.line}
          </button>
        ) : <span className="mono">{t.path}{line != null ? `:${line}` : ""}</span>}
        {t.outdated && <span className="pill">outdated</span>}
      </div>
      {t.comments.map((c) => <div key={c.id} className="pr-thread-comment"><Comment c={c} /></div>)}
    </div>
  );
}

function Comment({ c }: { c: PrComment }) {
  const edited = !!c.updated_at && !!c.created_at && c.updated_at > c.created_at;
  return (
    <>
      <div className="pr-entry-head">
        <b>{c.author ?? "someone"}</b>
        {c.created_at && <span className="faint"> <Time at={c.created_at} /></span>}
        {edited && <span className="faint" title={`Edited ${new Date(c.updated_at!).toLocaleString()}`}> · edited</span>}
      </div>
      {c.body.trim()
        ? <div className="md" dangerouslySetInnerHTML={{ __html: renderMarkdown(c.body) }} />
        : <div className="faint">No text.</div>}
    </>
  );
}

function Time({ at }: { at: string }) {
  return <span title={new Date(at).toLocaleString()}>{ago(at)}</span>;
}
