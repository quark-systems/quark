// Fails when the /v1 API changed in a way a client built for the previous release can't
// handle, unless the contract was bumped (quark_systems::API_CONTRACT):
//   node packaging/api-compat.mjs <old openapi.json> <new openapi.json> <old contract> <new contract>
// CI runs it against the newest v* tag's api/openapi.json. Breaking means: a path or
// operation removed, a schema property removed or its type changed, a new required
// parameter, or a new required property in a request body. Additions are fine.
import { readFileSync } from "node:fs";

const [oldPath, newPath, oldContract, newContract] = process.argv.slice(2);
if (!oldPath || !newPath || !oldContract || !newContract) {
  console.error("usage: node packaging/api-compat.mjs <old.json> <new.json> <old contract> <new contract>");
  process.exit(2);
}
const before = JSON.parse(readFileSync(oldPath, "utf8"));
const after = JSON.parse(readFileSync(newPath, "utf8"));

function breakingChanges(before, after) {
  const out = [];
  const methods = ["get", "put", "post", "delete", "patch"];
  const ref = (s) => s?.$ref?.split("/").pop();
  const requestSchemas = new Set();
  for (const ops of Object.values(after.paths ?? {}))
    for (const m of methods) {
      const body = ops[m]?.requestBody?.content?.["application/json"]?.schema;
      if (ref(body)) requestSchemas.add(ref(body));
    }

  for (const [path, ops] of Object.entries(before.paths ?? {})) {
    const now = after.paths?.[path];
    for (const m of methods) {
      if (!ops[m]) continue;
      if (!now?.[m]) { out.push(`${m.toUpperCase()} ${path} was removed`); continue; }
      const had = new Set((ops[m].parameters ?? []).map((p) => `${p.in}:${p.name}`));
      for (const p of now[m].parameters ?? [])
        if (p.required && !had.has(`${p.in}:${p.name}`))
          out.push(`${m.toUpperCase()} ${path} has a new required ${p.in} parameter ${p.name}`);
    }
  }
  const typeOf = (p) => p?.type ?? ref(p) ?? ref(p?.items) ?? (p?.oneOf || p?.allOf || p?.anyOf ? "composite" : "any");
  for (const [name, schema] of Object.entries(before.components?.schemas ?? {})) {
    const now = after.components?.schemas?.[name];
    if (!now) continue; // a schema no operation uses any more shows up as a removed path
    for (const [prop, def] of Object.entries(schema.properties ?? {})) {
      const nowDef = now.properties?.[prop];
      if (!nowDef) out.push(`${name}.${prop} was removed`);
      else if (JSON.stringify(typeOf(def)) !== JSON.stringify(typeOf(nowDef)))
        out.push(`${name}.${prop} changed type`);
    }
    if (requestSchemas.has(name)) {
      const was = new Set(schema.required ?? []);
      for (const r of now.required ?? []) if (!was.has(r)) out.push(`${name}.${r} is newly required in a request`);
    }
  }
  return out;
}

const breaking = breakingChanges(before, after);
if (!breaking.length) {
  console.log("/v1 API: no breaking changes");
} else if (Number(newContract) > Number(oldContract)) {
  console.log(`/v1 API: contract ${oldContract} -> ${newContract}, breaking changes allowed:\n  ${breaking.join("\n  ")}`);
} else {
  console.error(
    `/v1 API: breaking changes without bumping API_CONTRACT (still ${newContract}):\n  ${breaking.join("\n  ")}\n` +
      "Keep the old shape (add, don't change), or bump API_CONTRACT in crates/quark-systems/src/service.rs " +
      "and serve the previous contract until API_CONTRACT_MIN moves (docs/development/releasing.md).",
  );
  process.exit(1);
}
