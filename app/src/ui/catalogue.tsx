// The component catalogue: one entry per shared part in `src/ui`, with when to use it, its
// contract and a live example. The same entries render the catalogue page (`#/catalogue`).
import React from "react";
import { Button, CoordinatorMark, CountBadge, Kbd, Row, SectionLabel, StatusDot, Tone } from ".";

export interface CatalogueEntry {
  /** The export's name in `src/ui`. */
  name: string;
  when: string;
  contract: string;
  example: () => React.ReactNode;
}

const TONES: Tone[] = ["busy", "needs-you", "ready", "failed", "parked", "idle"];

export const CATALOGUE: CatalogueEntry[] = [
  {
    name: "StatusDot",
    when: "Mark the state of a worker, project or item with one of the glossary's state words.",
    contract: "tone: busy | needs-you | ready | failed | parked | idle; label overrides the spoken name. Color is never the only cue: pair it with words in the row.",
    example: () => <div className="cat-inline">{TONES.map((t) => <span key={t} className="cat-inline"><StatusDot tone={t} />{t}</span>)}</div>,
  },
  {
    name: "CountBadge",
    when: "Show how many things wait somewhere: decisions on a project, proposals to review.",
    contract: "n (renders nothing at 0); tone needs-you for things waiting on you, accent for news, neutral otherwise.",
    example: () => <div className="cat-inline"><CountBadge n={3} tone="needs-you" /><CountBadge n={2} tone="accent" /><CountBadge n={12} /></div>,
  },
  {
    name: "SectionLabel",
    when: "Head a group of rows in the left list or a section of a page.",
    contract: "children is the label; right holds an optional trailing control.",
    example: () => <SectionLabel right={<CountBadge n={2} />}>Needs you · oldest first</SectionLabel>,
  },
  {
    name: "Kbd",
    when: "Show the shortcut for an action beside it.",
    contract: "keys is the literal text, e.g. ⌘J.",
    example: () => <div className="cat-inline"><Kbd keys="⌘J" /><Kbd keys="⌘K" /><Kbd keys="⌘P" /></div>,
  },
  {
    name: "Row",
    when: "Any selectable line in a list: projects, coordinators and workers in the left list, items in a queue.",
    contract: "href makes a link, else a button with onClick; current marks the open one; lead, title, sub, trail; indent for children; dim for parked.",
    example: () => (
      <div style={{ width: 280 }}>
        <Row lead={<CoordinatorMark />} title="Coordinator" sub="1 decision · 3 workers" current trail={<CountBadge n={1} tone="needs-you" />} />
        <Row indent lead={<StatusDot tone="ready" />} title="attention-model" sub="PR ready · checks green" />
        <Row indent lead={<StatusDot tone="busy" />} title="transcript-search" sub="Running tests" />
        <Row indent dim lead={<StatusDot tone="parked" />} title="usage-ring" sub="Parked until slice 2" />
      </div>
    ),
  },
  {
    name: "Button",
    when: "An action. One primary per view; quiet for the least likely choice.",
    contract: "kind: primary | secondary | quiet; every other prop is a button's. Say what it does: 'Switch now', not 'OK'.",
    example: () => <div className="cat-inline"><Button kind="primary">Switch now</Button><Button>Wait a week</Button><Button kind="quiet">Open with evidence</Button></div>,
  },
  {
    name: "CoordinatorMark",
    when: "Stand for a project's coordinator wherever workers have a status dot.",
    contract: "size in px (22 by default).",
    example: () => <CoordinatorMark />,
  },
];
