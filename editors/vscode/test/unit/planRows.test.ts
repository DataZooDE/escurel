import { describe, expect, it } from 'vitest';
import type { Event } from '../../src/client/types';
import { planRows } from '../../src/views/planRows';

function ev(over: Partial<Event>): Event {
  return {
    event_id: 'e',
    at: '2026-10-04T10:00:00Z',
    source: 'runner',
    mime: 'application/json',
    label_skill: 'escurel:run',
    instance_page_id: null,
    status: 'processed',
    title: null,
    body: null,
    provenance: null,
    kind: 'system',
    root_event_id: null,
    run_id: null,
    ...over,
  };
}

const planFinished = (runId: string, over: Partial<Event> = {}): Event =>
  ev({
    event_id: `run:${runId}:finished`,
    title: 'run-finished',
    run_id: runId,
    root_event_id: `root-${runId}`,
    instance_page_id: 'markdown/instances/customer-order/order-4500131.md',
    body: JSON.stringify({ status: 'planned' }),
    at: '2026-10-04T10:05:00Z',
    ...over,
  });

const started = (runId: string, trigger: string): Event =>
  ev({
    event_id: `run:${runId}:started`,
    title: 'run-started',
    run_id: runId,
    provenance: { runner: { event_id: trigger } },
  });

const trigger = (id: string, skill: string): Event =>
  ev({
    event_id: id,
    label_skill: skill,
    kind: 'user',
    status: 'inbox',
    instance_page_id: 'markdown/instances/customer-order/order-4500131.md',
    provenance: { manual: { mode: 'plan' } },
  });

// A plan run ends 'planned' and waits for a person to approve it. The only trace of that wait used to
// be a toast; if it was dismissed the plan was lost. The Awaiting queue now lists every planned run
// nobody has approved yet.
describe('planRows', () => {
  it('lists a planned run that nobody approved, named by skill and page', () => {
    const rows = planRows(
      [planFinished('R1'), started('R1', 'T1')],
      [trigger('T1', 'supplier-risk')],
    );
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      kind: 'plan',
      runId: 'R1',
      skill: 'supplier-risk',
      pageId: 'markdown/instances/customer-order/order-4500131.md',
      label: 'Plan ready · supplier-risk on order-4500131',
      description: 'Approve plan',
    });
  });

  it('drops a plan once an event approves it', () => {
    const approval = ev({
      event_id: 'A1',
      label_skill: 'supplier-risk',
      kind: 'user',
      status: 'inbox',
      provenance: { manual: { mode: 'run', approved_plan_run_id: 'R1' } },
    });
    expect(
      planRows(
        [planFinished('R1'), started('R1', 'T1')],
        [trigger('T1', 'supplier-risk'), approval],
      ),
    ).toEqual([]);
  });

  it('ignores runs that finished any other way, and runs without an id', () => {
    const processed = planFinished('R2', { body: JSON.stringify({ status: 'processed' }) });
    const noRun = planFinished('R3', { run_id: null });
    expect(planRows([processed, noRun], [])).toEqual([]);
  });

  it('still lists the plan when its trigger event is unknown (skill falls back to the page)', () => {
    const rows = planRows([planFinished('R4')], []);
    expect(rows).toHaveLength(1);
    expect(rows[0]!.skill).toBeUndefined();
    expect(rows[0]!.label).toBe('Plan ready · order-4500131');
  });

  it('is newest first and lists each run once', () => {
    const rows = planRows(
      [
        planFinished('R5', { at: '2026-10-04T09:00:00Z' }),
        planFinished('R6', { at: '2026-10-04T11:00:00Z' }),
        planFinished('R6', { at: '2026-10-04T11:00:00Z', event_id: 'dup' }),
      ],
      [],
    );
    expect(rows.map((r) => r.runId)).toEqual(['R6', 'R5']);
  });

  it('survives a body that is not JSON', () => {
    expect(planRows([planFinished('R7', { body: 'not json' })], [])).toEqual([]);
  });
});
