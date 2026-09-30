import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import type { Event, GetRunToolCallsResponse, LineageNode } from '../../src/client';
import { buildRunView, mergeToolCallPage } from '../../src/runs/runModel';

const recorded = <T>(name: string): T =>
  JSON.parse(readFileSync(join(__dirname, 'fixtures/lineage', name), 'utf8')) as T;

const lineage = recorded<{ nodes: LineageNode[] }>('lineage-cascade.json');
const runNode = lineage.nodes.find((node) => node.type === 'run');
const runEvents = recorded<{ events: Event[] }>('run-events.json').events;
const page1 = recorded<GetRunToolCallsResponse>('run-tool-calls-page1.json');
const page2 = recorded<GetRunToolCallsResponse>('run-tool-calls-page2.json');

describe('run model against recorded gateway and runner responses', () => {
  it('builds the run from its own rows without review transitions leaking in', () => {
    const view = buildRunView(runNode, runEvents);
    const ownRows = runEvents.filter((event) => event.label_skill === 'escurel:run');

    expect(runEvents.length).toBeGreaterThan(ownRows.length);
    expect(view.status).toBe('processed');
    expect(view.tone).toBe('run');
    expect(view.attempts).toHaveLength(1);
    expect(view.attempts[0]).toMatchObject({ n: 1, outcome: 'ok' });
    expect(view.plan).toEqual([
      { step: 'read the target page', status: 'completed' },
      { step: 'draft the fold for review', status: 'in_progress' },
    ]);
    expect(view.toolCallCount).toBe(runNode?.tool_calls);
    expect(view.calls).toEqual([]);
    expect(JSON.stringify(view)).not.toContain('draft-created');
    expect(view).toEqual(buildRunView(runNode, ownRows));
  });

  it('normalizes the recorded zone-less run-attempt timestamps to UTC', () => {
    const attempt = buildRunView(runNode, runEvents).attempts[0];
    expect(attempt?.startedAt).toBe('2026-09-29T02:59:08.035Z');
    expect(attempt?.endedAt).toBe('2026-09-29T02:59:08.210Z');
  });

  it('merges tool-call pages by seq without mutating the original view', () => {
    const initial = buildRunView(runNode, runEvents);
    const first = mergeToolCallPage(initial, page1);
    const repeated = mergeToolCallPage(first, page1);
    const complete = mergeToolCallPage(repeated, page2);

    expect(initial.calls).toEqual([]);
    expect(first.nextAfter).toBe(2);
    expect(repeated.calls).toHaveLength(2);
    expect(complete.calls.map((call) => call.seq)).toEqual([1, 2, 3, 4]);
    expect(complete.nextAfter).toBeNull();
    expect(complete.calls[0]).toMatchObject({
      tool: 'list_inbox',
      bytes: { request: 2, response: 1058 },
    });
  });
});

