// The Conversation tab's right column: what changed since you last looked, what needs you,
// open PRs and recent decisions in this project. It only reads: the Overview tab marks things read.
import React, { useEffect, useMemo, useState } from "react";
import { api, OverviewDigest } from "../api";
import { href } from "../nav";
import { useStore } from "../store";
import { ago } from "../util";
import { SectionLabel, StatusDot } from "../ui";
import { lastSeen, summarize } from "../screens/dashboard/summary";
import { useAttention } from "./NextAttention";

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
  const recent = useMemo(() => Object.values(decisions).filter((d) => d.project_id === pid && d.state === "answered")
    .sort((a, b) => (b.answered_at ?? "").localeCompare(a.answered_at ?? "")).slice(0, 3), [decisions, pid]);

  return (
    <aside className="since" aria-label="Since you looked" data-testid="since-you-looked">
      {digest && (
        <section>
          <SectionLabel>{seen === null ? "Since the log began" : `Since you looked · ${ago(digest.from)}`}</SectionLabel>
          <p className="since-summary" data-testid="since-summary">{summarize(digest)} <a href={href({ name: "overview", project: pid })}>Overview</a></p>
        </section>
      )}
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
      {recent.length > 0 && (
        <section>
          <SectionLabel right={<a href={href({ name: "decisions", project: pid })}>All</a>}>Recent decisions</SectionLabel>
          {recent.map((d) => (
            <a key={d.id} className="since-item" href={href({ name: "inbox", id: d.id })}>
              <span className="since-title">{d.question}</span><span className="since-kind">{d.answer}</span>
            </a>
          ))}
        </section>
      )}
    </aside>
  );
}
