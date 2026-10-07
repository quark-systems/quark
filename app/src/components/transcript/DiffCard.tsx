import type { ToolInfo } from "../../api";

/** The first lines of an edit, as the agent made it. */
export function DiffCard({ tool }: { tool: ToolInfo }) {
  return (
    <div className="tx-diff">
      <div className="tx-diff-head">
        <span className="mono ellipsis">{tool.path ?? tool.title}</span>
        <span className="spacer" />
        {tool.additions !== undefined && <span className="adds">+{tool.additions}</span>}
        {tool.deletions !== undefined && <span className="dels">−{tool.deletions}</span>}
      </div>
      <div className="tx-diff-body">
        {tool.diff!.map((l, i) => (
          <div key={i} className={"tx-diff-line " + l.kind}>
            <span className="sign">{l.kind === "add" ? "+" : l.kind === "del" ? "−" : " "}</span>
            <span className="code">{l.text || " "}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
