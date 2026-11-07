// Setup: what this machine still needs before Quark can run a project (GET /v1/setup), with
// the fixes quarkd can run itself (POST /v1/setup/<id>) and the commands for the rest, such
// as sign-ins. The app opens it on its own the first time the checks fail (see firstRun.ts).
import React, { useEffect, useState } from "react";
import { api, NotAvailable, type SetupCheck, type SetupReport } from "../api";
import { go } from "../nav";
import { Button, StatusDot, type Tone } from "../ui";
import { Unavailable } from "../components/Unavailable";
import { errText } from "../util";
import { dismissSetup } from "../firstRun";

const TONE: Record<SetupCheck["status"], Tone> = { ok: "ready", warn: "needs-you", missing: "failed" };

export function Setup() {
  const [report, setReport] = useState<SetupReport | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [checking, setChecking] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [running, setRunning] = useState<string | null>(null);
  const [output, setOutput] = useState<Record<string, { ok: boolean; text: string }>>({});

  const load = async () => {
    setChecking(true); setErr(null);
    try { setReport(await api.setup()); }
    catch (e) { if (e instanceof NotAvailable) setUnavailable(true); else setErr(errText(e)); }
    finally { setChecking(false); }
  };
  useEffect(() => { void load(); }, []);

  const fix = async (id: string) => {
    setRunning(id); setErr(null);
    try {
      const r = await api.fixSetup(id);
      setReport(r.report);
      setOutput((o) => ({ ...o, [id]: { ok: r.ok, text: r.output } }));
    } catch (e) { setErr(errText(e)); }
    finally { setRunning(null); }
  };

  const done = () => { dismissSetup(); go({ name: "projects" }); };
  const required = report?.checks.filter((c) => c.required) ?? [];
  const optional = report?.checks.filter((c) => !c.required) ?? [];

  return (
    <>
      <div className="header">
        <h1>Setup</h1>
        <span className="faint">What this Mac needs before Quark can run a project</span>
        <span className="spacer" />
        <Button onClick={load} disabled={checking || !!running}>{checking ? "Checking…" : "Check again"}</Button>
        <Button kind="primary" onClick={done} data-testid="setup-done">{report?.ready ? "Continue" : "Skip for now"}</Button>
      </div>
      <div className="screen scroll">
        {unavailable ? <Unavailable what="Setup" endpoint="GET /v1/setup" /> : (
          <div className="setup" data-testid="setup">
            {err && <div className="form-error">{err}</div>}
            {report && (
              <p className={report.ready ? "setup-ready" : "setup-notready"} data-testid="setup-summary">
                {report.ready ? "Everything Quark needs is here. Create a project to get started."
                  : "Quark needs the items marked below. Fix them here, or run the command shown in a terminal and press Check again."}
              </p>
            )}
            {!report && !err && <div className="empty">Checking this Mac…</div>}
            {required.length > 0 && <h2>Needed</h2>}
            {required.map((c) => <CheckRow key={c.id} c={c} running={running} out={output[c.id]} onFix={fix} />)}
            {optional.length > 0 && <h2>Optional</h2>}
            {optional.map((c) => <CheckRow key={c.id} c={c} running={running} out={output[c.id]} onFix={fix} />)}
          </div>
        )}
      </div>
    </>
  );
}

function CheckRow({ c, running, out, onFix }: {
  c: SetupCheck; running: string | null; out?: { ok: boolean; text: string }; onFix: (id: string) => void;
}) {
  const tone: Tone = c.status === "missing" && !c.required ? "idle" : TONE[c.status];
  const fix = c.status !== "ok" ? c.fix : null;
  return (
    <section className="setup-check" data-testid={`setup-${c.id}`} data-status={c.status}>
      <StatusDot tone={tone} label={c.status} />
      <div className="setup-body">
        <div className="setup-title">{c.title}</div>
        <div className="faint">{c.why}</div>
        <div className="mono setup-detail">{c.detail}</div>
        {fix && !fix.runnable && <div className="setup-cmd">Run in a terminal: <code>{fix.command}</code></div>}
        {out && <pre className={out.ok ? "setup-out" : "setup-out bad"}>{out.text || (out.ok ? "Done." : "Failed.")}</pre>}
      </div>
      {fix?.runnable && (
        <Button onClick={() => onFix(c.id)} disabled={!!running} title={fix.command}>
          {running === c.id ? "Working…" : fix.label}
        </Button>
      )}
    </section>
  );
}
