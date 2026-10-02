import { detectAdminState, type AdminState, type ToolInfo } from './adminState';

/**
 * Whether the signed-in token is an admin's, asked of the gateway once and remembered.
 *
 * Every surface that shows a control reads this, so one `tools/list` serves them all instead of
 * one per tree item. It is forgotten when the token changes (`invalidate`), and a failure to ask
 * is answered `unknown` WITHOUT being remembered: being offline for a moment must not leave
 * controls in a guessed state until the next sign-in.
 */
export class AdminStateProvider {
  private cached: Promise<AdminState> | undefined;
  private readonly listeners = new Set<() => void>();

  constructor(private readonly loadTools: () => Promise<readonly ToolInfo[]>) {}

  get(): Promise<AdminState> {
    this.cached ??= this.loadTools().then(detectAdminState, () => {
      // Not remembered: the next caller asks again.
      this.cached = undefined;
      return 'unknown' as const;
    });
    return this.cached;
  }

  /** The token changed: forget the answer and tell whoever drew controls from it. */
  invalidate(): void {
    this.cached = undefined;
    for (const listener of this.listeners) listener();
  }

  onDidChange(listener: () => void): { dispose(): void } {
    this.listeners.add(listener);
    return { dispose: () => this.listeners.delete(listener) };
  }
}
