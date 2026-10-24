// `escurel-overlay-install` against a fake code-server tree, and render-settings / entrypoint against a real overlay.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { loadOverlay } from '../overlay.mjs';
import { install } from '../overlay-install.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const wb = join(here, '..');

function tree() {
  const root = mkdtempSync(join(tmpdir(), 'cs-'));
  const t = {
    root,
    workbenchCss: join(root, 'workbench', 'workbench.css'),
    productJson: join(root, 'product.json'),
    pagesDir: join(root, 'pages'),
    installed: [],
    installExtension(f) { this.installed.push(f); },
  };
  mkdirSync(join(root, 'workbench'));
  mkdirSync(t.pagesDir);
  writeFileSync(t.workbenchCss, '.stock{}\n');
  writeFileSync(t.productJson, JSON.stringify({ nameShort: 'code-server', nameLong: 'code-server', keep: 1 }));
  writeFileSync(join(t.pagesDir, 'global.css'), ':root{--vscode-button-background:#111111;}\n');
  writeFileSync(join(t.pagesDir, 'login.css'), '.login-form{}\n');
  writeFileSync(join(t.pagesDir, 'login.html'), '<div class="titlebar"><svg></svg>\n      <span>Escurel</span>\n</div>');
  return t;
}
function overlayDir(files) {
  const dir = mkdtempSync(join(tmpdir(), 'ov-'));
  for (const [n, b] of Object.entries(files)) {
    mkdirSync(join(dir, n, '..'), { recursive: true });
    writeFileSync(join(dir, n), typeof b === 'string' ? b : JSON.stringify(b));
  }
  return dir;
}
const read = (f) => readFileSync(f, 'utf8');

test('install without an overlay changes nothing but still asserts the targets exist', () => {
  const t = tree();
  install(loadOverlay(join(t.root, 'none')), t);
  assert.equal(read(t.workbenchCss), '.stock{}\n');
  assert.equal(t.installed.length, 0);
  const broken = { ...tree(), workbenchCss: join(t.root, 'moved.css') };
  assert.throws(() => install(loadOverlay(join(t.root, 'none')), broken), /workbench\.css .* not found/);
});

test('install applies CSS, product patch, login theme, product name, assets and extensions', () => {
  const t = tree();
  const dir = overlayDir({
    'workbench.css': '.acme{color:red}',
    'login.css': '.login-form{border-color:var(--vscode-focusBorder)}',
    'login-tokens.json': { productName: 'Acme Workbench', colors: { 'button.background': '#0a7f8e' } },
    'product.json': { nameShort: 'Acme', nameLong: 'Acme Workbench' },
    'assets/Acme.woff2': 'font',
    'extensions/acme.vsix': 'x',
  });
  install(loadOverlay(dir), t);
  assert.match(read(t.workbenchCss), /^\.stock\{\}\n[\s\S]*escurel overlay: workbench\.css[\s\S]*\.acme\{color:red\}\n$/);
  assert.deepEqual(JSON.parse(read(t.productJson)), { nameShort: 'Acme', nameLong: 'Acme Workbench', keep: 1 });
  const g = read(join(t.pagesDir, 'global.css'));
  assert.ok(g.indexOf('#111111') < g.indexOf('--vscode-button-background: #0a7f8e'), 'token override comes after the base value');
  assert.match(read(join(t.pagesDir, 'login.css')), /\.login-form\{\}\n[\s\S]*var\(--vscode-focusBorder\)/);
  assert.match(read(join(t.pagesDir, 'login.html')), /<span>Acme Workbench<\/span>/);
  assert.ok(existsSync(join(t.root, 'workbench', 'Acme.woff2')) && existsSync(join(t.pagesDir, 'Acme.woff2')));
  assert.deepEqual(t.installed.map((f) => f.split('/').pop()), ['acme.vsix']);
});

test('a second install is refused instead of appending twice', () => {
  const t = tree();
  const o = loadOverlay(overlayDir({ 'workbench.css': '.a{}' }));
  install(o, t);
  assert.throws(() => install(o, t), /already applied/);
});

