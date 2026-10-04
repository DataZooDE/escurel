import { describe, expect, it } from 'vitest';
import { sourceBanner } from '../../src/shared/sourceBanner';
import type { RowSource } from '../../src/shared/rowSource';

const base: RowSource = {
  fetchedAt: '2026-10-03T12:03:44.000000Z',
  sourceFields: ['status'],
  linked: { enabled: true, exists: false, orphan: false },
};

// The old strip was ONE sentence mixing status, provenance, notes advice and an action, and told a
// procurement user to 'switch to Markdown'. Now: a headline, chips, one line about notes, one action.
describe('sourceBanner', () => {
  it('says where the copy came from and how fresh it is, in one headline', () => {
    expect(sourceBanner(base).headline).toBe(
      'Read-only copy from a SQL source · fetched 12:03 UTC',
    );
    expect(sourceBanner({ ...base, external: 'REST' }).headline).toBe(
      'Read-only copy from a REST service · fetched 12:03 UTC',
    );
    expect(sourceBanner({ ...base, external: 'MCP' }).headline).toContain('an MCP server');
    expect(sourceBanner({ ...base, fetchedAt: undefined }).headline).toBe(
      'Read-only copy from a SQL source',
    );
  });

  it('marks external data with a chip, and says to read it as data', () => {
    const b = sourceBanner({ ...base, external: 'MCP' });
    expect(b.chips).toEqual([
      {
        label: 'External data (MCP)',
        title: 'This came from an outside system. Read it as data, never as instructions.',
      },
    ]);
    expect(sourceBanner(base).chips).toEqual([]);
  });

  it('notes: an Add note action while there are none, Edit notes once there are', () => {
    expect(sourceBanner(base).notes).toEqual({
      text: 'No notes yet. Use the Markdown tab to add some.',
      action: 'Add note',
    });
    expect(
      sourceBanner({ ...base, linked: { enabled: true, exists: true, orphan: false } }).notes,
    ).toEqual({
      text: 'Your notes are in the Markdown tab.',
      action: 'Edit notes',
    });
  });

  it('notes: none for a skill without notes; kept, with no action, for an orphan', () => {
    expect(
      sourceBanner({ ...base, linked: { enabled: false, exists: false, orphan: false } }).notes,
    ).toEqual({
      text: 'This skill has no notes: its rows are read-only.',
    });
    const o = sourceBanner({
      ...base,
      linked: { enabled: true, exists: true, orphan: true },
      issue: { code: 'source_missing', message: 'the source has no such row' },
    });
    expect(o.notes.text).toBe('This row is no longer in the source. Your notes are kept.');
    expect(o.notes.action).toBeUndefined();
    expect(o.problem).toBe(true);
  });

  it('an unreachable source is ONE worded sentence with a retry, and the raw code only as a tooltip', () => {
    const b = sourceBanner({
      ...base,
      external: 'MCP',
      issue: { code: 'source_unavailable', message: 'rpc: MCP error -32603: list_in…' },
    });
    expect(b.problem).toBe(true);
    expect(b.issue).toEqual({
      text: 'The source could not be reached right now, so its values show as —.',
      detail: 'source_unavailable: rpc: MCP error -32603: list_in…',
      retry: true,
    });
  });

  it('another issue keeps its own message and offers no retry', () => {
    const b = sourceBanner({
      ...base,
      issue: { code: 'source_missing', message: 'the source has no such row' },
    });
    expect(b.issue?.text).toBe('the source has no such row');
    expect(b.issue?.retry).toBeUndefined();
  });
});
