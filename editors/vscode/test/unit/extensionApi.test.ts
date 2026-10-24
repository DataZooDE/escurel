import { describe, expect, it, vi } from 'vitest';
import * as vscode from 'vscode';
import { createContributions, createExtensionApi } from '../../src/api';
import { gatewayFacade } from '../../src/api/gateway';

function setup(gatewayUrl = 'http://gw.test', subject?: () => Promise<string | undefined>) {
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
    subject,
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
      'getHelperCredential',
      'getSubject',
      'onDidChangeTenant',
      'registerFocusProfile',
      'registerInboxDecorator',
      'registerOverviewTile',
      'registerPageAugment',
      'registerPageRenderer',
      'registerReviewHook',
      'themeCss',
      'themeTokens',
      'webviewHtml',
    ]);
  });

  it('answers getSubject with the subject string and nothing else', async () => {
    const { api } = setup('http://gw.test', async () => 'user-42');
    await expect(api.getSubject()).resolves.toBe('user-42');
  });

  it('answers getSubject with undefined without a verifier or without a session', async () => {
    await expect(setup('http://gw.test', async () => undefined).api.getSubject()).resolves.toBe(
      undefined,
    );
    await expect(setup().api.getSubject()).resolves.toBe(undefined);
  });

  it('never lets a failing subject lookup leak its message (it may carry a token)', async () => {
    const { api } = setup('http://gw.test', async () => {
      throw new Error('bad token eyJhbGciOi.abc.def');
    });
    await expect(api.getSubject()).resolves.toBe(undefined);
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

  it('registers review hooks, rejects an empty one, and removes one on dispose', () => {
    const { api, contributions } = setup();
    const d = api.registerReviewHook({ before: () => undefined });
    expect(contributions.reviewHooks.size).toBe(1);
    expect(() => api.registerReviewHook({})).toThrow(/at least one/);
    d.dispose();
    expect(contributions.reviewHooks.size).toBe(0);
  });

  it('registers inbox decorators, rejects an empty one, and removes one on dispose', () => {
    const { api, contributions } = setup();
    const d = api.registerInboxDecorator('acme-request', { decorate: () => undefined });
    expect(contributions.inboxDecorators.size).toBe(1);
    expect(() => api.registerInboxDecorator('acme-request', {})).toThrow(/at least one/);
    d.dispose();
    expect(contributions.inboxDecorators.size).toBe(0);
  });
});

describe('webviewHtml shell options: fonts, lang, title', () => {
  const webview = {
    cspSource: 'vscode-resource:',
    asWebviewUri: (u: { path: string }) => `vscode-resource:${u.path}`,
  } as unknown as vscode.Webview;
  const extensionUri = vscode.Uri.file('/ext/acme');
  const font = (rel: string) => vscode.Uri.joinPath(extensionUri, rel);
  const base = { webview, extensionUri, bundle: 'main', tag: 'acme-board' };

  it('adds @font-face and font-src for a font under the companion, nothing else loosened', () => {
    const { api } = setup();
    const html = api.webviewHtml({
      ...base,
      fonts: [
        { family: 'Lato', file: font('dist/webview/fonts/Lato-Regular.woff2'), weight: 400 },
        {
          family: 'Lato',
          file: font('dist/webview/fonts/Lato-Bold.woff2'),
          weight: 700,
          style: 'italic',
        },
      ],
    });
    expect(html).toContain('font-src vscode-resource:;');
    expect(html).toContain(
      '@font-face{font-family:"Lato";src:url("vscode-resource:/ext/acme/dist/webview/fonts/Lato-Regular.woff2") format("woff2");font-weight:400;font-style:normal;font-display:swap}',
    );
    expect(html).toContain('font-weight:700;font-style:italic');
    expect(html).toContain("default-src 'none'");
    expect(html).not.toContain('connect-src');
    expect(html).not.toMatch(/(img|script|style|default)-src[^;]*\bhttps?:/);
  });

  it('has no font-src and no @font-face without fonts, and keeps the old shell otherwise', () => {
    const { api } = setup();
    const html = api.webviewHtml(base);
    expect(html).not.toContain('font-src');
    expect(html).not.toContain('@font-face');
    expect(html).not.toContain('<title>');
    expect(html).toMatch(/<html>/);
  });

  it.each([
    ['a font outside the companion', vscode.Uri.file('/etc/fonts/x.woff2')],
    ['a font reached with ..', vscode.Uri.file('/ext/acme/../other/x.woff2')],
    ['a sibling directory with the same prefix', vscode.Uri.file('/ext/acme-evil/x.woff2')],
    ['a file that is not a font', vscode.Uri.file('/ext/acme/dist/x.js')],
    ['a remote font', vscode.Uri.parse('https://evil.example/x.woff2')],
    [
      'a path that breaks out of a CSS string',
      vscode.Uri.file('/ext/acme/a");}body{x:url(".woff2'),
    ],
  ])('refuses %s', (_n, file) => {
    const { api } = setup();
    expect(() => api.webviewHtml({ ...base, fonts: [{ family: 'Lato', file }] })).toThrow();
  });

  it.each([
    ['a family with a quote', 'La"to'],
    ['a family with a brace', 'Lato}{'],
    ['an empty family', ''],
    ['a long family', 'L'.repeat(61)],
  ])('refuses %s', (_n, family) => {
    const { api } = setup();
    const file = font('dist/webview/fonts/a.woff2');
    expect(() => api.webviewHtml({ ...base, fonts: [{ family, file }] })).toThrow(/family/);
  });

  it('refuses a bad weight, a bad style, too many fonts and a non-list', () => {
    const { api } = setup();
    const file = font('dist/webview/fonts/a.woff2');
    const f = { family: 'Lato', file };
    expect(() => api.webviewHtml({ ...base, fonts: [{ ...f, weight: 450 }] })).toThrow(/weight/);
    expect(() =>
      api.webviewHtml({ ...base, fonts: [{ ...f, weight: '400;x' as never }] }),
    ).toThrow();
    expect(() =>
      api.webviewHtml({ ...base, fonts: [{ ...f, style: 'oblique;' as never }] }),
    ).toThrow(/style/);
    expect(() => api.webviewHtml({ ...base, fonts: Array(9).fill(f) })).toThrow(/at most/);
    expect(() => api.webviewHtml({ ...base, fonts: 'x' as never })).toThrow();
  });

  it('sets <html lang> from a language tag and refuses anything else', () => {
    const { api } = setup();
    expect(api.webviewHtml({ ...base, lang: 'de' })).toContain('<html lang="de">');
    expect(api.webviewHtml({ ...base, lang: 'pt-BR' })).toContain('<html lang="pt-BR">');
    for (const bad of ['', 'de"><script>', 'x', 'english-language-name', 'de_DE', 5]) {
      expect(() => api.webviewHtml({ ...base, lang: bad as never })).toThrow(/lang/);
    }
  });

  it('sets <title> escaped and bounded, refuses a non-string or an overlong one', () => {
    const { api } = setup();
    const html = api.webviewHtml({ ...base, title: 'A & B <script>alert(1)</script> "x"' });
    expect(html).toContain(
      '<title>A &amp; B &lt;script&gt;alert(1)&lt;/script&gt; &quot;x&quot;</title>',
    );
    expect(html).not.toContain('<script>alert');
    expect(api.webviewHtml({ ...base, title: 'a\u0000b c' })).toContain('<title>a b c</title>');
    expect(() => api.webviewHtml({ ...base, title: 'x'.repeat(201) })).toThrow(/title/);
    expect(() => api.webviewHtml({ ...base, title: 5 as never })).toThrow(/title/);
  });
});
