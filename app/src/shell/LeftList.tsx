// The left list: All projects, then each project with its coordinator pinned on top and its
// workers under it, then Accounts and Hosts in the footer (plans/ui-shell-design.md). The app menu
// (Appearance, Updates, Setup) is the gear in its head.
import React, { useMemo, useState } from "react";
import { href, Route, useRoute } from "../nav";
import { api } from "../api";
import { setCollections, useStore } from "../store";
import { CoordinatorMark, CountBadge, Menu, MenuGroup, Row, StatusDot, TextInput } from "../ui";
import { useLabels } from "../persona";
import { leftListGroups, ProjectGroup, visibleProjects } from "./groups";
import { useShowHidden } from "./reveal";
import { RemoveCollection } from "./RemoveCollection";
import { CollectionSection, collectionSections, dropped, moved } from "./collections";
import { useGroupByCollection } from "./grouping";
import { NextAttention } from "./NextAttention";
import "./shell.css";
import { BeadText } from "../ui/BeadText";
import { AppMenu } from "./AppMenu";

export function LeftList({ mod }: { mod: string }) {
  const route = useRoute();
  const projects = useStore((s) => s.projects);
  const tasks = useStore((s) => s.tasks);
  const decisions = useStore((s) => s.decisions);
  const showHidden = useShowHidden();
  const groups = useMemo(() => leftListGroups(visibleProjects(projects, showHidden), tasks, decisions), [projects, tasks, decisions, showHidden]);
  const openPrs = useStore((s) => Object.values(s.pullRequests).filter((p) => p.state === "open" || p.state === "draft").length);
  const collections = useStore((s) => s.collections);
  const byCollection = useGroupByCollection();
  const grouped = byCollection && collections.length > 0;
  const sections = useMemo(() => collectionSections(collections, groups), [collections, groups]);
  /** Moves a collection among the others and saves each one's new place. */
  const moveCollection = async (id: string, delta: number) => {
    const was = collections.map((c) => c.id);
    const order = moved(was, id, delta);
    if (order.join("\u0000") === was.join("\u0000")) return;
    try {
      for (const [at, cid] of order.entries()) await api.updateCollection(cid, { position: at });
      setCollections(await api.collections());
    } catch { /* the order stays as it was */ }
  };

  return (
    <nav className="left-list sidebar" aria-label="Projects">
      <div className="drag-strip" data-tauri-drag-region />
      <div className="ll-head">
        <span className="ll-logo" aria-hidden="true">
          {/* The Quark mark, small-size drawing (docs/brand/marks/quark-mark-small.svg). */}
          <svg width="16" height="16" viewBox="0 0 100 100"><path d="M60 12 H36 Q12 12 12 36 V64 Q12 88 36 88 H64 Q88 88 88 64 V40" fill="none" stroke="currentColor" strokeWidth="15" strokeLinejoin="round" /><circle cx="40" cy="60" r="11" fill="currentColor" /><circle cx="80" cy="20" r="13" fill="currentColor" /></svg>
        </span>
        <a className="ll-brand" href={href({ name: "projects" })}>Quark</a>
        <AppMenu />
        <a className="ll-new" href={href({ name: "new" })} aria-label="New project" title="New project" data-testid="nav-new-project">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><path d="M12 5v14M5 12h14" /></svg>
        </a>
      </div>
      <NextAttention mod={mod} />
      <div className="ll-top">
        <Row href={href({ name: "projects" })} current={route.name === "projects" || route.name === "prs" || (route.name === "inbox" && !route.id)} title="All projects"
          trail={openPrs ? <span className="ll-trail" data-testid="ll-open-prs">{openPrs} PR{openPrs === 1 ? "" : "s"}</span> : undefined}
          lead={<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true"><rect x="3" y="3" width="7" height="7" rx="1.5" /><rect x="14" y="3" width="7" height="7" rx="1.5" /><rect x="3" y="14" width="7" height="7" rx="1.5" /><rect x="14" y="14" width="7" height="7" rx="1.5" /></svg>} />
      </div>
      <div className="ll-projects">
        {grouped
          ? sections.map((s) => (
              <CollectionRows key={s.id ?? "ungrouped"} section={s} route={route}
                reorder={{ move: moveCollection, count: collections.length }} />
            ))
          : groups.map((g) => <ProjectSection key={g.project.id} g={g} route={route} />)}
        {!groups.length && <div className="ll-empty">No projects yet. <a href={href({ name: "new" })}>Create one</a>.</div>}
      </div>
      <div className="ll-foot">
        <Row href={href({ name: "accounts" })} current={route.name === "accounts"} testid="nav-accounts" title="Accounts"
          lead={<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true"><circle cx="12" cy="8" r="4" /><path d="M4 21c1.5-4 4.5-6 8-6s6.5 2 8 6" /></svg>} />
        <Row href={href({ name: "hosts" })} current={route.name === "hosts"} testid="nav-hosts" title="Hosts"
          lead={<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true"><rect x="3" y="4" width="18" height="7" rx="2" /><rect x="3" y="13" width="18" height="7" rx="2" /></svg>} />
      </div>
    </nav>
  );
}

function ProjectSection({ g, route }: { g: ProjectGroup; route: Route }) {
  const l = useLabels(g.project.id);
  const coordCurrent = route.name === "project" && route.id === g.project.id;
  return (
    <section className="ll-project" aria-label={g.project.name} data-testid="ll-project">
      {/* The project row is the coordinator's: there is one coordinator per project and its
          row went to the same place, so naming it on a row of its own cost a row per project.
          What it has to say stays, as the line under the project's name. */}
      <Row href={href({ name: "project", id: g.project.id })} current={coordCurrent} testid="ll-coordinator"
        lead={<CoordinatorMark />}
        title={<span title={`${g.project.name} — ${l.Role("coordinator")}`}>{g.project.name}</span>}
        sub={g.coordSub}
        trail={<CountBadge n={g.waiting} tone="needs-you" label={`${g.waiting} waiting on you`} />} />
    </section>
  );
}

/** One collection's heading with its projects under it. Inside a collection the projects can be
 *  reordered by dragging, or with Alt and the arrow keys for anyone not using a mouse; the order
 *  is saved on the collection. Ungrouped projects have no order of their own, so they are not
 *  draggable. */
/** How a collection is moved among the others, from its heading's menu. */
interface CollectionReorder {
  move: (id: string, delta: number) => Promise<void>;
  /** How many collections there are: with one there is nothing to reorder. */
  count: number;
}

function CollectionRows({ section, route, reorder }: { section: CollectionSection; route: Route; reorder: CollectionReorder }) {
  const [dragging, setDragging] = useState<string | null>(null);
  const [over, setOver] = useState<string | null>(null);
  const [renaming, setRenaming] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [draft, setDraft] = useState(section.name);
  // Ungrouped has no collection behind it, so its fold is only for this run.
  const [ungroupedFolded, setUngroupedFolded] = useState(false);

  const collapsed = section.id ? section.collapsed : ungroupedFolded;
  const sortable = section.id !== null && section.groups.length > 1;

  const reload = async () => setCollections(await api.collections());
  const rename = async (next: string) => {
    const name = next.trim();
    if (!section.id || name === "" || name === section.name) return;
    try { await api.updateCollection(section.id, { name }); await reload(); } catch { /* name kept */ }
  };
  const fold = async (next: boolean) => {
    if (!section.id) { setUngroupedFolded(next); return; }
    try { await api.updateCollection(section.id, { collapsed: next }); await reload(); } catch { /* stays open */ }
  };

  const save = async (next: string[]) => {
    if (!section.id || next.join("\u0000") === section.members.join("\u0000")) return;
    try {
      await api.updateCollection(section.id, { projects: next });
      setCollections(await api.collections());
    } catch { /* the daemon refused it; the list stays as it was */ }
  };
  const drop = (before: string | null) => {
    const id = dragging;
    setDragging(null); setOver(null);
    // The whole membership is reordered, so a member the list is not showing keeps its place.
    if (id && id !== before) void save(dropped(section.members, id, before));
  };

  return (
    <section className="ll-collection" data-testid="ll-collection" aria-label={section.name}
      onDragOver={(e) => { if (sortable && dragging) { e.preventDefault(); setOver(null); } }}
      onDrop={(e) => { if (sortable && dragging) { e.preventDefault(); drop(null); } }}>
      <div className="ll-collection-head">
        {renaming ? (
          <TextInput autoFocus value={draft} aria-label={`Rename ${section.name}`} data-testid="ll-collection-rename"
            onChange={(e) => setDraft(e.target.value)}
            onBlur={() => setRenaming(false)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setRenaming(false);
              if (e.key === "Enter") { setRenaming(false); void rename(draft); }
            }} />
        ) : (<>
          <h2 className="ll-collection-name">
            <button type="button" className="ll-collection-toggle" aria-expanded={!collapsed}
              onClick={() => void fold(!collapsed)} data-testid="ll-collection-toggle">
              <svg className={"ll-chevron" + (collapsed ? " folded" : "")} width="10" height="10" viewBox="0 0 24 24"
                fill="none" stroke="currentColor" strokeWidth="2.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M6 9l6 6 6-6" />
              </svg>
              {section.name}
              {section.groups.length > 0 && (
                <span className="ll-collection-count">({section.groups.length})</span>
              )}
            </button>
          </h2>
          <span className="ll-collection-rule" aria-hidden="true" />
          {section.id && (
            <Menu label={`${section.name} collection`} align="end" trigger={
              <button type="button" className="ll-collection-menu" aria-label={`${section.name} collection`} data-testid="ll-collection-menu">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><circle cx="5" cy="12" r="1.6" /><circle cx="12" cy="12" r="1.6" /><circle cx="19" cy="12" r="1.6" /></svg>
              </button>
            } groups={menuGroups(section.id, section.name, reorder, () => { setDraft(section.name); setRenaming(true); }, () => setRemoving(true))} />
          )}
        </>)}
      </div>
      {removing && section.id && <RemoveCollection id={section.id} name={section.name} onClose={() => setRemoving(false)} />}
      {!collapsed && section.groups.length === 0 && <p className="ll-collection-empty">Nothing in here yet.</p>}
      {!collapsed && section.groups.map((g) => (
        <div key={g.project.id} className={"ll-collection-item" + (over === g.project.id ? " ll-drop-before" : "")}
          draggable={sortable}
          onDragStart={(e) => { setDragging(g.project.id); e.dataTransfer.effectAllowed = "move"; e.dataTransfer.setData("text/plain", g.project.id); }}
          onDragEnd={() => { setDragging(null); setOver(null); }}
          onDragOver={(e) => { if (sortable && dragging) { e.preventDefault(); e.stopPropagation(); setOver(g.project.id); } }}
          onDrop={(e) => { if (sortable && dragging) { e.preventDefault(); e.stopPropagation(); drop(g.project.id); } }}
          onKeyDown={(e) => {
            if (!sortable || !e.altKey || (e.key !== "ArrowUp" && e.key !== "ArrowDown")) return;
            e.preventDefault();
            void save(moved(section.members, g.project.id, e.key === "ArrowUp" ? -1 : 1));
          }}>
          <ProjectSection g={g} route={route} />
        </div>
      ))}
    </section>
  );
}

/** A collection heading's menu: rename it, move it among the others, or remove it. The Reorder
 *  group is left out while it is the only collection. */
function menuGroups(id: string, name: string, reorder: CollectionReorder, rename: () => void, remove: () => void): MenuGroup[] {
  const groups: MenuGroup[] = [{ actions: [{ label: "Rename", onSelect: rename }] }];
  if (reorder.count > 1) {
    groups.push({ label: "Reorder", actions: [
      { label: "Move up", testid: "ll-collection-up", onSelect: () => void reorder.move(id, -1) },
      { label: "Move down", testid: "ll-collection-down", onSelect: () => void reorder.move(id, 1) },
    ] });
  }
  groups.push({ actions: [{ label: "Remove collection", onSelect: remove }] });
  return groups;
}
