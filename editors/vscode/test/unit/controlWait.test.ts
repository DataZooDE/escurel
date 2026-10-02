import { describe, expect, it } from 'vitest';
import { inFlight, pollControlResult } from '../../src/runs/controlWait';

const clock = () => {
  let t = 0;
  return { now: () => t, sleep: async (ms: number) => void (t += ms) };
};

// The request is already sent when the wait starts. A hiccup while LOOKING for the answer is not a
// refusal of the request, and must not be reported as one.
describe('pollControlResult', () => {
  it('returns the answer as soon as the runner has given it', async () => {
    const c = clock();
    let calls = 0;
    const out = await pollControlResult({
      find: async () => (++calls === 3 ? { outcome: 'cancelled' } : undefined),
      ...c,
      timeoutMs: 30_000,
      intervalMs: 500,
      cancelled: () => false,
    });
    expect(out).toEqual({ kind: 'result', result: { outcome: 'cancelled' } });
    expect(calls).toBe(3);
  });

  it('keeps waiting through a failed lookup instead of calling the request refused', async () => {
    const c = clock();
    let calls = 0;
    const out = await pollControlResult({
      find: async () => {
        calls += 1;
        if (calls === 1) throw new Error('socket hang up');
        return { outcome: 'paused' };
      },
      ...c,
      timeoutMs: 30_000,
      intervalMs: 500,
      cancelled: () => false,
    });
    expect(out).toEqual({ kind: 'result', result: { outcome: 'paused' } });
  });

  it('gives up at the deadline, and says whether the lookups were failing', async () => {
    const c = clock();
    const quiet = await pollControlResult({
      find: async () => undefined,
      ...c,
      timeoutMs: 2_000,
      intervalMs: 500,
      cancelled: () => false,
    });
    expect(quiet).toEqual({ kind: 'timeout', lookupFailed: false });
    const c2 = clock();
    const broken = await pollControlResult({
      find: async () => {
        throw new Error('down');
      },
      ...c2,
      timeoutMs: 2_000,
      intervalMs: 500,
      cancelled: () => false,
    });
    expect(broken).toEqual({ kind: 'timeout', lookupFailed: true });
  });

  it('stops when the person cancels the wait', async () => {
    const c = clock();
    let stop = false;
    const out = await pollControlResult({
      find: async () => {
        stop = true;
        return undefined;
      },
      ...c,
      timeoutMs: 30_000,
      intervalMs: 500,
      cancelled: () => stop,
    });
    expect(out).toEqual({ kind: 'cancelled' });
  });
});

// Two clicks on Retry are two requests, and the second one runs the work twice.
describe('inFlight', () => {
  it('runs a request once while it is outstanding, and again after it settled', async () => {
    const guard = inFlight();
    let runs = 0;
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const task = async () => {
      runs += 1;
      await gate;
    };
    const first = guard('retry:run-1', task);
    const second = guard('retry:run-1', task);
    release();
    await Promise.all([first, second]);
    expect(runs).toBe(1);
    await guard('retry:run-1', async () => void (runs += 1));
    expect(runs).toBe(2);
  });

  it('does not mix up different requests, and clears the key when the task throws', async () => {
    const guard = inFlight();
    let runs = 0;
    await guard('cancel:a', async () => void (runs += 1));
    await guard('cancel:b', async () => void (runs += 1));
    expect(runs).toBe(2);
    await expect(
      guard('x', async () => {
        throw new Error('boom');
      }),
    ).rejects.toThrow('boom');
    await guard('x', async () => void (runs += 1));
    expect(runs).toBe(3);
  });
});
