// Picks a Project's repositories: GitHub repositories the daemon's gh account can reach, found
// by typing part of a name; a local folder through the system dialog; or, for anything else,
// a clone URL pasted in.
import React, { useEffect, useId, useMemo, useRef, useState } from "react";
import { api, ApiError, ForgeRepository, NotAvailable } from "../api";
import { folderPicking, pickFolder } from "../folders";
import { Button, FieldHint } from "../ui";
import { ago } from "../util";
import "./RepoPicker.css";

/** One chosen repository: the URL or path the daemon clones, and how to show it. */
export interface PickedRepo { url: string; label: string; kind: "github" | "url" | "folder"; private?: boolean }

const URL_RE = /^(https?:\/\/\S+|ssh:\/\/\S+|git@\S+:\S+)$/;
const SHORT_RE = /^[\w.-]+\/[\w.-]+$/;
const PATH_RE = /^(\/|~\/)\S*/;

/** What typed text can be added as, when it matches no listed repository. */
export function typedRepo(text: string): PickedRepo | null {
  const t = text.trim();
  if (URL_RE.test(t)) return { url: t, label: t, kind: "url" };
  if (SHORT_RE.test(t)) return { url: `git@github.com:${t}.git`, label: t, kind: "github" };
  if (PATH_RE.test(t)) return { url: t, label: t, kind: "folder" };
  return null;
}

/** Repositories matching every word typed, live ones before archived, then most recently pushed. */
export function matchRepos(repos: ForgeRepository[], query: string, limit = 8): ForgeRepository[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  return repos
    .filter((r) => words.every((w) => r.full_name.toLowerCase().includes(w) || (r.description ?? "").toLowerCase().includes(w)))
    .sort((a, b) => Number(a.archived) - Number(b.archived) || (b.pushed_at ?? "").localeCompare(a.pushed_at ?? ""))
    .slice(0, limit);
}

let cached: Promise<ForgeRepository[]> | null = null;
function loadRepos(refresh = false) {
  if (!cached || refresh) {
    cached = api.forgeRepositories(refresh);
    cached.catch(() => { cached = null; });
  }
  return cached;
}

type Option = { key: string; repo: PickedRepo; title: string; sub?: string; tag?: string };

export function RepoPicker({ value, onChange }: { value: PickedRepo[]; onChange: (v: PickedRepo[]) => void }) {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const [repos, setRepos] = useState<ForgeRepository[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const listId = useId();

  const load = (refresh = false) => {
    setLoadError(null);
    loadRepos(refresh).then(setRepos).catch((e) => {
      setRepos([]);
      setLoadError(e instanceof NotAvailable ? "this daemon does not list GitHub repositories yet"
        : e instanceof ApiError && e.code === "forge_unavailable" ? "gh is not installed where the daemon runs"
        : e instanceof Error ? e.message : String(e));
    });
  };
  useEffect(() => { load(); }, []);

  const chosen = new Set(value.map((r) => r.url));
  const options: Option[] = useMemo(() => {
    const listed: Option[] = matchRepos((repos ?? []).filter((r) => !chosen.has(r.ssh_url)), query).map((r) => ({
      key: r.full_name,
      repo: { url: r.ssh_url, label: r.full_name, kind: "github", private: r.private },
      title: r.full_name,
      sub: [r.description, r.pushed_at && `pushed ${ago(r.pushed_at)}`].filter(Boolean).join(" · "),
      tag: r.archived ? "archived" : r.private ? "private" : undefined,
    }));
    const typed = typedRepo(query);
    if (typed && !chosen.has(typed.url) && !listed.some((o) => o.repo.url === typed.url)) {
      listed.push({ key: "typed", repo: typed, title: `Use ${typed.label}`, sub: typed.kind === "folder" ? "Local folder" : typed.kind === "url" ? "Clone URL" : "GitHub repository", tag: undefined });
    }
    return listed;
  }, [repos, query, value]);
  useEffect(() => setActive(0), [query]);

  const add = (r: PickedRepo) => {
    if (!chosen.has(r.url)) onChange([...value, r]);
    setQuery("");
    setOpen(false);
    input.current?.focus();
  };
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") { e.preventDefault(); setOpen(true); setActive((a) => Math.min(a + 1, options.length - 1)); }
    else if (e.key === "ArrowUp") { e.preventDefault(); setActive((a) => Math.max(a - 1, 0)); }
    else if (e.key === "Enter") { if (open && options[active]) { e.preventDefault(); add(options[active].repo); } else if (query) e.preventDefault(); }
    else if (e.key === "Escape") { setOpen(false); }
  };
  const folders = folderPicking();
  const addFolder = async () => {
    const p = await pickFolder("Add a local repository");
    if (p) add({ url: p, label: p, kind: "folder" });
  };

  const showList = open && (options.length > 0 || repos === null || query !== "");
  return (
    <div className="rp">
      {value.length > 0 && (
        <ul className="rp-chosen" aria-label="Chosen repositories">
          {value.map((r) => (
            <li key={r.url}>
              <span className={"rp-kind " + r.kind} aria-hidden="true" />
              <span className="rp-name">{r.label}</span>
              {r.private && <span className="rp-tag">private</span>}
              {r.kind !== "github" && <span className="rp-sub">{r.kind === "folder" ? "local folder" : "clone URL"}</span>}
              <Button kind="quiet" aria-label={`Remove ${r.label}`} onClick={() => onChange(value.filter((x) => x.url !== r.url))}>Remove</Button>
            </li>
          ))}
        </ul>
      )}
      <div className="rp-search">
        <input ref={input} className="ui-input" value={query} role="combobox" aria-label="Find a repository"
          aria-expanded={showList} aria-controls={listId} aria-autocomplete="list"
          aria-activedescendant={showList && options[active] ? `${listId}-${active}` : undefined}
          placeholder={repos && repos.length ? `Search ${repos.length} GitHub repositories, or paste a clone URL` : "Paste a clone URL or owner/name"}
          onChange={(e) => { setQuery(e.target.value); setOpen(true); }}
          onFocus={() => setOpen(true)} onBlur={() => setTimeout(() => { setOpen(false); setQuery(""); }, 120)} onKeyDown={onKey} />
        {folders.ok && <Button onClick={addFolder}>Add local folder…</Button>}
        {showList && (
          <ul className="rp-list" id={listId} role="listbox" aria-label="Repositories">
            {repos === null && <li className="rp-empty">Loading your GitHub repositories…</li>}
            {repos !== null && options.length === 0 && <li className="rp-empty">No repository matches “{query}”. Paste a clone URL to use one that is not on GitHub.</li>}
            {options.map((o, i) => (
              <li key={o.key} id={`${listId}-${i}`} role="option" aria-selected={i === active} className={"rp-opt" + (i === active ? " on" : "")}
                onMouseDown={(e) => { e.preventDefault(); add(o.repo); }} onMouseEnter={() => setActive(i)}>
                <span className="rp-opt-text">
                  <span className="rp-name">{o.title}</span>
                  {o.sub && <span className="rp-sub">{o.sub}</span>}
                </span>
                {o.tag && <span className="rp-tag">{o.tag}</span>}
              </li>
            ))}
          </ul>
        )}
      </div>
      {loadError
        ? <FieldHint>Can't list GitHub repositories: {loadError}. <button type="button" className="rp-retry" onClick={() => load(true)}>Try again</button></FieldHint>
        : <FieldHint>Your GitHub repositories, as the daemon's gh login sees them. Anything else: paste a clone URL{folders.ok ? " or add a local folder" : " or a path"}.</FieldHint>}
    </div>
  );
}
