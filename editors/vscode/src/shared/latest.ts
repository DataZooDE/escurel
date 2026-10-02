/**
 * "Latest wins" for async loads: `begin()` before the read, `isCurrent(token)` after it. A slow
 * earlier read that finishes last must not overwrite a newer result, and `invalidate()` retires
 * whatever is in flight (a tenant switch, a disposed view).
 */
export function latest(): {
  begin: () => number;
  isCurrent: (token: number) => boolean;
  invalidate: () => void;
} {
  let n = 0;
  return {
    begin: () => ++n,
    isCurrent: (token) => token === n,
    invalidate: () => void ++n,
  };
}
