import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { EscurelClient, ListLineageResponse, EventsPage } from '../../src/client';
import { resolveApprovalSubject } from '../../src/start/approvePlan';

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
  it('returns given skill and pageId directly when both are present', async () => {
    const client = makeClient(LINEAGE, EVENTS);
    const res = await resolveApprovalSubject(client, '01RUN', {
      skill: 'custom-skill',
      pageId: 'markdown/instances/order/o1.md',
    });
    expect(res).toEqual({
      skill: 'custom-skill',
      pageId: 'markdown/instances/order/o1.md',
    });
  });

  it('resolves pageId and skill from run-detail fixtures when both are omitted', async () => {
    const client = makeClient(LINEAGE, EVENTS);
    const runId = '01M3SXRWFGX05T89EEJ038ZWHB';

    const res = await resolveApprovalSubject(client, runId);
    // In run-detail-lineage.json:
    // target_page_id is "markdown/instances/order/o1.md"
    // root event "01M3SXRWDP8R2QZME546B380MV" has label_skill: "signal"
    expect(res.pageId).toBe('markdown/instances/order/o1.md');
    expect(res.skill).toBe('signal');
  });

  it('resolves skill from lineage root event when only pageId is provided', async () => {
    const client = makeClient(LINEAGE, EVENTS);
    const runId = '01M3SXRWFGX05T89EEJ038ZWHB';

    const res = await resolveApprovalSubject(client, runId, {
      pageId: 'markdown/instances/override.md',
    });
    expect(res.pageId).toBe('markdown/instances/override.md');
    expect(res.skill).toBe('signal');
  });

  it('resolves pageId from run detail when only skill is provided', async () => {
    const client = makeClient(LINEAGE, EVENTS);
    const runId = '01M3SXRWFGX05T89EEJ038ZWHB';

    const res = await resolveApprovalSubject(client, runId, {
      skill: 'provided-skill',
    });
    expect(res.pageId).toBe('markdown/instances/order/o1.md');
    expect(res.skill).toBe('provided-skill');
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
