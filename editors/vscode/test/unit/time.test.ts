import { describe, expect, it } from 'vitest';
import { formatClock, formatDateTime, formatDuration, toIsoUtc } from '../../src/shared/time';

// The gateway speaks two timestamp dialects (recorded in test/unit/fixtures/lineage/):
// RFC 3339 with `Z` almost everywhere, and `run-attempt`'s `2026-09-29 02:59:08.035678` —
// a space, microseconds, no zone. Three modules parsed them three ways before this one.
describe('gateway time', () => {
  it('reads the zone-less runner shape as UTC, whatever the machine zone', () => {
    expect(toIsoUtc('2026-09-29 02:59:08.035678')).toBe('2026-09-29T02:59:08.035Z');
  });

  it('reads a zone-less T form as UTC too, not local time', () => {
    expect(toIsoUtc('2026-09-29T02:59:08')).toBe('2026-09-29T02:59:08.000Z');
  });

  it('truncates sub-millisecond digits itself rather than trusting the engine', () => {
    // JavaScript engines disagree on more than three fractional digits; the parse must not.
    expect(toIsoUtc('2026-09-29T02:59:08.999999Z')).toBe('2026-09-29T02:59:08.999Z');
  });

  it('keeps an explicit zone', () => {
    expect(toIsoUtc('2026-09-29T04:59:08+02:00')).toBe('2026-09-29T02:59:08.000Z');
  });

  it('answers undefined for anything that is not a time', () => {
    for (const bad of [undefined, null, '', 'yesterday', 42, {}])
      expect(toIsoUtc(bad)).toBeUndefined();
  });

  it('formats a clock time and a dated time in UTC', () => {
    expect(formatClock('2026-09-29 02:59:08.035678')).toBe('02:59:08');
    // Run detail shows the date: an attempt spanning midnight is otherwise ambiguous.
    expect(formatDateTime('2026-09-29 02:59:08.035678')).toBe('2026-09-29 02:59:08 UTC');
    expect(formatClock('nonsense')).toBe('');
  });

  it('formats durations at the scale a person reads them', () => {
    expect(formatDuration('2026-09-29 02:59:08.035', '2026-09-29 02:59:08.210')).toBe('175 ms');
    expect(formatDuration('2026-09-29T06:12:10Z', '2026-09-29T06:12:58Z')).toBe('48 s');
    expect(formatDuration('2026-09-29T06:12:10Z', '2026-09-29T06:15:22Z')).toBe('3 min 12 s');
    expect(formatDuration('2026-09-29T06:12:10Z', undefined)).toBe('');
    // A clock skew that puts the end first is not a negative duration.
    expect(formatDuration('2026-09-29T06:12:10Z', '2026-09-29T06:12:09Z')).toBe('0 ms');
  });
});
