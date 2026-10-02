// Drives the real UI with trusted input in Chromium and checks each screen's interactions.
import { chromium } from "playwright-core";
const url = process.argv[2] ?? "http://127.0.0.1:1420/";
const out = process.argv[3] ?? "screenshots";
const D = "http://" + (process.env.DAEMON_HOST ?? "127.0.0.1:7420");
const exe = process.env.CHROMIUM ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome";
const browser = await chromium.launch({ executablePath: exe, args: ["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"] });
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
const errs = [];
page.on("pageerror", (e) => errs.push(e.message));
await page.goto(url);
await page.waitForSelector(".card");
const res = {};

// --- terminals: real keystrokes into the shell pane ---
await page.keyboard.press("Control+2");
await page.waitForTimeout(1500);
const shell = page.locator(".pane", { hasText: "shell" }).first();
await shell.locator(".pane-body").click();
await page.keyboard.type("echo quark-typed-ok", { delay: 60 });
await page.keyboard.press("Enter");
await page.waitForTimeout(800);
await page.evaluate(() => window.__quark.latency.reset());
for (let i = 0; i < 40; i++) { await page.keyboard.type("abcdefghij"[i % 10]); await page.waitForTimeout(100); }
await page.keyboard.press("Control+u");
await page.waitForTimeout(500);
res.echo_browser_trusted_keys = await page.evaluate(() => window.__quark.latency.stats());
res.workers_after_fit = await (await fetch(D + "/v1/workers")).json();
await page.screenshot({ path: `${out}/web-terminals-typed.png` });

if (process.env.ONLY_TYPING) { console.log(JSON.stringify(res.echo_browser_trusted_keys)); await browser.close(); process.exit(0); }
// --- inbox: j then 2 answers the second open decision ---
await page.keyboard.press("Control+5");
await page.waitForTimeout(500);
const before = (await (await fetch(D + "/v1/decisions")).json()).filter((d) => d.state === "open");
await page.keyboard.press("j");
await page.keyboard.press("2");
await page.waitForTimeout(800);
const after = await (await fetch(D + "/v1/decisions")).json();
res.decision_answer = { open_before: before.length, open_after: after.filter((d) => d.state === "open").length };

// --- diff: click a line, type a comment, Ctrl+Enter ---
await page.keyboard.press("Control+4");
await page.waitForSelector("table.diff tr.add");
await page.locator("table.diff tr.add").nth(2).click();
await page.keyboard.type("Typed from the Tauri POC diff view");
await page.keyboard.press("Control+Enter");
await page.waitForTimeout(800);
const prId = await page.locator(".diff-files select").inputValue();
const cs = await (await fetch(`${D}/v1/pull-requests/${prId}/comments`)).json();
res.comment_posted = cs.some((c) => c.body === "Typed from the Tauri POC diff view");
await page.screenshot({ path: `${out}/web-diff-comment.png` });

// --- chat: send a message, watch it stream ---
await page.keyboard.press("Control+3");
await page.waitForTimeout(300);
await page.locator(".composer textarea").fill("How are the workers doing? Show a code sample.");
await page.keyboard.press("Enter");
await page.waitForTimeout(900);
res.chat_streaming_seen = await page.locator(".cursor-blink").count();
await page.screenshot({ path: `${out}/web-chat-streaming.png` });
await page.waitForTimeout(4000);

// --- reconnect resumes from last seq ---
const s0 = await page.evaluate(() => ({ seq: window.__quark.getState().lastSeq, r: window.__quark.getState().reconnects }));
await page.evaluate(() => window.__quark.dropConnection());
await page.waitForTimeout(2500);
const s1 = await page.evaluate(() => ({ seq: window.__quark.getState().lastSeq, r: window.__quark.getState().reconnects, c: window.__quark.getState().connected }));
res.reconnect = { before: s0, after: s1 };

res.page_errors = errs;
console.log(JSON.stringify(res, null, 1));
await browser.close();
