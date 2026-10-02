import React, { useEffect } from "react";
import { Command } from "cmdk";
import { useStore } from "./store";
import { setNav, getNav, SCREENS } from "./nav";
import { answerDecision, sortDecisions } from "./screens/Inbox";
import { setStress } from "./actions";

export function Palette() {
  const projects = useStore((s) => s.projects);
  const prs = useStore((s) => s.prs);
  const decisions = useStore((s) => s.decisions);
  const close = () => setNav({ palette: false });
  const go = (p: Partial<ReturnType<typeof getNav>>) => setNav({ ...p, palette: false });

  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    return () => { prev?.focus?.(); };
  }, []);

  const open = sortDecisions(Object.values(decisions)).filter((d) => d.state === "open");
  const pname = (id: string) => projects.find((p) => p.id === id)?.name ?? id;

  return (
    <div className="palette-overlay" onMouseDown={close}>
      <div onMouseDown={(e) => e.stopPropagation()}>
        <Command label="Command palette" loop onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); close(); } }}>
          <Command.Input autoFocus placeholder="Jump to a screen, project, PR, or answer a decision…" />
          <Command.List>
            <Command.Empty>No results.</Command.Empty>
            <Command.Group heading="Screens">
              {SCREENS.map((s, i) => (
                <Command.Item key={s.id} value={"screen " + s.label} onSelect={() => go({ screen: s.id })}>
                  <span className="mono faint">{s.glyph}</span>{s.label}<span className="sub">Ctrl+{i + 1}</span>
                </Command.Item>
              ))}
            </Command.Group>
            <Command.Group heading="Projects">
              <Command.Item value="project all projects" onSelect={() => go({ project: null })}>All projects</Command.Item>
              {projects.map((p) => (
                <Command.Item key={p.id} value={"project " + p.name + " " + p.repo} onSelect={() => go({ project: p.id })}>
                  <span className="mono faint">#</span>{p.name}<span className="sub">{p.repo}</span>
                </Command.Item>
              ))}
            </Command.Group>
            <Command.Group heading="Pull requests">
              {Object.values(prs).sort((a, b) => b.number - a.number).map((p) => (
                <Command.Item key={p.id} value={`pr #${p.number} ${p.title} ${pname(p.project_id)}`} onSelect={() => go({ screen: "diff", pr: p.id })}>
                  <span className="mono faint">#{p.number}</span>{p.title}<span className="sub">{pname(p.project_id)} · {p.checks}</span>
                </Command.Item>
              ))}
            </Command.Group>
            <Command.Group heading="Answer decision">
              {open.flatMap((d) => d.options.map((o, k) => (
                <Command.Item key={d.id + ":" + k} value={`answer ${d.question} ${o.label} ${d.id}:${k}`}
                  onSelect={() => { answerDecision(d.id, k).catch((e) => alert(String(e))); close(); }}>
                  <span className="mono faint">{k + 1}</span>
                  <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{d.question} → <b>{o.label}</b></span>
                  <span className="sub">{pname(d.project_id)}{k === d.recommended ? " · rec" : ""}</span>
                </Command.Item>
              )))}
            </Command.Group>
            <Command.Group heading="Tools">
              <Command.Item value="toggle perf overlay" onSelect={() => go({ perf: !getNav().perf })}>Toggle perf overlay<span className="sub">Ctrl+Shift+P</span></Command.Item>
              <Command.Item value="toggle stress mode" onSelect={() => { setStress(!getNav().stress); close(); }}>Toggle stress mode (4 panes + chat)<span className="sub">Ctrl+Shift+S</span></Command.Item>
            </Command.Group>
          </Command.List>
        </Command>
      </div>
    </div>
  );
}
