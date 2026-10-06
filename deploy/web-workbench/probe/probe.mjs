// Drives the web workbench in a real (headless) Chromium: logs in, lets the workbench load, takes
// screenshots, and checks what the lock-down actually blocks. Used by hand and by the smoke script.
//
//   node probe.mjs <base-url> <password> <out-dir> [--tour]
//
// Needs `playwright-core` (it lives in editors/vscode/node_modules) and a Chromium
// (CHROMIUM=/usr/bin/chromium by default). Exit code 0 = every check held.
import { mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';

const require = createRequire(resolve(process.cwd(), 'editors/vscode/') + '/');
const { chromium } = require('playwright-core');

const [base, password, out, ...flags] = process.argv.slice(2);
if (!base || !password || !out) {
  console.error('usage: probe.mjs <base-url> <password> <out-dir> [--tour]');
  process.exit(2);
}
mkdirSync(out, { recursive: true });
const results = [];
const check = (name, ok, detail = '') => {
  results.push({ name, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`);
};

const browser = await chromium.launch({
  executablePath: process.env.CHROMIUM || '/usr/bin/chromium',
  headless: true,
  args: ['--no-sandbox'],
});
const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 }, ignoreHTTPSErrors: true });
const page = await ctx.newPage();

// 1. A wrong password is refused.
await page.goto(`${base}/login`);
await page.fill('input[type=password]', 'definitely-not-the-password');
await page.click('input[type=submit], button[type=submit]');
await page.waitForLoadState('networkidle');
const refusedText = (await page.textContent('body')) ?? '';
check('a wrong password is refused', /incorrect|invalid|wrong/i.test(refusedText) && page.url().includes('/login'));
await page.screenshot({ path: `${out}/00-login-refused.png` });

// 2. The right one lands in the workbench.
await page.fill('input[type=password]', password);
await page.click('input[type=submit], button[type=submit]');
await page.waitForSelector('.monaco-workbench', { timeout: 60_000 });
check('the right password opens the workbench', true);
await page.waitForTimeout(8000);
await page.screenshot({ path: `${out}/01-workbench.png` });

// 3. What is on screen.
const chrome = await page.evaluate(() => ({
  menubar: !!document.querySelector('.menubar') && getComputedStyle(document.querySelector('.menubar')).display !== 'none',
  statusbar: !!document.querySelector('.part.statusbar') && document.querySelector('.part.statusbar').offsetHeight > 0,
  activitybar: !!document.querySelector('.part.activitybar') && document.querySelector('.part.activitybar').offsetHeight > 0,
  title: document.title,
}));
check('no menu bar', !chrome.menubar);
check('no status bar', !chrome.statusbar);
check('no activity bar (no Explorer/Search/SCM/Run/Extensions icons)', !chrome.activitybar);
console.log(`      title: ${chrome.title}`);

// 4. The command palette cannot open a terminal.
async function palette(text) {
  await page.keyboard.press('F1');
  await page.waitForSelector('.quick-input-widget', { state: 'visible' });
  await page.keyboard.type(`>${text}`);
  await page.waitForTimeout(700);
  const rows = await page.$$eval('.quick-input-list .monaco-list-row', (els) => els.map((e) => e.getAttribute('aria-label') ?? e.textContent ?? ''));
  return rows;
}
async function closePalette() {
  await page.keyboard.press('Escape');
  await page.waitForTimeout(300);
}
const terminalRows = await palette('Terminal: Create New Terminal');
console.log(`      palette rows for "Terminal: Create New Terminal": ${terminalRows.length}`);
if (terminalRows.length) {
  await page.keyboard.press('Enter');
  await page.waitForTimeout(6000); // the pty host answers within 5 s or the terminal never starts
  await page.keyboard.type('echo MARK-$((6*7)); id');
  await page.keyboard.press('Enter');
  await page.waitForTimeout(2500);
}
await page.screenshot({ path: `${out}/02-terminal-attempt.png` });
const terminalText = await page.evaluate(
  () => `${document.querySelector('.xterm-rows')?.textContent ?? ''} ${document.querySelector('.xterm-accessibility-tree')?.textContent ?? ''}`,
);
// A tab may appear (the UI creates it before it asks the pty host); what must not happen is a shell.
check('a terminal runs no command (no shell answers)', !/MARK-42|uid=/.test(terminalText), `terminal text: ${JSON.stringify(terminalText.trim().slice(0, 80))}`);
await page.keyboard.press('Escape');

// 5. The marketplace is not reachable: the Extensions view finds nothing to install.
const ext = await palette('View: Show Extensions');
if (ext.length) {
  await page.keyboard.press('Enter');
  await page.waitForTimeout(2500);
  await page.keyboard.type('python');
  await page.waitForTimeout(5000);
}
await page.screenshot({ path: `${out}/03-extensions-attempt.png` });
const gallery = await page.evaluate(() => document.querySelectorAll('.extensions-list .extension-action.install:not(.disabled)').length);
check('the marketplace offers nothing to install', gallery === 0, `${gallery} installable`);

await browser.close();
const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} checks held`);
process.exit(failed.length ? 1 : 0);
