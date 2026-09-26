import type {
  Changeset,
  DiffDraftResponse,
  Draft,
  Event,
  PromoteChangesetResponse,
} from '../client';
import { EscurelError } from '../client/errors';
import { describeError } from '../errors';
import { pageSlug } from '../shared/pageId';
import { pluralise } from '../shared/text';

export const REVIEW_SCHEME = 'escurel-review';

export type ReviewSide = 'base' | 'proposed';

/**
 * Builds an `escurel-review` URI for a draft side.
 * A draft diff requires two documents: the target page's current base markdown,
 * and the draft's proposed markdown. The custom scheme lets TextDocumentContentProvider
 * serve them in-memory without creating temporary files on disk.
 */
export function reviewPath(draftId: string, side: ReviewSide): string {
  return `/${draftId}/${side}.md`;
}

/**
 * The draft and side a review URI names, from its parts. Pure on purpose:
 * `vscode.Uri` parsing is the editor's, so a model that took one could only
 * be tested against a hand-written stand-in — and a stand-in that parses
 * differently makes the test lie. `src/review/uri.ts` does the conversion.
 *
 * Both shapes VS Code can produce are read: the path form
 * `/<draftId>/<side>.md` and the authority form `//<draftId>/<side>.md`.
 */
export function parseReviewParts(
  scheme: string,
  path: string,
  authority = '',
): { draftId: string; side: ReviewSide } | undefined {
  if (scheme !== REVIEW_SCHEME) return undefined;
  const parts = path.split('/').filter(Boolean);
  // Exactly `<draftId>/<side>` — a deeper path is some other URI that merely
  // shares our scheme, and guessing at it would open the wrong draft.
  const fromAuthority = Boolean(authority) && parts.length === 1;
  if (!fromAuthority && parts.length !== 2) return undefined;
  const [draftId, sidePart] = fromAuthority ? [authority, parts[0]] : [parts[0], parts[1]];
  if (!draftId || !sidePart) return undefined;
  const side = sidePart.replace(/\.md$/, '');
  if (side !== 'base' && side !== 'proposed') return undefined;
  return { draftId, side };
}

/**
 * Formats the diff editor tab title as `<slug> — draft by <author>`.
 */
export function formatDraftDiffTitle(draft: Pick<Draft, 'target_page_id' | 'author'>): string {
  const slug = pageSlug(draft.target_page_id);
  return `${slug} — draft by ${draft.author}`;
}

export interface BaseMovedCheck {
  baseMoved: boolean;
  warning?: string;
  hint?: string;
}

/**
 * Checks whether the target page moved under the draft since its base was captured.
 * If true, promoting will be refused by the server with a conflict.
 */
export function checkBaseMoved(diff: Pick<DiffDraftResponse, 'base_moved'>): BaseMovedCheck {
  if (!diff.base_moved) {
    return { baseMoved: false };
  }
  return {
    baseMoved: true,
    warning: 'Target page moved under draft; promoting will conflict. Re-draft to resolve.',
    hint: 'Re-draft',
  };
}

/**
 * Summarizes the diff for display in QuickPick or notifications.
 */
export function formatDiffSummary(diff?: DiffDraftResponse): string {
  if (!diff) return 'no diff available';

  const parts: string[] = [];
  if (!diff.exists) parts.push('new page');
  if (diff.base_moved) parts.push('base moved');

  const fmCount = diff.frontmatter_changes?.length ?? 0;
  if (fmCount > 0) {
    parts.push(pluralise(fmCount, 'field'));
  }

  const blockCount = diff.block_changes?.length ?? 0;
  if (blockCount > 0) {
    parts.push(pluralise(blockCount, 'block'));
  }

  return parts.length > 0 ? parts.join(' · ') : 'no changes';
}

export interface ChangesetQuickPickItem {
  // Not `kind`: `vscode.QuickPickItem` reserves that name for its own
  // separator/default enum, and a string there fails the overload.
  action: 'promote_all' | 'discard_all' | 'draft';
  label: string;
  description: string;
  /** Set only on a `draft` item: the draft that row opens. */
  draft?: Draft;
}

/**
 * Builds the QuickPick menu items for reviewing a changeset, placing batch operations
 * first so reviewers can land or reject the entire set without clicking through each draft.
 */
export function buildChangesetQuickPickItems(
  changesetId: string,
  drafts: Draft[],
  diffs: Map<string, DiffDraftResponse>,
): ChangesetQuickPickItem[] {
  const items: ChangesetQuickPickItem[] = [
    {
      action: 'promote_all',
      label: '$(check) Promote all',
      description: `Land all ${pluralise(drafts.length, 'draft')} in changeset ${changesetId}`,
    },
    {
      action: 'discard_all',
      label: '$(trash) Discard all',
      description: `Refuse all ${pluralise(drafts.length, 'draft')} in changeset ${changesetId}`,
    },
  ];

  for (const draft of drafts) {
    const diff = diffs.get(draft.draft_id);
    items.push({
      action: 'draft',
      label: pageSlug(draft.target_page_id),
      description: formatDiffSummary(diff),
      draft,
    });
  }

  return items;
}

