import { describe, expect, it } from 'vitest';
import type { ToolCallRow } from '../../src/shared/protocol';
import { callDuration, formatBytes, traceTimeline } from '../../src/shared/trace';

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
