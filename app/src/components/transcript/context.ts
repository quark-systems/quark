import { createContext, useContext } from "react";

export interface TranscriptSettings {
  /** Who the agent is, for the live line: "coordinator" or "worker". */
  agent: string;
  /** Show coordinator actions (workers started, questions asked) as cards. */
  actions: boolean;
  /** Entries with a larger id arrived after the first load, so they animate in. */
  freshAfter: number;
}

export const TranscriptContext = createContext<TranscriptSettings>({ agent: "agent", actions: false, freshAfter: Infinity });

export const useTranscript = () => useContext(TranscriptContext);

/** The class that animates an entry in when it arrived after the first load. */
export const rise = (id: number, freshAfter: number) => (id > freshAfter ? " tx-rise" : "");
