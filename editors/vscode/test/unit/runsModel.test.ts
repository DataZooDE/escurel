import { describe, expect, it } from 'vitest';
import type { Event } from '../../src/client';
import {
  applyFilter,
  describeRunner,
  foldRuns,
  groupRuns,
  insightLine,
  runDescription,
  runLabel,
  type RunRecord,
} from '../../src/views/runsModel';

const NOW = Date.parse('2026-10-04T12:00:00Z');
const iso = (secondsAgo: number) => new Date(NOW - secondsAgo * 1000).toISOString();

let n = 0;
function ev(
  runId: string,
  title: string,
  at: string,
  body: object = {},
  extra: Partial<Event> = {},
): Event {
  n += 1;
  return {
    event_id: `run:${runId}:${title}:${n}`,
    at,
    source: 'escurel-runner',
    mime: 'application/json',
    label_skill: 'escurel:run',
    instance_page_id: 'markdown/instances/customer-order__order-4500123.md',
    status: 'processed',
    title,
    body: JSON.stringify(body),
    provenance: { runner: { event_id: `trigger-${runId}`, harness: 'echo' } },
    kind: 'system',
    root_event_id: `root-${runId}`,
    run_id: runId,
    ...extra,
  } as Event;
}
const started = (id: string, ago: number) => ev(id, 'run-started', iso(ago));
const finished = (id: string, ago: number, status: string, more: object = {}) =>
  ev(id, 'run-finished', iso(ago), { status, attempts: 1, ...more });

const SKILLS = new Map([
  ['trigger-a', 'supplier-risk'],
  ['trigger-b', 'supplier-risk'],
]);

describe('foldRuns', () => {
  it('turns the lifecycle events of one run into one record', () => {
    const [r] = foldRuns(
      [started('a', 20), finished('a', 14, 'processed', { summary: 'drafted', tool_calls: 4 })],
      {
        nowMs: NOW,
        skillByEvent: SKILLS,
      },
    );
    expect(r).toMatchObject({
      runId: 'a',
      state: 'succeeded',
      skill: 'supplier-risk',
      targetPageId: 'markdown/instances/customer-order__order-4500123.md',
      rootEventId: 'root-a',
      triggerEventId: 'trigger-a',
      harness: 'echo',
      toolCalls: 4,
      summary: 'drafted',
    });
    expect(r!.durationMs).toBe(6000);
  });

  it('an unfinished run is running while the runner says it is live, and unknown when it does not', () => {
    const e = [started('a', 30)];
    expect(foldRuns(e, { nowMs: NOW, liveRunIds: new Set(['a']) })[0]!.state).toBe('running');
    // The status says nothing is live: a "running" row that is not running is the stale-row bug.
    expect(foldRuns(e, { nowMs: NOW, liveRunIds: new Set() })[0]!.state).toBe('unknown');
    // With no status at all we cannot tell: believe the start.
    expect(foldRuns(e, { nowMs: NOW })[0]!.state).toBe('running');
  });

  it('reads each terminal status, with the reason and the error', () => {
    const rs = foldRuns(
      [
        finished('p', 50, 'planned'),
        finished('f', 40, 'failed', { reason: 'harness error', error: 'boom' }),
        finished('d', 30, 'dead_letter', { reason: 'permanent' }),
        finished('c', 20, 'cancelled', { reason: 'cancelled by alice' }),
      ],
      { nowMs: NOW },
    );
    const by = Object.fromEntries(rs.map((r) => [r.runId, r]));
    expect(by.p!.state).toBe('planned');
    expect(by.f).toMatchObject({ state: 'failed', reason: 'harness error', error: 'boom' });
    expect(by.d!.state).toBe('dead_letter');
    expect(by.c!.state).toBe('cancelled');
  });

  it('sorts newest first and ignores events that are not run lifecycle', () => {
    const noise = ev('x', 'run-progress', iso(5), { plan: [] });
    const rs = foldRuns(
      [started('a', 100), finished('a', 90, 'processed'), started('b', 40), noise],
      {
        nowMs: NOW,
        liveRunIds: new Set(['b']),
      },
    );
    expect(rs.map((r) => r.runId)).toEqual(['b', 'a']);
  });

  it('measures a run from its attempts when they are present (the run events are whole seconds)', () => {
    const attempt = ev('a', 'run-attempt', iso(14), {
      attempt: 1,
      started_at: '2026-10-04 11:59:40.100000',
      ended_at: '2026-10-04 11:59:46.600000',
      outcome: 'ok',
    });
    const [r] = foldRuns([started('a', 20), attempt, finished('a', 14, 'processed')], {
      nowMs: NOW,
    });
    expect(r!.durationMs).toBe(6500);
  });
});

