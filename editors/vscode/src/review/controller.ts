import * as vscode from 'vscode';
import type { DiffDraftResponse, Draft, EscurelClient } from '../client';
import { EscurelError } from '../client/errors';
import { describeError } from '../errors';
import { log } from '../log';
import { ReviewCommentsController } from './commentsController';
import { ReviewContentProvider } from './contentProvider';
import {
  buildChangesetQuickPickItems,
  checkBaseMoved,
  formatDraftDiffTitle,
  interpretDiscardError,
  interpretDiscardResult,
  interpretPromoteChangesetResult,
  interpretPromoteDraftError,
  interpretPromoteDraftSuccess,
  resolveReviewTarget,
  type ChangesetQuickPickItem,
  type DecisionOutcome,
} from './reviewModel';
import { pluralise } from '../shared/text';
import { decodeReviewUri, encodeReviewUri } from './uri';

/**
 * Orchestrates the Escurel Review Surface.
 * Wires the diff view, document content provider, comments controller,
 * and review decision commands (escurel.openReview, escurel.promote, escurel.discard).
 */
export class ReviewController implements vscode.Disposable {
  private readonly disposables: vscode.Disposable[] = [];
  private readonly openingDrafts = new Set<string>();

  constructor(
    private readonly client: () => EscurelClient,
    private readonly contentProvider: ReviewContentProvider,
    private readonly commentsController: ReviewCommentsController,
    private readonly refreshAwaiting: () => void,
  ) {
    this.disposables.push(
      vscode.commands.registerCommand('escurel.openReview', (arg?: unknown) =>
        this.openReview(arg),
      ),
      vscode.commands.registerCommand('escurel.promote', (arg?: unknown) => this.promote(arg)),
      vscode.commands.registerCommand('escurel.discard', (arg?: unknown) => this.discard(arg)),
      vscode.workspace.onDidOpenTextDocument(async (doc) => {
        const decoded = decodeReviewUri(doc.uri);
        if (decoded && decoded.side === 'proposed') {
          // When openDraftReview explicitly opens a diff, it loads comments itself
          // after vscode.diff resolves. Skip here so document activation during diff
          // creation does not duplicate that explicit load. Window reload does not go
          // through openDraftReview, so this path remains the sole loader for restored tabs.
          if (this.openingDrafts.has(decoded.draftId)) {
            return;
          }
          const draft = this.contentProvider.getDraft(decoded.draftId);
          if (draft) {
            await this.commentsController.loadCommentsForDraft(draft);
          }
        }
      }),
    );
  }

  static register(
    context: vscode.ExtensionContext,
    client: () => EscurelClient,
    refreshAwaiting: () => void,
  ): ReviewController {
    const contentProvider = ReviewContentProvider.register(context, client);
    const commentsController = ReviewCommentsController.register(context, client);
    const controller = new ReviewController(
      client,
      contentProvider,
      commentsController,
      refreshAwaiting,
    );
    context.subscriptions.push(controller);
    return controller;
  }

  /**
   * Opens the review surface for a draft, changeset, or confirm gate row.
   */
  async openReview(arg?: unknown): Promise<void> {
    if (arg && typeof arg === 'object' && 'kind' in arg) {
      if (arg.kind === 'confirm_gate') {
        void vscode.window.showInformationMessage(
          'escurel: this run requires confirmation but has not proposed a markdown change.',
        );
        return;
      }
    }

    const target = resolveReviewTarget(arg);
    if (target?.kind === 'changeset') {
      await this.openChangesetReview(target.changesetId);
      return;
    }

    if (target?.kind === 'draft') {
      await this.openDraftReview(target.draftId, arg);
      return;
    }

    // Direct draft object passed
    if (arg && typeof arg === 'object' && 'target_page_id' in arg && 'content' in arg) {
      await this.openDraftReview(arg as Draft);
      return;
    }

    void vscode.window.showInformationMessage('escurel: select an item awaiting review.');
  }

