// node --test deploy/web-workbench/login/
// The login/error pages must stay the Escurel Calm look: generated from the theme, every token they use is
// defined, and the text/button colours hold WCAG AA.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build, lightTokens } from './gen-global-css.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const read = (f) => readFileSync(join(here, f), 'utf8');
const tokens = new Map(lightTokens());

test('global.css is the theme-derived output, byte for byte', () => {
  assert.equal(read('global.css'), build());
});

test('every --vscode-* colour a page uses is defined by the Calm theme or has a fallback in global.base.css', () => {
  const used = new Set(
    [read('global.base.css'), read('login.css'), read('error.css')]
      .join('\n')
      .match(/--vscode-[\w-]+/g)
      .filter((n) => !['--vscode-font-family', '--vscode-font-size'].includes(n)),
  );
  const base = read('global.base.css');
  for (const n of used) {
    // A name outside the theme is only allowed inside a var(..., fallback) chain of the --w-* aliases.
    if (!tokens.has(n)) assert.ok(new RegExp(`var\\(\\s*${n}\\s*,`).test(base), `${n} is neither in the theme nor given a fallback`);
  }
});

const lum = (h) => {
  const c = [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16) / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
};
const ratio = (a, b) => {
  const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
};
const t = (n) => tokens.get(`--vscode-${n}`);

test('text, muted text, button and link colours hold WCAG AA on the widget surface', () => {
  const widget = t('editorWidget-background');
  assert.ok(ratio(t('foreground'), widget) >= 4.5, 'foreground');
  assert.ok(ratio(t('descriptionForeground'), widget) >= 4.5, 'muted text');
  assert.ok(ratio(t('button-foreground'), t('button-background')) >= 4.5, 'button label');
  assert.ok(ratio(t('button-foreground'), t('button-hoverBackground')) >= 4.5, 'button label on hover');
  assert.ok(ratio(t('textLink-foreground'), widget) >= 4.5, 'link');
  assert.ok(ratio(t('foreground'), t('input-background')) >= 4.5, 'input text');
  assert.ok(ratio(t('focusBorder'), t('input-background')) >= 3, 'focus ring');
});
