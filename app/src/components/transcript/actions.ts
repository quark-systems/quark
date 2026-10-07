// Coordinator steps that change the Project rather than read it: starting a
// worker and asking the person a question. The coordinator chat shows these as
// cards that stay visible when the rest of the work folds. Today's coordinator
// runs firstmate's scripts, so they are recognized from the command it ran.
import type { Step } from "./turns";

export type CoordinatorAction =
  | { kind: "spawn"; task: string; role: "worker" | "scout" | "second mate"; relaunch: boolean }
  | { kind: "decision"; task: string; reason?: string };

const SPAWN = /(?:^|[\s;&|/"'(])fm-spawn\.sh\s+([A-Za-z0-9][\w.-]*)([^\n;&|]*)/;
const HOLD = /(?:^|[\s;&|/"'(])fm-captain-hold\.sh\s+hold\s+([A-Za-z0-9][\w.-]*)([^\n;&|]*)/;
const REASON = /--reason(?:=|\s+)(?:"((?:[^"\\]|\\.)*)"|'([^']*)'|(\S+))/;

/** What a coordinator step did to the Project, or null for an ordinary step or one that failed. */
export function coordinatorAction(step: Step): CoordinatorAction | null {
  if (step.status === "error" || !step.call) return null;
  const tool = step.call.tool;
  if (tool && tool.kind !== "shell") return null;
  const command = tool?.command ?? commandFromInput(step.call.text);
  if (!command) return null;

  const spawn = SPAWN.exec(command);
  if (spawn) {
    const args = spawn[2];
    const role = /\s--scout\b/.test(args) ? "scout" : /\s--secondmate\b/.test(args) ? "second mate" : "worker";
    return { kind: "spawn", task: spawn[1], role, relaunch: /\s--relaunch\b/.test(args) };
  }
  const hold = HOLD.exec(command);
  if (hold) {
    const r = REASON.exec(hold[2]);
    const reason = r ? (r[1]?.replace(/\\(.)/g, "$1") ?? r[2] ?? r[3]) : undefined;
    return { kind: "decision", task: hold[1], reason: reason?.trim() || undefined };
  }
  return null;
}

/** The command of a shell call recorded before the daemon summarized tools: raw text or `{"command": ...}`. */
function commandFromInput(text: string): string | null {
  const t = text.trim();
  if (!t.startsWith("{")) return t || null;
  try {
    const v = JSON.parse(t) as { command?: unknown; cmd?: unknown };
    const c = v.command ?? v.cmd;
    return typeof c === "string" ? c : Array.isArray(c) ? c.join(" ") : null;
  } catch {
    return null;
  }
}

/** "Started worker", "Relaunched scout", "Asked you to decide": the card's heading, before the task name. */
export function actionTitle(a: CoordinatorAction): string {
  if (a.kind === "decision") return "Asked you to decide";
  return `${a.relaunch ? "Relaunched" : "Started"} ${a.role}`;
}
