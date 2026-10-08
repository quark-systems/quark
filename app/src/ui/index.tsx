// Shared UI parts for the shell. Every part here has an entry in the component catalogue
// (`catalogue.tsx`, shown at `#/catalogue`); build screens from these before adding new ones.
import React from "react";
import { Tone, TONE_LABEL } from "./tone";
import "./ui.css";

export { taskTone, TONE_LABEL } from "./tone";
export type { Tone } from "./tone";

/** A small round dot for one of the glossary's state words. Busy pulses with a halo. */
export function StatusDot({ tone, label }: { tone: Tone; label?: string }) {
  return <span className={"ui-dot " + tone} role="img" aria-label={label ?? TONE_LABEL[tone]} />;
}

/** A count in a pill; renders nothing for zero. Needs-you counts are violet, others neutral. */
export function CountBadge({ n, tone = "neutral", label }: { n: number; tone?: "needs-you" | "accent" | "neutral"; label?: string }) {
  if (!n) return null;
  return <span className={"ui-count " + tone} aria-label={label}>{n}</span>;
}

/** The small uppercase heading over a group of rows or a section of a page. */
export function SectionLabel({ children, right }: { children: React.ReactNode; right?: React.ReactNode }) {
  return <div className="ui-section-label"><span>{children}</span>{right}</div>;
}

/** A keyboard shortcut, e.g. `<Kbd keys="⌘J" />`. */
export function Kbd({ keys }: { keys: string }) {
  return <kbd className="ui-kbd">{keys}</kbd>;
}

/** A row in a list: a leading mark, a title with an optional second line, and trailing bits. */
export function Row({ href, current, lead, title, sub, trail, indent, dim, testid, onClick }: {
  href?: string; current?: boolean; lead?: React.ReactNode; title: React.ReactNode; sub?: React.ReactNode;
  trail?: React.ReactNode; indent?: boolean; dim?: boolean; testid?: string; onClick?: () => void;
}) {
  const cls = "ui-row" + (current ? " current" : "") + (indent ? " indent" : "") + (dim ? " dim" : "");
  const body = (
    <>
      {lead && <span className="ui-row-lead">{lead}</span>}
      <span className="ui-row-text">
        <span className="ui-row-title">{title}</span>
        {sub && <span className="ui-row-sub">{sub}</span>}
      </span>
      {trail}
    </>
  );
  return href
    ? <a className={cls} href={href} aria-current={current ? "page" : undefined} data-testid={testid} onClick={onClick}>{body}</a>
    : <button type="button" className={cls} data-testid={testid} onClick={onClick}>{body}</button>;
}

/** A button in one of three weights: primary (one per view), secondary, or quiet. */
export function Button({ kind = "secondary", ...props }: React.ButtonHTMLAttributes<HTMLButtonElement> & { kind?: "primary" | "secondary" | "quiet" }) {
  return <button type="button" {...props} className={"ui-btn " + kind + (props.className ? " " + props.className : "")} />;
}

/** The coordinator's round "C" mark, styled apart from workers. */
export function CoordinatorMark({ size = 22 }: { size?: number }) {
  return <span className="ui-coord-mark" aria-hidden="true" style={{ width: size, height: size }}>C</span>;
}
