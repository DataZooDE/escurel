import { describe, expect, it } from 'vitest';
import { latest } from '../../src/shared/latest';

// A slow read that finishes after a newer one must not overwrite the newer result (a reconnect, a
// live burst and a manual refresh can all start a load while an earlier one is still in flight).
describe('latest', () => {
  it('only the most recently begun operation is current', () => {
    const l = latest();
    const a = l.begin();
    expect(l.isCurrent(a)).toBe(true);
    const b = l.begin();
    expect(l.isCurrent(a)).toBe(false);
    expect(l.isCurrent(b)).toBe(true);
  });

  it('invalidate() retires an operation that is still running, e.g. after a tenant switch', () => {
    const l = latest();
    const a = l.begin();
    l.invalidate();
    expect(l.isCurrent(a)).toBe(false);
  });

  it('keeps separate sequences independent', () => {
    const x = latest();
    const y = latest();
    const a = x.begin();
    y.begin();
    y.begin();
    expect(x.isCurrent(a)).toBe(true);
  });
});
