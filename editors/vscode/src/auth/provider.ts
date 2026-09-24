import * as vscode from 'vscode';
import { createServer, type Server } from 'node:http';
import { log } from '../log';
import { readConfig } from '../config';
import {
  authorizationUrl,
  decodeSubject,
  discover,
  exchangeCode,
  pkce,
  pollDeviceToken,
  randomState,
  refreshTokens,
  startDeviceFlow,
  type IssuerMetadata,
  type TokenResponse,
} from './oidc';
import { TokenRefresher, type StoredSession } from './refresher';

export const AUTH_PROVIDER_ID = 'escurel';
const SECRET_KEY = 'escurel.session';

/**
 * The `escurel` AuthenticationProvider (SPEC §1): OIDC against the issuer
 * in `escurel.auth.issuer`, PKCE over a loopback redirect reached through
 * `vscode.env.asExternalUri` (so it works in remote / web windows), with
 * the device-authorization grant as the fallback. One session at most;
 * it lives in SecretStorage and is refreshed by the shared TokenRefresher.
 */
export class EscurelAuthProvider implements vscode.AuthenticationProvider, vscode.Disposable {
  private readonly changed =
    new vscode.EventEmitter<vscode.AuthenticationProviderAuthenticationSessionsChangeEvent>();
  readonly onDidChangeSessions = this.changed.event;
  readonly refresher: TokenRefresher;
  private readonly disposables: vscode.Disposable[] = [];

  constructor(private readonly secrets: vscode.SecretStorage) {
    this.refresher = new TokenRefresher({
      load: () => this.load(),
      save: (s) => this.save(s),
      refresh: async (s) => {
        const meta = await discover(s.issuer);
        const tokens = await refreshTokens(meta, {
          clientId: s.clientId,
          refreshToken: s.refreshToken!,
        });
        return toSession(tokens, s.issuer, s.clientId, s.subject);
      },
    });
    this.disposables.push(
      this.changed,
      vscode.authentication.registerAuthenticationProvider(AUTH_PROVIDER_ID, 'Escurel', this, {
        supportsMultipleAccounts: false,
      }),
    );
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
  }

  /** True when `escurel.auth.issuer` is set; otherwise the gateway runs without a verifier and no token is sent. */
  static configured(): boolean {
    return readConfig().auth.issuer.trim() !== '';
  }

  private async load(): Promise<StoredSession | undefined> {
    const raw = await this.secrets.get(SECRET_KEY);
    if (!raw) return undefined;
    try {
      return JSON.parse(raw) as StoredSession;
    } catch {
      return undefined;
    }
  }

  private async save(s: StoredSession | undefined): Promise<void> {
    if (s) await this.secrets.store(SECRET_KEY, JSON.stringify(s));
    else await this.secrets.delete(SECRET_KEY);
  }

  async getSessions(): Promise<vscode.AuthenticationSession[]> {
    const s = await this.load();
    return s ? [asVsSession(s)] : [];
  }

  async createSession(scopes: readonly string[]): Promise<vscode.AuthenticationSession> {
    const cfg = readConfig().auth;
    if (!cfg.issuer)
      throw new Error(
        'escurel.auth.issuer is not set (a gateway without a verifier needs no sign-in)',
      );
    if (!cfg.clientId) throw new Error('escurel.auth.clientId is not set');
    const meta = await discover(cfg.issuer);
    const wanted = scopes.length ? [...scopes] : cfg.scopes;
    let tokens: TokenResponse;
    try {
      tokens = await this.pkceFlow(meta, cfg.clientId, wanted);
    } catch (e) {
      log().warn(
        `escurel: PKCE sign-in did not complete (${(e as Error).message}); falling back to the device code`,
      );
      tokens = await this.deviceFlow(meta, cfg.clientId, wanted);
    }
    const session = toSession(
      tokens,
      meta.issuer,
      cfg.clientId,
      decodeSubject(tokens.access_token) ?? 'user',
    );
    await this.save(session);
    const vs = asVsSession(session);
    this.changed.fire({ added: [vs], removed: [], changed: [] });
    return vs;
  }

