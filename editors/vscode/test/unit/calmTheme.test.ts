import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const root = join(__dirname, '..', '..');
const manifest = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')) as {
  contributes: { themes: { label: string; uiTheme: string; path: string }[] };
};
const entry = manifest.contributes.themes.find((t) => t.label === 'Escurel Calm');
const theme = JSON.parse(readFileSync(join(root, entry!.path), 'utf8')) as {
  type: string;
  colors: Record<string, string>;
};

/** WCAG 2.x relative luminance and contrast ratio of two #rrggbb colours. */
function channel(v: number): number {
  const s = v / 255;
  return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}
function luminance(hex: string): number {
  const n = parseInt(hex.slice(1, 7), 16);
  return (
    0.2126 * channel((n >> 16) & 255) + 0.7152 * channel((n >> 8) & 255) + 0.0722 * channel(n & 255)
  );
}
function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi! + 0.05) / (lo! + 0.05);
}
const c = (key: string): string => {
  const v = theme.colors[key];
  if (!v || !/^#[0-9a-f]{6}$/i.test(v))
    throw new Error(`${key} must be a #rrggbb colour, got ${v}`);
  return v;
};

describe('the Escurel Calm theme', () => {
  it('does not set contrastBorder, the high-contrast border: webviews read it as "high contrast is on"', () => {
    // The thread canvas drew its connectors and card borders with contrastBorder when a theme set it. The
    // calm theme set a pale rule there, and the connectors all but vanished on the white canvas.
    expect(theme.colors).not.toHaveProperty('contrastBorder');
  });

  it('is shipped as a light theme named in the manifest', () => {
    expect(entry?.uiTheme).toBe('vs');
    expect(theme.type).toBe('light');
  });

  it.each([
    ['foreground', 'editor.background', 4.5],
    ['descriptionForeground', 'editor.background', 4.5],
    ['sideBar.foreground', 'sideBar.background', 4.5],
    ['errorForeground', 'editor.background', 4.5],
    ['editorWarning.foreground', 'editor.background', 4.5],
    ['textLink.foreground', 'editor.background', 4.5],
    ['button.foreground', 'button.background', 4.5],
    ['badge.foreground', 'badge.background', 4.5],
    ['charts.green', 'editor.background', 4.5],
    ['charts.orange', 'editor.background', 4.5],
    ['charts.blue', 'editor.background', 4.5],
    ['charts.purple', 'editor.background', 4.5],
    ['list.activeSelectionForeground', 'list.activeSelectionBackground', 4.5],
    ['focusBorder', 'editor.background', 3],
  ])('%s on %s reads at %s:1 or better', (fg, bg, min) => {
    expect(contrast(c(fg), c(bg))).toBeGreaterThanOrEqual(min);
  });
});