  /**
   * Displays a QuickPick of drafts within a changeset, offering batch promote/discard
   * actions as well as opening individual draft diffs.
   */
  async openChangesetReview(changesetId: string): Promise<void> {
    try {
      const drafts = await this.client().listDrafts();
      const csDrafts = drafts.filter((d) => d.changeset_id === changesetId);

      if (csDrafts.length === 0) {
        void vscode.window.showInformationMessage(
          `escurel: changeset ${changesetId} has no open drafts.`,
        );
        return;
      }

      // Fetch diffs in parallel to annotate each QuickPick draft item.
      const diffMap = new Map<string, DiffDraftResponse>();
      await Promise.all(
        csDrafts.map(async (d) => {
          try {
            const diff = await this.client().diffDraft({ draft_id: d.draft_id });
            diffMap.set(d.draft_id, diff);
          } catch (err) {
            log().warn(`review: diff_draft failed for ${d.draft_id}: ${describeError(err)}`);
          }
        }),
      );

      const items = buildChangesetQuickPickItems(changesetId, csDrafts, diffMap);
      const selected = await vscode.window.showQuickPick<ChangesetQuickPickItem>(items, {
        placeHolder: `Changeset ${changesetId} (${pluralise(csDrafts.length, 'draft')})`,
      });

      if (!selected) return;

      if (selected.action === 'promote_all') {
        await this.promote({ changesetId });
      } else if (selected.action === 'discard_all') {
        await this.discard({ changesetId });
      } else if (selected.action === 'draft' && selected.draft) {
        await this.openDraftReview(selected.draft);
      }
    } catch (err) {
      void vscode.window.showErrorMessage(
        `escurel: failed to load changeset ${changesetId} — ${describeError(err)}`,
      );
    }
  }

  /**
   * Opens the diff view for a draft, checking for base_moved and loading review comments.
   */
  async openDraftReview(draftOrId: Draft | string, rawArg?: unknown): Promise<void> {
    let draft: Draft | undefined;

    if (typeof draftOrId === 'object') {
      draft = draftOrId;
    } else {
      draft = this.contentProvider.getDraft(draftOrId);
      if (!draft && rawArg && typeof rawArg === 'object' && 'draft' in rawArg) {
        draft = (rawArg as { draft: Draft }).draft;
      }
      if (!draft) {
        try {
          const drafts = await this.client().listDrafts();
          draft = drafts.find((d) => d.draft_id === draftOrId);
        } catch (err) {
          log().warn(`review: listDrafts failed: ${describeError(err)}`);
        }
      }
    }

    if (!draft) {
      const id = typeof draftOrId === 'string' ? draftOrId : 'unknown';
      void vscode.window.showErrorMessage(`escurel: draft ${id} not found or no longer open.`);
      return;
    }

    this.contentProvider.setDraft(draft);

    try {
      const diff = await this.client().diffDraft({ draft_id: draft.draft_id });
      const check = checkBaseMoved(diff);
      if (check.baseMoved && check.warning) {
        void vscode.window.showWarningMessage(`escurel: ${check.warning}`);
      }
    } catch (err) {
      log().warn(`review: diff_draft failed for ${draft.draft_id}: ${describeError(err)}`);
    }

    const baseUri = encodeReviewUri(draft.draft_id, 'base');
    const proposedUri = encodeReviewUri(draft.draft_id, 'proposed');
    const title = formatDraftDiffTitle(draft);

    this.openingDrafts.add(draft.draft_id);
    try {
      await vscode.commands.executeCommand('vscode.diff', baseUri, proposedUri, title);
      await this.commentsController.loadCommentsForDraft(draft);
    } finally {
      this.openingDrafts.delete(draft.draft_id);
    }
  }

