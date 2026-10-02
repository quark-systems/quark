// Text-input checks for the web build in headless Chromium: paste, non-ASCII text, IME composition.
// usage: node web/input-checks.mjs <page url> <out dir>
import { createRequire } from 'node:module';
const require = createRequire(process.env.PW_DIR || '/home/claude/src/pw/');
const { chromium } = require('playwright');

const [url, out] = process.argv.slice(2);
const b = await chromium.launch({
  executablePath: process.env.PW_CHROMIUM || '/opt/pw-browsers/chromium',
  args: ['--use-gl=angle', '--use-angle=swiftshader', '--enable-unsafe-swiftshader'],
});
const ctx = await b.newContext({ viewport: { width: 1400, height: 900 } });
await ctx.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: new URL(url).origin });
const p = await ctx.newPage();
p.on('pageerror', e => { if (!/control flow/.test(e.message)) console.log('pageerror:', e.message.slice(0, 300)); });
p.on('console', m => { if (/ime|composition|marked|clipboard/i.test(m.text())) console.log('console:', m.text().slice(0, 200)); });
await p.goto(url);
await p.waitForSelector('canvas');
await p.waitForTimeout(6000);
await p.mouse.click(900, 700);
await p.keyboard.press('Control+3');
await p.waitForTimeout(1000);
await p.keyboard.press('Enter');
const input = { x: 210, y: 820, width: 1190, height: 50 };
const crop = async (name) => { await p.screenshot({ path: `${out}/${name}`, clip: input }); console.log('shot', name); };

// 1. Paste from the system clipboard.
await p.evaluate(() => navigator.clipboard.writeText('pasted: café ✓ '));
await p.keyboard.press('Control+v');
await p.waitForTimeout(800);
await crop('web-input-1-paste.png');

// 2. Text that is not on the keyboard layout (Playwright sends it as an input event, like an IME commit).
await p.keyboard.insertText('typed: ünïcödé 日本語 한국어 🦊 ');
await p.waitForTimeout(800);
await crop('web-input-2-insert-text.png');

// 2b. A real keydown carrying a non-ASCII character, as a non-US keyboard layout produces.
const cdp = await ctx.newCDPSession(p);
for (const ch of ['é', 'ß', 'ж']) {
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: ch, text: ch, unmodifiedText: ch });
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: ch });
}
await p.waitForTimeout(800);
await crop('web-input-2b-layout-keys.png');

// 3. IME composition (preedit) through CDP, then commit.
await cdp.send('Input.imeSetComposition', { text: 'にほんご', selectionStart: 4, selectionEnd: 4 });
await p.waitForTimeout(800);
await crop('web-input-3-ime-preedit.png');
await cdp.send('Input.insertText', { text: '日本語' });
await p.waitForTimeout(800);
await crop('web-input-4-ime-commit.png');

// 4. Font fallback: paste CJK / Hangul / emoji (only DejaVu is embedded; warpui has no web fallback fonts).
await p.evaluate(() => navigator.clipboard.writeText(' 日本語 한국어 🦊 →✓'));
await p.keyboard.press('Control+v');
await p.waitForTimeout(800);
await crop('web-input-5-cjk-emoji.png');
await b.close();
