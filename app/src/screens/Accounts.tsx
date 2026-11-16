// J9 accounts (quark#23): every harness account with its login health and quota, grouped
// by harness. Add another config directory as an account (log in once with the harness
// pointed at it), or for IBM Bob an API key kept in the Keychain; put accounts in pools
// that dispatch profiles name, or remove one. Each account can be turned off for Quark: disabling
// or removing one first shows what it touches (running work, every project's dispatch profiles)
// and Quark's proposal for each profile left without an account, applied once confirmed.
// Quota readings arrive live as `account.quota_changed`.
import React, { useEffect, useMemo, useState } from "react";
import { Account, AccountChangeAction, AccountChangePlan, AccountDisposition, api, HarnessInfo, NotAvailable } from "../api";
import {
  addable, availabilityMeta, dispositionKey, dispositionText, groupByHarness, healthMeta, loginHint, parsePools, placeText, POOL_RE,
  proposedDispositions, quotaCls, quotaText, usageText, whereText,
} from "../accounts";
import { Button, FormError, Select, Toggle } from "../ui";
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
  // A draining account turns disabled, and a removal finishes, when its last worker does.
  const waiting = accounts.some((a) => a.availability === "draining" || a.removing);
  useEffect(() => {
    if (!waiting) return;
    const t = setInterval(() => { loadAccounts().catch(() => {}); }, 5000);
    return () => clearInterval(t);
  }, [waiting]);

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
                {g.accounts.map((a) => (
                  <AccountRow key={a.id} account={a} onError={setErr}
                    keyEnv={harnesses.find((h) => h.id === a.harness)?.key_env ?? null} />
                ))}
              </section>
            ))}
            {!groups.length && (
              <div className="empty">
                {available === null ? (connected ? "Loading…" : "Waiting for the daemon…") : "No harness is installed."}
              </div>
            )}
            <AddAccount harnesses={harnesses} accounts={accounts} />
          </div>
        )}
      </div>
    </>
  );
}

