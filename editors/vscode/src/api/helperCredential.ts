import type { HelperCredential, HelperCredentialRequest } from './types';

/** `publisher.name`, as VS Code spells an extension id. */
const EXTENSION_ID = /^[A-Za-z0-9][A-Za-z0-9-]*\.[A-Za-z0-9][A-Za-z0-9._-]*$/;
/** Becomes part of the helper's agent subject (`agent:helper-<audience>`): letters, digits, `_`, `-`. */
const AUDIENCE = /^[A-Za-z][A-Za-z0-9_-]{0,39}$/;
export const DEFAULT_TTL_SECONDS = 300;
export const MAX_TTL_SECONDS = 3600;

export interface HelperDeps {
  /** False when this window sends no bearer at all (no issuer, no session): a helper needs none either. */
  authRequired(): Promise<boolean>;
  /** The gateway's `mint_agent_token`: a bearer for one agent run, never admin, no refresh. */
  mint(req: { skill: string; ttl_secs: number }): Promise<{ token: string; expires_at: string }>;
  /** `escurel.api.helperCredentialExtensions` (user level). Anything but a list of strings counts as empty. */
  allowlist(): unknown;
  /** Extensions the user said yes to in the consent dialog. */
  consents: { has(id: string): boolean; grant(id: string): Promise<void> };
  /** The modal consent question. Resolves true only on an explicit yes. */
  confirm(message: string): Promise<boolean>;
  isInstalled(id: string): boolean;
  gatewayUrl(): string;
  /** A log line. Never receives a token. */
  warn(message: string): void;
}

export interface HelperBroker {
  get(request: HelperCredentialRequest): Promise<HelperCredential>;
}

const lower = (s: string): string => s.toLowerCase();

function credential(
  token: string | undefined,
  expiresAt: string | undefined,
  gatewayUrl: string,
): HelperCredential {
  const out = { expiresAt, gatewayUrl } as {
    expiresAt?: string;
    gatewayUrl: string;
    token?: string;
  };
  // Not enumerable: a spread, JSON.stringify or a log of the object does not carry the token along.
  Object.defineProperty(out, 'token', { value: token, enumerable: false });
  Object.defineProperty(out, 'toJSON', {
    value: () => ({ expiresAt, gatewayUrl }),
    enumerable: false,
  });
  return Object.freeze(out) as HelperCredential;
}

/**
 * The credential a companion may hand to a helper process it starts (an importer, say). The token that
 * the user signed in with never leaves the host: the gateway mints a NEW bearer for the helper, with the
 * same tenant and the same acting person, never admin, valid for minutes, with no refresh token. Who may
 * ask is decided here: an allowlisted extension id, or a one-time yes from the user. Everything refuses by
 * default.
 */
export function createHelperCredentialBroker(deps: HelperDeps): HelperBroker {
  const asking = new Map<string, Promise<boolean>>();

  async function allowed(id: string, audience: string, ttl: number): Promise<boolean> {
    const list = deps.allowlist();
    if (Array.isArray(list) && list.some((x) => typeof x === 'string' && lower(x) === lower(id))) {
      return true;
    }
    if (deps.consents.has(lower(id))) return true;
    const key = lower(id);
    let pending = asking.get(key);
    if (!pending) {
      pending = (async () => {
        const yes = await deps.confirm(
          `The extension ${id} asks for a short-lived credential for a helper process ("${audience}"). ` +
            `It acts as you on this Escurel gateway for up to ${Math.round(ttl / 60)} minute(s) per request, ` +
            'without admin rights and without a refresh token. Allow this extension to request them?',
        );
        if (yes) await deps.consents.grant(key);
        return yes;
      })().finally(() => asking.delete(key));
      asking.set(key, pending);
    }
    return pending;
  }

  return {
    async get(request: HelperCredentialRequest): Promise<HelperCredential> {
      if (typeof request !== 'object' || request === null || Array.isArray(request)) {
        throw new Error('getHelperCredential: expected an options object');
      }
      const { extensionId, audience, scopes, ttlSeconds } = request;
      if (typeof extensionId !== 'string' || !EXTENSION_ID.test(extensionId)) {
        throw new Error('getHelperCredential: extensionId must be the caller\'s "publisher.name"');
      }
      if (typeof audience !== 'string' || !AUDIENCE.test(audience)) {
        throw new Error(
          'getHelperCredential: audience is 1 to 40 letters, digits, "_" or "-", starting with a letter',
        );
      }
      if (scopes !== undefined && (!Array.isArray(scopes) || scopes.length > 0)) {
        throw new Error(
          'getHelperCredential: this gateway cannot narrow a credential by scope; omit scopes (the ' +
            'credential carries your own access, never admin)',
        );
      }
      const ttl = ttlSeconds ?? DEFAULT_TTL_SECONDS;
      if (!Number.isInteger(ttl) || ttl < 1 || ttl > MAX_TTL_SECONDS) {
        throw new Error(
          `getHelperCredential: ttlSeconds is a whole number from 1 to ${MAX_TTL_SECONDS}`,
        );
      }
      const gatewayUrl = deps.gatewayUrl();
      if (!(await deps.authRequired())) return credential(undefined, undefined, gatewayUrl);
      if (!deps.isInstalled(extensionId)) {
        throw new Error(`getHelperCredential: extension ${extensionId} is not installed`);
      }
      if (!(await allowed(extensionId, audience, ttl))) {
        deps.warn(`escurel: helper credential refused for ${extensionId}`);
        throw new Error(
          `getHelperCredential: ${extensionId} is not allowed to request a helper credential ` +
            '(add it to escurel.api.helperCredentialExtensions or accept the consent question)',
        );
      }
      let minted: { token?: unknown; expires_at?: unknown };
      try {
        minted = await deps.mint({ skill: `helper-${audience}`, ttl_secs: ttl });
      } catch (error) {
        let message = error instanceof Error ? error.message : String(error);
        // A failure after the mint may carry the bearer: strip anything token-shaped.
        message = message.replace(
          /[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}/g,
          '[redacted]',
        );
        throw new Error(
          `getHelperCredential: the gateway refused a helper credential (${message})`,
        );
      }
      if (typeof minted?.token !== 'string' || minted.token === '') {
        throw new Error('getHelperCredential: the gateway returned no credential');
      }
      return credential(
        minted.token,
        typeof minted.expires_at === 'string' ? minted.expires_at : undefined,
        gatewayUrl,
      );
    },
  };
}
