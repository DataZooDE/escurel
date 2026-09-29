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
});

describe('a stale page arriving late', () => {
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
