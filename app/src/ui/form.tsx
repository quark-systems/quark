// Shared form parts. Every field on every screen is a Field around one of the controls below, so
// labels, heights, borders, focus rings and spacing come from one place. Styled with Tailwind on
// the theme tokens (tailwind.css); Select, OptionCards and Disclosure take their behaviour
// (keyboard, focus, screen readers) from Radix.
import React from "react";
import { Collapsible, RadioGroup, Select as RSelect } from "radix-ui";
import { cx } from "./cx";

/** A vertical stack of fields with the standard gap, and the row of buttons that ends it. */
export function Form({ children, className, ...props }: React.FormHTMLAttributes<HTMLFormElement>) {
  return <form {...props} className={cx("flex flex-col gap-6", className)}>{children}</form>;
}

export function FormActions({ children }: { children: React.ReactNode }) {
  return <div className="flex items-center justify-end gap-2 pt-2">{children}</div>;
}

const LABEL = "p-0 font-sans text-control font-medium text-fg";

/** A labelled field: label, the control(s), then an optional hint and error lines.
 *  One control: the label wraps it. Several controls, or option cards: pass `group` for a fieldset. */
export function Field({ label, hint, error, group, children, testid }: {
  label: React.ReactNode; hint?: React.ReactNode; error?: React.ReactNode; group?: boolean;
  children: React.ReactNode; testid?: string;
}) {
  const extra = (
    <>
      {hint && (Array.isArray(hint) ? hint : [hint]).map((h, i) => h && <FieldHint key={i}>{h}</FieldHint>)}
      {error && (Array.isArray(error) ? error : [error]).map((e, i) => e && <FieldError key={i}>{e}</FieldError>)}
    </>
  );
  return group ? (
    <fieldset className="m-0 flex min-w-0 flex-col gap-2 border-0 p-0" data-testid={testid}>
      {/* A legend ignores the fieldset's flex gap, so it carries its own margin. */}
      <legend className={cx(LABEL, "mb-2")}>{label}</legend>
      {children}
      {extra}
    </fieldset>
  ) : (
    <div className="flex min-w-0 flex-col gap-2" data-testid={testid}>
      <label className="flex min-w-0 flex-col gap-2">
        <span className={LABEL}>{label}</span>
        {children}
      </label>
      {extra}
    </div>
  );
}

export function FieldHint({ children }: { children: React.ReactNode }) {
  return <div className="font-sans text-s text-faint [&_.mono]:font-mono [&_.mono]:text-dim">{children}</div>;
}

export function FieldError({ children }: { children: React.ReactNode }) {
  return <div className="font-sans text-s text-red">{children}</div>;
}

type ControlProps = { invalid?: boolean; mono?: boolean };

/** The box every text control and select trigger shares. */
export function controlClass({ invalid, mono }: ControlProps = {}, extra?: string) {
  return cx(
    "box-border h-control w-full min-w-0 rounded-m border border-line-2 bg-surface-1 px-3 text-fg outline-none",
    "transition-[border-color,box-shadow] placeholder:text-faint enabled:hover:border-fg/20",
    "focus:border-accent focus:ring-3 focus:ring-accent-soft disabled:cursor-default disabled:opacity-55",
    "data-[state=open]:border-accent data-[state=open]:ring-3 data-[state=open]:ring-accent-soft",
    mono ? "font-mono text-s" : "font-sans text-control",
    invalid && "border-red focus:border-red focus:ring-red/20",
    extra,
  );
}

/** A one-line text box. `mono` for paths, URLs and ids; `invalid` marks it red. */
export function TextInput({ invalid, mono, className, ...props }: React.InputHTMLAttributes<HTMLInputElement> & ControlProps) {
  return <input {...props} aria-invalid={invalid || undefined} className={controlClass({ invalid, mono }, className)} />;
}

/** A multi-line text box that grows by dragging its bottom edge only. */
export function TextArea({ invalid, mono, className, ...props }: React.TextareaHTMLAttributes<HTMLTextAreaElement> & ControlProps) {
  return <textarea {...props} aria-invalid={invalid || undefined}
    className={controlClass({ invalid, mono }, cx("h-auto min-h-16 resize-y py-2 leading-normal", className))} />;
}

/** Radix reserves "" for "no value", so an option with value "" travels under this one. */
const EMPTY = "\u0000empty";

interface Choice { value: string; label: React.ReactNode; disabled?: boolean }

/** The `<option>` children of a Select, read as choices. */
function choicesOf(children: React.ReactNode): Choice[] {
  const out: Choice[] = [];
  React.Children.forEach(children, (c) => {
    if (!React.isValidElement(c)) return;
    const p = c.props as { value?: string; children?: React.ReactNode; disabled?: boolean };
    if (c.type === React.Fragment) out.push(...choicesOf(p.children));
    else if (c.type === "option") out.push({ value: p.value ?? String(p.children ?? ""), label: p.children, disabled: p.disabled });
  });
  return out;
}

