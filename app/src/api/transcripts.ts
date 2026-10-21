// Coordinator chat and worker transcripts.

import { enc, req } from "./client";

export type TranscriptRole = "user" | "assistant" | "thinking" | "tool_call" | "tool_result";
export type ToolKind = "read" | "edit" | "write" | "shell" | "search" | "web" | "agent" | "plan" | "other";
export interface ToolDiffLine { kind: "add" | "del" | "context"; text: string }
/** A readable summary of a tool call, from the daemon. */
export interface ToolInfo {
  kind: ToolKind; title: string; path?: string; command?: string; query?: string;
  additions?: number; deletions?: number; diff?: ToolDiffLine[];
}
export interface TranscriptEntry {
  role: TranscriptRole; text: string; tool_name?: string | null; tool_call_id?: string | null;
  is_error: boolean; truncated: boolean; ts?: string | null;
  /** On `tool_call` entries; absent on entries recorded before the daemon summarized tools. */
  tool?: ToolInfo;
}
/** A transcript entry with its id: the `seq` of the event that carried it. */
export interface TranscriptItem extends TranscriptEntry { id: number }
/** What the user asks a coordinator for with one control (`CoordinatorCommand` in quarkd). */
export type CoordinatorCommand = "catch_up" | "status_report" | "wrap_up";
export interface MessageAccepted {
  coordinator_id: string; confirmed: boolean; accepted_at: string;
  /** The command that ran, when the message was one: a catch up with nothing to recap from runs as a status report. */
  command?: CoordinatorCommand;
}

export const transcriptsApi = {
  chat: (cid: string) => req<TranscriptItem[]>("GET", `/v1/coordinators/${enc(cid)}/messages?limit=1000`),
  sendChat: (cid: string, text: string) => req<MessageAccepted | undefined>("POST", `/v1/coordinators/${enc(cid)}/messages`, { text }),
  runCommand: (cid: string, command: CoordinatorCommand) => req<MessageAccepted | undefined>("POST", `/v1/coordinators/${enc(cid)}:run`, { command }),
  transcript: (id: string) => req<TranscriptItem[]>("GET", `/v1/tasks/${enc(id)}/transcript?limit=1000`),
};
