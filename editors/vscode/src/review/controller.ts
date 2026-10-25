import { notify } from '../commands/notify';
import { nodeRefs } from '../commands/nodeRefs';
import { noticeActions, type NoticeTarget } from '../shared/notice';
import * as vscode from 'vscode';
import type { DiffDraftResponse, Draft, EscurelClient } from '../client';
import { EscurelError } from '../client/errors';
import { describeError } from '../errors';
import { log } from '../log';
import { ReviewCommentsController } from './commentsController';
import { ReviewContentProvider } from './contentProvider';
import {
  buildChangesetQuickPickItems,
  changesetPickTitle,
  checkBaseMoved,
  formatDraftDiffTitle,
  interpretDiscardError,
  interpretDiscardResult,
  interpretPromoteChangesetResult,
  interpretPromoteDraftError,
  interpretPromoteDraftSuccess,
  resolveReviewTarget,
  type ChangesetQuickPickItem,
  type ReviewTarget,
  type DecisionOutcome,
} from './reviewModel';
import { decodeReviewUri, encodeReviewUri } from './uri';
import { collectBefore, runAfter, type ReviewHookRegistry } from './hooks';
import type { ReviewPreviewPanel } from './previewPanel';
import type { GatewayClient, ReviewDecision, ReviewDraftRef } from '../api/types';

/** What the host wires in from companion extensions: decision hooks and the preview beside a diff. */
export interface ReviewExtensions {
  hooks: ReviewHookRegistry;
  gateway: GatewayClient;
  preview?: ReviewPreviewPanel;
}

