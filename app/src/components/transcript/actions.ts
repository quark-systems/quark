// Coordinator steps that change the Project rather than read it: starting a
// worker and asking the person a question. The coordinator chat shows these as
// cards that stay visible when the rest of the work folds. firstmate's
// coordinator runs scripts, so its steps are recognized from the command it
// ran; the native coordinator's questions are its `ask_user` tool calls.
import type { DecisionOption } from "../../api";
import type { Step } from "./turns";

/** A question the native coordinator filed with `ask_user`, as it asked it. */
export interface AskAction {
  kind: "ask";
  key?: string;
  question: string;
  context?: string;
  options: DecisionOption[];
  recommended?: string;
  /** The decision's number (`D-<n>`), from the tool's reply. */
  number?: number;
}

export type CoordinatorAction =
  | { kind: "spawn"; task: string; role: "worker" | "scout" | "second mate"; relaunch: boolean }
  | { kind: "decision"; task: string; reason?: string }
  | AskAction;

const SPAWN = /(?:^|[\s;&|/"'(])fm-spawn\.sh\s+([A-Za-z0-9][\w.-]*)([^\n;&|]*)/;
const HOLD = /(?:^|[\s;&|/"'(])fm-captain-hold\.sh\s+hold\s+([A-Za-z0-9][\w.-]*)([^\n;&|]*)/;
const REASON = /--reason(?:=|\s+)(?:"((?:[^"\\]|\\.)*)"|'([^']*)'|(\S+))/;

/** What a coordinator step did to the Project, or null for an ordinary step or one that failed. */
export function coordinatorAction(step: Step): CoordinatorAction | null {
  if (step.status === "error" || !step.call) return null;
  if (isAskUser(step.call.tool_name)) return askAction(step);
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

/** `ask_user` on the coordinator's MCP server (`mcp__quark-coordinator__ask_user` in Claude Code). */
function isAskUser(name: string | null | undefined): boolean {
  return !!name && (name === "ask_user" || /^mcp__quark-coordinator__ask_user$/.test(name));
}

function askAction(step: Step): AskAction | null {
  let input: Record<string, unknown>;
  try {
    const v: unknown = JSON.parse(step.call!.text);
    if (!v || typeof v !== "object") return null;
    input = v as Record<string, unknown>;
  } catch {
    return null;
  }
  const text = (k: string) => (typeof input[k] === "string" && (input[k] as string).trim() ? (input[k] as string).trim() : undefined);
  const question = text("question");
  if (!question) return null;
  const options = (Array.isArray(input.options) ? input.options : []).flatMap((o): DecisionOption[] => {
    if (typeof o === "string") return o.trim() ? [{ label: o.trim() }] : [];
    const { label, consequence } = (o ?? {}) as { label?: unknown; consequence?: unknown };
    return typeof label === "string" && label.trim()
      ? [{ label: label.trim(), consequence: typeof consequence === "string" ? consequence : undefined }] : [];
  });
  // "Asked the user (decision 12). ..." in coordinator_hands.rs.
  const n = /\(decision (\d+)\)/.exec(step.result?.text ?? "");
  return {
    kind: "ask", key: text("key"), question, context: text("context"), options,
    recommended: text("recommended"), number: n ? Number(n[1]) : undefined,
  };
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
  if (a.kind === "decision" || a.kind === "ask") return "Asked you to decide";
  return `${a.relaunch ? "Relaunched" : "Started"} ${a.role}`;
}
