import type { Event, GetRunToolCallsResponse, LineageNode } from '../client';
import type { PlanStep, RunAttempt, RunView, ToolCallRow } from '../shared/protocol';

type Body = Record<string, unknown>;

function bodyOf(event: Event): Body {
  try {
    const parsed: unknown = JSON.parse(event.body ?? '{}');
    return parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed)
      ? (parsed as Body)
      : {};
  } catch {
    return {};
  }
}

function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined;
}

function numberValue(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function timestamp(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined;
  // Runner attempt timestamps have a space and no zone; Date.parse would use local time.
  const utc = /^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d(?:\.\d+)?$/.test(value)
    ? `${value.replace(' ', 'T')}Z`
    : value;
  const date = new Date(utc);
  return Number.isNaN(date.getTime()) ? undefined : date.toISOString();
}

function planSteps(value: unknown): PlanStep[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const allowed = new Set<PlanStep['status']>(['pending', 'in_progress', 'completed', 'blocked']);
  return value.flatMap((entry: unknown): PlanStep[] => {
    if (entry === null || typeof entry !== 'object') return [];
    const row = entry as Record<string, unknown>;
    if (typeof row.step !== 'string' || !allowed.has(row.status as PlanStep['status'])) {
      return [];
    }
    return [{ step: row.step, status: row.status as PlanStep['status'] }];
  });
}

/**
 * The lineage node owns summary attributes; final rows fill gaps and settle the status.
 *
 * `runEvents` is `list_events { run_id }` in its default order, oldest first — the order
 * that makes "the last `run-progress` seen" the newest plan. Ask for `newest_first` and the
 * plan shown would be the first one the run reported.
 */
export function buildRunView(runNode: LineageNode | undefined, runEvents: Event[]): RunView {
  let progressPlan: PlanStep[] | undefined;
  let finished: Body = {};
  const attempts: RunAttempt[] = [];

  for (const event of runEvents) {
    if (event.label_skill !== 'escurel:run') continue;
    const body = bodyOf(event);
    if (event.title === 'run-attempt') {
      const n = numberValue(body.attempt);
      if (n !== undefined) {
        attempts.push({
          n,
          startedAt: timestamp(body.started_at),
          endedAt: timestamp(body.ended_at),
          outcome: stringValue(body.outcome) ?? '',
          error: stringValue(body.error),
        });
      }
    } else if (event.title === 'run-progress') {
      progressPlan = planSteps(body.plan) ?? progressPlan;
    } else if (event.title === 'run-finished') {
      finished = body;
    }
  }

  attempts.sort((a, b) => a.n - b.n);
  const status = stringValue(finished.status) ?? runNode?.state ?? 'running';
  return {
    runId:
      runNode?.id ?? runEvents.find((event) => event.label_skill === 'escurel:run')?.run_id ?? '',
    status,
    tone: ['failed', 'dead_letter', 'cancelled'].includes(status) ? 'failed' : 'run',
    harness: stringValue(runNode?.harness) ?? stringValue(finished.harness),
    model: stringValue(runNode?.model) ?? stringValue(finished.model),
    autonomy: stringValue(runNode?.autonomy) ?? stringValue(finished.autonomy),
    targetPageId: stringValue(runNode?.target_page_id) ?? stringValue(finished.target_page_id),
    traceId: stringValue(runNode?.trace_id) ?? stringValue(finished.trace_id),
    startedAt: timestamp(runNode?.started_at) ?? timestamp(finished.started_at),
    finishedAt: timestamp(runNode?.finished_at) ?? timestamp(finished.finished_at),
    depth: numberValue(runNode?.depth) ?? numberValue(finished.depth),
    attempts,
    maxAttempts: numberValue(runNode?.max_attempts) ?? numberValue(finished.max_attempts),
    plan: planSteps(finished.plan) ?? progressPlan ?? planSteps(runNode?.plan) ?? [],
    summary: stringValue(runNode?.summary) ?? stringValue(finished.summary),
    toolCallCount: numberValue(runNode?.tool_calls) ?? numberValue(finished.tool_calls),
    calls: [],
    nextAfter: null,
  };
}

function callRow(call: GetRunToolCallsResponse['calls'][number]): ToolCallRow {
  return {
    seq: call.seq,
    tool: call.tool,
    status: call.status,
    errorCode: call.error_code,
    durationMs: call.duration_ms,
    bytes: { request: call.request_bytes, response: call.response_bytes },
    at: call.at,
  };
}

/**
 * Replayed pages can overlap, so seq identifies a row across every page.
 *
 * Only the page that reaches furthest decides `nextAfter`. A live refresh can re-merge
 * page 1 after the last page, and page 1's cursor is stale: taking it would put
 * "Load more" back on a run whose calls are all on screen.
 */
export function mergeToolCallPage(view: RunView, page: GetRunToolCallsResponse): RunView {
  const furthest = view.calls.reduce((max, call) => Math.max(max, call.seq), 0);
  const calls = new Map(view.calls.map((call) => [call.seq, call]));
  for (const call of page.calls) {
    if (!calls.has(call.seq)) calls.set(call.seq, callRow(call));
  }
  const pageReach = page.calls.reduce((max, call) => Math.max(max, call.seq), 0);
  return {
    ...view,
    calls: [...calls.values()].sort((a, b) => a.seq - b.seq),
    nextAfter: pageReach >= furthest ? page.next_after : view.nextAfter,
  };
}
