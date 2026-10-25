import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelApi } from '../../../src/extension';
import type { EscurelExtensionApi } from '../../../src/api/types';
import { activate } from './support';

const EXT = 'datazoo.escurel';

function claims(jwt: string): Record<string, unknown> {
  return JSON.parse(Buffer.from(jwt.split('.')[1]!, 'base64url').toString('utf8')) as Record<
    string,
    unknown
  >;
}

// The helper credential in a real extension host against a verifying gateway: the gateway really
// mints it, and it is a different, short-lived, non-admin bearer for the same person.
suite('helper credential', () => {
  let api: EscurelApi & EscurelExtensionApi;
  const setting = () => vscode.workspace.getConfiguration('escurel');
  let previous: unknown;

  suiteSetup(async function () {
    this.timeout(120_000);
    api = (await activate()) as EscurelApi & EscurelExtensionApi;
    previous = setting().inspect('api.helperCredentialExtensions')?.globalValue;
    // The extension asks the user once for an unlisted caller; a headless window cannot answer.
    await setting().update(
      'api.helperCredentialExtensions',
      [EXT],
      vscode.ConfigurationTarget.Global,
    );
  });

  suiteTeardown(async () => {
    await setting().update(
      'api.helperCredentialExtensions',
      previous,
      vscode.ConfigurationTarget.Global,
    );
  });

  test('getSubject is the signed-in subject and not a token', async () => {
    const subject = await api.getSubject();
    assert.equal(subject, process.env.ESCUREL_TEST_SUBJECT ?? 'alice');
    assert.ok(!String(subject).includes('.'), 'no JWT in it');
  });

  test('the gateway mints a short-lived, non-admin credential for the same person', async function () {
    this.timeout(60_000);
    const c = await api.getHelperCredential({
      extensionId: EXT,
      audience: 'importer',
      ttlSeconds: 120,
    });
    assert.ok(c.token, 'a token');
    assert.notEqual(c.token, process.env.ESCUREL_TEST_BEARER, 'not the user session token');
    const p = claims(c.token!);
    const ttl = Number(p.exp) - Number(p.iat);
    assert.ok(ttl > 0 && ttl <= 120, `ttl ${ttl}`);
    assert.equal(
      (p.act as { sub?: string } | undefined)?.sub,
      process.env.ESCUREL_TEST_SUBJECT ?? 'alice',
    );
    assert.ok(!JSON.stringify(p).includes('"escurel:admin"'), 'never admin');
    assert.ok(!JSON.stringify(c).includes(c.token!), 'not in the JSON of the credential');
    assert.equal(c.gatewayUrl.replace(/\/+$/, ''), api.services.gatewayUrl.replace(/\/+$/, ''));
    // And it works as a bearer.
    const res = await fetch(new URL('/mcp', c.gatewayUrl), {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        accept: 'application/json, text/event-stream',
        authorization: `Bearer ${c.token}`,
      },
      body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list' }),
    });
    assert.equal(res.status, 200);
  });

  test('refuses an extension that is not installed, and a scope it cannot honour', async () => {
    await assert.rejects(
      api.getHelperCredential({ extensionId: 'acme.not-installed', audience: 'importer' }),
      /not installed/,
    );
    await assert.rejects(
      api.getHelperCredential({ extensionId: EXT, audience: 'importer', scopes: ['admin'] }),
      /scope/,
    );
  });
});
