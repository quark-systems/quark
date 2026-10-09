// Shared form parts. Every field on every screen is a Field around one of the controls below,
// so labels, heights, borders, focus rings and spacing come from one place (form.css).
import React, { useId } from "react";
import "./form.css";

/** A vertical stack of fields with the standard gap, and the row of buttons that ends it. */
export function Form({ children, className, ...props }: React.FormHTMLAttributes<HTMLFormElement>) {
  return <form {...props} className={"ui-form" + (className ? " " + className : "")}>{children}</form>;
}

export function FormActions({ children }: { children: React.ReactNode }) {
  return <div className="ui-form-actions">{children}</div>;
}

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
    <fieldset className="ui-field" data-testid={testid}>
      <legend className="ui-field-label">{label}</legend>
      {children}
      {extra}
    </fieldset>
  ) : (
    <div className="ui-field" data-testid={testid}>
      <label className="ui-field-wrap">
        <span className="ui-field-label">{label}</span>
        {children}
      </label>
      {extra}
    </div>
  );
}

export function FieldHint({ children }: { children: React.ReactNode }) {
  return <div className="ui-field-hint">{children}</div>;
}

export function FieldError({ children }: { children: React.ReactNode }) {
  return <div className="ui-field-error">{children}</div>;
}

type ControlProps = { invalid?: boolean; mono?: boolean };
const controlClass = (base: string, { invalid, mono }: ControlProps, extra?: string) =>
  base + (invalid ? " invalid" : "") + (mono ? " mono" : "") + (extra ? " " + extra : "");

/** A one-line text box. `mono` for paths, URLs and ids; `invalid` marks it red. */
export function TextInput({ invalid, mono, className, ...props }: React.InputHTMLAttributes<HTMLInputElement> & ControlProps) {
  return <input {...props} aria-invalid={invalid || undefined} className={controlClass("ui-input", { invalid, mono }, className)} />;
}

/** A multi-line text box that grows by dragging its bottom edge only. */
export function TextArea({ invalid, mono, className, ...props }: React.TextareaHTMLAttributes<HTMLTextAreaElement> & ControlProps) {
  return <textarea {...props} aria-invalid={invalid || undefined} className={controlClass("ui-input ui-textarea", { invalid, mono }, className)} />;
}

/** A dropdown drawn like the text box beside it, with its own chevron. */
export function Select({ invalid, className, children, ...props }: React.SelectHTMLAttributes<HTMLSelectElement> & { invalid?: boolean }) {
  return (
    <span className={"ui-select" + (className ? " " + className : "")}>
      <select {...props} aria-invalid={invalid || undefined} className={controlClass("ui-input", { invalid })}>{children}</select>
    </span>
  );
}

/** Controls side by side that share a field, e.g. harness, model and effort. Each takes an equal share. */
export function ControlRow({ children }: { children: React.ReactNode }) {
  return <div className="ui-control-row">{children}</div>;
}

export interface OptionCardChoice<T extends string> { value: T; label: React.ReactNode; description?: React.ReactNode }

/** Pick one of a few choices that each need a sentence of explanation. Fewer than five; else use Select. */
export function OptionCards<T extends string>({ name, value, options, onChange }: {
  name: string; value: T; options: OptionCardChoice<T>[]; onChange: (v: T) => void;
}) {
  return (
    <div className="ui-option-cards" role="radiogroup">
      {options.map((o) => (
        <label key={o.value} className={"ui-option-card" + (value === o.value ? " on" : "")}>
          <input type="radio" name={name} value={o.value} checked={value === o.value} onChange={() => onChange(o.value)} />
          <span className="ui-option-card-text">
            <span className="ui-option-card-label">{o.label}</span>
            {o.description && <span className="ui-option-card-desc">{o.description}</span>}
          </span>
        </label>
      ))}
    </div>
  );
}

/** Fields most people leave alone, folded under a summary line. */
export function Disclosure({ summary, children, defaultOpen }: { summary: React.ReactNode; children: React.ReactNode; defaultOpen?: boolean }) {
  const id = useId();
  return (
    <details className="ui-disclosure" open={defaultOpen}>
      <summary aria-controls={id}>{summary}</summary>
      <div className="ui-disclosure-body" id={id}>{children}</div>
    </details>
  );
}

/** A form-level message after submitting, e.g. the daemon refused the request. */
export function FormError({ children }: { children: React.ReactNode }) {
  return <div className="ui-form-error" role="alert">{children}</div>;
}
