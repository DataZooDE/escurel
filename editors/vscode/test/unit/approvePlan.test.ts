import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { EscurelClient, ListLineageResponse, EventsPage } from '../../src/client';
import { approvePlanRun, resolveApprovalSubject } from '../../src/start/approvePlan';
import { inFlight } from '../../src/runs/controlWait';

const FIXTURES = join(__dirname, 'fixtures', 'lineage');
const LINEAGE: ListLineageResponse = JSON.parse(
  readFileSync(join(FIXTURES, 'run-detail-lineage.json'), 'utf8'),
);
const EVENTS: EventsPage = JSON.parse(
  readFileSync(join(FIXTURES, 'run-detail-events.json'), 'utf8'),
);

function makeClient(lineage: ListLineageResponse, events: EventsPage): EscurelClient {
  return {
    listLineage: async () => lineage,
    listEvents: async () => events,
    getRunToolCalls: async () => ({ calls: [], next_after: null }),
  } as unknown as EscurelClient;
}

describe('resolveApprovalSubject', () => {
  const runId = '01M3SXRWFGX05T89EEJ038ZWHB';

  it('derives pageId and skill from the run, never from the caller', async () => {
    const client = makeClient(LINEAGE, EVENTS);
    const res = await resolveApprovalSubject(client, runId);
    expect(res.pageId).toBe('markdown/instances/order/o1.md');
    expect(res.skill).toBe('signal');
  });

  it('refuses to guess when target page cannot be resolved', async () => {
    // Lineage without target_page_id
    const strippedLineage: ListLineageResponse = {
      root_event_id: '01EVT',
      nodes: [
        {
          id: '01EVT',
          type: 'event',
          state: 'inbox',
          label_skill: 'my-skill',
          parent: null,
        },
        {
          id: '01RUN_NO_PAGE',
          type: 'run',
          state: 'planned',
          parent: '01EVT',
        },
      ],
    };
    const client = makeClient(strippedLineage, { events: [] });

    await expect(resolveApprovalSubject(client, '01RUN_NO_PAGE')).rejects.toThrow(
      /missing target page/,
    );
  });

  it('refuses to guess when skill cannot be resolved', async () => {
    // Lineage without root event label_skill
    const strippedLineage: ListLineageResponse = {
      root_event_id: '01M3SXRWDP8R2QZME546B380MV',
      nodes: [
        {
          id: '01M3SXRWFGX05T89EEJ038ZWHB',
          type: 'run',
          state: 'planned',
          target_page_id: 'markdown/instances/order/o1.md',
          parent: '01M3SXRWDP8R2QZME546B380MV',
        },
      ],
    };
    const client = makeClient(strippedLineage, EVENTS);

    await expect(resolveApprovalSubject(client, '01M3SXRWFGX05T89EEJ038ZWHB')).rejects.toThrow(
      /missing skill/,
    );
  });
});


describe('approvePlanRun', () => {
  const runId = '01M3SXRWFGX05T89EEJ038ZWHB';
  const withStatus = (status: string): EventsPage => ({
    ...EVENTS,
    events: EVENTS.events.map((e) => {
      if (e.title !== 'run-finished') return e;
      const body = JSON.parse(e.body ?? '{}') as Record<string, unknown>;
      return { ...e, body: JSON.stringify({ ...body, status }) };
    }),
  });
  const captures: unknown[] = [];
  const clientFor = (events: EventsPage) =>
    ({
      listLineage: async () => LINEAGE,
      listEvents: async () => events,
      getRunToolCalls: async () => ({ calls: [], next_after: null }),
      captureEvent: async (req: unknown) => {
        captures.push(req);
        return { event_id: 'EV1' };
      },
    }) as unknown as EscurelClient;

  it('asks first — naming skill, page and run — and captures nothing when declined', async () => {
    captures.length = 0;
    const asked: string[] = [];
    const out = await approvePlanRun(clientFor(withStatus('planned')), runId, {
      confirm: async (m) => (asked.push(m), false),
      harness: '',
    });
    expect(out).toEqual({ kind: 'declined' });
    expect(captures).toEqual([]);
    expect(asked).toHaveLength(1);
    expect(asked[0]).toContain('signal');
    expect(asked[0]).toContain('markdown/instances/order/o1.md');
    expect(asked[0]).toContain(runId);
  });

  it('captures the approval for the run\'s own skill and page once confirmed', async () => {
    captures.length = 0;
    const out = await approvePlanRun(clientFor(withStatus('planned')), runId, {
      confirm: async () => true,
      harness: '',
    });
    expect(out).toEqual({ kind: 'approved', eventId: 'EV1' });
    expect(captures).toHaveLength(1);
    expect(JSON.stringify(captures[0])).toContain(runId);
  });

  it('refuses a run that is no longer planned, without asking or capturing', async () => {
    captures.length = 0;
    let asked = 0;
    const out = await approvePlanRun(clientFor(withStatus('processed')), runId, {
      confirm: async () => (asked++, true),
      harness: '',
    });
    expect(out).toEqual({ kind: 'not-planned', status: 'processed' });
    expect(asked).toBe(0);
    expect(captures).toEqual([]);
  });
});

describe('inFlight on approvals', () => {
  it('a double invocation for one run runs the task once', async () => {
    const once = inFlight();
    let runs = 0;
    const task = async () => {
      runs++;
      await new Promise((r) => setTimeout(r, 10));
    };
    await Promise.all([once('RUN', task), once('RUN', task)]);
    expect(runs).toBe(1);
  });
});
