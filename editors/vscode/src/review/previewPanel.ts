import * as vscode from 'vscode';
import type { Draft } from '../client';
import type { GatewayClient } from '../api/types';
import type { AugmentCommand } from '../shared/augment';
import { COMMAND_ID } from '../shared/augment';
import type { ReviewPreviewHostToWebview, ReviewPreviewWebviewToHost } from '../shared/protocol';
import { safePost } from '../shared/safePost';
import { THEME_CSS } from '../shared/themeTokens';
import { webviewHtml, webviewOptions } from '../shared/webviewHtml';
import { collectPreview, type ReviewHookRegistry } from './hooks';

export interface ReviewPreviewDeps {
  hooks: ReviewHookRegistry;
  gateway: GatewayClient;
  extensionUri: vscode.Uri;
  /** Test seam: the editor panel beside the diff. */
  createPanel?: () => vscode.WebviewPanel;
}

/**
 * The panel beside a review diff that shows what companion hooks add (a reference-impact preview, say).
 * It is drawn by the core webview from the declarative schema; the webview names only handles, and the
 * host runs the command it holds for one. Without a section to show, no panel exists.
 */
export class ReviewPreviewPanel implements vscode.Disposable {
  private panel: vscode.WebviewPanel | undefined;
  private shown: { commands: Map<string, AugmentCommand>; message: ReviewPreviewHostToWebview } | undefined;
  private subs: vscode.Disposable[] = [];

  constructor(private readonly deps: ReviewPreviewDeps) {}

  async show(draft: Draft): Promise<void> {
    if (!this.deps.hooks.hasPreview) return;
    const collected = await collectPreview(this.deps.hooks, {
      draftId: draft.draft_id,
      targetPageId: draft.target_page_id,
      client: this.deps.gateway,
    });
    if (!collected) {
      this.shown = undefined;
      this.panel?.dispose();
      return;
    }
    this.shown = {
      commands: collected.commands,
      message: { type: 'review-preview', view: collected.view },
    };
    if (!this.panel) this.open();
    else this.panel.reveal(undefined, true);
    if (this.panel) safePost(this.panel, this.shown.message);
  }

  private open(): void {
    const panel =
      this.deps.createPanel?.() ??
      vscode.window.createWebviewPanel(
        'escurel.reviewPreview',
        'Review context',
        { viewColumn: vscode.ViewColumn.Beside, preserveFocus: true },
        webviewOptions(this.deps.extensionUri),
      );
    panel.webview.options = webviewOptions(this.deps.extensionUri);
    panel.title = 'Review context';
    panel.webview.html = webviewHtml(
      panel.webview,
      this.deps.extensionUri,
      'review-preview',
      'escurel-review-preview',
      THEME_CSS,
    );
    this.panel = panel;
    this.subs.push(
      panel.webview.onDidReceiveMessage((m: ReviewPreviewWebviewToHost) => this.onMessage(m)),
    );
    panel.onDidDispose(() => {
      this.subs.forEach((s) => s.dispose());
      this.subs = [];
      this.panel = undefined;
    });
  }

  /** The webview is not trusted: it can name a handle the host handed out, nothing else. */
  private onMessage(m: ReviewPreviewWebviewToHost): void {
    if (m?.type === 'ready') {
      if (this.panel && this.shown) safePost(this.panel, this.shown.message);
      return;
    }
    if (m?.type !== 'augment-action' || typeof m.id !== 'string') return;
    const cmd = this.shown?.commands.get(m.id);
    if (!cmd || !COMMAND_ID.test(cmd.command)) return;
    void vscode.commands.executeCommand(cmd.command, ...cmd.args);
  }

  dispose(): void {
    this.panel?.dispose();
    this.subs.forEach((s) => s.dispose());
    this.subs = [];
  }
}
