import { defineConfig } from "@playwright/test";

// Runs the app against the demo daemon in quiet mode (no background activity).
// Set PW_CHROMIUM to use an already-installed Chromium instead of Playwright's download.
// A verification gate (ADR-15) starts both servers itself with `npm run e2e:serve`
// and sets GATE_APP_URL, so Playwright reuses them instead of starting its own.
const reuseExistingServer = !!process.env.GATE_APP_URL;
export default defineConfig({
  testDir: "e2e",
  timeout: 30_000,
  workers: 1,
  use: {
    baseURL: "http://127.0.0.1:1421",
    viewport: { width: 1400, height: 900 },
    launchOptions: process.env.PW_CHROMIUM ? { executablePath: process.env.PW_CHROMIUM } : {},
  },
  webServer: [
    { command: "node mock/daemon.mjs --port 7392 --quiet", url: "http://127.0.0.1:7392/v1/health", reuseExistingServer },
    { command: "npx vite --port 1421", url: "http://127.0.0.1:1421/", reuseExistingServer },
  ],
});
