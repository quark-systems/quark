// A worker on the Project board: harness mark, title, latest note, the model it runs and its branch,
// kind, pull request, how much it has changed so far and when it last moved. Card layout follows MonoCode's session cards
// (https://github.com/hardbeat920/monocode, MIT, Copyright (c) 2026 Nick); the data is quarkd's.
import type { Task } from "../../api";
import { href } from "../../nav";
import { ago } from "../../util";
import { HarnessLogo } from "./HarnessLogo";
import { harnessMark } from "./harnessMarks";
import { shortModel } from "./agentLine";
import { useDiffStat } from "./diffStat";
import "./WorkerCard.css";

export function WorkerCard({ task, flash }: { task: Task; flash?: boolean }) {
  const stat = useDiffStat(task);
  const live = task.state === "running";
  return (
    <a className={"card wcard" + (flash ? " flash" : "")} href={href({ name: "task", id: task.id })} data-testid="task-card">
      <div className="wcard-top">
        {task.harness
          ? <HarnessLogo harness={task.harness} size={16} />
          : <span className="harness-logo harness-none" aria-hidden />}
        <div className="title">{task.title}</div>
      </div>
      {task.state_note && <div className="note" title={task.state_note}>{task.state_note}</div>}
      {(task.model || task.branch) && (
        <div className="wcard-agent" data-testid="card-agent">
          {task.model && <span className="wcard-model" title={`Model: ${task.model}`} data-testid="card-model">{shortModel(task.model)}</span>}
          {task.branch && (
            <span className="wcard-branch" title={`Branch: ${task.branch}`} data-testid="card-branch">
              <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true"><path fill="currentColor" d="M5 3.25a.75.75 0 1 1-1.5 0 .75.75 0 0 1 1.5 0Zm0 2.122a2.25 2.25 0 1 0-1.5 0v5.256a2.25 2.25 0 1 0 1.5 0V9.25c0-.69.56-1.25 1.25-1.25h3.5A2.75 2.75 0 0 0 12.5 5.372a2.25 2.25 0 1 0-1.5 0A1.25 1.25 0 0 1 9.75 6.5h-3.5c-.45 0-.874.106-1.25.295V5.372ZM4.25 12a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Zm7.5-8.75a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Z" /></svg>
              <span>{task.branch}</span>
            </span>
          )}
        </div>
      )}
      <div className="meta">
        {live && <span className="wcard-live" title="Working" aria-label="Working" />}
        {task.harness && <span className="wcard-harness">{harnessMark(task.harness).name}</span>}
        {task.kind && <span className="pill">{task.kind}</span>}
        {task.pull_request_url && <span className="pill green">PR</span>}
        {stat && (stat.adds > 0 || stat.dels > 0) && (
          <span className="wcard-diff" title={`${stat.files} ${stat.files === 1 ? "file" : "files"} changed`} data-testid="card-diff">
            <span className="adds">+{stat.adds}</span><span className="dels">−{stat.dels}</span>
          </span>
        )}
        <span className="spacer" />
        <span title={task.updated_at}>{ago(task.updated_at)}</span>
      </div>
    </a>
  );
}
