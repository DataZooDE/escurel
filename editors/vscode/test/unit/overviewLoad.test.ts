import { describe, expect, it } from 'vitest';
import type { Skill } from '../../src/client';
import { OPEN_SKILLS_MAX, pickOpenSkills } from '../../src/overview/load';

const skill = (id: string, over: Partial<Skill> = {}): Skill =>
  ({ id, is_event_typed: false, ...over }) as unknown as Skill;

describe('which skills the "Open items" tile counts', () => {
  it('counts records and processes, not events, reports, helpers or the engine’s own skills', () => {
    const picked = pickOpenSkills([
      skill('customer-order', { role: 'record' }),
      skill('supplier-risk-analysis', { role: 'process' }),
      skill('meeting', { is_event_typed: true }),
      skill('risk-report', { role: 'report' }),
      skill('helper-thing', { role: 'helper' }),
      skill('escurel:run'),
      skill('plain-skill'),
    ]);
    expect(picked.map((s) => s.id)).toEqual([
      'customer-order',
      'supplier-risk-analysis',
      'plain-skill',
    ]);
  });

  it('is bounded: a tenant with hundreds of skills costs a handful of reads', () => {
    const many = Array.from({ length: 200 }, (_, i) => skill(`s${i}`, { role: 'record' }));
    expect(pickOpenSkills(many)).toHaveLength(OPEN_SKILLS_MAX);
  });

  it('names a skill by its title when it has one, else by its id in words', () => {
    const [a, b] = pickOpenSkills([
      skill('customer-order', { title: 'Customer orders' }),
      skill('supplier_check'),
    ]);
    expect([a!.title, b!.title]).toEqual(['Customer orders', 'Supplier check']);
  });
});
