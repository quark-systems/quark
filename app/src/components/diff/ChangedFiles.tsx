// A pull request's diff with GitHub's "Files changed" tree beside it: only the changed files,
// single-folder chains joined, a status mark per file, a filter box and a file type / viewed
// filter. Clicking a file scrolls the diff to it; scrolling the diff marks the file in view.
import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Menu } from "../../ui";
import type { DiffFile } from "./model";
import { DiffFocus, DiffView, LineComment, OnComment } from "./DiffView";
import type { FileSource } from "./ImageDiff";
import { buildFileTree, extensionOf, extensions, filterFiles, treeOrder, TreeNode, TreeStatus, visibleRows } from "./fileTree";
import { useViewedFiles } from "./viewed";

const TREE_KEY = "quark.diffTree";
const readTreeOpen = () => { try { return localStorage.getItem(TREE_KEY) !== "hidden"; } catch { return true; } };
const writeTreeOpen = (open: boolean) => { try { if (open) localStorage.removeItem(TREE_KEY); else localStorage.setItem(TREE_KEY, "hidden"); } catch { /* storage unavailable */ } };

export function ChangedFiles({ files, viewedKey, conflicts, comments, onComment, focus, source, children }: {
  files: DiffFile[];
  /** Where the Viewed marks are kept (one per pull request). */
  viewedKey: string;
  conflicts?: ReadonlySet<string>;
  comments?: LineComment[]; onComment?: OnComment; focus?: DiffFocus | null; source?: FileSource;
  /** Notes shown over the diff (a cut diff, how to comment). */
  children?: React.ReactNode;
}) {
  const [treeOpen, setTreeOpen] = useState(readTreeOpen);
  const [hiddenExt, setHiddenExt] = useState<ReadonlySet<string>>(new Set());
  const [hideViewed, setHideViewed] = useState(false);
  const [active, setActive] = useState<string | null>(null);
  const viewed = useViewedFiles(viewedKey, files);
  const scroller = useRef<HTMLDivElement>(null);
  // A file picked in the tree stays marked while the diff cannot scroll it to the top (near the end).
  const pinned = useRef<string | null>(null);

  // The diff reads in the tree's order. Extension and viewed filters narrow both; the filter box only the tree.
  const ordered = useMemo(() => treeOrder(buildFileTree(files)), [files]);
  const shown = useMemo(() => ordered.filter((f) => !hiddenExt.has(extensionOf(f.path)) && !(hideViewed && viewed.isViewed(f.path))),
    [ordered, hiddenExt, hideViewed, viewed]);

  const toggleTree = () => { const v = !treeOpen; setTreeOpen(v); writeTreeOpen(v); };

  // Mark the file whose header is at the top of the diff.
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    let frame = 0;
    const update = () => {
      frame = 0;
      if (!el.clientHeight) return; // on a hidden tab nothing has a position
      const top = el.getBoundingClientRect().top + 24;
      let current: string | null = null;
      for (const f of el.querySelectorAll<HTMLElement>(".dv-file[data-path]")) {
        if (f.getBoundingClientRect().top <= top) current = f.dataset.path ?? null; else break;
      }
      const atEnd = el.scrollTop + el.clientHeight >= el.scrollHeight - 2;
      if (pinned.current && atEnd) return;
      pinned.current = null;
      if (current == null) current = el.querySelector<HTMLElement>(".dv-file[data-path]")?.dataset.path ?? null;
      setActive(current);
    };
    const onScroll = () => { if (!frame) frame = requestAnimationFrame(update); };
    const unpin = () => { pinned.current = null; };
    // Also when the tab is shown or the window resized.
    const sized = new ResizeObserver(onScroll);
    sized.observe(el);
    el.addEventListener("scroll", onScroll, { passive: true });
    el.addEventListener("wheel", unpin, { passive: true });
    el.addEventListener("touchmove", unpin, { passive: true });
    return () => {
      sized.disconnect();
      el.removeEventListener("scroll", onScroll);
      el.removeEventListener("wheel", unpin);
      el.removeEventListener("touchmove", unpin);
      if (frame) cancelAnimationFrame(frame);
    };
  }, [shown]);

  const jump = useCallback((path: string) => {
    const el = scroller.current;
    const target = el?.querySelector<HTMLElement>(`.dv-file[data-path="${CSS.escape(path)}"]`);
    if (!el || !target) return;
    pinned.current = path;
    setActive(path);
    const pad = parseFloat(getComputedStyle(el).paddingTop) || 0;
    el.scrollTo({ top: target.getBoundingClientRect().top - el.getBoundingClientRect().top + el.scrollTop - pad });
  }, []);

  const exts = useMemo(() => extensions(files), [files]);
  const filterOn = hiddenExt.size > 0 || hideViewed;
  const filterMenu = (
    <Menu label="Filter files" align="end"
      trigger={<button type="button" className={"ft-filter-btn" + (filterOn ? " on" : "")} aria-label="Filter files by type and viewed"
        title="Filter by file type or viewed" data-testid="file-filter-menu"><FilterIcon /></button>}
      groups={[
        { label: "File types", checkboxes: exts.map(({ ext, n }) => ({
          label: ext, detail: n, checked: !hiddenExt.has(ext),
          onCheckedChange: (on: boolean) => setHiddenExt((s) => { const next = new Set(s); if (on) next.delete(ext); else next.add(ext); return next; }),
        })) },
        { checkboxes: [{ label: "Viewed files", detail: viewed.count, checked: !hideViewed, onCheckedChange: (on: boolean) => setHideViewed(!on), testid: "filter-viewed" }] },
        ...(filterOn ? [{ actions: [{ label: "Show all files", onSelect: () => { setHiddenExt(new Set()); setHideViewed(false); } }] }] : []),
      ]} />
  );

  const toggle = (
    <button type="button" className="dv-icon" onClick={toggleTree} aria-pressed={treeOpen} data-testid="file-tree-toggle"
      title={treeOpen ? "Hide the file tree" : "Show the file tree"} aria-label={treeOpen ? "Hide the file tree" : "Show the file tree"}>
      <TreeIcon />
    </button>
  );

  return (
    <div className={"cf" + (treeOpen ? " with-tree" : "")}>
      {treeOpen && <FileTree files={shown} active={active} onOpen={jump} conflicts={conflicts} filterMenu={filterMenu} />}
      <div className="diff-body" ref={scroller} data-testid="pr-diff-scroll">
        {children}
        {shown.length > 0
          ? <DiffView files={shown} comments={comments} onComment={onComment} focus={focus} source={source}
            lead={<>{toggle}{!treeOpen && filterMenu}</>} viewed={viewed} conflicts={conflicts} />
          : <>
            <div className="dv-bar">{toggle}{!treeOpen && filterMenu}</div>
            <div className="empty">No files match the filter.</div>
          </>}
      </div>
    </div>
  );
}