describe('groupRuns', () => {
  const rs = (): RunRecord[] =>
    foldRuns(
      [
        started('run1', 30),
        started('ok1', 300),
        finished('ok1', 290, 'processed'),
        finished('plan1', 200, 'planned'),
        finished('bad1', 150, 'dead_letter', { reason: 'permanent' }),
        started('old1', 4000),
        finished('old1', 3990, 'processed'),
      ],
      { nowMs: NOW, liveRunIds: new Set(['run1']) },
    );

  it('puts every run in one section: running, waiting, needs attention, history', () => {
    const g = groupRuns(rs(), NOW);
    expect(g.running.map((r) => r.runId)).toEqual(['run1']);
    expect(g.waiting.map((r) => r.runId)).toEqual(['plan1']);
    expect(g.attention.map((r) => r.runId)).toEqual(['bad1']);
    // History is everything that ENDED, newest first (a plan that is only waiting has not ended).
    expect(g.history.map((r) => r.runId)).toEqual(['bad1', 'ok1', 'old1']);
  });

  it('a failure that was retried and then succeeded no longer needs attention', () => {
    const records = foldRuns(
      [
        finished('first', 300, 'dead_letter', { reason: 'permanent' }),
        finished('second', 100, 'processed'),
      ].map((e) => ({
        ...e,
        // Both runs were started for the SAME trigger event: the second one is the retry.
        provenance: { runner: { event_id: 'same-trigger', harness: 'echo' } },
      })) as Event[],
      { nowMs: NOW },
    );
    const g = groupRuns(records, NOW);
    expect(g.attention).toEqual([]);
    expect(g.history.map((r) => r.runId)).toEqual(['second', 'first']);
  });

  it('a plan that was approved no longer waits', () => {
    const records = foldRuns(
      [finished('plan', 300, 'planned'), finished('real', 100, 'processed')].map((e) => ({
        ...e,
        provenance: { runner: { event_id: 'same-trigger', harness: 'echo' } },
      })) as Event[],
      { nowMs: NOW },
    );
    expect(groupRuns(records, NOW).waiting).toEqual([]);
  });
});

describe('applyFilter', () => {
  const records = foldRuns(
    [
      started('a', 100),
      finished('a', 94, 'processed'),
      started('b', 90),
      finished('b', 80, 'failed', { reason: 'x' }),
      started('c', 70),
      finished('c', 60, 'cancelled'),
    ],
    {
      nowMs: NOW,
      skillByEvent: new Map([
        ['trigger-a', 'supplier-risk'],
        ['trigger-b', 'customer-notice'],
      ]),
    },
  );

  it('filters by state, by skill and by text, and the three combine', () => {
    expect(applyFilter(records, { states: ['failed'] }).map((r) => r.runId)).toEqual(['b']);
    expect(applyFilter(records, { skill: 'supplier-risk' }).map((r) => r.runId)).toEqual(['a']);
    expect(applyFilter(records, { text: 'order-4500123' }).length).toBe(3);
    expect(applyFilter(records, { text: 'nothing like this' })).toEqual([]);
    expect(
      applyFilter(records, { states: ['succeeded', 'failed'], skill: 'customer-notice' }).map(
        (r) => r.runId,
      ),
    ).toEqual(['b']);
  });

  it('no filter keeps everything', () => {
    expect(applyFilter(records, {}).length).toBe(3);
  });
});

