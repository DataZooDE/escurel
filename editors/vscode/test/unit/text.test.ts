import { describe, expect, it } from 'vitest';
import { pluralise } from '../../src/shared/text';

describe('pluralise', () => {
  it('formats singular and plural counts for drafts, fields, and blocks', () => {
    expect(pluralise(1, 'draft')).toBe('1 draft');
    expect(pluralise(3, 'draft')).toBe('3 drafts');
    expect(pluralise(0, 'draft')).toBe('0 drafts');
    expect(pluralise(1, 'field')).toBe('1 field');
    expect(pluralise(2, 'field')).toBe('2 fields');
    expect(pluralise(1, 'block')).toBe('1 block');
    expect(pluralise(5, 'block')).toBe('5 blocks');
  });
});
