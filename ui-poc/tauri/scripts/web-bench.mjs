// Runs the ?bench=1 scenario in headless Chromium and prints the result JSON.
// usage: node scripts/web-bench.mjs "<url with ?bench=1...>"
import { chromium } from "playwright-core";
const exe = process.env.CHROMIUM ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome";
const browser = await chromium.launch({ executablePath: exe, args: ["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"] });
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
await page.goto(process.argv[2]);
await page.waitForFunction(() => window.__QUARK_BENCH__, null, { timeout: 90000, polling: 500 });
console.log("QUARK_BENCH_RESULT " + JSON.stringify(await page.evaluate(() => window.__QUARK_BENCH__)));
await browser.close();