describe('insightLine', () => {
  it('counts what finished in the window: runs, ok, failed, average duration', () => {
    const records = foldRuns(
      [
        finished('a', 3600, 'processed'),
        finished('b', 1800, 'processed'),
        finished('c', 900, 'failed', { reason: 'x' }),
        finished('old', 3 * 86400, 'processed'),
      ],
      { nowMs: NOW },
    );
    // No attempts and no start events: no durations to average, so the line omits it.
    expect(insightLine(records, NOW)).toBe('Last 24 h: 3 runs · 2 ok · 1 failed');
  });

  it('includes the average when durations are known and hides itself when nothing finished', () => {
    const records = foldRuns(
      [
        started('a', 20),
        finished('a', 14, 'processed'),
        started('b', 12),
        finished('b', 2, 'processed'),
      ],
      { nowMs: NOW },
    );
    expect(insightLine(records, NOW)).toBe('Last 24 h: 2 runs · 2 ok · avg 8 s');
    expect(insightLine([], NOW)).toBeUndefined();
  });
});

describe('words for a row', () => {
  const r = foldRuns([started('a', 20), finished('a', 14, 'processed')], {
    nowMs: NOW,
    skillByEvent: SKILLS,
  })[0]!;

  it('names the skill and the page, never the id', () => {
    expect(runLabel(r)).toBe('supplier-risk · order-4500123');
    expect(runLabel({ ...r, skill: undefined })).toBe('order-4500123');
    expect(runLabel({ ...r, skill: undefined, targetPageId: null })).toBe('Run');
  });

  it('says what happened and how long ago', () => {
    expect(runDescription(r, NOW)).toBe('succeeded in 6 s · just now');
    expect(runDescription({ ...r, state: 'failed', reason: 'harness error' }, NOW)).toBe(
      'failed · harness error · just now',
    );
    const running = foldRuns([started('b', 12)], { nowMs: NOW, liveRunIds: new Set(['b']) })[0]!;
    expect(runDescription(running, NOW)).toBe('running for 12 s');
    expect(runDescription({ ...r, state: 'planned' }, NOW)).toBe(
      'plan ready · waiting for you · just now',
    );
  });
});

describe('describeRunner', () => {
  const fresh = {
    at: iso(3),
    body: { harness: 'echo', live_runs: [], paused_tenants: [], tenant: 'vsx' },
  };

  it('says in words whether a runner is there', () => {
    expect(describeRunner(null, NOW, { isAdmin: true, tenant: 'vsx' }).text).toBe(
      'No runner has reported yet.',
    );
    const d = describeRunner(fresh, NOW, { isAdmin: true, tenant: 'vsx' });
    expect(d.text).toBe('Runner ok · echo harness · last seen 3 s ago');
    expect(d.paused).toBe(false);
  });

  it('says so when dispatch is paused, and who may change that', () => {
    const paused = { at: iso(3), body: { ...fresh.body, paused_tenants: ['vsx'] } };
    const admin = describeRunner(paused, NOW, { isAdmin: true, tenant: 'vsx' });
    expect(admin.paused).toBe(true);
    expect(admin.text).toContain('Dispatch is paused');
    expect(admin.dispatchHint).toBe('Resume dispatch');
    const human = describeRunner(paused, NOW, { isAdmin: false, tenant: 'vsx' });
    expect(human.dispatchHint).toBe('Only an admin can resume dispatch.');
  });

  it('calls a silent runner stale', () => {
    const old = { at: iso(600), body: fresh.body };
    expect(
      describeRunner(old, NOW, { isAdmin: true, tenant: 'vsx', intervalMs: 30_000 }).text,
    ).toMatch(/^Runner not responding/);
  });
});

import { buildRunsTree, tooltipFor, type RunsNode } from '../../src/views/runsModel';

