import { describe, expect, it } from 'vitest';
import { requireSavedVisibleFile } from '../../src/evolve/localFileReview';

describe('local private training file review', () => {
  it('accepts matching saved bytes, including a file not open in an editor', () => {
    expect(() => requireSavedVisibleFile('file:///training.csv', 'saved', [])).not.toThrow();
    expect(() => requireSavedVisibleFile('file:///training.csv', 'saved', [
      { uri: 'file:///training.csv', isDirty: false, text: 'saved' },
    ])).not.toThrow();
  });

  it('rejects dirty and stale sibling files as well as the active CSV', () => {
    for (const uri of ['file:///training.csv', 'file:///training.manifest.json',
      'file:///training.template.json']) {
      expect(() => requireSavedVisibleFile(uri, 'saved', [
        { uri, isDirty: true, text: 'changed' },
      ])).toThrow(/unsaved or stale/);
      expect(() => requireSavedVisibleFile(uri, 'saved', [
        { uri, isDirty: false, text: 'changed' },
      ])).toThrow(/unsaved or stale/);
    }
  });
});
