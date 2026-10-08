// The five state words of the glossary (busy, needs you, ready, failed, parked) plus idle,
// and how a worker's state maps to them. One mapping, used by every dot and row.
import type { TaskState } from "../api";

export type Tone = "busy" | "needs-you" | "ready" | "failed" | "parked" | "idle";

export const TONE_LABEL: Record<Tone, string> = {
  busy: "Busy", "needs-you": "Needs you", ready: "Ready", failed: "Failed", parked: "Parked", idle: "Idle",
};

/** The tone a worker in `state` shows in the left list and on its cards. */
export function taskTone(state: TaskState): Tone {
  switch (state) {
    case "running": return "busy";
    case "needs_decision":
    case "blocked": return "needs-you";
    case "in_review": return "ready";
    case "failed": return "failed";
    case "paused":
    case "queued": return "parked";
    default: return "idle";
  }
}
