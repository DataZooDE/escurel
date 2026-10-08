// Walks the S2D demo's click path (editors/vscode/demo/s2d/REHEARSAL.md) in the web workbench in a real (headless)
// Chromium: login, the Overview board, the three stories (mail, record, review, approve), the two brain-teasers. It
// takes a screenshot per step (to be READ by a person) and checks the numbers the rehearsal names.
//
//   node s2d-tour.mjs <base-url> <password-file> <out-dir>
//
// The password is read from a FILE (never an argument, never printed). Needs `playwright-core` (editors/vscode/node_modules)
// and a Chromium (CHROMIUM=/usr/bin/chromium by default). Run from the repository root. It APPROVES the proposals: run it
// against a throwaway instance (or `./s2d-reset.sh` afterwards).
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
/** The text of the first webview frame that contains every needle (webviews are nested iframes). */
async function webviewText(needles, timeout = 30_000) {
  const want = [].concat(needles);
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    for (const f of frames()) {
      // The webviews are Lit components: their text lives in shadow roots, which innerText does not enter.
      const t = await f
        .evaluate(() => {
          const walk = (n) =>
            n.nodeType === 3
              ? `${n.textContent} `
              : [...(n.shadowRoot?.childNodes ?? []), ...n.childNodes].map(walk).join('');
          return document.body ? walk(document.body) : '';
        })
        .catch(() => '');
      if (want.every((n) => t.includes(n))) return t;
    }
    await page.waitForTimeout(500);
  }
  return '';
}

/** The Knowledge tree is virtualised: scroll from the top until the row is rendered. */
async function knowledgeRow(name) {
  const k = pane('Knowledge');
  const row = k.getByRole('treeitem', { name });
  const list = k.locator('.monaco-list').first();
  await page.mouse.move(1000, 700);
  await list.hover();
  await list.focus();
  await page.keyboard.press('Home');
  await page.mouse.wheel(0, -10_000);
  await page.waitForTimeout(400);
  for (let i = 0; i < 40 && (await row.count()) === 0; i += 1) {
    await page.mouse.wheel(0, 120);
    await page.waitForTimeout(250);
  }
  if ((await row.count()) > 0) return row.first();
  const names = await k.getByRole('treeitem').evaluateAll((els) => els.map((e) => e.getAttribute('aria-label')));
  console.log(`      (no Knowledge row ${name}; rendered rows: ${names.slice(0, 12).join(' | ')})`);
  return undefined;
}
async function openRow(skillId, rowRe) {
  for (let attempt = 0; attempt < 6; attempt += 1) {
    const skill = await knowledgeRow(new RegExp(`skill (?:\\(inferred\\) )?${skillId},`));
    if (!skill) break;
    if ((await skill.getAttribute('aria-expanded')) !== 'true') await skill.click();
    await page.mouse.move(1000, 700);
    await page.waitForTimeout(1500);
    const row = await knowledgeRow(rowRe);
    if (row) {
      await row.click();
      return true;
    }
  }
  return false;
}
async function approve(re) {
  const item = pane('Awaiting').getByRole('treeitem', { name: re }).first();
  await item.hover();
  await item.getByRole('button', { name: /Promote/ }).first().click();
  await page.waitForTimeout(3500);
}

await page.goto(`${base}/login`);
await page.fill('input[type=password]', password);
await page.click('input[type=submit], button[type=submit]');
await page.waitForSelector('.monaco-workbench', { timeout: 90_000 });
await page.waitForTimeout(12_000);
await shot('01-overview');
const board = await webviewText(['Today', 'waiting for you']);
check('the Overview board is the first screen and names what waits', board.length > 0, board.split('\n').filter((l) => /waiting/i.test(l)).slice(0, 1).join(''));
for (const id of ['res-l-24117', 'tp-stuttgart-lyon', 'ltb-sp-3307'])
  check(`Awaiting you holds ${id}`, (await pane('Awaiting').getByRole('treeitem', { name: new RegExp(id) }).count()) > 0);
for (const mail of [/Delivery delay PO-4500182/, /Booking cut-off week 41/, /Product discontinuation notice/])
  check(`Inbox holds ${mail.source}`, (await pane('Inbox').getByRole('treeitem', { name: mail }).count()) > 0);

