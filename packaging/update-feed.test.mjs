// node --test packaging/*.test.mjs  (CI runs it in the desktop-app job)
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const script = join(dirname(fileURLToPath(import.meta.url)), "update-feed.mjs");

function feed(releases, signed) {
  const dir = mkdtempSync(join(tmpdir(), "quark-feed-"));
  writeFileSync(join(dir, "releases.json"), JSON.stringify(releases));
  for (const tag of signed) {
    mkdirSync(join(dir, "assets", tag), { recursive: true });
    const platforms = { "darwin-aarch64": { url: `https://x/${tag}`, signature: "sig" } };
    writeFileSync(join(dir, "assets", tag, "latest.json"), JSON.stringify({ version: tag.slice(1), platforms }));
    writeFileSync(join(dir, "assets", tag, "compat.json"), JSON.stringify({ api_version: 1 }));
  }
  execFileSync("node", [script, join(dir, "releases.json"), join(dir, "assets"), join(dir, "out")]);
  const read = (c) => {
    const p = join(dir, "out", `${c}.json`);
    return existsSync(p) ? JSON.parse(readFileSync(p, "utf8")) : null;
  };
  return { stable: read("stable"), beta: read("beta") };
}

const rel = (tagName, isPrerelease = false, isDraft = false) => ({ tagName, isPrerelease, isDraft });

test("stable is the newest full release, beta includes pre-releases", () => {
  const { stable, beta } = feed(
    [rel("v0.1.0"), rel("v0.1.10"), rel("v0.1.2"), rel("v0.2.0-beta.2", true), rel("v0.2.0-beta.10", true)],
    ["v0.1.0", "v0.1.10", "v0.1.2", "v0.2.0-beta.2", "v0.2.0-beta.10"],
  );
  assert.equal(stable.version, "0.1.10");
  assert.equal(beta.version, "0.2.0-beta.10");
  assert.deepEqual(stable.compat, { api_version: 1 });
  assert.equal(typeof stable.min_from, "string");
});

test("a release beats its own pre-releases on beta", () => {
  const { beta } = feed([rel("v0.2.0-beta.3", true), rel("v0.2.0")], ["v0.2.0-beta.3", "v0.2.0"]);
  assert.equal(beta.version, "0.2.0");
});

test("drafts, unsigned releases and non-version tags are skipped", () => {
  const { stable, beta } = feed(
    [rel("v0.3.0", false, true), rel("v0.2.0"), rel("v0.1.0"), rel("update-feed", true)],
    ["v0.3.0", "v0.1.0"],
  );
  assert.equal(stable.version, "0.1.0");
  assert.equal(beta.version, "0.1.0");
});

test("no signed release writes no manifest", () => {
  const { stable, beta } = feed([rel("v0.1.0")], []);
  assert.equal(stable, null);
  assert.equal(beta, null);
});
