// The coordinator header's agent and context meter: which harness, model and effort the
// coordinator runs, and how full its context is, like the plan usage popover in Claude.
// The popover says when it compacts, when it last did, and compacts on request. It shows a
// change of agent under way, and links to Settings › Agent to make one.
import { useCallback, useEffect, useState } from "react";
import { Popover } from "radix-ui";
import { api, CoordinatorContext as Context, NotAvailable } from "../api";
import { agentLine, compactionLine, contextLine, fullness, switching, switchLine, tokens } from "../coordinatorContext";
import { href } from "../nav";
import { ago, errText } from "../util";
import { Button, buttonClass, UsageMeter } from "../ui";

/** How often the header reads the context while it is shown. */
const POLL_MS = 15_000;

/** Reads the coordinator's context now, every {@link POLL_MS}, and whenever `changed` does. */
function useCoordinatorContext(cid: string, changed: unknown) {
  const [ctx, setCtx] = useState<Context | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const load = useCallback(() => {
    api.coordinatorContext(cid).then(setCtx).catch((e) => { if (e instanceof NotAvailable) setUnavailable(true); });
  }, [cid]);
  useEffect(() => {
    load();
    const t = setInterval(load, POLL_MS);
    return () => clearInterval(t);
  }, [load]);
  useEffect(() => { load(); }, [load, changed]);
  return { ctx, unavailable, load };
}

export function CoordinatorContext({ cid, changed }: { cid: string; changed?: unknown }) {
  const { ctx, unavailable, load } = useCoordinatorContext(cid, changed);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  if (unavailable || !ctx) return null;
  const agent = agentLine(ctx);
  const used = ctx.context_tokens ?? 0;
  const max = ctx.window_tokens ?? 0;
  const mark = ctx.compact_at_tokens ?? undefined;
  const compact = async () => {
    setBusy(true); setNote(null);
    try {
      await api.compactCoordinator(cid);
      setNote("Compacting. It shows in the conversation when it is done.");
      load();
    } catch (e) { setNote(errText(e)); } finally { setBusy(false); }
  };
  return (
    <span className="coord-context">
      <span className="coord-agent" data-testid="coordinator-agent" title="Harness, model and effort; change them in Settings › Agent">
        {agent.harness} · {agent.model} · {agent.effort}
      </span>
      <Popover.Root onOpenChange={(o) => { if (o) load(); else setNote(null); }}>
        <Popover.Trigger className={buttonClass("quiet", "coord-meter")} data-testid="context-meter"
          aria-label={`Context: ${contextLine(ctx)}`}>
          <span className="coord-meter-bar"><UsageMeter used={used} max={max || used || 1} mark={max ? mark : undefined} label="Context" /></span>
          <span className="mono">{fullness(ctx)}</span>
        </Popover.Trigger>
        <Popover.Portal>
          <Popover.Content align="end" sideOffset={6} data-testid="context-popover"
            className="z-50 flex w-80 flex-col gap-3 rounded-l bg-popover p-4 font-sans text-s text-fg shadow-popover outline-none">
            <div className="flex flex-col gap-1.5">
              <div className="flex items-baseline justify-between gap-3">
                <span className="text-m">Context</span>
                <span className="text-dim" data-testid="context-line">{contextLine(ctx)}{ctx.window_tokens && ctx.context_tokens != null ? ` · ${fullness(ctx)}` : ""}</span>
              </div>
              <UsageMeter used={used} max={max || Math.max(used, mark ?? 0) || 1} mark={mark}
                markLabel={mark ? `Compacts at ${tokens(mark)}` : undefined} label="Context" testid="context-bar" />
              {ctx.measured_at && ctx.context_tokens != null && <span className="text-xs text-faint" title={new Date(ctx.measured_at).toLocaleString()}>Measured {ago(ctx.measured_at)}, when a turn ended</span>}
            </div>
            <div className="flex flex-col gap-1">
              <span data-testid="compaction-line">{compactionLine(ctx)}</span>
              {ctx.last_compaction && (
                <span className="text-dim" data-testid="last-compaction" title={ctx.last_compaction.at ? new Date(ctx.last_compaction.at).toLocaleString() : undefined}>
                  Last compacted {ctx.last_compaction.at ? ago(ctx.last_compaction.at) : ""}: {ctx.last_compaction.text.replace(/^./, (c) => c.toLowerCase())}.
                </span>
              )}
            </div>
            <div className="flex flex-col gap-0.5 border-t border-line pt-3">
              <span className="text-xs text-faint">Agent</span>
              <span>{agent.harness}, {agent.model}, {agent.effort}{ctx.agent_config?.pool ? `, pool ${ctx.agent_config.pool}` : ""}</span>
              {ctx.switch && (switching(ctx.switch) || ctx.switch.state === "failed") &&
                <span className="text-dim" data-testid="context-switch">{switchLine(ctx.switch)}</span>}
              <a className="text-accent" href={href({ name: "settings", project: cid })}>Change agent</a>
            </div>
            <div className="flex items-center gap-2">
              <Button disabled={busy || ctx.compacting !== "idle"} onClick={() => void compact()} data-testid="compact-now"
                title="Compact without a wrap up first. Wrap up before if the conversation holds something unsaved.">
                {busy ? "Asking…" : "Compact now"}
              </Button>
              <a className={buttonClass("quiet")} href={href({ name: "settings", project: cid })}>Compaction settings</a>
            </div>
            {note && <span className="text-dim" role="status">{note}</span>}
          </Popover.Content>
        </Popover.Portal>
      </Popover.Root>
    </span>
  );
}
