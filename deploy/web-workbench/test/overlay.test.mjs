// The overlay contract (OVERLAY.md): loader, validation, merges. No docker, no code-server.
// node --test deploy/web-workbench/test deploy/web-workbench/login
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, symlinkSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import {
  loadOverlay, mergeSettings, composeWorkbenchCss, loginTokensCss, patchProduct, OverlayError,
} from '../overlay.mjs';

function overlay(files) {
  const dir = mkdtempSync(join(tmpdir(), 'overlay-'));
  for (const [name, body] of Object.entries(files)) {
    mkdirSync(join(dir, name, '..'), { recursive: true });
    writeFileSync(join(dir, name), typeof body === 'string' ? body : JSON.stringify(body));
  }
  return dir;
}
const problems = (dir) => {
  try {
    loadOverlay(dir);
  } catch (e) {
    assert.ok(e instanceof OverlayError, String(e));
    return e.problems.join('\n');
  }
  assert.fail('expected the overlay to be rejected');
};

test('an absent overlay directory is a no-op', () => {
  const o = loadOverlay(join(tmpdir(), 'no-such-overlay-dir'));
  assert.equal(o.present, false);
  assert.deepEqual(o.settings, {});
  assert.equal(o.workbenchCss, '');
  assert.deepEqual(mergeSettings({ a: 1 }, o.settings), { a: 1 });
  assert.equal(composeWorkbenchCss('base{}', o.workbenchCss), 'base{}');
  assert.equal(loginTokensCss(o.loginTokens), '');
  assert.deepEqual(patchProduct({ nameShort: 'x', k: 1 }, o.product), { nameShort: 'x', k: 1 });
});

test('an empty overlay directory is a no-op too', () => {
  const o = loadOverlay(overlay({}));
  assert.equal(o.present, true);
  assert.deepEqual(o.extensions, []);
});

test('settings are merged over the base, later wins, and the inputs are not mutated', () => {
  const base = { 'window.title': 'Escurel', 'editor.minimap.enabled': false };
  const o = loadOverlay(overlay({ 'settings.json': { 'window.title': 'Acme Workbench', '[markdown]': { 'editor.wordWrap': 'on' } } }));
  const merged = mergeSettings(base, o.settings);
  assert.equal(merged['window.title'], 'Acme Workbench');
  assert.equal(merged['editor.minimap.enabled'], false);
  assert.deepEqual(merged['[markdown]'], { 'editor.wordWrap': 'on' });
  assert.equal(base['window.title'], 'Escurel');
});

test('invalid JSON fails with the file name', () => {
  assert.match(problems(overlay({ 'settings.json': '{ nope' })), /settings\.json: not valid JSON/);
  assert.match(problems(overlay({ 'settings.json': '[1]' })), /settings\.json: must be a JSON object/);
});

test('settings keys that name trust or security are refused, with the key in the message', () => {
  for (const key of [
    'security.workspace.trust.enabled', 'window.confirmBeforeClose.trust', 'terminal.integrated.defaultProfile.linux',
    'extensions.gallery', 'update.mode', 'telemetry.telemetryLevel', 'escurel.gatewayUrl', 'escurel.auth.issuer',
  ]) {
    assert.match(problems(overlay({ 'settings.json': { [key]: true } })), new RegExp(`settings\\.json: .*${key.replaceAll('.', '\\.')}`), key);
  }
});

test('a settings key that is not a setting name is refused', () => {
  assert.match(problems(overlay({ 'settings.json': { 'has space': 1 } })), /has space/);
});

test('oversized files are refused', () => {
  assert.match(problems(overlay({ 'workbench.css': 'a{}'.repeat(100_000) })), /workbench\.css: .* exceeds/);
});

test('an unknown entry is refused', () => {
  assert.match(problems(overlay({ 'setings.json': '{}' })), /unknown entry setings\.json/);
});

test('a symlink is refused', () => {
  const dir = overlay({});
  symlinkSync('/etc/passwd', join(dir, 'settings.json'));
  assert.match(problems(dir), /settings\.json: symlink/);
  rmSync(dir, { recursive: true });
});

test('workbench CSS follows the base CSS, in that order, with a marker', () => {
  const o = loadOverlay(overlay({ 'workbench.css': '.x{color:red}' }));
  const out = composeWorkbenchCss('.base{}\n', o.workbenchCss);
  assert.ok(out.startsWith('.base{}\n'));
  assert.ok(out.indexOf('escurel overlay: workbench.css') > out.indexOf('.base{}'));
  assert.ok(out.endsWith('.x{color:red}\n'));
});

test('CSS may not import or load external resources', () => {
  assert.match(problems(overlay({ 'workbench.css': '@import "x.css";' })), /workbench\.css: @import/);
  assert.match(problems(overlay({ 'workbench.css': 'a{background:url(https://x.test/a.png)}' })), /workbench\.css: external url/);
  assert.match(problems(overlay({ 'login.css': 'a{background:url(//x.test/a.png)}' })), /login\.css: external url/);
});

