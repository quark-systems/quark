// The Project dashboard's tab strip: one link per tab, the current one marked.
import React from "react";
import { href } from "../../nav";

const TABS = [
  { name: "overview", label: "Overview" },
  { name: "metrics", label: "Metrics" },
  { name: "settings", label: "Settings" },
] as const;

export function DashboardTabs({ project, current }: { project: string; current: (typeof TABS)[number]["name"] }) {
  return (
    <nav className="dash-tabs" aria-label="Project dashboard">
      {TABS.map((t) => (
        <a key={t.name} href={href({ name: t.name, project })} className={"dash-tab" + (t.name === current ? " on" : "")}
          aria-current={t.name === current ? "page" : undefined} data-testid={`dash-tab-${t.name}`}>{t.label}</a>
      ))}
    </nav>
  );
}
