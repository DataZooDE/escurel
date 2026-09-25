import { describe, expect, it } from 'vitest';
import {
  chipsForSkill,
  instanceRow,
  isReadOnlySkill,
  skillRow,
} from '../../src/views/knowledgeModel';
import { findWikilinks } from '../../src/skills/wikilinks';
import { fixture } from './mockGateway';
import type { Instance, Skill } from '../../src/client';

const skills = (
  fixture('list_skills').response as { result: { structuredContent: { skills: Skill[] } } }
).result.structuredContent.skills;
const customer = skills.find((s) => s.id === 'customer')!;

describe('knowledge model', () => {
  it("a skill row: id as label, description, chips in the mock's order (autonomy, event-typed, backend, layer, shadows)", () => {
    const row = skillRow({
      ...customer,
      autonomy: 'review',
      is_event_typed: true,
      backend: { kind: 'sql_view' },
      layer: 'base@supply-essentials@v3',
      shadows: undefined,
    });
    expect(row.label).toBe('customer');
    expect(row.description).toContain('review');
    expect(chipsForSkill(row.skill)).toEqual([
      'review',
      'event-typed',
      'sql_view',
      'base@supply-essentials@v3',
    ]);
  });

  it('autonomy absent reads as "review" (hold for review), never as auto', () => {
    const { autonomy, ...noAutonomy } = customer;
    void autonomy;
    expect(chipsForSkill(noAutonomy as Skill)[0]).toBe('review');
    expect(chipsForSkill({ ...customer, autonomy: 'auto' })[0]).toBe('auto');
    expect(chipsForSkill({ ...customer, autonomy: 'confirm' })[0]).toBe('confirm');
  });

  it('overlay skills are editable; base@ pins and shadowed bases are read-only', () => {
    expect(isReadOnlySkill({ ...customer, layer: 'overlay' })).toBe(false);
    expect(isReadOnlySkill({ ...customer, layer: 'base@supply-essentials@v3' })).toBe(true);
    expect(chipsForSkill({ ...customer, layer: 'overlay', shadows: 'base@p@v1' })).toContain(
      'shadows',
    );
  });

  it('an instance row shows the slug, the skill for context and the frontmatter title when there is one', () => {
    const inst: Instance = {
      page_id: 'markdown/instances/customer/acme.md',
      skill: 'customer',
      frontmatter: { id: 'acme', name: 'Acme Corp' },
      at: null,
    };
    const row = instanceRow(inst);
    expect(row.label).toBe('acme');
    expect(row.description).toBe('Acme Corp');
    expect(row.pageId).toBe(inst.page_id);
    // A flat corpus names the file `<skill>__<id>.md`; the row shows the id.
    const flat = instanceRow({ ...inst, page_id: 'markdown/instances/customer__acme.md' });
    expect(flat.label).toBe('acme');
    expect(flat.description).toBe('Acme Corp');
    // A file that merely contains `__` keeps its name.
    expect(instanceRow({ ...inst, page_id: 'markdown/instances/other__acme.md' }).label).toBe(
      'other__acme',
    );
  });
});

describe('findWikilinks', () => {
  it('finds [[skill::id]] links with their offsets and parsed parts, ignoring code fences', () => {
    const text =
      'See [[customer::acme]] and [[playbook::renewal#step-2|the playbook]].\n```\n[[not::linked]]\n```\n[[broken';
    const links = findWikilinks(text);
    expect(links.map((l) => l.text)).toEqual([
      '[[customer::acme]]',
      '[[playbook::renewal#step-2|the playbook]]',
    ]);
    expect(links[0]).toMatchObject({ start: 4, end: 22, skill: 'customer', id: 'acme' });
    expect(links[1]).toMatchObject({ skill: 'playbook', id: 'renewal', anchor: 'step-2' });
  });
});
