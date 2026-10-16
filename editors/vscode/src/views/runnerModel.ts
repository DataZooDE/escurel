import type { Event } from '../client';
import { cleanText } from '../shared/untrustedText';

/**
 * Runner status heartbeat interval assumption:
 * The heartbeat interval is not transmitted in the runner status body.
 * In crates/escurel-runner-core/src/config.rs (ESCUREL_RUNNER_STATUS_INTERVAL),
 * the default interval is 5 minutes (300,000 ms; it was 30 s until every heartbeat turned out to
 * cost an event row, and on a lake backend a snapshot). An observed interval (see
 * `estimateHeartbeatIntervalMs`) always wins over this assumption.
 */
export const ESCUREL_RUNNER_STATUS_INTERVAL_MS = 300_000;

export interface RunnerLiveRun {
  run_id: string;
  event_id: string;
  instance_page_id?: string | null;
}

export interface RunnerRunsCount {
  processed?: number;
  failed?: number;
  dead_letter?: number;
  cancelled?: number;
  planned?: number;
  pending?: number;
  total?: number;
}

export interface RunnerThrottled {
  max_concurrent?: number;
  paused?: number;
  runs_per_min?: number;
}

export interface RunnerStatusBody {
  draining?: boolean;
  /** The harness names a manual start may ask for (BACKEND_GAPS PR-10). Absent on an old runner. */
  harness_allow?: string[];
  harness?: string;
  harness_permits_available?: number;
  last_poll_age_ms?: number;
  live_runs?: RunnerLiveRun[];
  paused_tenants?: string[];
  runner_id?: string;
  runs?: RunnerRunsCount;
  /** `started` | `changed` | `heartbeat` | `stopping`: set by the gateway from `report_runner_status`. */
  state?: string;
  /** How often this runner reports while idle (seconds). Absent on a runner that files events. */
  status_interval_s?: number;
  started_at?: string;
  last_change_at?: string;
  tenant?: string;
  throttled?: RunnerThrottled;
  uptime_s?: number;
  version?: string;
}

export interface HealthInfo {
  status: 'ok' | 'stale' | 'draining' | 'none';
  state: 'ok' | 'stale' | 'draining' | 'none';
  label: string;
  description?: string;
}

export function parseRunnerStatusBody(
  event: Event | { body?: string | null } | null | undefined,
): RunnerStatusBody | null {
  if (!event || !event.body) return null;
  try {
    return JSON.parse(event.body) as RunnerStatusBody;
  } catch {
    return null;
  }
}

/** The most harness names the picker ever offers, each at most this long. */
export const MAX_HARNESS_NAMES = 20;
export const MAX_HARNESS_NAME_LENGTH = 40;

/**
 * The published allow-list as displayable names. Text from the status is untrusted, so every name
 * goes through `cleanText` and over-long ones are dropped, never truncated into a different name.
 * `undefined` means the runner did not publish a list (unknown status, or a runner from before
 * PR-10) — not an empty allow-list.
 */
export function allowedHarnesses(
  body: RunnerStatusBody | null | undefined,
): string[] | undefined {
  const raw = body?.harness_allow;
  if (raw === undefined || raw === null) return undefined;
  if (!Array.isArray(raw)) return undefined;
  const seen = new Set<string>();
  const out: string[] = [];
  for (const entry of raw) {
    if (typeof entry !== 'string') continue;
    const name = entry.trim();
    if (!name || name.length > MAX_HARNESS_NAME_LENGTH) continue;
    const clean = cleanText(name, MAX_HARNESS_NAME_LENGTH);
    if (!clean || seen.has(clean)) continue;
    seen.add(clean);
    out.push(clean);
    if (out.length >= MAX_HARNESS_NAMES) break;
  }
  return out;
}

/**
 * The runner's heartbeat interval, OBSERVED. `ESCUREL_RUNNER_STATUS_INTERVAL` is configurable and
 * the status body does not carry it, so the default alone would call a healthy runner with a
 * longer heartbeat "stale". The rows show it: the gaps between consecutive `heartbeat` rows (a
 * `changed` row arrives whenever something changes, so it says nothing about the interval). The
 * median, so one late heartbeat does not move it. With fewer than two heartbeats to measure, the
 * runner's default.
 */
