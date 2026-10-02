import { describe, expect, it } from 'vitest';
import type { RunView } from '../../src/shared/protocol';
import { runControls } from '../../src/runs/controls';
import { resolveRunAction, visibleRunControls } from '../../src/runs/runActions';
import { loadRun } from '../../src/runs/loadRun';
import type { EscurelClient } from '../../src/client';
import events from './fixtures/lineage/run-detail-events.json';
import lineage from './fixtures/lineage/run-detail-lineage.json';
import calls from './fixtures/lineage/run-detail-tool-calls-page1.json';
import deadLetterStart from './fixtures/runner/run-started-deadletter.json';

const started = events.events.find((event) => event.title === 'run-started')!;
const triggerEventId = (started.provenance.runner as { event_id: string }).event_id;
const skill = lineage.nodes.find(
  (node) => node.type === 'event' && node.parent === null,
)!.label_skill;
const base = {
  runId: started.run_id,
  status: 'running',
  tone: 'run',
  targetPageId: started.instance_page_id,
  triggerEventId,
  skill,
  attempts: [],
  plan: [],
  calls: [],
  nextAfter: null,
} as RunView & { triggerEventId: string };
const view = (status: string, admin: 'admin' | 'not-admin' = 'admin') => ({
  ...base,
  status,
  controls: visibleRunControls({ ...base, status, controls: runControls(status, admin) }),
});

describe('run detail actions', () => {
  it('loads the root skill and trigger event from recorded run data', async () => {
    const client = {
      listEvents: async () => events,
      listLineage: async () => lineage,
      getRunToolCalls: async () => calls,
    } as unknown as EscurelClient;
    const loaded = await loadRun(client, base.runId);
    expect(loaded.view.skill).toBe(skill);
    expect((loaded.view as typeof base).triggerEventId).toBe(triggerEventId);
  });

  it('uses the recorded dead-letter trigger rather than the run-started row id', async () => {
    const client = {
      listEvents: async () => ({ events: [deadLetterStart] }),
      listLineage: async () => ({ nodes: [] }),
      getRunToolCalls: async () => calls,
    } as unknown as EscurelClient;
    const loaded = await loadRun(client, deadLetterStart.run_id);
    expect((loaded.view as typeof base).triggerEventId).toBe(
      deadLetterStart.provenance.runner.event_id,
    );
    expect((loaded.view as typeof base).triggerEventId).not.toBe(deadLetterStart.event_id);
  });

  it('keeps a terminal-started run readable when its root lineage is unavailable', async () => {
    const client = {
      listEvents: async () => ({
        events: events.events.map((event) => ({ ...event, root_event_id: null })),
      }),
      listLineage: async () => {
        throw new Error('lineage unavailable');
      },
      getRunToolCalls: async () => calls,
    } as unknown as EscurelClient;
    const loaded = await loadRun(client, base.runId);
    expect(loaded.view.skill).toBeUndefined();
    expect(
      visibleRunControls({
        ...loaded.view,
        status: 'planned',
        controls: runControls('planned', 'admin'),
      }),
    ).toEqual([]);
    const unreadable = {
      ...client,
      listEvents: async () => events,
    } as unknown as EscurelClient;
    expect((await loadRun(unreadable, base.runId)).view.skill).toBeUndefined();
  });

  it('shows only actionable controls and deactivates Requeue for a known human', () => {
    expect(view('running').controls.map((c) => c.action)).toEqual(['cancel']);
    expect(view('planned').controls.map((c) => c.action)).toEqual(['approve']);
    expect(view('failed').controls.map((c) => c.action)).toEqual(['retry', 'fix-skill']);
    expect(view('cancelled').controls.map((c) => c.action)).toEqual(['retry']);
    const human = view('dead_letter', 'not-admin');
    expect(human.controls.map((c) => c.action)).toEqual(['retry', 'requeue', 'fix-skill']);
    expect(human.controls[1]).toMatchObject({ enabled: false, disabledReason: expect.any(String) });
    expect(view('dead_letter').controls[1]?.enabled).toBe(true);
    expect(visibleRunControls({ ...view('planned'), skill: undefined })).toEqual([]);
    expect(
      visibleRunControls({ ...view('failed'), skill: undefined }).map((c) => c.action),
    ).toEqual(['retry']);
    expect(
      visibleRunControls({ ...view('dead_letter'), triggerEventId: undefined }).map(
        (c) => c.action,
      ),
    ).toEqual(['retry', 'fix-skill']);
  });

  it('resolves only controls offered by this panel using host identifiers', () => {
    expect(
      resolveRunAction(view('running'), {
        type: 'run-control',
        action: 'cancel',
        runId: base.runId,
      }),
    ).toEqual({ command: 'escurel.cancelRun', args: { runId: base.runId } });
    expect(
      resolveRunAction(view('cancelled'), {
        type: 'run-control',
        action: 'retry',
        runId: base.runId,
      }),
    ).toEqual({ command: 'escurel.retryRun', args: { runId: base.runId } });
    expect(
      resolveRunAction(view('dead_letter'), {
        type: 'run-control',
        action: 'requeue',
        runId: base.runId,
      }),
    ).toEqual({ command: 'escurel.requeue', args: { eventId: triggerEventId } });
    expect(
      resolveRunAction(view('planned'), {
        type: 'run-control',
        action: 'approve',
        runId: base.runId,
      }),
    ).toEqual({
      command: 'escurel.approvePlan',
      args: { runId: base.runId, skill, pageId: base.targetPageId },
    });
    expect(
      resolveRunAction(view('failed'), {
        type: 'run-control',
        action: 'fix-skill',
        runId: base.runId,
      }),
    ).toEqual({ command: 'escurel.viewSkill', args: skill });
    expect(resolveRunAction(view('failed'), { type: 'view-skill', skill })).toEqual({
      command: 'escurel.viewSkill',
      args: skill,
    });
  });

  it('rejects forged run ids, unavailable or disabled actions, event ids and skills', () => {
    const forged = [
      [view('running'), { type: 'run-control', action: 'cancel', runId: 'other' }],
      [view('running'), { type: 'run-control', action: 'retry', runId: base.runId }],
      [
        view('dead_letter', 'not-admin'),
        { type: 'run-control', action: 'requeue', runId: base.runId },
      ],
      [
        view('dead_letter'),
        { type: 'run-control', action: 'requeue', runId: base.runId, eventId: 'other' },
      ],
      [
        view('planned'),
        { type: 'run-control', action: 'approve', runId: base.runId, skill: 'other' },
      ],
      [view('failed'), { type: 'view-skill', skill: 'other' }],
      [view('failed'), { type: 'view-skill', skill, runId: 'other' }],
    ] as const;
    for (const [run, message] of forged) expect(resolveRunAction(run, message)).toBeUndefined();
  });
});
