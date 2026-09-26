import * as vscode from 'vscode';
import type { Draft, EscurelClient } from '../client';
import { describeError } from '../errors';
import { log } from '../log';
import { buildReviewCommentThreads } from './reviewModel';
import { decodeReviewUri, encodeReviewUri } from './uri';

/**
 * Bridges draft review comments with VS Code's native Comments API.
 * Comments are anchored on the proposed markdown document. Existing comments
 * are fetched from page events matching `escurel:review-comment` and grouped
 * into threads. New comments are captured back to the gateway.
 */
export class ReviewCommentsController implements vscode.Disposable {
  private readonly controller: vscode.CommentController;
  private readonly threadsByDraft = new Map<string, vscode.CommentThread[]>();
  private readonly threadLineMap = new WeakMap<vscode.CommentThread, number | undefined>();
  private readonly inFlightLoads = new Map<string, Promise<void>>();
  private readonly disposables: vscode.Disposable[] = [];

  get commentController(): vscode.CommentController {
    return this.controller;
  }

  constructor(private readonly client: () => EscurelClient) {
    this.controller = vscode.comments.createCommentController('escurel-review', 'Escurel Review');
    this.disposables.push(this.controller);

    // Commenting is only enabled on the proposed side of a draft diff.
    this.controller.commentingRangeProvider = {
      provideCommentingRanges: (document: vscode.TextDocument): vscode.Range[] => {
        const decoded = decodeReviewUri(document.uri);
        if (decoded && decoded.side === 'proposed') {
          return [new vscode.Range(0, 0, Math.max(0, document.lineCount - 1), 0)];
        }
        return [];
      },
    };

    this.disposables.push(
      vscode.commands.registerCommand('escurel.submitComment', (reply: vscode.CommentReply) =>
        this.submitComment(reply),
      ),
    );
  }

  static register(
    context: vscode.ExtensionContext,
    client: () => EscurelClient,
  ): ReviewCommentsController {
    const commentsController = new ReviewCommentsController(client);
    context.subscriptions.push(commentsController);
    return commentsController;
  }

  /**
   * Fetches existing review comments for a draft from page events and groups them
   * into line threads on the proposed diff document.
   */
  async loadCommentsForDraft(draft: Draft): Promise<void> {
    const inFlight = this.inFlightLoads.get(draft.draft_id);
    if (inFlight) {
      // Coalesce with the running load: multiple callers (e.g. diff open + document open)
      // await the same fetch rather than creating duplicate thread widgets in VS Code.
      await inFlight;
      return;
    }

    const loadPromise = this.loadCommentsForDraftInternal(draft);
    this.inFlightLoads.set(draft.draft_id, loadPromise);
    try {
      await loadPromise;
    } finally {
      this.inFlightLoads.delete(draft.draft_id);
    }
  }

  private async loadCommentsForDraftInternal(draft: Draft): Promise<void> {
    const proposedUri = encodeReviewUri(draft.draft_id, 'proposed');

    try {
      const eventsResponse = await this.client().listEvents({
        instance_page_id: draft.target_page_id,
        include_system: true,
      });

      const threadModels = buildReviewCommentThreads(eventsResponse.events, draft.draft_id);

      // Clean up stale threads immediately before creating the new set.
      // Disposing and creating in the same synchronous turn prevents overlapping
      // calls or window events from seeing a half-cleared or duplicated thread state.
      const existing = this.threadsByDraft.get(draft.draft_id);
      if (existing) {
        for (const t of existing) {
          t.dispose();
        }
        this.threadsByDraft.delete(draft.draft_id);
      }

      const createdThreads: vscode.CommentThread[] = [];

      for (const tm of threadModels) {
        // Line numbers in provenance are 1-indexed. Unanchored comments (line: undefined)
        // are placed at line 0 (top of file) to match the thread ordering spec.
        const lineIdx = tm.line !== undefined ? Math.max(0, tm.line - 1) : 0;
        const range = new vscode.Range(lineIdx, 0, lineIdx, 0);

        const vscodeComments: vscode.Comment[] = tm.comments.map((c) => ({
          author: { name: c.author },
          body: new vscode.MarkdownString(c.body),
          mode: vscode.CommentMode.Preview,
          timestamp: c.at ? new Date(c.at) : undefined,
        }));

        const thread = this.controller.createCommentThread(proposedUri, range, vscodeComments);
        thread.collapsibleState = vscode.CommentThreadCollapsibleState.Expanded;
        thread.canReply = true;
        this.threadLineMap.set(thread, tm.line);
        createdThreads.push(thread);
      }

      this.threadsByDraft.set(draft.draft_id, createdThreads);
    } catch (err) {
      log().warn(
        `review: failed to load comments for draft ${draft.draft_id}: ${describeError(err)}`,
      );
    }
  }

  /**
   * Submits a new comment or reply. The gateway assigns author and lineage;
   * only label_skill, body, and review provenance are submitted.
   * If capture fails, the comment draft text remains in the editor input.
   */
  async submitComment(reply: vscode.CommentReply): Promise<void> {
    const { thread, text } = reply;
    if (!text || !text.trim()) return;

    const decoded = decodeReviewUri(thread.uri);
    if (!decoded) return;

    let line: number | undefined;
    if (this.threadLineMap.has(thread)) {
      // Known thread: could be unanchored (undefined) or already registered with a line.
      line = this.threadLineMap.get(thread);
    } else if (thread.range) {
      // Fresh thread started by reviewer clicking a line gutter in the diff editor.
      line = thread.range.start.line + 1;
      this.threadLineMap.set(thread, line);
    }

    try {
      await this.client().captureEvent({
        label_skill: 'escurel:review-comment',
        mime: 'text/plain',
        source: 'workbench',
        body: text,
        provenance: {
          review: {
            draft_id: decoded.draftId,
            ...(line !== undefined ? { line } : {}),
          },
        },
      });

      const newComment: vscode.Comment = {
        author: { name: 'You' },
        body: new vscode.MarkdownString(text),
        mode: vscode.CommentMode.Preview,
        timestamp: new Date(),
      };
      thread.comments = [...thread.comments, newComment];
      thread.canReply = true;
    } catch (err) {
      void vscode.window.showErrorMessage(describeError(err));
    }
  }

  dispose(): void {
    for (const threads of this.threadsByDraft.values()) {
      for (const t of threads) {
        t.dispose();
      }
    }
    this.threadsByDraft.clear();
    this.inFlightLoads.clear();
    for (const d of this.disposables) {
      d.dispose();
    }
    this.disposables.length = 0;
  }
}