  /**
   * Promotes a draft or changeset.
   */
  async promote(arg?: unknown): Promise<void> {
    const activeUri = vscode.window.activeTextEditor?.document.uri.toString();
    const target = resolveReviewTarget(arg, activeUri);

    if (!target) {
      void vscode.window.showErrorMessage('escurel: no active review or item selected to promote.');
      return;
    }

    let outcome: DecisionOutcome;

    if (target.kind === 'draft') {
      try {
        await this.client().promoteDraft({ draft_id: target.draftId });
        outcome = interpretPromoteDraftSuccess(target.draftId);
        void vscode.window.showInformationMessage(`escurel: ${outcome.message}`);
      } catch (err) {
        outcome = interpretPromoteDraftError(err, target.draftId);
        if (outcome.kind === 'already_decided') {
          void vscode.window.showInformationMessage(`escurel: ${outcome.message}`);
        } else {
          void vscode.window.showErrorMessage(`escurel: ${outcome.message}`);
        }
      }

      if (outcome.refresh) {
        this.refreshAwaiting();
      }
      if (outcome.closeDiff) {
        await this.closeDiffIfOpen(target.draftId);
      }
      return;
    }

    // target.kind === 'changeset'
    try {
      const res = await this.client().promoteChangeset({ changeset_id: target.changesetId });
      outcome = interpretPromoteChangesetResult(res);
      if (outcome.kind === 'partial') {
        void vscode.window.showWarningMessage(`escurel: ${outcome.message}`);
      } else {
        void vscode.window.showInformationMessage(`escurel: ${outcome.message}`);
      }
    } catch (err) {
      if (err instanceof EscurelError && err.kind === 'already_decided') {
        outcome = {
          kind: 'already_decided',
          message: `Changeset ${target.changesetId} was already decided.`,
          closeDiff: true,
          refresh: true,
        };
        void vscode.window.showInformationMessage(`escurel: ${outcome.message}`);
      } else {
        outcome = {
          kind: 'error',
          message: describeError(err),
          closeDiff: false,
          refresh: false,
        };
        void vscode.window.showErrorMessage(`escurel: ${outcome.message}`);
      }
    }

    if (outcome.refresh) {
      this.refreshAwaiting();
    }
    if (outcome.closeDiff) {
      await this.closeDiffIfOpen();
    }
  }

  /**
   * Discards a draft or changeset, prompting the user for an optional reason.
   */
  async discard(arg?: unknown): Promise<void> {
    const activeUri = vscode.window.activeTextEditor?.document.uri.toString();
    const target = resolveReviewTarget(arg, activeUri);

    if (!target) {
      void vscode.window.showErrorMessage('escurel: no active review or item selected to discard.');
      return;
    }

    const input = await vscode.window.showInputBox({
      prompt: `Reason for discarding ${target.kind === 'draft' ? 'draft' : 'changeset'} (optional)`,
      placeHolder: 'Optional discard reason',
    });
    // User pressed Escape to cancel
    if (input === undefined) return;
    const reason = input.trim() ? input.trim() : undefined;

    let outcome: DecisionOutcome;

    if (target.kind === 'draft') {
      try {
        await this.client().discardDraft({ draft_id: target.draftId, reason });
        outcome = interpretDiscardResult('draft', target.draftId);
        void vscode.window.showInformationMessage(`escurel: ${outcome.message}`);
      } catch (err) {
        outcome = interpretDiscardError(err, 'draft', target.draftId);
        if (outcome.kind === 'already_decided') {
          void vscode.window.showInformationMessage(`escurel: ${outcome.message}`);
        } else {
          void vscode.window.showErrorMessage(`escurel: ${outcome.message}`);
        }
      }

      if (outcome.refresh) {
        this.refreshAwaiting();
      }
      if (outcome.closeDiff) {
        await this.closeDiffIfOpen(target.draftId);
      }
      return;
    }

    // target.kind === 'changeset'
    try {
      await this.client().discardChangeset({ changeset_id: target.changesetId, reason });
      outcome = interpretDiscardResult('changeset', target.changesetId);
      void vscode.window.showInformationMessage(`escurel: ${outcome.message}`);
    } catch (err) {
      outcome = interpretDiscardError(err, 'changeset', target.changesetId);
      if (outcome.kind === 'already_decided') {
        void vscode.window.showInformationMessage(`escurel: ${outcome.message}`);
      } else {
        void vscode.window.showErrorMessage(`escurel: ${outcome.message}`);
      }
    }

    if (outcome.refresh) {
      this.refreshAwaiting();
    }
    if (outcome.closeDiff) {
      await this.closeDiffIfOpen();
    }
  }

  /**
   * Closes the active diff editor tab if it matches the decided review.
   */
  private async closeDiffIfOpen(draftId?: string): Promise<void> {
    const activeDoc = vscode.window.activeTextEditor?.document;
    if (!activeDoc) return;

    const decoded = decodeReviewUri(activeDoc.uri);
    if (decoded) {
      if (!draftId || decoded.draftId === draftId) {
        await vscode.commands.executeCommand('workbench.action.closeActiveEditor');
      }
    }
  }

  dispose(): void {
    for (const d of this.disposables) {
      d.dispose();
    }
    this.disposables.length = 0;
  }
}
