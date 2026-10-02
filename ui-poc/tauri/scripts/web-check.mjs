// Loads the frontend in Chromium (the "web build" story) and screenshots each screen.
// usage: node scripts/web-check.mjs [url] [outdir]
import { chromium } from "playwright-core";
const url = process.argv[2] ?? "http://127.0.0.1:1420/";
const out = process.argv[3] ?? "screenshots";
const exe = process.env.CHROMIUM ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome";
const browser = await chromium.launch({ executablePath: exe, args: ["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"] });
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
const logs = [];
page.on("console", (m) => logs.push(`[${m.type()}] ${m.text()}`));
page.on("pageerror", (e) => logs.push(`[pageerror] ${e.message}`));
await page.goto(url);
await page.waitForSelector(".card", { timeout: 15000 });
await page.waitForTimeout(1000);
await page.screenshot({ path: `${out}/web-board.png` });
for (const [i, name] of [[2, "terminals"], [3, "chat"], [4, "diff"], [5, "inbox"]]) {
  await page.keyboard.press(`Control+${i}`);
  await page.waitForTimeout(1500);
  await page.screenshot({ path: `${out}/web-${name}.png` });
}
await page.keyboard.press("Control+k");
await page.waitForTimeout(300);
await page.keyboard.type("pr");
await page.waitForTimeout(300);
await page.screenshot({ path: `${out}/web-palette.png` });
await browser.close();
console.log(logs.filter((l) => !l.includes("[debug]")).slice(0, 40).join("\n"));
