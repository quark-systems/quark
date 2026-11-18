// The app menu: the gear in the left list's head. It holds what belongs to the whole app rather
// than one project: Appearance (System, Light, Dark), whether hidden projects are shown, Updates
// and Setup.
import React from "react";
import { go } from "../nav";
import { Menu } from "../ui";
import { setThemePreference, useThemePreference, type ThemePreference } from "../theme";
import { useUpdateStatus } from "../appUpdate";
import { setShowHidden, useShowHidden } from "./reveal";
import { setGroupByCollection, useGroupByCollection } from "./collections";

const svg = (d: React.ReactNode) => <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{d}</svg>;

export const APPEARANCE_ICONS: Record<ThemePreference, React.ReactNode> = {
  system: svg(<><rect x="3" y="4" width="18" height="12" rx="2" /><path d="M8 20h8M12 16v4" /></>),
  light: svg(<><circle cx="12" cy="12" r="4" /><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" /></>),
  dark: svg(<path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z" />),
};

const GEAR = svg(<><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" /></>);

const STACK = svg(<><rect x="3" y="4" width="18" height="5" rx="1.5" /><rect x="3" y="12" width="18" height="5" rx="1.5" /><path d="M6 20h12" /></>);
const EYE = svg(<><path d="M2 12s3.6-6.5 10-6.5S22 12 22 12s-3.6 6.5-10 6.5S2 12 2 12z" /><circle cx="12" cy="12" r="2.6" /></>);

export function AppMenu() {
  const pref = useThemePreference();
  const showHidden = useShowHidden();
  const byCollection = useGroupByCollection();
  const update = useUpdateStatus();
  const ready = update?.state === "ready" ? update.offer?.version : undefined;
  return (
    <Menu label="App menu" groups={[
      { label: "Appearance", value: pref, onChange: (v) => setThemePreference(v as ThemePreference),
        options: (["system", "light", "dark"] as const).map((t) => ({ value: t, label: t === "system" ? "System" : t === "light" ? "Light" : "Dark", icon: APPEARANCE_ICONS[t] })) },
      { label: "Projects", checkbox: { label: "Group by collection", checked: byCollection, onCheckedChange: setGroupByCollection,
        icon: STACK, testid: "menu-group-by-collection" } },
      { checkbox: { label: "Show hidden projects", checked: showHidden, onCheckedChange: setShowHidden,
        icon: EYE, testid: "menu-show-hidden" } },
      { actions: [
        { label: "Updates", testid: "nav-updates", detail: ready, onSelect: () => go({ name: "updates" }),
          icon: svg(<><path d="M12 4v11M7 10l5 5 5-5" /><path d="M5 20h14" /></>) },
        { label: "Setup", testid: "nav-setup", onSelect: () => go({ name: "setup" }),
          icon: svg(<><path d="M9 12l2 2 4-4" /><circle cx="12" cy="12" r="9" /></>) },
      ] },
    ]} trigger={
      <button type="button" className="ll-new ll-menu" aria-label={ready ? `App menu: update ${ready} ready` : "App menu"} title="Appearance, hidden projects, updates and setup" data-testid="app-menu">
        {GEAR}
        {ready && <span className="ll-menu-dot" data-testid="ll-update-ready" />}
      </button>
    } />
  );
}