// Story 1: the supplier mail, its record with the impact, the proposal, the approval.
await pane('Inbox').getByRole('treeitem', { name: /Delivery delay PO-4500182/ }).first().click();
await page.waitForTimeout(5000);
await shot('02-s1-mail-thread');
check('story 1: the record opens', await openRow('supplier_exception', /l-24117/));
const impact = await webviewText(['Orders late'], 40_000);
await shot('03-s1-record-impact');
check('story 1: the record shows the impact (4 orders late)', /Orders late\s*4|4\s*Orders late/i.test(impact.replace(/\n+/g, ' ')), impact.replace(/\n+/g, ' ').slice(0, 0));
await pane('Awaiting').getByRole('treeitem', { name: /res-l-24117/ }).first().click();
await page.waitForTimeout(2500);
await shot('04-s1-review-picker');
await page.keyboard.press('Escape');
await approve(/res-l-24117/);
await shot('05-s1-approved');
check('story 1: approved (the row left Awaiting you)', (await pane('Awaiting').getByRole('treeitem', { name: /res-l-24117/ }).count()) === 0);

// Story 2: transport consolidation.
await pane('Inbox').getByRole('treeitem', { name: /Booking cut-off week 41/ }).first().click();
await page.waitForTimeout(4000);
await shot('06-s2-mail-thread');
await approve(/tp-stuttgart-lyon/);
check('story 2: approved', (await pane('Awaiting').getByRole('treeitem', { name: /tp-stuttgart-lyon/ }).count()) === 0);
check('story 2: the plan opens', await openRow('transport_plan', /tp-stuttgart-lyon/));
const plan = await webviewText(['SH-77003'], 40_000);
await shot('07-s2-plan');
check('story 2: SH-77001..3 ship together, SH-77004 stays', /SH-77001/.test(plan) && /SH-77003/.test(plan) && /SH-77004/.test(plan));

// Story 3: last-time buy.
await pane('Inbox').getByRole('treeitem', { name: /Product discontinuation notice/ }).first().click();
await page.waitForTimeout(4000);
await shot('08-s3-mail-thread');
await approve(/ltb-sp-3307/);
check('story 3: approved', (await pane('Awaiting').getByRole('treeitem', { name: /ltb-sp-3307/ }).count()) === 0);
check('story 3: the decision opens', await openRow('ltb_decision', /ltb-sp-3307/));
const ltb = await webviewText(['634'], 40_000);
await shot('09-s3-decision');
check('story 3: 634 units, EUR 748,120, split 349 / 159 / 127', ['634', '748,120', '349', '159', '127'].every((n) => ltb.includes(n)));

// Teaser 1: the price of the last percent (Preview with parameters on the query page).
const methods = await knowledgeRow(/^folder logistics\/methods$/);
if (methods && (await methods.getAttribute('aria-expanded')) !== 'true') await methods.click();
check('teaser 1: the methods open', await openRow('query', /Ltb quantity/));
await page.waitForTimeout(2000);
const q = await knowledgeRow(/ltb_quantity/);
if (q) {
  await q.hover();
  await q.getByRole('button', { name: /Preview with parameters/ }).click();
  const input = page.locator('.quick-input-widget input.input').first();
  await page.locator('.quick-input-widget').getByText(/^part \(text, required\)/).waitFor({ timeout: 15_000 }).catch(() => undefined);
  await input.fill('SP-3307');
  await page.keyboard.press('Enter');
  await page.locator('.quick-input-widget').getByText(/^service level \(text, required\)/).waitFor({ timeout: 15_000 }).catch(() => undefined);
  await input.fill('0.95, 0.98, 0.99');
  await page.keyboard.press('Enter');
  await page.waitForTimeout(4000);
  const preview = await webviewText(['748,120'], 40_000);
  await shot('10-t1-price-of-the-last-percent');
  // The built-in markdown preview's text is not always reachable from here: READ the screenshot (10-...png) when this says so.
  console.log(['634', '666', '688', '748,120', '785,880', '811,840'].every((n) => preview.includes(n)) ? 'PASS  teaser 1: 634 / 666 / 688 units and EUR 748,120 / 785,880 / 811,840' : 'NOTE  teaser 1: the preview text was not read back; look at 10-t1-price-of-the-last-percent.png');
} else check('teaser 1: the ltb_quantity row exists', false);

// Teaser 2: one warehouse, two decisions: both records side by side.
await openRow('exception_resolution', /res-l-24117/);
await page.waitForTimeout(3000);
await page.keyboard.press('Control+\\');
await openRow('ltb_decision', /ltb-sp-3307/);
await page.waitForTimeout(4000);
await shot('11-t2-one-warehouse-two-decisions');

await browser.close();
console.log(failed ? `\n${failed} check(s) FAILED` : '\nall checks held');
process.exit(failed ? 1 : 0);
