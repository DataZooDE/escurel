import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import type { ListLineageResponse } from '../../src/client/types';
import type { ThreadNode } from '../../src/shared/protocol';
import { formatAge } from '../../src/shared/time';
import {
  CARD_HEIGHT,
  COMPACT_HEIGHT,
  NEEDS_YOU_HEIGHT,
  SHORT_HEIGHT,
  heightFor,
} from '../../src/thread/layout';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';

const branches = JSON.parse(
  readFileSync(join(__dirname, 'fixtures', 'lineage', 'lineage-review-branches.json'), 'utf8'),
) as ListLineageResponse;
const view = toThreadView(foldLineage([branches]));
const byKind = (kind: string, state: string) =>
  view.nodes.find((n) => n.kind === kind && n.state === state);

// Work that waits on a person must be impossible to miss. What waits: an open changeset (review), a
// planned run (approve the plan), a run that failed or dead-lettered (retry or fix).
describe('what needs you', () => {
  it('marks an open changeset for review', () => {
    expect(byKind('changeset', 'open')?.needsYou).toEqual({
      reason: 'review',
      text: 'Review changes',
    });
  });

  it('marks a planned run for approval', () => {
    expect(byKind('run', 'planned')?.needsYou).toEqual({
      reason: 'approve-plan',
      text: 'Approve the plan',
    });
  });

  it('marks a failed or dead-lettered run', () => {
    expect(byKind('run', 'dead_letter')?.needsYou?.reason).toBe('failed');
    const failed = toThreadView(
      foldLineage([
        {
          root_event_id: 'e',
          nodes: [
            { id: 'e', type: 'event', parent: null, state: 'processed' },
            { id: 'r', type: 'run', parent: 'e', state: 'failed' },
          ],
        },
      ]),
    );
    expect(failed.nodes.find((n) => n.id === 'r')?.needsYou?.reason).toBe('failed');
  });

  it('marks a run that waits on a human answer (the state is not in the lineage yet, so this is dormant)', () => {
    const waiting = toThreadView(
      foldLineage([
        {
          root_event_id: 'e',
          nodes: [
            { id: 'e', type: 'event', parent: null, state: 'processed' },
            { id: 'r', type: 'run', parent: 'e', state: 'awaiting_human' },
          ],
        },
      ]),
    );
    expect(waiting.nodes.find((n) => n.id === 'r')?.needsYou?.reason).toBe('ask-human');
  });

  it('does not mark finished or running work, nor the drafts inside an open changeset', () => {
    expect(byKind('run', 'processed')?.needsYou).toBeUndefined();
    expect(byKind('event', 'processed')?.needsYou).toBeUndefined();
    // A draft inside an open changeset is decided through the changeset, which carries the mark.
    expect(
      view.nodes.filter((n) => n.kind === 'draft').every((n) => n.needsYou === undefined),
    ).toBe(true);
  });

  it('gives a card that needs you its own emphasis', () => {
    for (const n of view.nodes.filter((x) => x.needsYou)) expect(n.emphasis).toBe('needs-you');
  });

  it('marks a draft nobody has put in a changeset (a live human draft or a gate)', () => {
    const lone = toThreadView(
      foldLineage([
        {
          root_event_id: 'e',
          nodes: [
            { id: 'e', type: 'event', parent: null, state: 'processed' },
            {
              id: 'd',
              type: 'draft',
              parent: 'e',
              state: 'open',
              target_page_id: 'markdown/instances/a__b.md',
            },
          ],
        },
      ]),
    );
    expect(lone.nodes.find((n) => n.id === 'd')?.needsYou?.reason).toBe('review');
  });
});

describe('the changeset card', () => {
  const changeset = byKind('changeset', 'open') as ThreadNode;

  it('knows who proposed it, when, and which pages it changes', () => {
    expect(changeset.changeset?.author).toBe('agent:supplier-risk');
    expect(changeset.changeset?.at).toBe('2026-10-03T08:00:06Z');
    expect(changeset.changeset?.drafts.map((d) => d.title)).toEqual([
      'order-4500131',
      'meier-guss-2026-10-03',
    ]);
  });

  it('lists each draft by its own id so it can be opened', () => {
    expect(changeset.changeset?.drafts.map((d) => d.id)).toEqual([
      '01M4DRF1000000000000000001',
      '01M4DRF2000000000000000002',
    ]);
  });
});

describe('formatAge', () => {
  const now = new Date('2026-10-03T08:10:00Z');
  it('says it in words a person uses', () => {
    expect(formatAge('2026-10-03T08:09:40Z', now)).toBe('just now');
    expect(formatAge('2026-10-03T08:07:00Z', now)).toBe('3 min ago');
    expect(formatAge('2026-10-03T05:10:00Z', now)).toBe('3 h ago');
    expect(formatAge('2026-10-01T08:10:00Z', now)).toBe('2 d ago');
  });
  it('says nothing for a time that is missing or unreadable', () => {
    expect(formatAge(undefined, now)).toBe('');
    expect(formatAge('garbage', now)).toBe('');
  });
});

describe('card heights by emphasis', () => {
  const base = (over: Partial<ThreadNode>): ThreadNode => ({
    id: 'n',
    kind: 'run',
    parent: null,
    children: [],
    state: null,
    tone: 'neutral',
    title: 'n',
    meta: [],
    chips: [],
    target: { open: 'nothing' },
    collapsible: false,
    ...over,
  });

  it('gives a card that needs you more room than a normal one, and a finished one the least', () => {
    expect(heightFor(base({ emphasis: 'compact' }))).toBe(COMPACT_HEIGHT);
    expect(heightFor(base({ emphasis: 'normal', meta: ['a line'] }))).toBe(CARD_HEIGHT);
    expect(heightFor(base({ emphasis: 'needs-you' }))).toBe(NEEDS_YOU_HEIGHT);
    expect(NEEDS_YOU_HEIGHT > CARD_HEIGHT).toBe(true);
  });

  it('gives a normal card with nothing to show but its title and state a shorter box', () => {
    // An open draft page inside a changeset had a full-height box with nothing in it.
    expect(heightFor(base({ emphasis: 'normal', meta: [] }))).toBe(SHORT_HEIGHT);
    expect(heightFor(base({ emphasis: 'normal', meta: ['08:00 · user'] }))).toBe(CARD_HEIGHT);
    expect(SHORT_HEIGHT > COMPACT_HEIGHT && SHORT_HEIGHT < CARD_HEIGHT).toBe(true);
  });

  it('grows an open changeset with the drafts it lists, up to a limit', () => {
    const drafts = (n: number) =>
      Array.from({ length: n }, (_, i) => ({ id: `d${i}`, title: `p${i}` }));
    const h = (n: number) =>
      heightFor(
        base({ kind: 'changeset', emphasis: 'needs-you', changeset: { drafts: drafts(n) } }),
      );
    expect(h(1) < h(3)).toBe(true);
    expect(h(9)).toBe(h(5)); // more than the card can list is summarised as "+N more"
  });
});
