// Coordinator chat and worker transcripts.

import { enc, req } from "./client";
import type { AgentConfig } from "./projects";
import type { CompactionSettings } from "./settings";

/** `compaction`: the harness compacted the conversation here; `text` says who and at what size. */
export type TranscriptRole = "user" | "assistant" | "thinking" | "tool_call" | "tool_result" | "compaction";
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

/** Where a compaction Quark started is: a wrap up first, then the harness's `/compact`. */
export type CompactionPhase = "idle" | "wrapping_up" | "compacting";
/** A coordinator's agent and how full its context is (`GET /v1/coordinators/{id}/context`). */
export interface CoordinatorContext {
  coordinator_id: string;
  agent_config?: AgentConfig | null;
  /** The model its session log names, when it differs from or fills in the configured one. */
  model_seen?: string | null;
  /** Tokens in context when its last turn ended; absent before the first turn and right after a compaction. */
  context_tokens?: number | null;
  /** The model's context window, when the daemon knows it. */
  window_tokens?: number | null;
  measured_at?: string | null;
  compaction: CompactionSettings;
  /** The context size it compacts at (`at_percent` of the window); absent when automatic compaction is off. */
  compact_at_tokens?: number | null;
  compacting: CompactionPhase;
  last_compaction?: { at?: string | null; text: string } | null;
  /** Its latest change of agent, under way or finished. */
  switch?: CoordinatorSwitch | null;
}

/** How a change of agent carries the conversation over: the same session, or a handoff brief. */
export type SwitchKind = "resume" | "handoff";
export type SwitchState = "requested" | "wrapping_up" | "switching" | "starting" | "briefing" | "done" | "failed";

/** A change of the coordinator's harness, model, effort or pool (`PUT /v1/coordinators/{id}/agent`). */
export interface CoordinatorSwitch {
  id: string;
  project_id: string;
  from?: AgentConfig | null;
  to: AgentConfig;
  kind: SwitchKind;
  state: SwitchState;
  brief?: string | null;
  error?: string | null;
  requested_at: string;
  updated_at: string;
}

export const transcriptsApi = {
  chat: (cid: string) => req<TranscriptItem[]>("GET", `/v1/coordinators/${enc(cid)}/messages?limit=1000`),
  sendChat: (cid: string, text: string) => req<MessageAccepted | undefined>("POST", `/v1/coordinators/${enc(cid)}/messages`, { text }),
  runCommand: (cid: string, command: CoordinatorCommand) => req<MessageAccepted | undefined>("POST", `/v1/coordinators/${enc(cid)}:run`, { command }),
  coordinatorContext: (cid: string) => req<CoordinatorContext>("GET", `/v1/coordinators/${enc(cid)}/context`),
  compactCoordinator: (cid: string) => req<MessageAccepted | undefined>("POST", `/v1/coordinators/${enc(cid)}:compact`),
  interruptCoordinator: (cid: string) => req<MessageAccepted | undefined>("POST", `/v1/coordinators/${enc(cid)}:interrupt`),
  switchCoordinatorAgent: (cid: string, agent: AgentConfig) => req<CoordinatorSwitch | undefined>("PUT", `/v1/coordinators/${enc(cid)}/agent`, agent),
  transcript: (id: string) => req<TranscriptItem[]>("GET", `/v1/tasks/${enc(id)}/transcript?limit=1000`),
};
