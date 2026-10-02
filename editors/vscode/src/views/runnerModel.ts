import type { Event } from '../client';
import { pageSlug } from '../shared/pageId';

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

export interface DeadLetterItem {
  runId: string;
  eventId: string;
  targetPageId?: string | null;
  slug: string;
  reason?: string;
  error?: string;
  description?: string;
}

export type RunnerRowKind =
  | 'health'
  | 'runner'
  | 'runs'
  | 'throttled'
  | 'paused'
  | 'pausedTenant'
  | 'permits'
  | 'liveRuns'
  | 'liveRun'
  | 'deadLetters'
  | 'deadLetter'
  | 'quotas';

export interface RunnerRow {
  kind: RunnerRowKind;
  label: string;
  description?: string;
  contextValue?: string;
  runId?: string;
  eventId?: string;
  children?: RunnerRow[];
  collapsibleState?: 'none' | 'collapsed' | 'expanded';
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

/**
 * Extracts dead-letter items from runner events.
 * Finds run-finished events with status dead_letter, and correlates with
 * matching run-started events to find the trigger event id from
 * provenance.runner.event_id.
 * Max 20 items are retained.
 */
export function extractDeadLetters(events: Event[], maxItems = 20): DeadLetterItem[] {
  // Index run-started events by run_id for fast trigger event_id lookup
  const runStartedByRunId = new Map<string, Event>();
  for (const e of events) {
    if (e.run_id && (e.title === 'run-started' || e.event_id.endsWith(':started'))) {
      runStartedByRunId.set(e.run_id, e);
    }
  }

  const deadLetters: DeadLetterItem[] = [];

  for (const e of events) {
    if (!e.run_id) continue;

    // Check if run-finished
    let isDeadLetter = false;
    let reason: string | undefined;
    let error: string | undefined;

    if (e.body) {
      try {
        const parsed = JSON.parse(e.body) as {
          status?: string;
          reason?: string;
          error?: string;
        };
        if (parsed.status === 'dead_letter') {
          isDeadLetter = true;
          reason = parsed.reason;
          error = parsed.error;
        }
      } catch {
        // ignore JSON parse error
      }
    }

    if (!isDeadLetter) continue;

    const started = runStartedByRunId.get(e.run_id);
    const triggerEventId =
      (started?.provenance as { runner?: { event_id?: string } } | undefined)?.runner?.event_id ??
      (e.provenance as { runner?: { event_id?: string } } | undefined)?.runner?.event_id ??
      e.root_event_id ??
      '';

    const pageId = e.instance_page_id ?? started?.instance_page_id ?? null;
    const slug = pageId ? pageSlug(pageId) : e.run_id;

    // Format description: reason and/or first words of error
    const firstWords = error ? error.split(/\s+/).slice(0, 10).join(' ') : '';
    let description = '';
    if (reason && firstWords) {
      description = `${reason} — ${firstWords}`;
    } else if (reason) {
      description = reason;
    } else if (firstWords) {
      description = firstWords;
    }

    deadLetters.push({
      runId: e.run_id,
      eventId: triggerEventId,
      targetPageId: pageId,
      slug,
      reason,
      error,
      description,
    });

    if (deadLetters.length >= maxItems) break;
  }

  return deadLetters;
}

/**
 * Builds runner TreeView rows according to SPEC §3.3, §3.9 and M4 plan decisions.
 */
export function buildRunnerRows(
  status:
    | Event
    | RunnerStatusBody
    | { at?: string | null; body?: string | RunnerStatusBody | null }
    | null
    | undefined,
  deadLetters: (DeadLetterItem | Event)[],
  opts: {
    admin: 'admin' | 'not-admin' | 'unknown';
    quotas?: Record<string, unknown>;
    /** The observed heartbeat interval; see `estimateHeartbeatIntervalMs`. */
    intervalMs?: number;
  },
  now: Date | number | string = new Date(),
): RunnerRow[] {
  let statusRow: { at?: string | null; body?: string | RunnerStatusBody | null } | null = null;
  let body: RunnerStatusBody | null = null;

  if (status) {
    if ('body' in status) {
      statusRow = status;
      body =
        typeof status.body === 'string'
          ? parseRunnerStatusBody(status as Event)
          : (status.body as RunnerStatusBody);
    } else {
      // Passed RunnerStatusBody directly
      body = status as RunnerStatusBody;
      statusRow = { body };
    }
  }

  const rows: RunnerRow[] = [];

  // 1. Health row
  const health = deriveHealth(statusRow, now, {
    ...(opts.intervalMs !== undefined ? { intervalMs: opts.intervalMs } : {}),
  });
  const healthDesc = health.description ? `${health.label} · ${health.description}` : health.label;
  rows.push({
    kind: 'health',
    label: 'Health',
    description: healthDesc,
    collapsibleState: 'none',
  });

  // If no status body available, return health row (+ dead letters if any)
  if (!body) {
    return rows;
  }

  // 2. Runner row: runner_id, version, harness, tenant, uptime_s
  const runnerDescParts: string[] = [];
  if (body.runner_id) runnerDescParts.push(body.runner_id);
  if (body.version) runnerDescParts.push(`v${body.version}`);
  if (body.harness) runnerDescParts.push(`harness: ${body.harness}`);
  if (body.tenant) runnerDescParts.push(`tenant: ${body.tenant}`);
  if (body.uptime_s !== undefined) runnerDescParts.push(`uptime ${body.uptime_s}s`);

  rows.push({
    kind: 'runner',
    label: 'Runner',
    description: runnerDescParts.join(' · '),
    collapsibleState: 'none',
  });

  // 3. Runs row: live_runs.length live, then processed, failed, dead_letter, cancelled, planned, pending
  const liveRuns = body.live_runs ?? [];
  const runsCount = body.runs ?? {};
  const runsDesc = `${liveRuns.length} live · ${runsCount.processed ?? 0} processed, ${runsCount.failed ?? 0} failed, ${runsCount.dead_letter ?? 0} dead letter, ${runsCount.cancelled ?? 0} cancelled, ${runsCount.planned ?? 0} planned, ${runsCount.pending ?? 0} pending`;

  rows.push({
    kind: 'runs',
    label: 'Runs',
    description: runsDesc,
    collapsibleState: 'none',
  });

  // 4. Throttled row: ONLY when any throttled.* > 0
  const throttled = body.throttled;
  const hasThrottle =
    throttled &&
    ((throttled.runs_per_min ?? 0) > 0 ||
      (throttled.max_concurrent ?? 0) > 0 ||
      (throttled.paused ?? 0) > 0);

  if (hasThrottle) {
    const parts: string[] = [];
    if ((throttled.runs_per_min ?? 0) > 0) parts.push(`${throttled.runs_per_min} runs/min`);
    if ((throttled.max_concurrent ?? 0) > 0)
      parts.push(`${throttled.max_concurrent} max concurrent`);
    if ((throttled.paused ?? 0) > 0) parts.push(`${throttled.paused} paused`);
    rows.push({
      kind: 'throttled',
      label: 'Throttled',
      description: parts.join(' · '),
      collapsibleState: 'none',
    });
  }

  // 5. Paused row: listing paused_tenants (each with context value paused)
  const pausedTenants = body.paused_tenants ?? [];
  if (pausedTenants.length > 0) {
    rows.push({
      kind: 'paused',
      label: 'Paused',
      description: `${pausedTenants.length} tenant${pausedTenants.length === 1 ? '' : 's'}`,
      collapsibleState: 'expanded',
      children: pausedTenants.map((tenant) => ({
        kind: 'pausedTenant',
        label: tenant,
        contextValue: 'paused',
        collapsibleState: 'none',
      })),
    });
  }

  // 6. Permits row
  if (body.harness_permits_available !== undefined) {
    rows.push({
      kind: 'permits',
      label: 'Permits',
      description: `${body.harness_permits_available} available`,
      collapsibleState: 'none',
    });
  }

  // 7. Live runs group
  if (liveRuns.length > 0) {
    rows.push({
      kind: 'liveRuns',
      label: 'Live runs',
      description: String(liveRuns.length),
      collapsibleState: 'expanded',
      children: liveRuns.map((r) => {
        const slug = r.instance_page_id ? pageSlug(r.instance_page_id) : r.run_id;
        return {
          kind: 'liveRun',
          label: slug,
          description: r.run_id,
          contextValue: 'liveRun',
          runId: r.run_id,
          eventId: r.event_id,
          collapsibleState: 'none',
        };
      }),
    });
  }

  // 8. Dead letters group (newest 20)
  // Convert events to dead letters if Event[] was passed
  const deadLetterItems: DeadLetterItem[] =
    deadLetters.length > 0 && 'at' in deadLetters[0]!
      ? extractDeadLetters(deadLetters as Event[], 20)
      : (deadLetters as DeadLetterItem[]);

  if (deadLetterItems.length > 0) {
    rows.push({
      kind: 'deadLetters',
      label: 'Dead letters',
      description: String(deadLetterItems.length),
      collapsibleState: 'expanded',
      children: deadLetterItems.map((dl) => ({
        kind: 'deadLetter',
        label: dl.slug,
        description: dl.description ?? dl.reason ?? dl.error ?? '',
        contextValue: 'deadLetter',
        runId: dl.runId,
        eventId: dl.eventId,
        collapsibleState: 'none',
      })),
    });
  }

  // 9. Quotas row ONLY for an admin and ONLY when admin_quota answers with numbers
  if (opts.admin === 'admin' && opts.quotas) {
    // Only render if we can read real {used, limit} style numbers
    const used = opts.quotas.used;
    const limit = opts.quotas.limit;
    if (typeof used === 'number' && typeof limit === 'number') {
      rows.push({
        kind: 'quotas',
        label: 'Quotas',
        description: `${used} / ${limit}`,
        collapsibleState: 'none',
      });
    }
  }

  return rows;
}
