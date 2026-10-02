// Per-renderer feature checks in headless Chromium: open time, grid screenshot, selection,
// copy, paste, IME (CDP), Kitty keyboard protocol.
// usage: node scripts/renderer-check.mjs <renderer> <daemon url> [screenshot dir]
import { chromium } from "playwright-core";
const [renderer, daemon, outDir = "screenshots"] = process.argv.slice(2);
const exe = process.env.CHROMIUM ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome";
const browser = await chromium.launch({ executablePath: exe, args: ["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"] });
const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 }, permissions: ["clipboard-read", "clipboard-write"] });
const page = await ctx.newPage();
const cdp = await ctx.newCDPSession(page);
const errors = [];
page.on("pageerror", (e) => errors.push(e.message));
page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });
const base = (process.env.APP_URL ?? "http://127.0.0.1:1420/");
await page.goto(`${base}?renderer=${renderer}&screen=terminals&daemon=${daemon}`);
const res = { renderer };
await page.waitForFunction(() => window.__quark?.allTerms().length >= 4 && window.__quark.allTerms().every((t) => t.adapter || String(t.renderer).startsWith("error")), null, { timeout: 30000 }).catch((e) => errors.push("open timeout"));
res.kinds = await page.evaluate(() => window.__quark.allTerms().map((t) => t.renderer));
res.open_ms = await page.evaluate(() => window.__quark.allTerms().map((t) => Math.round(t.openMs)));
await page.waitForTimeout(2500);
await page.screenshot({ path: `${outDir}/renderer-${renderer}-chromium.png` });

const probe = () => window.__quark.allTerms().find((h) => h.probe);
const taps = async () => page.evaluate(() => { const d = window.__taps; window.__taps = []; return d; });
await page.evaluate(() => { window.__taps = []; window.__quark.dataTaps.add((wid, d) => { if (window.__quark.allTerms().find((h) => h.probe)?.id === wid) window.__taps.push(d); }); });

// selection: drag across the first pane (cargo test output)
const box = await page.locator(".pane").first().locator(".pane-body").boundingBox();
await page.mouse.move(box.x + 12, box.y + 30); await page.mouse.down();
await page.mouse.move(box.x + 420, box.y + 110, { steps: 12 }); await page.mouse.up();
await page.waitForTimeout(200);
res.selection = await page.evaluate(() => (window.__quark.allTerms()[0].adapter.getSelection() || "").slice(0, 60));
// copy via the browser copy command (what Ctrl+C / Cmd+C / menu copy trigger)
await page.evaluate(() => navigator.clipboard.writeText("<empty>"));
await page.evaluate(() => document.execCommand("copy"));
res.copy_execcommand = (await page.evaluate(() => navigator.clipboard.readText())).slice(0, 60);
await page.keyboard.press("Control+Shift+C");
res.copy_ctrl_shift_c = (await page.evaluate(() => navigator.clipboard.readText())).slice(0, 60);

// focus the shell pane
const shell = page.locator(".pane", { hasText: "shell" }).first();
await shell.locator(".pane-body").click({ position: { x: 200, y: 120 } });
await page.waitForTimeout(200);
await page.keyboard.press("Control+c"); await page.waitForTimeout(300); await taps();

// paste
await page.evaluate(() => navigator.clipboard.writeText("echo pasted-ok"));
await page.keyboard.press("Control+Shift+V"); await page.waitForTimeout(300);
res.paste_ctrl_shift_v = await taps();
await page.keyboard.press("Control+V"); await page.waitForTimeout(300);
res.paste_ctrl_v = await taps();
await page.keyboard.press("Control+u"); await page.waitForTimeout(200); await taps();
// synthetic paste event on the terminal's input element (what the OS paste menu / Cmd+V dispatch)
res.paste_event = await page.evaluate(async () => {
  const el = window.__quark.allTerms().find((h) => h.probe).adapter.inputElement();
  if (!el) return "no input element";
  const dt = new DataTransfer(); dt.setData("text/plain", "echo paste-event-ok");
  el.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }));
  await new Promise((r) => setTimeout(r, 300));
  const d = window.__taps; window.__taps = []; return d;
});
await page.keyboard.press("Control+u"); await page.waitForTimeout(200); await taps();

// typing round trip
await page.keyboard.type("echo typed-ok", { delay: 30 });
await page.waitForTimeout(500);
res.typed = (await taps()).join("");
await page.keyboard.press("Control+u"); await page.waitForTimeout(200); await taps();

// IME composition via CDP
await cdp.send("Input.imeSetComposition", { text: "にほ", selectionStart: 2, selectionEnd: 2 });
await page.waitForTimeout(150);
res.ime_during_composition = await taps();
await cdp.send("Input.imeSetComposition", { text: "日本", selectionStart: 2, selectionEnd: 2 });
await cdp.send("Input.insertText", { text: "日本語" });
await page.waitForTimeout(700);
res.ime_committed = await taps();
res.ime_cursor_line = await page.evaluate(() => window.__quark.allTerms().find((h) => h.probe).adapter.cursorLine());
await page.keyboard.press("Control+u"); await page.waitForTimeout(200); await taps();

// Kitty keyboard protocol: the app pushes flags (CSI > 1 u); query (CSI ? u); then Ctrl+Shift+A
const wr = (s) => page.evaluate((s) => window.__quark.allTerms().find((h) => h.probe).adapter.write(new TextEncoder().encode(s)), s);
await wr("\x1b[>1u"); await wr("\x1b[?u"); await page.waitForTimeout(200);
res.kitty_query_reply = JSON.stringify((await taps()).join(""));
const KEYS = ["Control+Shift+A", "Control+Shift+B", "Shift+Enter", "Escape", "Control+i"];
const probeKeys = async () => { const o = {}; for (const k of KEYS) { await page.keyboard.press(k); await page.waitForTimeout(150); o[k] = JSON.stringify((await taps()).join("")); } return o; };
res.kitty_mode_keys = await probeKeys();
await wr("\x1b[<u"); await page.waitForTimeout(100); await taps();
res.legacy_mode_keys = await probeKeys();
await page.keyboard.press("Control+c"); await taps();

res.errors = errors.slice(0, 10);
console.log(JSON.stringify(res));
await browser.close();