/** `escurel-review:/<draftId>/<side>.md` as a plain string. */
export function parseReviewString(
  value: string,
): { draftId: string; side: ReviewSide } | undefined {
  const colon = value.indexOf(':');
  if (colon < 0) return undefined;
  const scheme = value.slice(0, colon);
  let rest = value.slice(colon + 1);
  let authority = '';
  if (rest.startsWith('//')) {
    rest = rest.slice(2);
    const slash = rest.indexOf('/');
    authority = slash < 0 ? rest : rest.slice(0, slash);
    rest = slash < 0 ? '' : rest.slice(slash);
  }
  return parseReviewParts(scheme, rest, authority);
}

export type ReviewTarget =
  { kind: 'changeset'; changesetId: string } | { kind: 'draft'; draftId: string };

/**
 * Resolves whether a command invocation targets a changeset or a draft,
 * accepting tree rows, bare payload objects, or the active diff editor's URI.
 */
export function resolveReviewTarget(arg: unknown, activeUri?: string): ReviewTarget | undefined {
  if (arg && typeof arg === 'object') {
    if ('kind' in arg) {
      if (arg.kind === 'changeset' && 'changeset' in arg) {
        const cs = (arg as { changeset: Changeset }).changeset;
        return { kind: 'changeset', changesetId: cs.changeset_id };
      }
      if (arg.kind === 'draft' && 'draft' in arg) {
        const d = (arg as { draft: Draft }).draft;
        return { kind: 'draft', draftId: d.draft_id };
      }
    }

    // A draft payload carries both its own `draft_id` and the parent `changeset_id`
    // it belongs to. Checking draft identity first ensures an object naming a draft
    // opens that draft's diff; matching changeset first would misroute any draft in
    // a changeset into the changeset QuickPick.
    if ('draftId' in arg && typeof (arg as { draftId: unknown }).draftId === 'string') {
      return { kind: 'draft', draftId: (arg as { draftId: string }).draftId };
    }
    if ('draft_id' in arg && typeof (arg as { draft_id: unknown }).draft_id === 'string') {
      return { kind: 'draft', draftId: (arg as { draft_id: string }).draft_id };
    }

    if ('changesetId' in arg && typeof (arg as { changesetId: unknown }).changesetId === 'string') {
      return { kind: 'changeset', changesetId: (arg as { changesetId: string }).changesetId };
    }
    if (
      'changeset_id' in arg &&
      typeof (arg as { changeset_id: unknown }).changeset_id === 'string'
    ) {
      return { kind: 'changeset', changesetId: (arg as { changeset_id: string }).changeset_id };
    }

    // A `vscode.Uri` arrives from an editor-title action: read its parts,
    // never its parsing.
    if ('scheme' in arg && 'path' in arg) {
      const u = arg as { scheme: string; path: string; authority?: string };
      const decoded = parseReviewParts(u.scheme, u.path, u.authority ?? '');
      if (decoded) return { kind: 'draft', draftId: decoded.draftId };
    }
  } else if (typeof arg === 'string') {
    const decoded = parseReviewString(arg);
    if (decoded) return { kind: 'draft', draftId: decoded.draftId };
  }

  if (activeUri) {
    const decoded = parseReviewString(activeUri);
    if (decoded) return { kind: 'draft', draftId: decoded.draftId };
  }

  return undefined;
}

export interface ReviewComment {
  eventId: string;
  author: string;
  body: string;
  /** `null` for an undated capture, which sorts last rather than first. */
  at: string | null;
  line?: number;
}

export interface ReviewCommentThread {
  /** 1-indexed line from provenance. undefined denotes a file-level comment at the top of the file. */
  line?: number;
  comments: ReviewComment[];
}

/**
 * Extracts and normalizes review comments for a specific draft from page events.
 * The gateway stores comments under label `escurel:review-comment` with provenance metadata.
 */
export function extractReviewComments(events: Event[], draftId: string): ReviewComment[] {
  const comments: ReviewComment[] = [];

  for (const event of events) {
    if (event.label_skill !== 'escurel:review-comment') continue;

    const prov = event.provenance as {
      review?: { draft_id?: string; line?: number; commented_by?: string };
      captured_by?: string;
    } | null;
    const review = prov?.review;
    if (review?.draft_id !== draftId) continue;

    // Authorship precedence: explicit reviewer -> session user who captured -> source name.
    const author = review?.commented_by ?? prov?.captured_by ?? event.source ?? 'anonymous';
    const line = typeof review?.line === 'number' ? review.line : undefined;

    comments.push({
      eventId: event.event_id,
      author,
      body: event.body ?? '',
      at: event.at,
      line,
    });
  }

  // A thread reads as a conversation, so oldest first. An undated capture
  // has no place in that order and goes last rather than pretending to be
  // the first thing anyone said.
  const when = (c: ReviewComment) => (c.at ? new Date(c.at).getTime() : Number.MAX_SAFE_INTEGER);
  comments.sort((a, b) => when(a) - when(b));
  return comments;
}

