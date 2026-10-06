#!/usr/bin/env node
// Drives the app in the run's headless Chromium over CDP, one action per
// call, so an agent can act, look and decide between steps. Run it through
// `quark-verify browser ...`, which sets QV_CDP, QV_APP and QV_EVIDENCE.
//
// Locators (combine with --in <testid> to search inside one element):
//   --role <role> --name <name> [--exact] | --label <label> | --testid <id>
//   | --placeholder <text> | --text <text>, optionally narrowed with --has-text <text>
// Commands:
//   open <hash>                 load the app at <hash> (e.g. "#/new"); --daemon <url> overrides the run's daemon
//   url                         print the current URL
//   click|dblclick <locator>    click the element
//   fill <locator> --value V    replace the text in an input or textarea
//   select <locator> --value V  choose an option of a <select>
//   press <key> [<locator>]     press a key, on the element or the page
//   type --value V [<locator>]  click the element (if given), then type V as key presses (e.g. into a terminal)
//   wait <locator> [--contains T] [--hidden] [--timeout ms]   wait until visible (or hidden), optionally containing T
//   text <locator>              print the element's visible text
//   count <locator>             print how many elements match
//   snapshot [<locator>] [--path F]   ARIA snapshot (of the page or element); printed and, with --path, saved
//   screenshot --path F         save a full-page PNG
//   terminal <task-id> [--contains T]   print the open task terminal's text from the emulator's buffer
//                               (read-only); with --contains, first wait until the text appears
// Relative --path values land in the run's evidence directory.
import { createRequire } from "node:module";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const appDir = path.resolve(here, "../../../../app");
const { chromium } = createRequire(path.join(appDir, "package.json"))("@playwright/test");

const env = (k) => process.env[k] || (() => { throw new Error(`browser.mjs: ${k} is not set; run through quark-verify`); })();
const [cmd, ...rest] = process.argv.slice(2);
const flags = {};
const positional = [];
for (let i = 0; i < rest.length; i++) {
  const a = rest[i];
  if (!a.startsWith("--")) { positional.push(a); continue; }
  const k = a.slice(2);
  if (k === "exact" || k === "hidden") flags[k] = true;
  else flags[k] = rest[++i];
}
const out = (p) => {
  const file = path.isAbsolute(p) ? p : path.join(env("QV_EVIDENCE"), p);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  return file;
};

const browser = await chromium.connectOverCDP(env("QV_CDP"));
try {
  const ctx = browser.contexts()[0] ?? (await browser.newContext());
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  page.setDefaultTimeout(Number(flags.timeout ?? 10_000));
  const scope = flags.in ? page.getByTestId(flags.in) : page;
  const locate = () => {
    const base = pick();
    return base && flags["has-text"] ? base.filter({ hasText: flags["has-text"] }) : base;
  };
  const pick = () => {
    const exact = !!flags.exact;
    if (flags.role) return scope.getByRole(flags.role, flags.name ? { name: flags.name, exact } : {});
    if (flags.label) return scope.getByLabel(flags.label, { exact });
    if (flags.testid) return scope.getByTestId(flags.testid);
    if (flags.placeholder) return scope.getByPlaceholder(flags.placeholder, { exact });
    if (flags.text) return scope.getByText(flags.text, { exact });
    if (flags.in) return page.getByTestId(flags.in);
    return null;
  };
  const need = () => locate() ?? (() => { throw new Error(`${cmd}: give a locator (--role/--label/--testid/--placeholder/--text)`); })();

  switch (cmd) {
    case "open": {
      const hash = positional[0] ?? "";
      const daemon = flags.daemon ?? env("QV_DAEMON_FOR_APP");
      await page.goto(`${env("QV_APP")}?daemon=${encodeURIComponent(daemon)}${hash}`);
      console.log(page.url());
      break;
    }
    case "url": console.log(page.url()); break;
    case "click": await need().first().click(); break;
    case "dblclick": await need().first().dblclick(); break;
    case "fill": await need().first().fill(flags.value ?? ""); break;
    case "select": await need().first().selectOption(flags.value); break;
    case "press": {
      const loc = locate();
      if (loc) await loc.first().press(positional[0]);
      else await page.keyboard.press(positional[0]);
      break;
    }
    case "type": {
      const loc = locate();
      if (loc) await loc.first().click();
      await page.keyboard.type(flags.value ?? "");
      break;
    }
    case "wait": {
      const loc = need().first();
      if (flags.hidden) { await loc.waitFor({ state: "hidden" }); break; }
      await loc.waitFor({ state: "visible" });
      if (flags.contains) {
        const deadline = Date.now() + Number(flags.timeout ?? 10_000);
        while (!(await loc.innerText()).includes(flags.contains)) {
          if (Date.now() > deadline) throw new Error(`wait: element never contained ${JSON.stringify(flags.contains)}; it reads ${JSON.stringify(await loc.innerText())}`);
          await page.waitForTimeout(200);
        }
      }
      console.log("ok");
      break;
    }
    case "text": console.log(await need().first().innerText()); break;
    case "count": console.log(await need().count()); break;
    case "snapshot": {
      const snap = await (locate() ?? page.locator("body")).first().ariaSnapshot();
      if (flags.path) fs.writeFileSync(out(flags.path), snap + "\n");
      console.log(snap);
      break;
    }
    case "screenshot": {
      const file = out(flags.path ?? `screenshot-${Date.now()}.png`);
      await page.screenshot({ path: file, fullPage: true });
      console.log(file);
      break;
    }
    case "terminal": {
      const read = () => page.evaluate((id) => window.__quark?.terminalText(id) ?? null, positional[0]);
      const deadline = Date.now() + Number(flags.timeout ?? 10_000);
      let text = await read();
      while (flags.contains && !(text ?? "").includes(flags.contains) && Date.now() < deadline) {
        await page.waitForTimeout(200);
        text = await read();
      }
      if (text === null) throw new Error(`terminal: no open terminal for task ${positional[0]}; open the worker view first`);
      console.log(text.replace(/\s+$/, ""));
      if (flags.contains && !text.includes(flags.contains)) throw new Error(`terminal: never showed ${JSON.stringify(flags.contains)}`);
      break;
    }
    default:
      throw new Error(`browser.mjs: unknown command ${JSON.stringify(cmd)}; see the header of ${fileURLToPath(import.meta.url)}`);
  }
} catch (e) {
  // One readable line instead of a stack: the agent decides what to do next.
  console.error(`browser ${cmd}: ${String(e.message ?? e).split("\n")[0]}`);
  process.exitCode = 1;
} finally {
  // Disconnects only; the run's Chromium and page stay up for the next call.
  await browser.close();
}
