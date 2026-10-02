/** What waiting for the runner's answer to a control request ended in. */
export type WaitOutcome<R> =
  | { kind: 'result'; result: R }
  | { kind: 'timeout'; lookupFailed: boolean }
  | { kind: 'cancelled' };

export interface PollOptions<R> {
  /** Look for the runner's answer; `undefined` = not there yet. */
  find: () => Promise<R | undefined>;
  now: () => number;
  sleep: (ms: number) => Promise<void>;
  timeoutMs: number;
  intervalMs: number;
  cancelled: () => boolean;
}

/**
 * Wait for the runner to answer a control request that has ALREADY been sent.
 *
 * A failed lookup is not the gateway refusing the request: it is retried until the deadline, and the
 * outcome says whether the lookups were failing, so the caller can word the timeout honestly instead
 * of reporting a refusal for a request that went through.
 */
export async function pollControlResult<R>(o: PollOptions<R>): Promise<WaitOutcome<R>> {
  const deadline = o.now() + o.timeoutMs;
  let lookupFailed = false;
  while (!o.cancelled() && o.now() < deadline) {
    try {
      const result = await o.find();
      lookupFailed = false;
      if (result !== undefined) return { kind: 'result', result };
    } catch {
      lookupFailed = true;
    }
    if (o.cancelled()) break;
    await o.sleep(o.intervalMs);
  }
  return o.cancelled() ? { kind: 'cancelled' } : { kind: 'timeout', lookupFailed };
}

/**
 * Runs a task once while one with the same key is outstanding; a second caller gets the first's
 * promise. A double click on Retry must not send the request twice.
 */
export function inFlight(): <T>(key: string, task: () => Promise<T>) => Promise<T> {
  const running = new Map<string, Promise<unknown>>();
  return <T>(key: string, task: () => Promise<T>): Promise<T> => {
    const existing = running.get(key);
    if (existing) return existing as Promise<T>;
    const p = task().finally(() => running.delete(key));
    running.set(key, p);
    return p;
  };
}
