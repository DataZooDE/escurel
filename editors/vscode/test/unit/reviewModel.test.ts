import { describe, expect, it } from 'vitest';
import { fixture } from './mockGateway';
import type {
  Changeset,
  DiffDraftResponse,
  Draft,
  Event,
  PromoteChangesetResponse,
} from '../../src/client';
import { EscurelError } from '../../src/client/errors';
import type { AwaitingRow } from '../../src/views/awaitingModel';
import {
  REVIEW_SCHEME,
  buildChangesetQuickPickItems,
  buildReviewCommentThreads,
  checkBaseMoved,
  parseReviewParts,
  parseReviewString,
  reviewPath,
  extractReviewComments,
  formatDiffSummary,
  formatDraftDiffTitle,
  groupCommentsIntoThreads,
  interpretDiscardError,
  interpretDiscardResult,
  interpretPromoteChangesetResult,
  interpretPromoteDraftError,
  interpretPromoteDraftSuccess,
  resolveReviewTarget,
} from '../../src/review/reviewModel';

describe('reviewModel', () => {
  describe('Review URI encoding and decoding (round-trip)', () => {
    it('round-trips base and proposed review URIs', () => {
      const draftId = '01M3CAHP17503GSZN7QRRA03GA';

      expect(reviewPath(draftId, 'base')).toBe(`/${draftId}/base.md`);
      expect(reviewPath(draftId, 'proposed')).toBe(`/${draftId}/proposed.md`);

      // Both halves of the round trip, as a string and as the parts VS Code
      // hands us from a Uri.
      for (const side of ['base', 'proposed'] as const) {
        const asString = `${REVIEW_SCHEME}:${reviewPath(draftId, side)}`;
        expect(parseReviewString(asString)).toEqual({ draftId, side });
        expect(parseReviewParts(REVIEW_SCHEME, reviewPath(draftId, side))).toEqual({
          draftId,
          side,
        });
      }
    });

    it('decodes string URIs with or without .md extension', () => {
      const draftId = 'draft-42';
      expect(parseReviewString(`escurel-review:/${draftId}/base.md`)).toEqual({
        draftId,
        side: 'base',
      });
      expect(parseReviewString(`escurel-review:/${draftId}/proposed`)).toEqual({
        draftId,
        side: 'proposed',
      });
      expect(parseReviewString(`escurel-review://${draftId}/proposed.md`)).toEqual({
        draftId,
        side: 'proposed',
      });
    });

    it('rejects foreign schemes, malformed paths, or invalid sides', () => {
      expect(parseReviewString('escurel:/markdown/instances/test.md')).toBeUndefined();
      expect(parseReviewString('file:///tmp/test.md')).toBeUndefined();
      expect(parseReviewString('escurel-review:/invalid')).toBeUndefined();
      expect(parseReviewString('escurel-review:/d1/other.md')).toBeUndefined();
      expect(parseReviewString('escurel-review:/d1/base/extra/parts')).toBeUndefined();
    });
  });

  describe('formatDraftDiffTitle', () => {
    it('formats title as <slug> — draft by <author>', () => {
      const draft: Pick<Draft, 'target_page_id' | 'author'> = {
        target_page_id: 'markdown/instances/customer__alpina-biotech.md',
        author: 'anonymous',
      };
      expect(formatDraftDiffTitle(draft)).toBe('alpina-biotech — draft by anonymous');

      const draftSpine: Pick<Draft, 'target_page_id' | 'author'> = {
        target_page_id: 'markdown/instances/engagement__ha-spine.md',
        author: 'agt:echo',
      };
      expect(formatDraftDiffTitle(draftSpine)).toBe('ha-spine — draft by agt:echo');
    });
  });

  describe('checkBaseMoved', () => {
    it('returns no conflict when base_moved is false', () => {
      const res = checkBaseMoved({ base_moved: false });
      expect(res.baseMoved).toBe(false);
      expect(res.warning).toBeUndefined();
    });

    it('returns conflict warning and re-draft hint when base_moved is true', () => {
      const res = checkBaseMoved({ base_moved: true });
      expect(res.baseMoved).toBe(true);
      expect(res.warning).toContain('conflict');
      expect(res.warning).toContain('Re-draft');
      expect(res.hint).toContain('Re-draft');
    });
  });

  describe('formatDiffSummary', () => {
    it('summarizes block changes and frontmatter changes', () => {
      const diff1: DiffDraftResponse = {
        ok: true,
        draft_id: 'd1',
        target_page_id: 'p1',
        exists: true,
        base_moved: false,
        run_id: null,
        root_event_id: null,
        frontmatter_changes: [],
        block_changes: [{ anchor: 'b1', kind: 'replace', preview: 'hello' }],
      };
      expect(formatDiffSummary(diff1)).toBe('1 block');

      const diff2: DiffDraftResponse = {
        ...diff1,
        frontmatter_changes: [
          { key: 'status', from: 'qualifying', to: 'active' },
          { key: 'risk', from: 0.2, to: 0.5 },
        ],
        block_changes: [
          { anchor: 'b1', kind: 'replace', preview: 'a' },
          { anchor: 'b2', kind: 'add', preview: 'b' },
          { anchor: 'b3', kind: 'delete', preview: 'c' },
        ],
      };
      expect(formatDiffSummary(diff2)).toBe('2 fields · 3 blocks');
    });

    it('includes new page and base moved annotations', () => {
      const newPageDiff: DiffDraftResponse = {
        ok: true,
        draft_id: 'd2',
        target_page_id: 'p2',
        exists: false,
        base_moved: false,
        run_id: null,
        root_event_id: null,
        frontmatter_changes: [{ key: 'id', from: null, to: 'new' }],
        block_changes: [],
      };
      expect(formatDiffSummary(newPageDiff)).toBe('new page · 1 field');

      const movedDiff: DiffDraftResponse = {
        ok: true,
        draft_id: 'd3',
        target_page_id: 'p3',
        exists: true,
        base_moved: true,
        run_id: null,
        root_event_id: null,
        frontmatter_changes: [],
        block_changes: [{ anchor: 'b1', kind: 'replace', preview: 'x' }],
      };
      expect(formatDiffSummary(movedDiff)).toBe('base moved · 1 block');
    });

    it('handles empty changes and missing diff gracefully', () => {
      const emptyDiff: DiffDraftResponse = {
        ok: true,
        draft_id: 'd4',
        target_page_id: 'p4',
        exists: true,
        base_moved: false,
        run_id: null,
        root_event_id: null,
        frontmatter_changes: [],
        block_changes: [],
      };
      expect(formatDiffSummary(emptyDiff)).toBe('no changes');
      expect(formatDiffSummary(undefined)).toBe('no diff available');
    });
  });

  describe('buildChangesetQuickPickItems', () => {
    it('prepends Promote all and Discard all, followed by drafts with diff summaries', () => {
      const draftA: Draft = {
        draft_id: 'd-a',
        target_page_id: 'markdown/instances/customer__alpina-biotech.md',
        content: '',
        content_sha256: '',
        base_sha256: null,
        author: 'anonymous',
        event_id: null,
        changeset_id: 'cs-1',
        status: 'open',
        reason: null,
        decided_by: null,
        created_at: '2026-09-25T10:00:00Z',
        base_version: null,
        run_id: null,
        root_event_id: null,
      };
      const draftB: Draft = {
        ...draftA,
        draft_id: 'd-b',
        target_page_id: 'markdown/instances/customer__acme.md',
      };

      const diffs = new Map<string, DiffDraftResponse>([
        [
          'd-a',
          {
            ok: true,
            draft_id: 'd-a',
            target_page_id: draftA.target_page_id,
            exists: true,
            base_moved: false,
            run_id: null,
            root_event_id: null,
            frontmatter_changes: [],
            block_changes: [{ anchor: 'b1', kind: 'replace', preview: 'x' }],
          },
        ],
      ]);

      const items = buildChangesetQuickPickItems('cs-1', [draftA, draftB], diffs);
      expect(items.length).toBe(4);

      expect(items[0]).toEqual({
        action: 'promote_all',
        label: '$(check) Promote all',
        description: 'Land all 2 drafts in changeset cs-1',
      });
      expect(items[1]).toEqual({
        action: 'discard_all',
        label: '$(trash) Discard all',
        description: 'Refuse all 2 drafts in changeset cs-1',
      });
      expect(items[2]).toEqual({
        action: 'draft',
        label: 'alpina-biotech',
        description: '1 block',
        draft: draftA,
      });
      expect(items[3]).toEqual({
        action: 'draft',
        label: 'acme',
        description: 'no diff available',
        draft: draftB,
      });
    });

    it('uses singular "1 draft" in promote and discard descriptions for single-draft changeset', () => {
      const draftA: Draft = {
        draft_id: 'd-a',
        target_page_id: 'markdown/instances/customer__alpina-biotech.md',
        content: '',
        content_sha256: '',
        base_sha256: null,
        author: 'anonymous',
        event_id: null,
        changeset_id: '01M3CAHP14HT8AGG0CH60H73HM',
        status: 'open',
        reason: null,
        decided_by: null,
        created_at: '2026-09-25T10:00:00Z',
        base_version: null,
        run_id: null,
        root_event_id: null,
      };
      const diffs = new Map<string, DiffDraftResponse>();
      const items = buildChangesetQuickPickItems('01M3CAHP14HT8AGG0CH60H73HM', [draftA], diffs);

      expect(items[0]!.description).toBe(
        'Land all 1 draft in changeset 01M3CAHP14HT8AGG0CH60H73HM',
      );
      expect(items[1]!.description).toBe(
        'Refuse all 1 draft in changeset 01M3CAHP14HT8AGG0CH60H73HM',
      );
    });
  });

  describe('resolveReviewTarget', () => {
    it('resolves ChangesetRow, DraftRow, and bare objects', () => {
      const csRow: AwaitingRow = {
        kind: 'changeset',
        id: 'cs-1',
        label: 'cs-1',
        description: '2 drafts',
        timestamp: '2026-09-25T10:00:00Z',
        changeset: { changeset_id: 'cs-1' } as Changeset,
      };
      expect(resolveReviewTarget(csRow)).toEqual({ kind: 'changeset', changesetId: 'cs-1' });

      const draftRow: AwaitingRow = {
        kind: 'draft',
        id: 'd-1',
        label: 'alpina',
        description: 'human',
        timestamp: '2026-09-25T10:00:00Z',
        draft: { draft_id: 'd-1' } as Draft,
      };
      expect(resolveReviewTarget(draftRow)).toEqual({ kind: 'draft', draftId: 'd-1' });

      expect(resolveReviewTarget({ changesetId: 'cs-99' })).toEqual({
        kind: 'changeset',
        changesetId: 'cs-99',
      });
      expect(resolveReviewTarget({ changeset_id: 'cs-99' })).toEqual({
        kind: 'changeset',
        changesetId: 'cs-99',
      });
      expect(resolveReviewTarget({ draftId: 'd-99' })).toEqual({
        kind: 'draft',
        draftId: 'd-99',
      });
      expect(resolveReviewTarget({ draft_id: 'd-99' })).toEqual({
        kind: 'draft',
        draftId: 'd-99',
      });
    });

    it('resolves review URI or active editor review URI', () => {
      // What an editor-title action hands us: a Uri's parts, not a string.
      expect(
        resolveReviewTarget({ scheme: REVIEW_SCHEME, path: reviewPath('d-test', 'proposed') }),
      ).toEqual({ kind: 'draft', draftId: 'd-test' });
      expect(resolveReviewTarget('escurel-review:/d-str/base.md')).toEqual({
        kind: 'draft',
        draftId: 'd-str',
      });
      expect(resolveReviewTarget(undefined, 'escurel-review:/d-active/proposed.md')).toEqual({
        kind: 'draft',
        draftId: 'd-active',
      });
      expect(resolveReviewTarget(undefined, undefined)).toBeUndefined();
    });

    it('resolves a real-shaped Draft with non-null changeset_id to draft, not changeset', () => {
      const realDraft: Draft = {
        draft_id: '01M3CAHP17503GSZN7QRRA03GA',
        target_page_id: 'markdown/instances/engagement__ha-spine.md',
        content: '# Title\n',
        content_sha256: '5d4aaebe44d75d6f68c6c5468e41304ba7d5e915eb9702792cf3922ae93fea7f',
        base_sha256: '3a9c8023486580715b08422327ea5dddafc7f5a513fda2518672856ac071827c',
        author: 'anonymous',
        event_id: '01M3C286Y4T8QTQPJ4DDWA6S09',
        changeset_id: '01M3CAHP14HT8AGG0CH60H73HM',
        status: 'open',
        reason: null,
        decided_by: null,
        created_at: '2026-09-25T13:02:19Z',
        base_version: 'v3',
        run_id: null,
        root_event_id: null,
      };

      expect(resolveReviewTarget(realDraft)).toEqual({
        kind: 'draft',
        draftId: '01M3CAHP17503GSZN7QRRA03GA',
      });
    });
  });

  describe('review comments extraction, filtering, and ordering', () => {
    const draftId = '01M3CAHP17503GSZN7QRRA03GA';

    const events: Event[] = [
      {
        event_id: 'ev-not-comment',
        at: '2026-09-26T02:00:00Z',
        source: 'escurel-runner',
        mime: 'application/json',
        label_skill: 'escurel:run',
        instance_page_id: 'p1',
        status: 'processed',
        title: 'run-started',
        body: '{}',
        provenance: null,
        kind: 'system',
        root_event_id: 'r1',
        run_id: 'run-1',
      },
      {
        event_id: 'ev-other-draft',
        at: '2026-09-26T02:30:00Z',
        source: 'workbench',
        mime: 'text/plain',
        label_skill: 'escurel:review-comment',
        instance_page_id: 'p1',
        status: 'inbox',
        title: '',
        body: 'comment on another draft',
        provenance: { review: { draft_id: 'other-draft', line: 2 } },
        kind: 'user',
        root_event_id: 'ev-other-draft',
        run_id: null,
      },
      {
        event_id: 'ev-c2',
        at: '2026-09-26T03:05:00Z',
        source: 'workbench',
        mime: 'text/plain',
        label_skill: 'escurel:review-comment',
        instance_page_id: 'p1',
        status: 'inbox',
        title: '',
        body: 'reply to line 4 comment',
        provenance: {
          review: { draft_id: draftId, line: 4, commented_by: 'alice' },
          captured_by: 'alice',
        },
        kind: 'user',
        root_event_id: 'ev-c2',
        run_id: null,
      },
      {
        event_id: 'ev-c1',
        at: '2026-09-26T03:00:27Z',
        source: 'workbench',
        mime: 'text/plain',
        label_skill: 'escurel:review-comment',
        instance_page_id: 'p1',
        status: 'inbox',
        title: '',
        body: 'the second paragraph overstates the risk',
        provenance: {
          review: { draft_id: draftId, line: 4 },
          captured_by: 'anonymous',
        },
        kind: 'user',
        root_event_id: 'ev-c1',
        run_id: null,
      },
      {
        event_id: 'ev-c0',
        at: '2026-09-26T02:50:00Z',
        source: 'workbench',
        mime: 'text/plain',
        label_skill: 'escurel:review-comment',
        instance_page_id: 'p1',
        status: 'inbox',
        title: '',
        body: 'top-level summary comment on draft overall',
        provenance: {
          review: { draft_id: draftId, commented_by: 'reviewer' },
        },
        kind: 'user',
        root_event_id: 'ev-c0',
        run_id: null,
      },
    ];

    it('extracts comments matching draftId, extracts author, and sorts by at ascending', () => {
      const comments = extractReviewComments(events, draftId);
      expect(comments.length).toBe(3);

      expect(comments[0]).toEqual({
        eventId: 'ev-c0',
        author: 'reviewer',
        body: 'top-level summary comment on draft overall',
        at: '2026-09-26T02:50:00Z',
        line: undefined,
      });

      expect(comments[1]).toEqual({
        eventId: 'ev-c1',
        author: 'anonymous',
        body: 'the second paragraph overstates the risk',
        at: '2026-09-26T03:00:27Z',
        line: 4,
      });

      expect(comments[2]).toEqual({
        eventId: 'ev-c2',
        author: 'alice',
        body: 'reply to line 4 comment',
        at: '2026-09-26T03:05:00Z',
        line: 4,
      });
    });

    it('groups comments into threads: no-line thread first, then sorted by line number', () => {
      const comments = extractReviewComments(events, draftId);
      const threads = groupCommentsIntoThreads(comments);

      expect(threads.length).toBe(2);

      // First thread: no-line comments (top of file)
      expect(threads[0]!.line).toBeUndefined();
      expect(threads[0]!.comments.map((c) => c.eventId)).toEqual(['ev-c0']);

      // Second thread: line 4 comments, preserving chronological order
      expect(threads[1]!.line).toBe(4);
      expect(threads[1]!.comments.map((c) => c.eventId)).toEqual(['ev-c1', 'ev-c2']);
    });

    it('buildReviewCommentThreads combines extraction and grouping directly', () => {
      const threads = buildReviewCommentThreads(events, draftId);
      expect(threads.length).toBe(2);
      expect(threads[0]!.line).toBeUndefined();
      expect(threads[1]!.line).toBe(4);
    });
  });

  describe('promote and discard decision outcomes', () => {
    describe('interpretPromoteDraftError', () => {
      it('treats already_decided as non-failure: refreshes awaiting and closes diff', () => {
        const err = new EscurelError('already_decided', 'draft was already decided', {
          draft: { draft_id: 'd-1', status: 'promoted' },
        });

        const outcome = interpretPromoteDraftError(err, 'd-1');
        expect(outcome.kind).toBe('already_decided');
        expect(outcome.closeDiff).toBe(true);
        expect(outcome.refresh).toBe(true);
        expect(outcome.message).toContain('already decided');
      });

      it('treats conflict as error, leaves diff open and does not close diff', () => {
        const err = new EscurelError('conflict', 'target page moved under draft', {
          head_sha256: 'a'.repeat(64),
        });

        const outcome = interpretPromoteDraftError(err, 'd-1');
        expect(outcome.kind).toBe('conflict');
        expect(outcome.closeDiff).toBe(false);
        expect(outcome.refresh).toBe(false);
        expect(outcome.message).toContain('Conflict');
      });

      it('treats generic errors as errors, leaves diff open', () => {
        const err = new Error('network down');
        const outcome = interpretPromoteDraftError(err, 'd-1');
        expect(outcome.kind).toBe('error');
        expect(outcome.closeDiff).toBe(false);
        expect(outcome.message).toBe('network down');
      });
    });

    describe('interpretPromoteDraftSuccess', () => {
      it('returns success: closes diff and refreshes awaiting', () => {
        const outcome = interpretPromoteDraftSuccess('d-1');
        expect(outcome.kind).toBe('success');
        expect(outcome.closeDiff).toBe(true);
        expect(outcome.refresh).toBe(true);
        expect(outcome.message).toContain('Promoted draft d-1');
      });
    });

    describe('interpretPromoteChangesetResult', () => {
      it('handles already_decided response', () => {
        const res: PromoteChangesetResponse = {
          ok: true,
          changeset_id: 'cs-1',
          already_decided: true,
          results: [],
        };

        const outcome = interpretPromoteChangesetResult(res);
        expect(outcome.kind).toBe('already_decided');
        expect(outcome.closeDiff).toBe(true);
        expect(outcome.refresh).toBe(true);
        expect(outcome.message).toContain('already decided');
      });

      it('handles full success reporting per-draft outcome', () => {
        const res: PromoteChangesetResponse = {
          ok: true,
          changeset_id: 'cs-1',
          results: [
            { draft_id: 'd1', page_id: 'markdown/instances/customer__alpina.md', ok: true },
            {
              draft_id: 'd2',
              page_id: 'markdown/instances/customer__acme.md',
              ok: true,
              already_applied: true,
            },
          ],
        };

        const outcome = interpretPromoteChangesetResult(res);
        expect(outcome.kind).toBe('success');
        expect(outcome.closeDiff).toBe(true);
        expect(outcome.refresh).toBe(true);
        expect(outcome.message).toContain('Promoted changeset cs-1');
        expect(outcome.message).toContain('alpina: applied');
        expect(outcome.message).toContain('acme: already applied');
      });

      it('handles partial changeset result reporting failed vs ok drafts and leaves diff open', () => {
        const res: PromoteChangesetResponse = {
          ok: true,
          changeset_id: 'cs-1',
          partial: true,
          results: [
            { draft_id: 'd1', page_id: 'markdown/instances/customer__alpina.md', ok: true },
            {
              draft_id: 'd2',
              page_id: 'markdown/instances/customer__acme.md',
              ok: false,
              status: 'conflict',
            },
          ],
        };

        const outcome = interpretPromoteChangesetResult(res);
        expect(outcome.kind).toBe('partial');
        expect(outcome.closeDiff).toBe(false);
        expect(outcome.refresh).toBe(true);
        expect(outcome.message).toContain('partially promoted');
        expect(outcome.message).toContain('1 succeeded, 1 failed');
        expect(outcome.message).toContain('acme: conflict');
      });
    });

    describe('interpretDiscardResult and interpretDiscardError', () => {
      it('returns success for discard', () => {
        const dOut = interpretDiscardResult('draft', 'd-1');
        expect(dOut.kind).toBe('success');
        expect(dOut.closeDiff).toBe(true);
        expect(dOut.refresh).toBe(true);

        const csOut = interpretDiscardResult('changeset', 'cs-1');
        expect(csOut.kind).toBe('success');
        expect(csOut.closeDiff).toBe(true);
        expect(csOut.refresh).toBe(true);
      });

      it('handles already_decided error for discard without failing', () => {
        const err = new EscurelError('already_decided', 'draft already decided');
        const outcome = interpretDiscardError(err, 'draft', 'd-1');
        expect(outcome.kind).toBe('already_decided');
        expect(outcome.closeDiff).toBe(true);
        expect(outcome.refresh).toBe(true);
      });

      it('handles generic discard error', () => {
        const err = new Error('gateway failed');
        const outcome = interpretDiscardError(err, 'draft', 'd-1');
        expect(outcome.kind).toBe('error');
        expect(outcome.closeDiff).toBe(false);
        expect(outcome.refresh).toBe(false);
      });
    });
  });
});