function AccountRow({ account: a, onError, keyEnv }: {
  account: Account; onError: (e: string | null) => void;
  /** The harness's API key variable, when it signs in with keys. */
  keyEnv: string | null;
}) {
  const h = healthMeta[a.health.state];
  const q = a.quota;
  const [editing, setEditing] = useState(false);
  const [pools, setPools] = useState(a.pools.join(", "));
  const [busy, setBusy] = useState(false);
  const [change, setChange] = useState<AccountChangeAction | null>(null);
  const enabled = a.enabled !== false;
  const standing = availabilityMeta(a);
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
  const grant = async () => {
    setBusy(true); onError(null);
    try {
      await api.keychainGrant(a.id);
      setAccounts(await api.accounts());
    } catch (e) { onError(errText(e)); } finally { setBusy(false); }
  };
  const [keying, setKeying] = useState(false);
  const [key, setKey] = useState("");
  const badKey = /[\s"\\]/.test(key.trim());
  // A default login can keep a key, so quarkd signs in without the person's shell (launchd).
  const keyable = a.default && !!keyEnv;
  const saveKey = async () => {
    if (!key.trim() || badKey) return;
    setBusy(true); onError(null);
    try {
      await api.setAccountKey(a.id, key.trim());
      setAccounts(await api.accounts());
      setKeying(false); setKey("");
    } catch (e) { onError(errText(e)); } finally { setBusy(false); }
  };
  const forgetKey = async () => {
    setBusy(true); onError(null);
    try {
      await api.clearAccountKey(a.id);
      setAccounts(await api.accounts());
    } catch (e) { onError(errText(e)); } finally { setBusy(false); }
  };
  const toggle = async (on: boolean) => {
    if (!on) { setChange("disable"); return; }
    setBusy(true); onError(null);
    try {
      await api.enableAccount(a.id);
      setAccounts(await api.accounts());
    } catch (e) { onError(errText(e)); } finally { setBusy(false); }
  };

  return (
    <div className={"account-row" + (enabled ? "" : " off")} data-testid="account-row" data-account={a.id}>
      <div className="account-main">
        <div className="account-name">
          <Toggle checked={enabled && change !== "disable"} onCheckedChange={toggle} disabled={busy || change !== null}
            label={enabled ? `Quark may use ${a.label}; turn off to disable it` : `Enable ${a.label} for Quark`} testid="account-enabled" />
          <b>{a.label}</b>
          {a.default && <span className="pill">Default</span>}
          {standing && <span className={"pill " + standing.cls} title={standing.title} data-testid="account-availability">{standing.label}</span>}
          {!a.launchable && <span className="pill yellow" title="The engine cannot start this harness under another account yet">Quota only</span>}
        </div>
        <div className="mono faint ellipsis" title={a.config_dir ?? undefined}>{whereText(a)}</div>
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
              <span key={w.id}>{w.label} {w.percent_remaining != null ? `${Math.round(w.percent_remaining)}%` : "?"}{usageText(w) && ` · ${usageText(w)}`}</span>
            ))}
          </div>
        )}
        {q.state !== "known" && q.detail && <div className="faint ellipsis">{q.detail}</div>}
        {!!q.providers?.length && (
          <div className="quota-providers">
            {q.providers.map((p) => (
              <div key={p.provider} className="quota-provider" data-testid="quota-provider" title={p.detail ?? undefined}>
                <div className="quota-line">
                  <b>{p.label}</b>
                  <span className={"pill " + quotaCls(p)}>{quotaText(p)}</span>
                  {p.plan && <span className="faint">{p.plan}</span>}
                </div>
                {p.windows.length > 0 && (
                  <div className="faint quota-windows">
                    {p.windows.map((w) => (
                      <span key={w.id}>{w.label} {w.percent_remaining != null ? `${Math.round(w.percent_remaining)}%` : "?"}{usageText(w) && ` · ${usageText(w)}`}</span>
                    ))}
                  </div>
                )}
                {p.state !== "known" && p.detail && <div className="faint ellipsis">{p.detail}</div>}
              </div>
            ))}
          </div>
        )}
        {q.keychain_grant && (
          <button className="btn small" onClick={grant} disabled={busy} data-testid="keychain-grant"
            title="macOS asks once for this login's Keychain item; choose Always Allow">
            {busy ? "Waiting for macOS…" : "Allow Keychain access"}
          </button>
        )}
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
        {keyable && !keying && (a.api_key ? (
          <button className="btn small" onClick={forgetKey} disabled={busy} data-testid="forget-key"
            title={`Sign in from quarkd's own environment (${keyEnv} or the harness's login) again`}>
            Forget key
          </button>
        ) : (
          <button className="btn small" onClick={() => setKeying(true)} disabled={busy} data-testid="set-key"
            title={`Keep an API key in the Keychain, so quarkd signs in without ${keyEnv} in its environment`}>
            Use API key
          </button>
        ))}
        {keying && (
          <form className="pool-form" onSubmit={(e) => { e.preventDefault(); void saveKey(); }}>
            <input autoFocus type="password" value={key} onChange={(e) => setKey(e.target.value)}
              placeholder={keyEnv ?? "API key"} aria-label={`API key for ${a.label}`} autoComplete="off"
              className={badKey ? "invalid" : ""} />
            <button className="btn small" type="submit" disabled={busy || !key.trim() || badKey}>Save</button>
            <button className="btn small" type="button" onClick={() => { setKeying(false); setKey(""); }}>Cancel</button>
          </form>
        )}
        {!a.default && !a.removing && (
          <button className="btn small" onClick={() => setChange("remove")} disabled={busy || change !== null}
            aria-label={`Remove ${a.label}`}>
            Remove
          </button>
        )}
      </div>
      {change && (
        <AccountChange account={a} action={change} onError={onError}
          onDone={async () => { setChange(null); setAccounts(await api.accounts()); }}
          onCancel={() => setChange(null)} />
      )}
    </div>
  );
}

/** Before disabling or removing an account: what runs under it, every profile that reaches it, and a
 *  choice (Quark's proposal first) for each profile it leaves without an enabled account. */
