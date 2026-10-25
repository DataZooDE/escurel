import { describe, expect, it, vi } from 'vitest';
import * as vscode from 'vscode';
import { createContributions, createExtensionApi } from '../../src/api';
import { createHelperCredentialBroker, type HelperDeps } from '../../src/api/helperCredential';

const TOKEN = 'eyJhbGciOiJSUzI1NiJ9.payload-part.signature-part';

function broker(over: Partial<HelperDeps> = {}) {
  const mints: Record<string, unknown>[] = [];
  const consents = new Set<string>();
  const asked: string[] = [];
  const logged: string[] = [];
  const deps: HelperDeps = {
    authRequired: async () => true,
    mint: async (req) => {
      mints.push(req);
      return { token: TOKEN, expires_at: '2030-01-01T00:05:00Z' };
    },
    allowlist: () => [],
    consents: { has: (id) => consents.has(id), grant: async (id) => void consents.add(id) },
    confirm: async (message) => (asked.push(message), true),
    isInstalled: (id) => id === 'acme.example' || id === 'acme.other',
    gatewayUrl: () => 'http://gw.test',
    warn: (m) => void logged.push(m),
    ...over,
  };
  return { b: createHelperCredentialBroker(deps), mints, consents, asked, logged };
}
const ask = { extensionId: 'acme.example', audience: 'importer' };

describe('getHelperCredential: what a granted call returns', () => {
  it('mints a short-lived token for the helper through the gateway, same tenant', async () => {
    const { b, mints } = broker();
    const c = await b.get(ask);
    expect(c.token).toBe(TOKEN);
    expect(c.expiresAt).toBe('2030-01-01T00:05:00Z');
    expect(c.gatewayUrl).toBe('http://gw.test');
    expect(mints).toEqual([{ skill: 'helper-importer', ttl_secs: 300 }]);
  });

  it('passes a requested TTL up to the cap', async () => {
    const { b, mints } = broker();
    await b.get({ ...ask, ttlSeconds: 3600 });
    expect(mints[0]).toMatchObject({ ttl_secs: 3600 });
  });

  it('keeps the token out of JSON, enumeration and string conversion', async () => {
    const c = await broker().b.get(ask);
    expect(JSON.stringify(c)).not.toContain('eyJ');
    expect(Object.keys(c)).not.toContain('token');
    expect(String(c)).not.toContain('eyJ');
    expect(Object.isFrozen(c)).toBe(true);
    const inspected = (await import('node:util')).inspect(c);
    expect(inspected).not.toContain('eyJ');
  });

  it('returns no token when the gateway runs without a verifier, and asks nothing', async () => {
    const { b, mints, asked } = broker({ authRequired: async () => false });
    const c = await b.get(ask);
    expect(c.token).toBeUndefined();
    expect(c.gatewayUrl).toBe('http://gw.test');
    expect(mints).toEqual([]);
    expect(asked).toEqual([]);
  });
});

describe('getHelperCredential: consent and allowlist', () => {
  it('asks once, naming the extension, the audience and the lifetime, then remembers', async () => {
    const { b, asked, mints } = broker();
    await b.get(ask);
    await b.get(ask);
    expect(asked).toHaveLength(1);
    expect(asked[0]).toContain('acme.example');
    expect(asked[0]).toContain('importer');
    expect(mints).toHaveLength(2);
  });

  it('asks again for another extension', async () => {
    const { b, asked } = broker();
    await b.get(ask);
    await b.get({ ...ask, extensionId: 'acme.other' });
    expect(asked).toHaveLength(2);
  });

  it('does not ask for an extension on the allowlist', async () => {
    const { b, asked } = broker({ allowlist: () => ['Acme.Example'] });
    await b.get(ask);
    expect(asked).toEqual([]);
  });

  it('refuses when the user declines, mints nothing and does not remember', async () => {
    let answer = false;
    const { b, mints, consents } = broker({ confirm: async () => answer });
    await expect(b.get(ask)).rejects.toThrow(/not allowed/);
    expect(mints).toEqual([]);
    expect(consents.size).toBe(0);
    answer = true;
    await expect(b.get(ask)).resolves.toBeDefined();
  });

  it('asks only once for concurrent first calls', async () => {
    let release!: (v: boolean) => void;
    const confirm = vi.fn(() => new Promise<boolean>((r) => (release = r)));
    const { b, mints } = broker({ confirm });
    const calls = [b.get(ask), b.get(ask), b.get(ask)];
    await Promise.resolve();
    release(true);
    await Promise.all(calls);
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(mints).toHaveLength(3);
  });

  it('refuses an extension that is not installed, even if allowlisted', async () => {
    const { b, mints } = broker({ allowlist: () => ['acme.ghost'] });
    await expect(b.get({ ...ask, extensionId: 'acme.ghost' })).rejects.toThrow(/not installed/);
    expect(mints).toEqual([]);
  });

  it('treats an allowlist that is not an array of strings as empty', async () => {
    const { b, asked } = broker({
      allowlist: () => 'acme.example' as unknown as string[],
      confirm: async () => (asked.push('q'), false),
    });
    await expect(b.get(ask)).rejects.toThrow(/not allowed/);
    expect(asked).toEqual(['q']);
  });
});

