import { describe, expect, it } from 'vitest';
import { cleanBlock, cleanText } from '../../src/shared/untrustedText';

// Text from a REST/MCP/SQL source or another person's page ends up in tree labels, tooltips and
// notifications. It is data: no bidi overrides (a filename that reads backwards), no control characters,
// no unbounded length.
describe('cleanText', () => {
  it('removes bidi overrides, isolates, zero-width and control characters', () => {
    expect(cleanText('a‮b‬c⁦d⁩e​f\u0000g\u0007h')).toBe('abcdefgh');
  });

  it('collapses whitespace runs and newlines to one space', () => {
    expect(cleanText('a \n\t b\r\nc')).toBe('a b c');
  });

  it('caps the length with an ellipsis, never splitting a surrogate pair', () => {
    const out = cleanText('😀'.repeat(50), 11);
    expect(out.endsWith('…')).toBe(true);
    expect([...out].length).toBeLessThanOrEqual(11);
    expect(out).not.toMatch(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])/);
  });

  it('leaves ordinary text alone', () => {
    expect(cleanText('Müller & Söhne GmbH — Pforzheim')).toBe('Müller & Söhne GmbH — Pforzheim');
  });
});

describe('cleanBlock', () => {
  it('keeps line breaks and tabs, strips the rest, and caps the length', () => {
    expect(cleanBlock('a\n\tb\u202Ec\u0000')).toBe('a\n\tbc');
    expect(cleanBlock('z'.repeat(100), 10)).toHaveLength(10);
  });
});
