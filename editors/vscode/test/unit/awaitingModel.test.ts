import { describe, expect, it } from 'vitest';
import type { Changeset, Draft, Event, Skill } from '../../src/client';
import {
  buildAwaitingRows,
  changesetRow,
  confirmGateRow,
  draftRow,
  isConfirmGate,
  sortAwaitingNewestFirst,
} from '../../src/views/awaitingModel';

describe('awaitingModel', () => {
  describe('changesetRow', () => {
    it('sets label to changeset_id and description to "N drafts · <author>"', () => {
      const cs: Changeset = {
        changeset_id: 'cs-42',
        run_id: 'run-1',
        author: 'agt:lead-scorer',
        status: 'open',
        drafts: 3,
        target_page_ids: ['markdown/instances/customer__alpina-biotech.md'],
        event_ids: ['ev-1'],
        created_at: '2026-09-25T10:00:00Z',
        root_event_id: null,
      };
      const row = changesetRow(cs);
      expect(row.kind).toBe('changeset');
      expect(row.id).toBe('cs-42');
      expect(row.label).toBe('cs-42');
      expect(row.description).toBe('3 drafts · agt:lead-scorer');
      expect(row.timestamp).toBe('2026-09-25T10:00:00Z');
    });

    it('formats singular "1 draft · <author>" when changeset holds exactly one draft', () => {
      const cs: Changeset = {
        changeset_id: '01M3CAHP14HT8AGG0CH60H73HM',
        run_id: null,
        author: 'anonymous',
        status: 'open',
        drafts: 1,
        target_page_ids: ['markdown/instances/engagement__ha-spine.md'],
        event_ids: ['ev-1'],
        created_at: '2026-09-25T13:02:19Z',
        root_event_id: null,
      };
      const row = changesetRow(cs);
      expect(row.description).toBe('1 draft · anonymous');
    });
  });

  describe('draftRow', () => {
    it('sets label to slug of target_page_id and description to author', () => {
      const draft: Draft = {
        draft_id: 'd-1',
        target_page_id: 'markdown/instances/customer__alpina-biotech.md',
        content: '# Alpina Biotech\n',
        content_sha256: 'abc',
        base_sha256: null,
        author: 'agt:analyst',
        event_id: 'ev-1',
        changeset_id: null,
        status: 'open',
        reason: null,
        decided_by: null,
        created_at: '2026-09-25T09:00:00Z',
        base_version: null,
        run_id: null,
        root_event_id: null,
      };
      const row = draftRow(draft);
      expect(row.kind).toBe('draft');
      expect(row.id).toBe('d-1');
      expect(row.label).toBe('alpina-biotech');
      expect(row.description).toBe('agt:analyst');
      expect(row.timestamp).toBe('2026-09-25T09:00:00Z');
    });
  });

  describe('confirmGateRow and isConfirmGate', () => {
    const confirmSkill: Skill = {
      id: 'customer_notice',
      description: 'Customer notification',
      required_frontmatter: [],
      optional_frontmatter: [],
      is_event_typed: true,
      visibility: 'public',
      owner_field: null,
      backend: { kind: 'markdown' },
      capabilities: { writable: true, granularity: 'block', search: 'hybrid', supports_crdt: true },
      layer: 'overlay',
      autonomy: 'confirm',
    };

    const reviewSkill: Skill = {
      ...confirmSkill,
      id: 'review_skill',
      autonomy: 'review',
    };

    const defaultSkill: Skill = {
      ...confirmSkill,
      id: 'default_skill',
      autonomy: undefined, // absent
    };

    const skillMap = new Map<string, Skill>([
      [confirmSkill.id, confirmSkill],
      [reviewSkill.id, reviewSkill],
      [defaultSkill.id, defaultSkill],
    ]);

    it('identifies confirm gates only when skill declares autonomy: confirm', () => {
      const inboxEvent: Event = {
        event_id: 'ev-1',
        at: '2026-09-25T08:00:00Z',
        source: 'workbench',
        mime: 'text/plain',
        label_skill: 'customer_notice',
        instance_page_id: null,
        status: 'inbox',
        title: 'Confirm notice · 4500123 Hoffmann',
        body: null,
        provenance: null,
        kind: 'user',
        root_event_id: 'ev-1',
        run_id: null,
      };

      expect(isConfirmGate(inboxEvent, skillMap)).toBe(true);

      // Absent autonomy must NOT be confirm (absent means review)
      expect(isConfirmGate({ ...inboxEvent, label_skill: 'default_skill' }, skillMap)).toBe(false);

      // Explicit review is NOT confirm
      expect(isConfirmGate({ ...inboxEvent, label_skill: 'review_skill' }, skillMap)).toBe(false);

      // Non-inbox status is NOT awaiting
      expect(isConfirmGate({ ...inboxEvent, status: 'processed' }, skillMap)).toBe(false);
    });

    it('builds a confirm gate row with event title and confirm description', () => {
      const inboxEvent: Event = {
        event_id: 'ev-1',
        at: '2026-09-25T08:00:00Z',
        source: 'workbench',
        mime: 'text/plain',
        label_skill: 'customer_notice',
        instance_page_id: null,
        status: 'inbox',
        title: 'Confirm notice · 4500123 Hoffmann',
        body: null,
        provenance: null,
        kind: 'user',
        root_event_id: 'ev-1',
        run_id: null,
      };

      const row = confirmGateRow(inboxEvent);
      expect(row.kind).toBe('confirm_gate');
      expect(row.id).toBe('ev-1');
      expect(row.label).toBe('Confirm notice · 4500123 Hoffmann');
      expect(row.description).toBe('confirm · customer_notice');
      expect(row.timestamp).toBe('2026-09-25T08:00:00Z');
    });

    it('falls back to event_id when event has no title', () => {
      const inboxEvent: Event = {
        event_id: 'ev-99',
        at: '2026-09-25T08:00:00Z',
        source: 'workbench',
        mime: 'text/plain',
        label_skill: 'customer_notice',
        instance_page_id: null,
        status: 'inbox',
        title: null,
        body: null,
        provenance: null,
        kind: 'user',
        root_event_id: 'ev-99',
        run_id: null,
      };

      const row = confirmGateRow(inboxEvent);
      expect(row.label).toBe('ev-99');
    });
  });

  describe('buildAwaitingRows', () => {
    it('merges open changesets, open unparented drafts, and confirm gates, newest first', () => {
      const csOpen: Changeset = {
        changeset_id: 'cs-open',
        run_id: 'r1',
        author: 'agt:one',
        status: 'open',
        drafts: 2,
        target_page_ids: [],
        event_ids: [],
        created_at: '2026-09-25T08:00:00Z',
        root_event_id: null,
      };
      const csPromoted: Changeset = {
        ...csOpen,
        changeset_id: 'cs-promoted',
        status: 'promoted',
        created_at: '2026-09-25T12:00:00Z',
      };

      const draftStandalone: Draft = {
        draft_id: 'd-standalone',
        target_page_id: 'markdown/instances/customer__corp.md',
        content: '',
        content_sha256: '',
        base_sha256: null,
        author: 'human',
        event_id: null,
        changeset_id: null,
        status: 'open',
        reason: null,
        decided_by: null,
        created_at: '2026-09-25T10:00:00Z',
        base_version: null,
        run_id: null,
        root_event_id: null,
      };
      const draftInChangeset: Draft = {
        ...draftStandalone,
        draft_id: 'd-in-cs',
        changeset_id: 'cs-open',
        created_at: '2026-09-25T11:00:00Z',
      };
      const draftClosed: Draft = {
        ...draftStandalone,
        draft_id: 'd-closed',
        status: 'promoted',
        created_at: '2026-09-25T11:30:00Z',
      };

      const confirmSkill: Skill = {
        id: 'gate_skill',
        description: '',
        required_frontmatter: [],
        optional_frontmatter: [],
        is_event_typed: true,
        visibility: 'public',
        owner_field: null,
        backend: { kind: 'markdown' },
        capabilities: {
          writable: true,
          granularity: 'block',
          search: 'hybrid',
          supports_crdt: true,
        },
        layer: 'overlay',
        autonomy: 'confirm',
      };

      const gateEvent: Event = {
        event_id: 'ev-gate',
        at: '2026-09-25T09:00:00Z',
        source: 'workbench',
        mime: null,
        label_skill: 'gate_skill',
        instance_page_id: null,
        status: 'inbox',
        title: 'Pending gate',
        body: null,
        provenance: null,
        kind: 'user',
        root_event_id: 'ev-gate',
        run_id: null,
      };

      const rows = buildAwaitingRows({
        changesets: [csOpen, csPromoted],
        drafts: [draftStandalone, draftInChangeset, draftClosed],
        events: [gateEvent],
        skills: [confirmSkill],
      });

      // csPromoted, draftInChangeset, draftClosed are filtered out.
      // Remaining: draftStandalone (10:00), gateEvent (09:00), csOpen (08:00).
      expect(rows.map((r) => r.id)).toEqual(['d-standalone', 'ev-gate', 'cs-open']);
      expect(rows[0]!.kind).toBe('draft');
      expect(rows[1]!.kind).toBe('confirm_gate');
      expect(rows[2]!.kind).toBe('changeset');
    });
  });

  describe('sortAwaitingNewestFirst', () => {
    it('sorts newest first by timestamp, breaking ties by id', () => {
      const r1 = {
        kind: 'changeset' as const,
        id: 'a',
        label: 'a',
        description: '',
        timestamp: '2026-09-25T10:00:00Z',
        changeset: {} as Changeset,
      };
      const r2 = {
        kind: 'draft' as const,
        id: 'b',
        label: 'b',
        description: '',
        timestamp: '2026-09-25T11:00:00Z',
        draft: {} as Draft,
      };
      const r3 = {
        kind: 'confirm_gate' as const,
        id: 'c',
        label: 'c',
        description: '',
        timestamp: '2026-09-25T10:00:00Z',
        event: {} as Event,
      };

      const sorted = sortAwaitingNewestFirst([r1, r2, r3]);
      expect(sorted.map((r) => r.id)).toEqual(['b', 'c', 'a']);
    });
  });
});