test('login.css may not carry colour literals: colours live in login-tokens.json', () => {
  for (const lit of ['#fff', '#0a7f8e', 'rgb(1,2,3)', 'rgba(1,2,3,.5)', 'hsl(10 20% 30%)']) {
    assert.match(problems(overlay({ 'login.css': `a{color:${lit}}` })), /login\.css: colour literal/, lit);
  }
  loadOverlay(overlay({ 'login.css': 'a{color:var(--vscode-foreground);border-color:transparent}' }));
});

test('login-tokens.json becomes a :root block of --vscode-* variables', () => {
  const o = loadOverlay(overlay({
    'login-tokens.json': { productName: 'Acme Workbench', colors: { 'button.background': '#0a7f8e', 'textLink.foreground': '#0a7f8eff' } },
  }));
  const css = loginTokensCss(o.loginTokens);
  assert.match(css, /:root \{[^}]*--vscode-button-background: #0a7f8e;/);
  assert.match(css, /--vscode-textLink-foreground: #0a7f8eff;/);
  assert.equal(o.loginTokens.productName, 'Acme Workbench');
});

test('login tokens are validated: unknown colour id, non-hex value, bad product name', () => {
  assert.match(problems(overlay({ 'login-tokens.json': { colors: { 'made.up': '#fff' } } })), /login-tokens\.json: .*made\.up/);
  assert.match(problems(overlay({ 'login-tokens.json': { colors: { 'button.background': 'teal' } } })), /login-tokens\.json: .*button\.background/);
  assert.match(problems(overlay({ 'login-tokens.json': { productName: '<b>x</b>' } })), /login-tokens\.json: productName/);
  assert.match(problems(overlay({ 'login-tokens.json': { productName: 'x', extra: 1 } })), /login-tokens\.json: unknown key extra/);
});

test('product.json patch is merged over code-server product.json; only name keys are allowed', () => {
  const o = loadOverlay(overlay({ 'product.json': { nameShort: 'Acme', nameLong: 'Acme Workbench' } }));
  const out = patchProduct({ nameShort: 'code-server', nameLong: 'code-server', extensionsGallery: { x: 1 } }, o.product);
  assert.deepEqual(out, { nameShort: 'Acme', nameLong: 'Acme Workbench', extensionsGallery: { x: 1 } });
  assert.match(problems(overlay({ 'product.json': { extensionsGallery: {} } })), /product\.json: .*extensionsGallery/);
});

test('extensions and bootstrap scripts are listed in order; only the right suffix is accepted', () => {
  const o = loadOverlay(overlay({
    'extensions/b.vsix': 'x', 'extensions/a.vsix': 'x', 'bootstrap.d/20-b.js': 'exports.activate=async()=>{}', 'bootstrap.d/10-a.js': 'exports.activate=async()=>{}',
  }));
  assert.deepEqual(o.extensions.map((p) => p.split('/').pop()), ['a.vsix', 'b.vsix']);
  assert.deepEqual(o.bootstrap.map((p) => p.split('/').pop()), ['10-a.js', '20-b.js']);
  assert.match(problems(overlay({ 'extensions/a.zip': 'x' })), /extensions\/a\.zip/);
  assert.match(problems(overlay({ 'bootstrap.d/a.sh': 'x' })), /bootstrap\.d\/a\.sh/);
});

test('bootstrap scripts that spawn, read the environment, use the network or eval are refused', () => {
  for (const src of ["require('child_process')", 'process.env.X', "require('node:https')", 'fetch("x")', 'eval("1")', 'new Function("1")']) {
    assert.match(problems(overlay({ 'bootstrap.d/a.js': src })), /bootstrap\.d\/a\.js: forbidden/, src);
  }
});

test('assets: allowed suffixes only', () => {
  assert.deepEqual(loadOverlay(overlay({ 'assets/Font.woff2': 'x' })).assets.map((p) => p.split('/').pop()), ['Font.woff2']);
  assert.match(problems(overlay({ 'assets/run.sh': 'x' })), /assets\/run\.sh/);
});

test('all problems are reported at once', () => {
  const p = problems(overlay({ 'settings.json': '{', 'product.json': '[]', 'zzz': 'x' }));
  assert.match(p, /settings\.json/);
  assert.match(p, /product\.json/);
  assert.match(p, /zzz/);
});

test('the shipped example overlay is valid under the contract it documents', async () => {
  const { fileURLToPath } = await import('node:url');
  const dir = fileURLToPath(new URL('../examples/overlay-acme', import.meta.url));
  const o = loadOverlay(dir);
  assert.equal(o.loginTokens.productName, 'Acme Workbench');
  assert.equal(o.bootstrap.length, 1);
});
