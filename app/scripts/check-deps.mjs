// Runs before `npm run dev` and `npm run build` (and so before `tauri dev` and `tauri build`):
// when a pull added a dependency that is not installed yet, say so plainly instead of letting
// Vite fail later with "Failed to resolve import".
import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const app = join(dirname(fileURLToPath(import.meta.url)), "..");
const pkg = JSON.parse(readFileSync(join(app, "package.json"), "utf8"));
const wanted = Object.keys({ ...pkg.dependencies, ...pkg.devDependencies });
const specs = { ...pkg.dependencies, ...pkg.devDependencies };
const installed = (name) => {
  const f = join(app, "node_modules", name, "package.json");
  return existsSync(f) ? JSON.parse(readFileSync(f, "utf8")).version : null;
};
const missing = wanted.filter((name) => !installed(name));
// An exact pin (e.g. a beta carrying a fix) must match: an older copy left from before the pull
// would still resolve and quietly bring the old bug back.
const stale = wanted.filter((name) => /^\d/.test(specs[name]) && installed(name) && installed(name) !== specs[name]);
if (missing.length || stale.length) {
  const what = [
    missing.length ? `Missing packages: ${missing.join(", ")}.` : "",
    stale.length ? `Out of date: ${stale.map((n) => `${n} ${installed(n)} (wants ${specs[n]})`).join(", ")}.` : "",
  ].filter(Boolean).join("\n");
  console.error(`\n${what}\nRun \`npm install\` in app/ and start again.\n`);
  process.exit(1);
}
