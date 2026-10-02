import React, { useEffect, useMemo, useRef, useState } from "react";
import { useStore, getState } from "../store";
import { useNav, getNav, setNav } from "../nav";
import { api, Decision, PullRequest } from "../api";

export async function answerDecision(id: string, option: number) {
  const d = await api.answer(id, option);
  // optimistic local update; the decision.answered event will confirm
  const s = getState();
  s.decisions[id] = d ?? { ...s.decisions[id], state: "answered", answer: option };
}

export function sortDecisions(ds: Decision[]) {
  return [...ds].sort((a, b) => (a.state === b.state ? b.id.localeCompare(a.id, undefined, { numeric: true }) : a.state === "open" ? -1 : 1));
}

export function Inbox() {
  const nav = useNav();
  const decisionsMap = useStore((s) => s.decisions);
  const prsMap = useStore((s) => s.prs);
  const projects = useStore((s) => s.projects);
  const decisions = useMemo(() => sortDecisions(Object.values(decisionsMap).filter((d) => !nav.project || d.project_id === nav.project)), [decisionsMap, nav.project]);
  const prs = useMemo(() => Object.values(prsMap).filter((p) => !nav.project || p.project_id === nav.project).sort((a, b) => b.number - a.number), [prsMap, nav.project]);
  const [pane, setPane] = useState<"d" | "p">("d");
  const [di, setDi] = useState(0);
  const [pi, setPi] = useState(0);
  const [pending, setPending] = useState<Record<string, number>>({});
  const listRef = useRef<HTMLDivElement>(null);
  const pname = (id: string) => projects.find((p) => p.id === id)?.name ?? id;

  const state = useRef({ pane, di, pi, decisions, prs });
  state.current = { pane, di, pi, decisions, prs };

  const answer = (d: Decision, opt: number) => {
    if (d.state !== "open" || opt < 0 || opt >= d.options.length) return;
    setPending((p) => ({ ...p, [d.id]: opt }));
    answerDecision(d.id, opt).catch((e) => { alert(String(e)); setPending((p) => { const { [d.id]: _, ...r } = p; return r; }); });
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (getNav().palette || e.ctrlKey || e.metaKey || e.altKey) return;
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      const { pane, di, pi, decisions, prs } = state.current;
      const n = pane === "d" ? decisions.length : prs.length;
      const setI = pane === "d" ? setDi : setPi;
      const i = pane === "d" ? di : pi;
      if (e.key === "j" || e.key === "ArrowDown") setI(Math.min(n - 1, i + 1));
      else if (e.key === "k" || e.key === "ArrowUp") setI(Math.max(0, i - 1));
      else if (e.key === "g") setI(0);
      else if (e.key === "G") setI(n - 1);
      else if (e.key === "Tab" || e.key === "h" || e.key === "l") setPane(pane === "d" ? "p" : "d");
      else if (e.key === "Enter") {
        if (pane === "d" && decisions[di]) answer(decisions[di], decisions[di].recommended);
        if (pane === "p" && prs[pi]) setNav({ screen: "diff", pr: prs[pi].id });
      } else if (pane === "d" && /^[1-9]$/.test(e.key) && decisions[di]) answer(decisions[di], Number(e.key) - 1);
      else return;
      e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    listRef.current?.querySelector(".row.sel")?.scrollIntoView({ block: "nearest" });
  }, [di, pi, pane]);

  return (
    <div className="inbox" ref={listRef}>
      <section>
        <h2 style={{ color: pane === "d" ? "var(--fg)" : undefined }}>Decisions <span className="pill accent">{decisions.filter((d) => d.state === "open").length} open</span></h2>
        <div className="list">
          {decisions.map((d, i) => {
            const chosen = d.answer ?? pending[d.id];
            return (
              <div key={d.id} className={"row" + (pane === "d" && i === di ? " sel" : "") + (d.state === "answered" ? " answered" : "")}
                onClick={() => { setPane("d"); setDi(i); }}>
                <div className="q">{d.question}</div>
                <div className="ctx"><span className="pill">{pname(d.project_id)}</span> {d.context}</div>
                {d.options.map((o, k) => (
                  <div key={k} className={"opt" + (k === d.recommended ? " rec" : "") + (chosen === k ? " chosen" : "")}
                    onClick={(e) => { e.stopPropagation(); answer(d, k); }} style={{ cursor: d.state === "open" ? "pointer" : "default" }}>
                    <span className="num">{k + 1}</span><span className="label">{o.label}</span><span className="cons">— {o.consequence}</span>
                  </div>
                ))}
              </div>
            );
          })}
          {!decisions.length && <div className="empty">No decisions.</div>}
        </div>
      </section>
      <section>
        <h2 style={{ color: pane === "p" ? "var(--fg)" : undefined }}>Pull requests <span className="pill">{prs.length}</span></h2>
        <div className="list">
          {prs.map((p, i) => <PrRow key={p.id} p={p} sel={pane === "p" && i === pi} project={pname(p.project_id)}
            onClick={() => { setPane("p"); setPi(i); }} onOpen={() => setNav({ screen: "diff", pr: p.id })} />)}
          {!prs.length && <div className="empty">No pull requests.</div>}
        </div>
      </section>
      <div className="hint" style={{ gridColumn: "1 / 3" }}>
        <span><span className="kbd">j</span>/<span className="kbd">k</span> move</span>
        <span><span className="kbd">Tab</span> switch list</span>
        <span><span className="kbd">1</span>–<span className="kbd">9</span> pick option</span>
        <span><span className="kbd">Enter</span> accept recommended / open PR diff</span>
      </div>
    </div>
  );
}

function PrRow({ p, sel, project, onClick, onOpen }: { p: PullRequest; sel: boolean; project: string; onClick: () => void; onOpen: () => void }) {
  return (
    <div className={"row pr-row" + (sel ? " sel" : "")} onClick={onClick} onDoubleClick={onOpen}>
      <span className="pr-num">#{p.number}</span>
      <div style={{ minWidth: 0 }}>
        <div className="q" style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{p.title}</div>
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
          <span className="pill">{project}</span>
          <span className={"pill " + (p.checks === "passing" ? "green" : p.checks === "failing" ? "red" : "yellow")}>{p.checks}</span>
          <span className={"pill " + (p.state === "merged" ? "accent" : p.state === "draft" ? "" : "blue")}>{p.state}</span>
          <span className="pill">risk {p.risk}</span>
        </div>
      </div>
      <span className="mono" style={{ fontSize: 11 }}><span style={{ color: "var(--green)" }}>+{p.additions}</span> <span style={{ color: "var(--red)" }}>−{p.deletions}</span></span>
    </div>
  );
}
