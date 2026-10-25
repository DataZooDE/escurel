import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { THEME_CSS, THEME_TOKENS } from '../../src/shared/themeTokens';

// The tokens a companion gets on :root must be the ones the core webviews declare.
describe('theme tokens', () => {
  const squash = (t: string) => t.replace(/\s+/g, '');
  const source = squash(readFileSync('webview/shared/theme.css.ts', 'utf8'));

  it('match the declarations in webview/shared/theme.css.ts', () => {
    for (const [name, value] of Object.entries(THEME_TOKENS)) {
      expect(source, name).toContain(squash(`${name}: ${value};`));
    }
  });

  it('are theme variables only', () => {
    expect(THEME_CSS).not.toMatch(/#[0-9a-f]{3,8}\b|rgb|hsl/i);
    for (const v of Object.values(THEME_TOKENS)) expect(v).toMatch(/^var\(--vscode-/);
  });
});
