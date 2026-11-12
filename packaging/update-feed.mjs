// Builds the update feed's channel manifests from Quark's published releases:
//   node packaging/update-feed.mjs <releases.json> <assets dir> <out dir>
//
// <releases.json> is `gh release list --json tagName,isDraft,isPrerelease,publishedAt`;
// <assets dir>/<tag>/ holds that release's latest.json and compat.json (either may be
// missing on old releases). Writes <out dir>/stable.json (newest published full release)
// and <out dir>/beta.json (newest published release, pre-releases included), which the
// update-feed workflow attaches to the `update-feed` release. quarkd's updater reads them;
// see docs/development/releasing.md, Updates.
//
// A manifest is the release's latest.json (Tauri's format: version, notes, pub_date,
// platforms) plus:
//   compat     what the build serves and writes (`quarkd compat --json`)
//   min_from   oldest version that may update straight to it (packaging/update-policy.json)
//   pulled     versions withdrawn after release; an updater on one is offered this one
// A release in `pulled` is never served; the newest release that isn't is.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const [releasesPath, assetsDir, outDir] = process.argv.slice(2);
if (!releasesPath || !assetsDir || !outDir) {
  console.error("usage: node packaging/update-feed.mjs <releases.json> <assets dir> <out dir>");
  process.exit(1);
}

const here = dirname(fileURLToPath(import.meta.url));
const policy = JSON.parse(readFileSync(join(here, "update-policy.json"), "utf8"));
const pulled = new Set(policy.pulled ?? []);

const readJson = (path) => (existsSync(path) ? JSON.parse(readFileSync(path, "utf8")) : null);

// SemVer order, with a pre-release below its release (0.2.0-beta.1 < 0.2.0).
function compareVersions(a, b) {
  const split = (v) => {
    const [core, pre] = v.replace(/^v/, "").split("-", 2);
    return { core: core.split(".").map(Number), pre: pre ? pre.split(".") : [] };
  };
  const x = split(a), y = split(b);
  for (let i = 0; i < 3; i++) if (x.core[i] !== y.core[i]) return x.core[i] - y.core[i];
  if (!x.pre.length || !y.pre.length) return y.pre.length - x.pre.length;
  for (let i = 0; i < Math.max(x.pre.length, y.pre.length); i++) {
    const p = x.pre[i], q = y.pre[i];
    if (p === undefined) return -1;
    if (q === undefined) return 1;
    const pn = /^\d+$/.test(p), qn = /^\d+$/.test(q);
    if (pn && qn && Number(p) !== Number(q)) return Number(p) - Number(q);
    if (pn !== qn) return pn ? -1 : 1;
    if (p !== q) return p < q ? -1 : 1;
  }
  return 0;
}

const releases = JSON.parse(readFileSync(releasesPath, "utf8"))
  .filter((r) => /^v\d+\.\d+\.\d+/.test(r.tagName) && !r.isDraft)
  .map((r) => ({ ...r, version: r.tagName.slice(1) }))
  .filter((r) => !pulled.has(r.version))
  .sort((a, b) => compareVersions(b.version, a.version));

function manifest(release) {
  const dir = join(assetsDir, release.tagName);
  const latest = readJson(join(dir, "latest.json"));
  if (!latest || !Object.keys(latest.platforms ?? {}).length) return null;
  return {
    ...latest,
    compat: readJson(join(dir, "compat.json")),
    min_from: policy.min_from,
    pulled: [...pulled],
  };
}

function newest(includePre) {
  for (const r of releases) {
    if (r.isPrerelease && !includePre) continue;
    const m = manifest(r);
    if (m) return m;
  }
  return null;
}

mkdirSync(outDir, { recursive: true });
for (const [channel, includePre] of [["stable", false], ["beta", true]]) {
  const m = newest(includePre);
  if (!m) {
    console.log(`${channel}: no published release with signed update bundles`);
    continue;
  }
  writeFileSync(join(outDir, `${channel}.json`), JSON.stringify(m, null, 2) + "\n");
  console.log(`${channel}: ${m.version} (${Object.keys(m.platforms).join(", ")})`);
}
