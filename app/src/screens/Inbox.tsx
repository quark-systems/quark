// J5: decisions held for a person, across every Project, answered from one place. It is the
// project Decisions screen with no project: the same log, filters and full card.
import { Decisions } from "./Decisions";

export function Inbox({ id }: { id?: string }) {
  return (
    <>
      <div className="header">
        <h1>Decisions</h1>
        <span className="crumb">across all projects</span>
      </div>
      <Decisions id={id} />
    </>
  );
}
