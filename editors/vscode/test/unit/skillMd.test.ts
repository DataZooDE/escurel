import { readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { parse } from 'yaml';
import { exportSkillMd } from '../../src/skills/skillMd';

// The fixtures the Rust converter (`crates/escurel-md/src/skill_md.rs`) is pinned to as well: one
// directory per case, each a `page.md` and the `SKILL.md` it exports to.
const FIXTURES = fileURLToPath(
  new URL('../../../../crates/escurel-md/tests/fixtures/skill-md/', import.meta.url),
);
const cases = readdirSync(FIXTURES).sort();
const fixture = (c: string, name: string) => readFileSync(`${FIXTURES}${c}/${name}`, 'utf8');

/** Frontmatter as data (the two YAML emitters may format it differently) and the body verbatim. */
function split(md: string): { fm: unknown; body: string } {
  const m = /^---\n([\s\S]*?)\n---\n([\s\S]*)$/.exec(md);
  if (!m) throw new Error('no frontmatter');
  return { fm: parse(m[1]!), body: m[2]! };
}

describe('exportSkillMd', () => {
  it('has the shared cases', () => {
    expect(cases).toEqual(
      expect.arrayContaining(['crlf', 'no-description', 'no-links', 'passthrough-only', 'triage']),
    );
  });

  it.each(cases)('renders the shared %s fixture the way the Rust converter does', (c) => {
    const out = exportSkillMd(fixture(c, 'page.md'));
    const got = split(out.skillMd);
    const want = split(fixture(c, 'SKILL.md'));
    expect(got.fm).toEqual(want.fm);
    expect(got.body).toBe(want.body);
  });

  it('keeps a CRLF body byte for byte', () => {
    const out = exportSkillMd(fixture('crlf', 'page.md'));
    expect(out.skillMd).toContain('# windows\r\n\r\nCheck [[customer::acme]] first.\r\n');
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
