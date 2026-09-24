// Pure OIDC helpers (no vscode import): discovery, PKCE, the code exchange,
// the refresh grant and the device-authorization grant. Everything here is
// `fetch` + URL-encoded forms, so it is unit-tested against a mock issuer.
import { createHash, randomBytes } from 'node:crypto';

export interface IssuerMetadata {
  issuer: string;
  authorization_endpoint: string;
  token_endpoint: string;
  device_authorization_endpoint?: string;
  jwks_uri?: string;
}

export interface TokenResponse {
  access_token: string;
  token_type: string;
  expires_in?: number;
  refresh_token?: string;
  id_token?: string;
  scope?: string;
}

export interface DeviceAuthorization {
  device_code: string;
  user_code: string;
  verification_uri: string;
  verification_uri_complete?: string;
  expires_in: number;
  interval?: number;
}

export class OidcError extends Error {
  constructor(
    readonly error: string,
    description?: string,
    readonly status?: number,
  ) {
    super(description ? `${error}: ${description}` : error);
    this.name = 'OidcError';
  }
}

const b64url = (b: Buffer): string =>
  b.toString('base64').replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');

export async function discover(issuer: string): Promise<IssuerMetadata> {
  const res = await fetch(`${issuer.replace(/\/+$/, '')}/.well-known/openid-configuration`);
  if (!res.ok) throw new OidcError('discovery_failed', `${issuer}: HTTP ${res.status}`, res.status);
  return (await res.json()) as IssuerMetadata;
}

/** RFC 7636 S256: a fresh verifier and its challenge. */
export function pkce(): { verifier: string; challenge: string; method: 'S256' } {
  const verifier = b64url(randomBytes(32));
  return {
    verifier,
    challenge: b64url(createHash('sha256').update(verifier).digest()),
    method: 'S256',
  };
}

export function randomState(): string {
  return b64url(randomBytes(16));
}

export function authorizationUrl(
  meta: IssuerMetadata,
  p: { clientId: string; redirectUri: string; scopes: string[]; challenge: string; state: string },
): string {
  const u = new URL(meta.authorization_endpoint);
  u.search = new URLSearchParams({
    response_type: 'code',
    client_id: p.clientId,
    redirect_uri: p.redirectUri,
    scope: p.scopes.join(' '),
    code_challenge: p.challenge,
    code_challenge_method: 'S256',
    state: p.state,
  }).toString();
  return u.toString();
}

async function tokenRequest(
  meta: IssuerMetadata,
  form: Record<string, string>,
): Promise<TokenResponse> {
  const res = await fetch(meta.token_endpoint, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded', accept: 'application/json' },
    body: new URLSearchParams(form).toString(),
  });
  const body = (await res.json().catch(() => ({}))) as Partial<TokenResponse> & {
    error?: string;
    error_description?: string;
  };
  if (!res.ok || body.error)
    throw new OidcError(body.error ?? `http_${res.status}`, body.error_description, res.status);
  if (!body.access_token) throw new OidcError('invalid_response', 'no access_token');
  return body as TokenResponse;
}

export function exchangeCode(
  meta: IssuerMetadata,
  p: { clientId: string; code: string; verifier: string; redirectUri: string },
): Promise<TokenResponse> {
  return tokenRequest(meta, {
    grant_type: 'authorization_code',
    client_id: p.clientId,
    code: p.code,
    code_verifier: p.verifier,
    redirect_uri: p.redirectUri,
  });
}

export function refreshTokens(
  meta: IssuerMetadata,
  p: { clientId: string; refreshToken: string; scopes?: string[] },
): Promise<TokenResponse> {
  const form: Record<string, string> = {
    grant_type: 'refresh_token',
    client_id: p.clientId,
    refresh_token: p.refreshToken,
  };
  if (p.scopes?.length) form.scope = p.scopes.join(' ');
  return tokenRequest(meta, form);
}

export async function startDeviceFlow(
  meta: IssuerMetadata,
  p: { clientId: string; scopes: string[] },
): Promise<DeviceAuthorization> {
  if (!meta.device_authorization_endpoint)
    throw new OidcError(
      'device_flow_unsupported',
      'the issuer publishes no device_authorization_endpoint',
    );
  const res = await fetch(meta.device_authorization_endpoint, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded', accept: 'application/json' },
    body: new URLSearchParams({ client_id: p.clientId, scope: p.scopes.join(' ') }).toString(),
  });
  const body = (await res.json().catch(() => ({}))) as Partial<DeviceAuthorization> & {
    error?: string;
    error_description?: string;
  };
  if (!res.ok || body.error)
    throw new OidcError(body.error ?? `http_${res.status}`, body.error_description, res.status);
  return body as DeviceAuthorization;
}

/** RFC 8628 §3.4/3.5: poll until the user approves, honouring `slow_down`; rejects on `expired_token` / `access_denied` / abort. */
export async function pollDeviceToken(
  meta: IssuerMetadata,
  p: {
    clientId: string;
    device: DeviceAuthorization;
    onPending?: () => void;
    signal?: AbortSignal;
    sleep?: (ms: number) => Promise<void>;
  },
): Promise<TokenResponse> {
  const sleep = p.sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms)));
  let interval = Math.max(0, p.device.interval ?? 5) * 1000;
  const deadline = Date.now() + p.device.expires_in * 1000;
  for (;;) {
    if (p.signal?.aborted) throw new OidcError('access_denied', 'cancelled');
    if (Date.now() > deadline) throw new OidcError('expired_token', 'the device code expired');
    try {
      return await tokenRequest(meta, {
        grant_type: 'urn:ietf:params:oauth:grant-type:device_code',
        client_id: p.clientId,
        device_code: p.device.device_code,
      });
    } catch (e) {
      if (!(e instanceof OidcError)) throw e;
      if (e.error === 'authorization_pending') p.onPending?.();
      else if (e.error === 'slow_down') interval += 5000;
      else throw e;
    }
    await sleep(interval);
  }
}

/** The `exp` claim of a JWT without verifying it (the gateway verifies; this only schedules refreshes). */
export function decodeExp(jwt: string): number | undefined {
  const payload = jwt.split('.')[1];
  if (!payload) return undefined;
  try {
    const exp = (
      JSON.parse(Buffer.from(payload, 'base64url').toString('utf8')) as { exp?: unknown }
    ).exp;
    return typeof exp === 'number' ? exp : undefined;
  } catch {
    return undefined;
  }
}

/** The `sub` claim of a JWT (display only). */
export function decodeSubject(jwt: string): string | undefined {
  const payload = jwt.split('.')[1];
  if (!payload) return undefined;
  try {
    const sub = (
      JSON.parse(Buffer.from(payload, 'base64url').toString('utf8')) as { sub?: unknown }
    ).sub;
    return typeof sub === 'string' ? sub : undefined;
  } catch {
    return undefined;
  }
}
