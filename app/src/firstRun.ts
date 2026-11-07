// The first run: when the daemon's setup checks fail, the app opens Setup once, unless you
// already went past it (Continue or Skip for now) on this daemon version.
import { api, NotAvailable, type SetupReport } from "./api";
import { go, parseRoute } from "./nav";

const KEY = "quark.setup.dismissed";

function dismissed(): string | null {
  try { return localStorage.getItem(KEY); } catch { return null; }
}

/** Remember that you went past Setup, so it doesn't open on its own again. */
export function dismissSetup(version = "any") {
  try { localStorage.setItem(KEY, version); } catch { /* storage unavailable */ }
}

/** Whether to open Setup on its own: the checks fail and you never went past it. */
export function shouldOpenSetup(report: SetupReport, seen: string | null, hash: string): boolean {
  if (report.ready || seen) return false;
  const r = parseRoute(hash);
  // A deep link (a worker, a decision) wins; only the starting screens give way.
  return r.name === "projects" || r.name === "new";
}

let checked = false;

/** Once per app launch, after the daemon answers: open Setup if this machine needs it. */
export async function checkFirstRun() {
  if (checked) return;
  checked = true;
  let report: SetupReport;
  try { report = await api.setup(); }
  catch (e) { if (!(e instanceof NotAvailable)) console.warn("setup", e); return; }
  if (shouldOpenSetup(report, dismissed(), location.hash)) go({ name: "setup" });
}
