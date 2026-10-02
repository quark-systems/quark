import React from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import { App } from "./App";
import { start, getState } from "./store";

void start();
// Automation handle for the end-to-end tests.
(window as any).__quark = { getState };
createRoot(document.getElementById("root")!).render(<App />);
