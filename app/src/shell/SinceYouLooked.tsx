// The Conversation tab's right column: what changed since you last looked, what needs you,
// open PRs and recent decisions in this project. It only reads: the Overview tab marks things read.
import React, { useEffect, useMemo, useState } from "react";
import { api, Notice, OverviewDigest } from "../api";
import { renderMarkdownInline } from "../markdown";
import { href } from "../nav";
import { useStore } from "../store";
import { ago, workerNamed } from "../util";
import { SectionLabel, StatusDot, taskTone } from "../ui";
import { lastSeen, summarize } from "../screens/dashboard/summary";
import { useAttention } from "./NextAttention";
import { describeUpdate } from "./updates";
import { CommandButton } from "../components/CoordinatorCommand";

type Posted = Notice & { seq: number; ts: string };

/** One update: which worker, what happened and the detail, linking to the worker; or a digest that opens to list what waited. */
function Update({ notice: n }: { notice: Posted }) {
  const tasks = useStore((s) => s.tasks);
  const [open, setOpen] = useState(false);
  const task = n.task_id ? workerNamed(tasks, n.task_id) : undefined;
  const text = <span className="since-title md" dangerouslySetInnerHTML={{ __html: renderMarkdownInline(n.text) }} />;
  const when = <span className="since-kind" title={n.ts}>{ago(n.ts)}</span>;
  if (n.items?.length) {
    return (
      <div className="since-update" data-testid="since-update">
        <button className="since-item" aria-expanded={open} onClick={() => setOpen(!open)}>
          <StatusDot tone="idle" label="Digest" />{text}{when}
        </button>
        {open && (
          <ul className="since-digest">
            {n.items.map((i, k) => {
              const t = i.task_id ? workerNamed(tasks, i.task_id) : undefined;
              const line = <span className="md" dangerouslySetInnerHTML={{ __html: renderMarkdownInline(i.summary) }} />;
              return <li key={k}>{t ? <a href={href({ name: "task", id: t.id })}>{line}</a> : line}</li>;
            })}
          </ul>
        )}
      </div>
    );
  }
  const v = describeUpdate(n, task?.title);
  const body = (
    <>
      <StatusDot tone={v.tone} label={v.label} />
      <span className="since-body">
        <span className="since-head"><b>{v.label}</b>{v.who && <> · {v.who}</>}</span>
        {v.detail && <span className="since-detail md" dangerouslySetInnerHTML={{ __html: renderMarkdownInline(v.detail) }} />}
      </span>
      {when}
    </>
  );
  return task
    ? <a className="since-item since-update" href={href({ name: "task", id: task.id })} title={n.text} data-testid="since-update">{body}</a>
    : <div className="since-item since-update plain" title={n.text} data-testid="since-update">{body}</div>;
}

const KIND = { decision: "decision", pr: "check failed", worker: "worker stuck" } as const;

export function SinceYouLooked({ project: pid }: { project: string }) {
  const queue = useAttention();
  const prs = useStore((s) => s.pullRequests);
  const decisions = useStore((s) => s.decisions);
  const connected = useStore((s) => s.connected);
  const [digest, setDigest] = useState<OverviewDigest | null>(null);
  const seen = lastSeen(pid);

  useEffect(() => {
    let live = true;
    api.projectOverview(pid, seen ?? 0).then((o) => { if (live) setDigest(o.digest ?? null); }).catch(() => { if (live) setDigest(null); });
    return () => { live = false; };
  }, [pid, seen, connected]);

  const mine = queue.filter((i) => i.projectId === pid);
  const open = useMemo(() => Object.values(prs).filter((p) => p.project_id === pid && (p.state === "open" || p.state === "draft"))
    .sort((a, b) => (b.updated_at ?? "").localeCompare(a.updated_at ?? "")), [prs, pid]);
  const allNotices = useStore((s) => s.notices);
  const notices = useMemo(() => allNotices.filter((n) => n.project_id === pid).slice(-5).reverse(), [allNotices, pid]);
  const recent = useMemo(() => Object.values(decisions).filter((d) => d.project_id === pid && d.state === "answered")
    .sort((a, b) => (b.answered_at ?? "").localeCompare(a.answered_at ?? "")).slice(0, 3), [decisions, pid]);

  return (
    <aside className="since" aria-label="Since you looked" data-testid="since-you-looked">
      <section>
        <SectionLabel right={<CommandButton cid={pid} command="catch_up" />}>
          {!digest ? "Since you looked" : seen === null ? "Since the log began" : `Since you looked · ${ago(digest.from)}`}
        </SectionLabel>
        {digest && <p className="since-summary" data-testid="since-summary">{summarize(digest)} <a href={href({ name: "overview", project: pid })}>Overview</a></p>}
      </section>
      <section>
        <SectionLabel>Needs you</SectionLabel>
        {mine.length ? mine.map((i) => (
          <a key={i.key} className="since-item needs" href={href(i.route)} data-testid="since-needs">
            <StatusDot tone={i.kind === "decision" ? "needs-you" : "failed"} />
            <span className="since-title">{i.title}</span><span className="since-kind">{KIND[i.kind]}</span>
          </a>
        )) : <p className="since-none">Nothing needs you here.</p>}
      </section>
      <section>
        <SectionLabel>Open PRs</SectionLabel>
        {open.length ? open.map((p) => (
          <a key={p.id} className="since-item" href={href({ name: "pr", id: p.id })}>
            <StatusDot tone={p.checks_state === "failing" ? "failed" : p.checks_state === "passing" ? "ready" : "busy"} />
            <span className="since-title">{p.title ?? `${p.repo}#${p.number}`}</span><span className="since-kind">#{p.number}</span>
          </a>
        )) : <p className="since-none">No open PRs.</p>}
      </section>
      {notices.length > 0 && (
        <section data-testid="since-updates">
          <SectionLabel right={<a href={href({ name: "automation", project: pid })} title="Choose what you hear about and how">Choose updates</a>}>Updates</SectionLabel>
          {notices.map((n) => <Update key={n.seq} notice={n} />)}
        </section>
      )}
      {recent.length > 0 && (
        <section>
          <SectionLabel right={<a href={href({ name: "decisions", project: pid })}>All</a>}>Recent decisions</SectionLabel>
          {recent.map((d) => (
            <a key={d.id} className="since-item" href={href({ name: "decisions", project: pid, id: d.id })}>
              <span className="since-title">{d.question}</span><span className="since-kind">{d.answer}</span>
            </a>
          ))}
        </section>
      )}
    </aside>
  );
}
