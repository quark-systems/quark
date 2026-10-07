// An agent's work in a turn: steps folded into one summary line, with edit
// previews and coordinator actions kept in view. While the agent works, the
// latest step stays visible with a running state. Layout follows MonoCode's
// WorkFoldLine and ActivityToolRow (https://github.com/hardbeat920/monocode, MIT).
import { useState } from "react";
import { href } from "../../nav";
import { actionTitle, coordinatorAction, CoordinatorAction } from "./actions";
import { rise, useTranscript } from "./context";
import { DiffCard } from "./DiffCard";
import { Icon } from "./Icon";
import { Step, stepKind, stepTitle, summarize, TurnPart } from "./turns";

type WorkPart = Extract<TurnPart, { kind: "work" }>;

export function Work({ part, live }: { part: WorkPart; live: boolean }) {
  const [open, setOpen] = useState(false);
  const failed = part.steps.filter((s) => s.status === "error").length;
  if (live) {
    // While the agent works, earlier steps fold behind one line and the latest stays in view.
    const earlier = part.steps.slice(0, -1);
    const latest = part.steps[part.steps.length - 1];
    return (
      <div className="tx-work">
        {earlier.length > 0 && (
          open ? earlier.map((s) => <StepRow key={s.key} step={s} />) : (
            <>
              <button className="tx-fold" onClick={() => setOpen(true)}>+{earlier.length} earlier {earlier.length === 1 ? "step" : "steps"}</button>
              <Previews steps={earlier} />
            </>
          )
        )}
        {latest && <StepRow step={latest} live />}
      </div>
    );
  }
  return (
    <div className="tx-work">
      <button className="tx-fold" aria-expanded={open} onClick={() => setOpen(!open)}>
        <Icon kind={open ? "chevron-down" : "chevron-right"} />
        {summarize(part.steps)}
        {failed > 0 && <span className="bad"> · {failed} failed</span>}
      </button>
      {open ? part.steps.map((s) => <StepRow key={s.key} step={s} />) : <Previews steps={part.steps} />}
    </div>
  );
}

/** What stays visible when steps fold: edit previews and coordinator actions. */
function Previews({ steps }: { steps: Step[] }) {
  const { actions } = useTranscript();
  return (
    <>
      {steps.map((s) => {
        const action = actions ? coordinatorAction(s) : null;
        if (action) return <ActionCard key={s.key} id={s.key} action={action} />;
        return s.call?.tool?.diff?.length ? <DiffCard key={s.key} tool={s.call.tool} /> : null;
      })}
    </>
  );
}

function StepRow({ step, live }: { step: Step; live?: boolean }) {
  const [open, setOpen] = useState(false);
  const { actions, freshAfter } = useTranscript();
  const kind = stepKind(step);
  const tool = step.call?.tool;
  const action = actions ? coordinatorAction(step) : null;
  const detail = step.thinking?.text ?? [tool?.command ?? (tool ? null : step.call?.text), step.result?.text].filter(Boolean).join("\n\n");
  const running = step.status === "running";
  return (
    <div className={"tx-step" + (step.status === "error" ? " error" : "") + rise(step.key, freshAfter)}>
      <button className="tx-step-row" aria-expanded={open} onClick={() => setOpen(!open)} disabled={!detail}>
        <Icon kind={kind} />
        <span className={"tx-step-title" + (live && running ? " shimmer" : "")}>{stepTitle(step)}</span>
        {tool?.additions !== undefined && <span className="adds">+{tool.additions}</span>}
        {tool?.deletions !== undefined && tool.deletions > 0 && <span className="dels">−{tool.deletions}</span>}
        <span className="tx-step-status">{running ? <span className="spinner" aria-label="running" /> : step.status === "error" ? <Icon kind="cross" /> : null}</span>
      </button>
      {action ? <ActionCard id={step.key} action={action} /> : tool?.diff?.length ? <DiffCard tool={tool} /> : null}
      {open && detail && <pre className="tx-detail">{detail}{step.result?.truncated ? "\n…" : ""}</pre>}
    </div>
  );
}

/** A worker the coordinator started, or a question it put to the person, linking to where it is answered. */
function ActionCard({ id, action }: { id: number; action: CoordinatorAction }) {
  const { freshAfter } = useTranscript();
  const decision = action.kind === "decision";
  return (
    <div className={"tx-action tx-action-" + action.kind + rise(id, freshAfter)} data-testid="coordinator-action">
      <Icon kind={decision ? "decision" : "agent"} />
      <div className="tx-action-body">
        <div className="tx-action-head">
          <span>{actionTitle(action)}</span>
          <span className="mono ellipsis">{action.task}</span>
        </div>
        {decision && action.reason && <div className="tx-action-reason">{action.reason}</div>}
        {decision && <a className="tx-action-link" href={href({ name: "inbox" })}>Open inbox</a>}
      </div>
    </div>
  );
}