/** The proposal's own provenance as strings: what a companion may want to record with a decision. */
function provenanceOf(d: Draft): Record<string, string> {
  const out: Record<string, string> = {};
  const put = (k: string, v: unknown) => {
    if (typeof v === 'string' && v) out[k] = v;
  };
  put('author', d.author);
  put('run_id', d.run_id);
  put('root_event_id', d.root_event_id);
  put('event_id', d.event_id);
  put('changeset_id', d.changeset_id);
  put('created_at', d.created_at);
  put('base_version', d.base_version);
  put('base_sha256', d.base_sha256);
  put('content_sha256', d.content_sha256);
  return out;
}

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
    private readonly extensions?: ReviewExtensions,
  ) {
    this.disposables.push(
      vscode.commands.registerCommand('escurel.openReview', (arg?: unknown) =>
        this.openReview(arg),
      ),
      vscode.commands.registerCommand('escurel.promote', (arg?: unknown) => this.promote(arg)),
      vscode.commands.registerCommand('escurel.discard', (arg?: unknown) => this.discard(arg)),
      // The header of a review diff: the way to what the change is about.
      vscode.commands.registerCommand('escurel.reviewOpenInstance', () =>
        this.openFromReview('page'),
      ),
      vscode.commands.registerCommand('escurel.reviewOpenThread', () =>
        this.openFromReview('thread'),
      ),
      vscode.commands.registerCommand('escurel.reviewOpenRun', () => this.openFromReview('run')),
      vscode.commands.registerCommand('escurel.reviewViewSkill', () =>
        this.openFromReview('skill'),
      ),
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
    extensions?: ReviewExtensions,
  ): ReviewController {
    const contentProvider = ReviewContentProvider.register(context, client);
    const commentsController = ReviewCommentsController.register(context, client);
    const controller = new ReviewController(
      client,
      contentProvider,
      commentsController,
      refreshAwaiting,
      extensions,
    );
    context.subscriptions.push(controller);
    return controller;
  }

  /**
   * From the header of the review diff that is in front: open the instance, thread or run the change
   * belongs to, or its skill. The draft is found from the diff's own URI; when the change has no run or
   * thread behind it (a person's draft) the answer is a worded notice, not silence.
   */
  private async openFromReview(what: 'page' | 'thread' | 'run' | 'skill'): Promise<void> {
    const uri = vscode.window.activeTextEditor?.document.uri ?? this.activeReviewUri();
    const decoded = uri ? decodeReviewUri(uri) : undefined;
    const draft = decoded ? this.contentProvider.getDraft(decoded.draftId) : undefined;
    if (!draft) {
      void vscode.window.showInformationMessage('Open a change from Awaiting you first.');
      return;
    }
    const refs = nodeRefs({ kind: 'draft', draft });
    const target: NoticeTarget =
      what === 'page'
        ? { kind: 'page', pageId: refs.pageId }
        : what === 'thread'
          ? { kind: 'thread', rootEventId: refs.rootEventId }
          : what === 'run'
            ? { kind: 'run', runId: refs.runId }
            : { kind: 'skill', skill: refs.skill };
    const [action] = noticeActions([target]);
    if (!action) {
      void vscode.window.showInformationMessage(
        what === 'page' || what === 'skill'
          ? 'This change does not say which page it is for.'
          : 'No agent run is behind this change: a person made it.',
      );
      return;
    }
    await vscode.commands.executeCommand(action.command, ...action.args);
  }

  /** The diff tab that is in front, when it is a review diff (the text editor is not the diff itself). */
  private activeReviewUri(): vscode.Uri | undefined {
    const input = vscode.window.tabGroups.activeTabGroup.activeTab?.input;
    return input instanceof vscode.TabInputTextDiff ? input.modified : undefined;
  }

  /**
   * Opens the review surface for a draft, changeset, or confirm gate row.
   */
  async openReview(arg?: unknown): Promise<void> {
    if (arg && typeof arg === 'object' && 'kind' in arg) {
      if (arg.kind === 'confirm_gate') {
        void vscode.window.showInformationMessage(
          'Nothing to review yet: the agent is waiting for your confirmation and has not proposed a change.',
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

    void vscode.window.showInformationMessage('Pick an item in Awaiting you to review it.');
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
          'There is nothing left to review in that set of changes.',
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
        placeHolder: changesetPickTitle(csDrafts),
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
        `Could not open that set of changes: ${describeError(err)}`,
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
      void vscode.window.showErrorMessage('That change was already handled or removed.');
      return;
    }

    this.contentProvider.setDraft(draft);

    try {
      const diff = await this.client().diffDraft({ draft_id: draft.draft_id });
      const check = checkBaseMoved(diff);
      if (check.baseMoved && check.warning) {
        void vscode.window.showWarningMessage(`${check.warning}`);
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
      // What companions add beside the diff is an addition: it must never cost the reviewer the diff.
      await this.extensions?.preview?.show(draft).catch((err: unknown) => {
        log().warn(`review: preview unavailable: ${describeError(err)}`);
      });
    } finally {
      this.openingDrafts.delete(draft.draft_id);
    }
  }

  /** The open drafts a decision applies to, read before the decision so hooks can name them. */
  private async draftsOf(target: ReviewTarget): Promise<Draft[]> {
    try {
      if (target.kind === 'draft') {
        const cached = this.contentProvider.getDraft(target.draftId);
        if (cached) return [cached];
        return (await this.client().listDrafts()).filter((d) => d.draft_id === target.draftId);
      }
      return (await this.client().listDrafts()).filter(
        (d) => d.changeset_id === target.changesetId && d.status === 'open',
      );
    } catch (err) {
      log().warn(`review: could not read the drafts for the hooks: ${describeError(err)}`);
      return [];
    }
  }

  /**
   * Before a decision: companions may veto and may ask for a note. Returns the note (empty when none was
   * asked), or undefined when the decision must not go on (a veto, or the reviewer cancelled the prompt).
   * Without a hook that asks, `fallback` is the host's own optional reason prompt (a reject has one).
   */
  private async beforeDecision(
    decision: ReviewDecision,
    drafts: Draft[],
    fallback?: { prompt: string; placeHolder: string },
  ): Promise<{ note: string } | undefined> {
    const hooks = this.extensions?.hooks;
    const asked =
      hooks && hooks.size
        ? await collectBefore(hooks, {
            decision,
            drafts: drafts.map(
              (d): ReviewDraftRef => ({ draftId: d.draft_id, targetPageId: d.target_page_id }),
            ),
            client: this.extensions!.gateway,
          })
        : { note: 'none' as const };
    if ('veto' in asked && asked.veto) {
      void vscode.window.showWarningMessage(asked.veto);
      return undefined;
    }
    if (asked.note === 'none' && !fallback) return { note: '' };
    const required = asked.note === 'required';
    const prompt = 'prompt' in asked ? asked.prompt : undefined;
    const input = await vscode.window.showInputBox({
      prompt:
        prompt ??
        (asked.note === 'none'
          ? fallback!.prompt
          : `Decision note (${required ? 'required' : 'optional'})`),
      ...(asked.note === 'none' ? { placeHolder: fallback!.placeHolder } : {}),
      ...(required
        ? { validateInput: (v: string) => (v.trim() ? undefined : 'A note is required.') }
        : {}),
    });
    // The reviewer pressed Escape.
    if (input === undefined) return undefined;
    return { note: input.trim() };
  }

  /** After a decision was recorded: tell the hooks, once per decided draft. Never throws. */
  private async afterDecision(
    decision: ReviewDecision,
    decided: { draftId: string; targetPageId: string; draft?: Draft }[],
    note: string,
  ): Promise<void> {
    const hooks = this.extensions?.hooks;
    if (!hooks || !hooks.size) return;
    for (const d of decided) {
      await runAfter(hooks, {
        draftId: d.draftId,
        targetPageId: d.targetPageId,
        decision,
        note,
        provenance: d.draft ? provenanceOf(d.draft) : {},
        client: this.extensions!.gateway,
      });
    }
  }

  /**
   * Promotes a draft or changeset. A companion's own "accept with edits" command saves its edit and
   * passes `withEdits: true`, so the hooks hear `accepted_with_edits`.
   */
  async promote(arg?: unknown): Promise<void> {
    const activeUri = vscode.window.activeTextEditor?.document.uri.toString();
    const target = resolveReviewTarget(arg, activeUri);

    if (!target) {
      void vscode.window.showErrorMessage('Open a change from Awaiting you first, then apply it.');
      return;
    }

    const decision: ReviewDecision =
      (arg as { withEdits?: unknown } | undefined)?.withEdits === true
        ? 'accepted_with_edits'
        : 'accepted';
    const before = this.extensions?.hooks.size ? await this.draftsOf(target) : [];
    const gate = await this.beforeDecision(decision, before);
    if (!gate) return;

    let outcome: DecisionOutcome;

    if (target.kind === 'draft') {
      try {
        await this.client().promoteDraft({ draft_id: target.draftId });
        outcome = interpretPromoteDraftSuccess(target.draftId);
        await this.afterDecision(
          decision,
          before.map((d) => ({ draftId: d.draft_id, targetPageId: d.target_page_id, draft: d })),
          gate.note,
        );
        this.tell(outcome, 'info', arg);
      } catch (err) {
        outcome = interpretPromoteDraftError(err, target.draftId);
        if (outcome.kind === 'already_decided') {
          this.tell(outcome, 'info', arg);
        } else {
          this.tell(outcome, 'error', arg);
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
      await this.afterDecision(
        decision,
        res.results
          .filter((row) => row.ok && !row.already_applied)
          .map((row) => ({
            draftId: row.draft_id,
            targetPageId: row.page_id,
            draft: before.find((d) => d.draft_id === row.draft_id),
          })),
        gate.note,
      );
      if (outcome.kind === 'partial') {
        this.tell(outcome, 'warning', arg);
      } else {
        this.tell(outcome, 'info', arg);
      }
    } catch (err) {
      if (err instanceof EscurelError && err.kind === 'already_decided') {
        outcome = {
          kind: 'already_decided',
          message: `Changeset ${target.changesetId} was already decided.`,
          closeDiff: true,
          refresh: true,
        };
        this.tell(outcome, 'info', arg);
      } else {
        outcome = {
          kind: 'error',
          message: describeError(err),
          closeDiff: false,
          refresh: false,
        };
        this.tell(outcome, 'error', arg);
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
      void vscode.window.showErrorMessage('Open a change from Awaiting you first, then reject it.');
      return;
    }

    const before = this.extensions?.hooks.size ? await this.draftsOf(target) : [];
    const gate = await this.beforeDecision('rejected', before, {
      prompt: `Reason for discarding ${target.kind === 'draft' ? 'draft' : 'changeset'} (optional)`,
      placeHolder: 'Optional discard reason',
    });
    // Escape, or a companion's veto
    if (!gate) return;
    const reason = gate.note ? gate.note : undefined;

    let outcome: DecisionOutcome;

    if (target.kind === 'draft') {
      try {
        await this.client().discardDraft({ draft_id: target.draftId, reason });
        outcome = interpretDiscardResult('draft', target.draftId);
        await this.afterDecision(
          'rejected',
          before.map((d) => ({ draftId: d.draft_id, targetPageId: d.target_page_id, draft: d })),
          gate.note,
        );
        this.tell(outcome, 'info', arg);
      } catch (err) {
        outcome = interpretDiscardError(err, 'draft', target.draftId);
        if (outcome.kind === 'already_decided') {
          this.tell(outcome, 'info', arg);
        } else {
          this.tell(outcome, 'error', arg);
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
      await this.afterDecision(
        'rejected',
        before.map((d) => ({ draftId: d.draft_id, targetPageId: d.target_page_id, draft: d })),
        gate.note,
      );
      this.tell(outcome, 'info', arg);
    } catch (err) {
      outcome = interpretDiscardError(err, 'changeset', target.changesetId);
      if (outcome.kind === 'already_decided') {
        this.tell(outcome, 'info', arg);
      } else {
        this.tell(outcome, 'error', arg);
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
  /**
   * A decision notice. It offers to open what it names: the pages that were applied (or the page the
   * row was about), else the thread the row belongs to (at most two buttons, see `noticeActions`).
   */
  private tell(outcome: DecisionOutcome, level: 'info' | 'warning' | 'error', arg?: unknown): void {
    const refs = nodeRefs(arg);
    // Two buttons at most: one page AND the thread, so "Applied 2 changes" leads to where they came from
    // and to what they changed. A second applied page is one click away from the thread.
    const [firstPage, ...otherPages] = outcome.pages ?? [];
    const targets: NoticeTarget[] = [
      { kind: 'page', pageId: firstPage ?? refs.pageId },
      { kind: 'thread', rootEventId: refs.rootEventId },
      { kind: 'run', runId: refs.runId },
      ...otherPages.map((pageId) => ({ kind: 'page' as const, pageId })),
    ];
    void notify(level, outcome.message, targets);
  }

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