/** A dropdown drawn like the text box beside it. Children are plain `<option>` elements. */
export function Select({ value, onValueChange, children, disabled, invalid, className, ...aria }: {
  value: string; onValueChange: (v: string) => void; children: React.ReactNode; disabled?: boolean; invalid?: boolean;
  className?: string; "aria-label"?: string;
}) {
  const choices = choicesOf(children);
  const enc = (v: string) => (v === "" ? EMPTY : v);
  return (
    <RSelect.Root value={enc(value)} onValueChange={(v) => onValueChange(v === EMPTY ? "" : v)} disabled={disabled}>
      <RSelect.Trigger {...aria} aria-invalid={invalid || undefined}
        className={controlClass({ invalid }, cx("flex cursor-pointer items-center justify-between gap-2 text-left", className))}>
        <span className="min-w-0 truncate"><RSelect.Value /></span>
        <RSelect.Icon className="shrink-0 text-dim"><Chevron /></RSelect.Icon>
      </RSelect.Trigger>
      <RSelect.Portal>
        <RSelect.Content position="popper" sideOffset={4}
          className="z-50 max-h-(--radix-select-content-available-height) min-w-(--radix-select-trigger-width) overflow-hidden rounded-m bg-popover text-fg shadow-popover">
          <RSelect.Viewport className="p-1">
            {choices.map((c) => (
              <RSelect.Item key={c.value} value={enc(c.value)} disabled={c.disabled}
                className="flex cursor-pointer select-none items-center gap-2 rounded-s px-3 py-1.5 font-sans text-control text-fg outline-none data-[disabled]:cursor-default data-[disabled]:text-faint data-[highlighted]:bg-selection-strong">
                <RSelect.ItemText>{c.label}</RSelect.ItemText>
                <RSelect.ItemIndicator className="ml-auto text-accent"><Check /></RSelect.ItemIndicator>
              </RSelect.Item>
            ))}
          </RSelect.Viewport>
        </RSelect.Content>
      </RSelect.Portal>
    </RSelect.Root>
  );
}

function Chevron() {
  return <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M2 3.5 5 6.5 8 3.5" fill="none" stroke="currentColor" strokeWidth="1.5" /></svg>;
}
function Check() {
  return <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><path d="M2.5 6.5 5 9l4.5-6" fill="none" stroke="currentColor" strokeWidth="1.6" /></svg>;
}

/** Controls side by side that share a field, e.g. harness, model and effort. Each takes an equal share. */
export function ControlRow({ children }: { children: React.ReactNode }) {
  return <div className="grid auto-cols-[minmax(0,1fr)] grid-flow-col gap-2">{children}</div>;
}

export interface OptionCardChoice<T extends string> { value: T; label: React.ReactNode; description?: React.ReactNode }

/** Pick one of a few choices that each need a sentence of explanation. Fewer than five; else use Select. */
export function OptionCards<T extends string>({ name, value, options, onChange }: {
  name: string; value: T; options: OptionCardChoice<T>[]; onChange: (v: T) => void;
}) {
  return (
    <RadioGroup.Root name={name} value={value} onValueChange={(v) => onChange(v as T)}
      className="grid grid-cols-[repeat(auto-fit,minmax(220px,1fr))] gap-2">
      {options.map((o) => (
        <RadioGroup.Item key={o.value} value={o.value}
          className="group flex cursor-pointer items-start gap-3 rounded-m border border-line-2 bg-surface-1 px-4 py-3 text-left transition-colors hover:border-fg/20 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent data-[state=checked]:border-accent data-[state=checked]:bg-accent/8">
          <span className="mt-0.5 flex size-3.5 shrink-0 items-center justify-center rounded-full border border-line-2 bg-bg group-data-[state=checked]:border-accent">
            <RadioGroup.Indicator className="size-1.5 rounded-full bg-accent" />
          </span>
          <span className="flex min-w-0 flex-col gap-1">
            <span className="font-sans text-control font-medium text-fg">{o.label}</span>
            {o.description && <span className="font-sans text-s text-dim">{o.description}</span>}
          </span>
        </RadioGroup.Item>
      ))}
    </RadioGroup.Root>
  );
}

/** Fields most people leave alone, folded under a summary line. */
export function Disclosure({ summary, children, defaultOpen }: { summary: React.ReactNode; children: React.ReactNode; defaultOpen?: boolean }) {
  return (
    <Collapsible.Root defaultOpen={defaultOpen}>
      <Collapsible.Trigger className="group inline-flex cursor-pointer items-center gap-2 rounded-s border-0 bg-transparent p-0 font-sans text-control font-medium text-dim hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent">
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true" className="-rotate-90 transition-transform group-data-[state=open]:rotate-0">
          <path d="M2 3.5 5 6.5 8 3.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
        </svg>
        {summary}
      </Collapsible.Trigger>
      <Collapsible.Content className="flex flex-col gap-6 pt-6">{children}</Collapsible.Content>
    </Collapsible.Root>
  );
}

/** A form-level message after submitting, e.g. the daemon refused the request. */
export function FormError({ children }: { children: React.ReactNode }) {
  return <div role="alert" className="rounded-m border border-red/40 bg-red/8 px-3 py-2 font-sans text-s text-danger-fg">{children}</div>;
}
