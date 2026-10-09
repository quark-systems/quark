// The component catalogue: one entry per shared part in `src/ui`, with when to use it, its
// contract and a live example. The same entries render the catalogue page (`#/catalogue`).
import React, { useState } from "react";
import type { Decision } from "../api";
import { Button, ButtonLink, ControlRow, CoordinatorMark, Disclosure, Field, FieldError, FieldHint, FolderPicker, Form, FormActions, FormError, OptionCards, Select, TextArea, TextInput, CountBadge, DecisionCard, DecisionEvidence, Kbd, Row, SectionLabel, StatusDot, Tone } from ".";

export interface CatalogueEntry {
  /** The export's name in `src/ui`. */
  name: string;
  when: string;
  contract: string;
  example: () => React.ReactNode;
}

// A sample decision for the cards below; answering it from here reaches whatever daemon is connected.
const SAMPLE: Decision = {
  id: "catalogue-sample", number: 14, project_id: "catalogue", task_id: null, question: "Switch slice 2 (dispatch) to the native engine?",
  state: "open", opened_at: new Date(Date.now() - 4 * 60_000).toISOString(),
  brief: {
    context: "Slice 2 has run in shadow for 7 days and agreed with firstmate on all 212 dispatches. Firstmate keeps running beside it for a week.",
    options: [
      { label: "Switch now", consequence: "Merges #95. Slices 3 and 4 can start their shadow window today." },
      { label: "Wait a week", consequence: "Shadow keeps comparing. Slices 3 and 4 stay blocked." },
    ],
    recommended: "Switch now", recommended_why: "No disagreements in 212 dispatches, and switching back is one setting.",
    asked_by: "coordinator", blocks: ["https://github.com/quark-systems/quark/pull/95"],
    evidence: [{ label: "Shadow comparison, last 7 days", url: null }],
  },
  answer: null, answered_by: null, answered_at: null, answered_via: null, answer_why: null,
  outcome: null, acted_at: null, rule_id: null, made_rule_id: null,
};

function FolderPickerExample() {
  const [path, setPath] = useState("");
  return <FolderPicker value={path} onChange={setPath} title="Choose a folder" label="Folder" />;
}

function SelectExample() {
  const [v, setV] = useState("high");
  return <Select value={v} onValueChange={setV} aria-label="Effort"><option value="">Default effort</option><option value="low">low</option><option value="high">high</option></Select>;
}

