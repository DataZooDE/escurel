// Gateway timestamps, read one way everywhere.
//
// The gateway speaks two dialects: RFC 3339 with a zone almost everywhere, and the
// runner's `run-attempt` rows, which write `2026-09-29 02:59:08.035678` — a space,
// microseconds, and no zone at all. A zone-less time is UTC on the wire; `Date` would
// read it as local time. Before this module the thread model, the run model and the run
// webview each parsed the two shapes differently. Shared by the host and the webviews,
// so nothing here may import `vscode` or Node.

const SHAPE = /^(\d{4}-\d\d-\d\d)[ T](\d\d:\d\d:\d\d)(?:\.(\d+))?(Z|[+-]\d\d:?\d\d)?$/;

/** A gateway timestamp as a `Date`, or `undefined` for anything that is not one. */
export function parseGatewayTime(raw: unknown): Date | undefined {
  if (typeof raw !== 'string') return undefined;
  const m = SHAPE.exec(raw.trim());
  if (!m) return undefined;
  const [, date, time, fraction, zone] = m;
  // Three digits, cut rather than rounded: engines disagree on longer fractions, and
  // rounding .9999 would move a time into the next second.
  const millis = (fraction ?? '').slice(0, 3).padEnd(3, '0');
  const d = new Date(`${date}T${time}.${millis}${zone ?? 'Z'}`);
  return Number.isNaN(d.getTime()) ? undefined : d;
}

/** Normalised ISO 8601 UTC (`…Z`), or `undefined`. */
export function toIsoUtc(raw: unknown): string | undefined {
  return parseGatewayTime(raw)?.toISOString();
}

const pad = (n: number): string => String(n).padStart(2, '0');

/** `HH:MM:SS` in UTC — what a thread card has room for. Empty when unparseable. */
export function formatClock(raw: unknown): string {
  const d = parseGatewayTime(raw);
  if (!d) return '';
  return `${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())}`;
}

/** `YYYY-MM-DD HH:MM:SS UTC` — run detail shows the date, since runs can span midnight. */
export function formatDateTime(raw: unknown): string {
  const d = parseGatewayTime(raw);
  if (!d) return '';
  return `${d.toISOString().slice(0, 10)} ${formatClock(raw)} UTC`;
}

/** The time between two gateway timestamps at the scale a person reads it. */
export function formatDuration(start: unknown, end: unknown): string {
  const a = parseGatewayTime(start);
  const b = parseGatewayTime(end);
  if (!a || !b) return '';
  // A skewed clock can put the end first; that is not a negative duration.
  const ms = Math.max(0, b.getTime() - a.getTime());
  if (ms < 1000) return `${ms} ms`;
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s} s`;
  const rest = s % 60;
  return rest ? `${Math.floor(s / 60)} min ${rest} s` : `${Math.floor(s / 60)} min`;
}
