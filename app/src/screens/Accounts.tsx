// J9 accounts (quark#23): every harness account with its login health and quota, grouped
// by harness. Add another config directory as an account (log in once with the harness
// pointed at it), put accounts in pools that dispatch profiles name, or remove one.
// Quota readings arrive live as `account.quota_changed`.
import React, { useEffect, useMemo, useState } from "react";
import { Account, api, HarnessInfo, NotAvailable } from "../api";
import { groupByHarness, healthMeta, loginHint, parsePools, POOL_RE, quotaCls, quotaText } from "../accounts";
import { loadAccounts, setAccounts, useStore } from "../store";
import { ago, errText } from "../util";
import { Unavailable } from "../components/Unavailable";

export function Accounts() {
  const byId = useStore((s) => s.accounts);
  const order = useStore((s) => s.accountOrder);
  const available = useStore((s) => s.accountsAvailable);
  const connected = useStore((s) => s.connected);
  const [harnesses, setHarnesses] = useState<HarnessInfo[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);

  useEffect(() => { loadAccounts().catch((e) => setErr(errText(e))); }, [connected]);
  useEffect(() => {
    api.harnesses().then(setHarnesses).catch((e) => { if (!(e instanceof NotAvailable)) console.warn("harnesses", e); });
  }, []);

  const accounts = useMemo(() => order.map((id) => byId[id]).filter(Boolean), [order, byId]);
  const groups = useMemo(() => groupByHarness(accounts, harnesses), [accounts, harnesses]);

  const refresh = async () => {
    setRefreshing(true); setErr(null);
    try { await loadAccounts(true); } catch (e) { setErr(errText(e)); } finally { setRefreshing(false); }
  };

  return (
    <>
      <div className="header">
        <h1>Accounts</h1>
        <span className="faint">Harness logins, pools and quota</span>
        <span className="spacer" />
        <button className="btn" onClick={refresh} disabled={refreshing || available === false}>
          {refreshing ? "Reading quota…" : "Refresh quota"}
        </button>
      </div>
      <div className="screen scroll">
        {available === false ? (
          <Unavailable what="Accounts" endpoint="GET /v1/accounts" />
        ) : (
          <div className="accounts">
            {err && <div className="form-error">{err}</div>}
            {groups.map((g) => (
              <section key={g.harness} className="account-group" data-testid={`accounts-${g.harness}`}>
                <h2>{g.name} <span className="count faint">{g.accounts.length}</span></h2>
                {g.accounts.map((a) => <AccountRow key={a.id} account={a} onError={setErr} />)}
              </section>
            ))}
            {!groups.length && (
              <div className="empty">
                {available === null ? (connected ? "Loading…" : "Waiting for the daemon…") : "No harness with accounts is installed."}
              </div>
            )}
            <AddAccount harnesses={harnesses} accounts={accounts} />
          </div>
        )}
      </div>
    </>
  );
}

function AccountRow({ account: a, onError }: { account: Account; onError: (e: string | null) => void }) {
  const h = healthMeta[a.health.state];
  const q = a.quota;
  const [editing, setEditing] = useState(false);
  const [pools, setPools] = useState(a.pools.join(", "));
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const parsed = parsePools(pools);
  const badPool = parsed.find((p) => !POOL_RE.test(p));

  const savePools = async () => {
    if (badPool) return;
    setBusy(true); onError(null);
    try {
      await api.updateAccount(a.id, { pools: parsed });
      setAccounts(await api.accounts());
      setEditing(false);
    } catch (e) { onError(errText(e)); } finally { setBusy(false); }
  };
  const remove = async () => {
    if (!confirm) { setConfirm(true); return; }
    setBusy(true); onError(null);
    try {
      await api.removeAccount(a.id);
      setAccounts(await api.accounts());
    } catch (e) { onError(errText(e)); setConfirm(false); } finally { setBusy(false); }
  };

  return (
    <div className="account-row" data-testid="account-row" data-account={a.id}>
      <div className="account-main">
        <div className="account-name">
          <b>{a.label}</b>
          {a.default && <span className="pill">Default</span>}
          {!a.launchable && <span className="pill yellow" title="The engine cannot start this harness under another account yet">Quota only</span>}
        </div>
        <div className="mono faint ellipsis" title={a.config_dir ?? undefined}>{a.config_dir ?? "no config directory"}</div>
      </div>
      <div className="account-health">
        <span className={"pill " + h.cls} title={a.health.detail ?? undefined} data-testid="account-health">{h.label}</span>
      </div>
      <div className="account-quota" data-testid="account-quota" title={q.detail ?? undefined}>
        <div className="quota-line">
          <span className={"pill " + quotaCls(q)}>{quotaText(q)}</span>
          {q.plan && <span className="faint">{q.plan}</span>}
        </div>
        {q.state === "known" && q.remaining_percent != null && (
          <div className="quota-bar"><div className={quotaCls(q)} style={{ width: `${Math.max(0, Math.min(100, q.remaining_percent))}%` }} /></div>
        )}
        {q.windows.length > 0 && (
          <div className="faint quota-windows">
            {q.windows.map((w) => (
              <span key={w.id}>{w.label} {w.percent_remaining != null ? `${Math.round(w.percent_remaining)}%` : "?"}</span>
            ))}
          </div>
        )}
        {q.state !== "known" && q.detail && <div className="faint ellipsis">{q.detail}</div>}
        {q.checked_at && <div className="faint">read {ago(q.checked_at)}</div>}
      </div>
      <div className="account-pools">
        {editing ? (
          <form className="pool-form" onSubmit={(e) => { e.preventDefault(); void savePools(); }}>
            <input autoFocus value={pools} onChange={(e) => setPools(e.target.value)} placeholder="max, batch"
              aria-label={`Pools for ${a.label}`} className={badPool ? "invalid" : ""} />
            <button className="btn small" type="submit" disabled={busy || !!badPool}>Save</button>
            <button className="btn small" type="button" onClick={() => { setEditing(false); setPools(a.pools.join(", ")); }}>Cancel</button>
          </form>
        ) : (
          <>
            {a.pools.map((p) => <span key={p} className="pill accent">{p}</span>)}
            {!a.pools.length && <span className="faint">no pool</span>}
            <button className="btn small" onClick={() => setEditing(true)} aria-label={`Edit pools for ${a.label}`}>Pools</button>
          </>
        )}
      </div>
      <div className="account-actions">
        <span className="faint">{a.active_tasks} running</span>
        {!a.default && (
          <button className={"btn small" + (confirm ? " danger" : "")} onClick={remove} disabled={busy}
            onBlur={() => setConfirm(false)} aria-label={`Remove ${a.label}`}>
            {confirm ? "Confirm remove" : "Remove"}
          </button>
        )}
      </div>
    </div>
  );
}

