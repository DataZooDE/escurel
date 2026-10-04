import { latest } from './latest';

/**
 * A reload that can be started again while it is still running (every live event, view-state change and
 * gateway change starts one): only the NEWEST read is ever applied or reported, however the reads
 * finish, and `invalidate()` retires whatever is in flight (a tenant switch, a closed editor).
 */
export function latestLoader<T>(
  read: () => Promise<T>,
  apply: (value: T) => void,
  fail: (error: unknown) => void = () => undefined,
): { run: () => Promise<void>; invalidate: () => void } {
  const gate = latest();
  return {
    invalidate: () => gate.invalidate(),
    run: async () => {
      const mine = gate.begin();
      try {
        const value = await read();
        if (gate.isCurrent(mine)) apply(value);
      } catch (error) {
        if (gate.isCurrent(mine)) fail(error);
      }
    },
  };
}
