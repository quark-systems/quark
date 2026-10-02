// Serves the app as the e2e tests expect: the demo daemon on :7392 in quiet
// mode, then Vite on :1421 once the daemon answers. A verification gate runs
// this and waits for http://127.0.0.1:1421/; Playwright then reuses both.
import { spawn } from "node:child_process";

const children = [];
const run = (cmd, args) => {
  const child = spawn(cmd, args, { stdio: "inherit" });
  children.push(child);
  child.on("exit", (code) => stop(code ?? 1));
  return child;
};
const stop = (code) => {
  for (const c of children) if (c.exitCode === null) c.kill("SIGTERM");
  process.exit(code);
};
process.on("SIGTERM", () => stop(0));
process.on("SIGINT", () => stop(0));

run(process.execPath, ["mock/daemon.mjs", "--port", "7392", "--quiet"]);
for (let i = 0; ; i++) {
  try {
    if ((await fetch("http://127.0.0.1:7392/v1/health")).ok) break;
  } catch { /* not up yet */ }
  if (i >= 100) { console.error("e2e-serve: the demo daemon did not answer on :7392"); stop(1); }
  await new Promise((r) => setTimeout(r, 100));
}
run("npx", ["vite", "--port", "1421", "--strictPort"]);
