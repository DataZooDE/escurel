import { describe, expect, it } from 'vitest';
import type { Changeset } from '../../src/client';
import { buildOverview, type OverviewInputs } from '../../src/overview/model';
import { changesetRow } from '../../src/views/awaitingModel';
import type { RunRecord } from '../../src/views/runsModel';

const NOW = Date.parse('2026-10-06T10:00:00Z');
const run = (over: Partial<RunRecord>): RunRecord => ({
  runId: 'r1',
  state: 'succeeded',
  skill: 'supplier-risk',
  targetPageId: 'markdown/instances/customer-order/order-4500123.md',
  rootEventId: 'root1',
  triggerEventId: 'ev1',
  startedAtMs: NOW - 60_000,
  finishedAtMs: NOW - 30_000,
  durationMs: 30_000,
  ...over,
});
const changeset = (id: string, page: string): Changeset =>
  ({
    changeset_id: id,
    target_page_ids: [`markdown/instances/customer-order/${page}.md`],
    drafts: 2,
    author: 'agent:supplier-risk',
    created_at: '2026-10-06T09:00:00Z',
    status: 'open',
  }) as unknown as Changeset;

const base = (over: Partial<OverviewInputs> = {}): OverviewInputs => ({
  nowMs: NOW,
  focusOn: false,
  awaiting: [],
  runs: [],
  runner: { text: 'Agents are running · last seen just now', paused: false, health: 'ok' },
  open: [],
  ...over,
});
const tile = (v: ReturnType<typeof buildOverview>, id: string) =>
  v.view.tiles.find((t) => t.id === id)!;

describe('the overview board model', () => {
  it('always offers the same five tiles, in the order a day starts: decisions first', () => {
    const { view } = buildOverview(base());
    expect(view.tiles.map((t) => t.id)).toEqual([
      'decisions',
      'agents',
      'attention',
      'open',
      'recent',
    ]);
  });

  it('says plainly when nothing waits, and when something does', () => {
    expect(tile(buildOverview(base()), 'decisions').headline).toBe('Nothing is waiting for you');
    const out = buildOverview(base({ awaiting: [changesetRow(changeset('c1', 'order-4500131'))] }));
    const t = tile(out, 'decisions');
    expect(t.headline).toBe('1 waiting for you');
    expect(t.tone).toBe('attention');
    expect(t.items[0]!.label).toContain('order-4500131');
  });

  it('every item opens through a key the host can resolve, never an id the page names', () => {
    const out = buildOverview(
      base({
        awaiting: [changesetRow(changeset('c1', 'order-1'))],
        runs: [run({ runId: 'rF', state: 'failed', reason: 'harness refused' })],
      }),
    );
    const keys = out.view.tiles.flatMap((t) => t.items.map((i) => i.key));
    expect(new Set(keys).size).toBe(keys.length);
    for (const k of keys) expect(out.actions.has(k)).toBe(true);
    const failed = tile(out, 'attention').items[0]!;
    expect(out.actions.get(failed.key)).toEqual({
      command: 'escurel.openRun',
      args: [{ runId: 'rF' }],
    });
    const decision = tile(out, 'decisions').items[0]!;
    expect(out.actions.get(decision.key)?.command).toBe('escurel.openReview');
  });

  it('a failed run needs attention until a newer run for the same trigger replaced it', () => {
    const failed = run({ runId: 'rF', state: 'failed', finishedAtMs: NOW - 120_000 });
    expect(tile(buildOverview(base({ runs: [failed] })), 'attention').headline).toBe(
      '1 needs a look',
    );
    const retried = run({ runId: 'rOK', state: 'succeeded', finishedAtMs: NOW - 10_000 });
    expect(tile(buildOverview(base({ runs: [failed, retried] })), 'attention').headline).toBe(
      'All clear',
    );
  });

  it('shows what agents are doing now and what they finished, in words', () => {
    const out = buildOverview(
      base({
        runs: [
          run({ runId: 'rA', state: 'running', finishedAtMs: undefined }),
          run({ runId: 'rB', state: 'succeeded' }),
        ],
      }),
    );
    expect(tile(out, 'agents').headline).toBe('Agents are running · last seen just now');
    expect(tile(out, 'agents').items[0]!.label).toBe('Running · supplier-risk · order-4500123');
    expect(tile(out, 'recent').items[0]!.label).toBe('Done · supplier-risk · order-4500123');
  });

  it('counts open items per skill and says when there are more than a page', () => {
    const out = buildOverview(
      base({
        open: [
          { skill: 'customer-order', title: 'Customer order', count: 50, more: true },
          { skill: 'supplier', title: 'Supplier', count: 3, more: false },
        ],
      }),
    );
    expect(tile(out, 'open').items.map((i) => [i.label, i.detail])).toEqual([
      ['Customer order', '50+ records'],
      ['Supplier', '3 records'],
    ]);
  });

  it('caps a tile and says how many more there are', () => {
    const rows = Array.from({ length: 8 }, (_, i) =>
      changesetRow(changeset(`c${i}`, `order-${i}`)),
    );
    const t = tile(buildOverview(base({ awaiting: rows })), 'decisions');
    expect(t.items).toHaveLength(5);
    expect(t.more).toBe(3);
  });

  it('cleans text a gateway wrote: control and bidi characters never reach the board', () => {
    const out = buildOverview(
      base({ runs: [run({ state: 'failed', reason: 'bad\u202Etxt.exe\u0000 reason' })] }),
    );
    const detail = tile(out, 'attention').items[0]!.detail ?? '';
    expect(detail.includes(String.fromCharCode(0x202e))).toBe(false);
    expect(detail.includes(String.fromCharCode(0))).toBe(false);
  });

  it('carries the focus state so the board can offer the way out', () => {
    expect(buildOverview(base({ focusOn: true })).view.focusOn).toBe(true);
  });
});
