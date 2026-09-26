/**
 * The one place a bearer comes from (SPEC §1: one refresher shared by every
 * HTTP and WS client). `get()` resolves `undefined` when the gateway runs
 * without a verifier (no `escurel.auth.issuer`): then no header is sent.
 */
export interface TokenSource {
  get(): Promise<string | undefined>;
  /**
   * The signed-in subject, or `undefined` on a gateway with no verifier — where
   * every caller is the same principal, so "whose is this?" has one answer.
   * Used to tell your own held work from someone else's.
   */
  subject?(): Promise<string | undefined>;
  /** Fires with the new token after a refresh, so long-lived sockets reconnect. */
  onDidRefresh?(listener: (token: string | undefined) => void): { dispose(): void };
}

/** A fixed token (tests, a pasted bearer) or none at all (dev gateway). */
export function staticTokens(token?: string): TokenSource {
  return { get: async () => token };
}
