import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { VisibleTicker } from '../../src/views/visibleTicker';
import { runtimeRows } from '../../src/views/runtimesModel';
import type { Runtime } from '../../src/client/types';

// The Runtimes view re-reads on a timer while it is visible (crew U-F10): a runner that stops
// reporting turns from online to stale on screen with no manual refresh.
describe('VisibleTicker', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('ticks only while visible and stops when hidden or disposed', () => {
    const tick = vi.fn();
    const t = new VisibleTicker(1_000, tick);
    vi.advanceTimersByTime(5_000);
    expect(tick).not.toHaveBeenCalled();
    t.setVisible(true);
    t.setVisible(true);
    vi.advanceTimersByTime(3_000);
    expect(tick).toHaveBeenCalledTimes(3);
    t.setVisible(false);
    vi.advanceTimersByTime(5_000);
    expect(tick).toHaveBeenCalledTimes(3);
    t.setVisible(true);
    t.dispose();
    vi.advanceTimersByTime(5_000);
    expect(t.running).toBe(false);
    expect(tick).toHaveBeenCalledTimes(3);
  });

  it('turns an online runner stale on the next tick, without a reload', () => {
    const base: Runtime = {
      runner_id: 'laptop',
      page_id: 'markdown/instances/runtime/laptop.md',
      harnesses: [{ name: 'echo' }],
      labels: [],
      max_concurrent: 1,
      status: 'online',
      last_seen: '2026-11-13T10:00:00Z',
      age_s: 1,
      stale_after_s: 3,
      version: null,
      live_runs: 0,
    };
    let served: Runtime = base;
    let shown = runtimeRows([served]);
    const t = new VisibleTicker(1_000, () => {
      shown = runtimeRows([served]);
    });
    t.setVisible(true);
    expect(shown[0]?.status).toBe('online');
    served = { ...base, status: 'stale', age_s: 10 };
    vi.advanceTimersByTime(1_000);
    expect(shown[0]?.status).toBe('stale');
    expect(shown[0]?.tooltip).toContain('No recent status report; the runner may be unavailable.');
    t.dispose();
  });
});
