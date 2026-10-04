import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import type { Event, EventsPage } from '../../src/client';
import { buildThreadStrip, findThreadStrip } from '../../src/shared/threadStrip';

// `list_events { instance_page_id, include_system: true }` returns rows of exactly the shape
// `list_events { run_id }` does, which is what the `run-detail-*` recording holds: one run,
// whole. A run's own rows are attached to its target page, so they are what a page's events
// contain.
const events = (
  JSON.parse(
    readFileSync(join(__dirname, 'fixtures', 'lineage', 'run-detail-events.json'), 'utf8'),
  ) as EventsPage
).events;
const finished = events.find((e) => e.label_skill === 'escurel:run' && e.title === 'run-finished')!;

describe('buildThreadStrip', () => {
  it('names the thread and the run that produced the page, from its run-finished row', () => {
    const strip = buildThreadStrip(events);
    expect(strip).toEqual({
      rootEventId: finished.root_event_id,
      runId: finished.run_id,
      runStatus: 'processed',
    });
  });

  it('ignores review transitions and unfinished runs', () => {
    // A page is "produced by" a run only once that run has finished: a run-started row says
    // a run is underway, not that it wrote this version.
    const onlyStarted = events.filter((e) => e.title !== 'run-finished');
    expect(buildThreadStrip(onlyStarted)).toBeUndefined();
    const review = events.filter((e) => e.label_skill === 'escurel:review');
    expect(buildThreadStrip(review)).toBeUndefined();
  });

  it('takes the newest finished run when a page has been through several', () => {
    const older: Event = {
      ...finished,
      event_id: 'older',
      run_id: 'run-old',
      at: '2026-01-01T00:00:00Z',
    };
    const newer: Event = {
      ...finished,
      event_id: 'newer',
      run_id: 'run-new',
      at: '2026-09-01T00:00:00Z',
    };
    expect(buildThreadStrip([newer, older])?.runId).toBe('run-new');
    expect(buildThreadStrip([older, newer])?.runId).toBe('run-new');
  });

  it('carries a failed status rather than hiding it', () => {
    const failed: Event = { ...finished, body: JSON.stringify({ status: 'failed' }) };
    expect(buildThreadStrip([failed])?.runStatus).toBe('failed');
  });

  it('answers undefined for no events, or a run with no thread root', () => {
    expect(buildThreadStrip([])).toBeUndefined();
    const rootless: Event = { ...finished, root_event_id: null, provenance: {} };
    expect(buildThreadStrip([rootless])).toBeUndefined();
  });
});

describe('findThreadStrip', () => {
  // A busy page can have more than one page of newer events than its last finished run.
  const filler = (n: number): Event[] =>
    Array.from({ length: n }, (_, i) => ({
      ...finished,
      event_id: `noise-${i}`,
      label_skill: 'escurel:review',
      title: 'draft-created',
    }));

  it('follows the cursor past newer events to the run that produced the page', async () => {
    const calls: (string | undefined)[] = [];
    const strip = await findThreadStrip(async (cursor) => {
      calls.push(cursor);
      return cursor
        ? { events: [finished] }
        : { events: filler(50), next_cursor: 'page-2', has_more: true };
    });
    expect(calls).toEqual([undefined, 'page-2']);
    expect(strip?.runId).toBe(finished.run_id);
  });

  it('stops at the page cap instead of reading a huge history', async () => {
    let n = 0;
    const strip = await findThreadStrip(async () => {
      n += 1;
      return { events: filler(3), next_cursor: `c${n}`, has_more: true };
    }, 4);
    expect(n).toBe(4);
    expect(strip).toBeUndefined();
  });

  it('degrades to no strip when the read fails, rather than costing the user the page', async () => {
    const strip = await findThreadStrip(async () => {
      throw new Error('boom');
    });
    expect(strip).toBeUndefined();
  });
});