function AccountChange({ account: a, action, onDone, onCancel, onError }: {
  account: Account; action: AccountChangeAction; onDone: () => void; onCancel: () => void;
  onError: (e: string | null) => void;
}) {
  const [plan, setPlan] = useState<AccountChangePlan | null>(null);
  const [choices, setChoices] = useState<Record<string, AccountDisposition>>({});
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    api.planAccountChange(a.id, action).then((p) => {
      if (!live) return;
      setPlan(p); setChoices(proposedDispositions(p.references));
    }).catch((e) => { if (live) setErr(errText(e)); });
    return () => { live = false; };
  }, [a.id, action]);

  const stranded = plan?.references.filter((r) => r.stranded) ?? [];
  const kept = plan?.references.filter((r) => !r.stranded) ?? [];
  const projects = [...new Set(stranded.map((r) => r.project_name))];
  const verb = action === "disable" ? "Disable" : "Remove";

  const confirm = async () => {
    setBusy(true); setErr(null); onError(null);
    try {
      if (action === "disable") await api.disableAccount(a.id, choices);
      else await api.removeAccountWhenIdle(a.id, choices);
      onDone();
    } catch (e) { setErr(errText(e)); } finally { setBusy(false); }
  };

  return (
    <section className="account-change mt-2 flex flex-col gap-3 rounded-m border border-line-2 bg-surface-1 p-4" data-testid="account-change"
      aria-label={`${verb} ${a.label}`}>
      <div className="flex flex-col gap-1">
        <b className="text-m">{verb} {a.label}?</b>
        <span className="text-s text-dim">
          {action === "disable"
            ? "Nothing new starts under it, in any project. You can enable it again at any time."
            : "Nothing new starts under it, in any project, and Quark forgets it. Its folder is left as it is."}
        </span>
      </div>
      {!plan && !err && <span className="text-s text-dim">Checking which projects use it…</span>}
      {plan?.refused && <FormError>{plan.refused}</FormError>}
      {plan && !plan.refused && (
        <>
          {plan.running.length > 0 && (
            <div className="flex flex-col gap-1" data-testid="account-change-running">
              <span className="text-s font-medium">Still running under it ({plan.running.length})</span>
              <span className="text-s text-dim">
                {action === "disable"
                  ? "They finish first; the account shows as draining until then."
                  : "They finish first; the account is removed when the last one does."}
              </span>
              <ul className="m-0 flex flex-col gap-0.5 pl-4 text-s">
                {plan.running.map((w) => <li key={`${w.project_id}/${w.task_id ?? "coordinator"}`}>{w.project_name}: {w.title}</li>)}
              </ul>
            </div>
          )}
          {stranded.length > 0 && (
            <div className="flex flex-col gap-2" data-testid="account-change-stranded">
              <span className="text-s font-medium">
                {stranded.length === 1 ? "1 profile is" : `${stranded.length} profiles are`} left without an enabled account
                {projects.length ? ` in ${projects.join(", ")}` : ""}
              </span>
              <span className="text-s text-dim">Quark proposes what to do with each; change any before you confirm. Dispatch rule changes are committed to each project's rules.</span>
              {stranded.map((r) => (
                <div key={r.id} className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)] items-center gap-3" data-testid="account-change-ref">
                  <span className="min-w-0 text-s">
                    <b>{r.project_name}</b> · {placeText(r)}
                    <span className="text-dim"> · {[r.harness, r.model, r.effort].filter(Boolean).join(" ")}{r.pool ? `, pool ${r.pool}` : ", default account"}</span>
                  </span>
                  <Select value={choices[r.id] ? dispositionKey(choices[r.id]) : ""} aria-label={`What to do with ${placeText(r)} in ${r.project_name}`}
                    onValueChange={(k) => {
                      const o = r.options.find((x) => dispositionKey(x) === k);
                      if (o) setChoices((c) => ({ ...c, [r.id]: o }));
                    }}>
                    {r.options.map((o, i) => (
                      <option key={dispositionKey(o)} value={dispositionKey(o)}>{dispositionText(o, r)}{i === 0 ? " (proposed)" : ""}</option>
                    ))}
                  </Select>
                </div>
              ))}
            </div>
          )}
          {kept.length > 0 && (
            <div className="flex flex-col gap-1">
              <span className="text-s font-medium">Keep running on other accounts</span>
              <ul className="m-0 flex flex-col gap-0.5 pl-4 text-s text-dim">
                {kept.map((r) => (
                  <li key={r.id}>{r.project_name} · {placeText(r)}: pool {r.pool} keeps {r.remaining.join(", ")}</li>
                ))}
              </ul>
            </div>
          )}
          {!plan.running.length && !plan.references.length && (
            <span className="text-s text-dim">No project's dispatch rules or coordinator use it, and nothing runs under it.</span>
          )}
        </>
      )}
      {err && <FormError>{err}</FormError>}
      <div className="flex items-center justify-end gap-2">
        <Button kind="quiet" onClick={onCancel} disabled={busy}>Cancel</Button>
        <Button kind={action === "remove" ? "danger" : "primary"} onClick={confirm} disabled={busy || !plan || !!plan.refused}
          data-testid="account-change-confirm">
          {busy ? `${verb === "Disable" ? "Disabling" : "Removing"}…` : stranded.length ? `Apply and ${verb.toLowerCase()}` : `${verb} account`}
        </Button>
      </div>
    </section>
  );
}

