import React from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import { App } from "./App";
import { start, getState } from "./store";
import { terminalText } from "./terminal";
import { initTheme } from "./theme";

initTheme();
void start();
// Automation handle for the end-to-end tests.
(window as any).__quark = { getState, terminalText };
createRoot(document.getElementById("root")!).render(<App />);
