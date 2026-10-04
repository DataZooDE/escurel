import * as vscode from 'vscode';
import type { EscurelClient } from '../client';
import { describeError } from '../errors';
import { pageIdFromPath } from '../fs/read';
import {
  buildSkillPageModel,
  type SkillPageToHost,
  type SkillPageToWebview,
} from '../shared/skillPage';
import { safePost } from '../shared/safePost';

export const SKILL_VIEW_TYPE = 'escurel.skillPage';

/**
 * The readable page of a skill, as a CustomReadonlyEditor on `escurel:/skills/<id>.md`: the host reads the
 * skill row, its first instances and its latest events, folds them into the model and posts it. The
 * Markdown source stays available through Show Markdown (the title action and the button in the page).
 */
export class SkillPageEditor implements vscode.CustomReadonlyEditorProvider {
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
        SKILL_VIEW_TYPE,
        new SkillPageEditor(context, client, onDidChange),
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
    const parsed = pageIdFromPath(doc.uri.path);
    const pageId = parsed?.kind === 'skill' ? parsed.pageId : undefined;
    const skillId = pageId?.replace(/^markdown\/skills\//, '').replace(/\.md$/, '');
    panel.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.context.extensionUri, 'dist', 'webview')],
    };
    panel.webview.html = this.html(panel.webview);
    const post = (m: SkillPageToWebview) => safePost(panel, m);
    const load = async () => {
      if (!skillId)
        return post({ type: 'error', message: `not a skill page: ${doc.uri.toString()}` });
      post({ type: 'loading' });
      try {
        const c = this.client();
        const skills = await c.listSkills();
        const skill = skills.find((s) => s.id === skillId);
        if (!skill)
          return post({ type: 'error', message: `skill ${skillId} is not in the catalogue` });
        // The two lists are additions to the page: a failure in either degrades to empty, it must not
        // cost the person the skill itself.
        const [instances, events] = await Promise.all([
          c
            .listInstancesPage({ skill_id: skillId, order_by: 'at desc', limit: 11 })
            .then((r) => r.instances)
            .catch(() => []),
          c
            .listEvents({ label_skill: skillId, newest_first: true, limit: 8 })
            .then((r) => r.events)
            .catch(() => []),
        ]);
        post({ type: 'skill', model: buildSkillPageModel(skill, instances, events) });
      } catch (err) {
        post({ type: 'error', message: describeError(err) });
      }
    };
    const subs: vscode.Disposable[] = [
      panel.webview.onDidReceiveMessage((m: SkillPageToHost) => this.onMessage(m, pageId, load)),
      this.onDidChange(() => void load()),
      panel.onDidChangeViewState((e) => {
        if (e.webviewPanel.active) void load();
      }),
    ];
    panel.onDidDispose(() => subs.forEach((s) => s.dispose()));
  }

  private onMessage(
    m: SkillPageToHost,
    pageId: string | undefined,
    load: () => Promise<void>,
  ): void {
    switch (m.type) {
      case 'ready':
      case 'refresh':
        return void load();
      case 'show-raw':
        return void vscode.commands.executeCommand('escurel.showRaw', pageId);
      case 'open-page':
        return void vscode.commands.executeCommand('escurel.openPage', m.pageId);
      case 'open-thread':
        return void vscode.commands.executeCommand('escurel.openThread', m.rootEventId);
      case 'open-run':
        return void vscode.commands.executeCommand('escurel.openRun', m.runId);
      case 'start-skill':
        return void vscode.commands.executeCommand('escurel.startSkill', {
          skill: m.skill,
          mode: m.mode,
        });
    }
  }

  private html(webview: vscode.Webview): string {
    const script = webview.asWebviewUri(
      vscode.Uri.joinPath(this.context.extensionUri, 'dist', 'webview', 'skill-page.js'),
    );
    const nonce = Array.from({ length: 16 }, () =>
      Math.floor(Math.random() * 36).toString(36),
    ).join('');
    return `<!doctype html><html><head><meta charset="utf-8" />
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; script-src 'nonce-${nonce}';" />
<style>body{margin:0}</style></head>
<body><escurel-skill-page></escurel-skill-page><script type="module" nonce="${nonce}" src="${script}"></script></body></html>`;
  }
}