export function estimateHeartbeatIntervalMs(
  rows: readonly { at?: string | null; title?: string | null }[],
): number {
  const times = rows
    .filter((r) => r.title === 'heartbeat' && r.at)
    .map((r) => Date.parse(r.at as string))
    .filter((t) => Number.isFinite(t))
    .sort((a, b) => a - b);
  const gaps: number[] = [];
  for (let i = 1; i < times.length; i += 1) gaps.push(times[i]! - times[i - 1]!);
  if (gaps.length === 0) return ESCUREL_RUNNER_STATUS_INTERVAL_MS;
  gaps.sort((a, b) => a - b);
  const mid = Math.floor(gaps.length / 2);
  return gaps.length % 2 ? gaps[mid]! : (gaps[mid - 1]! + gaps[mid]!) / 2;
}

/**
 * The interval of the runner whose status is NEWEST in `rows` (newest first). A runner on the
 * overwritten record declares it (`body.status_interval_s`), which is exact. A runner that still
 * files heartbeat events does not, so it is observed from the gaps between ITS OWN rows (several
 * runners of a tenant share the label, and mixing their gaps would measure no runner).
 */
export function runnerIntervalMs(
  rows: readonly { at?: string | null; title?: string | null; body?: string | null }[],
): number {
  const newest = rows[0];
  if (!newest) return ESCUREL_RUNNER_STATUS_INTERVAL_MS;
  const body = parseRunnerStatusBody(newest);
  const declared = body?.status_interval_s;
  if (typeof declared === 'number' && Number.isFinite(declared) && declared > 0) {
    return declared * 1000;
  }
  const id = body?.runner_id;
  const ownRows = id ? rows.filter((r) => parseRunnerStatusBody(r)?.runner_id === id) : rows;
  return estimateHeartbeatIntervalMs(ownRows);
}

/** Formats age in seconds: "last heartbeat N s ago". */
export function formatHeartbeatAge(ageMs: number): string {
  const sec = Math.max(0, Math.floor(ageMs / 1000));
  return `last heartbeat ${sec} s ago`;
}

/**
 * Health is DERIVED:
 * - draining => "draining"
 * - no status row => "no runner status yet"
 * - otherwise compare newest row's age (its `at`) with the heartbeat interval:
 *   fresh => "ok"
 *   older than 3x the interval (15 min at the 5 min default) or last_poll_age_ms beyond the same
 *   bound => "stale" (so a runner polling every 10 min is not flagged).
 */
export function deriveHealth(
  statusRow: { at?: string | null; body?: string | RunnerStatusBody | null } | null | undefined,
  now: Date | number | string = new Date(),
  options: { intervalMs?: number } = {},
): HealthInfo {
  const intervalMs = options.intervalMs ?? ESCUREL_RUNNER_STATUS_INTERVAL_MS;
  if (!statusRow) {
    return {
      status: 'none',
      state: 'none',
      label: 'no runner status yet',
    };
  }

  const nowMs = typeof now === 'number' ? now : new Date(now).getTime();
  let parsedBody: RunnerStatusBody | null = null;
  if (statusRow.body) {
    if (typeof statusRow.body === 'string') {
      try {
        parsedBody = JSON.parse(statusRow.body) as RunnerStatusBody;
      } catch {
        parsedBody = null;
      }
    } else {
      parsedBody = statusRow.body as RunnerStatusBody;
    }
  }

  // Draining takes priority
  if (parsedBody?.draining) {
    const ageMs = statusRow.at ? nowMs - new Date(statusRow.at).getTime() : 0;
    return {
      status: 'draining',
      state: 'draining',
      label: 'draining',
      description: statusRow.at ? formatHeartbeatAge(ageMs) : undefined,
    };
  }

  const ageMs = statusRow.at ? nowMs - new Date(statusRow.at).getTime() : 0;
  const isHeartbeatStale = ageMs > 3 * intervalMs;
  const isPollAgeStale = (parsedBody?.last_poll_age_ms ?? 0) > 3 * intervalMs;

  if (isHeartbeatStale || isPollAgeStale) {
    return {
      status: 'stale',
      state: 'stale',
      label: 'stale',
      description: statusRow.at ? formatHeartbeatAge(ageMs) : undefined,
    };
  }

  return {
    status: 'ok',
    state: 'ok',
    label: 'ok',
    description: statusRow.at ? formatHeartbeatAge(ageMs) : undefined,
  };
}
