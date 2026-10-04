import { describe, expect, it } from 'vitest';
import type { EscurelClient, Event } from '../../src/client';
import {
  MAX_LOADED_EVENTS,
  emptySnapshot,
  loadOlderRunEvents,
  redrawNeeded,
  refreshRunEvents,
} from '../../src/views/runsLoader';
import { buildRunsTree, foldRuns } from '../../src/views/runsModel';

const ev = (i: number, title = 'run-started'): Event =>
  ({
    event_id: `run:R${i}:${title}`,
    kind: 'system',
    label_skill: 'escurel:run',
    run_id: `R${i}`,
    title,
    at: new Date(1_700_000_000_000 + i * 1000).toISOString(),
    body: '{}',
  }) as Event;

/** A gateway holding `n` run events, newest first, 100 per page. */
function gateway(n: number): EscurelClient {
  const all = Array.from({ length: n }, (_, i) => ev(n - i));
  return {
    listEvents: async (q: { cursor?: string; limit?: number }) => {
      const from = q.cursor ? Number(q.cursor) : 0;
      const slice = all.slice(from, from + 100);
      const more = from + 100 < all.length;
      return { events: slice, has_more: more, next_cursor: more ? String(from + 100) : null };
    },
  } as unknown as EscurelClient;
}

describe('the runs snapshot is bounded', () => {
  it('Load more stops at the cap instead of holding every event ever read', async () => {
    const client = gateway(MAX_LOADED_EVENTS * 3);
    let snap = await refreshRunEvents(client, emptySnapshot(), { wantEnded: 1, maxPages: 1 });
    for (let i = 0; i < 200 && snap.hasMoreOlder; i += 1) {
      snap = await loadOlderRunEvents(client, snap, 1000);
    }
    expect(snap.events.size).toBeLessThanOrEqual(MAX_LOADED_EVENTS + 100);
    expect(snap.hasMoreOlder).toBe(false);
  });

  it('folding and building the tree for a full snapshot stays cheap', () => {
    const events = Array.from({ length: MAX_LOADED_EVENTS }, (_, i) => ev(i));
    const t0 = performance.now();
    const records = foldRuns(events, { nowMs: Date.now() });
    buildRunsTree({
      records,
      filter: {},
      nowMs: Date.now(),
      historyLimit: 25,
      hasMoreHistory: false,
      runner: undefined,
      isAdmin: false,
    });
    expect(performance.now() - t0).toBeLessThan(2000);
    expect(records.length).toBe(MAX_LOADED_EVENTS);
  });
});

describe('redrawNeeded: the 2 s tick only repaints what is on screen and changes', () => {
  it('does nothing while the view is hidden or the window unfocused', () => {
    expect(redrawNeeded({ visible: false, focused: true, anyRunning: true })).toBe('none');
    expect(redrawNeeded({ visible: true, focused: false, anyRunning: true })).toBe('none');
  });
  it('refreshes only the header when nothing is running, the whole tree when something is', () => {
    expect(redrawNeeded({ visible: true, focused: true, anyRunning: false })).toBe('header');
    expect(redrawNeeded({ visible: true, focused: true, anyRunning: true })).toBe('tree');
  });
});
