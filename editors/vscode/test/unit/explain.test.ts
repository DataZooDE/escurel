import { describe, expect, it } from 'vitest';
import { explainText } from '../../src/shared/explain';

// "Explain this view": one screen for somebody who knows the business but not escurel's vocabulary.
describe('explainText', () => {
  const text = explainText();

  it('tells the story event -> run -> changeset -> instances in plain words', () => {
    for (const part of ['event', 'run', 'changeset', 'instance']) {
      expect(text.toLowerCase(), `mentions ${part}`).toContain(part);
    }
    expect(text).toMatch(/skill/i);
  });

  it('says what read-only, source and external data mean', () => {
    expect(text).toMatch(/read-only/i);
    expect(text).toMatch(/source/i);
    expect(text).toMatch(/external/i);
  });

  it('points to where things are: the Details panel, Runs, Awaiting You', () => {
    expect(text).toContain('Escurel Details');
    expect(text).toContain('Runs');
    expect(text).toContain('Awaiting You');
  });

  it('explains the words the views use: gate, draft vs changeset, cascade, engine, autonomy, source, dead letter, trace', () => {
    for (const word of [
      'Needs you',
      'changeset',
      'Cascade',
      'Agent engine',
      'Autonomy',
      'Dead letter',
      'Trace',
      'notes',
    ])
      expect(text, `explains ${word}`).toContain(word);
    expect(text).not.toMatch(/harness/i);
  });

  it('fits on one screen and avoids the vocabulary it is meant to replace', () => {
    expect(text.split('\n').length).toBeLessThanOrEqual(60);
    for (const jargon of ['page_kind', 'etag', 'projection', 'backend_ref', 'sql_view']) {
      expect(text.toLowerCase(), `no ${jargon}`).not.toContain(jargon);
    }
  });
});