describe('buildRunsTree', () => {
  const runner = describeRunner({ at: iso(3), body: { harness: 'echo', tenant: 'vsx' } }, NOW, {
    isAdmin: true,
    tenant: 'vsx',
  });
  const base = {
    nowMs: NOW,
    filter: {},
    historyLimit: 25,
    hasMoreHistory: false,
    runner,
    isAdmin: true,
  };
  const records = () =>
    foldRuns(
      [
        started('run1', 30),
        started('ok1', 300),
        finished('ok1', 290, 'processed'),
        finished('plan1', 200, 'planned'),
        finished('bad1', 150, 'dead_letter', { reason: 'permanent' }),
      ],
      {
        nowMs: NOW,
        liveRunIds: new Set(['run1']),
        skillByEvent: new Map([['trigger-run1', 'supplier-risk']]),
      },
    );
  const find = (nodes: RunsNode[], id: string) => nodes.find((n) => n.id === id);

  it('leads with the dispatch row, then the sections, each with its count', () => {
    const tree = buildRunsTree({ ...base, records: records() });
    expect(tree.map((n) => n.id)).toEqual([
      'dispatch',
      'group:running',
      'group:waiting',
      'group:attention',
      'group:history',
    ]);
    expect(find(tree, 'group:running')).toMatchObject({ label: 'Running now', description: '1' });
    expect(find(tree, 'group:waiting')).toMatchObject({
      label: 'Waiting for you',
      description: '1',
    });
    expect(find(tree, 'group:attention')).toMatchObject({
      label: 'Needs attention',
      description: '1',
    });
    expect(find(tree, 'group:history')).toMatchObject({ label: 'History', description: '2' });
  });

  it('the dispatch row says what dispatch is doing and what an admin or anyone else can do', () => {
    const on = find(buildRunsTree({ ...base, records: [] }), 'dispatch')!;
    expect(on).toMatchObject({
      label: 'Dispatch is on',
      contextValue: 'dispatch.running',
      description: 'Pause dispatch',
    });
    const paused = describeRunner(
      { at: iso(3), body: { harness: 'echo', paused_tenants: ['vsx'] } },
      NOW,
      { isAdmin: false, tenant: 'vsx' },
    );
    const row = find(
      buildRunsTree({ ...base, runner: paused, isAdmin: false, records: [] }),
      'dispatch',
    )!;
    expect(row).toMatchObject({
      label: 'Dispatch is paused',
      contextValue: 'dispatch.paused',
      description: 'Only an admin can resume dispatch.',
    });
  });

  it('an empty section says so in words; waiting and attention hide when empty', () => {
    const tree = buildRunsTree({ ...base, records: [] });
    expect(tree.map((n) => n.id)).toEqual(['dispatch', 'group:running', 'group:history']);
    expect(find(tree, 'group:running')!.children![0]).toMatchObject({
      kind: 'empty',
      label: 'Nothing is running. Start a skill from a record.',
    });
    expect(find(tree, 'group:history')!.children![0]).toMatchObject({
      kind: 'empty',
      label: 'No finished runs yet.',
    });
  });

  it('a run row carries what the commands need and never an id in its label', () => {
    const tree = buildRunsTree({ ...base, records: records() });
    const run = find(tree, 'group:running')!.children![0]!;
    expect(run).toMatchObject({
      kind: 'run',
      label: 'supplier-risk · order-4500123',
      contextValue: 'run.running',
      runId: 'run1',
      rootEventId: 'root-run1',
      eventId: 'trigger-run1',
      pageId: 'markdown/instances/customer-order__order-4500123.md',
    });
    expect(run.label).not.toMatch(/[0-9A-Z]{20,}/);
    expect(find(tree, 'group:waiting')!.children![0]!.contextValue).toBe('run.planned');
    expect(find(tree, 'group:attention')!.children![0]!.contextValue).toBe('run.failed');
    expect(
      find(tree, 'group:history')!.children!.every(
        (c) => c.contextValue === 'run.done' || c.contextValue === 'run.failed',
      ),
    ).toBe(true);
  });

  it('history is cut at the limit and offers Load more only when there is more to load', () => {
    const many = foldRuns(
      Array.from({ length: 30 }, (_, i) => [
        started(`h${i}`, 1000 + i * 10),
        finished(`h${i}`, 990 + i * 10, 'processed'),
      ]).flat(),
      { nowMs: NOW },
    );
    const t1 = buildRunsTree({ ...base, records: many, historyLimit: 25, hasMoreHistory: false });
    const h1 = find(t1, 'group:history')!;
    expect(h1.children!.filter((c) => c.kind === 'run')).toHaveLength(25);
    expect(h1.children!.at(-1)).toMatchObject({ kind: 'more', label: 'Load 5 more…' });
    const t2 = buildRunsTree({ ...base, records: many, historyLimit: 100, hasMoreHistory: true });
    expect(find(t2, 'group:history')!.children!.at(-1)).toMatchObject({
      kind: 'more',
      label: 'Load more…',
    });
    const t3 = buildRunsTree({ ...base, records: many, historyLimit: 100, hasMoreHistory: false });
    expect(find(t3, 'group:history')!.children!.some((c) => c.kind === 'more')).toBe(false);
  });

  it('a filter narrows History only, and a filter that matches nothing says so', () => {
    const tree = buildRunsTree({ ...base, records: records(), filter: { states: ['failed'] } });
    expect(find(tree, 'group:history')!.children![0]).toMatchObject({
      kind: 'empty',
      label: 'No runs match the filter.',
    });
    expect(find(tree, 'group:running')!.description).toBe('1');
  });

  it('a load error is one worded row that retries', () => {
    const tree = buildRunsTree({ ...base, records: [], error: 'connection refused' });
    expect(tree).toHaveLength(1);
    expect(tree[0]).toMatchObject({
      kind: 'error',
      label: "Couldn't load runs: connection refused. Try again.",
    });
  });

  it('the tooltip gives the full account of a run, with the id only there', () => {
    const r = records().find((x) => x.runId === 'bad1')!;
    const tip = tooltipFor(r, NOW);
    expect(tip).toContain('failed for good');
    expect(tip).toContain('permanent');
    expect(tip).toContain('Run id: bad1');
  });
});

