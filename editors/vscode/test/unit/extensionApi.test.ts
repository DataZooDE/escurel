import { describe, expect, it, vi } from 'vitest';
import * as vscode from 'vscode';
import { createContributions, createExtensionApi } from '../../src/api';
import { gatewayFacade } from '../../src/api/gateway';

function setup(gatewayUrl = 'http://gw.test') {
  const changed = new vscode.EventEmitter<void>();
  const calls: [string, unknown][] = [];
  const deps = {
    client: () => ({
      callTool: <T>(name: string, args?: Record<string, unknown>) => {
        calls.push([name, args]);
        return Promise.resolve({ ok: true } as T);
      },
    }),
    gatewayUrl: () => gatewayUrl,
    onDidChange: changed.event,
  };
  const contributions = createContributions(deps);
  const api = createExtensionApi(deps, contributions);
  return { api, changed, calls, contributions };
}

describe('the public extension API', () => {
  it('is versioned and frozen', () => {
    const { api } = setup();
    expect(api.apiVersion).toBe(1);
    expect(Object.isFrozen(api)).toBe(true);
  });

  it('calls a tool with the session and holds no token', async () => {
    const { api, calls } = setup();
    const client = api.getClient();
    await expect(client.callTool('list_skills', {})).resolves.toEqual({ ok: true });
    expect(calls).toEqual([['list_skills', {}]]);
    expect(Object.keys(client)).toEqual(['callTool']);
    expect(Object.keys(api).sort()).toEqual([
      'apiVersion',
      'getClient',
      'onDidChangeTenant',
      'registerOverviewTile',
      'registerPageAugment',
      'registerPageRenderer',
      'themeCss',
      'themeTokens',
      'webviewHtml',
    ]);
  });

  it('accepts the connected tenant and rejects another', () => {
    const { api } = setup('http://gw.test/');
    expect(() => api.getClient('http://gw.test')).not.toThrow();
    expect(() => api.getClient('http://other.test')).toThrow(/not connected/);
  });

  it('re-validates what a caller passes to callTool', async () => {
    const { api, calls } = setup();
    const c = api.getClient();
    await expect(c.callTool('Bad Name')).rejects.toThrow(/tool name/);
    await expect(c.callTool('../x')).rejects.toThrow(/tool name/);
    await expect(c.callTool('expand', [] as unknown as Record<string, unknown>)).rejects.toThrow(
      /object/,
    );
    await expect(
      c.callTool('expand', new (class X {})() as unknown as Record<string, unknown>),
    ).rejects.toThrow(/object/);
    expect(calls).toEqual([]);
  });

  it('looks the client up per call, so a rebuilt client is used', async () => {
    let n = 0;
    const seen: number[] = [];
    const f = gatewayFacade(() => {
      const id = ++n;
      return { callTool: <T>() => (seen.push(id), Promise.resolve(undefined as T)) };
    });
    await f.callTool('a');
    await f.callTool('a');
    expect(seen).toEqual([1, 2]);
  });

  it('fires onDidChangeTenant when the connection changes', () => {
    const { api, changed } = setup();
    const seen: unknown[] = [];
    api.onDidChangeTenant((t) => seen.push(t));
    changed.fire();
    expect(seen).toEqual([{ gatewayUrl: 'http://gw.test' }]);
  });

  it('registers and unregisters a tile and a page renderer; a duplicate is refused', () => {
    const { api, contributions } = setup();
    const tile = { id: 'acme.example', title: 'Example', load: async () => ({ headline: 'x' }) };
    const onChange = vi.fn();
    contributions.tiles.onDidChange(onChange);
    const d = api.registerOverviewTile(tile);
    expect(contributions.tiles.size).toBe(1);
    expect(() => api.registerOverviewTile(tile)).toThrow(/already/);
    d.dispose();
    expect(contributions.tiles.size).toBe(0);
    expect(onChange).toHaveBeenCalledTimes(2);

    const r = { render: () => undefined };
    const rd = api.registerPageRenderer('acme-thing', r);
    expect(contributions.renderers.get('acme-thing')).toBe(r);
    expect(() => api.registerPageRenderer('acme-thing', r)).toThrow(/already/);
    expect(contributions.renderers.get('__proto__')).toBeUndefined();
    rd.dispose();
    expect(contributions.renderers.get('acme-thing')).toBeUndefined();
  });

  it('refuses an invalid tile or renderer', () => {
    const { api } = setup();
    const load = async () => ({ headline: 'x' });
    expect(() => api.registerOverviewTile({ id: 'Bad Id', title: 't', load })).toThrow();
    expect(() => api.registerOverviewTile({ id: 'a', title: ' ', load })).toThrow();
    expect(() =>
      api.registerOverviewTile({ id: 'a', title: 't', load, openCommand: 'rm -rf' }),
    ).toThrow();
    expect(() => api.registerPageRenderer('', { render: () => undefined })).toThrow();
    expect(() => api.registerPageRenderer('s', {} as never)).toThrow();
  });
});

describe('webviewHtml for a companion', () => {
  const webview = {
    cspSource: 'vscode-resource:',
    asWebviewUri: (u: { path: string }) => `vscode-resource:${u.path}`,
  } as unknown as vscode.Webview;
  const extensionUri = vscode.Uri.file('/ext/acme');

  it('builds a nonce-gated shell with the theme tokens and no network', () => {
    const { api } = setup();
    const html = api.webviewHtml({ webview, extensionUri, bundle: 'main', tag: 'acme-board' });
    expect(html).toContain("default-src 'none'");
    expect(html).toMatch(/script-src 'nonce-[^']+'/);
    expect(html).toContain('<acme-board></acme-board>');
    expect(html).toContain('--escurel-skill:var(--vscode-charts-purple)');
    expect(html).not.toMatch(/#[0-9a-f]{3,8}\b/i);
    expect(html).not.toContain('connect-src');
  });

  it('refuses a bundle or element name that could break out of the markup', () => {
    const { api } = setup();
    const base = { webview, extensionUri };
    expect(() => api.webviewHtml({ ...base, bundle: '../x', tag: 'a-b' })).toThrow();
    expect(() => api.webviewHtml({ ...base, bundle: 'x', tag: 'a-b><script>' })).toThrow();
    expect(() => api.webviewHtml({ ...base, bundle: 'x', tag: 'nohyphen' })).toThrow();
  });
});
