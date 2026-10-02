import React from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import { App } from "./App";
import { start, getState, dropConnection } from "./store";
import { frames, latency } from "./perf";
import { allTerms, dataTaps } from "./terms";

start();
// debug/automation handle (used by scripts/interact.mjs)
(window as any).__quark = { getState, dropConnection, frames, latency, allTerms, dataTaps };
createRoot(document.getElementById("root")!).render(<App />);
