// Verification evidence for a PR (ADR-15): a per-gate summary for the side panel, and the full
// view with each case, its message and its artifacts (Playwright traces, screenshots, videos, logs).
import React, { useEffect, useState } from "react";
import { api, Evidence, EvidenceArtifact, EvidenceCase, EvidenceGate } from "../api";
import { ago } from "../util";
import { caseCounts, evidenceOutcome, formatBytes, formatMs, GATE_LABEL, sortCases, traceViewerUrl } from "../prs";

const GATES = ["checks", "journeys", "holdout"];
const ordered = (gates: EvidenceGate[]) =>
  [...gates].sort((a, b) => (GATES.indexOf(a.kind) + 9) % 9 - (GATES.indexOf(b.kind) + 9) % 9);

function StaleNote({ ev }: { ev: Evidence }) {
  if (!ev.stale) return null;
  return (
    <div className="state-note bad" data-testid="evidence-stale">
      This evidence is for an older commit ({ev.head_sha.slice(0, 7)}), not the PR's current head.
    </div>
  );
}

/** One line per gate, for the PR view's side panel. */
export function EvidenceSummary({ ev, onOpen }: { ev: Evidence | null | undefined; onOpen: () => void }) {
  if (!ev) return <div className="side-note faint" data-testid="pr-evidence">No verification evidence yet.</div>;
  const run = evidenceOutcome(ev.state);
  return (
    <div className="evidence-summary" data-testid="pr-evidence">
      <StaleNote ev={ev} />
      {ordered(ev.gates).map((g) => {
        const o = evidenceOutcome(g.state);
        const n = caseCounts(g.cases);
        return (
          <button key={g.kind} className="check-row gate-row" onClick={onOpen} title="Show the evidence">
            <span className={"pr-glyph " + o.cls}>{o.glyph}</span>
            <span className="ellipsis">{GATE_LABEL[g.kind] ?? g.kind}</span>
            <span className="faint small-text">
              {g.cases.length ? `${n.passed}/${g.cases.length}` : ""}{n.failed ? <span className="red"> · {n.failed} failed</span> : null}
            </span>
          </button>
        );
      })}
      {!ev.gates.length && <div className="side-note faint">No gates reported.</div>}
      <div className="side-note faint small-text">
        <span className={run.cls}>{run.label}</span>
        {ev.completed_at ? ` · ${ago(ev.completed_at)}` : ev.started_at ? ` · started ${ago(ev.started_at)}` : ""}
        {" · "}<button className="link" onClick={onOpen}>details</button>
      </div>
    </div>
  );
}

/** The full evidence: every gate and case, with artifacts inline where the browser can show them. */
export function EvidencePanel({ prId, ev }: { prId: string; ev: Evidence | null | undefined }) {
  const [preview, setPreview] = useState<{ src: string; name: string } | null>(null);
  if (!ev) {
    return <div className="empty" data-testid="evidence-panel">No verification evidence for this pull request yet.</div>;
  }
  return (
    <div className="evidence" data-testid="evidence-panel">
      <StaleNote ev={ev} />
      <div className="faint small-text ev-head">
        Commit <span className="mono">{ev.head_sha.slice(0, 7)}</span>
        {ev.completed_at ? <> · finished {ago(ev.completed_at)}</> : ev.started_at ? <> · started {ago(ev.started_at)}</> : null}
      </div>
      {ordered(ev.gates).map((g) => <Gate key={g.kind} prId={prId} g={g} onPreview={setPreview} />)}
      {preview && <Lightbox src={preview.src} name={preview.name} onClose={() => setPreview(null)} />}
    </div>
  );
}

