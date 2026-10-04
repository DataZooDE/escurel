/**
 * A suite that needs the real gateway/runner/admin token calls this in `suiteSetup`. Locally a missing
 * variable skips the suite (convenient while iterating on one pass); in CI it FAILS: a green run in which
 * the suite silently did not run is worse than a red one.
 */
export function requireEnv(
  ctx: { skip: () => void },
  name: string,
  env: Record<string, string | undefined> = process.env,
): void {
  if (env[name]) return;
  if (env.CI) {
    throw new Error(
      `${name} is not set, so this suite cannot run. In CI that is a failure, not a skip: the harness (runTests.ts) must provide it.`,
    );
  }
  ctx.skip();
}
