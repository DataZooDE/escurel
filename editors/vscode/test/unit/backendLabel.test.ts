import { describe, expect, it } from 'vitest';
import { backendLabel, durationWords } from '../../src/shared/backendLabel';

describe('backendLabel', () => {
  it('names where the data comes from, never the wire word', () => {
    expect(backendLabel('sql_view')).toBe('SQL table');
    expect(backendLabel('openapi')).toBe('REST API');
    expect(backendLabel('mcp')).toBe('MCP tool');
    expect(backendLabel('markdown')).toBe('Pages in this knowledge base');
    expect(backendLabel('weird')).toBe('weird');
  });
});

describe('durationWords', () => {
  it('reads an ISO duration', () => {
    expect(durationWords('P90D')).toBe('90 days');
    expect(durationWords('P1D')).toBe('1 day');
    expect(durationWords('P2W')).toBe('2 weeks');
    expect(durationWords('PT36H')).toBe('36 hours');
    expect(durationWords('P1DT12H')).toBe('1 day 12 hours');
  });
  it('says nothing about what is not one', () => {
    expect(durationWords('soon')).toBeUndefined();
    expect(durationWords('P')).toBeUndefined();
  });
});
