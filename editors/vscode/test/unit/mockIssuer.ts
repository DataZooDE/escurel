// A fake OIDC issuer: discovery, PKCE authorization-code exchange, refresh,
// and the device-authorization grant with `authorization_pending` polling.
import { createServer, type Server } from 'node:http';
import { createHash } from 'node:crypto';

export interface MockIssuer {
  url: string;
  /** The PKCE challenge the /authorize request carried, and the code it minted. */
  authorize: (params: URLSearchParams) => { code: string; redirect: string };
  approveDevice: () => void;
  tokenRequests: Record<string, string>[];
  close: () => Promise<void>;
}

const b64url = (b: Buffer) =>
  b.toString('base64').replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');

export async function startMockIssuer(
  opts: { clientId: string; ttl?: number } = { clientId: 'vscode' },
): Promise<MockIssuer> {
  const ttl = opts.ttl ?? 3600;
  const codes = new Map<string, { challenge: string; redirect: string }>();
  const refreshTokens = new Set<string>();
  let deviceApproved = false;
  let deviceCode = '';
  const tokenRequests: Record<string, string>[] = [];
  let n = 0;
  const mint = (sub: string) => {
    const now = Math.floor(Date.now() / 1000);
    const payload = b64url(
      Buffer.from(
        JSON.stringify({
          iss: url,
          sub,
          aud: 'escurel',
          tenant: 't',
          roles: ['escurel:admin'],
          iat: now,
          exp: now + ttl,
        }),
      ),
    );
    const rt = `rt-${++n}`;
    refreshTokens.add(rt);
    return {
      access_token: `h.${payload}.s${n}`,
      token_type: 'Bearer',
      expires_in: ttl,
      refresh_token: rt,
      id_token: `h.${payload}.i`,
    };
  };
  let url = '';
  const server: Server = createServer((req, res) => {
    let raw = '';
    req.on('data', (c) => (raw += c));
    req.on('end', () => {
      const json = (status: number, body: unknown) =>
        res.writeHead(status, { 'content-type': 'application/json' }).end(JSON.stringify(body));
      const path = new URL(req.url ?? '/', url).pathname;
      if (path === '/.well-known/openid-configuration') {
        return json(200, {
          issuer: url,
          authorization_endpoint: `${url}/authorize`,
          token_endpoint: `${url}/token`,
          device_authorization_endpoint: `${url}/device`,
          jwks_uri: `${url}/jwks.json`,
        });
      }
      if (path === '/device') {
        deviceCode = `dev-${++n}`;
        return json(200, {
          device_code: deviceCode,
          user_code: 'ABCD-EFGH',
          verification_uri: `${url}/activate`,
          verification_uri_complete: `${url}/activate?user_code=ABCD-EFGH`,
          expires_in: 600,
          interval: 0,
        });
      }
      if (path === '/token') {
        const form = Object.fromEntries(new URLSearchParams(raw));
        tokenRequests.push(form);
        if (form.client_id !== opts.clientId) return json(401, { error: 'invalid_client' });
        if (form.grant_type === 'authorization_code') {
          const c = codes.get(form.code ?? '');
          if (!c) return json(400, { error: 'invalid_grant' });
          const expected = b64url(
            createHash('sha256')
              .update(form.code_verifier ?? '')
              .digest(),
          );
          if (expected !== c.challenge || form.redirect_uri !== c.redirect)
            return json(400, { error: 'invalid_grant', error_description: 'pkce' });
          codes.delete(form.code!);
          return json(200, mint('alice'));
        }
        if (form.grant_type === 'refresh_token') {
          if (!refreshTokens.delete(form.refresh_token ?? ''))
            return json(400, { error: 'invalid_grant' });
          return json(200, mint('alice'));
        }
        if (form.grant_type === 'urn:ietf:params:oauth:grant-type:device_code') {
          if (form.device_code !== deviceCode) return json(400, { error: 'invalid_grant' });
          if (!deviceApproved) return json(400, { error: 'authorization_pending' });
          return json(200, mint('bob'));
        }
        return json(400, { error: 'unsupported_grant_type' });
      }
      res.writeHead(404).end();
    });
  });
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
  url = `http://127.0.0.1:${(server.address() as { port: number }).port}`;
  return {
    url,
    tokenRequests,
    authorize: (params) => {
      const code = `code-${++n}`;
      codes.set(code, {
        challenge: params.get('code_challenge') ?? '',
        redirect: params.get('redirect_uri') ?? '',
      });
      return {
        code,
        redirect: `${params.get('redirect_uri')}?code=${code}&state=${params.get('state')}`,
      };
    },
    approveDevice: () => (deviceApproved = true),
    close: () => new Promise((r) => server.close(() => r())),
  };
}
