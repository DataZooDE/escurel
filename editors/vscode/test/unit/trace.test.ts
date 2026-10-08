import { describe, expect, it } from 'vitest';
import type { ToolCallRow } from '../../src/shared/protocol';
import {
  axisLabel,
  callDuration,
  formatBytes,
  traceAxis,
  traceSameTimeNote,
  traceTimeline,
} from '../../src/shared/trace';

const call = (seq: number, over: Partial<ToolCallRow> = {}): ToolCallRow => ({
  seq,
  tool: 'read_page',
  status: 'ok',
  durationMs: 10,
  bytes: { request: 100, response: 2048 },
  at: `2026-10-04T12:00:0${seq}.000Z`,
  ...over,
});

describe('formatBytes', () => {
  it('says bytes, KB and MB the way a person reads them', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(2048)).toBe('2 KB');
    expect(formatBytes(1536)).toBe('1.5 KB');
    expect(formatBytes(5 * 1024 * 1024)).toBe('5 MB');
  });
});

describe('callDuration', () => {
  it('keeps milliseconds under a second, then seconds', () => {
    expect(callDuration(0)).toBe('< 1 ms');
    expect(callDuration(12.34)).toBe('12 ms');
    expect(callDuration(999.4)).toBe('999 ms');
    expect(callDuration(1500)).toBe('1.5 s');
    expect(callDuration(61_000)).toBe('1 min 1 s');
  });
});

describe('traceTimeline', () => {
  it('numbers the calls in order with the offset from the run start and a bar relative to the slowest', () => {
    const rows = traceTimeline(
      [call(1, { durationMs: 100 }), call(2, { durationMs: 400, tool: 'capture_event' })],
      '2026-10-04T12:00:00.000Z',
    );
    expect(rows.map((r) => r.seq)).toEqual([1, 2]);
    expect(rows[0]).toMatchObject({
      tool: 'read_page',
      offset: '+1 s',
      duration: '100 ms',
      barPercent: 25,
    });
    expect(rows[1]).toMatchObject({ tool: 'capture_event', offset: '+2 s', barPercent: 100 });
    expect(rows[0]!.sizes).toBe('sent 100 B · received 2 KB');
  });

  it('a call that failed says so in words, with its code, and is flagged', () => {
    const [row] = traceTimeline(
      [call(1, { status: 'error', errorCode: 'PERMISSION_DENIED' })],
      undefined,
    );
    expect(row).toMatchObject({ failed: true, outcome: 'failed', detail: 'PERMISSION_DENIED' });
    expect(row!.offset).toBe('');
    const [rejected] = traceTimeline([call(1, { status: 'rejected' })], undefined);
    expect(rejected).toMatchObject({ failed: true, outcome: 'rejected' });
    const [ok] = traceTimeline([call(1)], undefined);
    expect(ok).toMatchObject({ failed: false, outcome: 'ok', detail: '' });
  });

  it('is empty without calls and never divides by zero', () => {
    expect(traceTimeline([], undefined)).toEqual([]);
    expect(traceTimeline([call(1, { durationMs: 0 })], undefined)[0]!.barPercent).toBe(0);
  });
});

describe('toolWords', () => {
  it('says what a call did, and keeps the raw name for the tooltip', async () => {
    const { toolWords } = await import('../../src/shared/trace');
    expect(toolWords('list_inbox')).toBe('Read the inbox');
    expect(toolWords('list_instances')).toBe('Looked up records');
    expect(toolWords('expand')).toBe('Opened a page');
    expect(toolWords('something_new')).toBe('Something new');
    const [row] = traceTimeline([call(1, { tool: 'list_inbox' })], undefined);
    expect(row).toMatchObject({ tool: 'list_inbox', label: 'Read the inbox' });
  });
});

