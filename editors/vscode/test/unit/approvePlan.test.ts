import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { EscurelClient, ListLineageResponse, EventsPage } from '../../src/client';
import { approvePlanRun, evolveApprovalRevision, existingEvolveApproval, resolveApprovalSubject } from '../../src/start/approvePlan';
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

  it("captures the approval for the run's own skill and page once confirmed", async () => {
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

describe('Evolve plan revision binding', () => {
  const hash = 'a'.repeat(64);
  const pageId = 'markdown/instances/evolve_problem/reorder.md';
  const runId = '01M3SXRWFGX05T89EEJ038ZWHB';
  const rootId = '01M3SXRWDP8R2QZME546B380MV';
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

  it('keeps the run-derived target and Evolve revision review in the merged approval path', async () => {
    const lineage = structuredClone(LINEAGE);
    const root = lineage.nodes.find((node) => node.id === rootId)!;
    root.label_skill = 'evolve_run';
    const run = lineage.nodes.find((node) => node.id === runId)!;
    run.target_page_id = pageId;
    run.harness = 'gemini';
    const events = structuredClone(EVENTS);
    for (const event of events.events) {
      if (event.title === 'run-finished') {
        const body = JSON.parse(event.body ?? '{}') as Record<string, unknown>;
        event.body = JSON.stringify({ ...body, status: 'planned' });
      }
    }
    const captured: unknown[] = [];
    const client = {
      listEvents: async (req: { event_id?: string }) => req.event_id === rootId
        ? { events: [{ event_id: rootId, kind: 'user', label_skill: 'evolve_run',
          instance_page_id: pageId, revision_binding_attested: true,
          provenance: { manual: { mode: 'plan', target_page_sha256: hash } } }] }
        : req.event_id ? { events: [] } : events,
      listLineage: async () => lineage,
      getRunToolCalls: async () => ({ calls: [], next_after: null }),
      expand: async () => ({ page: { page_id: pageId, skill: 'evolve_problem', page_kind: 'instance' },
        content: '# Reviewed problem', content_sha256: hash,
        frontmatter: { search_request: { pilot: 'p1_decision', holdout_id: 'sealed-1' } },
        body: '# Reviewed problem', blocks: [], wikilinks_out: [] }),
      captureEvent: async (request: unknown) => { captured.push(request); return { event_id: 'approved-1' }; },
    } as unknown as EscurelClient;
    const asked: Array<{ message: string; action?: string }> = [];
    const result = await approvePlanRun(client, runId, {
      harness: 'local-default-changed',
      confirm: async (message, action) => { asked.push({ message, action }); return true; },
    });
    expect(result).toEqual({ kind: 'approved', eventId: 'approved-1' });
    expect(asked).toHaveLength(1);
    expect(asked[0]).toMatchObject({ action: 'Approve search' });
    expect(asked[0]!.message).toContain('holdout ID: sealed-1');
    expect(asked[0]!.message).toContain(`Page SHA-256: ${hash}`);
    expect(captured).toHaveLength(1);
    expect(captured[0]).toMatchObject({ label_skill: 'evolve_run', instance_page_id: pageId,
      provenance: { manual: { harness: 'gemini', expected_page_sha256: hash } } });

    run.harness = 'echo';
    captured.length = 0;
    await expect(approvePlanRun(client, runId, {
      harness: 'gemini', confirm: async () => true,
    })).rejects.toThrow(/cannot authorize Evolve search/);
    expect(captured).toHaveLength(0);
  });
});
