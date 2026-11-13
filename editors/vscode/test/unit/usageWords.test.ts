import { describe, expect, it } from 'vitest';
import { compactTokens, formatCost, successRate, usageChip } from '../../src/shared/usageWords';

describe('usage in words', () => {
  it('compacts token counts the way a chip has room for', () => {
    expect(compactTokens(860)).toBe('860');
    expect(compactTokens(12_400)).toBe('12.4k');
    expect(compactTokens(12_000)).toBe('12k');
    expect(compactTokens(3_450_000)).toBe('3.5M');
  });

  it('prices with the precision a small run needs', () => {
    expect(formatCost(0)).toBe('$0');
    expect(formatCost(0.0421)).toBe('$0.042');
    expect(formatCost(0.00123)).toBe('$0.0012');
    expect(formatCost(12.345)).toBe('$12.35');
  });

  it('builds the chip text and its full spoken label', () => {
    expect(
      usageChip({
        inputTokens: 12_400,
        outputTokens: 860,
        costUsd: 0.0421,
        model: 'claude-sonnet-4',
      }),
    ).toEqual({
      text: '12.4k in · 860 out · $0.042',
      label: 'Cost: 12,400 input tokens, 860 output tokens, $0.042, model claude-sonnet-4',
      title: 'Input 12,400 tokens · output 860 tokens · $0.042 · model claude-sonnet-4',
    });
    // No price reported: the chip says tokens only, never "$0".
    expect(usageChip({ inputTokens: 10, outputTokens: 2 }).text).toBe('10 in · 2 out');
  });

  it('reads the success rate from by_status, and says nothing for no runs', () => {
    expect(successRate({ runs: 4, by_status: { processed: 3, failed: 1 } })).toBe('75% ok');
    expect(successRate({ runs: 3, by_status: { processed: 2, dead_letter: 1 } })).toBe('67% ok');
    expect(successRate({ runs: 0, by_status: {} })).toBe('');
  });
});
