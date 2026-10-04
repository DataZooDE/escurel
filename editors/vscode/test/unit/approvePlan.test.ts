import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { EscurelClient, ListLineageResponse, EventsPage } from '../../src/client';
import { evolveApprovalRevision, existingEvolveApproval, resolveApprovalSubject } from '../../src/start/approvePlan';

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

describe('resolveApprovalSubject for a plan with no target', () => {
  it('keeps an explicit empty page: the plan was started without one, it is not missing', async () => {
    const client = { listLineage: async () => ({ nodes: [] }) } as never;
    const res = await resolveApprovalSubject(client, '01RUN', {
      skill: 'supplier-risk',
      pageId: '',
    });
    expect(res).toEqual({ skill: 'supplier-risk', pageId: '' });
  });
});

describe('Evolve plan revision binding', () => {
  const hash = 'a'.repeat(64);
  const pageId = 'markdown/instances/evolve_problem/reorder.md';
  function client(currentHash: string, mode = 'plan'): EscurelClient {
    return {
      listEvents: async () => ({ events: [{
        kind: 'user', label_skill: 'evolve_run', instance_page_id: pageId,
        revision_binding_attested: true,
        provenance: { manual: { mode, target_page_sha256: hash } },
      }] }),
      expand: async () => ({
        page: { page_id: pageId, skill: 'evolve_problem', page_type: 'instance' },
        content: '# Reviewed problem', content_sha256: currentHash,
        frontmatter: {}, body: '# Reviewed problem', blocks: [], wikilinks_out: [],
      }),
    } as unknown as EscurelClient;
  }

  it('approves only the page revision frozen by the plan event', async () => {
    await expect(evolveApprovalRevision(client(hash), 'ROOT', pageId)).resolves.toBe(hash);
    await expect(evolveApprovalRevision(client('b'.repeat(64)), 'ROOT', pageId))
      .rejects.toThrow(/changed after planning/);
  });

  it('rejects an execution event masquerading as a plan', async () => {
    await expect(evolveApprovalRevision(client(hash, 'run'), 'ROOT', pageId))
      .rejects.toThrow(/not bound/);
  });

  it('rejects a pre-upgrade plan with no server attestation', async () => {
    const old = client(hash);
    const original = old.listEvents.bind(old);
    old.listEvents = async (...args) => {
      const page = await original(...args);
      page.events[0]!.revision_binding_attested = false;
      return page;
    };
    await expect(evolveApprovalRevision(old, 'ROOT', pageId)).rejects.toThrow(/not bound/);
  });

  it('recovers the same approval event after a response is lost, without reading the edited page', async () => {
    const approved = {
      listEvents: async () => ({ events: [{
        event_id: 'evolve-approval-01RUN', kind: 'user', label_skill: 'evolve_run',
        instance_page_id: pageId, revision_binding_attested: true,
        provenance: { manual: { approved_plan_run_id: '01RUN' } },
      }] }),
      expand: async () => { throw new Error('must not read a newly edited page on retry'); },
    } as unknown as EscurelClient;
    await expect(existingEvolveApproval(approved, 'evolve-approval-01RUN', pageId, '01RUN'))
      .resolves.toBe('evolve-approval-01RUN');
    await expect(existingEvolveApproval(approved, 'evolve-approval-01RUN', pageId, 'DIFFERENT'))
      .resolves.toBeUndefined();
  });
});
