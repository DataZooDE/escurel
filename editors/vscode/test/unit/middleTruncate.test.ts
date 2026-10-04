import { describe, expect, it } from 'vitest';
import { middleTruncate } from '../../src/shared/middleTruncate';

// A 64-char hash or a 26-char id wrapped across the Details panel. Cut the middle: both ends of an
// identifier are what a person compares.
describe('middleTruncate', () => {
  it('leaves short text alone', () => {
    expect(middleTruncate('order-4500131', 24)).toBe('order-4500131');
  });
  it('keeps the head and the tail and marks the cut', () => {
    const out = middleTruncate('01M41QQB0QYAYPPVK6ERX3FFK0', 14);
    expect(out).toBe('01M41QQ…X3FFK0');
    expect(out.length).toBe(14);
  });
  it('never returns more than the limit, and never splits a surrogate pair', () => {
    expect(middleTruncate('a'.repeat(100), 10).length).toBe(10);
    expect(middleTruncate('😀'.repeat(20), 9)).not.toMatch(/[\ud800-\udbff](?![\udc00-\udfff])/);
  });
  it('handles tiny limits', () => {
    expect(middleTruncate('abcdef', 1)).toBe('…');
    expect(middleTruncate('abcdef', 0)).toBe('');
  });
});
