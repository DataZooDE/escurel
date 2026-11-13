// A skill's `schedule:` (#819) in words, with its next window where that can be told for sure (no `vscode`).
import { cleanText } from './untrustedText';

const MONTHS = ['JAN', 'FEB', 'MAR', 'APR', 'MAY', 'JUN', 'JUL', 'AUG', 'SEP', 'OCT', 'NOV', 'DEC'];
const DAYS = ['SUN', 'MON', 'TUE', 'WED', 'THU', 'FRI', 'SAT'];
const DAY_WORDS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
/** A schedule that opens no window within a year is not one this preview can say anything about. */
const HORIZON_DAYS = 366;

interface Field {
  values: Set<number>;
  /** `*` (or `*` with a step): "any", which matters for the day-of-month / day-of-week rule. */
  any: boolean;
}

/** One cron field (`*`, `*\/n`, `a`, `a-b`, `a-b/n`, lists, names); `undefined` when unreadable. */
function field(raw: string, min: number, max: number, names?: string[]): Field | undefined {
  const values = new Set<number>();
  const value = (s: string): number | undefined => {
    const named = names?.indexOf(s.toUpperCase()) ?? -1;
    if (named >= 0) return named + (min === 1 ? 1 : 0);
    return /^\d+$/.test(s) ? Number(s) : undefined;
  };
  for (const part of raw.split(',')) {
    const [range, stepRaw] = part.split('/');
    const step = stepRaw === undefined ? 1 : /^\d+$/.test(stepRaw) ? Number(stepRaw) : 0;
    if (!range || step < 1) return undefined;
    let lo: number | undefined;
    let hi: number | undefined;
    if (range === '*') [lo, hi] = [min, max];
    else if (range.includes('-')) {
      const [a, b] = range.split('-');
      [lo, hi] = [value(a ?? ''), value(b ?? '')];
    } else {
      lo = value(range);
      hi = stepRaw === undefined ? lo : max;
    }
    if (lo === undefined || hi === undefined || lo < min || hi > max || lo > hi) return undefined;
    for (let v = lo; v <= hi; v += step) values.add(v);
  }
  return { values, any: raw.startsWith('*') };
}

interface Cron {
  zone: string;
  expr: string;
  minute: Field;
  hour: Field;
  dom: Field;
  month: Field;
  dow: Field;
}

function parse(schedule: string): { zone: string; expr: string; cron?: Cron } | undefined {
  const m = /^\s*(?:CRON_TZ=(\S+)\s+)?(.*?)\s*$/.exec(schedule);
  if (!m || !m[2]) return undefined;
  const zone = m[1] ?? 'UTC';
  const expr = m[2];
  const parts = expr.split(/\s+/);
  if (parts.length !== 5) return { zone, expr };
  const [minute, hour, dom, month, dow] = [
    field(parts[0]!, 0, 59),
    field(parts[1]!, 0, 23),
    field(parts[2]!, 1, 31),
    field(parts[3]!, 1, 12, MONTHS),
    field(parts[4]!, 0, 7, DAYS),
  ];
  if (!minute || !hour || !dom || !month || !dow) return { zone, expr };
  if (dow.values.has(7)) dow.values.add(0);
  return { zone, expr, cron: { zone, expr, minute, hour, dom, month, dow } };
}

const isUtc = (zone: string) => ['UTC', 'Etc/UTC', 'Z', 'GMT', 'Etc/GMT'].includes(zone);

/**
 * The first window after `now`, for a UTC schedule this preview can read. A local zone is not
 * guessed (daylight saving moves it) and an extension it does not know (`@daily`, `L`) answers
 * nothing: the runner, not this preview, decides when a skill fires.
 */
export function nextRun(schedule: string, now: Date): Date | undefined {
  const cron = parse(schedule)?.cron;
  if (!cron || !isUtc(cron.zone)) return undefined;
  const start = new Date(now.getTime());
  start.setUTCSeconds(0, 0);
  const day0 = Date.UTC(start.getUTCFullYear(), start.getUTCMonth(), start.getUTCDate());
  const hours = [...cron.hour.values].sort((a, b) => a - b);
  const minutes = [...cron.minute.values].sort((a, b) => a - b);
  for (let d = 0; d <= HORIZON_DAYS; d += 1) {
    const day = new Date(day0 + d * 86_400_000);
    if (!cron.month.values.has(day.getUTCMonth() + 1)) continue;
    const domOk = cron.dom.values.has(day.getUTCDate());
    const dowOk = cron.dow.values.has(day.getUTCDay());
    // Cron's rule: with both day fields restricted a day matching EITHER fires.
    const dayOk = cron.dom.any || cron.dow.any ? domOk && dowOk : domOk || dowOk;
    if (!dayOk) continue;
    for (const h of hours)
      for (const m of minutes) {
        const t = day.getTime() + h * 3_600_000 + m * 60_000;
        if (t > now.getTime()) return new Date(t);
      }
  }
  return undefined;
}

const pad = (n: number) => String(n).padStart(2, '0');

function when(next: Date, now: Date): string {
  const clock = `${pad(next.getUTCHours())}:${pad(next.getUTCMinutes())} UTC`;
  const days = Math.floor(
    (Date.UTC(next.getUTCFullYear(), next.getUTCMonth(), next.getUTCDate()) -
      Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate())) /
      86_400_000,
  );
  if (days === 0) return clock;
  if (days < 7) return `${DAY_WORDS[next.getUTCDay()]} ${clock}`;
  return `${next.toISOString().slice(0, 10)} ${clock}`;
}

/** A one-line `hint` for a list row and the whole `detail` for a tooltip or a fact. */
export function scheduleWords(
  skill: { schedule?: string; schedule_budget?: { runs_per_day?: number } },
  now: Date,
): { hint: string; detail: string } | undefined {
  const raw = typeof skill.schedule === 'string' ? cleanText(skill.schedule, 120) : '';
  const parsed = raw ? parse(raw) : undefined;
  if (!parsed) return undefined;
  const next = nextRun(raw, now);
  const budget = skill.schedule_budget?.runs_per_day;
  const detail = [
    `Runs on its own on the schedule ${parsed.expr} (${parsed.zone}).`,
    next ? `Next run: ${next.toISOString().slice(0, 10)} ${when(next, next)}.` : '',
    typeof budget === 'number' && budget > 0
      ? `At most ${budget} ${budget === 1 ? 'run' : 'runs'} a day; a window past that is skipped and recorded.`
      : '',
  ]
    .filter(Boolean)
    .join(' ');
  return { hint: next ? `scheduled · next ${when(next, now)}` : 'scheduled', detail };
}
