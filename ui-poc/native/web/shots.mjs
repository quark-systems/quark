// Headless-Chromium screenshots of the web build.
// usage: node web/shots.mjs <page url> <out dir>
// Needs Playwright (resolved from $PW_DIR, default /home/claude/src/pw/) and a Chromium at
// $PW_CHROMIUM (default /opt/pw-browsers/chromium); rendering uses SwiftShader (software WebGL2).
import { createRequire } from 'node:module';
const require = createRequire(process.env.PW_DIR || '/home/claude/src/pw/');
const { chromium } = require('playwright');

const [url, out] = process.argv.slice(2);
const b = await chromium.launch({
  executablePath: process.env.PW_CHROMIUM || '/opt/pw-browsers/chromium',
  args: ['--use-gl=angle', '--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--enable-unsafe-webgpu'],
});
const ctx = await b.newContext({ viewport: { width: 1400, height: 900 } });
await ctx.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: new URL(url).origin });
const p = await ctx.newPage();
const log = [];
p.on('console', m => { const t = `${m.type()}: ${m.text().slice(0, 300)}`; log.push(t); if (m.type() === 'error') console.log(t); });
p.on('pageerror', e => { if (!/control flow/.test(e.message)) console.log('pageerror:', e.message.slice(0, 500)); });

const t0 = Date.now();
await p.goto(url);
await p.waitForSelector('canvas');
await p.waitForTimeout(6000);
const shot = async (name) => { await p.screenshot({ path: `${out}/${name}` }); console.log('shot', name, ((Date.now() - t0) / 1000).toFixed(1) + 's'); };

// Focus the canvas (click empty board area), then the board twice, a few seconds apart.
await p.mouse.click(900, 700);
await shot('web-1-board-a.png');
await p.waitForTimeout(8000);
await shot('web-2-board-b.png');

// Chat: Ctrl+3, Enter focuses the input, type, Enter sends; reply streams in.
await p.keyboard.press('Control+3');
await p.waitForTimeout(1500);
await p.keyboard.press('Enter');
await p.keyboard.type('Hello from the web build: please list the running workers.', { delay: 15 });
await shot('web-3-chat-typing.png');
await p.keyboard.press('Enter');
await p.waitForTimeout(6000);
await shot('web-4-chat-after-send.png');

// Clipboard: select-all-ish drag in the chat, Ctrl+C, read navigator.clipboard.
await p.keyboard.press('Escape');
await p.mouse.move(300, 120); await p.mouse.down(); await p.mouse.move(900, 300, { steps: 10 }); await p.mouse.up();
await p.keyboard.press('Control+c');
await p.waitForTimeout(500);
const clip = await p.evaluate(async () => { try { return await navigator.clipboard.readText(); } catch (e) { return 'ERR ' + e; } });
console.log('clipboard after chat copy:', JSON.stringify(clip.slice(0, 120)));

// Other screens.
for (const [k, name] of [['Control+2', 'web-5-terminals.png'], ['Control+4', 'web-6-diff.png'], ['Control+5', 'web-7-decisions-prs.png']]) {
  await p.keyboard.press(k);
  await p.waitForTimeout(2500);
  await shot(name);
}
// Palette + perf overlay on the board.
await p.keyboard.press('Control+1');
await p.keyboard.press('F2');
await p.waitForTimeout(4000);
await shot('web-8-board-perf-overlay.png');
await p.keyboard.press('Control+k');
await p.keyboard.type('dec', { delay: 30 });
await p.waitForTimeout(1500);
await shot('web-9-palette.png');
console.log('console lines:', log.length);
await b.close();