/** The tree: a filter box, then folders and files. Arrow keys move, Left and Right fold, Enter opens. */
export function FileTree({ files, active, onOpen, conflicts, filterMenu }: {
  files: DiffFile[]; active: string | null; onOpen: (path: string) => void;
  conflicts?: ReadonlySet<string>; filterMenu?: React.ReactNode;
}) {
  const [query, setQuery] = useState("");
  const [closed, setClosed] = useState<ReadonlySet<string>>(new Set());
  const [focusKey, setFocusKey] = useState<string | null>(null);
  const tree = useMemo(() => buildFileTree(filterFiles(files, query)), [files, query]);
  // While filtering every folder is open, so every match shows.
  const rows = useMemo(() => visibleRows(tree, query.trim() ? new Set() : closed), [tree, closed, query]);
  const list = useRef<HTMLDivElement>(null);
  const keyOf = (n: TreeNode) => (n.kind === "dir" ? "d:" : "f:") + n.path;
  const tabKey = rows.some((r) => keyOf(r.node) === focusKey) ? focusKey
    : rows.find((r) => r.node.kind === "file" && r.node.path === active) ? "f:" + active : rows[0] ? keyOf(rows[0].node) : null;

  // Keep the file in view marked and visible in the tree.
  useEffect(() => {
    if (!active || !list.current) return;
    const row = list.current.querySelector<HTMLElement>(`[data-key="${CSS.escape("f:" + active)}"]`);
    if (!row) return;
    const box = list.current.getBoundingClientRect(), r = row.getBoundingClientRect();
    if (r.top < box.top) list.current.scrollTop -= box.top - r.top + 4;
    else if (r.bottom > box.bottom) list.current.scrollTop += r.bottom - box.bottom + 4;
  }, [active]);

  const setOpen = (path: string, open: boolean) => setClosed((s) => {
    const next = new Set(s);
    if (open) next.delete(path); else next.add(path);
    return next;
  });
  const focusRow = (key: string) => {
    setFocusKey(key);
    list.current?.querySelector<HTMLElement>(`[data-key="${CSS.escape(key)}"]`)?.focus();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    const i = rows.findIndex((r) => keyOf(r.node) === tabKey);
    if (i < 0) return;
    const { node, parent } = rows[i];
    const open = node.kind === "dir" && (!!query.trim() || !closed.has(node.path));
    const move = (j: number) => { const r = rows[Math.max(0, Math.min(rows.length - 1, j))]; if (r) focusRow(keyOf(r.node)); };
    switch (e.key) {
      case "ArrowDown": move(i + 1); break;
      case "ArrowUp": move(i - 1); break;
      case "Home": move(0); break;
      case "End": move(rows.length - 1); break;
      case "ArrowRight":
        if (node.kind === "dir") { if (!open) setOpen(node.path, true); else move(i + 1); }
        break;
      case "ArrowLeft":
        if (node.kind === "dir" && open && !query.trim()) setOpen(node.path, false);
        else if (parent) focusRow("d:" + parent);
        break;
      case "Enter": case " ":
        if (node.kind === "dir") setOpen(node.path, !open); else onOpen(node.path);
        break;
      default: return;
    }
    e.preventDefault();
  };

  return (
    <aside className="ft" aria-label="Changed files" data-testid="file-tree">
      <div className="ft-head">
        <div className="ft-search">
          <SearchIcon />
          <input type="search" value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Filter files…"
            aria-label="Filter changed files" data-testid="file-tree-filter"
            onKeyDown={(e) => { if (e.key === "Escape" && query) { e.stopPropagation(); setQuery(""); } }} />
        </div>
        {filterMenu}
      </div>
      <div className="ft-list" role="tree" aria-label="Changed files" ref={list} onKeyDown={onKeyDown}>
        {rows.map(({ node, depth }) => {
          const key = keyOf(node);
          const pad = { paddingLeft: 6 + depth * 14 };
          if (node.kind === "dir") {
            const open = !!query.trim() || !closed.has(node.path);
            return (
              <div key={key} data-key={key} role="treeitem" aria-level={depth + 1} aria-expanded={open} tabIndex={key === tabKey ? 0 : -1}
                className="ft-row dir" style={pad} title={node.path} onFocus={() => setFocusKey(key)}
                onClick={() => { setFocusKey(key); if (!query.trim()) setOpen(node.path, !open); }}>
                <Chevron open={open} />
                <FolderIcon open={open} />
                <span className="ft-name">{node.name}</span>
              </div>
            );
          }
          const status: TreeStatus = conflicts?.has(node.path) ? "conflicted" : node.file.status;
          const current = node.path === active;
          return (
            <div key={key} data-key={key} role="treeitem" aria-level={depth + 1} aria-selected={current} aria-current={current ? "location" : undefined}
              tabIndex={key === tabKey ? 0 : -1} className={"ft-row file" + (current ? " current" : "")} style={pad}
              title={`${node.path} (${STATUS_LABEL[status]})`} data-testid="file-tree-file" data-status={status}
              onFocus={() => setFocusKey(key)} onClick={() => { setFocusKey(key); onOpen(node.path); }}>
              <span className="ft-chev-space" />
              <FileStatusIcon status={status} />
              <span className="ft-name">{node.name}</span>
            </div>
          );
        })}
        {!rows.length && <div className="ft-empty">{files.length ? "No files match." : "No files."}</div>}
      </div>
    </aside>
  );
}

