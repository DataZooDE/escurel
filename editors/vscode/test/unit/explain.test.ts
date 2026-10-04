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

  it('points to where things are: the Details panel, the Runner, Awaiting You', () => {
    expect(text).toContain('Escurel Details');
    expect(text).toContain('Runner');
    expect(text).toContain('Awaiting You');
  });

  it('fits on one screen and avoids the vocabulary it is meant to replace', () => {
    expect(text.split('\n').length).toBeLessThanOrEqual(32);
    for (const jargon of ['page_kind', 'etag', 'harness', 'projection', 'autonomy', 'backend_ref']) {
      expect(text.toLowerCase(), `no ${jargon}`).not.toContain(jargon);
    }
  });
});
