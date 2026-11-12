// Updates: what quarkd's updater found (GET /v1/update), Install and restart, Roll back,
// and the update settings. Check for Updates… in the app menu and QuarkBar open it with
// `#/updates/check`, which checks as it opens.
import React, { useEffect, useState } from "react";
import { api, type UpdateSettings, type UpdateStatus } from "../api";
import { Button, StatusDot, type Tone } from "../ui";
import { errText } from "../util";
import { markInstallSeen, setUpdateStatus, updateSummary, useUpdateStatus } from "../appUpdate";

const TONE: Record<UpdateStatus["state"], Tone> = {
  disabled: "idle", idle: "idle", checking: "busy", up_to_date: "ready", available: "ready",
  downloading: "busy", ready: "needs-you", failed: "failed", installing: "busy",
};

export function Updates({ check }: { check?: boolean }) {
  const s = useUpdateStatus();
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const run = async (what: string, call: () => Promise<UpdateStatus>) => {
    setBusy(what); setErr(null);
    try { setUpdateStatus(await call()); }
    catch (e) { setErr(errText(e)); }
    finally { setBusy(null); }
  };
  useEffect(() => { if (check) void run("check", api.checkUpdate); }, [check]);
  useEffect(() => { if (s?.last_install) markInstallSeen(s.last_install.started_at); }, [s?.last_install?.started_at]);

  const save = (patch: Partial<UpdateSettings>) => s && run("settings", () => api.putUpdateSettings({ ...s.settings, ...patch }));
  const disabled = s?.state === "disabled";
  const installing = s?.state === "installing";
  const last = s?.last_install;

  return (
    <>
      <div className="header">
        <h1>Updates</h1>
        {s && <span className="faint">Quark {s.current_version}</span>}
        <span className="spacer" />
        <Button onClick={() => run("check", api.checkUpdate)} disabled={!s || disabled || installing || !!busy} data-testid="update-check">
          {busy === "check" || s?.state === "checking" ? "Checking…" : "Check for updates"}
        </Button>
      </div>
      <div className="screen scroll">
        <div className="updates" data-testid="updates">
          {err && <div className="form-error">{err}</div>}
          {!s ? <p className="faint">This quarkd doesn't report updates.</p> : (
            <>
              <p data-testid="update-summary"><StatusDot tone={TONE[s.state]} /> {updateSummary(s)}</p>
              {s.checked_at && <p className="faint">Last checked {new Date(s.checked_at).toLocaleString()}</p>}
              {s.offer && (
                <section className="update-offer">
                  <h2>Quark {s.offer.version}</h2>
                  {s.offer.pub_date && <p className="faint">Released {new Date(s.offer.pub_date).toLocaleDateString()}</p>}
                  {s.offer.notes && <p>{s.offer.notes}</p>}
                  <p className="faint">
                    Quark, QuarkBar and quarkd restart; running workers carry on. If the new version doesn't start, the old one comes back by itself.
                    {s.offer.rollback_needs_restore && " Rolling back later restores your data from the backup taken just before installing."}
                  </p>
                  {s.state === "available" && (
                    <Button onClick={() => run("download", api.downloadUpdate)} disabled={!!busy}>{busy === "download" ? "Downloading…" : "Download"}</Button>
                  )}
                  {s.state === "ready" && (
                    <Button kind="primary" onClick={() => run("install", api.installUpdate)} disabled={!!busy} data-testid="update-install">
                      {busy === "install" ? "Starting…" : "Install and restart"}
                    </Button>
                  )}
                </section>
              )}
              {last && (
                <section className="update-last" data-testid="update-last">
                  <h2>Last {last.rollback ? "rollback" : "update"}</h2>
                  <p>
                    {last.rollback ? `Back to Quark ${last.to} from ${last.from}` : `Quark ${last.from} to ${last.to}`}:{" "}
                    {{ pending: "starting", staged: "unpacked", swapped: "restarting", done: "done", rolled_back: "rolled back", failed: "failed" }[last.phase]}
                    {last.finished_at && `, ${new Date(last.finished_at).toLocaleString()}`}
                  </p>
                  {last.error && <p className="bad">{last.error}</p>}
                  {s.can_roll_back && (
                    <Button onClick={() => run("rollback", api.rollBackUpdate)} disabled={!!busy || installing} data-testid="update-rollback">
                      {busy === "rollback" ? "Starting…" : `Roll back to Quark ${last.from}`}
                    </Button>
                  )}
                </section>
              )}
              {!disabled && (
                <section className="update-settings">
                  <h2>Settings</h2>
                  <label><input type="checkbox" checked={s.settings.check} onChange={(e) => save({ check: e.target.checked })} /> Check for updates every six hours</label>
                  <label><input type="checkbox" checked={s.settings.auto_download} onChange={(e) => save({ auto_download: e.target.checked })} /> Download updates when they're found</label>
                  <label><input type="checkbox" checked={s.settings.channel === "beta"} onChange={(e) => save({ channel: e.target.checked ? "beta" : "stable" })} /> Get pre-releases (beta)</label>
                </section>
              )}
            </>
          )}
        </div>
      </div>
    </>
  );
}

/** The banner above every screen while an update is ready, installing, or just failed. */
export function UpdateBannerView({ banner, onInstall, onDismiss }: {
  banner: { text: string; tone: Tone; action?: "install" | "open" };
  onInstall: () => void;
  onDismiss?: () => void;
}) {
  return (
    <div className={`update-banner tone-${banner.tone}`} role="status" data-testid="update-banner">
      <StatusDot tone={banner.tone} />
      <span className="ellipsis">{banner.text}</span>
      <span className="spacer" />
      {banner.action === "install" && <Button kind="primary" onClick={onInstall}>Install and restart</Button>}
      {banner.action === "open" && <a className="btn small" href="#/updates">Details</a>}
      {onDismiss && <button className="link" onClick={onDismiss} aria-label="Dismiss">✕</button>}
    </div>
  );
}