  async removeSession(): Promise<void> {
    const s = await this.load();
    await this.save(undefined);
    if (s) this.changed.fire({ added: [], removed: [asVsSession(s)], changed: [] });
  }

  /** Authorization code + PKCE over a loopback listener, opened in the user's browser. */
  private async pkceFlow(
    meta: IssuerMetadata,
    clientId: string,
    scopes: string[],
  ): Promise<TokenResponse> {
    const { verifier, challenge } = pkce();
    const state = randomState();
    const { server, port } = await listenLoopback();
    try {
      const external = await vscode.env.asExternalUri(
        vscode.Uri.parse(`http://127.0.0.1:${port}/callback`),
      );
      const redirectUri = external.toString(true);
      const code = new Promise<string>((resolve, reject) => {
        const timer = setTimeout(
          () => reject(new Error('timed out waiting for the browser')),
          5 * 60_000,
        );
        server.on('request', (req, res) => {
          const u = new URL(req.url ?? '/', 'http://127.0.0.1');
          if (u.pathname !== '/callback') return void res.writeHead(404).end();
          const err = u.searchParams.get('error');
          const got = u.searchParams.get('code');
          if (u.searchParams.get('state') !== state || err || !got) {
            res
              .writeHead(400, { 'content-type': 'text/plain' })
              .end(`escurel: sign-in failed (${err ?? 'bad state'}). You can close this tab.`);
            clearTimeout(timer);
            return void reject(new Error(err ?? 'state mismatch'));
          }
          res
            .writeHead(200, { 'content-type': 'text/plain' })
            .end('escurel: signed in. You can close this tab.');
          clearTimeout(timer);
          resolve(got);
        });
      });
      const url = authorizationUrl(meta, { clientId, redirectUri, scopes, challenge, state });
      const opened = await vscode.env.openExternal(vscode.Uri.parse(url));
      if (!opened) throw new Error('could not open the browser');
      return await exchangeCode(meta, { clientId, code: await code, verifier, redirectUri });
    } finally {
      server.close();
    }
  }

  /** RFC 8628: show the user code, open the verification page, poll. */
  private async deviceFlow(
    meta: IssuerMetadata,
    clientId: string,
    scopes: string[],
  ): Promise<TokenResponse> {
    const device = await startDeviceFlow(meta, { clientId, scopes });
    const open = 'Open browser';
    void vscode.window
      .showInformationMessage(
        `Sign in to escurel: enter the code ${device.user_code} at ${device.verification_uri}`,
        open,
      )
      .then((pick) => {
        if (pick === open)
          void vscode.env.openExternal(
            vscode.Uri.parse(device.verification_uri_complete ?? device.verification_uri),
          );
      });
    return vscode.window.withProgress(
      {
        location: vscode.ProgressLocation.Notification,
        title: `escurel: waiting for code ${device.user_code} to be approved…`,
        cancellable: true,
      },
      (_p, ct) => {
        const ac = new AbortController();
        ct.onCancellationRequested(() => ac.abort());
        return pollDeviceToken(meta, { clientId, device, signal: ac.signal });
      },
    );
  }
}

function listenLoopback(): Promise<{ server: Server; port: number }> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () =>
      resolve({ server, port: (server.address() as { port: number }).port }),
    );
  });
}

function toSession(
  t: TokenResponse,
  issuer: string,
  clientId: string,
  subject: string,
): StoredSession {
  return { accessToken: t.access_token, refreshToken: t.refresh_token, subject, issuer, clientId };
}

function asVsSession(s: StoredSession): vscode.AuthenticationSession {
  return {
    id: `${s.issuer}#${s.subject}`,
    accessToken: s.accessToken,
    account: { id: s.subject, label: s.subject },
    scopes: [],
  };
}