export const STATUS_LABEL: Record<TreeStatus, string> = { added: "added", modified: "modified", deleted: "deleted", renamed: "renamed", conflicted: "conflict" };

/** A file outline with the change inside, as GitHub marks files: + added, ± modified, − deleted,
 *  → renamed, ! conflict. The mark, not only its color, tells them apart. */
export function FileStatusIcon({ status }: { status: TreeStatus }) {
  return (
    <svg className={"ft-status st-" + status} width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3"
      strokeLinecap="round" strokeLinejoin="round" role="img" aria-label={STATUS_LABEL[status]}>
      <rect x="2.5" y="1.75" width="11" height="12.5" rx="2" />
      {status === "added" && <path d="M8 5.25v5.5M5.25 8h5.5" />}
      {status === "deleted" && <path d="M5.25 8h5.5" />}
      {status === "modified" && <path d="M8 4.5v4M6 6.5h4M6 11h4" />}
      {status === "renamed" && <path d="M5 8h5.5M8.5 5.75 10.75 8 8.5 10.25" />}
      {status === "conflicted" && <><path d="M8 4.75v4" /><circle cx="8" cy="11" r=".4" fill="currentColor" /></>}
    </svg>
  );
}

const svg = { width: 16, height: 16, viewBox: "0 0 16 16", fill: "none", stroke: "currentColor", strokeWidth: 1.3, strokeLinecap: "round" as const, strokeLinejoin: "round" as const, "aria-hidden": true };
function Chevron({ open }: { open: boolean }) {
  return <svg {...svg} width={12} height={12} className={"ft-chev" + (open ? " open" : "")}><path d="M6 4l4 4-4 4" /></svg>;
}
function FolderIcon({ open }: { open: boolean }) {
  return open
    ? <svg {...svg} className="ft-folder"><path d="M1.75 12.5V3.75c0-.55.45-1 1-1h3.2l1.5 1.5h5.8c.55 0 1 .45 1 1v1" /><path d="M1.75 12.5 3.6 7.2c.14-.4.52-.7.95-.7h9.7c.7 0 1.18.68.95 1.33l-1.6 4.5c-.14.4-.52.67-.95.67H1.75Z" /></svg>
    : <svg {...svg} className="ft-folder"><path d="M1.75 3.75c0-.55.45-1 1-1h3.2l1.5 1.5h5.8c.55 0 1 .45 1 1v6.5c0 .55-.45 1-1 1H2.75c-.55 0-1-.45-1-1Z" /></svg>;
}
function SearchIcon() {
  return <svg {...svg} width={14} height={14} className="ft-search-icon"><circle cx="7" cy="7" r="4.5" /><path d="m10.5 10.5 3.5 3.5" /></svg>;
}
function FilterIcon() {
  return <svg {...svg} width={14} height={14}><path d="M2 4h12M4.5 8h7M7 12h2" /></svg>;
}
function TreeIcon() {
  return <svg {...svg} width={14} height={14}><rect x="1.75" y="2.25" width="12.5" height="11.5" rx="2" /><path d="M6 2.25v11.5" /></svg>;
}
