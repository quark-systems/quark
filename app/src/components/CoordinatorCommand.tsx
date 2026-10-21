// A button that asks a project's coordinator to catch up, write a status report or wrap up.
// The answer arrives in the coordinator's conversation like any reply.
import { useState } from "react";
import { api, CoordinatorCommand } from "../api";
import { commandLabel, HINTS } from "../commands";
import { useLabels } from "../persona";
import { errText } from "../util";
import { Button, ButtonKind } from "../ui";

/** Runs a command for one project, with its busy state and last error. */
export function useRunCommand(cid: string) {
  const [busy, setBusy] = useState<CoordinatorCommand | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const run = async (command: CoordinatorCommand): Promise<boolean> => {
    if (busy) return false;
    setBusy(command); setErr(null);
    try { await api.runCommand(cid, command); return true; }
    catch (e) { setErr(errText(e)); return false; }
    finally { setBusy(null); }
  };
  return { run, busy, err };
}

export function CommandButton({ cid, command, kind = "quiet", onRan }: {
  cid: string; command: CoordinatorCommand; kind?: ButtonKind;
  /** After the coordinator took the request: e.g. go to its conversation. */
  onRan?: () => void;
}) {
  const l = useLabels(cid);
  const { run, busy, err } = useRunCommand(cid);
  return (
    <span className="command-run">
      <Button kind={kind} disabled={busy !== null} title={HINTS[command]} data-testid={`command-${command}`}
        onClick={async () => { if (await run(command)) onRan?.(); }}>
        {busy ? "Asking…" : commandLabel(l, command)}
      </Button>
      {err && <span className="command-error" role="alert">{err}</span>}
    </span>
  );
}
