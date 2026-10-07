// A worker on the Project board: harness mark, title, latest note, kind, pull request, how much it
// has changed so far and when it last moved. Card layout follows MonoCode's session cards
// (https://github.com/hardbeat920/monocode, MIT, Copyright (c) 2026 Nick); the data is quarkd's.
import type { Task } from "../../api";
import { href } from "../../nav";
import { ago } from "../../util";
import { HarnessLogo } from "./HarnessLogo";
import { harnessMark } from "./harnessMarks";
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