function OptionCardsExample() {
  const [v, setV] = useState<"single" | "light">("single");
  return <OptionCards name="cat-preset" value={v} onChange={setV} options={[
    { value: "single", label: "Same for every worker", description: "Every worker runs the agent above." },
    { value: "light", label: "Low effort for small edits", description: "Renames and typo fixes run at low effort." },
  ]} />;
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
    contract: "kind: primary | secondary | quiet | danger (undoes or removes something); every other prop is a button's. Say what it does: 'Switch now', not 'OK'. buttonClass(kind) gives the same look to other elements.",
    example: () => <div className="cat-inline"><Button kind="primary">Switch now</Button><Button>Wait a week</Button><Button kind="quiet">Open with evidence</Button><Button kind="danger">Revoke the rule</Button></div>,
  },
  {
    name: "ButtonLink",
    when: "An action that navigates, such as Cancel back to a list. Looks exactly like Button.",
    contract: "kind as Button's; every other prop is an anchor's (href).",
    example: () => <div className="cat-inline"><ButtonLink href="#/catalogue">Cancel</ButtonLink></div>,
  },
  {
    name: "Form",
    when: "Any form on a screen. Stacks its fields with the standard gap; screens set only width and padding.",
    contract: "Every prop is a form's. Children are Fields, a Disclosure, a FormError and FormActions last.",
    example: () => (
      <Form onSubmit={(e) => e.preventDefault()}>
        <Field label="Name"><TextInput placeholder="Parser rewrite" /></Field>
        <FormActions><Button kind="quiet">Cancel</Button><Button kind="primary" type="submit">Create</Button></FormActions>
      </Form>
    ),
  },
  {
    name: "FormActions",
    when: "The row of buttons that ends a Form, right-aligned, primary last.",
    contract: "children are Buttons.",
    example: () => <FormActions><Button>Save draft</Button><Button kind="primary">Create project</Button></FormActions>,
  },
  {
    name: "FormError",
    when: "After submitting, when the whole request failed (the daemon refused it). Field problems go on the Field.",
    contract: "children is the message; it is announced as an alert.",
    example: () => <FormError>The daemon refused the project: a project with this name exists.</FormError>,
  },
  {
    name: "Field",
    when: "Every input on every form: a label, the control, then hints and errors. Never hand-roll a label.",
    contract: "label; hint and error take one node or a list (falsy entries are skipped); group renders a fieldset for several controls or OptionCards, else the label wraps the one control.",
    example: () => (
      <div style={{ display: "grid", gap: 24 }}>
        <Field label="Goal" hint="The coordinator plans against it."><TextArea rows={2} placeholder="What this Project should achieve." /></Field>
        <Field label="Repository" error="“not a repo” is not owner/name, a clone URL or a local path."><TextInput mono invalid defaultValue="not a repo" /></Field>
      </div>
    ),
  },
  {
    name: "FieldHint",
    when: "A line of help under a control when Field's hint prop does not fit, e.g. inside a custom group.",
    contract: "children is the text; wrap commands in span.mono.",
    example: () => <FieldHint>Pi is not installed: <span className="mono">npm install -g pi</span></FieldHint>,
  },
  {
    name: "FieldError",
    when: "A problem with one field, said as what is wrong and what fits.",
    contract: "children is the message.",
    example: () => <FieldError>Codex is not signed in.</FieldError>,
  },
  {
    name: "TextInput",
    when: "One line of text. Use mono for paths, URLs, branch names and ids.",
    contract: "Every prop is an input's; mono; invalid marks it red (and sets aria-invalid).",
    example: () => <div style={{ display: "grid", gap: 8 }}><TextInput placeholder="Parser rewrite" /><TextInput mono placeholder="~/work/parser" /></div>,
  },
  {
    name: "TextArea",
    when: "A sentence or more of text: a goal, a note, an answer.",
    contract: "Every prop is a textarea's; mono; invalid. Resizes vertically only.",
    example: () => <TextArea rows={3} placeholder="What this Project should achieve, in a sentence or two." />,
  },
  {
    name: "Select",
    when: "Pick one of a list, or of more than four choices. Same height and border as TextInput.",
    contract: "value and onValueChange (an option may have value \"\"); children are plain <option>s; disabled; invalid; aria-label when the Field's label is not enough. A Radix Select underneath: its list is drawn in our colours and keyboard and screen readers work as a native one.",
    example: () => <SelectExample />,
  },
  {
    name: "ControlRow",
    when: "Several controls that make one setting, side by side: harness, model, effort.",
    contract: "children share the row equally; put it in a Field with group.",
    example: () => (
      <ControlRow>
        <Select value="claude" onValueChange={() => {}} aria-label="Harness"><option value="claude">Claude Code 2.1.0</option></Select>
        <TextInput placeholder="Model (optional)" aria-label="Model" />
        <SelectExample />
      </ControlRow>
    ),
  },
  {
    name: "OptionCards",
    when: "Pick one of two to four choices that each need a sentence to explain. More than four: Select.",
    contract: "name, value, onChange, options of { value, label, description }. Radio buttons underneath, so arrow keys move between them.",
    example: () => <OptionCardsExample />,
  },
  {
    name: "FolderPicker",
    when: "Any folder on disk: a workspace, a local repository. Never a plain text box for a path.",
    contract: "value, onChange, title (the dialog's), label. Opens the system folder dialog in the desktop app when the daemon runs on this machine; otherwise falls back to typing the path, and says why.",
    example: () => <FolderPickerExample />,
  },
  {
    name: "Disclosure",
    when: "Fields most people leave alone, folded under a summary such as Advanced.",
    contract: "summary; children are Fields; defaultOpen.",
    example: () => (
      <Disclosure summary="Advanced" defaultOpen>
        <Field label="Workspace path" hint="Leave empty to create a new workspace."><TextInput mono placeholder="~/work/parser" /></Field>
      </Disclosure>
    ),
  },
  {
    name: "CoordinatorMark",
    when: "Stand for a project's coordinator wherever workers have a status dot.",
    contract: "size in px (22 by default).",
    example: () => <CoordinatorMark />,
  },
  {
    name: "DecisionCard",
    when: "Show a decision wherever it comes up: inline in a conversation, full in the Decisions tab, compact on a phone.",
    contract: "d is the decision; size inline (one tap answers with an option) | full (options with consequences, why, standing rule; the log entry once answered) | phone (first two options). onAnswered gets the answered decision, already in the store.",
    example: () => (
      <div style={{ display: "grid", gap: 16, minWidth: 0 }}>
        <DecisionCard d={SAMPLE} size="inline" />
        <div style={{ maxWidth: 360 }}><DecisionCard d={SAMPLE} size="phone" /></div>
        <DecisionCard d={SAMPLE} size="full" />
      </div>
    ),
  },
  {
    name: "DecisionEvidence",
    when: "Beside a full decision card: the asker's evidence, what waits on the answer, and earlier calls like it.",
    contract: "d is the decision; links open in a new window, workers and decisions in the app.",
    example: () => <div style={{ maxWidth: 340 }}><DecisionEvidence d={SAMPLE} /></div>,
  },
];
