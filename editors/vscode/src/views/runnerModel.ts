import type { Event } from '../client';

/**
 * Runner status heartbeat interval assumption:
 * The heartbeat interval is not transmitted in the runner status body.
 * In crates/escurel-runner-core/src/config.rs (ESCUREL_RUNNER_STATUS_INTERVAL),
 * the default interval is 30 seconds (30,000 ms).
 */
export const ESCUREL_RUNNER_STATUS_INTERVAL_MS = 30_000;

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
  harness?: string;
  harness_permits_available?: number;
  last_poll_age_ms?: number;
  live_runs?: RunnerLiveRun[];
  paused_tenants?: string[];
  runner_id?: string;
  runs?: RunnerRunsCount;
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
 *   older than 3x the interval (90s) or last_poll_age_ms huge (>90s) => "stale".
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