describe('traceAxis', () => {
  it('labels ticks in the unit a person reads', () => {
    expect(axisLabel(0)).toBe('0');
    expect(axisLabel(250)).toBe('250 ms');
    expect(axisLabel(1000)).toBe('1 s');
    expect(axisLabel(1500)).toBe('1.5 s');
    expect(axisLabel(60_000)).toBe('1 min');
    expect(axisLabel(90_000)).toBe('1 min 30 s');
  });

  it('spans the run from its start to the end of its last call, with round ticks', () => {
    const calls = [call(1, { durationMs: 100 }), call(2, { durationMs: 400 })];
    const axis = traceAxis(calls, '2026-10-04T12:00:00.000Z')!;
    // the last call starts at +2 s and takes 400 ms
    expect(axis.totalMs).toBe(2400);
    expect(axis.ticks[0]).toEqual({ percent: 0, label: '0' });
    expect(axis.ticks.map((t) => t.label)).toEqual(['0', '500 ms', '1 s', '1.5 s', '2 s']);
    expect(axis.ticks.at(-1)!.percent).toBeCloseTo((2000 / 2400) * 100, 5);
    expect(axis.endLabel).toBe('2.4 s');
  });

  it('puts each bar where its call ran on that scale and never lets a short call vanish', () => {
    const rows = traceTimeline(
      [call(1, { durationMs: 100 }), call(2, { durationMs: 400 })],
      '2026-10-04T12:00:00.000Z',
    );
    expect(rows[0]!.leftPercent).toBeCloseTo((1000 / 2400) * 100, 5);
    expect(rows[1]!.leftPercent).toBeCloseTo((2000 / 2400) * 100, 5);
    expect(rows[1]!.widthPercent).toBeCloseTo((400 / 2400) * 100, 5);
    const tiny = traceTimeline(
      [call(1, { durationMs: 0 }), call(2, { durationMs: 5000 })],
      '2026-10-04T12:00:00.000Z',
    );
    expect(tiny[0]!.widthPercent).toBeGreaterThanOrEqual(1);
  });

  it('starts at the first call when the run start is unknown, and has no axis without times', () => {
    const calls = [call(1), call(2)];
    const axis = traceAxis(calls, undefined)!;
    expect(axis.totalMs).toBe(1010);
    expect(traceAxis([call(1, { at: 'not a time' })], undefined)).toBeUndefined();
    expect(traceAxis([], undefined)).toBeUndefined();
    const rows = traceTimeline([call(1, { at: 'not a time' })], undefined);
    expect(rows[0]!.leftPercent).toBe(0);
    expect(rows[0]!.widthPercent).toBe(100);
  });

  it('keeps the bars inside the track however long the run took', () => {
    const rows = traceTimeline([call(1, { durationMs: 90_000 })], '2026-10-04T11:58:00.000Z');
    for (const r of rows) expect(r.leftPercent + r.widthPercent).toBeLessThanOrEqual(100.0001);
  });
});

describe('what a call asked and got back', () => {
  it('shows the summaries as readable JSON, and nothing when there are none', () => {
    const [withDetail, without] = traceTimeline(
      [
        call(1, { argsSummary: '{"page_id":"markdown/p.md"}', resultSummary: 'not json at all' }),
        call(2),
      ],
      undefined,
    );
    expect(withDetail!.args).toBe('{\n  "page_id": "markdown/p.md"\n}');
    expect(withDetail!.result).toBe('not json at all');
    expect(without).toMatchObject({ args: '', result: '' });
  });

  it('keeps a cut summary as it is: pretty-printing a truncated string would hide the cut', () => {
    const [row] = traceTimeline([call(1, { argsSummary: '{"k":"v"…[truncated]' })], undefined);
    expect(row!.args).toBe('{"k":"v"…[truncated]');
  });

  it('says which kind of trace this is', async () => {
    const { traceNote } = await import('../../src/shared/trace');
    expect(traceNote([call(1)])).toContain('not its arguments or its result');
    expect(traceNote([call(1, { argsSummary: '{}' })])).toContain('credentials');
  });
});

describe('a run start that is far from its calls, and calls that share one time', () => {
  it('starts the axis at the first call when the run start lies hours before it (a clock that is two hours off)', () => {
    const calls = [call(1, { durationMs: 100 }), call(2, { durationMs: 400 })];
    const farStart = '2026-10-04T10:00:00.000Z'; // two hours before the first call
    const axis = traceAxis(calls, farStart)!;
    expect(axis.totalMs).toBe(1400); // first call at +0, the last ends 1 s later + 400 ms
    expect(axis.ticks[0]).toEqual({ percent: 0, label: '0' });
    const rows = traceTimeline(calls, farStart);
    expect(rows[0]!.offset).toBe('+0 ms');
    expect(rows[0]!.leftPercent).toBe(0);
    expect(rows[1]!.offset).toBe('+1 s');
    // an ordinary lag between the run start and its first call still counts
    const near = traceAxis(calls, '2026-10-04T11:59:58.000Z')!;
    expect(near.totalMs).toBe(4400); // 3 s of lead-in, then the same 1.4 s
  });

  it('has no axis when every call carries the same time, and says so in one line instead', () => {
    const same = [1, 2, 3].map((n) => call(n, { at: '2026-10-04T12:00:01Z', durationMs: 20 * n }));
    expect(traceAxis(same, '2026-10-04T12:00:00Z')).toBeUndefined();
    expect(traceSameTimeNote(same)).toBe('All 3 steps ran within the same second.');
    const rows = traceTimeline(same, '2026-10-04T12:00:00Z');
    // without a scale the bars compare how long each took, side by side from the left
    expect(rows.map((r) => r.leftPercent)).toEqual([0, 0, 0]);
    expect(rows.map((r) => r.widthPercent)).toEqual([33, 67, 100]);
    expect(traceSameTimeNote([call(1), call(2)])).toBeUndefined();
    expect(traceSameTimeNote([call(1)])).toBeUndefined();
  });
});
