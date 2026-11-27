// A card for one of Quark's own objects (a decision, a worker, a pull request) that a message is
// about: what kind it is, its handle, its title and state, and a click through to it. Never the
// object's id or link as text.
import React from "react";
import type { RefKind } from "../objectRefs";
import { REF_KIND_LABEL } from "../objectRefs";
import { cx } from "./cx";
import type { Tone } from "./tone";

// 16px outline glyphs, one per kind of object.
const GLYPH: Record<RefKind, string> = {
  decision: "M8 2a6 6 0 1 1 0 12A6 6 0 0 1 8 2zM6.3 6.3a1.8 1.8 0 1 1 2.4 1.7c-.5.2-.7.6-.7 1.1v.4M8 11.5h.01",
  worker: "M4 5h8v7H4zM8 2v3M6 8h.01M10 8h.01M6.5 10.5h3",
  "pull-request": "M4.5 2.5a1.5 1.5 0 1 1 0 3 1.5 1.5 0 0 1 0-3zM4.5 5.5v5M4.5 10.5a1.5 1.5 0 1 1 0 3 1.5 1.5 0 0 1 0-3zM11.5 10.5a1.5 1.5 0 1 1 0 3 1.5 1.5 0 0 1 0-3zM11.5 10.5V6a2 2 0 0 0-2-2H7.5M9 2.5L7.5 4 9 5.5",
  page: "M6.5 9.5l3-3M7 4.5l1-1a2.5 2.5 0 0 1 3.5 3.5l-1 1M9 11.5l-1 1a2.5 2.5 0 0 1-3.5-3.5l1-1",
};

// Each kind's mark: a tinted square with the glyph in the kind's color.
const MARK: Record<RefKind, string> = {
  decision: "bg-violet/15 text-violet",
  worker: "bg-blue/15 text-blue",
  "pull-request": "bg-green/15 text-green",
  page: "bg-surface-3 text-dim",
};

export function ObjectRef({ kind, tag, title, status, href, className, testid = "object-ref" }: {
  kind: RefKind;
  /** A short handle beside the kind: `D-14`, `#95`. */
  tag?: string;
  /** The object's title or question, as text or with issue titles filled in (BeadText). */
  title: React.ReactNode;
  status?: { label: string; tone: Tone };
  /** Where a click opens it. */
  href: string;
  className?: string;
  testid?: string;
}) {
  const kindLabel = REF_KIND_LABEL[kind];
  return (
    <a
      href={href} data-testid={testid} data-kind={kind}
      className={cx(
        "group flex min-w-0 items-start gap-2.5 rounded-m border border-line-2 bg-bg px-3 py-2 text-left whitespace-normal no-underline transition-colors",
        "hover:border-accent hover:bg-surface-2 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent",
        className,
      )}
    >
      <span className={cx("mt-0.5 inline-flex size-7 flex-none items-center justify-center rounded-s", MARK[kind])} aria-hidden="true">
        <svg viewBox="0 0 16 16" width="16" height="16">
          <path d={GLYPH[kind]} fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex min-w-0 items-center gap-2 text-xs text-dim">
          <span className="font-semibold uppercase tracking-wide">{kindLabel}</span>
          {tag && <span className="font-mono">{tag}</span>}
          {status && (
            <span className="ml-auto inline-flex flex-none items-center gap-1.5 pl-2" data-testid={`${testid}-status`}>
              <span className={"ui-dot " + status.tone} aria-hidden="true" />{status.label}
            </span>
          )}
        </span>
        <span className="line-clamp-2 text-m font-medium text-fg group-hover:text-link">{title}</span>
      </span>
    </a>
  );
}
