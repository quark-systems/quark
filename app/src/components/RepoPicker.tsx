// Picks a Project's repositories: GitHub repositories the daemon's gh account can reach, found
// by typing part of a name; a local folder through the system dialog; or, for anything else,
// a clone URL pasted in. The search is a cmdk list in a Radix popover, the shadcn combobox.
import React, { useEffect, useMemo, useRef, useState } from "react";
import { Command } from "cmdk";
import { Popover } from "radix-ui";
import { api, ApiError, ForgeRepository, NotAvailable } from "../api";
import { folderPicking, pickFolder } from "../folders";
import { Button, controlClass, cx, FieldHint } from "../ui";
import { ago } from "../util";

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

const NAME = "truncate font-mono text-s text-fg";
const SUB = "truncate font-sans text-s text-faint";
const TAG = "shrink-0 rounded-full border border-line-2 px-[7px] font-sans text-xs font-medium leading-[18px] text-dim";
const KIND_DOT: Record<PickedRepo["kind"], string> = { github: "bg-accent", folder: "bg-yellow", url: "bg-faint" };

export function RepoPicker({ value, onChange }: { value: PickedRepo[]; onChange: (v: PickedRepo[]) => void }) {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [repos, setRepos] = useState<ForgeRepository[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const anchor = useRef<HTMLDivElement>(null);

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
      listed.push({ key: "typed", repo: typed, title: `Use ${typed.label}`, sub: typed.kind === "folder" ? "Local folder" : typed.kind === "url" ? "Clone URL" : "GitHub repository" });
    }
    return listed;
  }, [repos, query, value]);

  const close = () => { setOpen(false); setQuery(""); };
  const add = (r: PickedRepo) => {
    if (!chosen.has(r.url)) onChange([...value, r]);
    setQuery("");
    setOpen(false);
    input.current?.focus();
  };
  const folders = folderPicking();
  const addFolder = async () => {
    const p = await pickFolder("Add a local repository");
    if (p) add({ url: p, label: p, kind: "folder" });
  };

  const showList = open && (options.length > 0 || repos === null || query !== "");
  return (
    <div className="flex flex-col gap-2">
      {value.length > 0 && (
        <ul className="m-0 flex list-none flex-col divide-y divide-line rounded-m border border-line-2 bg-surface-1 p-0" aria-label="Chosen repositories">
          {value.map((r) => (
            <li key={r.url} className="flex min-h-control items-center gap-2 py-0.5 pr-1 pl-3">
              <span className={cx("size-2 shrink-0 rounded-[2px]", KIND_DOT[r.kind])} aria-hidden="true" />
              <span className={NAME}>{r.label}</span>
              {r.private && <span className={TAG}>private</span>}
              {r.kind !== "github" && <span className={SUB}>{r.kind === "folder" ? "local folder" : "clone URL"}</span>}
              <Button kind="quiet" className="ml-auto" aria-label={`Remove ${r.label}`} onClick={() => onChange(value.filter((x) => x.url !== r.url))}>Remove</Button>
            </li>
          ))}
        </ul>
      )}
      {/* We rank the matches ourselves (matchRepos), so cmdk only does the keyboard and the ARIA. */}
      <Command shouldFilter={false} loop label="Find a repository">
        <Popover.Root open={showList} onOpenChange={(o) => (o ? setOpen(true) : close())}>
          <Popover.Anchor asChild>
            <div ref={anchor} className="flex gap-2">
              <Command.Input ref={input} value={query} className={controlClass({}, "flex-1")}
                placeholder={repos && repos.length ? `Search ${repos.length} GitHub repositories, or paste a clone URL` : "Paste a clone URL or owner/name"}
                onValueChange={(q) => { setQuery(q); setOpen(true); }}
                onFocus={() => setOpen(true)} onClick={() => setOpen(true)}
                onKeyDown={(e) => { if (e.key === "Escape" && showList) { e.preventDefault(); close(); } }}
                onBlur={(e) => { if (!anchor.current?.parentElement?.contains(e.relatedTarget as Node)) close(); }} />
              {folders.ok && <Button onClick={addFolder}>Add local folder…</Button>}
            </div>
          </Popover.Anchor>
          <Popover.Portal>
            <Popover.Content align="start" sideOffset={4}
              // Focus stays in the search box; clicks inside the list must not blur it.
              onOpenAutoFocus={(e) => e.preventDefault()} onCloseAutoFocus={(e) => e.preventDefault()}
              onMouseDown={(e) => e.preventDefault()}
              onInteractOutside={(e) => { if (anchor.current?.contains(e.target as Node)) e.preventDefault(); }}
              className="z-50 w-(--radix-popover-trigger-width) rounded-m bg-popover text-fg shadow-popover outline-none">
              <Command.List label="Repositories" className="max-h-80 overflow-auto p-1">
                {repos === null && <Command.Loading><div className="px-3 py-2 font-sans text-s text-faint">Loading your GitHub repositories…</div></Command.Loading>}
                {repos !== null && (
                  <Command.Empty className="px-3 py-2 font-sans text-s text-faint">
                    No repository matches “{query}”. Paste a clone URL to use one that is not on GitHub.
                  </Command.Empty>
                )}
                {options.map((o) => (
                  <Command.Item key={o.key} value={o.key} onSelect={() => add(o.repo)}
                    className="flex cursor-pointer items-center gap-3 rounded-s px-3 py-1.5 data-[selected=true]:bg-selection-strong">
                    <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                      <span className={NAME}>{o.title}</span>
                      {o.sub && <span className={SUB}>{o.sub}</span>}
                    </span>
                    {o.tag && <span className={TAG}>{o.tag}</span>}
                  </Command.Item>
                ))}
              </Command.List>
            </Popover.Content>
          </Popover.Portal>
        </Popover.Root>
      </Command>
      {loadError
        ? <FieldHint>Can't list GitHub repositories: {loadError}. <button type="button" className="cursor-pointer border-0 bg-transparent p-0 font-[inherit] text-link underline" onClick={() => load(true)}>Try again</button></FieldHint>
        : <FieldHint>Your GitHub repositories, as the daemon's gh login sees them. Anything else: paste a clone URL{folders.ok ? " or add a local folder" : " or a path"}.</FieldHint>}
    </div>
  );
}
