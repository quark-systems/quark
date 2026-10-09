// Runs before `npm run dev` and `npm run build` (and so before `tauri dev` and `tauri build`):
// when a pull added a dependency that is not installed yet, say so plainly instead of letting
// Vite fail later with "Failed to resolve import".
import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const app = join(dirname(fileURLToPath(import.meta.url)), "..");
const pkg = JSON.parse(readFileSync(join(app, "package.json"), "utf8"));
const wanted = Object.keys({ ...pkg.dependencies, ...pkg.devDependencies });
const missing = wanted.filter((name) => !existsSync(join(app, "node_modules", name, "package.json")));
if (missing.length) {
  console.error(`\nMissing packages: ${missing.join(", ")}.\nRun \`npm install\` in app/ and start again.\n`);
  process.exit(1);
}