/**
 * Groups comments into threads by line.
 * Unanchored comments form one thread at the top of the file, followed by line-anchored
 * threads in ascending line order.
 */
export function groupCommentsIntoThreads(comments: ReviewComment[]): ReviewCommentThread[] {
  const topComments: ReviewComment[] = [];
  const lineMap = new Map<number, ReviewComment[]>();

  for (const comment of comments) {
    if (comment.line === undefined) {
      topComments.push(comment);
    } else {
      const existing = lineMap.get(comment.line) ?? [];
      existing.push(comment);
      lineMap.set(comment.line, existing);
    }
  }

  const threads: ReviewCommentThread[] = [];
  if (topComments.length > 0) {
    threads.push({ line: undefined, comments: topComments });
  }

  const sortedLines = Array.from(lineMap.keys()).sort((a, b) => a - b);
  for (const line of sortedLines) {
    threads.push({ line, comments: lineMap.get(line)! });
  }

  return threads;
}

export function buildReviewCommentThreads(events: Event[], draftId: string): ReviewCommentThread[] {
  return groupCommentsIntoThreads(extractReviewComments(events, draftId));
}

export type DecisionOutcomeKind = 'success' | 'already_decided' | 'conflict' | 'partial' | 'error';

export interface DecisionOutcome {
  kind: DecisionOutcomeKind;
  message: string;
  closeDiff: boolean;
  refresh: boolean;
}

/**
 * Translates errors from draft promotion into user-facing outcomes.
 * `already_decided` is handled gracefully without treating it as an operational failure.
 */
export function interpretPromoteDraftError(err: unknown, draftId: string): DecisionOutcome {
  if (err instanceof EscurelError) {
    if (err.kind === 'already_decided') {
      const status = err.draft?.status ? ` (${err.draft.status})` : '';
      return {
        kind: 'already_decided',
        message: `Draft ${draftId} was already decided${status}.`,
        closeDiff: true,
        refresh: true,
      };
    }
    if (err.kind === 'conflict') {
      return {
        kind: 'conflict',
        message: `Conflict: target page moved under draft ${draftId}. Re-draft required.`,
        closeDiff: false,
        refresh: false,
      };
    }
  }

  return {
    kind: 'error',
    message: describeError(err),
    closeDiff: false,
    refresh: false,
  };
}

export function interpretPromoteDraftSuccess(draftId: string): DecisionOutcome {
  return {
    kind: 'success',
    message: `Promoted draft ${draftId}.`,
    closeDiff: true,
    refresh: true,
  };
}

/**
 * Analyzes the response of a changeset promotion, breaking down results per draft.
 */
export function interpretPromoteChangesetResult(res: PromoteChangesetResponse): DecisionOutcome {
  if (res.already_decided) {
    return {
      kind: 'already_decided',
      message: `Changeset ${res.changeset_id} was already decided.`,
      closeDiff: true,
      refresh: true,
    };
  }

  const summaries = res.results.map((r) => {
    const slug = pageSlug(r.page_id);
    const status = r.already_applied
      ? 'already applied'
      : r.ok
        ? 'applied'
        : (r.status ?? 'failed');
    return `${slug}: ${status}`;
  });

  const hasFailures = res.partial || res.results.some((r) => r.ok === false);
  if (hasFailures) {
    const okCount = res.results.filter((r) => r.ok).length;
    const failCount = res.results.length - okCount;
    return {
      kind: 'partial',
      message: `Changeset ${res.changeset_id} partially promoted (${okCount} succeeded, ${failCount} failed): ${summaries.join(', ')}`,
      closeDiff: false,
      refresh: true,
    };
  }

  return {
    kind: 'success',
    message: `Promoted changeset ${res.changeset_id}: ${summaries.join(', ')}`,
    closeDiff: true,
    refresh: true,
  };
}

export function interpretDiscardResult(kind: 'draft' | 'changeset', id: string): DecisionOutcome {
  const noun = kind === 'draft' ? 'draft' : 'changeset';
  return {
    kind: 'success',
    message: `Discarded ${noun} ${id}.`,
    closeDiff: true,
    refresh: true,
  };
}

export function interpretDiscardError(
  err: unknown,
  action: 'draft' | 'changeset',
  id: string,
): DecisionOutcome {
  if (err instanceof EscurelError && err.kind === 'already_decided') {
    const noun = action === 'draft' ? 'Draft' : 'Changeset';
    return {
      kind: 'already_decided',
      message: `${noun} ${id} was already decided.`,
      closeDiff: true,
      refresh: true,
    };
  }

  return {
    kind: 'error',
    message: describeError(err),
    closeDiff: false,
    refresh: false,
  };
}