describe('hand-written edge inputs absent from the recordings', () => {
  it('ignores a malformed JSON body', () => {
    const malformed = { ...runEvents[0]!, body: '{invalid', title: 'run-finished' };
    expect(() => buildRunView(undefined, [malformed])).not.toThrow();
    expect(buildRunView(undefined, [malformed]).status).toBe('running');
  });

  it('uses the failed tone for a failed run', () => {
    const failed: LineageNode = { id: 'failed-run', type: 'run', parent: null, state: 'failed' };
    expect(buildRunView(failed, []).tone).toBe('failed');
  });

  it('normalizes non-finite call metrics and timestamps while preserving unparseable times', () => {
    // The recording has finite metrics; a nullable gateway response needs a hand-written row.
    const nullable = {
      ...page1.calls[0]!,
      duration_ms: null,
      request_bytes: Number.NaN,
      response_bytes: Number.POSITIVE_INFINITY,
      at: '2026-09-29 02:59:08.035678',
    } as unknown as GetRunToolCallsResponse['calls'][number];
    const first = mergeToolCallPage(buildRunView(undefined, []), {
      ...page1,
      calls: [nullable],
    });
    expect(first.calls[0]).toMatchObject({
      durationMs: 0,
      bytes: { request: 0, response: 0 },
      at: '2026-09-29T02:59:08.035Z',
    });
    const second = mergeToolCallPage(first, {
      ...page1,
      calls: [{ ...page1.calls[1]!, at: 'unparseable' }],
    });
    expect(second.calls[1]?.at).toBe('unparseable');
  });

  it.each([
    [' DEAD_LETTER ', 'dead_letter', 'failed'],
    [' Cancelled ', 'cancelled', 'failed'],
    [' weird ', 'weird', 'neutral'],
  ] as const)('normalizes status %s to %s with %s tone', (raw, status, tone) => {
    const node: LineageNode = { id: 'run', type: 'run', parent: null, state: raw };
    expect(buildRunView(node, [])).toMatchObject({ status, tone });
  });

  it('selects the newest progress plan by timestamp in either event order', () => {
    // The recording has one progress snapshot; an earlier one exposes order dependence.
    const earlier: Event = {
      ...runEvents.find((event) => event.title === 'run-progress')!,
      event_id: 'earlier-progress',
      at: '2026-09-29T02:59:07Z',
      body: JSON.stringify({ plan: [{ step: 'old step', status: 'pending' }] }),
    };
    const events = [...runEvents.filter((event) => event.title !== 'run-finished'), earlier];
    const expected = buildRunView(runNode, runEvents).plan;
    expect(buildRunView(runNode, events).plan).toEqual(expected);
    expect(buildRunView(runNode, [...events].reverse()).plan).toEqual(expected);
    // A final plan is authoritative even when a progress snapshot has a later timestamp.
    const finished = {
      ...runEvents.find((e) => e.title === 'run-finished')!,
      body: JSON.stringify({ plan: [{ step: 'final step', status: 'completed' }] }),
    };
    expect(buildRunView(runNode, [...events, finished]).plan).toEqual([
      { step: 'final step', status: 'completed' },
    ]);
  });

  it('keeps a valid plan when later plans contain no valid steps', () => {
    // Malformed later snapshots are absent from the recording.
    const progress = runEvents.find((event) => event.title === 'run-progress')!;
    const invalid = {
      ...progress,
      event_id: 'invalid-progress',
      at: '2026-09-29T02:59:09Z',
      body: JSON.stringify({ plan: [{ step: 'bad', status: 'unknown' }] }),
    };
    const finished = {
      ...runEvents.find((event) => event.title === 'run-finished')!,
      body: JSON.stringify({ plan: [{ step: 'bad', status: 'unknown' }] }),
    };
    expect(buildRunView(undefined, [progress, invalid, finished]).plan).toEqual([
      { step: 'read the target page', status: 'completed' },
      { step: 'draft the fold for review', status: 'in_progress' },
    ]);
  });

  it('uses the first run event with a non-null run id', () => {
    // The recording has no run event with a null run id.
    const first = { ...runEvents[0]!, run_id: null };
    expect(buildRunView(undefined, [first, runEvents[1]!]).runId).toBe(runEvents[1]?.run_id);
  });

  it.each([
    [' ok ', ' ok '],
    ['', 'unknown'],
    ['   ', '   '],
  ])('preserves non-empty outcome %j as %j', (outcome, expected) => {
    // Empty outcomes are absent from the recording.
    const attempt = runEvents.find((event) => event.title === 'run-attempt')!;
    const body = { ...(JSON.parse(attempt.body!) as Record<string, unknown>), outcome };
    expect(
      buildRunView(undefined, [{ ...attempt, body: JSON.stringify(body) }]).attempts[0]?.outcome,
    ).toBe(expected);
  });

  it('uses the final row tool-call count when the node has none', () => {
    const node = { ...runNode! };
    delete node.tool_calls;
    expect(buildRunView(node, runEvents).toolCallCount).toBe(4);
  });
});

describe('a stale page arriving late', () => {
  it('ends paging when the trailing page is empty', () => {
    // The recorded pages both contain calls, so this terminal response is hand-written.
    const first = mergeToolCallPage(buildRunView(undefined, []), page1);
    expect(
      mergeToolCallPage(first, { ...page1, calls: [], next_after: null }).nextAfter,
    ).toBeNull();
  });
  it('does not bring back "load more" once the last page has been read', () => {
    // A live refresh can re-merge page 1 after page 2. Its `next_after` is stale: taking
    // it would put a "Load more" button back on a run whose calls are all on screen.
    const view = buildRunView(undefined, []);
    const done = mergeToolCallPage(mergeToolCallPage(view, page1), page2);
    const again = mergeToolCallPage(done, page1);
    expect(again.nextAfter).toBeNull();
    expect(again.calls.map((c) => c.seq)).toEqual([1, 2, 3, 4]);
  });
});
