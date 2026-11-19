// The glyphs and colours a project can be shown with. Drawn here rather than pulled from an
// icon package: the app draws every other icon inline, and a picker needs the whole set in the
// bundle, which a package would make far larger than the few dozen used here.
import React from "react";

/** A glyph, by the name it is stored under. `words` are what the picker searches. */
export interface ProjectIcon { name: string; words: string; path: React.ReactNode }

const p = (d: string) => <path d={d} />;

export const PROJECT_ICONS: ProjectIcon[] = [
  { name: "cube", words: "project box package default", path: <><path d="M12 2.6l8 4.3v9.4l-8 4.3-8-4.3V6.9z" /><path d="M4 7l8 4.3L20 7M12 11.3V20" /></> },
  { name: "server", words: "server backend host api", path: <><rect x="3" y="4" width="18" height="7" rx="2" /><rect x="3" y="13" width="18" height="7" rx="2" /><path d="M7 7.5v.01M7 16.5v.01" /></> },
  { name: "database", words: "database store data sql", path: <><ellipse cx="12" cy="6" rx="8" ry="3" /><path d="M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6" /><path d="M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3" /></> },
  { name: "chart", words: "chart metrics analytics telemetry graph", path: <><path d="M4 20V10M10 20V4M16 20v-7M22 20H2" /></> },
  { name: "pulse", words: "pulse monitoring health telemetry", path: p("M2 12h4l3-8 4 16 3-8h6") },
  { name: "cloud", words: "cloud deploy hosting infra", path: p("M7 18h10a4 4 0 0 0 .3-8A6 6 0 0 0 6 11a3.5 3.5 0 0 0 1 7z") },
  { name: "globe", words: "globe web site public network", path: <><circle cx="12" cy="12" r="9" /><path d="M3 12h18M12 3c2.5 2.6 2.5 15 0 18-2.5-3-2.5-15.4 0-18z" /></> },
  { name: "terminal", words: "terminal cli shell command", path: <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M7 9l3 3-3 3M13 15h4" /></> },
  { name: "code", words: "code source repo library", path: p("M9 17l-5-5 5-5M15 7l5 5-5 5") },
  { name: "branch", words: "branch git fork version", path: <><circle cx="7" cy="6" r="2.4" /><circle cx="7" cy="18" r="2.4" /><circle cx="17" cy="9" r="2.4" /><path d="M7 8.4v7.2M17 11.4c0 3.2-3.4 3.2-6 4.2" /></> },
  { name: "bug", words: "bug fix defect issue", path: <><rect x="8" y="7" width="8" height="12" rx="4" /><path d="M8 11H4M20 11h-4M8 16H4.5M20 16h-3.5M9.5 7L8 4.5M14.5 7L16 4.5" /></> },
  { name: "shield", words: "shield security auth permissions", path: p("M12 3l7 3v6c0 4-3 7.3-7 9-4-1.7-7-5-7-9V6z") },
  { name: "key", words: "key secret credential token auth", path: <><circle cx="8" cy="12" r="4" /><path d="M12 12h9M18 12v3.5M15.5 12v2.5" /></> },
  { name: "bolt", words: "bolt fast performance speed", path: p("M13 2L5 13h6l-1 9 8-11h-6z") },
  { name: "flask", words: "flask experiment lab research spike", path: <><path d="M10 3v6L4.6 18a2 2 0 0 0 1.7 3h11.4a2 2 0 0 0 1.7-3L14 9V3" /><path d="M9 3h6M7.6 14h8.8" /></> },
  { name: "book", words: "book docs documentation guide notes", path: <><path d="M4 5a2 2 0 0 1 2-2h13v16H6a2 2 0 0 0-2 2z" /><path d="M8 3v14" /></> },
  { name: "inbox", words: "inbox mail messages queue", path: <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M3 13h5l1.5 2.5h5L16 13h5" /></> },
  { name: "bell", words: "bell alerts notifications paging", path: <><path d="M6 9a6 6 0 0 1 12 0c0 5 2 6 2 6H4s2-1 2-6z" /><path d="M10.5 19a2 2 0 0 0 3 0" /></> },
  { name: "rocket", words: "rocket launch release ship", path: <><path d="M12 3c3.5 2 5.5 5.6 5.5 9.5L12 18l-5.5-5.5C6.5 8.6 8.5 5 12 3z" /><circle cx="12" cy="10" r="1.8" /><path d="M8 17l-2 4 4-2M16 17l2 4-4-2" /></> },
  { name: "wrench", words: "wrench tools maintenance chore", path: p("M20 5.5a5 5 0 0 1-6.6 6.6L5.8 19.7a2 2 0 0 1-2.8-2.8l7.6-7.6A5 5 0 0 1 17.2 2.7l-3 3 2.6 2.6 3-3c.1.4.2.8.2 1.2z") },
  { name: "gear", words: "gear settings config platform", path: <><circle cx="12" cy="12" r="3" /><path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.2 2.2M16.9 16.9l2.2 2.2M4.9 19.1l2.2-2.2M16.9 7.1l2.2-2.2" /></> },
  { name: "layers", words: "layers stack platform architecture", path: p("M12 3l9 5-9 5-9-5zM3 13l9 5 9-5M3 17l9 5 9-5") },
  { name: "grid", words: "grid apps dashboard overview", path: <><rect x="3" y="3" width="7" height="7" rx="1.5" /><rect x="14" y="3" width="7" height="7" rx="1.5" /><rect x="3" y="14" width="7" height="7" rx="1.5" /><rect x="14" y="14" width="7" height="7" rx="1.5" /></> },
  { name: "home", words: "home house building office site", path: <><path d="M3.5 10.5L12 3.5l8.5 7" /><path d="M5.5 9.5V20h13V9.5" /><path d="M10 20v-5.5h4V20" /></> },
  { name: "folder", words: "folder files storage archive", path: p("M3 7a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2V18a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z") },
  { name: "star", words: "star favourite important flagship", path: p("M12 3.5l2.7 5.6 6.1.8-4.4 4.3 1.1 6.1L12 17.4 6.5 20.3l1.1-6.1L3.2 9.9l6.1-.8z") },
  { name: "flag", words: "flag milestone goal target", path: <><path d="M5 21V4M5 4h12l-2 4 2 4H5" /></> },
  { name: "people", words: "people team users customers", path: <><circle cx="9" cy="8" r="3.2" /><path d="M3 20c1.2-3.4 3.5-5 6-5s4.8 1.6 6 5" /><path d="M16 5.5a3.2 3.2 0 0 1 0 6M17 15c2 .6 3.4 2.2 4 5" /></> },
  { name: "chat", words: "chat support conversation feedback", path: p("M4 5h16v11H9l-5 4z") },
  { name: "card", words: "card billing payments money", path: <><rect x="2.5" y="5" width="19" height="14" rx="2.5" /><path d="M2.5 10h19M6 15h4" /></> },
  { name: "cart", words: "cart shop commerce store orders", path: <><circle cx="9.5" cy="20" r="1.4" /><circle cx="18" cy="20" r="1.4" /><path d="M2.5 4h3l2.3 11h11L21 7.5H6.3" /></> },
  { name: "phone", words: "phone mobile ios android app", path: <><rect x="6.5" y="2.5" width="11" height="19" rx="2.5" /><path d="M10.5 18.5h3" /></> },
  { name: "camera", words: "camera media video images", path: <><rect x="3" y="7" width="13" height="11" rx="2" /><path d="M16 11l5-3v9l-5-3z" /></> },
  { name: "music", words: "music audio sound", path: <><circle cx="7" cy="18" r="2.6" /><circle cx="18" cy="16" r="2.6" /><path d="M9.6 18V6l11-2v12" /></> },
  { name: "heart", words: "heart health wellbeing gym fitness", path: p("M12 20s-7-4.4-7-9.2A4 4 0 0 1 12 8a4 4 0 0 1 7 2.8C19 15.6 12 20 12 20z") },
  { name: "leaf", words: "leaf green sustainability energy", path: <><path d="M20 4C9 4 4 9 4 15a5 5 0 0 0 5 5c6 0 11-5 11-16z" /><path d="M9 15c3-3 6-4.5 9-5" /></> },
  { name: "map", words: "map location geo places", path: <><path d="M9 4L3 6.5v13L9 17l6 2.5 6-2.5v-13L15 6.5z" /><path d="M9 4v13M15 6.5v13" /></> },
];

/** The colours a project icon can be drawn in, by the token each uses. */
export const ICON_COLORS = [
  { name: "slate", token: "var(--fg-dim)" },
  { name: "accent", token: "var(--accent)" },
  { name: "violet", token: "var(--violet)" },
  { name: "cyan", token: "var(--cyan)" },
  { name: "green", token: "var(--green)" },
  { name: "yellow", token: "var(--yellow)" },
  { name: "red", token: "var(--red)" },
];

/** The glyph a project with no choice of its own is drawn with. */
export const DEFAULT_ICON = "cube";
/** The colour a project with no choice of its own is drawn in. */
export const DEFAULT_COLOR = "slate";

export function iconNamed(name?: string | null): ProjectIcon {
  return PROJECT_ICONS.find((i) => i.name === name) ?? PROJECT_ICONS.find((i) => i.name === DEFAULT_ICON)!;
}

export function colorNamed(name?: string | null): string {
  return (ICON_COLORS.find((c) => c.name === name) ?? ICON_COLORS[0]).token;
}

/** A project's icon at `size`, in its colour. */
export function ProjectIconMark({ icon, color, size = 20 }: { icon?: string | null; color?: string | null; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke={colorNamed(color)} strokeWidth="1.7"
      strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" className="project-icon">
      {iconNamed(icon).path}
    </svg>
  );
}
