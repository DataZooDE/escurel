import { describe, expect, it } from 'vitest';
import { requireEnv } from '../../test/integration/requireEnv';

// A suite that needs the real gateway/runner used to call this.skip() when its env var was missing, so a
// misconfigured CI run went GREEN with the suite never having run. Locally a skip is convenient; in CI it
// is a lie.
describe('requireEnv', () => {
  const ctx = () => {
    const calls: string[] = [];
    return { calls, skip: () => void calls.push('skip') };
  };

  it('does nothing when the variable is set', () => {
    const c = ctx();
    requireEnv(c, 'ESCUREL_TEST_RUNNER', { ESCUREL_TEST_RUNNER: '1' });
    expect(c.calls).toEqual([]);
  });

  it('skips locally when it is missing', () => {
    const c = ctx();
    requireEnv(c, 'ESCUREL_TEST_RUNNER', {});
    expect(c.calls).toEqual(['skip']);
  });

  it('FAILS in CI when it is missing, naming the variable', () => {
    const c = ctx();
    expect(() => requireEnv(c, 'ESCUREL_TEST_RUNNER', { CI: 'true' })).toThrow(/ESCUREL_TEST_RUNNER/);
    expect(c.calls).toEqual([]);
  });

  it('treats an empty variable as missing', () => {
    const c = ctx();
    expect(() => requireEnv(c, 'X', { CI: '1', X: '' })).toThrow(/X/);
  });
});
