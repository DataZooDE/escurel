import { describe, expect, it } from 'vitest';
import { noThreadMessage, noThreadNote } from '../../src/commands/noThreadWording';

describe('noThreadMessage', () => {
  it('says a source record has no thread because no agent wrote it', () => {
    expect(noThreadMessage('sql_view')).toContain('comes from a SQL table');
    expect(noThreadMessage('openapi')).toContain('a REST API');
    expect(noThreadMessage('mcp')).toContain('an MCP tool');
    for (const kind of ['sql_view', 'openapi', 'mcp'])
      expect(noThreadMessage(kind)).not.toMatch(/sql_view|openapi|\bmcp\b/);
  });
  it('points to Runs for this record, for a page nothing changed yet', () => {
    expect(noThreadMessage('markdown')).toContain('Runs for this record');
    expect(noThreadMessage(undefined)).toContain('no thread to open');
  });
});

describe('noThreadNote', () => {
  it('is a note for a source record without a thread, and nothing otherwise', () => {
    expect(noThreadNote(true, false)).toContain('no agent wrote it');
    expect(noThreadNote(true, true)).toBe('');
    expect(noThreadNote(false, false)).toBe('');
  });
});
