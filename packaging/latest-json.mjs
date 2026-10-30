// Writes the updater manifest (latest.json) for a release from the bundles the release
// workflow built:
//   node packaging/latest-json.mjs <version> <dir with the bundles and their .sig files>
// Platforms whose updater bundle or signature is missing are left out.
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [version, dir] = process.argv.slice(2);
if (!version || !dir) {
  console.error("usage: node packaging/latest-json.mjs <version> <dir>");
  process.exit(1);
}
const files = readdirSync(dir);
const base = `https://github.com/quark-systems/quark/releases/download/v${version}`;
const platforms = {};
const add = (platform, match) => {
  const bundle = files.find((f) => match(f) && !f.endsWith(".sig"));
  if (!bundle || !files.includes(`${bundle}.sig`)) return;
  platforms[platform] = {
    signature: readFileSync(join(dir, `${bundle}.sig`), "utf8").trim(),
    url: `${base}/${encodeURIComponent(bundle)}`,
  };
};
add("darwin-aarch64", (f) => f.endsWith(".app.tar.gz"));
add("windows-x86_64", (f) => f.endsWith("-setup.exe"));
const manifest = {
  version,
  notes: `See https://github.com/quark-systems/quark/releases/tag/v${version}`,
  pub_date: new Date().toISOString(),
  platforms,
};
writeFileSync(join(dir, "latest.json"), JSON.stringify(manifest, null, 2) + "\n");
console.log(`latest.json: ${Object.keys(platforms).join(", ") || "no signed platforms"}`);
