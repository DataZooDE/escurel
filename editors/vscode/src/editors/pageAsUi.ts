import * as vscode from 'vscode';
import type { EscurelClient } from '../client';
import { describeError } from '../errors';
import { pageIdFromPath } from '../fs/read';
import { log } from '../log';
import { buildPageModel } from '../shared/page';
import type { HostToWebview, WebviewToHost } from '../shared/protocol';

export const VIEW_TYPE = 'escurel.pageAsUi';

/**
 * Page as UI (SPEC §3.4) as a CustomReadonlyEditor on `escurel:` instance
 * URIs: the host reads `expand` + the skill row, folds them into the
 * PageModel and posts it; the webview never sees a token or a tool name.
 */
export class PageAsUiEditor implements vscode.CustomReadonlyEditorProvider {
  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly client: () => EscurelClient,
    private readonly onDidChange: vscode.Event<void>,
  ) {}

  static register(
    context: vscode.ExtensionContext,
    client: () => EscurelClient,
    onDidChange: vscode.Event<void>,
  ): void {
    context.subscriptions.push(
      vscode.window.registerCustomEditorProvider(
        VIEW_TYPE,
        new PageAsUiEditor(context, client, onDidChange),
        {
          webviewOptions: { retainContextWhenHidden: true },
          supportsMultipleEditorsPerDocument: true,
        },
      ),
    );
  }

  openCustomDocument(uri: vscode.Uri): vscode.CustomDocument {
    return { uri, dispose: () => undefined };
  }

  async resolveCustomEditor(doc: vscode.CustomDocument, panel: vscode.WebviewPanel): Promise<void> {
    const pageId = pageIdFromPath(doc.uri.path)?.pageId;
    panel.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.context.extensionUri, 'dist', 'webview')],
    };
    panel.webview.html = this.html(panel.webview);
    const post = (m: HostToWebview) => void panel.webview.postMessage(m);
    const load = async () => {
      if (!pageId)
        return post({ type: 'error', message: `not an escurel page: ${doc.uri.toString()}` });
      post({ type: 'loading' });
      try {
        const c = this.client();
        const [e, skills] = await Promise.all([c.expand({ page_id: pageId }), c.listSkills()]);
        if (!e.page)
          return post({
            type: 'error',
            message: 'This page does not exist, or you may not read it.',
          });
        const skill = skills.find((s) => s.id === e.page!.skill);
        if (!skill)
          return post({ type: 'error', message: `skill ${e.page.skill} is not in the catalogue` });
        post({ type: 'page', model: buildPageModel(e, skill) });
      } catch (err) {
        post({ type: 'error', message: describeError(err) });
      }
    };
    const subs: vscode.Disposable[] = [
      panel.webview.onDidReceiveMessage((m: WebviewToHost) => this.onMessage(m, pageId, load)),
      this.onDidChange(() => void load()),
      panel.onDidChangeViewState((e) => {
        if (e.webviewPanel.active) void load();
      }),
    ];
    panel.onDidDispose(() => subs.forEach((s) => s.dispose()));
  }

  private onMessage(m: WebviewToHost, pageId: string | undefined, load: () => Promise<void>): void {
    switch (m.type) {
      case 'ready':
      case 'refresh':
        return void load();
      case 'open-page':
        return void vscode.commands.executeCommand('escurel.openPage', m.pageId);
      case 'view-skill':
        return void vscode.commands.executeCommand('escurel.viewSkill', m.skill);
      case 'show-raw':
        return void vscode.commands.executeCommand('escurel.showRaw', pageId);
      case 'start-skill':
        log().info(`escurel: start ${m.skill} (${m.mode}) on ${pageId}`);
        return void vscode.commands.executeCommand('escurel.startSkill', {
          skill: m.skill,
          pageId,
          mode: m.mode,
        });
    }
  }

  private html(webview: vscode.Webview): string {
    const script = webview.asWebviewUri(
      vscode.Uri.joinPath(this.context.extensionUri, 'dist', 'webview', 'page-as-ui.js'),
    );
    const nonce = Array.from({ length: 16 }, () =>
      Math.floor(Math.random() * 36).toString(36),
    ).join('');
    return `<!doctype html><html><head><meta charset="utf-8" />
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}';" />
<style>body{margin:0}</style></head>
<body><escurel-page-as-ui></escurel-page-as-ui><script type="module" nonce="${nonce}" src="${script}"></script></body></html>`;
  }
}
