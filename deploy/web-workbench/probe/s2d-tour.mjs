// Walks the S2D demo in the web workbench in a real (headless) Chromium: login, the Overview board, Awaiting you,
// the three proposals, approve, the approved records, the teaser query. Screenshots to read, a few checks.
//
//   node s2d-tour.mjs <base-url> <password-file> <out-dir>
//
// The password is read from a FILE (never an argument, never printed). Needs `playwright-core` (editors/vscode/node_modules)
// and a Chromium (CHROMIUM=/usr/bin/chromium by default). Run from the repository root.
import { mkdirSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';

const require = createRequire(resolve(process.cwd(), 'editors/vscode/') + '/');
const { chromium } = require('playwright-core');
const [base, passwordFile, out] = process.argv.slice(2);
if (!base || !passwordFile || !out) {
  console.error('usage: s2d-tour.mjs <base-url> <password-file> <out-dir>');
  process.exit(2);
}
mkdirSync(out, { recursive: true });
const password = readFileSync(passwordFile, 'utf8').trim();
let failed = 0;
const check = (name, ok, detail = '') => {
  if (!ok) failed += 1;
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`);
};

const browser = await chromium.launch({ executablePath: process.env.CHROMIUM || '/usr/bin/chromium', headless: true, args: ['--no-sandbox'] });
const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 }, ignoreHTTPSErrors: true });
const page = await ctx.newPage();
const shot = (n) => page.screenshot({ path: `${out}/${n}.png` });
const pane = (title) => page.locator('.pane', { has: page.locator('.pane-header', { hasText: title }) });
const frames = (f = page.mainFrame()) => [f, ...f.childFrames().flatMap((c) => frames(c))];
/** Text of every webview frame that contains `needle` (webviews are nested iframes). */
async function webviewText(needle, timeout = 30_000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    for (const f of frames()) {
      const t = await f.evaluate(() => document.body?.innerText ?? '').catch(() => '');
      if (t.includes(needle)) return t;
    }
    await page.waitForTimeout(500);
  }
  return '';
}

await page.goto(`${base}/login`);
await page.fill('input[type=password]', password);
await page.click('input[type=submit], button[type=submit]');
await page.waitForSelector('.monaco-workbench', { timeout: 90_000 });
await page.waitForTimeout(12_000);
await shot('01-overview');
const overview = await webviewText('Today');
check('the Overview board is the first screen', overview.includes('Today'));
check('the board names decisions waiting', /waiting for you/i.test(overview), overview.split('\n').filter((l) => /waiting|decision/i.test(l)).slice(0, 2).join(' | '));

// Awaiting you: three proposals.
const awaiting = pane('Awaiting');
await awaiting.waitFor({ timeout: 30_000 }).catch(() => undefined);
for (const id of ['res-l-24117', 'tp-stuttgart-lyon', 'ltb-sp-3307']) {
  check(`Awaiting you holds ${id}`, (await awaiting.getByRole('treeitem', { name: new RegExp(id) }).count()) > 0);
}
const inbox = pane('Inbox');
for (const mail of [/Delivery delay PO-4500182/, /Booking cut-off week 41/, /Product discontinuation notice/]) {
  check(`Inbox holds ${mail.source}`, (await inbox.getByRole('treeitem', { name: mail }).count()) > 0);
}
await shot('02-sidebar');

// Review the supplier proposal.
await awaiting.getByRole('treeitem', { name: /res-l-24117/ }).first().click();
await page.waitForTimeout(2500);
await shot('03-review-picker');
await page.keyboard.press('Escape');

// Approve each proposal (the check mark on the row), then look at the approved records.
for (const id of ['res-l-24117', 'tp-stuttgart-lyon', 'ltb-sp-3307']) {
  const item = awaiting.getByRole('treeitem', { name: new RegExp(id) }).first();
  if (!(await item.count())) continue;
  await item.hover();
  await item.getByRole('button', { name: /Promote/ }).first().click();
  await page.waitForTimeout(3500);
}
await shot('04-after-approve');
check('Awaiting you is empty after the three approvals', (await awaiting.getByRole('treeitem', { name: /res-l-24117|tp-stuttgart-lyon|ltb-sp-3307/ }).count()) === 0);

// Open the Overview again: what it says now.
await page.keyboard.press('F1');
await page.keyboard.type('>Escurel: Open overview');
await page.waitForTimeout(800);
await page.keyboard.press('Enter');
await page.waitForTimeout(6000);
await shot('05-overview-after');
await browser.close();
console.log(failed ? `\n${failed} check(s) FAILED` : '\nall checks held');
process.exit(failed ? 1 : 0);