describe('getHelperCredential: input refusal', () => {
  it.each([
    ['no extension id', { audience: 'importer' }],
    ['an extension id that is not publisher.name', { extensionId: 'x', audience: 'importer' }],
    ['an extension id with markup', { extensionId: 'a.b"><x', audience: 'importer' }],
    ['no audience', { extensionId: 'acme.example' }],
    ['an audience with a space', { ...ask, audience: 'my importer' }],
    ['an audience that is too long', { ...ask, audience: 'a'.repeat(41) }],
    ['an audience with a colon', { ...ask, audience: 'agent:admin' }],
    ['a zero TTL', { ...ask, ttlSeconds: 0 }],
    ['a fractional TTL', { ...ask, ttlSeconds: 1.5 }],
    ['a TTL over the cap', { ...ask, ttlSeconds: 3601 }],
    ['a string TTL', { ...ask, ttlSeconds: '60' }],
    ['scopes the gateway cannot narrow to', { ...ask, scopes: ['admin'] }],
    ['scopes that are not a list', { ...ask, scopes: 'x' }],
  ])('refuses %s without asking or minting', async (_n, input) => {
    const { b, mints, asked } = broker();
    await expect(b.get(input as never)).rejects.toThrow();
    expect(mints).toEqual([]);
    expect(asked).toEqual([]);
  });

  it('refuses a call without an options object', async () => {
    await expect(broker().b.get(undefined as never)).rejects.toThrow();
    await expect(broker().b.get('importer' as never)).rejects.toThrow();
  });

  it('accepts an empty scopes list', async () => {
    await expect(broker().b.get({ ...ask, scopes: [] })).resolves.toBeDefined();
  });
});

describe('getHelperCredential: failure handling never leaks the token', () => {
  it('redacts the token from a mint failure and logs nothing secret', async () => {
    const { b, logged } = broker({
      mint: async () => {
        throw new Error(`gateway said no to ${TOKEN}`);
      },
    });
    const err = await b.get(ask).catch((e: Error) => e);
    expect((err as Error).message).not.toContain(TOKEN);
    expect((err as Error).message).toMatch(/helper credential/);
    expect(logged.join('\n')).not.toContain(TOKEN);
  });

  it('refuses a gateway answer without a token', async () => {
    const { b } = broker({ mint: async () => ({}) as never });
    await expect(b.get(ask)).rejects.toThrow(/no credential/);
  });

  it('never writes the token to the log on success', async () => {
    const { b, logged } = broker();
    await b.get(ask);
    expect(logged.join('\n')).not.toContain(TOKEN);
    expect(logged.join('\n')).not.toContain('eyJ');
  });
});

describe('the API member', () => {
  it('is exposed, validated and frozen with the API', async () => {
    const changed = new vscode.EventEmitter<void>();
    const { b } = broker();
    const deps = {
      client: () => ({ callTool: <T>() => Promise.resolve(undefined as T) }),
      gatewayUrl: () => 'http://gw.test',
      onDidChange: changed.event,
      helper: b,
    };
    const api = createExtensionApi(deps, createContributions(deps));
    expect(typeof api.getHelperCredential).toBe('function');
    await expect(api.getHelperCredential(ask)).resolves.toMatchObject({
      gatewayUrl: 'http://gw.test',
    });
    await expect(api.getHelperCredential({ ...ask, audience: '' })).rejects.toThrow();
  });

  it('refuses when the host wired no broker', async () => {
    const changed = new vscode.EventEmitter<void>();
    const deps = {
      client: () => ({ callTool: <T>() => Promise.resolve(undefined as T) }),
      gatewayUrl: () => 'http://gw.test',
      onDidChange: changed.event,
    };
    const api = createExtensionApi(deps, createContributions(deps));
    await expect(api.getHelperCredential(ask)).rejects.toThrow(/not available/);
  });
});
