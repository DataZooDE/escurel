// Launch a bare VS Code (own profile) under Xvfb with the focus settings; report what the DOM shows.
import { chromium } from '@playwright/test';
import { spawn, execFileSync } from 'node:child_process';
import { mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { createServer } from 'node:net';
const dir = process.env.PROBE_DIR;
const settings = JSON.parse(process.env.PROBE_SETTINGS);
const out = process.env.PROBE_OUT;
rmSync(dir, { recursive: true, force: true });
mkdirSync(`${dir}/profile/User`, { recursive: true });
mkdirSync(`${dir}/ws`, { recursive: true });
writeFileSync(`${dir}/profile/User/settings.json`, JSON.stringify({
  'security.workspace.trust.enabled': false, 'workbench.startupEditor': 'none',
  'telemetry.telemetryLevel': 'off', 'update.mode': 'none', 'chat.disableAIFeatures': true, ...settings }));
if (process.env.PROBE_PINS) {
  mkdirSync(`${dir}/profile/User/globalStorage`, { recursive: true });
  execFileSync('sqlite3', [`${dir}/profile/User/globalStorage/state.vscdb`, `CREATE TABLE IF NOT EXISTS ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB); INSERT INTO ItemTable VALUES ('workbench.activity.pinnedViewlets2', '${process.env.PROBE_PINS}');`]);
}
const port = await new Promise((r) => { const s = createServer(); s.listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => r(p)); }); });
const xvfb = spawn('Xvfb', ['-displayfd', '3', '-screen', '0', '1500x900x24'], { stdio: ['ignore', 'ignore', 'ignore', 'pipe'] });
const display = await new Promise((r) => { let b = ''; xvfb.stdio[3].on('data', (d) => { b += d; if (b.includes('\n')) r(':' + b.trim()); }); });
const code = spawn('code', ['--user-data-dir', `${dir}/profile`, '--extensions-dir', `${dir}/ext`, `--remote-debugging-port=${port}`, '--ozone-platform=x11', '--new-window', `${dir}/ws`, ...(process.env.PROBE_ARGS ? process.env.PROBE_ARGS.split(' ') : [])], { env: { ...process.env, DISPLAY: display, WAYLAND_DISPLAY: '' }, stdio: 'ignore', detached: true });
let browser;
for (let i = 0; i < 60 && !browser; i++) { try { browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`); } catch { await new Promise((r) => setTimeout(r, 500)); } }
let page; for (let i = 0; i < 120 && !page; i++) { page = browser.contexts()[0]?.pages().find((p) => /workbench/.test(p.url())); if (!page) await new Promise((r) => setTimeout(r, 500)); }
await page.waitForSelector('.monaco-workbench', { timeout: 60000 });
await new Promise((r) => setTimeout(r, 4000));
const vis = (sel) => page.evaluate((s) => { const e = document.querySelector(s); if (!e) return 'absent'; const r = e.getBoundingClientRect(); const cs = getComputedStyle(e); return `${r.width|0}x${r.height|0} display=${cs.display} vis=${cs.visibility}`; }, sel);
const res = {
  title: await page.title(),
  titlebar: await vis('.part.titlebar'), menubar: await vis('.menubar'), commandCenter: await vis('.command-center'),
  statusbar: await vis('.part.statusbar'), activitybar: await vis('.part.activitybar'),
  activityIcons: await page.evaluate(() => [...document.querySelectorAll('.part.activitybar .action-label')].map((e) => e.getAttribute('aria-label') || e.title)),
  breadcrumbs: await vis('.breadcrumbs-control'), minimap: await vis('.minimap'), tabs: await vis('.tabs-container'),
  windowTitleText: await page.evaluate(() => document.querySelector('.window-title')?.textContent ?? null),
};
console.log(JSON.stringify(res, null, 1));
await page.screenshot({ path: out });
await browser.close(); try { process.kill(-code.pid); } catch {} xvfb.kill();
