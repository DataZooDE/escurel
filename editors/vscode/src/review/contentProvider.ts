import * as vscode from 'vscode';
import type { Draft, EscurelClient } from '../client';
import { describeError } from '../errors';
import { readPageMarkdown } from '../fs/read';
import { log } from '../log';
import { REVIEW_SCHEME } from './reviewModel';
import { decodeReviewUri, encodeReviewUri } from './uri';

/**
 * Serves in-memory markdown content for base and proposed sides of a draft diff.
 * Backed by `escurel-review` scheme URIs to avoid temporary files on disk.
 */
export class ReviewContentProvider implements vscode.TextDocumentContentProvider {
  private readonly _onDidChange = new vscode.EventEmitter<vscode.Uri>();
  readonly onDidChange = this._onDidChange.event;
  private readonly draftCache = new Map<string, Draft>();

  constructor(private readonly client: () => EscurelClient) {}

  static register(
    context: vscode.ExtensionContext,
    client: () => EscurelClient,
  ): ReviewContentProvider {
    const provider = new ReviewContentProvider(client);
    context.subscriptions.push(
      vscode.workspace.registerTextDocumentContentProvider(REVIEW_SCHEME, provider),
      provider,
    );
    return provider;
  }

  setDraft(draft: Draft): void {
    this.draftCache.set(draft.draft_id, draft);
    this._onDidChange.fire(encodeReviewUri(draft.draft_id, 'base'));
    this._onDidChange.fire(encodeReviewUri(draft.draft_id, 'proposed'));
  }

  getDraft(draftId: string): Draft | undefined {
    return this.draftCache.get(draftId);
  }

  async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
    const decoded = decodeReviewUri(uri);
    if (!decoded) return '';
    const { draftId, side } = decoded;

    let draft = this.draftCache.get(draftId);
    if (!draft) {
      try {
        const drafts = await this.client().listDrafts();
        draft = drafts.find((d) => d.draft_id === draftId);
        if (draft) {
          this.draftCache.set(draftId, draft);
        }
      } catch (err) {
        log().warn(`review: failed to list drafts for ${draftId}: ${describeError(err)}`);
      }
    }

    if (!draft) return '';

    if (side === 'proposed') {
      return draft.content;
    }

    try {
      const page = await readPageMarkdown(this.client(), draft.target_page_id);
      return page?.text ?? '';
    } catch (err) {
      log().warn(`review: failed to read base page for draft ${draftId}: ${describeError(err)}`);
      return '';
    }
  }

  dispose(): void {
    this._onDidChange.dispose();
  }
}
