// Buttons. The `ui-btn` and kind classes stay on the element as hooks for the few places that
// size a button differently (the phone decision card); the look comes from the utilities.
import React from "react";
import { cx } from "./cx";

export type ButtonKind = "primary" | "secondary" | "quiet" | "danger";

const KIND: Record<ButtonKind, string> = {
  primary: "border border-accent bg-accent text-on-tone font-semibold hover:not-disabled:bg-accent/90 disabled:bg-surface-3 disabled:border-line-2 disabled:text-faint disabled:font-medium",
  secondary: "border border-line-2 bg-surface-2 text-fg hover:not-disabled:bg-selection disabled:opacity-45",
  quiet: "border border-transparent bg-transparent text-dim hover:not-disabled:bg-selection hover:not-disabled:text-fg disabled:opacity-45",
  danger: "border border-red/40 bg-transparent text-danger-fg hover:not-disabled:bg-red/10 disabled:opacity-45",
};

/** The classes for a button of `kind`, for elements that are not a Button (a link, a Radix trigger). */
export function buttonClass(kind: ButtonKind = "secondary", extra?: string) {
  return cx(
    "ui-btn", kind,
    "inline-flex h-control shrink-0 cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-m px-3 font-sans text-control no-underline transition-colors",
    "focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent disabled:cursor-default",
    KIND[kind], extra,
  );
}

/** A button in one of four weights: primary (one per view), secondary, quiet, or danger. */
export function Button({ kind = "secondary", className, ...props }: React.ButtonHTMLAttributes<HTMLButtonElement> & { kind?: ButtonKind }) {
  return <button type="button" {...props} className={buttonClass(kind, className)} />;
}

/** A link styled as a Button, for actions that navigate (Cancel back to a list). */
export function ButtonLink({ kind = "secondary", className, ...props }: React.AnchorHTMLAttributes<HTMLAnchorElement> & { kind?: ButtonKind }) {
  return <a {...props} className={buttonClass(kind, className)} />;
}
