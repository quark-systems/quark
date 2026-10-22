import { createContext, useContext } from "react";
import type { CoordinatorCommand } from "../../api";

export interface TranscriptSettings {
  /** Who the agent is, for the live line: "coordinator" or "worker". */
  agent: string;
  /** Show coordinator actions (workers started, questions asked) as cards. */
  actions: boolean;
  /** The Project whose coordinator this is, to find the decisions it asked. */
  project?: string;
  /** Entries with a larger id arrived after the first load, so they animate in. */
  freshAfter: number;
  /** A command's name in the Project's persona. */
  commandLabel: (c: CoordinatorCommand) => string;
}

const NEUTRAL: Record<CoordinatorCommand, string> = { catch_up: "Catch up", status_report: "Status report", wrap_up: "Wrap up" };
export const neutralCommandLabel = (c: CoordinatorCommand) => NEUTRAL[c];

export const TranscriptContext = createContext<TranscriptSettings>({ agent: "agent", actions: false, freshAfter: Infinity, commandLabel: neutralCommandLabel });

export const useTranscript = () => useContext(TranscriptContext);

/** The class that animates an entry in when it arrived after the first load. */
export const rise = (id: number, freshAfter: number) => (id > freshAfter ? " tx-rise" : "");