test('a login page without the product name placeholder fails the install', () => {
  const t = tree();
  writeFileSync(join(t.pagesDir, 'login.html'), '<p>custom</p>');
  assert.throws(() => install(loadOverlay(overlayDir({ 'login-tokens.json': { productName: 'Acme' } })), t), /login\.html .*titlebar/);
});

function render(overlay) {
  const out = mkdtempSync(join(tmpdir(), 'ud-'));
  const common = join(out, 'common.json');
  writeFileSync(common, '{"editor.fontSize": 14}');
  const r = spawnSync(process.execPath, [join(wb, 'render-settings.mjs'), common, join(wb, 'settings.base.json'), join(wb, 'keybindings.json'), out, ...(overlay ? [overlay] : [])], {
    env: { ...process.env, WORKBENCH_GATEWAY_URL: 'http://gw:8080' }, encoding: 'utf8',
  });
  return { r, settings: r.status === 0 ? JSON.parse(read(join(out, 'User', 'settings.json'))) : null };
}

test('render-settings: no overlay argument or an absent directory leaves the base settings', () => {
  const a = render();
  const b = render(join(tmpdir(), 'nope-overlay'));
  assert.equal(a.r.status, 0);
  assert.deepEqual(a.settings, b.settings);
  assert.equal(a.settings['window.title'], 'Escurel');
  assert.equal(a.settings['escurel.gatewayUrl'], 'http://gw:8080');
});

test('render-settings: overlay settings win over base, the environment wins over the overlay', () => {
  const { r, settings } = render(overlayDir({ 'settings.json': { 'window.title': 'Acme Workbench', 'workbench.activityBar.location': 'default' } }));
  assert.equal(r.status, 0, r.stderr);
  assert.equal(settings['window.title'], 'Acme Workbench');
  assert.equal(settings['workbench.activityBar.location'], 'default');
  assert.equal(settings['escurel.gatewayUrl'], 'http://gw:8080');
});

test('render-settings: an invalid overlay stops with exit 64 and a clear message', () => {
  const { r } = render(overlayDir({ 'settings.json': { 'security.workspace.trust.enabled': false } }));
  assert.equal(r.status, 64);
  assert.match(r.stderr, /escurel-web: overlay: .*security\.workspace\.trust\.enabled/);
});

test('entrypoint: an invalid overlay refuses to start (exit 64) before code-server is reached', () => {
  const opt = mkdtempSync(join(tmpdir(), 'opt-'));
  for (const f of ['overlay.mjs', 'render-settings.mjs', 'settings.base.json', 'keybindings.json']) writeFileSync(join(opt, f), read(join(wb, f)));
  mkdirSync(join(opt, 'login'));
  writeFileSync(join(opt, 'login', 'used-colors.mjs'), read(join(wb, 'login', 'used-colors.mjs')));
  writeFileSync(join(opt, 'settings.common.json'), '{}');
  const bad = overlayDir({ 'product.json': '[]' });
  const r = spawnSync('sh', [join(wb, 'entrypoint.sh')], {
    env: { PATH: process.env.PATH, PASSWORD: 'a-long-enough-pass', WORKBENCH_OPT: opt, WORKBENCH_OVERLAY_DIR: bad, WORKBENCH_NODE: process.execPath, WORKBENCH_DATA_DIR: join(opt, 'data'), WORKBENCH_GATEWAY_URL: 'http://gw:8080' },
    encoding: 'utf8',
  });
  assert.equal(r.status, 64, r.stderr);
  assert.match(r.stderr, /escurel-web: overlay: .*product\.json/);
});

test('the install command runs through its own entry point and ends with a real error, not an unsettled import', () => {
  // The Dockerfile calls `overlay.mjs install`. A circular dynamic import once left it hanging with exit 13 in the
  // image build, which the function-level tests above could not see. Here there is no code-server tree, so the
  // install must stop with the documented failure (64) and name the missing target.
  const r = spawnSync(process.execPath, [join(wb, 'overlay.mjs'), 'install', join(wb, 'examples', 'overlay-acme')], {
    encoding: 'utf8',
  });
  assert.equal(r.status, 64, `${r.status}: ${r.stderr}`);
  assert.match(r.stderr, /escurel-web: .*(workbench\.css|product\.json|login|code-server)/i);
  assert.doesNotMatch(r.stderr, /unsettled top-level await/);
});
