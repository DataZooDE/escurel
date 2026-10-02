import { describe, expect, it } from 'vitest';
import type { LineageNode } from '../../src/client';
import { rootSkill } from '../../src/runs/loadRun';

const event = (id: string, label: string, parent: string | null): LineageNode =>
  ({ id, type: 'event', parent, state: 'processed', label_skill: label }) as LineageNode;

describe('rootSkill', () => {
  it('is the root event’s skill', () => {
    expect(rootSkill([event('root', 'supplier-risk', null)], 'root')).toBe('supplier-risk');
  });

  it('prefers the root event by id over any other event with no parent', () => {
    // Lineage prunes what the caller may not read, and a node whose parent was pruned reads as
    // having no parent. One of those listed before the root must not be taken for it.
    const nodes = [
      event('orphan', 'someone-elses-skill', null),
      event('root', 'supplier-risk', null),
    ];
    expect(rootSkill(nodes, 'root')).toBe('supplier-risk');
  });

  it('falls back to a parentless event only when the root itself is not listed', () => {
    expect(rootSkill([event('only', 'supplier-risk', null)], 'root')).toBe('supplier-risk');
  });

  it('says nothing when there is nothing to say', () => {
    expect(rootSkill([], 'root')).toBeUndefined();
    expect(
      rootSkill([{ id: 'r', type: 'run', parent: 'root', state: 'x' } as LineageNode], 'root'),
    ).toBeUndefined();
  });
});