import { filterFromPicks, filterPickItems } from '../../src/views/runsModel';

describe('the filter pick list', () => {
  it('offers the states, then the skills seen, then a text search', () => {
    const items = filterPickItems(['customer-notice', 'supplier-risk'], {});
    expect(items.map((i) => i.id)).toEqual([
      'state:succeeded',
      'state:failed',
      'state:cancelled',
      'skill:customer-notice',
      'skill:supplier-risk',
      'text',
    ]);
    expect(items.find((i) => i.id === 'state:failed')).toMatchObject({ label: 'Failed' });
  });

  it('pre-selects what the current filter already says', () => {
    const items = filterPickItems(['supplier-risk'], {
      states: ['failed'],
      skill: 'supplier-risk',
    });
    expect(items.filter((i) => i.picked).map((i) => i.id)).toEqual([
      'state:failed',
      'skill:supplier-risk',
    ]);
  });

  it('turns the picks back into a filter: failed means failed and failed for good', () => {
    expect(filterFromPicks(['state:failed', 'skill:supplier-risk'], undefined)).toEqual({
      states: ['failed', 'dead_letter'],
      skill: 'supplier-risk',
    });
    expect(filterFromPicks([], undefined)).toEqual({});
    expect(filterFromPicks(['state:succeeded', 'text'], ' order-4500 ')).toEqual({
      states: ['succeeded'],
      text: 'order-4500',
    });
    // Only one skill at a time: the first one picked.
    expect(filterFromPicks(['skill:b', 'skill:a'], undefined)).toEqual({ skill: 'b' });
  });
});
