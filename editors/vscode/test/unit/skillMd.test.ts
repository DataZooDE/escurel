import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { parse } from 'yaml';
import { exportSkillMd } from '../../src/skills/skillMd';

// The fixture pair the CLI's converter (`crates/escurel-cli/src/skill_md.rs`) is pinned to as well.
const fixture = (name: string) =>
  readFileSync(
    fileURLToPath(
      new URL(`../../../../crates/escurel-cli/tests/fixtures/skill-md/${name}`, import.meta.url),
    ),
    'utf8',
  );

function split(md: string): { fm: unknown; body: string } {
  const m = /^---\n([\s\S]*?)\n---\n([\s\S]*)$/.exec(md);
  if (!m) throw new Error('no frontmatter');
  return { fm: parse(m[1]!), body: m[2]! };
}

describe('exportSkillMd', () => {
  it('renders the shared fixture the way the CLI does', () => {
    const out = exportSkillMd(fixture('page.md'));
    expect(out.name).toBe('triage');
    const got = split(out.skillMd);
    const want = split(fixture('SKILL.md'));
    expect(got.fm).toEqual(want.fm);
    expect(got.body).toBe(want.body);
  });

  it('a page without links and without extra keys is name + description only', () => {
    const out = exportSkillMd('---\nkind: skill\nid: plain\ndescription: d\n---\n# plain\n');
    expect(out.skillMd).toBe('---\nname: plain\ndescription: d\n---\n# plain\n');
  });

  it('falls back to the summary when there is no description, and drops the pack layer', () => {
    const out = exportSkillMd(
      '---\nlayer: base@crm@v2\nkind: skill\nid: s\nsummary: Sum.\n---\nx\n',
    );
    expect(split(out.skillMd).fm).toEqual({
      name: 's',
      description: 'Sum.',
      metadata: { escurel: { summary: 'Sum.' } },
    });
  });

  it('refuses an instance page, and an id that would not be a plain folder name', () => {
    expect(() => exportSkillMd('---\nkind: instance\nskill: c\nid: a\n---\nx\n')).toThrow(/skill/);
    expect(() => exportSkillMd('---\nkind: skill\nid: ..\ndescription: d\n---\nx\n')).toThrow(
      /folder/,
    );
  });
});