/**
 * The shapes above are hand-written; these are what a real gateway answered.
 * The fixtures were recorded against a running `escurel-server` (the crm-demo
 * corpus) after the `escurel:review-comment` carve-out landed, so they pin
 * the model against the wire rather than against our idea of it.
 */
describe('against fixtures recorded from a live gateway', () => {
  const structured = <T>(name: string): T =>
    (fixture(name).response as { result: { structuredContent: T } }).result.structuredContent;

  it('reads the open draft and changeset the gateway reported', () => {
    const { drafts } = structured<{ drafts: Draft[] }>('list_drafts_open');
    const draft = drafts.find((d) => d.status === 'open')!;
    expect(draft.changeset_id).toBeTruthy();
    expect(draft.content.startsWith('---\n')).toBe(true);

    const { changesets } = structured<{ changesets: Changeset[] }>('list_changesets_open');
    const cs = changesets.find((c) => c.changeset_id === draft.changeset_id)!;
    const items = buildChangesetQuickPickItems(cs.changeset_id, [draft], new Map());
    expect(items.map((i) => i.action)).toEqual(['promote_all', 'discard_all', 'draft']);
    expect(items.at(-1)!.label).toContain('ha-spine');
  });

  it('a diff the gateway says has not moved raises no warning', () => {
    const diff = structured<DiffDraftResponse>('diff_draft_ok');
    expect(diff.base_moved).toBe(false);
    expect(checkBaseMoved(diff).baseMoved).toBe(false);
  });

  it('extracts the two real comments and threads them by line', () => {
    const { drafts } = structured<{ drafts: Draft[] }>('list_drafts_open');
    const draftId = drafts.find((d) => d.status === 'open')!.draft_id;
    const { events } = structured<{ events: Event[] }>('list_events_with_comments');

    const comments = extractReviewComments(events, draftId);
    expect(comments.map((c) => c.body)).toEqual([
      'the second paragraph overstates the risk',
      'agreed overall',
    ]);
    // The gateway stamps the author; the caller never sends one.
    expect(comments.every((c) => c.author === 'anonymous')).toBe(true);
    expect(comments.map((c) => c.line)).toEqual([4, undefined]);

    // The unanchored comment opens the file-level thread, the anchored one
    // hangs on its line.
    const threads = groupCommentsIntoThreads(comments);
    expect(threads.map((t) => [t.line, t.comments.length])).toEqual([
      [undefined, 1],
      [4, 1],
    ]);

    // A comment about another draft on the same page is not this draft's.
    expect(extractReviewComments(events, 'some-other-draft')).toEqual([]);
  });
});
