import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import {
  discover,
  pkce,
  exchangeCode,
  refreshTokens,
  startDeviceFlow,
  pollDeviceToken,
  decodeExp,
} from '../../src/auth/oidc';
import { startMockIssuer, type MockIssuer } from './mockIssuer';

let issuer: MockIssuer;
beforeAll(async () => (issuer = await startMockIssuer({ clientId: 'vscode', ttl: 120 })));
afterAll(() => issuer.close());

describe('oidc', () => {
  it('discovers the endpoints from the issuer', async () => {
    const meta = await discover(issuer.url);
    expect(meta.authorization_endpoint).toBe(`${issuer.url}/authorize`);
    expect(meta.token_endpoint).toBe(`${issuer.url}/token`);
    expect(meta.device_authorization_endpoint).toBe(`${issuer.url}/device`);
  });

  it('PKCE: the code exchange carries the verifier that matches the challenge', async () => {
    const meta = await discover(issuer.url);
    const { verifier, challenge } = pkce();
    expect(challenge).not.toBe(verifier);
    const redirect = 'http://127.0.0.1:5555/callback';
    const { code } = issuer.authorize(
      new URLSearchParams({ code_challenge: challenge, redirect_uri: redirect, state: 's' }),
    );
    const tokens = await exchangeCode(meta, {
      clientId: 'vscode',
      code,
      verifier,
      redirectUri: redirect,
    });
    expect(tokens.access_token).toMatch(/^h\./);
    expect(tokens.refresh_token).toBeDefined();
    expect(decodeExp(tokens.access_token)! - Math.floor(Date.now() / 1000)).toBeGreaterThan(100);
    // A wrong verifier is refused by the issuer.
    const { code: code2 } = issuer.authorize(
      new URLSearchParams({ code_challenge: challenge, redirect_uri: redirect, state: 's' }),
    );
    await expect(
      exchangeCode(meta, {
        clientId: 'vscode',
        code: code2,
        verifier: 'nope',
        redirectUri: redirect,
      }),
    ).rejects.toThrow(/invalid_grant/);
  });

  it('refresh_token grant returns a new pair; a used refresh token is refused', async () => {
    const meta = await discover(issuer.url);
    const { verifier, challenge } = pkce();
    const redirect = 'http://127.0.0.1:5555/callback';
    const { code } = issuer.authorize(
      new URLSearchParams({ code_challenge: challenge, redirect_uri: redirect, state: 's' }),
    );
    const first = await exchangeCode(meta, {
      clientId: 'vscode',
      code,
      verifier,
      redirectUri: redirect,
    });
    const second = await refreshTokens(meta, {
      clientId: 'vscode',
      refreshToken: first.refresh_token!,
    });
    expect(second.access_token).not.toBe(first.access_token);
    await expect(
      refreshTokens(meta, { clientId: 'vscode', refreshToken: first.refresh_token! }),
    ).rejects.toThrow(/invalid_grant/);
  });

  it('device flow: starts, polls through authorization_pending, then completes', async () => {
    const meta = await discover(issuer.url);
    const dev = await startDeviceFlow(meta, { clientId: 'vscode', scopes: ['openid'] });
    expect(dev.user_code).toBe('ABCD-EFGH');
    expect(dev.verification_uri_complete).toContain('user_code=');
    const polls: string[] = [];
    const done = pollDeviceToken(meta, {
      clientId: 'vscode',
      device: dev,
      onPending: () => polls.push('pending'),
    });
    await new Promise((r) => setTimeout(r, 30));
    issuer.approveDevice();
    const tokens = await done;
    expect(polls.length).toBeGreaterThan(0);
    expect(tokens.access_token).toMatch(/^h\./);
  });
});
