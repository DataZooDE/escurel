import { describe, expect, it } from 'vitest';
import { nextRun, scheduleWords } from '../../src/shared/schedule';

// `schedule:` (references/01): a 5-field cron read in UTC unless prefixed `CRON_TZ=<zone> `;
// `schedule_budget: {runs_per_day}` caps a UTC day's triggers.
const at = (iso: string) => new Date(iso);

describe('the next window of a schedule', () => {
  it('finds the next minute a 5-field expression opens, in UTC', () => {
    const now = at('2026-11-13T06:20:00Z'); // a Friday
    expect(nextRun('15 7 * * 1-5', now)?.toISOString()).toBe('2026-11-13T07:15:00.000Z');
    expect(nextRun('15 6 * * 1-5', now)?.toISOString()).toBe('2026-11-16T06:15:00.000Z');
    expect(nextRun('*/15 * * * *', now)?.toISOString()).toBe('2026-11-13T06:30:00.000Z');
    expect(nextRun('0 0 1 * *', now)?.toISOString()).toBe('2026-12-01T00:00:00.000Z');
    expect(nextRun('0 9 * * MON,WED', now)?.toISOString()).toBe('2026-11-16T09:00:00.000Z');
    expect(nextRun('0 9 * * 0', now)?.toISOString()).toBe('2026-11-15T09:00:00.000Z');
    expect(nextRun('0 9 * * 7', now)?.toISOString()).toBe('2026-11-15T09:00:00.000Z');
  });

  it('is strictly after now', () => {
    expect(nextRun('20 6 * * *', at('2026-11-13T06:20:00Z'))?.toISOString()).toBe(
      '2026-11-14T06:20:00.000Z',
    );
  });

  it('ORs day-of-month and day-of-week when both are restricted, as cron does', () => {
    // the 20th, or any Monday
    expect(nextRun('0 0 20 * 1', at('2026-11-13T06:00:00Z'))?.toISOString()).toBe(
      '2026-11-16T00:00:00.000Z',
    );
  });

  it('computes a UTC zone, and does not guess a local one', () => {
    expect(nextRun('CRON_TZ=UTC 15 7 * * *', at('2026-11-13T06:00:00Z'))?.toISOString()).toBe(
      '2026-11-13T07:15:00.000Z',
    );
    expect(nextRun('CRON_TZ=Europe/Berlin 15 7 * * *', at('2026-11-13T06:00:00Z'))).toBeUndefined();
  });

  it('answers nothing for what it cannot read, rather than a wrong time', () => {
    for (const expr of ['', '@daily', '0 7 * *', '61 7 * * *', '0 7 L * *', 'x y z a b'])
      expect(nextRun(expr, at('2026-11-13T06:00:00Z'))).toBeUndefined();
  });
});

describe('a schedule in words', () => {
  const now = at('2026-11-13T06:20:00Z');
  it('gives a short hint for a list and the whole story for a tooltip', () => {
    expect(
      scheduleWords({ schedule: '15 7 * * 1-5', schedule_budget: { runs_per_day: 3 } }, now),
    ).toEqual({
      hint: 'scheduled · next 07:15 UTC',
      detail:
        'Runs on its own on the schedule 15 7 * * 1-5 (UTC). Next run: 2026-11-13 07:15 UTC. At most 3 runs a day; a window past that is skipped and recorded.',
    });
  });

  it('names the zone it cannot compute, and the day when it is not today', () => {
    expect(scheduleWords({ schedule: 'CRON_TZ=Europe/Berlin 15 7 * * *' }, now)).toEqual({
      hint: 'scheduled',
      detail:
        'Runs on its own on the schedule 15 7 * * * (Europe/Berlin). Next run not shown for zoned schedules.',
    });
    expect(scheduleWords({ schedule: '0 9 * * 1' }, now)?.hint).toBe(
      'scheduled · next Mon 09:00 UTC',
    );
  });

  // The runner fires nothing for either (crew review PR-1, F3/F4): the words must not say it works.
  it('says a schedule the gateway cannot read is blocked', () => {
    expect(scheduleWords({ schedule: '15 7 * * Mo-Fr', schedule_invalid: true }, now)).toEqual({
      hint: 'schedule blocked',
      detail:
        'Schedule blocked: the schedule 15 7 * * Mo-Fr cannot be read, so the runner fires nothing. Fix the expression; validate names the problem.',
    });
  });

  it('says a malformed budget blocks the schedule, never that it runs', () => {
    expect(
      scheduleWords(
        { schedule: '15 7 * * 1-5', schedule_budget: { runs_per_day: 0, invalid: true } },
        now,
      ),
    ).toEqual({
      hint: 'schedule blocked',
      detail:
        'Schedule blocked: schedule_budget.runs_per_day must be a positive integer, so the runner fires nothing.',
    });
  });

  it('is nothing for a skill without a schedule', () => {
    expect(scheduleWords({}, now)).toBeUndefined();
  });
});
