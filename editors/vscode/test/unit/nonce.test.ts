import { describe, expect, it } from 'vitest';
import { newNonce } from '../../src/editors/nonce';

// The CSP nonce is what lets exactly one script run. Math.random() is not a security primitive.
describe('newNonce', () => {
  it('is at least 128 bits of base64 from a cryptographic source', () => {
    const n = newNonce();
    expect(n).toMatch(/^[A-Za-z0-9+/]{22,}={0,2}$/);
    expect(Buffer.from(n, 'base64').length).toBeGreaterThanOrEqual(16);
  });

  it('never repeats', () => {
    const seen = new Set(Array.from({ length: 2000 }, () => newNonce()));
    expect(seen.size).toBe(2000);
  });
});
