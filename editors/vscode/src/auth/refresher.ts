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

  private fixed?: { token: string; subject: string };

  /**
   * Sign in with a bearer someone else already holds, bypassing the store and every refresh.
   *
   * For the integration suite, the e2e tests and the demo, whose bearer comes from a test issuer.
   * It is a METHOD, not a setting or an environment variable, on purpose, and the API object that
   * reaches it is only returned in Test and Development mode (see `exposedApi`): a production
   * install exports nothing, so no other extension can call it or read the token store.
   * Long-lived sockets are told, so they reconnect with it.
   */
  useStaticToken(token: string, subject: string): void {
    this.fixed = { token, subject };
    this.notify(token);
  }

  /** The stored session's subject; `undefined` in the no-verifier mode. */
  async subject(): Promise<string | undefined> {
    if (this.fixed) return this.fixed.subject;
    return (await this.deps.load())?.subject;
  }
  private readonly listeners = new Set<(token: string | undefined) => void>();

  constructor(private readonly deps: RefresherDeps) {}

  onDidRefresh(listener: (token: string | undefined) => void): { dispose(): void } {
    this.listeners.add(listener);
    return { dispose: () => this.listeners.delete(listener) };
  }

  async get(): Promise<string | undefined> {
    if (this.fixed) return this.fixed.token;
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
    // A fixed token cannot be refreshed: answering with it again is the honest result, and a
    // 401 on it is the gateway's verdict, not something to retry into a loop.
    if (this.fixed) return this.fixed.token;
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
