import { describe, expect, it } from 'vitest';
import type { ListLineageResponse } from '../../src/client/types';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';
import fixtureCascade from './fixtures/lineage/lineage-cascade.json';
import pagedFull from './fixtures/lineage/lineage-paged-full.json';
import paged1 from './fixtures/lineage/lineage-paged-page1.json';
import paged2 from './fixtures/lineage/lineage-paged-page2.json';
import paged3 from './fixtures/lineage/lineage-paged-page3.json';
import paged4 from './fixtures/lineage/lineage-paged-page4.json';
import paged5 from './fixtures/lineage/lineage-paged-page5.json';
import paged6 from './fixtures/lineage/lineage-paged-page6.json';
import paged7 from './fixtures/lineage/lineage-paged-page7.json';
import paged8 from './fixtures/lineage/lineage-paged-page8.json';
import paged9 from './fixtures/lineage/lineage-paged-page9.json';
import fixtureEventRunChangesetDraft from './fixtures/lineage/lineage-event-run-changeset-draft.json';

describe('threadModel', () => {
  // Test 1: Full lineage with all 4 node types in sequence.
  it('1. folding lineage-event-run-changeset-draft.json gives the four nodes with parents event → run → changeset → draft', () => {
    const folded = foldLineage([fixtureEventRunChangesetDraft as unknown as ListLineageResponse]);

    expect(folded.rootEventId).toBe('01M3EXWJ4869148JSG93WB9JGD');
    expect(folded.nodes.size).toBe(4);

    const eventNode = folded.nodes.get('01M3EXWJ4869148JSG93WB9JGD');
    const runNode = folded.nodes.get('01M3EXWJ671BWP2YBS03S1A1JW');
    const changesetNode = folded.nodes.get('01M3EXWJ981KYE49J0Y7DE3EV0');
    const draftNode = folded.nodes.get('01M3EXWJ9AP62MRV24GQYSB6Q8');

    expect(eventNode).toBeDefined();
    expect(runNode).toBeDefined();
    expect(changesetNode).toBeDefined();
    expect(draftNode).toBeDefined();

    // The parent alternation event → run → changeset → draft.
    expect(eventNode?.parent).toBeNull();
    expect(runNode?.parent).toBe(eventNode?.id);
    expect(changesetNode?.parent).toBe(runNode?.id);
    expect(draftNode?.parent).toBe(changesetNode?.id);
  });

  // Test 2: Paging traps — parent fallback resolution and terminal run state preservation.
  it('2. folding every recorded page, in any order, gives exactly the unpaged read', () => {
    // Recorded with `limit: 1`: nine pages. On early pages the changeset's parent is the
    // ROOT (its run is not on that page) and the run is `running`; the run first appears
    // `processed` on page 6. Naive last-write-wins or first-write-wins gets one of those
    // wrong depending on the order, which is why three orders are folded.
    const pages = [paged1, paged2, paged3, paged4, paged5, paged6, paged7, paged8, paged9].map(
      (p) => p as unknown as ListLineageResponse,
    );
    const truth = foldLineage([pagedFull as unknown as ListLineageResponse]);
    const orders = [
      pages,
      [...pages].reverse(),
      [
        pages[5]!,
        pages[0]!,
        pages[8]!,
        pages[2]!,
        pages[7]!,
        pages[1]!,
        pages[4]!,
        pages[3]!,
        pages[6]!,
      ],
    ];
    for (const order of orders) {
      const folded = foldLineage(order);
      expect([...folded.nodes.keys()].sort()).toEqual([...truth.nodes.keys()].sort());
      for (const [id, node] of truth.nodes) {
        expect(folded.nodes.get(id)?.parent, `parent of ${id}`).toBe(node.parent);
        expect(folded.nodes.get(id)?.state, `state of ${id}`).toBe(node.state);
      }
    }
  });

  it('2b. a run is reported as the pages show it, never guessed', () => {
    // Pages 1–5 never carry the run's `run-finished` row, so it is still `running` there.
    // The model must say so rather than infer an end from a promoted child: a run that
    // drafted and then failed would otherwise be shown as having succeeded.
    const early = foldLineage(
      [paged1, paged2, paged3, paged4, paged5].map((p) => p as unknown as ListLineageResponse),
    );
    const run = [...early.nodes.values()].find((n) => n.type === 'run');
    expect(run?.state).toBe('running');
  });

  // Test 3: Cross-skill cascade hop parentage.
  it('3. the cascade hop cascade:<draft_id> hangs off the run in the view', () => {
    const full = fixtureCascade as unknown as ListLineageResponse;
    const folded = foldLineage([full]);
    const view = toThreadView(folded);

    const cascadeHopId = 'cascade:01M3NHJJNME9ERH58A5BEWPKDP';
    const runId = '01M3NHJJGJ1WWAV26F5Z8Y4XKT';

    const hopNode = view.nodes.find((n) => n.id === cascadeHopId);
    const runNode = view.nodes.find((n) => n.id === runId);

    expect(hopNode).toBeDefined();
    expect(runNode).toBeDefined();

    // The cascade hop's parent is the run that produced the promoted draft.
    expect(hopNode?.parent).toBe(runId);
    expect(runNode?.children).toContain(cascadeHopId);
  });

  // Test 4: Pruned / absent parent hanging off root.
  it('4. a node whose parent is absent (page 2 alone) hangs off the root in the view', () => {
    const page2 = paged2 as unknown as ListLineageResponse;
    const folded = foldLineage([page2]);
    const view = toThreadView(folded);

    const runNode = view.nodes.find((n) => n.kind === 'run');

    expect(runNode).toBeDefined();
    // The root event is absent from page 2 alone, so the run hangs off the thread root id.
    expect(runNode?.parent).toBe(folded.rootEventId);
  });

  // Test 5: Gate presence on open changeset vs absence on promoted.
  it('5. gate: present on an open changeset, absent on a promoted one', () => {
    // Fixture changesets are promoted: gate must be absent.
    const foldedPromoted = foldLineage([
      fixtureEventRunChangesetDraft as unknown as ListLineageResponse,
    ]);
    const viewPromoted = toThreadView(foldedPromoted);
    const promotedCsNode = viewPromoted.nodes.find((n) => n.kind === 'changeset');
    expect(promotedCsNode?.gate).toBeUndefined();

    // Hand-written input: fixtures record only promoted runs, so verify an open changeset here.
    const openChangesetPage: ListLineageResponse = {
      root_event_id: 'ev-root',
      nodes: [
        {
          id: 'ev-root',
          type: 'event',
          parent: null,
          state: 'processed',
          label_skill: 'order',
          title: 'incoming order',
          at: '2026-09-29T02:00:00Z',
          kind: 'user',
        },
        {
          id: 'cs-open',
          type: 'changeset',
          parent: 'ev-root',
          state: 'open',
          drafts: 3,
        },
      ],
    };
    const viewOpen = toThreadView(foldLineage([openChangesetPage]));
    const openCsNode = viewOpen.nodes.find((n) => n.id === 'cs-open');
    expect(openCsNode?.gate).toEqual({ drafts: 3, changesetId: 'cs-open' });

    // Hand-written input: an open draft without a changeset carries its own single-draft gate.
    const openDraftPage: ListLineageResponse = {
      root_event_id: 'ev-root',
      nodes: [
        {
          id: 'ev-root',
          type: 'event',
          parent: null,
          state: 'processed',
          label_skill: 'note',
          title: 'note',
          at: '2026-09-29T02:00:00Z',
          kind: 'user',
        },
        {
          id: 'd-solo',
          type: 'draft',
          parent: 'ev-root',
          state: 'open',
          target_page_id: 'markdown/instances/note/memo.md',
        },
      ],
    };
    const viewDraftOpen = toThreadView(foldLineage([openDraftPage]));
    const soloDraftNode = viewDraftOpen.nodes.find((n) => n.id === 'd-solo');
    expect(soloDraftNode?.gate).toEqual({ drafts: 1, draftId: 'd-solo' });
  });

  // Test 6: Tone for failed, dead_letter, and cancelled runs.
  it('6. tone for a failed run', () => {
    // Hand-written input: fixtures only hold successful/running executions; test failure states here.
    const failedPage: ListLineageResponse = {
      root_event_id: 'ev-root',
      nodes: [
        {
          id: 'ev-root',
          type: 'event',
          parent: null,
          state: 'processed',
          label_skill: 'signal',
          at: '2026-09-29T02:00:00Z',
          kind: 'user',
        },
        {
          id: 'run-failed',
          type: 'run',
          parent: 'ev-root',
          state: 'failed',
          started_at: '2026-09-29T02:00:00Z',
        },
        {
          id: 'run-dead-letter',
          type: 'run',
          parent: 'ev-root',
          state: 'dead_letter',
          started_at: '2026-09-29T02:01:00Z',
        },
        {
          id: 'run-cancelled',
          type: 'run',
          parent: 'ev-root',
          state: 'cancelled',
          started_at: '2026-09-29T02:02:00Z',
        },
      ],
    };

    const view = toThreadView(foldLineage([failedPage]));

    const failedNode = view.nodes.find((n) => n.id === 'run-failed');
    const deadLetterNode = view.nodes.find((n) => n.id === 'run-dead-letter');
    const cancelledNode = view.nodes.find((n) => n.id === 'run-cancelled');

    expect(failedNode?.tone).toBe('failed');
    expect(failedNode?.chips).toEqual([{ text: 'failed', tone: 'failed' }]);

    expect(deadLetterNode?.tone).toBe('failed');
    expect(deadLetterNode?.chips).toEqual([{ text: 'dead_letter', tone: 'failed' }]);

    expect(cancelledNode?.tone).toBe('failed');
    expect(cancelledNode?.chips).toEqual([{ text: 'cancelled', tone: 'failed' }]);
  });

  // Additional tests: Card metadata, sorting, navigation targets, and columns.
  describe('card representation and layout metadata', () => {
    it('populates cards with mock-aligned title, subtitle, meta, chips, targets and collapsibility', () => {
      const full = fixtureCascade as unknown as ListLineageResponse;
      const view = toThreadView(foldLineage([full]));

      const rootEvent = view.nodes.find((n) => n.id === '01M3NHJJCWP2TXH9TQT46F70R1');
      const run = view.nodes.find((n) => n.id === '01M3NHJJGJ1WWAV26F5Z8Y4XKT');
      const changeset = view.nodes.find((n) => n.id === '01M3NHJJNJ7P58Q9A7PWEYSA9X');
      const draft = view.nodes.find((n) => n.id === '01M3NHJJNME9ERH58A5BEWPKDP');

      // Root event card
      expect(rootEvent?.title).toBe('signal');
      expect(rootEvent?.subtitle).toBe('supplier risk');
      expect(rootEvent?.tone).toBe('event');
      expect(rootEvent?.target).toEqual({ open: 'thread', rootEventId: rootEvent?.id });

      // Run card inherits the triggering event's label_skill as title
      expect(run?.title).toBe('signal');
      expect(run?.subtitle).toBe('run');
      expect(run?.tone).toBe('run');
      expect(run?.target).toEqual({ open: 'run', runId: run?.id });
      expect(run?.collapsible).toBe(true);

      // Changeset card
      expect(changeset?.title).toBe('changeset 01M3NHJJ');
      expect(changeset?.tone).toBe('instance');
      expect(changeset?.meta).toEqual(['1 draft']);
      expect(changeset?.target).toEqual({ open: 'review', changesetId: changeset?.id });

      // Draft card uses slug from target_page_id
      expect(draft?.title).toBe('o1');
      expect(draft?.tone).toBe('instance');
      expect(draft?.target).toEqual({
        open: 'review',
        draftId: draft?.id,
        changesetId: changeset?.id,
      });

      // Default 5 columns from mock
      expect(view.columns).toEqual([
        'root event',
        'run · changeset',
        'instances · drafts',
        'cascade · depth 1',
        'outbound · depth 2',
      ]);
    });

    it('extends columns dynamically when execution depth exceeds 2', () => {
      // Hand-written input: depth > 2 test
      const deepLineage: ListLineageResponse = {
        root_event_id: 'ev-root',
        nodes: [
          {
            id: 'ev-root',
            type: 'event',
            parent: null,
            state: 'processed',
            depth: null,
          },
          {
            id: 'ev-deep',
            type: 'event',
            parent: 'ev-root',
            state: 'inbox',
            depth: 4,
          },
        ],
      };

      const view = toThreadView(foldLineage([deepLineage]));
      expect(view.columns).toEqual([
        'root event',
        'run · changeset',
        'instances · drafts',
        'cascade · depth 1',
        'outbound · depth 2',
        'cascade · depth 3',
        'cascade · depth 4',
      ]);
    });
  });
});
