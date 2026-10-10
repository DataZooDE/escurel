// `gen-calm-tokens.mjs --check` is what keeps test/visual/tokens/calm.css (the Calm tokens the visual baselines
// are rendered with) from drifting away from themes/escurel-calm-color-theme.json.
import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const root = join(__dirname, '../..');
const script = join(root, 'scripts/gen-calm-tokens.mjs');

/** A scratch copy of the three files the script reads/writes, so a test can plant drift. */
function scratch(): string {
  const dir = mkdtempSync(join(tmpdir(), 'escurel-calm-'));
  mkdirSync(join(dir, 'themes'));
  mkdirSync(join(dir, 'test/visual/tokens'), { recursive: true });
  cpSync(
    join(root, 'themes/escurel-calm-color-theme.json'),
    join(dir, 'themes/escurel-calm-color-theme.json'),
  );
  for (const f of ['light.css', 'calm.css'])
    cpSync(join(root, 'test/visual/tokens', f), join(dir, 'test/visual/tokens', f));
  return dir;
}
const check = (cwd: string) =>
  spawnSync(process.execPath, [script, '--check'], { cwd, encoding: 'utf8' });

describe('gen-calm-tokens --check', () => {
  it('passes on the committed theme and tokens', () => {
    const r = check(root);
    expect(r.status, r.stderr).toBe(0);
    expect(r.stdout).toContain('in sync');
  });

  it('fails when the theme changed but calm.css was not regenerated', () => {
    const dir = scratch();
    const themePath = join(dir, 'themes/escurel-calm-color-theme.json');
    const theme = JSON.parse(readFileSync(themePath, 'utf8')) as { colors: Record<string, string> };
    theme.colors['editor.background'] = '#123456';
    writeFileSync(themePath, JSON.stringify(theme));
    const r = check(dir);
    expect(r.status).toBe(1);
    expect(r.stderr).toContain('is stale');
  });

  it('writing (no --check) brings it back in sync', () => {
    const dir = scratch();
    const themePath = join(dir, 'themes/escurel-calm-color-theme.json');
    const theme = JSON.parse(readFileSync(themePath, 'utf8')) as { colors: Record<string, string> };
    theme.colors['editor.background'] = '#123456';
    writeFileSync(themePath, JSON.stringify(theme));
    expect(spawnSync(process.execPath, [script], { cwd: dir }).status).toBe(0);
    expect(check(dir).status).toBe(0);
    expect(readFileSync(join(dir, 'test/visual/tokens/calm.css'), 'utf8')).toContain('#123456');
  });
});