function AddAccount({ harnesses, accounts }: { harnesses: HarnessInfo[]; accounts: Account[] }) {
  // Only harnesses that take an account directory; before /v1/harnesses answers, the ones listed.
  const options = useMemo(() => {
    const multi = harnesses.filter((h) => h.account_env);
    if (multi.length) return multi.map((h) => ({ id: h.id, name: h.name, h }));
    return [...new Set(accounts.map((a) => a.harness))].map((id) => ({ id, name: id, h: undefined }));
  }, [harnesses, accounts]);
  const [harness, setHarness] = useState("");
  const [label, setLabel] = useState("");
  const [dir, setDir] = useState("");
  const [pools, setPools] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [added, setAdded] = useState<string | null>(null);

  useEffect(() => { if (!harness && options[0]) setHarness(options[0].id); }, [options, harness]);
  const current = options.find((o) => o.id === harness);
  const parsed = parsePools(pools);
  const badPool = parsed.find((p) => !POOL_RE.test(p));
  const badDir = dir.trim() !== "" && !dir.trim().startsWith("/");
  const canSubmit = !!harness && dir.trim() !== "" && !badDir && !badPool && !busy;
  const hint = loginHint(current?.h, dir.trim());

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!canSubmit) return;
    setBusy(true); setError(null); setAdded(null);
    try {
      const a = await api.addAccount({ harness, label: label.trim() || null, config_dir: dir.trim(), pools: parsed });
      setAccounts(await api.accounts());
      setAdded(a.label);
      setLabel(""); setDir(""); setPools("");
    } catch (err) {
      setError(errText(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="form account-add" onSubmit={submit} data-testid="add-account-form">
      <div className="field"><span>Add an account</span></div>
      <div className="account-add-row">
        <select value={harness} onChange={(e) => setHarness(e.target.value)} aria-label="Harness">
          {options.map((o) => <option key={o.id} value={o.id}>{o.name}</option>)}
        </select>
        <input value={label} onChange={(e) => setLabel(e.target.value)} placeholder="Label, e.g. Work" aria-label="Label" />
      </div>
      <input value={dir} onChange={(e) => setDir(e.target.value)} placeholder="/Users/you/.claude-work"
        aria-label="Config directory" className={badDir ? "invalid" : ""} />
      {badDir && <div className="field-error">The config directory must be an absolute path.</div>}
      <input value={pools} onChange={(e) => setPools(e.target.value)} placeholder="Pools, e.g. max (optional)"
        aria-label="Pools" className={badPool ? "invalid" : ""} />
      {badPool && <div className="field-error">“{badPool}” is not a pool name: use lowercase letters, digits and dashes.</div>}
      {hint && <div className="hint-line">Log in once under it: <span className="mono">{hint}</span></div>}
      {error && <div className="form-error">{error}</div>}
      {added && <div className="hint-line ok" data-testid="account-added">Added {added}. Its quota is being read.</div>}
      <button className="btn add" type="submit" disabled={!canSubmit}>{busy ? "Adding…" : "Add account"}</button>
    </form>
  );
}
