import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import type { EscurelExtensionApi } from '../../../src/api/types';
import { activate } from './support';

// The shell options against the real host: a real Uri, a real asWebviewUri and a real webview panel.
suite('webviewHtml shell options', () => {
  let api: EscurelExtensionApi;
  suiteSetup(async function () {
    this.timeout(120_000);
    api = (await activate()) as unknown as EscurelExtensionApi;
  });

  test('fonts, lang and title land in the shell with the webview origin as the only font source', () => {
    const ext = vscode.extensions.getExtension('datazoo.escurel')!;
    const panel = vscode.window.createWebviewPanel('escurelShell', 't', vscode.ViewColumn.One, {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(ext.extensionUri, 'dist', 'webview')],
    });
    try {
      const html = api.webviewHtml({
        webview: panel.webview,
        extensionUri: ext.extensionUri,
        bundle: 'overview',
        tag: 'acme-board',
        lang: 'de',
        title: 'Board & more',
        fonts: [
          {
            family: 'Acme Sans',
            file: vscode.Uri.joinPath(ext.extensionUri, 'dist', 'webview', 'fonts', 'a.woff2'),
            weight: 700,
          },
        ],
      });
      assert.match(html, /<html lang="de">/);
      assert.match(html, /<title>Board &amp; more<\/title>/);
      assert.ok(html.includes(`font-src ${panel.webview.cspSource};`));
      assert.match(html, /@font-face\{font-family:"Acme Sans";src:url\("[^"]*fonts\/a\.woff2"\)/);
      assert.ok(!/connect-src/.test(html));
      assert.throws(
        () =>
          api.webviewHtml({
            webview: panel.webview,
            extensionUri: ext.extensionUri,
            bundle: 'overview',
            tag: 'acme-board',
            fonts: [{ family: 'X', file: vscode.Uri.file('/etc/fonts/x.woff2') }],
          }),
        /under the companion/,
      );
    } finally {
      panel.dispose();
    }
  });
});
