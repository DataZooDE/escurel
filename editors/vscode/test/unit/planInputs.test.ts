import { describe, expect, it } from 'vitest';
import type { Event, EventsPage } from '../../src/client/types';
import { loadPlanInputs } from '../../src/views/planInputs';
import { planRows } from '../../src/views/planRows';

const ev = (o: Partial<Event> & { event_id: string }): Event =>
  ({ kind: 'user', at: '2026-01-01T00:00:00Z', ...o }) as Event;

/** A tiny paging gateway: newest first, 100 per page, opaque cursor = offset. */
function gateway(all: { runs: Event[]; byRoot: Record<string, Event[]>; bySkill: Record<string, Event[]> }) {
  const page = (rows: Event[], cursor?: string, limit = 100): EventsPage => {
    const from = cursor ? Number(cursor) : 0;
    const slice = rows.slice(from, from + limit);
    const more = from + limit < rows.length;
    return { events: slice, has_more: more, next_cursor: more ? String(from + limit) : null } as EventsPage;
  };
  return {
    listEvents: async (q: Record<string, unknown>) => {
      if (q.label_skill === 'escurel:run') return page(all.runs, q.cursor as string | undefined);
      if (typeof q.root_event_id === 'string') return page(all.byRoot[q.root_event_id] ?? []);
      return page(all.bySkill[q.label_skill as string] ?? [], q.cursor as string | undefined);
    },
  };
}

describe('loadPlanInputs', () => {
  it('learns an approval that is older than the newest 100 events of its skill', async () => {
    const plan = ev({
      event_id: 'run:P:finished',
      kind: 'system',
      run_id: 'P',
      root_event_id: 'ROOT',
      label_skill: 'escurel:run',
      at: '2026-01-02T00:00:00Z',
      body: JSON.stringify({ status: 'planned' }),
    });
    const trigger = ev({ event_id: 'ROOT', label_skill: 'order', at: '2026-01-01T00:00:00Z' });
    const approval = ev({
      event_id: 'APPROVAL',
      label_skill: 'order',
      at: '2026-01-02T01:00:00Z',
      provenance: { manual: { approved_plan_run_id: 'P' } },
    });
    // 150 newer events of the same skill push the approval off the first page.
    const noise = Array.from({ length: 150 }, (_, i) =>
      ev({ event_id: `N${i}`, label_skill: 'order', at: '2026-02-01T00:00:00Z' }),
    );
    const client = gateway({
      runs: [plan],
      byRoot: { ROOT: [trigger] },
      bySkill: { order: [...noise, approval, trigger] },
    });
    const inputs = await loadPlanInputs(client as never);
    expect(planRows(inputs.runEvents, inputs.userEvents)).toEqual([]);
  });
});
