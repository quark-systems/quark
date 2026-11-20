// The Agents tab's project panel views: Needs you, PRs, and Activity (what changed since you last
// looked, updates and recent decisions). They only read: the Overview tab marks things read.
import { useEffect, useMemo, useState } from "react";
import { api, Notice, OverviewDigest } from "../api";
import { href } from "../nav";
import { useStore } from "../store";
import { ago, workerNamed } from "../util";
import { SectionLabel, StatusDot } from "../ui";
import { lastSeen, summarize } from "../screens/dashboard/summary";
import { useAttention } from "./NextAttention";
import { describeUpdate } from "./updates";
import { CommandButton } from "../components/CoordinatorCommand";
import { BeadMarkdown, BeadText } from "../ui/BeadText";

type Posted = Notice & { seq: number; ts: string };

/** One update: which worker, what happened and the detail, linking to the worker; or a digest that opens to list what waited. */
function Update({ notice: n }: { notice: Posted }) {
  const tasks = useStore((s) => s.tasks);
  const [open, setOpen] = useState(false);
  const task = n.task_id ? workerNamed(tasks, n.task_id) : undefined;
  const text = <BeadMarkdown inline link={false} className="since-title md" text={n.text} project={n.project_id} />;
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
              const line = <BeadMarkdown inline link={false} className="md" text={i.summary} project={n.project_id} />;
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
        {v.detail && <BeadMarkdown inline link={false} className="since-detail md" text={v.detail} project={n.project_id} />}
      </span>
      {when}
    </>
  );
  return task
    ? <a className="since-item since-update" href={href({ name: "task", id: task.id })} title={n.text} data-testid="since-update">{body}</a>
    : <div className="since-item since-update plain" title={n.text} data-testid="since-update">{body}</div>;
}

const KIND = { decision: "decision", pr: "check failed", worker: "worker stuck" } as const;

/** What needs you in this project: open decisions, failed checks, stuck workers. */
export function useNeedsYou(pid: string) {
  const queue = useAttention();
  return useMemo(() => queue.filter((i) => i.projectId === pid), [queue, pid]);
}

/** This project's open and draft PRs, newest first. */
export function useOpenPrs(pid: string) {
  const prs = useStore((s) => s.pullRequests);
  return useMemo(() => Object.values(prs).filter((p) => p.project_id === pid && (p.state === "open" || p.state === "draft"))
    .sort((a, b) => (b.updated_at ?? "").localeCompare(a.updated_at ?? "")), [prs, pid]);
}

/** The project panel's Needs you tab. */
export function NeedsYouPanel({ project: pid }: { project: string }) {
  const mine = useNeedsYou(pid);
  return (
    <div className="since" data-testid="panel-needs">
      <section>
        {mine.length ? mine.map((i) => (
          <a key={i.key} className="since-item needs" href={href(i.route)} data-testid="since-needs">
            <StatusDot tone={i.kind === "decision" ? "needs-you" : "failed"} />
            <span className="since-title"><BeadText text={i.title} project={i.projectId} /></span><span className="since-kind">{KIND[i.kind]}</span>
          </a>
        )) : <p className="since-none">Nothing needs you here.</p>}
      </section>
    </div>
  );
}

/** The project panel's PRs tab. */
export function OpenPrsPanel({ project: pid }: { project: string }) {
  const open = useOpenPrs(pid);
  return (
    <div className="since" data-testid="panel-prs">
      <section>
        <SectionLabel right={<a href={href({ name: "pulls", project: pid })}>All</a>}>Open PRs</SectionLabel>
        {open.length ? open.map((p) => (
          <a key={p.id} className="since-item" href={href({ name: "pulls", project: pid, id: p.id })}>
            <StatusDot tone={p.checks_state === "failing" ? "failed" : p.checks_state === "passing" ? "ready" : "busy"} />
            <span className="since-title">{p.title ?? `${p.repo}#${p.number}`}</span><span className="since-kind">#{p.number}</span>
          </a>
        )) : <p className="since-none">No open PRs.</p>}
      </section>
    </div>
  );
}

/** The project panel's Activity tab: what changed since you looked, updates and recent decisions. */
export function ActivityPanel({ project: pid }: { project: string }) {
  const decisions = useStore((s) => s.decisions);
  const connected = useStore((s) => s.connected);
  const [digest, setDigest] = useState<OverviewDigest | null>(null);
  const seen = lastSeen(pid);

  useEffect(() => {
    let live = true;
    api.projectOverview(pid, seen ?? 0).then((o) => { if (live) setDigest(o.digest ?? null); }).catch(() => { if (live) setDigest(null); });
    return () => { live = false; };
  }, [pid, seen, connected]);

  const allNotices = useStore((s) => s.notices);
  const notices = useMemo(() => allNotices.filter((n) => n.project_id === pid).slice(-5).reverse(), [allNotices, pid]);
  const recent = useMemo(() => Object.values(decisions).filter((d) => d.project_id === pid && d.state === "answered")
    .sort((a, b) => (b.answered_at ?? "").localeCompare(a.answered_at ?? "")).slice(0, 3), [decisions, pid]);

  return (
    <div className="since" aria-label="Since you looked" data-testid="since-you-looked">
      <section>
        <SectionLabel right={<CommandButton cid={pid} command="catch_up" />}>
          {!digest ? "Since you looked" : seen === null ? "Since the log began" : `Since you looked · ${ago(digest.from)}`}
        </SectionLabel>
        {digest && <p className="since-summary" data-testid="since-summary">{summarize(digest)} <a href={href({ name: "overview", project: pid })}>Overview</a></p>}
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
              <span className="since-title"><BeadText text={d.question} project={d.project_id} /></span><span className="since-kind"><BeadText text={d.answer} project={d.project_id} /></span>
            </a>
          ))}
        </section>
      )}
    </div>
  );
}