function Gate({ prId, g, onPreview }: { prId: string; g: EvidenceGate; onPreview: (p: { src: string; name: string }) => void }) {
  const o = evidenceOutcome(g.state);
  const n = caseCounts(g.cases);
  return (
    <section className="gate" data-testid={`gate-${g.kind}`}>
      <div className="gate-head">
        <span className={"pr-glyph " + o.cls}>{o.glyph}</span>
        <b>{GATE_LABEL[g.kind] ?? g.kind}</b>
        <span className={"pill " + o.cls}>{o.label}</span>
        {g.cases.length > 0 && <span className="faint small-text">{n.passed} of {g.cases.length} passed</span>}
        <span className="spacer" />
        {g.started_at && g.completed_at && (
          <span className="faint small-text">{formatMs(Math.max(0, Date.parse(g.completed_at) - Date.parse(g.started_at)))}</span>
        )}
      </div>
      {g.summary && <div className="gate-summary">{g.summary}</div>}
      {sortCases(g.cases).map((c, i) => <Case key={c.name + i} prId={prId} c={c} onPreview={onPreview} />)}
      {!g.cases.length && <div className="side-note faint">No cases reported.</div>}
    </section>
  );
}

function Case({ prId, c, onPreview }: { prId: string; c: EvidenceCase; onPreview: (p: { src: string; name: string }) => void }) {
  const o = evidenceOutcome(c.state);
  const shots = c.artifacts.filter((a) => a.kind === "screenshot");
  const videos = c.artifacts.filter((a) => a.kind === "video");
  const files = c.artifacts.filter((a) => a.kind !== "screenshot" && a.kind !== "video");
  // Failures open with their evidence showing; passes stay one line until asked.
  const [open, setOpen] = useState(o.cls === "red");
  const hasMore = !!c.message || c.artifacts.length > 0;
  return (
    <div className={"case" + (open ? " open" : "")} data-testid="evidence-case">
      <button className="case-head" onClick={() => hasMore && setOpen(!open)} aria-expanded={hasMore ? open : undefined}>
        <span className={"pr-glyph " + o.cls}>{o.glyph}</span>
        <span className="ellipsis">{c.name}</span>
        {c.artifacts.length > 0 && <span className="faint small-text">{c.artifacts.length} {c.artifacts.length === 1 ? "file" : "files"}</span>}
        {c.duration_ms != null && <span className="faint small-text">{formatMs(c.duration_ms)}</span>}
        <span className={"small-text " + o.cls}>{o.label}</span>
      </button>
      {open && hasMore && (
        <div className="case-body">
          {c.message && <pre className="case-msg">{c.message}</pre>}
          {shots.length > 0 && (
            <div className="shots">
              {shots.map((a) => {
                const src = api.artifactUrl(prId, a);
                return (
                  <button key={a.id} className="shot" onClick={() => onPreview({ src, name: a.path })} title={a.path}>
                    <img src={src} alt={a.path} loading="lazy" />
                  </button>
                );
              })}
            </div>
          )}
          {videos.map((a) => (
            <video key={a.id} className="ev-video" controls preload="metadata" src={api.artifactUrl(prId, a)} />
          ))}
          {files.map((a) => <ArtifactRow key={a.id} prId={prId} a={a} />)}
        </div>
      )}
    </div>
  );
}

function ArtifactRow({ prId, a }: { prId: string; a: EvidenceArtifact }) {
  const url = api.artifactUrl(prId, a);
  const name = a.path.split("/").pop() || a.path;
  return (
    <div className="artifact" data-testid="evidence-artifact">
      <span className="pill">{a.kind}</span>
      <span className="ellipsis mono" title={a.path}>{name}</span>
      <span className="faint small-text">{formatBytes(a.size_bytes)}</span>
      <span className="spacer" />
      {a.kind === "trace" && <a className="btn small" href={traceViewerUrl(url)} target="_blank" rel="noreferrer">Open trace</a>}
      {(a.kind === "log" || a.kind === "report") && <a className="btn small" href={url} target="_blank" rel="noreferrer">Open</a>}
      <a className="btn small" href={url} download={name}>Download</a>
    </div>
  );
}

function Lightbox({ src, name, onClose }: { src: string; name: string; onClose: () => void }) {
  useEffect(() => {
    const k = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [onClose]);
  return (
    <div className="palette-overlay lightbox" onMouseDown={onClose} data-testid="lightbox">
      <figure onMouseDown={(e) => e.stopPropagation()}>
        <img src={src} alt={name} />
        <figcaption className="faint mono small-text">{name}</figcaption>
      </figure>
    </div>
  );
}
