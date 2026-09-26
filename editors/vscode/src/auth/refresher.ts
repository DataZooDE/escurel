import type { TokenSource } from './tokenSource';
import { decodeExp } from './oidc';

/** What SecretStorage holds for the one signed-in session. */
export interface StoredSession {
  accessToken: string;
  refreshToken?: string;
  subject: string;
  issuer: string;
  clientId: string;
}

export interface RefresherDeps {
  load: () => Promise<StoredSession | undefined>;
  /** `undefined` clears the stored session (a refresh that the issuer refused). */
  save: (session: StoredSession | undefined) => Promise<void>;
  refresh: (session: StoredSession) => Promise<StoredSession>;
  /** Refresh this many seconds BEFORE `exp`: the gateway's verifier has zero leeway. Default 60. */
  skewSeconds?: number;
}

/**
 * The one token refresher every HTTP and WS client shares (SPEC §1). One
 * in-flight refresh serves all concurrent callers; a refresh the issuer
 * refuses clears the session so the caller sees "signed out" rather than
 * a 401 storm. With no stored session `get()` resolves `undefined` — the
 * dev-gateway "none" mode.
 */
export class TokenRefresher implements TokenSource {
  private inflight?: Promise<string | undefined>;

  /** The stored session's subject; `undefined` in the no-verifier mode. */
  async subject(): Promise<string | undefined> {
    return (await this.deps.load())?.subject;
  }
  private readonly listeners = new Set<(token: string | undefined) => void>();

  constructor(private readonly deps: RefresherDeps) {}

  onDidRefresh(listener: (token: string | undefined) => void): { dispose(): void } {
    this.listeners.add(listener);
    return { dispose: () => this.listeners.delete(listener) };
  }

  async get(): Promise<string | undefined> {
    const session = await this.deps.load();
    if (!session) return undefined;
    const exp = decodeExp(session.accessToken);
    const skew = this.deps.skewSeconds ?? 60;
    if (exp === undefined || exp - Math.floor(Date.now() / 1000) > skew) return session.accessToken;
    if (!session.refreshToken) {
      await this.deps.save(undefined);
      this.notify(undefined);
      return undefined;
    }
    this.inflight ??= this.refreshNow(session).finally(() => (this.inflight = undefined));
    return this.inflight;
  }

  /** Force a refresh (after a 401 the gateway answered on a token we thought fresh). */
  async invalidate(): Promise<string | undefined> {
    const session = await this.deps.load();
    if (!session?.refreshToken) {
      await this.deps.save(undefined);
      this.notify(undefined);
      return undefined;
    }
    this.inflight ??= this.refreshNow(session).finally(() => (this.inflight = undefined));
    return this.inflight;
  }

  private async refreshNow(session: StoredSession): Promise<string | undefined> {
    try {
      const fresh = await this.deps.refresh(session);
      await this.deps.save(fresh);
      this.notify(fresh.accessToken);
      return fresh.accessToken;
    } catch {
      await this.deps.save(undefined);
      this.notify(undefined);
      return undefined;
    }
  }

  private notify(token: string | undefined): void {
    for (const l of this.listeners) l(token);
  }
}
