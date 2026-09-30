import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import type {
  EscurelClient,
  EventsPage,
  GetRunToolCallsResponse,
  ListLineageRequest,
  ListLineageResponse,
} from '../../src/client';
import { carryCalls, loadRun } from '../../src/runs/loadRun';
import { mergeToolCallPage } from '../../src/runs/runModel';
import { loadThread } from '../../src/thread/loadThread';
import { foldLineage } from '../../src/thread/threadModel';

const dir = join(__dirname, 'fixtures', 'lineage');
const recorded = <T>(name: string): T => JSON.parse(readFileSync(join(dir, name), 'utf8')) as T;

// The recorded nine-page read: each page's `next_cursor` is the real cursor the gateway
// issued, so a fake that answers by cursor replays the gateway's own paging.
const pages = [1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) =>
  recorded<ListLineageResponse>(`lineage-paged-page${n}.json`),
);
const full = recorded<ListLineageResponse>('lineage-paged-full.json');

function pagedClient(log: ListLineageRequest[]): EscurelClient {
  return {
    listLineage: async (req: ListLineageRequest) => {
      log.push(req);
      const at = req.cursor ? pages.findIndex((p) => p.next_cursor === req.cursor) + 1 : 0;
      return pages[at]!;
    },
  } as unknown as EscurelClient;
}

describe('loadThread', () => {
  it('follows next_cursor to the last page and folds them into the unpaged read', async () => {
    const log: ListLineageRequest[] = [];
    const folded = await loadThread(pagedClient(log), full.root_event_id);
    const truth = foldLineage([full]);
    expect(log).toHaveLength(9);
    expect([...folded.nodes.keys()].sort()).toEqual([...truth.nodes.keys()].sort());
    for (const [id, node] of truth.nodes) {
      expect(folded.nodes.get(id)?.parent).toBe(node.parent);
      expect(folded.nodes.get(id)?.state).toBe(node.state);
    }
  });

  it('asks for every node type and the tool-call summary, once per page', async () => {
    const log: ListLineageRequest[] = [];
    await loadThread(pagedClient(log), full.root_event_id);
    for (const req of log) {
      expect(req.root_event_id).toBe(full.root_event_id);
      expect(req.include).toEqual(['events', 'runs', 'drafts', 'tool_calls']);
    }
  });

  it('stops at a page cap instead of following a cursor that never ends', async () => {
    // A gateway that keeps answering `next_cursor` must not hang the panel. The cap is
    // generous (a thread of thousands of events) and reported, not silent.
    let calls = 0;
    const endless = {
      listLineage: async () => {
        calls += 1;
        return { root_event_id: 'r', nodes: [], next_cursor: `c${calls}` };
      },
    } as unknown as EscurelClient;
    const folded = await loadThread(endless, 'r', { maxPages: 5 });
    expect(calls).toBe(5);
    expect(folded.truncated).toBe(true);
  });
});

// `run-detail-*`: one run recorded whole, so the run id, the root and the tool calls agree.
// The older `run-events.json` and `lineage-cascade.json` are two different recordings.
describe('loadRun', () => {
  const runEvents = recorded<EventsPage>('run-detail-events.json');
  const lineage = recorded<ListLineageResponse>('run-detail-lineage.json');
  const page1 = recorded<GetRunToolCallsResponse>('run-detail-tool-calls-page1.json');
  const run = lineage.nodes.find((n) => n.type === 'run')!;

  const client = {
    listEvents: async () => runEvents,
    listLineage: async () => lineage,
    getRunToolCalls: async () => page1,
  } as unknown as EscurelClient;

  it('joins the run node, its own events and the first page of calls', async () => {
    const loaded = await loadRun(client, run.id);
    expect(loaded.view.runId).toBe(run.id);
    expect(loaded.view.status).toBe('processed');
    expect(loaded.view.calls.map((c) => c.seq)).toEqual([1, 2]);
    expect(loaded.view.nextAfter).toBe(page1.next_after);
    // The thread this run belongs to, so run detail can link back to it.
    expect(loaded.rootEventId).toBe(runEvents.events.find((e) => e.root_event_id)?.root_event_id);
  });

  it('still shows a run whose lineage node cannot be read', async () => {
    // Denial is absence: the lineage may omit the node while its own events are readable.
    const blind = { ...client, listLineage: async () => ({ root_event_id: 'x', nodes: [] }) };
    const loaded = await loadRun(blind as unknown as EscurelClient, run.id);
    expect(loaded.view.runId).toBe(run.id);
    expect(loaded.view.attempts).toHaveLength(1);
  });
});

describe('carryCalls', () => {
  const pageTwo = recorded<GetRunToolCallsResponse>('run-detail-tool-calls-page2.json');
  const fresh = async () =>
    (
      await loadRun(
        {
          listEvents: async () => recorded<EventsPage>('run-detail-events.json'),
          listLineage: async () => recorded<ListLineageResponse>('run-detail-lineage.json'),
          getRunToolCalls: async () =>
            recorded<GetRunToolCallsResponse>('run-detail-tool-calls-page1.json'),
        } as unknown as EscurelClient,
        recorded<ListLineageResponse>('run-detail-lineage.json').nodes.find(
          (n) => n.type === 'run',
        )!.id,
      )
    ).view;

  it('keeps the pages the user already loaded when a live refetch rereads page one', async () => {
    // A live update rereads the first page. Without this the call list snaps back to its
    // first rows while the user is reading further down it.
    const loadedMore = mergeToolCallPage(await fresh(), pageTwo);
    expect(loadedMore.calls).toHaveLength(4);
    const carried = carryCalls(await fresh(), loadedMore);
    expect(carried.calls.map((c) => c.seq)).toEqual([1, 2, 3, 4]);
    expect(carried.nextAfter).toBe(loadedMore.nextAfter);
  });

  it('changes nothing when there was no previous view or it had read no further', async () => {
    const next = await fresh();
    expect(carryCalls(next, undefined)).toBe(next);
    expect(carryCalls(next, await fresh()).calls).toEqual(next.calls);
  });
});
