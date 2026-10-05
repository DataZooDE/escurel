import { describe, expect, it } from 'vitest';
import { pageTabTitle, skillTabTitle } from '../../src/shared/tabTitle';
import { pageIdFromPath } from '../../src/fs/read';

describe('pageTabTitle', () => {
  it('uses the page title when it says more than the id', () => {
    expect(
      pageTabTitle({
        title: 'Meier Guss — risk analysis',
        skill: 'supplier-risk-analysis',
        slug: 'meier-guss-2026-10-04',
      }),
    ).toBe('Meier Guss — risk analysis');
  });

  it('falls back to the skill and the id when the title is just the id', () => {
    expect(pageTabTitle({ title: 'all', skill: 'order-lines', slug: 'all' })).toBe(
      'order-lines · all',
    );
    expect(
      pageTabTitle({ title: 'order-4500123', skill: 'customer-order', slug: 'order-4500123' }),
    ).toBe('customer-order · order-4500123');
  });

  it('is never empty and never runs on', () => {
    expect(pageTabTitle({ title: '', skill: '', slug: '' })).toBe('Page');
    const long = pageTabTitle({ title: 'x'.repeat(200), skill: 's', slug: 'y' });
    expect(long.length).toBeLessThanOrEqual(48);
    expect(long.endsWith('…')).toBe(true);
  });

  it('strips control and bidi characters from a title an author wrote', () => {
    expect(pageTabTitle({ title: 'evil‮exe.txt\u0000', skill: 's', slug: 'y' })).toBe(
      'evilexe.txt',
    );
  });
});

describe('skillTabTitle', () => {
  it('names the skill, not the file', () => {
    expect(skillTabTitle('supplier-risk')).toBe('Skill · supplier-risk');
    expect(skillTabTitle('')).toBe('Skill');
  });
});

describe('page ids stay the URI, whatever the tab says', () => {
  it('still resolves a row instance URI to its page id', () => {
    expect(pageIdFromPath('/instances/order-lines/all.md')).toMatchObject({
      pageId: 'markdown/instances/order-lines/all.md',
      kind: 'instance',
    });
  });
});
