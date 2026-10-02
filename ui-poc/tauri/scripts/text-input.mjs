// Text-input checks in Chromium: terminal selection, IME composition (CDP), clipboard paste.
import { chromium } from "playwright-core";
const url = process.argv[2];
const exe = "/opt/pw-browsers/chromium-1194/chrome-linux/chrome";
const browser = await chromium.launch({ executablePath: exe, args: ["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"] });
const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 }, permissions: ["clipboard-read", "clipboard-write"] });
const page = await ctx.newPage();
const cdp = await ctx.newCDPSession(page);
await page.goto(url);
await page.waitForSelector(".card");
const res = {};
await page.keyboard.press("Control+2");
await page.waitForTimeout(1500);
// selection by mouse drag in the cargo pane
const box = await page.locator(".pane").first().locator(".pane-body").boundingBox();
await page.mouse.move(box.x + 10, box.y + 40); await page.mouse.down();
await page.mouse.move(box.x + 400, box.y + 120, { steps: 10 }); await page.mouse.up();
res.terminal_selection = await page.evaluate(() => window.__quark.allTerms()[0].term.getSelection().slice(0, 80));
// IME composition into the shell terminal
const shell = page.locator(".pane", { hasText: "shell" }).first();
await shell.locator(".pane-body").click();
await page.keyboard.press("Control+c"); await page.keyboard.press("Enter"); await page.waitForTimeout(400);
await page.evaluate(() => { window.__imeData = []; window.__quark.allTerms().find((h) => h.probe).term.onData((d) => window.__imeData.push(d)); });
await cdp.send("Input.imeSetComposition", { text: "にほ", selectionStart: 2, selectionEnd: 2 });
await page.waitForTimeout(200);
await cdp.send("Input.imeSetComposition", { text: "日本", selectionStart: 2, selectionEnd: 2 });
await cdp.send("Input.insertText", { text: "日本語" });
await page.waitForTimeout(800);
res.terminal_ime_ondata = await page.evaluate(() => window.__imeData);
res.terminal_ime_line = await page.evaluate(() => {
  const t = window.__quark.allTerms().find((h) => h.probe).term; const b = t.buffer.active;
  return b.getLine(b.cursorY + b.viewportY).translateToString(true);
});
await page.keyboard.press("Control+u");
// IME in chat composer: Enter during composition must not send
await page.keyboard.press("Control+3");
await page.locator(".composer textarea").click();
await cdp.send("Input.imeSetComposition", { text: "かんじ", selectionStart: 3, selectionEnd: 3 });
await cdp.send("Input.insertText", { text: "漢字テスト" });
res.chat_ime_value = await page.locator(".composer textarea").inputValue();
// paste into terminal via clipboard
await page.evaluate(() => navigator.clipboard.writeText("echo pasted-ok"));
await page.keyboard.press("Control+2"); await page.waitForTimeout(500);
await shell.locator(".pane-body").click();
await page.keyboard.press("Control+Shift+V").catch(() => {});
await page.waitForTimeout(600);
res.terminal_after_ctrl_shift_v = await page.evaluate(() => {
  const t = window.__quark.allTerms().find((h) => h.probe).term; const b = t.buffer.active;
  return b.getLine(b.cursorY + b.viewportY).translateToString(true);
});
await page.keyboard.press("Control+u");
await page.screenshot({ path: "/tmp/claude-0/-home-claude/5c5859ec-7498-51fb-8a8c-d8c246bba70c/scratchpad/textinput.png" });
console.log(JSON.stringify(res, null, 1));
await browser.close();
