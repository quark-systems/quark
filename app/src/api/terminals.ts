// Engine terminals: read, snapshot, resize and ordered input.

import { DaemonEvent, enc, req, utf8ToB64 } from "./client";
import { AgentRole } from "./harnesses";

export interface Terminal {
  id: string; project_id: string; role: AgentRole; task_id?: string | null; title: string; cols: number; rows: number;
}
export interface TerminalOutput {
  terminal_id: string; role: AgentRole; task_id?: string | null; kind: "output" | "snapshot";
  data_b64: string; cols?: number | null; rows?: number | null;
}

export const terminalsApi = {
  terminal: (id: string) => req<Terminal>("GET", `/v1/terminals/${enc(id)}`),
  /** Appends a snapshot `worker.output` event for the terminal and returns it. */
  terminalSnapshot: (id: string) => req<DaemonEvent<TerminalOutput>>("POST", `/v1/terminals/${enc(id)}/snapshot`),
  terminalResize: (id: string, cols: number, rows: number) =>
    req<Terminal>("POST", `/v1/terminals/${enc(id)}/resize`, { cols, rows }),
  /** Ordered input: one request in flight per terminal; keys typed meanwhile are coalesced. */
  terminalInput: (id: string, data: string) => enqueueInput(id, data),
};

// Concurrent fetches can reach the daemon out of order (POC: "echo" arrived as "ecoh"
// when typing fast), so input is serialized per task and coalesced while a POST is in flight.
interface InputQueue { buf: string; busy: boolean; waiters: ((e?: unknown) => void)[] }
const inputQ = new Map<string, InputQueue>();
function enqueueInput(id: string, data: string): Promise<void> {
  let q = inputQ.get(id);
  if (!q) inputQ.set(id, (q = { buf: "", busy: false, waiters: [] }));
  q.buf += data;
  const done = new Promise<void>((resolve, reject) => q!.waiters.push((e) => (e ? reject(e) : resolve())));
  if (!q.busy) void pump(id, q);
  return done;
}
async function pump(id: string, q: InputQueue) {
  q.busy = true;
  while (q.buf) {
    const chunk = q.buf, waiters = q.waiters;
    q.buf = ""; q.waiters = [];
    let err: unknown;
    try {
      await req<void>("POST", `/v1/terminals/${enc(id)}/input`, { data_b64: utf8ToB64(chunk) });
    } catch (e) { err = e; }
    waiters.forEach((w) => w(err));
  }
  q.busy = false;
}
