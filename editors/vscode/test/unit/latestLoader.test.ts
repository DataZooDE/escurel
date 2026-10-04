import { describe, expect, it } from 'vitest';
import { latestLoader } from '../../src/shared/latestLoader';

const deferred = <T>() => {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

// The page editor reloads on every live event, view-state change and gateway change, and each reload is
// several awaits. A slow OLD read that finishes last must never replace a newer result: it is what the
// host judges webview messages against, and what the person sees ("applying" over "applied").
describe('latestLoader', () => {
  it('applies only the newest read, however the reads finish', async () => {
    const reads = [deferred<string>(), deferred<string>()];
    let i = 0;
    const applied: string[] = [];
    const load = latestLoader(
      () => reads[i++]!.promise,
      (v) => applied.push(v),
    );
    const first = load.run();
    const second = load.run();
    reads[1]!.resolve('new');
    await second;
    reads[0]!.resolve('old');
    await first;
    expect(applied).toEqual(['new']);
  });

  it('a read that fails after a newer one started does not report its error either', async () => {
    const reads = [deferred<string>(), deferred<string>()];
    let i = 0;
    const applied: string[] = [];
    const failed: unknown[] = [];
    const load = latestLoader(
      () => reads[i++]!.promise,
      (v) => applied.push(v),
      (e) => failed.push(e),
    );
    const first = load.run();
    const second = load.run();
    reads[1]!.resolve('new');
    await second;
    reads[0]!.reject(new Error('socket hang up'));
    await first;
    expect(applied).toEqual(['new']);
    expect(failed).toEqual([]);
  });

  it('reports the error of the newest read', async () => {
    const failed: unknown[] = [];
    const load = latestLoader(
      async () => {
        throw new Error('boom');
      },
      () => undefined,
      (e) => failed.push((e as Error).message),
    );
    await load.run();
    expect(failed).toEqual(['boom']);
  });

  it('invalidate() retires a read in flight (a gateway switch, a closed editor)', async () => {
    const read = deferred<string>();
    const applied: string[] = [];
    const load = latestLoader(
      () => read.promise,
      (v) => applied.push(v),
    );
    const running = load.run();
    load.invalidate();
    read.resolve('stale');
    await running;
    expect(applied).toEqual([]);
  });
});
