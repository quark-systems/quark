// A button that asks a project's coordinator to catch up, write a status report or wrap up.
// Every way of asking (button, slash command, palette) goes through `useRunCommand`, which
// shows the request in the conversation at once, as a command card, until the coordinator's
// session records it.
import { useState, useSyncExternalStore } from "react";
import { api, CoordinatorCommand } from "../api";
import { commandLabel, HINTS } from "../commands";
import { useLabels } from "../persona";
import { getState } from "../store";
import { errText } from "../util";
import { Button, ButtonKind } from "../ui";
import { lastId, PendingCommand } from "./transcript";

// Commands asked for and not yet in the conversation, per project.
const pendingByProject = new Map<string, PendingCommand[]>();
const listeners = new Set<() => void>();
const NONE: PendingCommand[] = [];
let nextKey = 1;

function setPending(cid: string, f: (p: PendingCommand[]) => PendingCommand[]) {
  pendingByProject.set(cid, f(pendingByProject.get(cid) ?? NONE));
  listeners.forEach((l) => l());
}

/** The project's commands on their way to the coordinator. */
export function usePendingCommands(cid: string): [PendingCommand[], (left: PendingCommand[]) => void] {
  const pending = useSyncExternalStore(
    (l) => { listeners.add(l); return () => listeners.delete(l); },
    () => pendingByProject.get(cid) ?? NONE,
  );
  return [pending, (left) => setPending(cid, () => left)];
}

/** Asks for `command`, showing it in the conversation right away. Resolves to whether the coordinator took it. */
export async function runCommand(cid: string, command: CoordinatorCommand): Promise<boolean> {
  const key = nextKey++;
  const after = lastId(getState().chat[cid] ?? []);
  setPending(cid, (p) => [...p, { key, command, sending: true, confirmed: false, after }]);
  try {
    const r = await api.runCommand(cid, command);
    // A catch up with nothing to recap from runs as a status report.
    setPending(cid, (p) => p.map((x) => x.key === key ? { ...x, command: r?.command ?? command, sending: false, confirmed: r?.confirmed ?? true } : x));
    return true;
  } catch (e) {
    setPending(cid, (p) => p.filter((x) => x.key !== key));
    throw e;
  }
}

/** Runs commands for one project, with its busy state and last error. */
export function useRunCommand(cid: string) {
  const [busy, setBusy] = useState<CoordinatorCommand | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const run = async (command: CoordinatorCommand): Promise<boolean> => {
    if (busy) return false;
    setBusy(command); setErr(null);
    try { return await runCommand(cid, command); }
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