function AddAccount({ harnesses, accounts }: { harnesses: HarnessInfo[]; accounts: Account[] }) {
  // Only harnesses that take more accounts; before /v1/harnesses answers, the ones listed.
  const options = useMemo(() => {
    const multi = addable(harnesses);
    if (multi.length) return multi.map((h) => ({ id: h.id, name: h.name, h }));
    return [...new Set(accounts.map((a) => a.harness))].map((id) => ({ id, name: id, h: undefined }));
  }, [harnesses, accounts]);
  const [harness, setHarness] = useState("");
  const [label, setLabel] = useState("");
  const [dir, setDir] = useState("");
  const [key, setKey] = useState("");
  const [team, setTeam] = useState("");
  const [pools, setPools] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [added, setAdded] = useState<string | null>(null);

  useEffect(() => { if (!harness && options[0]) setHarness(options[0].id); }, [options, harness]);
  const current = options.find((o) => o.id === harness);
  const parsed = parsePools(pools);
  const badPool = parsed.find((p) => !POOL_RE.test(p));
  // A harness that takes keys and no directories (IBM Bob) adds an API key.
  const keyed = !!current?.h?.key_env && !current.h.account_env;
  const badDir = !keyed && dir.trim() !== "" && !dir.trim().startsWith("/");
  const badKey = keyed && /[\s"\\]/.test(key.trim());
  const filled = keyed ? key.trim() !== "" : dir.trim() !== "";
  const canSubmit = !!harness && filled && !badDir && !badKey && !badPool && !busy;
  const hint = keyed ? null : loginHint(current?.h, dir.trim());

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!canSubmit) return;
    setBusy(true); setError(null); setAdded(null);
    try {
      const a = await api.addAccount(keyed
        ? { harness, label: label.trim() || null, api_key: key.trim(), team_id: team.trim() || null, pools: parsed }
        : { harness, label: label.trim() || null, config_dir: dir.trim(), pools: parsed });
      setAccounts(await api.accounts());
      setAdded(a.label);
      setLabel(""); setDir(""); setKey(""); setTeam(""); setPools("");
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
      {keyed ? (
        <>
          <input type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value)}
            placeholder={`${current?.h?.key_env ?? "API key"}`} aria-label="API key" className={badKey ? "invalid" : ""} />
          {badKey && <div className="field-error">An API key has no spaces, quotes or backslashes.</div>}
          <input value={team} onChange={(e) => setTeam(e.target.value)} placeholder="Team ID (only for a general key)" aria-label="Team ID" />
          <div className="hint-line">The key is kept in your Keychain, never in Quark's database or logs.</div>
        </>
      ) : (
        <>
          <input value={dir} onChange={(e) => setDir(e.target.value)} placeholder="/Users/you/.claude-work"
            aria-label="Config directory" className={badDir ? "invalid" : ""} />
          {badDir && <div className="field-error">The config directory must be an absolute path.</div>}
        </>
      )}
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
