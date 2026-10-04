import { describe, expect, it } from 'vitest';
import type { Event } from '../../src/client';
import { nodeRefs, skillThreadItems } from '../../src/commands/nodeRefs';

const ev = (extra: Partial<Event>): Event =>
  ({
    event_id: 'e1',
    label_skill: 'supplier-risk',
    kind: 'user',
    status: 'inbox',
    ...extra,
  }) as Event;

// A row of any tree must be able to say where it leads: its thread, its run, its skill, its page.
describe('nodeRefs', () => {
  it('an Inbox event leads to its thread (the root, else itself), its skill and its page', () => {
    const row = {
      kind: 'event',
      pageId: 'markdown/instances/customer-order/order-1.md',
      event: ev({
        event_id: 'e1',
        root_event_id: 'root-1',
        instance_page_id: 'markdown/instances/customer-order/order-1.md',
      }),
    };
    expect(nodeRefs(row)).toEqual({
      rootEventId: 'root-1',
      skill: 'supplier-risk',
      pageId: 'markdown/instances/customer-order/order-1.md',
    });
    expect(nodeRefs({ kind: 'event', event: ev({ event_id: 'e9' }) }).rootEventId).toBe('e9');
  });

  it('an Awaiting changeset leads to its thread and run, and to the skill of the pages it changes', () => {
    const row = {
      kind: 'changeset',
      changeset: {
        changeset_id: 'c1',
        root_event_id: 'root-2',
        run_id: 'run-2',
        target_page_ids: ['markdown/instances/supplier-risk-analysis__meier-guss-2026-10-04.md'],
      },
    };
    expect(nodeRefs(row)).toEqual({
      rootEventId: 'root-2',
      runId: 'run-2',
      skill: 'supplier-risk-analysis',
    });
  });

  it('an Awaiting draft leads to its thread, run, page and skill', () => {
    const row = {
      kind: 'draft',
      draft: {
        draft_id: 'd1',
        root_event_id: 'root-3',
        run_id: 'run-3',
        target_page_id: 'markdown/instances/customer-order/order-1.md',
      },
    };
    expect(nodeRefs(row)).toEqual({
      rootEventId: 'root-3',
      runId: 'run-3',
      pageId: 'markdown/instances/customer-order/order-1.md',
      skill: 'customer-order',
    });
  });

  it('a Knowledge instance names its page and skill; a skill row names the skill', () => {
    expect(nodeRefs({ kind: 'instance', pageId: 'p.md', skill: 'supplier' })).toEqual({
      pageId: 'p.md',
      skill: 'supplier',
    });
    expect(nodeRefs({ kind: 'skill', skill: { id: 'supplier-risk-report' } })).toEqual({
      skill: 'supplier-risk-report',
    });
  });

  it('knows nothing of a thing it does not recognise', () => {
    expect(nodeRefs(undefined)).toEqual({});
    expect(nodeRefs('x')).toEqual({});
    expect(nodeRefs({ kind: 'folder' })).toEqual({});
  });
});

// "Show threads using this skill": the signals filed under it, one row per thread, newest first.
describe('skillThreadItems', () => {
  it('lists the threads of a skill by their trigger event, newest first, without system rows', () => {
    const items = skillThreadItems(
      [
        ev({ event_id: 'a', title: 'PO 1 moved', at: '2026-10-04T07:00:00Z', status: 'processed' }),
        ev({ event_id: 'b', title: 'PO 2 moved', at: '2026-10-04T08:00:00Z' }),
        ev({ event_id: 'c', kind: 'system', title: 'run-started' }),
      ],
      Date.parse('2026-10-04T09:00:00Z'),
    );
    expect(items.map((i) => i.rootEventId)).toEqual(['b', 'a']);
    expect(items[0]).toMatchObject({
      label: 'PO 2 moved',
      description: expect.stringContaining('1h ago'),
    });
  });

  it('uses the thread root when the event belongs to one, and lists a thread once', () => {
    const items = skillThreadItems([
      ev({ event_id: 'a', root_event_id: 'root', at: '2026-10-04T07:00:00Z' }),
      ev({ event_id: 'b', root_event_id: 'root', at: '2026-10-04T08:00:00Z' }),
    ]);
    expect(items.map((i) => i.rootEventId)).toEqual(['root']);
  });

  it('is empty for a skill nothing was filed under', () => {
    expect(skillThreadItems([])).toEqual([]);
  });
});
